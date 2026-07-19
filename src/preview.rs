//! `ablemod preview` — plays a VGM/VGZ file's chip-emulated stems live, in sync, in one window
//! laid out as a grid of small oscilloscope cells (one per non-silent channel, up to
//! GRID_MAX_CELLS) plus one reserved cell showing the file's own metadata (the same content
//! `ablemod list` prints — see `formats::vgm::summary_lines`).
//!
//! Feature-gated behind `preview` (see Cargo.toml's own comment) — SDL2/SDL2_ttf are system
//! libraries, not vendored/compiled from source like every other native dependency this
//! project links, so the default `ablemod` build never needs them.
//!
//! Stems come straight from `export::vgm_render::render_stems` (an in-memory render, same as
//! `extract-separated-tracks-wav` writes to disk) — no intermediate WAV files. The window
//! itself stays a fixed size (1920x1080); the grid (and so each cell) shrinks as more channels
//! are non-silent, rather than a handful of tracks each getting a full-width lane and many
//! tracks making the window ever taller. Each waveform cell shows a short trailing sample of
//! that channel's own audio as it's actually being played, redrawn fresh from the raw samples
//! every video frame — the real wave shape/cycles, not a DAW-style compressed whole-file
//! overview.
//!
//! Playback and rendering both go through SDL2 (not a separate audio crate) specifically so
//! they share one clock: every cell's trace position is read directly from the sample count
//! SDL2's own audio callback has actually written, not a wall-clock timer that could drift out
//! of sync with what's really coming out of the speakers. Playback loops back to the start by
//! default (pass --no-loop to play once and stop), matching how a game's own music loops
//! indefinitely too.
//!
//! `--record <file.mp4>` renders and encodes a video of exactly one playthrough instead of
//! opening a live, audio-driven window: frame timing is derived deterministically from the
//! sample count (frame f -> sample f/fps*rate), not real elapsed time or the live audio
//! device, so this runs as fast as the CPU can encode rather than waiting through the actual
//! song length. Each rendered frame's pixels are piped as raw video into an `ffmpeg`
//! subprocess (not a Rust encoding crate — there's no mature pure-Rust H.264 encoder, and
//! shelling out to ffmpeg is the standard, pragmatic approach), which also muxes in a
//! freshly-rendered mixdown WAV of the same tracks. Requires `ffmpeg` on PATH.

use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sdl2::audio::{AudioCallback, AudioSpecDesired};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::{Point, Rect};
use sdl2::render::WindowCanvas;
use sdl2::ttf::Sdl2TtfContext;

use crate::export::vgm_render::{peak, render, render_stems};
use crate::formats::vgm;

/// Bitstream Vera-derived license (see assets/DejaVuSansMono-LICENSE.txt) — permissive,
/// explicitly allows bundling/redistribution as part of a larger software package.
const FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");
const FONT_PT_SIZE: u16 = 16;
const RECORD_FPS: u32 = 30;

// The window itself is a fixed size — the grid divides it up, so a cell's actual on-screen
// size shrinks as more channels are non-silent rather than the window growing to keep every
// cell a fixed size.
const WINDOW_WIDTH: u32 = 1920;
const WINDOW_HEIGHT: u32 = 1080;
const CELL_MARGIN: u32 = 8;
// Grid cells are capped here — a 4x4 grid stays legible even on a modest display; a file with
// more non-silent channels than this still plays in full (every stem is summed into the mix),
// just only the first GRID_MAX_CELLS - 1 get their own waveform cell (one slot is always the
// info panel, see center_slot's own comment).
const GRID_MAX_CELLS: usize = 16;
// How much of the signal is visible at once, trailing the current playback position (the
// right edge of each cell's trace is always "now") — kept close to, but a little above, the
// ~735 audio frames a 44100Hz stream advances per video frame at 60fps, so consecutive
// redraws mostly overlap with a small shift rather than jumping, reading as a smoothly
// scrolling trace instead of a flipbook of unrelated windows.
const SCOPE_FRAMES: usize = 1024;

const TRACK_COLORS: [Color; 8] = [
    Color::RGB(90, 170, 255),
    Color::RGB(255, 140, 90),
    Color::RGB(120, 220, 140),
    Color::RGB(230, 120, 220),
    Color::RGB(240, 210, 90),
    Color::RGB(120, 200, 220),
    Color::RGB(220, 100, 100),
    Color::RGB(170, 150, 255),
];

struct Track {
    name: String,
    /// Interleaved stereo, -1.0..1.0 — every stem `render_stems` returns already shares one
    /// total-sample length (padded to the file's own declared total), so no length
    /// reconciliation is needed here the way loading independent WAV files would.
    interleaved: Vec<f32>,
}

/// Grid dimensions for `cell_count` cells — roughly square, biasing to more columns than rows
/// when they can't match exactly (e.g. 5 cells -> 3 columns x 2 rows, with one empty slot)
/// since displays are usually wider than tall.
fn grid_dims(cell_count: usize) -> (u32, u32) {
    if cell_count == 0 {
        return (1, 1);
    }
    let cols = (cell_count as f64).sqrt().ceil() as u32;
    let rows = (cell_count as u32).div_ceil(cols);
    (cols, rows)
}

/// The one grid slot (row-major index) reserved for the info panel — a fixed hole in the
/// middle of the grid, not assigned to any waveform cell. Well defined even for an even
/// col/row count (no single exact center then): integer-divides down to the nearest cell
/// short of true center rather than picking arbitrarily between several equally-central
/// candidates.
fn center_slot(cols: u32, rows: u32) -> usize {
    ((rows / 2) * cols + cols / 2) as usize
}

/// Maps a track's index (0-based, in load order) to its grid slot, skipping over `center` so
/// tracks never land there — every slot before the center is used as-is, every slot from the
/// center onward is pushed one further along to make room for the info panel.
fn slot_for_track(track_index: usize, center: usize) -> usize {
    if track_index < center {
        track_index
    } else {
        track_index + 1
    }
}

/// One (min, max) mono peak pair per pixel column across `[start_frame, end_frame)` — recomputed
/// fresh every video frame from a small trailing window of raw samples (SCOPE_FRAMES), unlike a
/// DAW's own once-per-load overview: this is deliberately not cached, since the window itself
/// moves every frame.
fn scope_peaks(interleaved: &[f32], start_frame: usize, end_frame: usize, width: u32) -> Vec<(f32, f32)> {
    let mut peaks = vec![(0.0f32, 0.0f32); width as usize];
    let frame_count = end_frame - start_frame;
    if frame_count == 0 {
        return peaks;
    }
    for (offset, chunk) in interleaved[start_frame * 2..end_frame * 2].chunks_exact(2).enumerate() {
        let mono = (chunk[0] + chunk[1]) * 0.5;
        let col = (offset * width as usize / frame_count).min(width as usize - 1);
        let (min, max) = &mut peaks[col];
        *min = min.min(mono);
        *max = max.max(mono);
    }
    peaks
}

/// Sums every track's interleaved stereo audio starting at a shared, atomically-shared frame
/// position — SDL2 calls this on its own audio thread whenever it needs more data; the frame
/// counter it advances here is the single source of truth the render loop reads back to place
/// every cell's own trace, so audio and visuals can never drift apart the way two
/// independently-timed clocks could. Wraps back to frame 0 mid-buffer when looping is on, so a
/// loop boundary that falls in the middle of one callback's worth of samples doesn't get
/// clipped or skipped.
struct MixCallback {
    tracks: Arc<Vec<Track>>,
    total_frames: usize,
    frame_pos: Arc<AtomicUsize>,
    loop_playback: bool,
}

impl AudioCallback for MixCallback {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        let mut frame = self.frame_pos.load(Ordering::Relaxed);
        for out_frame in out.chunks_exact_mut(2) {
            if frame >= self.total_frames {
                if self.loop_playback && self.total_frames > 0 {
                    frame = 0;
                } else {
                    out_frame[0] = 0.0;
                    out_frame[1] = 0.0;
                    continue;
                }
            }
            let mut l = 0.0;
            let mut r = 0.0;
            for track in self.tracks.iter() {
                l += track.interleaved[frame * 2];
                r += track.interleaved[frame * 2 + 1];
            }
            out_frame[0] = l.clamp(-1.0, 1.0);
            out_frame[1] = r.clamp(-1.0, 1.0);
            frame += 1;
        }
        self.frame_pos.store(frame, Ordering::Relaxed);
    }
}

/// One pre-rendered line of the info panel — rendered once at startup (the metadata never
/// changes while playing, unlike the waveform cells), not every video frame.
struct InfoLine<'t> {
    texture: sdl2::render::Texture<'t>,
    width: u32,
    height: u32,
}

/// Renders every `formats::vgm::summary_lines` line to its own cached texture — SDL2_ttf has
/// no built-in multi-line support (`Font::render` mangles embedded newlines), so each line
/// gets its own texture, stacked top-to-bottom when drawn.
fn render_info_lines<'t>(
    lines: &[String], font: &sdl2::ttf::Font, texture_creator: &'t sdl2::render::TextureCreator<sdl2::video::WindowContext>,
) -> Vec<InfoLine<'t>> {
    lines
        .iter()
        .filter_map(|line| {
            let text = if line.is_empty() { " " } else { line.as_str() };
            let surface = font.render(text).blended(Color::RGB(220, 220, 225)).ok()?;
            let (width, height) = (surface.width(), surface.height());
            let texture = texture_creator.create_texture_from_surface(&surface).ok()?;
            Some(InfoLine { texture, width, height })
        })
        .collect()
}

/// Renders every cell for one frame at playback position `played` — shared by the live loop
/// (called once per real video frame, `played` read from the live audio callback's own
/// counter) and `--record` mode (called once per encoded frame, `played` computed
/// deterministically from the frame index instead).
#[allow(clippy::too_many_arguments)]
fn draw_frame(
    canvas: &mut WindowCanvas, tracks: &[Track], info_lines: &[InfoLine], cell_count: usize, cols: u32, rows: u32, center: usize,
    total_frames: usize, played: usize,
) {
    canvas.set_draw_color(Color::RGB(20, 20, 24));
    canvas.clear();

    let end = played.min(total_frames);
    let start = end.saturating_sub(SCOPE_FRAMES);

    let cell_w = WINDOW_WIDTH / cols;
    let cell_h = WINDOW_HEIGHT / rows;
    let half_amplitude = (cell_h.saturating_sub(CELL_MARGIN * 2)) as f32 / 2.0;

    let center_col = center as u32 % cols;
    let center_row = center as u32 / cols;
    let center_rect = Rect::new((center_col * cell_w) as i32, (center_row * cell_h) as i32, cell_w, cell_h);
    canvas.set_clip_rect(Some(center_rect));
    canvas.set_draw_color(Color::RGB(45, 45, 50));
    let _ = canvas.draw_rect(center_rect);
    let text_margin = 12i32;
    let mut text_y = center_rect.y() + text_margin;
    for line in info_lines {
        if text_y + line.height as i32 > center_rect.y() + center_rect.height() as i32 {
            break;
        }
        let dest = Rect::new(center_rect.x() + text_margin, text_y, line.width.min(cell_w - text_margin as u32 * 2), line.height);
        let _ = canvas.copy(&line.texture, None, Some(dest));
        text_y += line.height as i32 + 2;
    }
    canvas.set_clip_rect(None);

    for (i, track) in tracks.iter().take(cell_count).enumerate() {
        let slot = slot_for_track(i, center);
        let col = slot as u32 % cols;
        let row = slot as u32 / cols;
        let cell_x = (col * cell_w) as i32;
        let cell_y = (row * cell_h) as i32;
        let cell_rect = Rect::new(cell_x, cell_y, cell_w, cell_h);
        let center_y = cell_y + (cell_h / 2) as i32;

        canvas.set_clip_rect(Some(cell_rect));
        canvas.set_draw_color(Color::RGB(45, 45, 50));
        let _ = canvas.draw_rect(cell_rect);

        canvas.set_draw_color(Color::RGB(50, 50, 56));
        let _ = canvas.draw_line(Point::new(cell_x, center_y), Point::new(cell_x + cell_w as i32, center_y));

        let peaks = scope_peaks(&track.interleaved, start, end, cell_w - CELL_MARGIN);
        canvas.set_draw_color(TRACK_COLORS[i % TRACK_COLORS.len()]);
        for (x, &(min, max)) in peaks.iter().enumerate() {
            let abs_x = cell_x + (CELL_MARGIN / 2) as i32 + x as i32;
            let y1 = center_y - (max * half_amplitude) as i32;
            let y2 = center_y - (min * half_amplitude) as i32;
            let _ = canvas.draw_line(Point::new(abs_x, y1), Point::new(abs_x, y2.max(y1)));
        }
        canvas.set_clip_rect(None);
    }
}

/// Renders every track's own full mixdown (the same sum `MixCallback` computes live, one shot
/// over the whole track instead of realtime) to a plain stereo WAV — `--record` mode's audio
/// source, muxed into the video by ffmpeg itself rather than captured from a live audio device.
fn write_mixdown_wav(tracks: &[Track], total_frames: usize, sample_rate: u32, path: &Path) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    for frame in 0..total_frames {
        let mut l = 0.0;
        let mut r = 0.0;
        for track in tracks {
            l += track.interleaved[frame * 2];
            r += track.interleaved[frame * 2 + 1];
        }
        writer.write_sample((l.clamp(-1.0, 1.0) * 32767.0) as i16).map_err(|e| e.to_string())?;
        writer.write_sample((r.clamp(-1.0, 1.0) * 32767.0) as i16).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// Spawns ffmpeg reading raw RGB24 video frames from its stdin (piped in by the caller, one
/// `draw_frame` + `read_pixels` per frame) plus the mixdown WAV as a second input, encoding
/// both into `output_path`. ffmpeg's own stderr is inherited so encoding errors (bad codec,
/// disk full, ...) surface directly instead of failing silently.
fn spawn_ffmpeg(output_path: &Path, audio_wav: &Path, fps: u32) -> Result<Child, String> {
    Command::new("ffmpeg")
        .args(["-y", "-f", "rawvideo", "-pixel_format", "rgb24"])
        .args(["-video_size", &format!("{WINDOW_WIDTH}x{WINDOW_HEIGHT}")])
        .args(["-framerate", &fps.to_string(), "-i", "pipe:0"])
        .arg("-i")
        .arg(audio_wav)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg (is it installed and on PATH? try `brew install ffmpeg` / `apt install ffmpeg`): {e}"))
}

#[allow(clippy::too_many_arguments)]
fn record(
    canvas: &mut WindowCanvas, event_pump: &mut sdl2::EventPump, output_path: &Path, sample_rate: u32, tracks: &[Track], total_frames: usize,
    cell_count: usize, cols: u32, rows: u32, center: usize, info_lines: &[InfoLine],
) -> Result<(), String> {
    let audio_wav_path = std::env::temp_dir().join(format!("ablemod-preview-{}.wav", std::process::id()));
    write_mixdown_wav(tracks, total_frames, sample_rate, &audio_wav_path)?;
    // Runs to completion (or the process is killed) regardless of how this function returns —
    // a temp file left behind on error is harmless clutter, not worth a panic-safety dance over.
    let cleanup = || {
        let _ = std::fs::remove_file(&audio_wav_path);
    };

    let mut ffmpeg = match spawn_ffmpeg(output_path, &audio_wav_path, RECORD_FPS) {
        Ok(child) => child,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };
    let mut ffmpeg_stdin = ffmpeg.stdin.take().expect("stdin was piped");

    let total_video_frames = ((total_frames as f64 / sample_rate as f64 * RECORD_FPS as f64).ceil() as usize).max(1);
    println!("Recording {total_video_frames} frame(s) @ {RECORD_FPS}fps to {}...", output_path.display());

    for f in 0..total_video_frames {
        // Still pump events so the OS doesn't consider the window unresponsive — Esc/Q/closing
        // it aborts the recording early (ffmpeg finishes with whatever frames it already got).
        for event in event_pump.poll_iter() {
            if matches!(event, Event::Quit { .. } | Event::KeyDown { keycode: Some(Keycode::Escape | Keycode::Q), .. }) {
                println!("Recording aborted at frame {f}/{total_video_frames}.");
                drop(ffmpeg_stdin);
                let _ = ffmpeg.wait();
                cleanup();
                return Ok(());
            }
        }

        let played = (f as f64 / RECORD_FPS as f64 * sample_rate as f64) as usize;
        draw_frame(canvas, tracks, info_lines, cell_count, cols, rows, center, total_frames, played);
        canvas.present();

        let pixels = canvas.read_pixels(None, PixelFormatEnum::RGB24).map_err(|e| {
            cleanup();
            e
        })?;
        if let Err(e) = ffmpeg_stdin.write_all(&pixels) {
            cleanup();
            return Err(format!("ffmpeg closed its input early (see its own stderr output above for why): {e}"));
        }

        if f % (RECORD_FPS as usize * 5) == 0 {
            println!("  {f}/{total_video_frames}");
        }
    }

    drop(ffmpeg_stdin);
    let status = ffmpeg.wait().map_err(|e| e.to_string())?;
    cleanup();
    if !status.success() {
        return Err(format!("ffmpeg exited with {status} — see its own stderr output above for why"));
    }
    println!("Wrote {}", output_path.display());
    Ok(())
}

pub fn run(module_path: &Path, record_to: Option<&Path>, no_loop: bool) -> Result<(), String> {
    let bytes = std::fs::read(module_path).map_err(|e| format!("failed to read {}: {e}", module_path.display()))?;
    let vgm_file = vgm::parse(&bytes)?;
    let info_text_lines = vgm::summary_lines(&vgm_file);

    let master = render(&vgm_file);
    let sample_rate = master.sample_rate;
    let stems = render_stems(&vgm_file);
    if stems.is_empty() {
        return Err(
            "no non-silent chip channels to preview — none of this file's music data is on a chip this converter \
             emulates, or the file is silent throughout; run `ablemod list` on it to check."
                .to_string(),
        );
    }
    let total_frames = master.left.len();
    // libvgm's own player mixes at its own internal per-chip volume balance — its raw output
    // has real headroom above a normalized range, by design (see export::vgm_render's own
    // NATIVE_UNIT_SCALE comment), meant to be brought into 0.9-peak range via this same
    // master-peak-derived gain before being written out (export::vgm_render::write_wav's own
    // callers all do this). Skipping it here summed every stem's own already-hot signal
    // together with no headroom at all, clipping hard on both playback (audibly saturated)
    // and the waveform display (traces pinned past each cell's own vertical bounds). All
    // stems share this one gain (not independently peak-normalized) for the same reason
    // write_wav's own doc comment gives: independently normalizing each stem would make a
    // quiet background voice as loud as the lead.
    let master_peak = peak(&master);
    let gain = if master_peak > 0.0 { 0.9 / master_peak } else { 1.0 };
    let tracks: Vec<Track> = stems
        .into_iter()
        .map(|stem| {
            let mut interleaved = Vec::with_capacity(stem.audio.left.len() * 2);
            for (&l, &r) in stem.audio.left.iter().zip(stem.audio.right.iter()) {
                interleaved.push(l * gain);
                interleaved.push(r * gain);
            }
            Track { name: stem.name, interleaved }
        })
        .collect();
    let loop_playback = !no_loop;

    // One slot is always reserved for the info panel (see center_slot's own comment) —
    // GRID_MAX_CELLS counts that slot too, so at most GRID_MAX_CELLS - 1 tracks actually get a
    // waveform cell.
    let cell_count = tracks.len().min(GRID_MAX_CELLS - 1);
    let (cols, rows) = grid_dims(cell_count + 1);
    let center = center_slot(cols, rows);

    println!("Loaded {} channel(s) @ {sample_rate}Hz, {:.1}s:", tracks.len(), total_frames as f64 / sample_rate as f64);
    for (i, track) in tracks.iter().enumerate() {
        if i < cell_count {
            let slot = slot_for_track(i, center);
            println!("  [{}, {}] {}", slot as u32 / cols, slot as u32 % cols, track.name);
        } else {
            println!("  (no cell, still playing) {}", track.name);
        }
    }
    if tracks.len() > cell_count {
        println!("Note: only the first {cell_count} channels get their own cell (one slot is always the info panel) — every channel still plays.");
    }

    let sdl_context = sdl2::init()?;
    let video_subsystem = sdl_context.video()?;
    let ttf_context: Sdl2TtfContext = sdl2::ttf::init().map_err(|e| e.to_string())?;
    let font_rwops = sdl2::rwops::RWops::from_bytes(FONT_BYTES)?;
    let font = ttf_context.load_font_from_rwops(font_rwops, FONT_PT_SIZE)?;

    let title = if record_to.is_some() { "ablemod preview (recording)" } else { "ablemod preview" };
    let window = video_subsystem.window(title, WINDOW_WIDTH, WINDOW_HEIGHT).position_centered().build().map_err(|e| e.to_string())?;
    // No vsync in record mode — recording should run as fast as the CPU can encode, not
    // throttled to the display's own refresh rate.
    let mut canvas = if record_to.is_some() {
        window.into_canvas().build().map_err(|e| e.to_string())?
    } else {
        window.into_canvas().present_vsync().build().map_err(|e| e.to_string())?
    };
    // info_lines' own textures are tied to this specific canvas' texture_creator (SDL2
    // textures belong to the renderer that created them) — both mode branches below reuse
    // this same canvas, never a second one, so that binding stays valid throughout.
    let texture_creator = canvas.texture_creator();
    let info_lines = render_info_lines(&info_text_lines, &font, &texture_creator);
    let mut event_pump = sdl_context.event_pump()?;

    if let Some(output_path) = record_to {
        return record(&mut canvas, &mut event_pump, output_path, sample_rate, &tracks, total_frames, cell_count, cols, rows, center, &info_lines);
    }

    println!("Looping: {}", if loop_playback { "on (pass --no-loop to play once)" } else { "off" });
    println!("Space = pause/resume, Esc/Q or close the window = quit.");

    let audio_subsystem = sdl_context.audio()?;
    let frame_pos = Arc::new(AtomicUsize::new(0));
    let tracks = Arc::new(tracks);

    let desired_spec = AudioSpecDesired { freq: Some(sample_rate as i32), channels: Some(2), samples: Some(1024) };
    let device = audio_subsystem.open_playback(None, &desired_spec, |_spec| MixCallback {
        tracks: tracks.clone(),
        total_frames,
        frame_pos: frame_pos.clone(),
        loop_playback,
    })?;
    device.resume();

    'running: loop {
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } | Event::KeyDown { keycode: Some(Keycode::Escape | Keycode::Q), .. } => break 'running,
                Event::KeyDown { keycode: Some(Keycode::Space), .. } => {
                    if device.status() == sdl2::audio::AudioStatus::Playing {
                        device.pause();
                    } else {
                        device.resume();
                    }
                }
                _ => {}
            }
        }

        let played = frame_pos.load(Ordering::Relaxed);
        draw_frame(&mut canvas, &tracks, &info_lines, cell_count, cols, rows, center, total_frames, played);
        canvas.present();
    }

    Ok(())
}

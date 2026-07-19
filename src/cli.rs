use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::export::als::{export_als, AmigaPanning};
use crate::export::midi::write_midi;
use crate::export::notes::compute_song_events;
use crate::export::wav::{sample_wav_filename, write_sample_wav};
use crate::formats::base::Module;
use crate::formats::detect::load_module;
use crate::formats::fasttracker2;
use crate::formats::protracker;
use crate::formats::vgm;

/// Convert ProTracker modules or VGM/VGZ chiptune recordings to Ableton Live projects.
#[derive(Parser)]
#[command(name = "ablemod")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Display the contents of a tracker module or VGM/VGZ file.
    List { module_path: PathBuf },

    /// Extract all samples from a tracker module as .wav files. Not applicable to VGM/VGZ
    /// (which has no stored samples) — use 'convert-als' instead.
    ExtractSamples {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_dir: PathBuf,
        /// Print extra detail about each sample as it's extracted.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Convert a tracker module's patterns to a .mid file (one track per sample). Not
    /// applicable to VGM/VGZ — use 'convert-als' instead.
    ExtractMidi {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_path: PathBuf,
        /// Print extra detail, including which effects were transcribed and which were ignored.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Render a VGM/VGZ file's full chip-emulated mix down to a single .wav file. VGM/VGZ
    /// only — tracker modules have no audio-rendering engine of their own yet (Ableton's own
    /// Sampler does that at playback time once you've run 'convert-als'), so use
    /// 'extract-samples' for a tracker module's raw stored samples instead.
    ExtractMixedTracks {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_path: PathBuf,
        /// Print extra detail about the render (duration, peak level).
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Render a VGM/VGZ file's chip-emulated audio to one .wav file per chip channel (stem),
    /// isolated by muting every other channel — the same per-channel WAVs 'convert-als'
    /// writes into an Ableton project's Samples/Imported folder, without the project itself.
    /// VGM/VGZ only — see 'extract-mixed-tracks' for why tracker modules aren't supported yet.
    ExtractSeparatedTracksWav {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_dir: PathBuf,
        /// Print extra detail about the render (which channels were non-silent).
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Convert a tracker module or VGM/VGZ file into an Ableton Live Set (.als).
    ///
    /// For a tracker module: one Sampler per sample, no audio synthesis done by ablemod
    /// itself. For a VGM/VGZ file: the chips are actually emulated and rendered to audio —
    /// one AudioTrack for the full mix plus one per chip channel (stems), no MIDI/Sampler
    /// involved (see 'list' to check which chips a given file uses, and README.md for the
    /// full supported-chip list).
    ConvertAls {
        module_path: PathBuf,
        /// Optional: a real .als exported from Ableton Live, containing a MIDI track with a
        /// Sampler device holding a sample and its content laid out in Arrangement view.
        /// Defaults to ablemod's bundled template.
        #[arg(long = "template")]
        template_path: Option<PathBuf>,
        #[arg(short = 'o', long = "output")]
        output_path: PathBuf,
        /// Baseline Panorama automation: real 4-channel tracker hardware hard-pans channels
        /// 0/3 left and 1/2 right (repeating every 4 channels) with no in-between mix — "full"
        /// reproduces that; "light"/"medium" soften it to 25%/50% stereo separation; "none"
        /// (default) keeps every note centered unless the module uses Set Panning (8xx) itself.
        /// Ignored for VGM/VGZ input.
        #[arg(long = "amiga-panning", default_value = "none")]
        amiga_panning: AmigaPanning,
        /// Print extra detail, including which effects were transcribed and which were ignored.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Play a VGM/VGZ file's chip-emulated stems live, one scrolling-waveform cell per channel
    /// in a single window (see src/preview.rs's own module comment) — VGM/VGZ only, same
    /// reason 'extract-mixed-tracks' is. Requires ablemod to have been built with `--features
    /// preview` (needs SDL2 + SDL2_ttf installed; not part of a default build).
    Preview {
        module_path: PathBuf,
        /// Instead of opening a live, audio-driven window, render exactly one playthrough to
        /// this video file (e.g. out.mp4) via ffmpeg (must be installed and on PATH).
        #[arg(long = "record")]
        record: Option<PathBuf>,
        /// Play through once and stop instead of looping back to the start.
        #[arg(long = "no-loop")]
        no_loop: bool,
    },
}

fn print_module_summary(module: &Module) {
    println!(
        "Verbose: {}, {} channel(s), {} pattern(s), order length {}, initial {} ticks/row @ {} BPM",
        module.source_format,
        module.num_channels,
        module.patterns.len(),
        module.order.len(),
        module.initial_speed_ticks,
        module.initial_tempo_bpm
    );
}

fn print_effect_table(counts: &std::collections::BTreeMap<u32, u32>, effect_name: fn(u32) -> &'static str) {
    let mut items: Vec<(u32, u32)> = counts.iter().map(|(&k, &v)| (k, v)).collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    for (code, count) in items {
        // Exx sub-commands use a synthetic 0xE0..=0xEF code (see
        // protracker::extended_subcommand_counts) — displayed as "E1x" (2 hex digits + one
        // placeholder), matching how modules/trackers document these, not "1xx" like every
        // other top-level code.
        let label = if (0xE0..=0xEF).contains(&code) { format!("E{:X}x", code & 0x0F) } else { format!("{code:X}xx") };
        println!("  {label:<5} {:<32} {} occurrence(s)", effect_name(code), count);
    }
}

fn print_effects_report(module: &Module) {
    // Effect semantics (which codes ablemod actually simulates) are format-specific — each
    // tracker parser module owns its own naming/implemented-effects tables (see this
    // codebase's existing per-format-parser convention, not shared between formats).
    let (implemented, unimplemented, effect_name): (_, _, fn(u32) -> &'static str) = match module.source_format.as_str() {
        "protracker" => (protracker::implemented_effect_counts(module), protracker::unimplemented_effect_counts(module), protracker::effect_name),
        "fasttracker2" => (fasttracker2::implemented_effect_counts(module), fasttracker2::unimplemented_effect_counts(module), fasttracker2::effect_name),
        _ => return,
    };

    if !implemented.is_empty() {
        println!("Verbose: effects correctly transcribed:");
        print_effect_table(&implemented, effect_name);
    } else {
        println!("Verbose: no implemented effects found in this module.");
    }

    if !unimplemented.is_empty() {
        println!("Verbose: unimplemented effects found (ignored during playback simulation):");
        print_effect_table(&unimplemented, effect_name);
    } else {
        println!("Verbose: no unimplemented effects found.");
    }
}

/// VGM/VGZ files have no Module/Pattern/Sample IR to speak of (see formats::vgm's own doc
/// comment) — they're routed to entirely separate handlers rather than through load_module.
fn is_vgm_path(path: &std::path::Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(), Some("vgm") | Some("vgz"))
}

pub fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::List { module_path } => {
            if is_vgm_path(&module_path) {
                vgm_list_cmd(&module_path)
            } else {
                list_cmd(&module_path)
            }
        }
        Command::ExtractSamples { module_path, output_dir, verbose } => {
            if is_vgm_path(&module_path) {
                return Err(
                    "extract-samples doesn't apply to VGM/VGZ files: unlike a tracker module, a VGM has no stored \
                     samples to extract — its audio only exists once the chip registers are actually synthesized. \
                     Use 'extract-mixed-tracks'/'extract-separated-tracks-wav' for the rendered chip audio, or \
                     'convert-als' for a full Ableton project."
                        .to_string(),
                );
            }
            extract_samples_cmd(&module_path, &output_dir, verbose)
        }
        Command::ExtractMidi { module_path, output_path, verbose } => {
            if is_vgm_path(&module_path) {
                return Err(
                    "extract-midi doesn't apply to VGM/VGZ files: this project renders VGM chip audio directly \
                     (see 'convert-als'/'extract-mixed-tracks'/'extract-separated-tracks-wav') rather than \
                     transcribing register writes into MIDI notes."
                        .to_string(),
                );
            }
            extract_midi_cmd(&module_path, &output_path, verbose)
        }
        Command::ExtractMixedTracks { module_path, output_path, verbose } => {
            if !is_vgm_path(&module_path) {
                return Err(
                    "extract-mixed-tracks doesn't apply to tracker modules yet: no audio-rendering engine exists \
                     for .mod/.xm/.s3m today (only VGM/VGZ's chip emulation renders real audio) — Ableton's own \
                     Sampler does the synthesis at playback time after 'convert-als', or use 'extract-samples' for \
                     the module's own raw stored samples."
                        .to_string(),
                );
            }
            extract_mixed_tracks_cmd(&module_path, &output_path, verbose)
        }
        Command::ExtractSeparatedTracksWav { module_path, output_dir, verbose } => {
            if !is_vgm_path(&module_path) {
                return Err(
                    "extract-separated-tracks-wav doesn't apply to tracker modules yet: no audio-rendering engine \
                     exists for .mod/.xm/.s3m today (only VGM/VGZ's chip emulation renders real audio) — Ableton's \
                     own Sampler does the synthesis at playback time after 'convert-als', or use 'extract-samples' \
                     for the module's own raw stored samples."
                        .to_string(),
                );
            }
            extract_separated_tracks_wav_cmd(&module_path, &output_dir, verbose)
        }
        Command::ConvertAls { module_path, template_path, output_path, amiga_panning, verbose } => {
            if is_vgm_path(&module_path) {
                vgm_convert_cmd(&module_path, template_path.as_deref(), &output_path, verbose)
            } else {
                convert_cmd(&module_path, template_path.as_deref(), &output_path, amiga_panning, verbose)
            }
        }
        Command::Preview { module_path, record, no_loop } => {
            if !is_vgm_path(&module_path) {
                return Err(
                    "preview doesn't apply to tracker modules yet: no audio-rendering engine exists for \
                     .mod/.xm/.s3m today (only VGM/VGZ's chip emulation renders real audio)."
                        .to_string(),
                );
            }
            #[cfg(feature = "preview")]
            {
                crate::preview::run(&module_path, record.as_deref(), no_loop)
            }
            #[cfg(not(feature = "preview"))]
            {
                let _ = (record, no_loop);
                Err(
                    "ablemod was built without the 'preview' feature — rebuild with `cargo build --features \
                     preview` (requires SDL2 and SDL2_ttf installed) to use this command."
                        .to_string(),
                )
            }
        }
    }
}

fn vgm_list_cmd(path: &std::path::Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vgm = vgm::parse(&bytes)?;

    println!("Title:      {}", vgm.title.as_deref().unwrap_or("(unknown)"));
    println!("Game:       {}", vgm.game.as_deref().unwrap_or("(unknown)"));
    println!("System:     {}", vgm.system.as_deref().unwrap_or("(unknown)"));
    println!("Author:     {}", vgm.author.as_deref().unwrap_or("(unknown)"));
    println!("Format:     VGM v{}", vgm::version_string(vgm.version));
    println!("Duration:   {:.1}s ({} samples @ 44100Hz)", vgm.total_samples as f64 / 44100.0, vgm.total_samples);
    if let Some(loop_start) = vgm.loop_start_sample {
        println!(
            "Loop:       from {:.1}s, {:.1}s long",
            loop_start as f64 / 44100.0,
            vgm.loop_samples as f64 / 44100.0
        );
    } else {
        println!("Loop:       (none)");
    }

    println!("\nChips:");
    if vgm.ym2413_clock > 0 {
        println!("  YM2413 (OPLL FM) @ {} Hz — emulated by convert", vgm.ym2413_clock);
    }
    if vgm.ay8910_clock > 0 {
        let variant = if vgm.ay8910_is_ym { " [YM2149-compatible]" } else { "" };
        println!("  AY8910 (PSG){variant} @ {} Hz — emulated by convert", vgm.ay8910_clock);
    }
    if vgm.scc_clock > 0 {
        println!("  K051649/SCC (Konami) @ {} Hz — emulated by convert", vgm.scc_clock);
    }
    for (clock, name) in [
        (vgm.ym3526_clock, "YM3526 (OPL FM)"),
        (vgm.ym3812_clock, "YM3812 (OPL2 FM)"),
        (vgm.ym2612_clock, "YM2612 (OPN2 FM, Sega Genesis/Mega Drive)"),
        (vgm.ym2151_clock, "YM2151 (OPM FM, arcade/X68000)"),
        (vgm.ym2203_clock, "YM2203 (OPN FM)"),
        (vgm.ym2608_clock, "YM2608 (OPNA FM)"),
        (vgm.ym2610_clock, "YM2610 (OPNB FM, Neo Geo)"),
        (vgm.segapcm_clock, "Sega PCM"),
        (vgm.rf5c68_clock, "RF5C68 (PCM)"),
        (vgm.rf5c164_clock, "RF5C164 (PCM)"),
        (vgm.gb_dmg_clock, "GameBoy DMG"),
        (vgm.nes_apu_clock, "NES APU"),
    ] {
        if clock > 0 {
            println!("  {name} @ {clock} Hz — emulated by convert");
        }
    }
    if !vgm.unsupported_commands.is_empty() {
        println!("\nOther chips used in this file (not emulated, silently skipped by convert):");
        let mut by_chip: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
        for (&cmd, &count) in &vgm.unsupported_commands {
            *by_chip.entry(vgm::unsupported_chip_name(cmd)).or_insert(0) += count;
        }
        for (name, count) in by_chip {
            println!("  {name}: {count} register write(s)");
        }
    }
    Ok(())
}

fn vgm_convert_cmd(
    path: &std::path::Path, template_path: Option<&std::path::Path>, output_path: &std::path::Path, verbose: bool,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vgm = vgm::parse(&bytes)?;
    if verbose {
        println!(
            "Verbose: VGM v{}, {:.1}s, YM2413={}Hz AY8910={}Hz SCC={}Hz YM3526={}Hz YM3812={}Hz",
            vgm::version_string(vgm.version),
            vgm.total_samples as f64 / 44100.0,
            vgm.ym2413_clock,
            vgm.ay8910_clock,
            vgm.scc_clock,
            vgm.ym3526_clock,
            vgm.ym3812_clock
        );
        let extra_chips: Vec<String> = [
            (vgm.ym2612_clock, "YM2612"),
            (vgm.ym2151_clock, "YM2151"),
            (vgm.ym2203_clock, "YM2203"),
            (vgm.ym2608_clock, "YM2608"),
            (vgm.ym2610_clock, "YM2610"),
            (vgm.segapcm_clock, "SegaPCM"),
            (vgm.rf5c68_clock, "RF5C68"),
            (vgm.rf5c164_clock, "RF5C164"),
            (vgm.gb_dmg_clock, "GB-DMG"),
            (vgm.nes_apu_clock, "NES-APU"),
        ]
        .into_iter()
        .filter(|&(clock, _)| clock > 0)
        .map(|(clock, name)| format!("{name}={clock}Hz"))
        .collect();
        if !extra_chips.is_empty() {
            println!("Verbose: {}", extra_chips.join(" "));
        }
        if !vgm.unsupported_commands.is_empty() {
            println!("Verbose: other chips found in this file are not emulated and will be silent:");
            let mut by_chip: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
            for (&cmd, &count) in &vgm.unsupported_commands {
                *by_chip.entry(vgm::unsupported_chip_name(cmd)).or_insert(0) += count;
            }
            for (name, count) in by_chip {
                println!("  {name}: {count} register write(s)");
            }
        }
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let template_bytes: Vec<u8> = match template_path {
        Some(p) => std::fs::read(p).map_err(|e| format!("failed to read {}: {e}", p.display()))?,
        None => crate::export::als::default_template_bytes().to_vec(),
    };

    let master = crate::export::vgm_render::render(&vgm);
    let stems = crate::export::vgm_render::render_stems(&vgm);
    // Wavetable/Operator approximation tracks are off by default — see export_als's own doc
    // comment. Only the bit-accurate WAV tracks are generated here.
    crate::export::vgm_als::export_als(&vgm, &master, &stems, output_path, &template_bytes, false)?;

    println!("wrote {}", output_path.display());
    println!(
        "wrote samples to {}",
        output_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("Samples").join("Imported").display()
    );
    if stems.is_empty() {
        println!(
            "\nWarning: the project has no tracks. None of this file's music data is on a chip \
             this converter emulates — run `ablemod list` on it to see which chip(s) actually \
             carry the music."
        );
    } else if verbose {
        match vgm.loop_start_sample {
            Some(_) => println!("Verbose: {} WAV track(s), each split into intro+loop clips at the file's declared loop point", stems.len()),
            None => println!("Verbose: {} WAV track(s) (no declared loop point — one full-length clip each)", stems.len()),
        }
    }
    Ok(())
}

/// Renders a VGM/VGZ file's full chip-emulated mix down to a single .wav — the same render
/// `vgm_convert_cmd` computes internally (`export::vgm_render::render`), just written out on
/// its own rather than packaged into an Ableton project. Self-normalized to the same 0.9 peak
/// target `export::vgm_als::export_als` uses for its own shared stem gain (see its own
/// comment) — there's only one file here, so "shared" and "self" gain are the same thing.
fn extract_mixed_tracks_cmd(path: &std::path::Path, output_path: &std::path::Path, verbose: bool) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vgm = vgm::parse(&bytes)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let master = crate::export::vgm_render::render(&vgm);
    let peak = crate::export::vgm_render::peak(&master);
    let gain = if peak > 0.0 { 0.9 / peak } else { 1.0 };
    crate::export::vgm_render::write_wav(&master, output_path, gain).map_err(|e| e.to_string())?;

    println!("wrote {}", output_path.display());
    if verbose {
        println!(
            "Verbose: {:.1}s @ {}Hz, peak {:.3} before normalization",
            master.left.len() as f64 / master.sample_rate as f64,
            master.sample_rate,
            peak
        );
    }
    Ok(())
}

/// Renders a VGM/VGZ file's chip-emulated audio to one .wav per non-silent channel (stem) —
/// the same per-channel renders `vgm_convert_cmd` writes into an Ableton project's own
/// Samples/Imported folder (`export::vgm_render::render_stems`), written out on their own
/// instead. Unlike `vgm_convert_cmd`, there's no intro/loop clip split here (that's an
/// Arrangement-view placement concern, not meaningful for a raw file) — each stem is one
/// full-length WAV. All stems share one gain (computed from the full mix's own peak, not each
/// stem independently) for the same reason `export::vgm_render::write_wav`'s own doc comment
/// gives: independently peak-normalizing each stem would make a quiet background voice as
/// loud as the lead.
fn extract_separated_tracks_wav_cmd(path: &std::path::Path, output_dir: &std::path::Path, verbose: bool) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vgm = vgm::parse(&bytes)?;
    std::fs::create_dir_all(output_dir).map_err(|e| format!("failed to create {}: {e}", output_dir.display()))?;

    let master = crate::export::vgm_render::render(&vgm);
    let stems = crate::export::vgm_render::render_stems(&vgm);
    let peak = crate::export::vgm_render::peak(&master);
    let gain = if peak > 0.0 { 0.9 / peak } else { 1.0 };

    if stems.is_empty() {
        println!(
            "\nWarning: no tracks to write. None of this file's music data is on a chip this converter emulates \
             — run `ablemod list` on it to see which chip(s) actually carry the music."
        );
        return Ok(());
    }

    for (i, stem) in stems.iter().enumerate() {
        let safe_name = crate::export::vgm_als::sanitize_filename(&stem.name);
        let wav_path = output_dir.join(format!("{:02}_{safe_name}.wav", i + 1));
        crate::export::vgm_render::write_wav(&stem.audio, &wav_path, gain).map_err(|e| format!("failed to write {}: {e}", wav_path.display()))?;
    }

    println!("wrote {} track(s) to {}", stems.len(), output_dir.display());
    if verbose {
        let names: Vec<&str> = stems.iter().map(|s| s.name.as_str()).collect();
        println!("Verbose: {}", names.join(", "));
    }
    Ok(())
}

fn list_cmd(module_path: &std::path::Path) -> Result<(), String> {
    let module = load_module(module_path)?;

    println!("Title:      {}", module.title);
    println!("Format:     {}", module.source_format);
    println!("Channels:   {}", module.num_channels);
    println!("Patterns:   {}", module.patterns.len());
    println!("Order:      {:?}", module.order);
    println!("Restart at: {}", module.restart_position);
    println!(
        "Speed/BPM:  {} ticks/row, {} BPM (initial)",
        module.initial_speed_ticks, module.initial_tempo_bpm
    );
    let effects = module.effects_used();
    let effects_str: Vec<String> = effects.iter().map(|e| format!("{e:X}")).collect();
    println!("Effects:    {}", if effects_str.is_empty() { "(none)".to_string() } else { effects_str.join(", ") });

    println!("\nSamples:");
    for sample in &module.samples {
        if sample.is_empty() {
            continue;
        }
        let frames = sample.pcm16.len() / 2;
        let loop_str = if sample.has_loop() {
            format!(", loop {}-{}", sample.loop_start, sample.loop_start + sample.loop_length)
        } else {
            String::new()
        };
        let name = if sample.name.is_empty() { "(unnamed)".to_string() } else { sample.name.clone() };
        println!(
            "  {:2}. {:<22} {:6} frames  vol={:2}  rate={}Hz{}",
            sample.index, name, frames, sample.volume, sample.sample_rate_hz, loop_str
        );
    }
    Ok(())
}

fn extract_samples_cmd(module_path: &std::path::Path, output_dir: &std::path::Path, verbose: bool) -> Result<(), String> {
    let module = load_module(module_path)?;
    if verbose {
        print_module_summary(&module);
    }
    std::fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    let mut count = 0;
    for sample in &module.samples {
        if sample.is_empty() {
            continue;
        }
        let out_path = output_dir.join(sample_wav_filename(sample));
        write_sample_wav(sample, &out_path).map_err(|e| e.to_string())?;
        println!("wrote {}", out_path.display());
        if verbose {
            let frames = sample.pcm16.len() / 2;
            let loop_str = if sample.has_loop() {
                format!("loop {}-{}", sample.loop_start, sample.loop_start + sample.loop_length)
            } else {
                "no loop".to_string()
            };
            println!("  Verbose: {} frames, {}Hz, volume={}, {}", frames, sample.sample_rate_hz, sample.volume, loop_str);
        }
        count += 1;
    }

    println!("\nExtracted {} sample(s) to {}", count, output_dir.display());
    Ok(())
}

fn extract_midi_cmd(module_path: &std::path::Path, output_path: &std::path::Path, verbose: bool) -> Result<(), String> {
    let module = load_module(module_path)?;
    if verbose {
        print_module_summary(&module);
        print_effects_report(&module);
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_midi(&module, output_path).map_err(|e| e.to_string())?;
    println!("wrote {}", output_path.display());
    if verbose {
        let song = compute_song_events(&module);
        let total_notes: usize = song.notes_by_sample.values().map(|v| v.len()).sum();
        println!(
            "Verbose: {} note(s), {} tempo change(s), {:.2} beats total",
            total_notes,
            song.tempo_changes.len(),
            song.total_beats
        );
    }
    Ok(())
}

fn convert_cmd(
    module_path: &std::path::Path, template_path: Option<&std::path::Path>, output_path: &std::path::Path,
    amiga_panning: AmigaPanning, verbose: bool,
) -> Result<(), String> {
    let module = load_module(module_path)?;
    if verbose {
        print_module_summary(&module);
        print_effects_report(&module);
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let template_bytes: Vec<u8> = match template_path {
        Some(p) => std::fs::read(p).map_err(|e| format!("failed to read {}: {e}", p.display()))?,
        None => crate::export::als::default_template_bytes().to_vec(),
    };
    export_als(&module, output_path, &template_bytes, amiga_panning)?;
    println!("wrote {}", output_path.display());
    println!("wrote samples to {}", output_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("Samples").join("Imported").display());
    if verbose {
        let song = compute_song_events(&module);
        // A sample triggered on several overlapping tracker channels gets more than one
        // voice/track (see the voice-assignment pass in export::notes::compute_song_events) —
        // count those too, so this matches the number of tracks export_als actually wrote.
        let track_count: usize =
            song.notes_by_sample.values().map(|notes| notes.iter().map(|n| n.voice + 1).max().unwrap_or(1)).sum();
        let total_notes: usize = song.notes_by_sample.values().map(|v| v.len()).sum();
        println!(
            "Verbose: {} track(s), {} note(s), {} tempo change(s), {:.2} beats total",
            track_count,
            total_notes,
            song.tempo_changes.len(),
            song.total_beats
        );
    }
    Ok(())
}

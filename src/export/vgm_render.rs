//! Renders a parsed VGM/VGZ file to audio by actually driving the chip emulators
//! (chips::ay8910, chips::ym2413, chips::scc) through its register-write log — there's no
//! stored waveform data to extract the way there is for a tracker module's samples; the sound
//! only exists once synthesized.

use std::path::Path;

use crate::chips::ay8910::Ayumi;
use crate::chips::scc::Scc;
use crate::chips::ym2413::Opll;
use crate::formats::vgm::{Chip, VgmFile};

/// VGM "wait" commands are always expressed in units of this rate, per the format spec,
/// regardless of what rate a player actually renders at — using it as our own output rate
/// too means every wait converts straight to an output-sample count with no resampling.
pub const VGM_SAMPLE_RATE: u32 = 44100;

/// Divisor bringing chips::scc::Scc's raw output into roughly the same perceived loudness as
/// chips::ay8910::Ayumi's — see the long comment at its one call site in `render_internal` for
/// why this is a calibrated guess rather than a value the format itself specifies.
const SCC_UNIT_SCALE: f32 = 45.0;

/// Owns an Ayumi instance plus the raw AY8910 register values (0-13) needed to reconstruct
/// each register's *other* half whenever just one changes — e.g. the tone period is split
/// across two registers (fine/coarse), and the mixer's per-channel enable bits (register 7)
/// and envelope-on bit (registers 8-10) both feed into a single `set_mixer` call.
struct Ay8910Driver {
    ay: Ayumi,
    regs: [u8; 14],
}

impl Ay8910Driver {
    fn new(is_ym: bool, clock_rate: f64, sample_rate: u32) -> Self {
        Ay8910Driver { ay: Ayumi::new(is_ym, clock_rate, sample_rate), regs: [0; 14] }
    }

    fn recompute_mixer(&mut self, ch: usize) {
        let t_off = (self.regs[7] >> ch) & 1;
        let n_off = (self.regs[7] >> (ch + 3)) & 1;
        let e_on = (self.regs[8 + ch] >> 4) & 1;
        self.ay.set_mixer(ch, t_off as i32, n_off as i32, e_on as i32);
    }

    fn mute_all(&mut self) {
        self.ay.mute_all();
    }

    fn write(&mut self, reg: u8, value: u8) {
        if reg as usize >= self.regs.len() {
            return; // registers 14/15 are I/O ports, not audio-relevant
        }
        self.regs[reg as usize] = value;
        match reg {
            0 | 1 => {
                let period = self.regs[0] as i32 | ((self.regs[1] as i32 & 0x0F) << 8);
                self.ay.set_tone(0, period);
            }
            2 | 3 => {
                let period = self.regs[2] as i32 | ((self.regs[3] as i32 & 0x0F) << 8);
                self.ay.set_tone(1, period);
            }
            4 | 5 => {
                let period = self.regs[4] as i32 | ((self.regs[5] as i32 & 0x0F) << 8);
                self.ay.set_tone(2, period);
            }
            6 => self.ay.set_noise(value as i32 & 0x1F),
            7 => {
                for ch in 0..3 {
                    self.recompute_mixer(ch);
                }
            }
            8 | 9 | 10 => {
                let ch = (reg - 8) as usize;
                self.recompute_mixer(ch);
                self.ay.set_volume(ch, (value & 0x0F) as i32);
            }
            11 | 12 => {
                let period = self.regs[11] as i32 | ((self.regs[12] as i32) << 8);
                self.ay.set_envelope(period);
            }
            13 => self.ay.set_envelope_shape(value as i32 & 0x0F),
            _ => {}
        }
    }
}

pub struct RenderedAudio {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// Core render loop shared by `render()` and `render_stems()` — `setup` runs once, right
/// after the chips are created, to configure channel muting/soloing for a stem (or leave
/// everything audible for the full mix); register writes are always applied to *every*
/// channel normally regardless, only the final output summing is affected by muting, so
/// isolating one channel never changes how any other channel's own state evolves.
fn render_internal(vgm: &VgmFile, setup: impl FnOnce(&mut Opll, &mut Ay8910Driver, &mut Scc)) -> RenderedAudio {
    let mut opll = Opll::new(vgm.ym2413_clock.max(1), VGM_SAMPLE_RATE);
    let mut ay = Ay8910Driver::new(vgm.ay8910_is_ym, vgm.ay8910_clock.max(1) as f64, VGM_SAMPLE_RATE);
    let mut scc = Scc::new(vgm.scc_clock.max(1), VGM_SAMPLE_RATE);
    setup(&mut opll, &mut ay, &mut scc);

    let total = vgm.total_samples as usize;
    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];

    let mut write_idx = 0usize;
    for (n, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        while write_idx < vgm.writes.len() && vgm.writes[write_idx].at_sample as usize <= n {
            let w = &vgm.writes[write_idx];
            match w.chip {
                Chip::Ym2413 => opll.write_reg(w.reg as u32, w.value),
                Chip::Ay8910 => ay.write(w.reg, w.value),
                Chip::Scc => scc.write(w.port, w.reg, w.value),
                // Not rendered to WAV — YM3526/YM3812 are approximated via Ableton's own
                // Operator instrument instead (export::vgm_operator), not a software chip
                // emulator.
                Chip::Ym3526 | Chip::Ym3812 => {}
            }
            write_idx += 1;
        }
        let ym_sample = opll.calc() as f32 / 32768.0;
        ay.ay.process();
        ay.ay.remove_dc();
        // SCC is a mono chip (no per-channel panning of its own, unlike AY8910). VGM has no
        // concept of cross-chip mixing levels — on real hardware that balance was set by an
        // analog summing circuit on the cartridge, which isn't recorded anywhere in the file
        // — so there's no "correct" scale to derive here, only a judgment call. MAME's own
        // internal mixing weight for this chip (dividing by 1024) turned out to render SCC
        // ~35-40x quieter than AY8910 in practice, which is implausible for a chip Konami's
        // MSX games typically use as the lead melodic voice. SCC_UNIT_SCALE was picked
        // instead by matching a single max-volume tone's peak amplitude on each chip (see the
        // calibration in this module's tests) — reasonable, but still a guess pending a real
        // listen (this project's usual bar for anything audio-subjective).
        let scc_sample = scc.calc() as f32 / SCC_UNIT_SCALE;
        *l = ym_sample + ay.ay.left as f32 + scc_sample;
        *r = ym_sample + ay.ay.right as f32 + scc_sample;
    }

    RenderedAudio { sample_rate: VGM_SAMPLE_RATE, left, right }
}

/// Renders the full, single play-through (0..total_samples) of the VGM — not the
/// indefinitely-repeating loop some players do; total_samples already includes the intro
/// plus one full loop, which reads as a complete, unsurprising rendition on its own. Writes
/// to chips other than YM2413/AY8910/K051649-SCC (see VgmFile::unsupported_commands) are
/// silently skipped, same as this project's tracker-effect philosophy: convert what's
/// implemented, don't guess at the rest.
pub fn render(vgm: &VgmFile) -> RenderedAudio {
    render_internal(vgm, |_, _, _| {})
}

const AY_CHANNEL_NAMES: [&str; 3] = ["AY-A", "AY-B", "AY-C"];
const SCC_CHANNEL_NAMES: [&str; 5] = ["SCC-1", "SCC-2", "SCC-3", "SCC-4", "SCC-5"];

fn is_silent(audio: &RenderedAudio) -> bool {
    audio.left.iter().chain(audio.right.iter()).all(|&x| x == 0.0)
}

/// One isolated voice's render, named for display/track-naming purposes (e.g. "AY-A",
/// "YM-3", "YM-BD"). Only non-silent stems are returned — a channel the song never actually
/// uses (including every YM2413 rhythm voice, unless the song enables rhythm mode) is
/// omitted rather than delivering a dozen empty tracks.
pub struct Stem {
    pub name: String,
    pub audio: RenderedAudio,
}

/// Renders one isolated stem per AY8910 tone channel (A/B/C), per K051649/SCC channel (1-5),
/// and per YM2413 voice (the 9 FM channels, plus the 5 rhythm-mode percussion voices which
/// replace channels 7-9 when rhythm mode is enabled — rendering both unconditionally and
/// dropping whichever half turns out silent handles a song that switches between the two
/// modes without needing to detect that switch ourselves).
pub fn render_stems(vgm: &VgmFile) -> Vec<Stem> {
    let mut stems = Vec::new();

    for (i, name) in AY_CHANNEL_NAMES.iter().enumerate() {
        let audio = render_internal(vgm, |opll, ay, scc| {
            opll.set_mask(u32::MAX);
            ay.ay.solo(i);
            scc.set_mask(crate::chips::scc::ALL_CHANNELS_MASK);
        });
        if !is_silent(&audio) {
            stems.push(Stem { name: name.to_string(), audio });
        }
    }

    for (i, name) in SCC_CHANNEL_NAMES.iter().enumerate() {
        let audio = render_internal(vgm, |opll, ay, scc| {
            opll.set_mask(u32::MAX);
            ay.mute_all();
            scc.solo(i);
        });
        if !is_silent(&audio) {
            stems.push(Stem { name: name.to_string(), audio });
        }
    }

    for i in 0..9 {
        let audio = render_internal(vgm, |opll, ay, scc| {
            opll.set_mask(Opll::solo_ch_mask(i));
            ay.mute_all();
            scc.set_mask(crate::chips::scc::ALL_CHANNELS_MASK);
        });
        if !is_silent(&audio) {
            stems.push(Stem { name: format!("YM-{}", i + 1), audio });
        }
    }

    for (mask, name) in [
        (Opll::MASK_BD, "YM-BD"),
        (Opll::MASK_HH, "YM-HH"),
        (Opll::MASK_SD, "YM-SD"),
        (Opll::MASK_TOM, "YM-TOM"),
        (Opll::MASK_CYM, "YM-CYM"),
    ] {
        let audio = render_internal(vgm, |opll, ay, scc| {
            opll.set_mask(Opll::solo_rhythm_mask(mask));
            ay.mute_all();
            scc.set_mask(crate::chips::scc::ALL_CHANNELS_MASK);
        });
        if !is_silent(&audio) {
            stems.push(Stem { name: name.to_string(), audio });
        }
    }

    stems
}

/// Cuts out `[start, end)` (in samples) of a rendered stem — used to split a stem's audio at
/// the VGM file's own declared loop point into an "intro" segment (played once) and a "loop"
/// segment (the repeating pattern), rather than exporting one long undifferentiated clip.
pub fn slice(audio: &RenderedAudio, start: usize, end: usize) -> RenderedAudio {
    RenderedAudio { sample_rate: audio.sample_rate, left: audio.left[start..end].to_vec(), right: audio.right[start..end].to_vec() }
}

pub fn peak(audio: &RenderedAudio) -> f32 {
    audio.left.iter().chain(audio.right.iter()).fold(0.0f32, |acc, &x| acc.max(x.abs()))
}

/// Writes a rendered stereo mix to a 16-bit WAV using the given linear `gain`. Deliberately
/// *not* self-normalizing: when writing a master mix alongside its stems, every file must
/// share the *same* gain (computed once from the master's own peak via `peak()`) so the
/// stems' relative loudness — the whole point of having them — survives; independently
/// peak-normalizing each stem would make a quiet background voice as loud as the lead.
pub fn write_wav(audio: &RenderedAudio, path: &Path, gain: f32) -> std::io::Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: audio.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(std::io::Error::other)?;
    for (&l, &r) in audio.left.iter().zip(audio.right.iter()) {
        writer.write_sample(((l * gain).clamp(-1.0, 1.0) * 32767.0) as i16).map_err(std::io::Error::other)?;
        writer.write_sample(((r * gain).clamp(-1.0, 1.0) * 32767.0) as i16).map_err(std::io::Error::other)?;
    }
    writer.finalize().map_err(std::io::Error::other)?;
    Ok(())
}

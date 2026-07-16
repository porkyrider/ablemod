//! Extracts each OPL-family channel's register-write log as MIDI note data plus a static
//! 2-operator FM "patch" snapshot, for driving an Ableton Operator instrument — covers both
//! YM3526 (OPL) and YM3812 (OPL2), which share an identical register map for every field this
//! module reads (see extract_channels's own comment). Both chips now also get a real,
//! bit-accurate WAV render (libvgm's own fmopl.c, see export::vgm_render/build.rs), so this
//! Operator export is an *approximation* kept alongside it, the same relationship
//! export::vgm_wavetable already has with SCC's own WAV render — not, as it once was before
//! that render existed, the only audible result for these chips.
//!
//! v1 deliberately keeps a smaller scope than export::vgm_wavetable's SCC handling:
//!
//! - **FM only.** Each channel's operator pair is always wired as modulator→carrier (operator 1
//!   modulates operator 2, only operator 2 is audible) regardless of that channel's own
//!   Connection register bit — a channel actually using "additive" mode (both operators audible,
//!   summed) will sound different from the original, not broken. Additive is a minority case in
//!   practice (mostly organ/bell-style patches on some games); FM covers the common melodic case.
//! - **No rhythm mode.** Channels 6-8 are always decoded as ordinary melodic FM channels even if
//!   the song ever sets the global rhythm-mode bit (register 0xBD bit 5), which on real hardware
//!   repurposes them as fixed bass/snare/tom/cymbal/hihat drum voices with entirely different
//!   register semantics. A rhythm-mode song will produce wrong content specifically on those 3
//!   channels rather than being detected and handled.
//! - **Hard retrigger, no pitch-bend absorption.** Unlike SCC's bend-vs-new-note logic, *any*
//!   change to a held note's F-Number/Block always closes the current note and opens a new one
//!   at the new pitch — there's no equivalent of SCC's vibrato-as-Detune-automation here. Arcade
//!   FM music tends to lean on the chip's own hardware vibrato LFO (a global, chip-wide register,
//!   not a per-note effect) rather than per-note pitch bends, so this is expected to matter less
//!   here than it did for MSX chiptune SCC data.
//! - **Static patch per channel, not per-note.** A channel's timbre (operator Multiple/Feedback/
//!   envelope rates) is snapshotted once, from whichever register state is active at that
//!   channel's *first* note, and baked directly into the cloned Operator device — it is never
//!   re-automated even if a game later reuses the same OPL channel for a differently-configured
//!   instrument. Only the carrier's Total Level (loudness) is tracked continuously as gain
//!   automation, mirroring SCC's own volume-envelope handling, since TL rewrites mid-note (to
//!   fake a volume envelope/tremolo) are common in FM game engines.
//! - **Envelope rate→time mapping is a labeled approximation**, not a hardware-derived formula —
//!   see opl_rate_to_ms. Operator's own linear-segment ADSR engine can't reproduce OPL's actual
//!   logarithmic per-sample envelope curve exactly regardless, so exact timing fidelity isn't
//!   achievable here even in principle.
//!
//! VGM writes are processed in groups sharing the same `at_sample`, same rationale as
//! export::vgm_wavetable: a driver commonly rewrites several operator registers in one
//! zero-wait burst when loading an instrument, and evaluating key-on/off/retrigger decisions
//! only once against the group's *final* state avoids spurious partial-instrument snapshots.

use crate::export::als::ClippedNote;
use crate::export::vgm_wavetable::{hz_to_midi_note, samples_to_beats};
use crate::formats::vgm::{Chip, VgmFile};

pub(crate) const OPL_CHANNELS: usize = 9;

/// Register offset of each channel's first ("modulator") operator — the second ("carrier")
/// operator is always this plus 3. Verbatim from the OPL/OPL2 register map (Jeffrey S. Lee's
/// "Programming the AdLib/Sound Blaster FM Music Chips", the standard reference for this
/// register layout, which YM3526 shares with the later YM3812/OPL2 for every register used
/// here).
const OPERATOR1_OFFSET: [u8; OPL_CHANNELS] = [0x00, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x10, 0x11, 0x12];

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Patch {
    /// Operator "Multiple" code (0-15), used directly as Operator's own Coarse ratio — both
    /// represent the same "harmonic multiple of the note's fundamental, 0=half" concept, though
    /// this specific correspondence is inferred from the two devices' matching value ranges
    /// rather than confirmed against real Ableton behavior.
    pub(crate) mod_coarse: u8,
    pub(crate) car_coarse: u8,
    /// 0-7 (raw register value) — see feedback_to_percent for the conversion to Operator's 0-100
    /// Feedback range.
    pub(crate) feedback: u8,
    pub(crate) mod_attack_ms: f64,
    pub(crate) mod_decay_ms: f64,
    /// Linear 0..1 gain, not the register's raw 4-bit code — see opl_sustain_level_to_gain.
    pub(crate) mod_sustain_gain: f64,
    pub(crate) mod_release_ms: f64,
    /// The modulator's own Total Level acts as FM depth/index (how strongly it modulates the
    /// carrier), not audible loudness — baked in statically like the rest of the patch, unlike
    /// the carrier's TL which drives continuous gain automation instead (see ChannelTrack::gains).
    pub(crate) mod_gain: f64,
    pub(crate) car_attack_ms: f64,
    pub(crate) car_decay_ms: f64,
    pub(crate) car_sustain_gain: f64,
    pub(crate) car_release_ms: f64,
}

pub(crate) struct ChannelTrack {
    /// Always Some once `notes` is non-empty — the patch captured from the channel's first note.
    pub(crate) patch: Option<Patch>,
    pub(crate) notes: Vec<ClippedNote>,
    /// (at_beat, gain 0..1, glide) — same convention as vgm_wavetable::ChannelTrack::gains,
    /// tracking the carrier operator's Total Level register across a held note.
    pub(crate) gains: Vec<(f64, f64, bool)>,
}

/// Fastest/slowest envelope segment times this project will ever emit, in milliseconds — endpoints
/// of the log-interpolated approximation in opl_rate_to_ms, chosen to span "near-instant" to
/// "several seconds" without hugging either of Operator's own hard field limits.
const ENV_TIME_MIN_MS: f64 = 0.5;
const ENV_TIME_MAX_MS: f64 = 8000.0;

/// Approximates a 4-bit OPL envelope rate code (0=slowest/never completes, 15=fastest) as a
/// real-world duration in milliseconds. This is *not* derived from the chip's actual envelope
/// generator (a shared per-sample logarithmic table with its own key-scale-rate adjustments) —
/// no simple closed-form public formula for that was found, and Operator's own linear-segment
/// ADSR engine couldn't reproduce that exact curve regardless. Instead this log-interpolates
/// evenly across [ENV_TIME_MIN_MS, ENV_TIME_MAX_MS] over the 16 rate codes: monotonic (higher
/// rate always yields a shorter time, matching real hardware's own ordering) and spans a
/// plausible instant-to-seconds range, but the specific per-code value is a rough approximation.
fn opl_rate_to_ms(rate: u8) -> f64 {
    if rate == 0 {
        return ENV_TIME_MAX_MS;
    }
    let t = rate.min(15) as f64 / 15.0;
    (ENV_TIME_MAX_MS.ln() + (ENV_TIME_MIN_MS.ln() - ENV_TIME_MAX_MS.ln()) * t).exp()
}

/// Sustain Level is a 4-bit code stepping ~3dB per unit (0=loudest/0dB, 15=silence) — distinct
/// from Total Level's ~0.75dB/step (see opl_total_level_to_gain), confirmed against the OPL
/// register reference's own separate description of the two fields.
fn opl_sustain_level_to_gain(sl: u8) -> f64 {
    let sl = sl.min(15);
    if sl == 15 { 0.0003162277571 } else { 10f64.powf(-3.0 * sl as f64 / 20.0) }
}

/// Total Level is a 6-bit code stepping 0.75dB per unit (0=loudest, 63=silence).
fn opl_total_level_to_gain(tl: u8) -> f64 {
    10f64.powf(-0.75 * tl.min(63) as f64 / 20.0)
}

fn velocity_from_total_level(tl: u8) -> i32 {
    (opl_total_level_to_gain(tl) * 127.0).round().clamp(1.0, 127.0) as i32
}

/// OPL Feedback is a 3-bit register value (0-7); Operator's own Feedback parameter is a 0-100
/// percentage — linearly rescaled, no finer hardware correspondence to preserve.
pub(crate) fn feedback_to_percent(feedback: u8) -> f64 {
    feedback.min(7) as f64 * 100.0 / 7.0
}

#[derive(Clone, Copy, Default)]
struct OperatorRegs {
    /// Bits 3-0 of the AM/VIB/EGT/KSR/Multiple register (0x20-0x35) — the other bits (AM/VIB
    /// depth-enable, EG sustain-type, key-scale-rate) aren't modeled in v1, see this module's
    /// own doc comment.
    multiple: u8,
    total_level: u8, // 0x40-0x55, bits 5-0 (KSL in bits 7-6 isn't modeled)
    attack_rate: u8, // 0x60-0x75, bits 7-4
    decay_rate: u8,  // 0x60-0x75, bits 3-0
    sustain_level: u8, // 0x80-0x95, bits 7-4
    release_rate: u8,  // 0x80-0x95, bits 3-0
}

struct OpenNote {
    start_sample: u64,
    pitch: i32,
    velocity: i32,
    /// (at_sample, carrier total_level register), chronological, always has at least the note's
    /// opening value.
    tl_points: Vec<(u64, u8)>,
}

struct ChannelState {
    fnum: u16,
    block: u8,
    key: bool,
    modulator: OperatorRegs,
    carrier: OperatorRegs,
    feedback: u8, // 0xC0-0xC8, bits 3-1 (Connection in bit 0 isn't modeled — see doc comment)
    patch: Option<Patch>,
    open: Option<OpenNote>,
    notes: Vec<ClippedNote>,
    gains: Vec<(f64, f64, bool)>,
}

impl ChannelState {
    fn new() -> Self {
        ChannelState {
            fnum: 0,
            block: 0,
            key: false,
            modulator: OperatorRegs::default(),
            carrier: OperatorRegs::default(),
            feedback: 0,
            patch: None,
            open: None,
            notes: Vec::new(),
            gains: Vec::new(),
        }
    }

    fn freq_hz(&self, clock: f64) -> f64 {
        clock * self.fnum as f64 / (2f64.powi(20 - self.block as i32) * 72.0)
    }

    fn capture_patch(&mut self) -> Patch {
        Patch {
            mod_coarse: self.modulator.multiple,
            car_coarse: self.carrier.multiple,
            feedback: self.feedback,
            mod_attack_ms: opl_rate_to_ms(self.modulator.attack_rate),
            mod_decay_ms: opl_rate_to_ms(self.modulator.decay_rate),
            mod_sustain_gain: opl_sustain_level_to_gain(self.modulator.sustain_level),
            mod_release_ms: opl_rate_to_ms(self.modulator.release_rate),
            mod_gain: opl_total_level_to_gain(self.modulator.total_level),
            car_attack_ms: opl_rate_to_ms(self.carrier.attack_rate),
            car_decay_ms: opl_rate_to_ms(self.carrier.decay_rate),
            car_sustain_gain: opl_sustain_level_to_gain(self.carrier.sustain_level),
            car_release_ms: opl_rate_to_ms(self.carrier.release_rate),
        }
    }

    fn open_note(&mut self, at_sample: u64, clock: f64) {
        let freq_hz = self.freq_hz(clock);
        if freq_hz <= 0.0 {
            return;
        }
        if self.patch.is_none() {
            self.patch = Some(self.capture_patch());
        }
        self.open = Some(OpenNote {
            start_sample: at_sample,
            pitch: hz_to_midi_note(freq_hz),
            velocity: velocity_from_total_level(self.carrier.total_level),
            tl_points: vec![(at_sample, self.carrier.total_level)],
        });
    }

    fn close_note(&mut self, at_sample: u64, tempo_bpm: f64) {
        let Some(open) = self.open.take() else { return };
        if at_sample <= open.start_sample {
            return;
        }
        let start_beat = samples_to_beats(open.start_sample, tempo_bpm);
        let end_beat = samples_to_beats(at_sample, tempo_bpm);
        self.notes.push(ClippedNote { start_beat, duration_beat: end_beat - start_beat, velocity: open.velocity, pitch: open.pitch });
        for (idx, &(at_sample, tl)) in open.tl_points.iter().enumerate() {
            let at_beat = samples_to_beats(at_sample, tempo_bpm);
            self.gains.push((at_beat, opl_total_level_to_gain(tl), idx > 0));
        }
    }
}

fn apply_operator_write(regs: &mut OperatorRegs, reg_class: u8, value: u8) {
    match reg_class {
        0x20 => regs.multiple = value & 0x0F,
        0x40 => regs.total_level = value & 0x3F,
        0x60 => {
            regs.attack_rate = value >> 4;
            regs.decay_rate = value & 0x0F;
        }
        0x80 => {
            regs.sustain_level = value >> 4;
            regs.release_rate = value & 0x0F;
        }
        _ => {}
    }
}

/// Extracts all 9 channels' note/gain data + static patch, for one specific OPL-family chip
/// (`chip`/`clock`), in a single pass over the VGM's writes. YM3812 (OPL2) is register-
/// compatible with YM3526 for every field this module reads (see formats::vgm::parse's own
/// comment on why they're still kept as two independent chip presences rather than merged) —
/// this same function decodes either one, just filtered to that chip's own writes. A channel
/// the song never actually keys on comes back with empty `notes` and `patch: None` — the
/// caller is responsible for skipping those (same convention as
/// export::vgm_wavetable::extract_channels and export::vgm_render::render_stems).
pub(crate) fn extract_channels(vgm: &VgmFile, chip: Chip, clock: u32, tempo_bpm: f64) -> [ChannelTrack; OPL_CHANNELS] {
    let clock = clock.max(1) as f64;
    let mut state: [ChannelState; OPL_CHANNELS] = std::array::from_fn(|_| ChannelState::new());
    let operator_channel: std::collections::HashMap<u8, (usize, bool)> = OPERATOR1_OFFSET
        .iter()
        .enumerate()
        .flat_map(|(ch, &off)| [(off, (ch, true)), (off + 3, (ch, false))])
        .collect();

    let mut i = 0;
    while i < vgm.writes.len() {
        let at_sample = vgm.writes[i].at_sample;
        let was_open: [bool; OPL_CHANNELS] = std::array::from_fn(|ch| state[ch].open.is_some());
        let fnum_before: [u16; OPL_CHANNELS] = std::array::from_fn(|ch| state[ch].fnum);
        let block_before: [u8; OPL_CHANNELS] = std::array::from_fn(|ch| state[ch].block);
        let tl_before: [u8; OPL_CHANNELS] = std::array::from_fn(|ch| state[ch].carrier.total_level);
        let mut pending_key: [Option<bool>; OPL_CHANNELS] = [None; OPL_CHANNELS];

        let mut j = i;
        while j < vgm.writes.len() && vgm.writes[j].at_sample == at_sample {
            let w = &vgm.writes[j];
            if w.chip == chip {
                match w.reg {
                    0x20..=0x35 | 0x40..=0x55 | 0x60..=0x75 | 0x80..=0x95 => {
                        // Each register class (0x20/0x40/0x60/0x80) repeats the exact same
                        // operator-offset layout in its own low 5 bits — see OPERATOR1_OFFSET.
                        let offset = w.reg & 0x1F;
                        if let Some(&(ch, is_modulator)) = operator_channel.get(&offset) {
                            let reg_class = w.reg & 0xE0;
                            let regs = if is_modulator { &mut state[ch].modulator } else { &mut state[ch].carrier };
                            apply_operator_write(regs, reg_class, w.value);
                        }
                    }
                    0xA0..=0xA8 => {
                        let ch = (w.reg - 0xA0) as usize;
                        state[ch].fnum = (state[ch].fnum & 0x300) | w.value as u16;
                    }
                    0xB0..=0xB8 => {
                        let ch = (w.reg - 0xB0) as usize;
                        state[ch].fnum = (state[ch].fnum & 0x0FF) | (((w.value & 0x03) as u16) << 8);
                        state[ch].block = (w.value >> 2) & 0x07;
                        pending_key[ch] = Some(w.value & 0x20 != 0);
                    }
                    0xC0..=0xC8 => {
                        let ch = (w.reg - 0xC0) as usize;
                        state[ch].feedback = (w.value >> 1) & 0x07;
                    }
                    _ => {}
                }
            }
            j += 1;
        }

        for (ch, s) in state.iter_mut().enumerate() {
            let Some(new_key) = pending_key[ch] else { continue };
            if new_key == s.key {
                continue;
            }
            s.key = new_key;
            if new_key {
                s.close_note(at_sample, tempo_bpm); // in case a stray retrigger left one open
                s.open_note(at_sample, clock);
            } else {
                s.close_note(at_sample, tempo_bpm);
            }
        }

        // Any pitch change on an already-sounding note is a hard retrigger in v1 (see this
        // module's own doc comment) — unlike SCC there's no bend/vibrato absorption here.
        for (ch, s) in state.iter_mut().enumerate() {
            if was_open[ch] && s.key && s.open.is_some() && (s.fnum != fnum_before[ch] || s.block != block_before[ch]) {
                s.close_note(at_sample, tempo_bpm);
                s.open_note(at_sample, clock);
            } else if was_open[ch] && s.open.is_some() && s.carrier.total_level != tl_before[ch] {
                if let Some(open) = &mut s.open {
                    open.tl_points.push((at_sample, s.carrier.total_level));
                }
            }
        }

        i = j;
    }

    for s in &mut state {
        s.close_note(vgm.total_samples, tempo_bpm);
    }

    state.map(|s| ChannelTrack { patch: s.patch, notes: s.notes, gains: s.gains })
}


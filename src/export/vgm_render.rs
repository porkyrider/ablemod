//! Renders a parsed VGM/VGZ file to audio using libvgm's own player (chips::player_ffi) —
//! a second, independent parse of the same raw bytes formats::vgm::parse already extracted
//! (see VgmFile::raw_data's own comment), driving the exact same chip cores
//! chips::scc/ay8910/ym2413 wrap, but through libvgm's own device/mixing/muting machinery
//! rather than this project walking VgmFile::writes and calling those wrappers directly (the
//! way it did before this migration — see git history for that version if a comparison is ever
//! needed). Per-stem isolation uses libvgm's own per-channel mute mask
//! (chips::player_ffi::Player::set_mute), the same DEV_DEF::SetMuteMask mechanism
//! chips::scc/ay8910/ym2413's own set_mask methods already use.
//!
//! YM3526/YM3812 (OPL/OPL2) are rendered here too — a real, bit-accurate WAV render via
//! libvgm's own fmopl.c (MAME's core; see build.rs's own licensing note), alongside
//! export::vgm_operator's Ableton-Operator approximation for the same channels (WAV = source
//! of truth, the native-instrument track is an editable bonus — the same relationship
//! export::vgm_wavetable already has with SCC's own WAV render).

use std::path::Path;

use crate::chips::player_ffi::{dev_id, Player};
use crate::formats::vgm::VgmFile;

/// VGM "wait" commands are always expressed in units of this rate, per the format spec,
/// regardless of what rate a player actually renders at — using it as our own output rate
/// too means every wait converts straight to an output-sample count with no resampling.
pub const VGM_SAMPLE_RATE: u32 = 44100;

/// libvgm's player mixes at its own internal per-chip volume balance (not something this
/// project calibrates by hand anymore — see git history for the old AY_UNIT_SCALE/
/// SCC_UNIT_SCALE constants this replaced). Its raw i32 output has real headroom above a
/// 16-bit range; this is just *a* fixed, consistent divisor to get into a roughly -1.0..1.0
/// float range before write_wav's own peak-based gain normalization takes over (which is
/// scale-invariant by construction — any consistent divisor here works equally well).
const NATIVE_UNIT_SCALE: f32 = 65536.0;

pub struct RenderedAudio {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

// Every FM chip this project links (YM2413, YM3526, YM3812) shares the same 14-channel layout:
// 0-8 are the 9 FM voices, 9-13 are the rhythm-mode percussion voices that replace channels 6-8
// when rhythm mode is enabled — bit positions match libvgm's own DeviceChannelNames ordering
// for these chips (confirmed directly against emu/cores/2413intf.c and oplintf.c).
const MASK_BD: u32 = 1 << 9;
const MASK_SD: u32 = 1 << 10;
const MASK_TOM: u32 = 1 << 11;
const MASK_CYM: u32 = 1 << 12;
const MASK_HH: u32 = 1 << 13;
const ALL_FM_CHANNELS: u32 = ((1 << 9) - 1) | MASK_BD | MASK_SD | MASK_TOM | MASK_CYM | MASK_HH;
const ALL_AY_CHANNELS: u32 = (1 << 3) - 1;
const ALL_SCC_CHANNELS: u32 = (1 << 5) - 1;

// Chips added since the initial libvgm migration (see README.md) — WAV-only, no
// Wavetable/Operator-style native-instrument approximation. YM2608/YM2610 (OPNA/OPNB) share
// one 13-channel layout (6 FM + 6 ADPCM-A + 1 ADPCM-B, confirmed against
// emu/cores/opnintf.c's own DeviceChannelNames_YM2608/2610 — identical for both). RF5C68 and
// RF5C164 share one DEVID (see chips::player_ffi::dev_id::RF5C68's own comment) and are told
// apart by PLR_DEV_ID *instance* (0 = RF5C68, 1 = RF5C164), not by dev_id.
const ALL_YM2612_CHANNELS: u32 = (1 << 7) - 1;
const ALL_YM2151_CHANNELS: u32 = (1 << 8) - 1;
const ALL_YM2203_CHANNELS: u32 = (1 << 3) - 1;
const ALL_OPNA_CHANNELS: u32 = (1 << 13) - 1;
const ALL_SEGAPCM_CHANNELS: u32 = (1 << 16) - 1;
const ALL_RF5C_CHANNELS: u32 = (1 << 8) - 1;
const ALL_GB_CHANNELS: u32 = (1 << 4) - 1;
const ALL_NES_CHANNELS: u32 = (1 << 5) - 1;

/// Mutes every channel on every chip this project links — stem rendering starts from this and
/// unmutes just the one channel/chip it wants, rather than every call site separately
/// enumerating "mute these other N chips" (error-prone to keep in sync as chips are added).
/// Muting a chip the file never actually instantiates (e.g. calling this on YM2612 for a file
/// with no YM2612 data at all) is a harmless no-op — libvgm's own SetDeviceMuting silently
/// does nothing for a DEV_ID/instance pair it never created a device for.
fn mute_everything(player: &mut Player) {
    player.set_mute(dev_id::YM2413, 0, ALL_FM_CHANNELS);
    player.set_mute(dev_id::AY8910, 0, ALL_AY_CHANNELS);
    player.set_mute(dev_id::K051649, 0, ALL_SCC_CHANNELS);
    player.set_mute(dev_id::YM3526, 0, ALL_FM_CHANNELS);
    player.set_mute(dev_id::YM3812, 0, ALL_FM_CHANNELS);
    player.set_mute(dev_id::YM2612, 0, ALL_YM2612_CHANNELS);
    player.set_mute(dev_id::YM2151, 0, ALL_YM2151_CHANNELS);
    player.set_mute(dev_id::YM2203, 0, ALL_YM2203_CHANNELS);
    player.set_mute(dev_id::YM2608, 0, ALL_OPNA_CHANNELS);
    player.set_mute(dev_id::YM2610, 0, ALL_OPNA_CHANNELS);
    player.set_mute(dev_id::SEGAPCM, 0, ALL_SEGAPCM_CHANNELS);
    player.set_mute(dev_id::RF5C68, 0, ALL_RF5C_CHANNELS);
    player.set_mute(dev_id::RF5C68, 1, ALL_RF5C_CHANNELS);
    player.set_mute(dev_id::GB_DMG, 0, ALL_GB_CHANNELS);
    player.set_mute(dev_id::NES_APU, 0, ALL_NES_CHANNELS);
}

/// libvgm's own player parses `VgmFile::raw_data` completely independently of
/// formats::vgm::parse (see this module's own doc comment) — it does *not* benefit from that
/// parser's own K051649-clock correction (see formats::vgm.rs's own comment: real rips have
/// been found storing the AY8910's clock, exactly half the real SCC hardware clock, in the
/// header's K051649 clock field by mistake, which silently played every SCC channel one octave
/// flat before that fix — confirmed directly by ear on this exact file after this player
/// migration reintroduced it via this second, independent parse). Patches a copy of the raw
/// header bytes with the already-corrected clock so libvgm's own parser sees the same value
/// formats::vgm::parse already decided was right, rather than re-deriving the correction in
/// C++ too.
fn player_ready_bytes(vgm: &VgmFile) -> std::borrow::Cow<'_, [u8]> {
    if vgm.scc_clock == 0 || vgm.raw_data.len() < 0xA0 {
        return std::borrow::Cow::Borrowed(&vgm.raw_data);
    }
    let mut data = vgm.raw_data.clone();
    data[0x9C..0xA0].copy_from_slice(&vgm.scc_clock.to_le_bytes());
    std::borrow::Cow::Owned(data)
}

/// Core render loop shared by `render()` and `render_stems()` — `mute_setup` runs once, right
/// after the player loads the file, to configure per-channel muting for a stem (or leave
/// everything audible for the full mix).
fn render_internal(vgm: &VgmFile, mute_setup: impl FnOnce(&mut Player)) -> RenderedAudio {
    let mut player = Player::load(&player_ready_bytes(vgm), VGM_SAMPLE_RATE)
        .expect("formats::vgm::parse already validated this file; libvgm's own parser rejecting it would be a real, reportable bug");
    mute_setup(&mut player);

    let samples = player.render(vgm.total_samples as u32);
    let mut left = Vec::with_capacity(samples.len());
    let mut right = Vec::with_capacity(samples.len());
    for s in &samples {
        left.push(s.l as f32 / NATIVE_UNIT_SCALE);
        right.push(s.r as f32 / NATIVE_UNIT_SCALE);
    }
    // A file's own declared total_samples can occasionally run a little past what the player
    // actually produces (e.g. a truncated rip) — pad with silence rather than leaving callers
    // to handle a shorter-than-expected buffer.
    left.resize(vgm.total_samples as usize, 0.0);
    right.resize(vgm.total_samples as usize, 0.0);

    RenderedAudio { sample_rate: VGM_SAMPLE_RATE, left, right }
}

/// Renders the full, single play-through (0..total_samples) of the VGM — not the
/// indefinitely-repeating loop some players do; total_samples already includes the intro
/// plus one full loop, which reads as a complete, unsurprising rendition on its own.
pub fn render(vgm: &VgmFile) -> RenderedAudio {
    render_internal(vgm, |_| {})
}

const AY_CHANNEL_NAMES: [&str; 3] = ["AY-A", "AY-B", "AY-C"];
const SCC_CHANNEL_NAMES: [&str; 5] = ["SCC-1", "SCC-2", "SCC-3", "SCC-4", "SCC-5"];
const YM2612_CHANNEL_NAMES: [&str; 7] = ["FM-1", "FM-2", "FM-3", "FM-4", "FM-5", "FM-6", "DAC"];
// Shared by YM2608 (OPNA) and YM2610 (OPNB) — identical channel layout, see ALL_OPNA_CHANNELS's
// own comment.
const OPNA_CHANNEL_NAMES: [&str; 13] = [
    "FM-1", "FM-2", "FM-3", "FM-4", "FM-5", "FM-6", "ADPCM-A-1", "ADPCM-A-2", "ADPCM-A-3", "ADPCM-A-4", "ADPCM-A-5",
    "ADPCM-A-6", "ADPCM-B",
];
const GB_CHANNEL_NAMES: [&str; 4] = ["Square-1", "Square-2", "Wave", "Noise"];
const NES_CHANNEL_NAMES: [&str; 5] = ["Square-1", "Square-2", "Triangle", "Noise", "DPCM"];

fn is_silent(audio: &RenderedAudio) -> bool {
    audio.left.iter().chain(audio.right.iter()).all(|&x| x == 0.0)
}

/// One isolated voice's render, named for display/track-naming purposes (e.g. "AY-A",
/// "YM-3", "YM-BD", "OPL-3"). Only non-silent stems are returned — a channel the song never
/// actually uses (including every rhythm voice, unless the song enables rhythm mode) is
/// omitted rather than delivering a dozen empty tracks.
pub struct Stem {
    pub name: String,
    pub audio: RenderedAudio,
}

/// Renders one isolated stem per channel of every chip this project links: AY8910 tone
/// channels (A/B/C), K051649/SCC channels (1-5), and per FM voice of YM2413/YM3526/YM3812 (the
/// 9 FM channels each, plus their 5 rhythm-mode percussion voices which replace channels 7-9
/// when rhythm mode is enabled — rendering both unconditionally and dropping whichever half
/// turns out silent handles a song that switches between the two modes without needing to
/// detect that switch ourselves).
pub fn render_stems(vgm: &VgmFile) -> Vec<Stem> {
    let mut stems = Vec::new();

    for (i, name) in AY_CHANNEL_NAMES.iter().enumerate() {
        let audio = render_internal(vgm, |player| {
            mute_everything(player);
            player.set_mute(dev_id::AY8910, 0, ALL_AY_CHANNELS & !(1 << i));
        });
        if !is_silent(&audio) {
            stems.push(Stem { name: name.to_string(), audio });
        }
    }

    for (i, name) in SCC_CHANNEL_NAMES.iter().enumerate() {
        let audio = render_internal(vgm, |player| {
            mute_everything(player);
            player.set_mute(dev_id::K051649, 0, ALL_SCC_CHANNELS & !(1 << i));
        });
        if !is_silent(&audio) {
            stems.push(Stem { name: name.to_string(), audio });
        }
    }

    for (dev, prefix) in [(dev_id::YM2413, "YM"), (dev_id::YM3526, "OPL"), (dev_id::YM3812, "OPL2")] {
        for i in 0..9 {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev, 0, ALL_FM_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("{prefix}-{}", i + 1), audio });
            }
        }

        for (mask, voice) in [(MASK_BD, "BD"), (MASK_HH, "HH"), (MASK_SD, "SD"), (MASK_TOM, "TOM"), (MASK_CYM, "CYM")] {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev, 0, ALL_FM_CHANNELS & !mask);
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("{prefix}-{voice}"), audio });
            }
        }
    }

    // Chips added since the initial libvgm migration — each block below is gated by the
    // file's own declared clock (VgmFile's presence-via-actual-write check, see
    // formats::vgm::parse's own comment) rather than always attempting all ten, unlike the
    // five chips above: a real file only ever uses one or two of these, and skipping a whole
    // render pass per unused channel avoids needlessly multiplying render time by the ~85
    // extra channels these chips add up to between them.
    if vgm.ym2612_clock > 0 {
        for (i, name) in YM2612_CHANNEL_NAMES.iter().enumerate() {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::YM2612, 0, ALL_YM2612_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("YM2612-{name}"), audio });
            }
        }
    }

    if vgm.ym2151_clock > 0 {
        for i in 0..8 {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::YM2151, 0, ALL_YM2151_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("YM2151-{}", i + 1), audio });
            }
        }
    }

    if vgm.ym2203_clock > 0 {
        for i in 0..3 {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::YM2203, 0, ALL_YM2203_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("YM2203-{}", i + 1), audio });
            }
        }
    }

    for (present, dev, prefix) in [(vgm.ym2608_clock > 0, dev_id::YM2608, "YM2608"), (vgm.ym2610_clock > 0, dev_id::YM2610, "YM2610")] {
        if !present {
            continue;
        }
        for (i, name) in OPNA_CHANNEL_NAMES.iter().enumerate() {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev, 0, ALL_OPNA_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("{prefix}-{name}"), audio });
            }
        }
    }

    if vgm.segapcm_clock > 0 {
        for i in 0..16 {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::SEGAPCM, 0, ALL_SEGAPCM_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("SegaPCM-{}", i + 1), audio });
            }
        }
    }

    // RF5C68 and RF5C164 share one dev_id, told apart only by PLR_DEV_ID instance (0 vs 1) —
    // see ALL_RF5C_CHANNELS's own comment.
    for (present, instance, prefix) in [(vgm.rf5c68_clock > 0, 0u32, "RF5C68"), (vgm.rf5c164_clock > 0, 1u32, "RF5C164")] {
        if !present {
            continue;
        }
        for i in 0..8 {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::RF5C68, instance, ALL_RF5C_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("{prefix}-{}", i + 1), audio });
            }
        }
    }

    if vgm.gb_dmg_clock > 0 {
        for (i, name) in GB_CHANNEL_NAMES.iter().enumerate() {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::GB_DMG, 0, ALL_GB_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("GB-{name}"), audio });
            }
        }
    }

    if vgm.nes_apu_clock > 0 {
        for (i, name) in NES_CHANNEL_NAMES.iter().enumerate() {
            let audio = render_internal(vgm, |player| {
                mute_everything(player);
                player.set_mute(dev_id::NES_APU, 0, ALL_NES_CHANNELS & !(1 << i));
            });
            if !is_silent(&audio) {
                stems.push(Stem { name: format!("NES-{name}"), audio });
            }
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



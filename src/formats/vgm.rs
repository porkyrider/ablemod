//! Parser for VGM (Video Game Music) files and their gzip-compressed VGZ variant.
//!
//! Unlike ProTracker modules, a VGM file has no "samples" or "patterns" to speak of — it's a
//! timestamped log of raw register writes to one or more real sound chips, captured from an
//! actual game/console. There's no format-agnostic IR shared with the tracker side of this
//! project (formats::base::Module) since the whole notion of "a sample triggered by a note"
//! doesn't apply here: the chips synthesize their sound in real time from these register
//! writes, so producing audio means actually emulating the chip (see chips::), not extracting
//! stored waveform data.
//!
//! Only commands this parser has been verified against a real file (or that are simple,
//! uniform 3-byte "register, value" writes per the VGM spec, safe to skip even when we don't
//! act on them) are recognized — anything else is a hard parse error rather than a guessed
//! byte length, since a wrong guess would silently desync the rest of the command stream.

use std::collections::BTreeMap;
use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Chip {
    Ym2413,
    Ay8910,
    Scc,
    Ym3526,
    Ym3812,
}

/// VGM version numbers are BCD, not a plain integer — 0x151 means "1.51", not "1.337" (0x151
/// == 337 decimal). Getting this wrong is an easy, silent mistake since 0x151 *looks* like it
/// should just shift-and-mask into two plain decimal numbers.
pub fn version_string(version: u32) -> String {
    format!("{:X}.{:02X}", version >> 8, version & 0xFF)
}

/// Human-readable chip name for an *unsupported* command byte, for --verbose-style
/// reporting (mirrors formats::protracker::effect_name's role for unimplemented effects).
/// Chips this project does emulate (see VgmFile's own per-chip clock fields) are deliberately
/// absent here — their command bytes are recognized/decoded above, not routed to this fallback.
pub fn unsupported_chip_name(cmd: u8) -> &'static str {
    match cmd {
        0x5C => "Y8950 (MSX-AUDIO FM)",
        0x5D => "YMZ280B",
        0x5E | 0x5F => "YMF262 (OPL3 FM)",
        0xA1..=0xAF => "second AY8910/compatible PSG",
        0xB2 => "PWM",
        0xB5 => "MultiPCM",
        0xB6 => "uPD7759",
        0xB7 => "OKIM6258",
        0xB8 => "OKIM6295",
        0xB9 => "HuC6280",
        0xBA => "K053260",
        0xBB => "Pokey",
        0xBC => "WonderSwan",
        0xBD => "SAA1099",
        0xBE => "ES5506",
        0xBF => "GA20",
        0xC3 => "MultiPCM (bank select)",
        0xC4 => "QSound",
        0xC5 => "SCSP",
        0xC6 => "WonderSwan (memory write)",
        0xC7 => "VSU",
        0xC8 => "X1-010",
        0xD0 => "YMF278B",
        0xD1 => "YMF271",
        0xD3 => "K054539",
        0xD4 => "C140",
        0xD5 => "ES5503",
        0xD6 => "ES5506 (16-bit)",
        _ => "unknown chip",
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegisterWrite {
    pub at_sample: u64, // absolute time, in output samples at the VGM's own 44100Hz sample clock
    pub chip: Chip,
    /// Only meaningful for Chip::Scc (VGM's 0xD2 command splits the K051649's address space
    /// into 4 sub-blocks this way — see chips::scc::Scc::write) — 0 for every other chip.
    pub port: u8,
    pub reg: u8,
    pub value: u8,
}

pub struct VgmFile {
    pub version: u32,
    pub ym2413_clock: u32,
    pub ay8910_clock: u32,
    pub scc_clock: u32,
    pub ym3526_clock: u32,
    pub ym3812_clock: u32,
    pub ym2612_clock: u32,
    pub ym2151_clock: u32,
    pub ym2203_clock: u32,
    pub ym2608_clock: u32,
    pub ym2610_clock: u32,
    pub segapcm_clock: u32,
    pub rf5c68_clock: u32,
    pub rf5c164_clock: u32,
    pub gb_dmg_clock: u32,
    pub nes_apu_clock: u32,
    /// true if the header's AY8910 Chip Type byte marks this as a YM2149-compatible part
    /// (subtly different DAC curve) rather than a plain AY-3-8910 — see chips::ay8910::Ay8910::new.
    pub ay8910_is_ym: bool,
    pub total_samples: u64,
    pub loop_start_sample: Option<u64>,
    pub loop_samples: u64,
    pub title: Option<String>,
    pub game: Option<String>,
    pub system: Option<String>,
    pub author: Option<String>,
    pub writes: Vec<RegisterWrite>,
    /// Command bytes this parser recognized (so it could skip over them correctly) but
    /// doesn't act on — either an unemulated chip's register write, or a structural command
    /// (e.g. a PCM data block) with no audible effect on the two chips we do emulate. Keyed
    /// by the raw command byte, counted, for --verbose-style reporting.
    pub unsupported_commands: BTreeMap<u8, u32>,
    /// The file's raw (gunzipped) bytes — export::vgm_render hands these to libvgm's own
    /// player (chips::player_ffi) for audio rendering, a second, independent parse of the same
    /// file used only for that purpose. `writes` above remains this parser's own extraction,
    /// still used for export::vgm_wavetable/vgm_operator's note-extraction (currently not
    /// wired into export::vgm_als — see its own module comment).
    pub raw_data: Vec<u8>,
}

fn gunzip_if_needed(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|e| format!("failed to gunzip VGZ: {e}"))?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    data.get(off..off + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn read_gd3_string(data: &[u8], pos: &mut usize) -> String {
    let start = *pos;
    while *pos + 1 < data.len() && !(data[*pos] == 0 && data[*pos + 1] == 0) {
        *pos += 2;
    }
    let s = String::from_utf16_lossy(
        &data[start..*pos].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>(),
    );
    *pos += 2; // skip the terminating NUL
    s
}

fn parse_gd3(data: &[u8], gd3_offset: usize) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    if data.len() < gd3_offset + 12 || &data[gd3_offset..gd3_offset + 4] != b"Gd3 " {
        return (None, None, None, None);
    }
    let mut pos = gd3_offset + 12;
    let mut strings = Vec::new();
    for _ in 0..11 {
        if pos >= data.len() {
            break;
        }
        strings.push(read_gd3_string(data, &mut pos));
    }
    let get = |i: usize| strings.get(i).filter(|s| !s.is_empty()).cloned();
    // order: track EN, track JP, game EN, game JP, system EN, system JP, author EN, author JP, date, ripper, notes
    (get(0), get(2), get(4), get(6))
}

pub fn parse(bytes: &[u8]) -> Result<VgmFile, String> {
    let data = gunzip_if_needed(bytes)?;

    if data.len() < 0x40 || &data[0..4] != b"Vgm " {
        return Err("not a VGM/VGZ file (missing 'Vgm ' magic)".to_string());
    }
    let get_u32 = |off: usize| u32_at(&data, off).ok_or_else(|| format!("VGM header truncated at offset {off:#x}"));

    let version = get_u32(0x08)?;
    let ym2413_clock = get_u32(0x10)?;
    let gd3_offset_field = get_u32(0x14)?;
    let total_samples = get_u32(0x18)? as u64;
    let loop_offset_field = get_u32(0x1C)?;
    let loop_samples = get_u32(0x20)?;
    let ay8910_clock = if version >= 0x151 { get_u32(0x74).unwrap_or(0) } else { 0 };
    let ay8910_is_ym = version >= 0x151 && data.get(0x78).is_some_and(|&b| b != 0);
    // Unlike most VGM-supported chips, the K051649/SCC only ever appears on MSX hardware,
    // where it's always clocked directly by the system's own fixed 3.579545MHz Z80 clock —
    // not a per-file configurable parameter the way it legitimately is for chips used across
    // many different systems. Several real-world rips have been found storing the AY8910's
    // own halved clock (Z80/2, ~1789772Hz) in this field instead — apparently a ripper tool
    // defaulting both PSG-family chips to the same value — which silently renders every SCC
    // channel exactly one octave flat (confirmed directly against a real recording on "a
    // dream of dreamer"). The header field is still read to tell whether the chip is present
    // at all (0 = absent), but the clock value itself always uses the real, invariant
    // hardware constant rather than a field known to be unreliable specifically for this chip.
    const K051649_HARDWARE_CLOCK_HZ: u32 = 3_579_545;
    let scc_clock = if version >= 0x151 && get_u32(0x9C).unwrap_or(0) > 0 { K051649_HARDWARE_CLOCK_HZ } else { 0 };
    let ym3526_clock = if version >= 0x151 { get_u32(0x54).unwrap_or(0) } else { 0 };
    // YM3812 (OPL2) is register-compatible with YM3526 for every register export::vgm_operator
    // actually uses (0x20-0xC8) — OPL2 only adds a per-operator waveform-select register
    // (0xE0-0xF5) on top, which this project doesn't model for either chip, so both are
    // decoded through the exact same OPL extraction pipeline as two independent chip presences.
    let ym3812_clock = if version >= 0x151 { get_u32(0x50).unwrap_or(0) } else { 0 };
    // Chips added since the initial libvgm migration (see README.md) — WAV-only, no
    // Wavetable/Operator-style note extraction (export::vgm_wavetable/vgm_operator are SCC/OPL
    // specific and unaffected by any of these). Clock offsets/version gates from the VGM
    // format spec (vgmrips.net/wiki/VGM_Specification). Masked to bits 0-29: bit 30 (dual-chip)
    // and bit 31 (chip-variant, e.g. YM2612's bit marks YM3438, YM2610's marks YM2610B, NES
    // APU's enables its FDS expansion — none of which this project distinguishes) are dropped
    // rather than left to corrupt the plain Hz value list_cmd displays.
    const CLOCK_MASK: u32 = 0x3FFF_FFFF;
    let ym2612_clock = if version >= 0x110 { get_u32(0x2C).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let ym2151_clock = if version >= 0x110 { get_u32(0x30).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let segapcm_clock = if version >= 0x151 { get_u32(0x38).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let rf5c68_clock = if version >= 0x151 { get_u32(0x40).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let ym2203_clock = if version >= 0x151 { get_u32(0x44).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let ym2608_clock = if version >= 0x151 { get_u32(0x48).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let ym2610_clock = if version >= 0x151 { get_u32(0x4C).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let rf5c164_clock = if version >= 0x151 { get_u32(0x6C).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let gb_dmg_clock = if version >= 0x161 { get_u32(0x80).unwrap_or(0) & CLOCK_MASK } else { 0 };
    let nes_apu_clock = if version >= 0x161 { get_u32(0x84).unwrap_or(0) & CLOCK_MASK } else { 0 };

    let vgm_data_offset = if version >= 0x150 { get_u32(0x34).unwrap_or(0) } else { 0 };
    let data_start = if vgm_data_offset != 0 { 0x34 + vgm_data_offset as usize } else { 0x40 };

    let (title, game, system, author) = if gd3_offset_field != 0 {
        parse_gd3(&data, 0x14 + gd3_offset_field as usize)
    } else {
        (None, None, None, None)
    };

    let loop_start_byte = if loop_offset_field != 0 { Some(0x1C + loop_offset_field as usize) } else { None };

    let mut writes = Vec::new();
    let mut unsupported_commands: BTreeMap<u8, u32> = BTreeMap::new();
    let mut at_sample: u64 = 0;
    let mut loop_start_sample: Option<u64> = None;
    let mut pos = data_start;

    // Presence-via-actual-write tracking for the chips added since the initial libvgm
    // migration (see this function's own header-clock comment) — same rationale as the
    // ym2413_clock/ay8910_clock/... gating below: a header clock field alone isn't a reliable
    // presence signal (real rips have been found with stale nonzero values for chips the
    // command stream never once writes to), and unlike Ym2413/Ay8910/Scc/Ym3526/Ym3812 these
    // chips don't need a full RegisterWrite log entry (no Wavetable/Operator-style note
    // extraction targets them), so a plain "seen at least one write" flag is all this needs.
    let mut ym2612_seen = false;
    let mut ym2151_seen = false;
    let mut ym2203_seen = false;
    let mut ym2608_seen = false;
    let mut ym2610_seen = false;
    let mut segapcm_seen = false;
    let mut rf5c68_seen = false;
    let mut rf5c164_seen = false;
    let mut gb_dmg_seen = false;
    let mut nes_apu_seen = false;

    while pos < data.len() {
        if loop_start_sample.is_none() && loop_start_byte == Some(pos) {
            loop_start_sample = Some(at_sample);
        }
        let cmd = data[pos];
        match cmd {
            0x51 | 0xA0 | 0x5A | 0x5B => {
                let (reg, value) = (*data.get(pos + 1).ok_or("VGM stream truncated mid-command")?, *data
                    .get(pos + 2)
                    .ok_or("VGM stream truncated mid-command")?);
                writes.push(RegisterWrite {
                    at_sample,
                    chip: match cmd {
                        0x51 => Chip::Ym2413,
                        0x5A => Chip::Ym3812,
                        0x5B => Chip::Ym3526,
                        _ => Chip::Ay8910,
                    },
                    port: 0,
                    reg,
                    value,
                });
                pos += 3;
            }
            // K051649/SCC (Konami) — cmd, port, register, value. The "port" byte splits the
            // chip's address space into 4 pre-decoded sub-blocks (waveform/frequency/volume/
            // key-on-off) — see chips::scc::Scc::write for the exact mapping, confirmed
            // against a real file's own port/register value distribution rather than guessed.
            0xD2 => {
                let (port, reg, value) = (
                    *data.get(pos + 1).ok_or("VGM stream truncated mid-command")?,
                    *data.get(pos + 2).ok_or("VGM stream truncated mid-command")?,
                    *data.get(pos + 3).ok_or("VGM stream truncated mid-command")?,
                );
                writes.push(RegisterWrite { at_sample, chip: Chip::Scc, port, reg, value });
                pos += 4;
            }
            // Chips added since the initial libvgm migration — audio rendering for these goes
            // entirely through chips::player_ffi's own independent parse of raw_data (see
            // export::vgm_render's own module comment), so this parser only needs to notice
            // that a write happened (for the header-clock presence check below), not decode
            // it — no RegisterWrite entry, no Chip enum variant. Still a uniform 3-byte
            // encoding (cmd, register, value) like every other chip in this arm family.
            0x52 | 0x53 => {
                ym2612_seen = true;
                pos += 3;
            }
            0x54 => {
                ym2151_seen = true;
                pos += 3;
            }
            0x55 => {
                ym2203_seen = true;
                pos += 3;
            }
            0x56 | 0x57 => {
                ym2608_seen = true;
                pos += 3;
            }
            0x58 | 0x59 => {
                ym2610_seen = true;
                pos += 3;
            }
            0xB0 => {
                rf5c68_seen = true;
                pos += 3;
            }
            0xB1 => {
                rf5c164_seen = true;
                pos += 3;
            }
            0xB3 => {
                gb_dmg_seen = true;
                pos += 3;
            }
            0xB4 => {
                nes_apu_seen = true;
                pos += 3;
            }
            // Other chips' plain "register, value" writes (Y8950, YMZ280B, YMF262, a second
            // AY8910, PWM, MultiPCM, uPD7759, OKIM6258, OKIM6295, HuC6280, K053260, Pokey,
            // WonderSwan, SAA1099, ES5506, GA20, ...) — a uniform 3-byte encoding (cmd,
            // register, value) across the whole VGM spec, safe to skip even though we don't
            // emulate these chips. YM3812/YM3526 (0x5A/0x5B) are carved out above, not here.
            0x5C..=0x5F | 0xA1..=0xAF | 0xB2 | 0xB5..=0xBF => {
                *unsupported_commands.entry(cmd).or_insert(0) += 1;
                pos += 3;
            }
            // Sega PCM and RF5C68/164 are entirely driven by these wider memory-write
            // commands (16-bit address + value), not the plain register writes above —
            // matching the VGM spec's own encoding for this chip family (same 4-byte shape as
            // K051649/SCC's 0xD2, handled above).
            0xC0 => {
                segapcm_seen = true;
                pos += 4;
            }
            0xC1 => {
                rf5c68_seen = true;
                pos += 4;
            }
            0xC2 => {
                rf5c164_seen = true;
                pos += 4;
            }
            // Chips addressed by a wider offset/register space (MultiPCM bank select, QSound,
            // SCSP, WonderSwan, VSU, X1-010: cmd + 16-bit address + value; YMF278B, YMF271,
            // K054539, C140, ES5503, ES5506: cmd + port + register + value) — both a uniform
            // 4-byte encoding per the VGM spec, same family K051649/SCC (0xD2, handled above)
            // belongs to.
            0xC3..=0xC8 | 0xD0 | 0xD1 | 0xD3..=0xD6 => {
                *unsupported_commands.entry(cmd).or_insert(0) += 1;
                pos += 4;
            }
            0x61 => {
                let n = u16::from_le_bytes([
                    *data.get(pos + 1).ok_or("VGM stream truncated mid-command")?,
                    *data.get(pos + 2).ok_or("VGM stream truncated mid-command")?,
                ]);
                at_sample += n as u64;
                pos += 3;
            }
            0x62 => {
                at_sample += 735;
                pos += 1;
            }
            0x63 => {
                at_sample += 882;
                pos += 1;
            }
            0x66 => break, // end of sound data
            0x70..=0x7F => {
                at_sample += (cmd & 0x0F) as u64 + 1;
                pos += 1;
            }
            _ => {
                return Err(format!(
                    "unrecognized/unverified VGM command byte {cmd:#04x} at offset {pos:#06x} — refusing to guess \
                     its length rather than risk desyncing the rest of the file"
                ));
            }
        }
    }

    // The header's per-chip clock fields are meant to double as presence flags (0 = chip
    // absent), but real-world rips have been found with stale/garbage nonzero values in a
    // clock field for a chip the file's actual command stream never writes to at all (seen on
    // "bubble.vgz": a bogus K051649 clock of 1534215296 despite zero 0xD2 writes anywhere in
    // the file — the song is 100% YM3526, a chip this project doesn't emulate). Trusting the
    // header alone would misreport that chip as present/emulated and silently produce an empty
    // project. Requiring at least one actual write for the chip is a strictly more reliable
    // presence check than the header field alone, and can't false-negative a real file (a
    // chip that's genuinely present but never once written to produces no audible difference
    // either way).
    let ym2413_clock = if writes.iter().any(|w| w.chip == Chip::Ym2413) { ym2413_clock } else { 0 };
    let ay8910_clock = if writes.iter().any(|w| w.chip == Chip::Ay8910) { ay8910_clock } else { 0 };
    let scc_clock = if writes.iter().any(|w| w.chip == Chip::Scc) { scc_clock } else { 0 };
    let ym3526_clock = if writes.iter().any(|w| w.chip == Chip::Ym3526) { ym3526_clock } else { 0 };
    let ym3812_clock = if writes.iter().any(|w| w.chip == Chip::Ym3812) { ym3812_clock } else { 0 };
    let ym2612_clock = if ym2612_seen { ym2612_clock } else { 0 };
    let ym2151_clock = if ym2151_seen { ym2151_clock } else { 0 };
    let ym2203_clock = if ym2203_seen { ym2203_clock } else { 0 };
    let ym2608_clock = if ym2608_seen { ym2608_clock } else { 0 };
    let ym2610_clock = if ym2610_seen { ym2610_clock } else { 0 };
    let segapcm_clock = if segapcm_seen { segapcm_clock } else { 0 };
    let rf5c68_clock = if rf5c68_seen { rf5c68_clock } else { 0 };
    let rf5c164_clock = if rf5c164_seen { rf5c164_clock } else { 0 };
    let gb_dmg_clock = if gb_dmg_seen { gb_dmg_clock } else { 0 };
    let nes_apu_clock = if nes_apu_seen { nes_apu_clock } else { 0 };

    Ok(VgmFile {
        version,
        ym2413_clock,
        ay8910_clock,
        scc_clock,
        ym3526_clock,
        ym3812_clock,
        ym2612_clock,
        ym2151_clock,
        ym2203_clock,
        ym2608_clock,
        ym2610_clock,
        segapcm_clock,
        rf5c68_clock,
        rf5c164_clock,
        gb_dmg_clock,
        nes_apu_clock,
        ay8910_is_ym,
        total_samples,
        loop_start_sample,
        loop_samples: loop_samples as u64,
        title,
        game,
        system,
        author,
        writes,
        unsupported_commands,
        raw_data: data,
    })
}

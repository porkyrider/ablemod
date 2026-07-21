//! Parser for FastTracker 2 .xm files.
//!
//! Structurally mirrors protracker.rs (linear offset-threading, no cursor abstraction,
//! infallible/panicking on malformed input) but the on-disk layout differs substantially:
//! variable-length pattern/instrument headers (always read their own size field, never
//! hardcoded), compressed pattern cells, delta-encoded PCM, and — the one XM concept MOD has
//! no equivalent of — instruments that indirect through a 96-entry note→sample keymap onto
//! one of several raw samples. That indirection is resolved here, at parse time (see
//! `resolve_reachable_samples`/`build_cell`), not downstream: everything after this module
//! only ever sees the flat `Module`/`Sample`/`Cell` IR from base.rs, exactly like ProTracker.
//!
//! Byte offsets below were verified against a real-world 32-channel/17-instrument .xm file
//! (see tests/fixtures/20th Anniversary.xm) by chaining every instrument+sample header
//! end-to-end and confirming it lands exactly on EOF, and by decompressing a real pattern and
//! confirming the byte count consumed matches its declared packed size exactly — both are
//! strong signals the field layout here is correct, not just plausible.

use std::collections::BTreeMap;

use crate::formats::base::{Cell, Envelope, EnvelopePoint, Module, Pattern, Sample};

const BASE_SAMPLE_RATE_HZ: f64 = 8363.0;
const BASE_MIDI_NOTE: i32 = 60; // same physical pitch protracker.rs anchors to period 428 "C-2"
const NOTE_OFFSET: i32 = 11; // xm_note + 11 = midi_note (FT2's "C-4", note 49, is that same pitch)
const KEY_OFF_NOTE: u8 = 97;
const EFFECT_KEY_OFF: u32 = 0x14; // 'K' — XM's effect-letter-to-number scheme is direct: 0-9,A-Z = 0-35

/// Human-readable names for XM effect codes, used for --verbose CLI output. 0x0-0xF (plus Exx
/// sub-commands) mean exactly what they do in protracker.rs — reused verbatim, just duplicated
/// here rather than shared, matching this codebase's existing per-format-parser convention.
/// Codes 0x10 and up are XM's own extended letters (G onward); only Key Off (K) is actually
/// translated (into `Cell.note_off` at parse time — see `build_cell`), the rest are parsed
/// into the IR but never simulated (silently inert, like every unrecognized effect already is
/// throughout export::notes/formats::playback).
pub fn effect_name(code: u32) -> &'static str {
    if (0xE0..=0xEF).contains(&code) {
        return match code & 0x0F {
            0x0 => "Set Filter (Exx, unused on Amiga)",
            0x1 => "Fine Portamento Up (Exx)",
            0x2 => "Fine Portamento Down (Exx)",
            0x3 => "Glissando Control (Exx)",
            0x4 => "Set Vibrato Waveform (Exx)",
            0x5 => "Set Finetune (Exx)",
            0x6 => "Pattern Loop (Exx)",
            0x7 => "Set Tremolo Waveform (Exx)",
            0x8 => "unused (Exx)",
            0x9 => "Retrigger Note (Exx)",
            0xA => "Fine Volume Slide Up (Exx)",
            0xB => "Fine Volume Slide Down (Exx)",
            0xC => "Note Cut (Exx)",
            0xD => "Note Delay (Exx)",
            0xE => "Pattern Delay (Exx)",
            _ => "unused (Exx)",
        };
    }
    match code {
        0x0 => "Arpeggio",
        0x1 => "Portamento Up",
        0x2 => "Portamento Down",
        0x3 => "Tone Portamento",
        0x4 => "Vibrato",
        0x5 => "Tone Portamento + Volume Slide",
        0x6 => "Vibrato + Volume Slide",
        0x7 => "Tremolo",
        0x8 => "Set Panning",
        0x9 => "Sample Offset",
        0xA => "Volume Slide",
        0xB => "Position Jump",
        0xC => "Set Volume",
        0xD => "Pattern Break",
        0xE => "Extended Effects (Exx)",
        0xF => "Speed/Tempo",
        0x10 => "Set Global Volume (Gxx)",
        0x11 => "Global Volume Slide (Hxx)",
        0x14 => "Key Off (Kxx)",
        0x15 => "Set Envelope Position (Lxx)",
        0x19 => "Panning Slide (Pxx)",
        0x1B => "Multi Retrig Note (Rxy)",
        0x1D => "Tremor (Txy)",
        0x21 => "Extra Fine Portamento (Xxx)",
        _ => "?",
    }
}

pub const IMPLEMENTED_EFFECTS: &[u32] =
    &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0xD, 0xF, EFFECT_KEY_OFF];

// Same sub-commands export::notes::compute_song_events simulates for MOD's Exx — identical
// semantics in XM, reused as-is.
const IMPLEMENTED_E_SUBCOMMANDS: &[u32] = &[0x1, 0x2, 0x9, 0xA, 0xB, 0xC, 0xD];

fn is_implemented(code: u32) -> bool {
    IMPLEMENTED_EFFECTS.contains(&code)
}

fn extended_subcommand_counts(module: &Module) -> BTreeMap<u32, u32> {
    let mut counts = BTreeMap::new();
    for pattern in &module.patterns {
        for row in &pattern.rows {
            for cell in row {
                if cell.effect == Some(0xE) {
                    let sub_command = (cell.effect_param.unwrap_or(0) >> 4) & 0x0F;
                    *counts.entry(0xE0 | sub_command).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

pub fn unimplemented_effect_counts(module: &Module) -> BTreeMap<u32, u32> {
    let mut result: BTreeMap<u32, u32> = module
        .effect_counts()
        .into_iter()
        .filter(|(code, _)| *code != 0xE && !is_implemented(*code))
        .collect();
    for (code, count) in extended_subcommand_counts(module) {
        if !IMPLEMENTED_E_SUBCOMMANDS.contains(&(code & 0x0F)) {
            result.insert(code, count);
        }
    }
    result
}

pub fn implemented_effect_counts(module: &Module) -> BTreeMap<u32, u32> {
    let mut result: BTreeMap<u32, u32> = module
        .effect_counts()
        .into_iter()
        .filter(|(code, _)| *code != 0xE && is_implemented(*code))
        .collect();
    for (code, count) in extended_subcommand_counts(module) {
        if IMPLEMENTED_E_SUBCOMMANDS.contains(&(code & 0x0F)) {
            result.insert(code, count);
        }
    }
    result
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

fn trimmed_string(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
}

struct XmHeader {
    title: String,
    restart_position: u32,
    num_channels: usize,
    num_patterns: usize,
    num_instruments: usize,
    linear_frequency_table: bool,
    default_speed_ticks: u32, // XM's header confusingly calls this field "tempo"
    default_bpm: u32,         // ...and calls this one "BPM" — this one is the real tempo
    order: Vec<u32>,
}

fn parse_header(data: &[u8]) -> (XmHeader, usize) {
    let title = trimmed_string(&data[17..37]);
    let header_size = read_u32(data, 60) as usize;

    let song_length = read_u16(data, 64) as usize;
    let restart_position = read_u16(data, 66) as u32;
    let num_channels = read_u16(data, 68) as usize;
    let num_patterns = read_u16(data, 70) as usize;
    let num_instruments = read_u16(data, 72) as usize;
    // Byte order in the file is [flags(u16), "tempo" a.k.a. ticks/row(u16), "BPM" a.k.a. the
    // real tempo(u16)]. flags bit 0: linear (1) vs. Amiga (0) frequency table — propagated to
    // Module.linear_frequency_table, which export::notes uses to pick the right period<->pitch
    // math for Portamento/Tone Portamento/Vibrato/Arpeggio (see its own doc comment). The base
    // sample-pitch formula in build_sample, below, still always assumes linear regardless of
    // this flag (a separate, already-documented scope cut, see README.md).
    let flags = read_u16(data, 74);
    let linear_frequency_table = flags & 0x01 != 0;
    let default_speed_ticks = read_u16(data, 76) as u32;
    let default_bpm = read_u16(data, 78) as u32;

    let order_table: Vec<u32> = data[80..80 + 256].iter().map(|&b| b as u32).collect();
    let order = order_table[..song_length.min(256)].to_vec();

    let header = XmHeader {
        title: if title.is_empty() { "(untitled)".to_string() } else { title },
        restart_position,
        num_channels,
        num_patterns,
        num_instruments,
        linear_frequency_table,
        default_speed_ticks,
        default_bpm,
        order,
    };
    (header, 60 + header_size)
}

/// One decompressed pattern cell, still in raw XM terms (not yet resolved to the IR — `note`
/// is the raw 1-96/97 byte, `instrument` is the raw 1-based instrument number or 0 for "keep
/// the channel's current instrument", `volume`/`effect`/`effect_param` are the raw column
/// bytes). See `build_cell` for how these become a real `base::Cell`.
#[derive(Clone, Copy, Default)]
struct RawCell {
    note: u8,
    instrument: u8,
    volume: u8,
    effect: u8,
    effect_param: u8,
}

/// Unpacks one pattern's compressed cell stream. A cell byte with bit 7 set is a presence
/// bitmask (bit0=note, bit1=instrument, bit2=volume, bit3=effect type, bit4=effect param);
/// any field whose bit is clear is simply absent from the stream for that cell (0, not a
/// carry-over from the previous cell — XM's compression only omits zero bytes, it isn't a
/// MOD-style "repeat last value" scheme). A cell byte with bit 7 clear *is* the note value,
/// and the 4 other fields follow unconditionally.
fn decompress_pattern(data: &[u8], mut offset: usize, num_rows: usize, channels: usize) -> (Vec<Vec<RawCell>>, usize) {
    let mut rows = Vec::with_capacity(num_rows);
    for _ in 0..num_rows {
        let mut row = Vec::with_capacity(channels);
        for _ in 0..channels {
            let b0 = data[offset];
            let cell = if b0 & 0x80 != 0 {
                let mask = b0 & 0x1F;
                offset += 1;
                let mut next = |present: bool| -> u8 {
                    if present {
                        let v = data[offset];
                        offset += 1;
                        v
                    } else {
                        0
                    }
                };
                RawCell {
                    note: next(mask & 0x01 != 0),
                    instrument: next(mask & 0x02 != 0),
                    volume: next(mask & 0x04 != 0),
                    effect: next(mask & 0x08 != 0),
                    effect_param: next(mask & 0x10 != 0),
                }
            } else {
                let cell = RawCell {
                    note: b0,
                    instrument: data[offset + 1],
                    volume: data[offset + 2],
                    effect: data[offset + 3],
                    effect_param: data[offset + 4],
                };
                offset += 5;
                cell
            };
            row.push(cell);
        }
        rows.push(row);
    }
    (rows, offset)
}

/// Reads an envelope's point/sustain/loop fields, returning `None` if the envelope's "on" flag
/// (bit 0 of `type_flags`) isn't set — an instrument can carry fully-authored envelope point
/// data that's simply switched off, and that should behave exactly like no envelope at all.
fn parse_envelope(points_raw: &[u8], num_points: u8, sustain_idx: u8, loop_start_idx: u8, loop_end_idx: u8, type_flags: u8) -> Option<Envelope> {
    if type_flags & 0x01 == 0 {
        return None;
    }
    let n = (num_points as usize).min(12);
    if n == 0 {
        return None;
    }
    let mut points = Vec::with_capacity(n);
    for i in 0..n {
        let tick = read_u16(points_raw, i * 4) as u32;
        let value = read_u16(points_raw, i * 4 + 2) as u32;
        points.push(EnvelopePoint { tick, value });
    }
    let sustain_point = (type_flags & 0x02 != 0 && (sustain_idx as usize) < points.len()).then_some(sustain_idx as usize);
    let has_loop = type_flags & 0x04 != 0 && (loop_start_idx as usize) < points.len() && (loop_end_idx as usize) < points.len();
    let (loop_start_point, loop_end_point) = if has_loop { (Some(loop_start_idx as usize), Some(loop_end_idx as usize)) } else { (None, None) };
    Some(Envelope { points, sustain_point, loop_start_point, loop_end_point })
}

struct XmInstrument {
    num_samples: usize,
    keymap: [u8; 96], // note 0-95 -> in-instrument sample index
    volume_envelope: Option<Envelope>,
    panning_envelope: Option<Envelope>,
    fadeout: u32,
}

/// Parses one instrument header (the `inst_size`-byte block starting at `offset`; sample
/// headers/PCM follow immediately after and are handled separately by the caller, since they
/// need `num_samples`, already known before this function is called). When `num_samples == 0`
/// XM omits the entire extended block (keymap/envelopes/etc.) — `inst_size` is just the small
/// base header in that case, confirmed against the real fixture (33 bytes vs. 263 with samples).
fn parse_instrument(data: &[u8], offset: usize, inst_size: usize, num_samples: usize) -> XmInstrument {
    if num_samples == 0 {
        return XmInstrument { num_samples: 0, keymap: [0; 96], volume_envelope: None, panning_envelope: None, fadeout: 0 };
    }
    let ext_start = offset + 29;
    let ext_len = inst_size.saturating_sub(29);
    let ext = &data[ext_start..ext_start + ext_len];
    let at = |idx: usize| -> u8 { ext.get(idx).copied().unwrap_or(0) };

    let mut keymap = [0u8; 96];
    let keymap_len = ext_len.saturating_sub(4).min(96);
    if keymap_len > 0 {
        keymap[..keymap_len].copy_from_slice(&ext[4..4 + keymap_len]);
    }

    let vol_env_raw = if ext_len >= 148 { &ext[100..148] } else { &[][..] };
    let pan_env_raw = if ext_len >= 196 { &ext[148..196] } else { &[][..] };
    let num_vol_points = at(196);
    let num_pan_points = at(197);
    let vol_sustain = at(198);
    let vol_loop_start = at(199);
    let vol_loop_end = at(200);
    let pan_sustain = at(201);
    let pan_loop_start = at(202);
    let pan_loop_end = at(203);
    let vol_type = at(204);
    let pan_type = at(205);
    let fadeout = if ext_len >= 212 { read_u16(ext, 210) as u32 } else { 0 };

    let volume_envelope = if vol_env_raw.len() == 48 { parse_envelope(vol_env_raw, num_vol_points, vol_sustain, vol_loop_start, vol_loop_end, vol_type) } else { None };
    let panning_envelope = if pan_env_raw.len() == 48 { parse_envelope(pan_env_raw, num_pan_points, pan_sustain, pan_loop_start, pan_loop_end, pan_type) } else { None };

    XmInstrument { num_samples, keymap, volume_envelope, panning_envelope, fadeout }
}

struct XmSampleHeader {
    length_bytes: usize,
    loop_start_bytes: u32,
    loop_length_bytes: u32,
    volume: u32,
    finetune: i32, // raw signed byte, -128..127 (1/128th semitone units)
    loop_type: u8, // 0=none, 1=forward, 2=ping-pong (folded into forward, see build_sample)
    sixteen_bit: bool,
    pan_byte: u8,
    relative_note: i32,
    name: String,
}

fn parse_sample_header(data: &[u8], offset: usize) -> XmSampleHeader {
    let length_bytes = read_u32(data, offset) as usize;
    let loop_start_bytes = read_u32(data, offset + 4);
    let loop_length_bytes = read_u32(data, offset + 8);
    let volume = data[offset + 12] as u32;
    let finetune = data[offset + 13] as i8 as i32;
    let type_byte = data[offset + 14];
    let pan_byte = data[offset + 15];
    let relative_note = data[offset + 16] as i8 as i32;
    let name = trimmed_string(&data[offset + 18..offset + 40]);
    XmSampleHeader {
        length_bytes,
        loop_start_bytes,
        loop_length_bytes,
        volume: volume.min(64),
        finetune,
        loop_type: type_byte & 0x03,
        sixteen_bit: type_byte & 0x10 != 0,
        pan_byte,
        relative_note,
        name,
    }
}

/// Delta-decodes 8-bit XM sample data (each sample = previous + this byte's delta, wrapping).
fn delta_decode_8bit(raw: &[u8]) -> Vec<i8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut acc: i8 = 0;
    for &b in raw {
        acc = acc.wrapping_add(b as i8);
        out.push(acc);
    }
    out
}

/// Delta-decodes 16-bit XM sample data (little-endian delta words, wrapping).
fn delta_decode_16bit(raw: &[u8]) -> Vec<i16> {
    let mut out = Vec::with_capacity(raw.len() / 2);
    let mut acc: i16 = 0;
    for chunk in raw.chunks_exact(2) {
        acc = acc.wrapping_add(i16::from_le_bytes([chunk[0], chunk[1]]));
        out.push(acc);
    }
    out
}

fn decode_pcm(raw: &[u8], sixteen_bit: bool) -> Vec<u8> {
    if sixteen_bit {
        let samples = delta_decode_16bit(raw);
        let mut out = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    } else {
        let samples = delta_decode_8bit(raw);
        let mut out = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            // Same 8->16 bit upscale convention protracker.rs uses for its own raw (non-delta)
            // 8-bit samples, applied here after delta-decoding instead of on raw file bytes.
            let widened = (s as i16) * 256;
            out.extend_from_slice(&widened.to_le_bytes());
        }
        out
    }
}

fn build_sample(index: u32, header: &XmSampleHeader, raw_pcm: &[u8], instrument: &XmInstrument) -> Sample {
    let frame_divisor = if header.sixteen_bit { 2 } else { 1 };
    let pcm16 = decode_pcm(raw_pcm, header.sixteen_bit);
    let semitone_shift = header.relative_note as f64 + (header.finetune as f64 / 128.0);
    let sample_rate_hz = (BASE_SAMPLE_RATE_HZ * 2f64.powf(semitone_shift / 12.0)).round() as u32;
    let pan = ((header.pan_byte as f64 - 128.0) / 127.0).clamp(-1.0, 1.0);
    Sample {
        index,
        name: header.name.clone(),
        pcm16,
        sample_rate_hz,
        loop_start: header.loop_start_bytes / frame_divisor,
        // Unlike MOD's "loop length <= 1 word means no loop" heuristic, XM has an explicit
        // loop-type flag — ping-pong (type 2) is folded into a plain forward loop for now
        // (documented fidelity simplification, see README).
        loop_length: if header.loop_type == 0 { 0 } else { header.loop_length_bytes / frame_divisor },
        volume: header.volume,
        // The per-sample pitch offset (relative_note + finetune) is fully baked into
        // sample_rate_hz above, exactly like protracker.rs's own finetune handling — this
        // field stays 0 rather than storing XM's wider -128..127 raw byte, since base.rs
        // documents it as MOD's narrower -8..7 range and nothing downstream reads it anyway.
        finetune: 0,
        base_note: BASE_MIDI_NOTE,
        pan,
        volume_envelope: instrument.volume_envelope.clone(),
        panning_envelope: instrument.panning_envelope.clone(),
        fadeout: instrument.fadeout,
    }
}

/// Per (pattern, row, channel), which instrument is actually in effect — the one named
/// explicitly on that row, or (if none) whatever the channel's most recent explicit
/// instrument was, carried forward across rows *and pattern boundaries* in raw file order.
/// That last part is a deliberately conservative superset of true song-order reachability
/// (see `resolve_reachable_samples`'s own doc comment for why that's fine).
fn effective_instruments(raw_patterns: &[Vec<Vec<RawCell>>], num_channels: usize) -> Vec<Vec<Vec<Option<usize>>>> {
    let mut channel_current: Vec<Option<usize>> = vec![None; num_channels];
    raw_patterns
        .iter()
        .map(|pattern| {
            pattern
                .iter()
                .map(|row| {
                    row.iter()
                        .enumerate()
                        .map(|(ch, cell)| {
                            if cell.instrument > 0 {
                                channel_current[ch] = Some((cell.instrument - 1) as usize);
                            }
                            channel_current[ch]
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// Resolves one cell's (effective instrument, raw note) to a concrete in-instrument sample
/// slot, if the instrument/keymap actually reach one. Shared by both passes below so the
/// resolution rule can't drift between "what's reachable" and "what a cell actually plays".
fn resolve_sample_slot(note: u8, effective_instrument: Option<usize>, instruments: &[XmInstrument]) -> Option<(usize, usize)> {
    if !(1..=96).contains(&note) {
        return None;
    }
    let inst_idx = effective_instrument?;
    let instrument = instruments.get(inst_idx)?;
    if instrument.num_samples == 0 {
        return None;
    }
    let in_instrument_idx = instrument.keymap[(note - 1) as usize] as usize;
    (in_instrument_idx < instrument.num_samples).then_some((inst_idx, in_instrument_idx))
}

/// Pass 1 of the keymap-indirection resolution (see this module's own top doc comment): walks
/// every cell in every pattern (raw file order, not true playback order — see
/// `effective_instruments`'s doc comment for why that's a safe, conservative superset) to find
/// every distinct (instrument, in-instrument-sample) pair ever reachable, then allocates each
/// one a stable 1-based `Module.samples` index in deterministic (sorted) order.
fn resolve_reachable_samples(raw_patterns: &[Vec<Vec<RawCell>>], instruments: &[XmInstrument], num_channels: usize) -> BTreeMap<(usize, usize), u32> {
    let effective = effective_instruments(raw_patterns, num_channels);
    let mut reachable: std::collections::BTreeSet<(usize, usize)> = std::collections::BTreeSet::new();
    for (p, pattern) in raw_patterns.iter().enumerate() {
        for (r, row) in pattern.iter().enumerate() {
            for (ch, cell) in row.iter().enumerate() {
                if let Some(slot) = resolve_sample_slot(cell.note, effective[p][r][ch], instruments) {
                    reachable.insert(slot);
                }
            }
        }
    }
    reachable.into_iter().enumerate().map(|(i, slot)| (slot, (i + 1) as u32)).collect()
}

/// Resolves XM's independent volume-column byte against the effect column for one cell.
/// `Cell` has only one effect+param slot, but XM's volume and effect columns are fully
/// independent — priority rule: Set Volume (0x10-0x50) always goes to `Cell.volume` (its own
/// home, never conflicts), and additionally promotes into the effect slot as a Set Volume
/// (0xC) when there's no new note and the effect slot is otherwise free, mirroring exactly how
/// protracker.rs's own parser double-populates `Cell.volume`/effect 0xC for MOD's Cxx (see
/// this crate's protracker.rs). Any other volume-column command (slide/panning/vibrato/tone
/// porta) is promoted into the effect slot only if it's empty that row; if the effect column
/// already holds a real command, the real command wins and the volume-column command is
/// silently dropped (same "unimplemented effects are inert, never fatal" philosophy already
/// established throughout export::notes).
fn resolve_volume_column(vol: u8, has_note: bool, effect: u8, effect_param: u8) -> (Option<u32>, Option<u32>, Option<u32>) {
    let has_real_effect = effect != 0 || effect_param != 0;
    let real_effect = has_real_effect.then_some((effect as u32, effect_param as u32));

    if (0x10..=0x50).contains(&vol) {
        let value = (vol - 0x10) as u32;
        if !has_note && !has_real_effect {
            return (Some(value), Some(0xC), Some(value));
        }
        return (Some(value), real_effect.map(|(e, _)| e), real_effect.map(|(_, p)| p));
    }
    if vol == 0 || has_real_effect {
        return (None, real_effect.map(|(e, _)| e), real_effect.map(|(_, p)| p));
    }

    let nibble = (vol & 0x0F) as u32;
    let promoted = match vol {
        0x60..=0x6F => Some((0xA, nibble)),             // volume slide down
        0x70..=0x7F => Some((0xA, nibble << 4)),         // volume slide up
        0x80..=0x8F => Some((0xE, (0xB << 4) | nibble)), // fine volume slide down
        0x90..=0x9F => Some((0xE, (0xA << 4) | nibble)), // fine volume slide up
        0xB0..=0xBF => Some((0x4, nibble)),              // vibrato (depth only, reuses remembered speed)
        0xC0..=0xCF => Some((0x8, nibble * 17)),         // set panning, 4-bit -> 8-bit
        0xF0..=0xFF => Some((0x3, 0)),                   // tone portamento, reuses remembered speed
        _ => None,                                       // 0xA0-0xAF vibrato speed, 0xD0-0xEF pan slide: not simulated
    };
    match promoted {
        Some((e, p)) => (None, Some(e), Some(p)),
        None => (None, None, None),
    }
}

fn build_cell(raw: &RawCell, effective_instrument: Option<usize>, instruments: &[XmInstrument], sample_alloc: &BTreeMap<(usize, usize), u32>) -> Cell {
    let has_note = (1..=96).contains(&raw.note);
    let note_off = raw.note == KEY_OFF_NOTE || raw.effect as u32 == EFFECT_KEY_OFF;

    let sample_index = resolve_sample_slot(raw.note, effective_instrument, instruments).and_then(|slot| sample_alloc.get(&slot).copied());
    let midi_note = if has_note && sample_index.is_some() { Some(raw.note as i32 + NOTE_OFFSET) } else { None };

    let (volume, effect, effect_param) = resolve_volume_column(raw.volume, has_note, raw.effect, raw.effect_param);

    Cell { sample_index, midi_note, volume, effect, effect_param, note_off }
}

pub fn parse(data: &[u8]) -> Module {
    let (header, mut offset) = parse_header(data);

    let mut raw_patterns: Vec<Vec<Vec<RawCell>>> = Vec::with_capacity(header.num_patterns);
    for _ in 0..header.num_patterns {
        let pattern_header_length = read_u32(data, offset) as usize;
        let num_rows = read_u16(data, offset + 5) as usize;
        let packed_size = read_u16(data, offset + 7) as usize;
        let cell_start = offset + pattern_header_length;
        let (rows, _) = decompress_pattern(data, cell_start, num_rows, header.num_channels);
        raw_patterns.push(rows);
        offset = cell_start + packed_size;
    }

    let mut instruments: Vec<XmInstrument> = Vec::with_capacity(header.num_instruments);
    let mut sample_headers: Vec<Vec<XmSampleHeader>> = Vec::with_capacity(header.num_instruments);
    let mut sample_pcm: Vec<Vec<Vec<u8>>> = Vec::with_capacity(header.num_instruments);
    for _ in 0..header.num_instruments {
        let inst_size = read_u32(data, offset) as usize;
        let num_samples = read_u16(data, offset + 27) as usize;
        let instrument = parse_instrument(data, offset, inst_size, num_samples);

        let mut headers = Vec::with_capacity(num_samples);
        let mut header_offset = offset + inst_size;
        for _ in 0..num_samples {
            headers.push(parse_sample_header(data, header_offset));
            header_offset += 40;
        }

        let mut pcms = Vec::with_capacity(num_samples);
        let mut pcm_offset = header_offset;
        for h in &headers {
            pcms.push(data[pcm_offset..pcm_offset + h.length_bytes].to_vec());
            pcm_offset += h.length_bytes;
        }

        instruments.push(instrument);
        sample_headers.push(headers);
        sample_pcm.push(pcms);
        offset = pcm_offset;
    }

    let sample_alloc = resolve_reachable_samples(&raw_patterns, &instruments, header.num_channels);

    let mut samples: Vec<Sample> = Vec::with_capacity(sample_alloc.len());
    for (&(inst_idx, in_inst_idx), &index) in &sample_alloc {
        let sample = build_sample(index, &sample_headers[inst_idx][in_inst_idx], &sample_pcm[inst_idx][in_inst_idx], &instruments[inst_idx]);
        samples.push(sample);
    }
    samples.sort_by_key(|s| s.index);

    let effective = effective_instruments(&raw_patterns, header.num_channels);
    let patterns: Vec<Pattern> = raw_patterns
        .iter()
        .enumerate()
        .map(|(p, pattern)| Pattern {
            rows: pattern
                .iter()
                .enumerate()
                .map(|(r, row)| row.iter().enumerate().map(|(ch, cell)| build_cell(cell, effective[p][r][ch], &instruments, &sample_alloc)).collect())
                .collect(),
        })
        .collect();

    Module {
        title: header.title,
        source_format: "fasttracker2".to_string(),
        num_channels: header.num_channels,
        samples,
        patterns,
        order: header.order,
        restart_position: header.restart_position,
        initial_tempo_bpm: header.default_bpm,
        initial_speed_ticks: header.default_speed_ticks,
        linear_frequency_table: header.linear_frequency_table,
    }
}

//! Parser for ProTracker/NoiseTracker/SoundTracker .mod files.
//!
//! Supports both the modern 31-sample layout (identified by a 4-byte magic tag
//! at offset 1080, e.g. "M.K.", "6CHN", "8CHN") and the legacy 15-sample
//! Ultimate SoundTracker layout (no tag, always 4 channels, header ends at
//! offset 600 instead of 1084).

use std::collections::BTreeMap;

use crate::formats::base::{Cell, Module, Pattern, Sample};

const TITLE_LEN: usize = 20;
const SAMPLE_HEADER_LEN: usize = 30;
const ROWS_PER_PATTERN: usize = 64;
const BASE_SAMPLE_RATE_HZ: f64 = 8363.0; // standard reference rate for finetune 0, period 428 ("C-2")
const BASE_MIDI_NOTE: i32 = 60; // MIDI note assigned to period 428 ("C-2"), i.e. Ableton's middle C

/// Human-readable names for every standard ProTracker effect code, used for --verbose
/// CLI output. Codes not in IMPLEMENTED_EFFECTS are parsed into the IR but silently
/// ignored by playback simulation (see export::notes / formats::playback). Extended
/// Effects (Exx) sub-commands use a synthetic code in 0xE0..=0xEF (0xE0 | sub-command
/// nibble) — see extended_subcommand_counts — since 0xE itself is a mix of implemented
/// and unimplemented sub-commands, not a single yes/no like every other top-level code.
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
        _ => "?",
    }
}

pub const IMPLEMENTED_EFFECTS: &[u32] = &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0xD, 0xF];

// Which Exx sub-command nibbles export::notes::compute_song_events actually simulates —
// see the EXTENDED branch there. E0/E3/E4/E5/E6/E7/E8/EE/EF are parsed but ignored.
const IMPLEMENTED_E_SUBCOMMANDS: &[u32] = &[0x1, 0x2, 0x9, 0xA, 0xB, 0xC, 0xD];

fn is_implemented(code: u32) -> bool {
    IMPLEMENTED_EFFECTS.contains(&code)
}

/// Exx occurrences broken down by sub-command (the high nibble of the effect param), keyed
/// by the same synthetic 0xE0..=0xEF code effect_name/print_effect_table expect — `0xE`
/// itself never appears as a key here or in unimplemented_effect_counts/
/// implemented_effect_counts, since "is Exx implemented" isn't a single yes/no.
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

/// Effect codes present in the module that ablemod parses but doesn't interpret
/// during playback simulation (i.e. silently ignored by extract-midi/convert).
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

/// Effect codes present in the module that ablemod does simulate during playback
/// (the counterpart to unimplemented_effect_counts, for --verbose CLI reporting).
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

fn detect_channels(tag: &[u8]) -> Option<usize> {
    match tag {
        b"M.K." | b"M!K!" | b"M&K!" | b"N.T." | b"FLT4" => return Some(4),
        b"6CHN" => return Some(6),
        b"FLT8" | b"8CHN" | b"CD81" | b"OKTA" | b"OCTA" => return Some(8),
        b"16CN" => return Some(16),
        b"32CN" => return Some(32),
        _ => {}
    }
    if tag.len() == 4 {
        if &tag[2..4] == b"CH" && tag[0].is_ascii_digit() && tag[1].is_ascii_digit() {
            let s = std::str::from_utf8(&tag[0..2]).ok()?;
            return s.parse::<usize>().ok();
        }
        if &tag[1..4] == b"CHN" && tag[0].is_ascii_digit() {
            let s = std::str::from_utf8(&tag[0..1]).ok()?;
            return s.parse::<usize>().ok();
        }
    }
    None
}

fn build_period_table() -> Vec<(i32, i32)> {
    // Standard PT period table, finetune 0, octaves 1-3 (36 entries).
    let octave1 = [856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453];
    let mut periods = Vec::with_capacity(36);
    for octave in 0..3u32 {
        for p in octave1 {
            periods.push(p >> octave);
        }
    }
    let start_note = BASE_MIDI_NOTE - 12;
    periods
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, start_note + i as i32))
        .collect()
}

fn period_to_note(period: i32, table: &[(i32, i32)]) -> i32 {
    table
        .iter()
        .min_by_key(|(p, _)| (p - period).abs())
        .expect("period table is never empty")
        .1
}

struct SampleHeader {
    name: String,
    length_bytes: usize,
    finetune: i32,
    volume: u32,
    loop_start_bytes: u32,
    loop_length_bytes: u32,
}

fn parse_sample_headers(data: &[u8], count: usize, mut offset: usize) -> (Vec<SampleHeader>, usize) {
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        let raw_name = &data[offset..offset + 22];
        let end = raw_name.iter().position(|&b| b == 0).unwrap_or(raw_name.len());
        let name = String::from_utf8_lossy(&raw_name[..end]).into_owned();

        let length_w = u16::from_be_bytes([data[offset + 22], data[offset + 23]]);
        let finetune_raw = data[offset + 24];
        let volume = data[offset + 25] as u32;
        let loop_off_w = u16::from_be_bytes([data[offset + 26], data[offset + 27]]);
        let loop_len_w = u16::from_be_bytes([data[offset + 28], data[offset + 29]]);

        let mut finetune = (finetune_raw & 0x0F) as i32;
        if finetune >= 8 {
            finetune -= 16;
        }

        headers.push(SampleHeader {
            name,
            length_bytes: length_w as usize * 2,
            finetune,
            volume: volume.min(64),
            loop_start_bytes: loop_off_w as u32 * 2,
            loop_length_bytes: loop_len_w as u32 * 2,
        });
        offset += SAMPLE_HEADER_LEN;
    }
    (headers, offset)
}

fn parse_patterns(
    data: &[u8], mut offset: usize, num_patterns: usize, channels: usize, table: &[(i32, i32)],
) -> (Vec<Pattern>, usize) {
    let mut patterns = Vec::with_capacity(num_patterns);
    for _ in 0..num_patterns {
        let mut rows = Vec::with_capacity(ROWS_PER_PATTERN);
        for _ in 0..ROWS_PER_PATTERN {
            let mut row = Vec::with_capacity(channels);
            for _ in 0..channels {
                let (b0, b1, b2, b3) = (data[offset], data[offset + 1], data[offset + 2], data[offset + 3]);
                offset += 4;
                let sample_num = (b0 & 0xF0) | (b2 >> 4);
                let period = (((b0 & 0x0F) as i32) << 8) | b1 as i32;
                let effect_num = (b2 & 0x0F) as u32;
                let effect_param = b3 as u32;
                let has_effect = !(effect_num == 0 && effect_param == 0);
                row.push(Cell {
                    sample_index: if sample_num != 0 { Some(sample_num as u32) } else { None },
                    midi_note: if period > 0 { Some(period_to_note(period, table)) } else { None },
                    volume: if effect_num == 0xC { Some(effect_param) } else { None },
                    effect: if has_effect { Some(effect_num) } else { None },
                    effect_param: if has_effect { Some(effect_param) } else { None },
                    note_off: false, // MOD has no note-off concept
                });
            }
            rows.push(row);
        }
        patterns.push(Pattern { rows });
    }
    (patterns, offset)
}

fn convert_8bit_to_16bit(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() * 2);
    for &b in raw {
        let signed = b as i8 as i32;
        let sample16 = (signed * 256) as i16;
        out.extend_from_slice(&sample16.to_le_bytes());
    }
    out
}

pub fn parse(data: &[u8]) -> Module {
    let title_end = data[0..TITLE_LEN].iter().position(|&b| b == 0).unwrap_or(TITLE_LEN);
    let title = String::from_utf8_lossy(&data[0..title_end]).into_owned();

    let tag = &data[1080..1084];
    let detected_channels = detect_channels(tag);
    let num_samples = if detected_channels.is_some() { 31 } else { 15 };
    let channels = detected_channels.unwrap_or(4);

    let (headers, offset) = parse_sample_headers(data, num_samples, TITLE_LEN);
    let song_length = data[offset] as usize;
    let restart_position = data[offset + 1] as u32;
    let order_table: Vec<u32> = data[offset + 2..offset + 2 + 128].iter().map(|&b| b as u32).collect();
    let order: Vec<u32> = order_table[..song_length].to_vec();

    let pattern_data_start = if num_samples == 31 { 1084 } else { offset + 2 + 128 };
    let num_patterns = (order_table.iter().max().copied().unwrap_or(0) + 1) as usize;
    let table = build_period_table();
    let (patterns, sample_data_start) = parse_patterns(data, pattern_data_start, num_patterns, channels, &table);

    let mut samples = Vec::with_capacity(headers.len());
    let mut pos = sample_data_start;
    for (i, h) in headers.iter().enumerate() {
        let raw = &data[pos..pos + h.length_bytes];
        pos += h.length_bytes;
        let sample_rate = (BASE_SAMPLE_RATE_HZ * 2f64.powf(h.finetune as f64 / 96.0)).round() as u32;
        samples.push(Sample {
            index: (i + 1) as u32,
            name: h.name.clone(),
            pcm16: convert_8bit_to_16bit(raw),
            sample_rate_hz: sample_rate,
            loop_start: h.loop_start_bytes,
            // a loop length of <= 1 word (2 bytes) is the standard MOD convention for "no loop"
            loop_length: if h.loop_length_bytes > 2 { h.loop_length_bytes } else { 0 },
            volume: h.volume,
            finetune: h.finetune,
            base_note: BASE_MIDI_NOTE,
            pan: 0.0, // MOD has no per-sample panning
            volume_envelope: None,
            panning_envelope: None,
            fadeout: 0,
        });
    }

    Module {
        title: if title.is_empty() { "(untitled)".to_string() } else { title },
        source_format: "protracker".to_string(),
        num_channels: channels,
        samples,
        patterns,
        order,
        restart_position,
        initial_tempo_bpm: 125,
        initial_speed_ticks: 6,
        linear_frequency_table: false, // MOD has no such concept; always the classic Amiga period formula
    }
}

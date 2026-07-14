//! Format-independent intermediate representation (IR) for tracker modules.
//!
//! Every format parser (ProTracker, FastTracker 2, ScreamTracker 3, ...) produces
//! a `Module`. Everything downstream (listing, WAV/MIDI/.als export) only ever
//! looks at this IR, never at format-specific structures.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Sample {
    pub index: u32, // 1-based, matches tracker sample numbering
    pub name: String,
    pub pcm16: Vec<u8>, // mono, 16-bit signed little-endian PCM
    pub sample_rate_hz: u32,
    pub loop_start: u32,  // in frames
    pub loop_length: u32, // in frames, 0 = no loop
    pub volume: u32,      // 0-64 (tracker convention)
    pub finetune: i32,    // signed, -8..7
    pub base_note: i32,   // MIDI note number this sample is pitched to play at unmodified
}

impl Sample {
    pub fn has_loop(&self) -> bool {
        self.loop_length > 0
    }

    pub fn is_empty(&self) -> bool {
        self.pcm16.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Cell {
    pub sample_index: Option<u32>, // None = no sample triggered this row
    pub midi_note: Option<i32>,    // None = no new note this row
    pub volume: Option<u32>,       // 0-64, None = use sample default
    pub effect: Option<u32>,
    pub effect_param: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Pattern {
    pub rows: Vec<Vec<Cell>>, // rows[row_index][channel]
}

impl Pattern {
    pub fn num_rows(&self) -> usize {
        self.rows.len()
    }
}

#[derive(Debug, Clone)]
pub struct Module {
    pub title: String,
    pub source_format: String,
    pub num_channels: usize,
    pub samples: Vec<Sample>,
    pub patterns: Vec<Pattern>,
    pub order: Vec<u32>, // sequence of pattern indices, i.e. the "song"
    pub restart_position: u32,
    pub initial_tempo_bpm: u32,
    pub initial_speed_ticks: u32,
}

impl Module {
    pub fn effect_counts(&self) -> BTreeMap<u32, u32> {
        let mut counts = BTreeMap::new();
        for pattern in &self.patterns {
            for row in &pattern.rows {
                for cell in row {
                    if let Some(effect) = cell.effect {
                        if (effect, cell.effect_param.unwrap_or(0)) != (0, 0) {
                            *counts.entry(effect).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
        counts
    }

    pub fn effects_used(&self) -> Vec<u32> {
        self.effect_counts().keys().copied().collect()
    }
}

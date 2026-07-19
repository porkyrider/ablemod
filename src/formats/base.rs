//! Format-independent intermediate representation (IR) for tracker modules.
//!
//! Every format parser (ProTracker, FastTracker 2, ScreamTracker 3, ...) produces
//! a `Module`. Everything downstream (listing, WAV/MIDI/.als export) only ever
//! looks at this IR, never at format-specific structures.

use std::collections::BTreeMap;

/// One (tick, value) breakpoint of an instrument envelope. `tick` is envelope-internal
/// (counted from note trigger, not a song row/tick), `value` is 0-64 for both volume and
/// panning envelopes (32 = center for panning) — matches FastTracker 2's own convention so
/// parsers can copy point data through unmodified.
#[derive(Debug, Clone, Default)]
pub struct EnvelopePoint {
    pub tick: u32,
    pub value: u32,
}

/// A volume or panning envelope, as authored on a tracker instrument. Cloned into every
/// `Sample` that shares it (an instrument with several samples duplicates its one envelope
/// across all of them) rather than referenced — envelopes are small (a handful of points) and
/// this keeps `Sample` self-contained like every other field on it.
#[derive(Debug, Clone, Default)]
pub struct Envelope {
    pub points: Vec<EnvelopePoint>, // tick-ordered, first point's tick is always 0
    pub sustain_point: Option<usize>, // index into `points`; freezes the value here while a note is held
    pub loop_start_point: Option<usize>, // not currently simulated by any exporter, carried for a later pass
    pub loop_end_point: Option<usize>,
}

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
    pub pan: f64,         // -1..1, 0 = center
    pub volume_envelope: Option<Envelope>,
    pub panning_envelope: Option<Envelope>,
    pub fadeout: u32, // per-instrument volume fadeout rate; 0 = none/unused
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
    pub note_off: bool, // explicit note release (XM Key Off); mutually exclusive with midi_note
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

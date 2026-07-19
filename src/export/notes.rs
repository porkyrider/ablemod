//! Simulates tracker playback into a format-agnostic list of note events.
//!
//! Shared by the MIDI exporter and the .als exporter so both target formats stay
//! in sync: playback is simulated once here (honoring pattern breaks/position
//! jumps via `iter_song_rows`, and instrument-carry-forward on cells with no
//! explicit sample number), and each exporter just converts the resulting beat
//! times into its own time base (MIDI ticks, or Ableton's native beat-float).
//!
//! Timing: a MOD row is always mapped to a fixed 1/16-note grid (`BEATS_PER_ROW`
//! == a quarter beat), regardless of the tracker's Speed value — Speed instead
//! scales the *tempo*: `daw_bpm = tracker_bpm * 6 / speed` (6 being the tracker
//! default, at which a row is conventionally a 16th note). This keeps the note
//! grid readable in a DAW's editor at any Speed, at the cost of the displayed
//! BPM no longer being the module's raw Tempo number whenever Speed isn't 6 —
//! matching common mod2midi tooling convention. Every distinct Speed/Tempo value
//! becomes its own tempo-automation point (see export::als for how those are
//! written as clean steps rather than ramps), even ones that only hold for a row
//! or two (e.g. a tracker shuffle/swing groove made of rapidly-alternating
//! Speed) — real-time-accurate, at the cost of a busy-looking tempo track for
//! modules that lean on that technique.

use std::collections::BTreeMap;

use crate::formats::base::{Cell, Envelope, Module, Sample};
use crate::formats::playback::iter_song_rows;

pub const BEATS_PER_ROW: f64 = 6.0 / 24.0; // a row is always a 16th note (4 rows/beat), independent of Speed
pub const NOTE_GAP_BEATS: f64 = 0.001; // inaudible but unambiguous silence between consecutive notes,
                                       // so the previous one is fully stopped before the next triggers
                                       // instead of exactly touching (which float rounding can turn
                                       // into a hairline overlap that some hosts merge/drop on import)

const ARPEGGIO: u32 = 0x0;
const PORTAMENTO_UP: u32 = 0x1;
const PORTAMENTO_DOWN: u32 = 0x2;
const TONE_PORTAMENTO: u32 = 0x3;
const VIBRATO: u32 = 0x4;
const TONE_PORTAMENTO_VOLSLIDE: u32 = 0x5;
const VIBRATO_VOLSLIDE: u32 = 0x6;
const TREMOLO: u32 = 0x7;
const SET_PANNING: u32 = 0x8;
const SAMPLE_OFFSET: u32 = 0x9;
const VOLUME_SLIDE: u32 = 0xA;
const SET_VOLUME: u32 = 0xC;
const EXTENDED: u32 = 0xE;

// Extended effect (Exx) sub-commands, i.e. the high nibble of the Exx cell's param byte.
const E_FINE_PORTAMENTO_UP: u32 = 0x1;
const E_FINE_PORTAMENTO_DOWN: u32 = 0x2;
const E_RETRIGGER: u32 = 0x9;
const E_FINE_VOLSLIDE_UP: u32 = 0xA;
const E_FINE_VOLSLIDE_DOWN: u32 = 0xB;
const E_NOTE_CUT: u32 = 0xC;
const E_NOTE_DELAY: u32 = 0xD;

// Amiga-period convention shared with formats::protracker: period 428 == MIDI note 60,
// equal-tempered. A Portamento Up/Down effect bends the *currently playing* note's pitch
// tick by tick without retriggering it, so it's recorded as a continuous pitch bend curve
// on that note (see NoteEvent::bends) rather than as new discrete notes.
const REFERENCE_PERIOD: f64 = 428.0;
const REFERENCE_NOTE: f64 = 60.0;

// ft2-clone's arpeggioTab[32] (src/ft2_tables.c): the intended pattern is just "i % 3", but
// FT2's actual binary only has 16 correct bytes — the rest (indices 16-31) are unrelated
// bytes that happened to follow the table in memory, and FT2 (bug and all) reads them
// anyway for very high Speed values. ft2-clone deliberately keeps this quirk for
// bit-exactness, so we replicate the exact same table rather than a clean "% 3".
const ARPEGGIO_TAB: [u32; 32] = [
    0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 0x00, 0x18, 0x31, 0x4A, 0x61, 0x78, 0x8D, 0xA1, 0xB4, 0xC5, 0xD4,
    0xE0, 0xEB, 0xF4, 0xFA, 0xFD,
];

fn period_to_fractional_note(period: f64) -> f64 {
    REFERENCE_NOTE - 12.0 * (period / REFERENCE_PERIOD).log2()
}

fn note_to_period(note: i32) -> f64 {
    REFERENCE_PERIOD * 2f64.powf((REFERENCE_NOTE - note as f64) / 12.0)
}

fn daw_bpm(tracker_bpm: u32, speed: u32) -> f64 {
    tracker_bpm as f64 * 6.0 / speed as f64
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchBend {
    pub at_beat: f64,
    pub semitones: f64, // offset from the note's fixed `pitch`, e.g. from a Portamento effect
    pub glide: bool, // True: continues smoothly from the *previous* PitchBend (one tick
                      // of an uninterrupted Portamento/Vibrato/Tone Portamento run) —
                      // export::als only steps (hold-then-jump) points where this is
                      // False, e.g. Arpeggio (always discrete jumps) or a run's first
                      // tick after a gap, so an ongoing glide reads as one smooth curve
                      // instead of a staircase, while Arpeggio keeps its buzzy character.
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeChange {
    pub at_beat: f64,
    pub tracker_volume: i32, // 0-64, absolute (same units as Sample.volume / Cell.volume)
    pub glide: bool, // True: continues smoothly from the *previous* VolumeChange (one
                      // tick of an uninterrupted Volume Slide) — export::als only steps
                      // (hold-then-jump) points where this is False, e.g. Set Volume or
                      // a slide's first tick after a gap, so an ongoing per-tick slide
                      // reads as one smooth fade instead of a staircase of little jumps.
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanChange {
    pub at_beat: f64,
    pub pan: f64,     // -1..1, absolute (0 = center), from a Set Panning (8xx) effect or an envelope point
    pub glide: bool,  // True: continues smoothly from the previous PanChange (an XM panning
                       // envelope's own points) — see VolumeChange's own `glide` doc comment,
                       // same convention. Set Panning (8xx) is always an instant jump (False).
}

#[derive(Debug, Clone)]
pub struct NoteEvent {
    pub start_beat: f64,
    pub duration_beat: f64,
    pub velocity: i32, // 0-127
    pub pitch: i32,    // MIDI note number
    pub trigger_volume: i32, // 0-64 tracker volume at note-on, for Volume Slide/Set Volume math
    pub channel: usize, // tracker channel index the note was triggered on, for optional hardwired Amiga/Atari panning (export::als::AmigaPanning)
    pub voice: usize, // which non-overlapping "voice" this note belongs to when several tracker
                       // channels share this sample with overlapping timing, *or* when it needs
                       // a different Sample Offset (9xx) than another note sharing this sample —
                       // see the voice-assignment pass at the end of compute_song_events. Each
                       // distinct voice becomes its own track in export::als/export::midi.
    pub sample_offset_frames: u32, // Sample Offset (9xx): where playback starts within the
                                    // sample instead of frame 0 — see export::als::build_track's
                                    // SampleStart. 0 = plays from the start, as normal.
    pub max_duration_beat: Option<f64>, // non-looped samples: never sustain past their natural length
    pub bends: Vec<PitchBend>,
    pub volumes: Vec<VolumeChange>,
    pub pans: Vec<PanChange>,
    // Instrument envelope points, kept *separate* from `volumes`/`pans` above rather than
    // mixed into them. Real tracker hardware/software applies an instrument envelope as an
    // independent layer multiplied (volume) or blended (panning) with the channel's own
    // Volume Slide/Set Volume/Set Panning — not as a competing set of points overriding
    // whatever the channel effects were doing. Mixing the two into one point list produced
    // exactly that bug: an enabled envelope's own attack point at a note's trigger beat would
    // silently clobber a same-beat Set Panning (including one *promoted* from XM's volume
    // column, e.g. "PF"), and a Volume Slide's rising per-tick curve would read as broken/
    // reversed whenever it competed with an envelope's own points nearby. See
    // export::als::merge_with_envelope for where these two independent layers are actually
    // combined, once, downstream.
    pub envelope_volumes: Vec<VolumeChange>,
    pub envelope_pans: Vec<PanChange>,
    pub release_beat: Option<f64>, // set once, the first time an XM Key Off is seen on this
                                    // note's channel — does *not* end the note (see the
                                    // `cell.note_off` branch in compute_song_events); only
                                    // marks where an instrument envelope's release phase, if
                                    // any, should begin. None for a note that's still playing
                                    // when the song ends, or that never receives a Key Off.
}

impl NoteEvent {
    fn new(start_beat: f64, pitch: i32, velocity: i32, trigger_volume: i32, channel: usize) -> Self {
        NoteEvent {
            start_beat,
            duration_beat: 0.0,
            velocity,
            pitch,
            trigger_volume,
            channel,
            voice: 0, // assigned for real once the whole song is known — see compute_song_events
            sample_offset_frames: 0,
            max_duration_beat: None,
            bends: Vec::new(),
            volumes: Vec::new(),
            pans: Vec::new(),
            envelope_volumes: Vec::new(),
            envelope_pans: Vec::new(),
            release_beat: None,
        }
    }

    fn close(&mut self, at_beat: f64) {
        let mut duration = at_beat - self.start_beat;
        if let Some(max_duration) = self.max_duration_beat {
            duration = duration.min(max_duration);
        }
        self.duration_beat = duration;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TempoChange {
    pub at_beat: f64,
    pub bpm: f64,
}

/// One pattern *play* in actual playback order — i.e. one contiguous run of rows at a
/// single `module.order` position, as walked by `iter_song_rows`. A Position Jump/Pattern
/// Break ends a segment early (possibly mid-pattern); revisiting the same pattern later in
/// the song order produces a separate segment. Consecutive segments are contiguous
/// (`segments[i].end_beat == segments[i + 1].start_beat`), covering the whole song. Used by
/// export::als to lay out one Arrangement clip per pattern play instead of a single
/// song-spanning clip.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub start_beat: f64,
    pub end_beat: f64,
    pub order_pos: usize,
    pub pattern_index: usize,
}

pub struct SongEvents {
    pub notes_by_sample: BTreeMap<u32, Vec<NoteEvent>>,
    pub tempo_changes: Vec<TempoChange>,
    pub total_beats: f64,
    pub segments: Vec<Segment>,
}

fn velocity(cell_volume: Option<u32>, sample_volume: u32) -> i32 {
    let vol = cell_volume.unwrap_or(sample_volume) as f64;
    ((vol / 64.0 * 127.0).round() as i32).clamp(1, 127)
}

fn pan_from_param(param: u32) -> f64 {
    ((param as f64 - 128.0) / 127.0).clamp(-1.0, 1.0)
}

fn envelope_pan_to_normalized(value: u32) -> f64 {
    ((value as f64 - 32.0) / 32.0).clamp(-1.0, 1.0)
}

/// The "attack" (pre-release) portion of an envelope, emitted once at note-trigger time using
/// the tick rate in effect *then* — a deliberate simplification (a Speed/BPM change partway
/// through a long, not-yet-sustained envelope ramp isn't reflected), documented in this
/// module's own top doc comment. If the envelope has a sustain point, only points up to and
/// including it are emitted: Ableton holds flat at that value afterward with no further
/// automation needed, exactly matching a tracker's own "envelope freezes at the sustain point
/// while the note is held" behavior. Without a sustain point the whole envelope is emitted
/// unconditionally (it isn't gated on note-off at all).
fn envelope_attack_points(env: &Envelope, start_beat: f64, beats_per_tick: f64) -> Vec<(f64, u32)> {
    let end = env.sustain_point.map_or(env.points.len(), |i| i + 1);
    env.points[..end].iter().map(|p| (start_beat + p.tick as f64 * beats_per_tick, p.value)).collect()
}

/// The release portion of an envelope (the points *after* the sustain point), anchored at
/// `release_beat` and using the tick rate in effect *then* (not at trigger time — see
/// envelope_attack_points). Empty if the envelope has no sustain point, since in that case
/// everything was already emitted at trigger.
fn envelope_release_points(env: &Envelope, release_beat: f64, beats_per_tick: f64) -> Vec<(f64, u32)> {
    let Some(sustain) = env.sustain_point else { return Vec::new() };
    let sustain_tick = env.points[sustain].tick as f64;
    env.points[sustain + 1..].iter().map(|p| (release_beat + (p.tick as f64 - sustain_tick) * beats_per_tick, p.value)).collect()
}

/// Triggers a new note on `ch`, closing whatever was previously held there — the shared core
/// of a normal same-row note-on *and* a delayed one (Note Delay/EDx defers this call to a
/// later tick instead of running it inline; Retrigger/E9x calls it again mid-row on top of an
/// already-held note). Pulled out of the main per-row/per-channel loop below so both call
/// sites stay in sync instead of hand-duplicating this bookkeeping.
#[allow(clippy::too_many_arguments)]
fn trigger_note(
    at_beat: f64,
    midi_note: i32,
    ch: usize,
    cell: &Cell,
    active_sample: u32,
    sample: &Sample,
    bpm: f64,
    beats_per_tick: f64,
    notes_by_sample: &mut BTreeMap<u32, Vec<NoteEvent>>,
    channel_held: &mut [Option<(u32, usize)>],
    channel_period: &mut [Option<f64>],
    channel_trigger_volume: &mut [Option<i32>],
    channel_current_volume: &mut [Option<i32>],
    channel_vibrato_pos: &mut [f64],
    channel_tremolo_pos: &mut [f64],
    channel_sample_offset: &mut [u32],
) {
    if let Some((held_sample, held_idx)) = channel_held[ch] {
        notes_by_sample.get_mut(&held_sample).unwrap()[held_idx].close(at_beat);
        channel_held[ch] = None;
    }

    let trigger_volume = cell.volume.map(|v| v as i32).unwrap_or(sample.volume as i32);
    let vel = velocity(cell.volume, sample.volume);
    let mut event = NoteEvent::new(at_beat, midi_note, vel, trigger_volume, ch);
    channel_period[ch] = Some(note_to_period(midi_note));
    channel_trigger_volume[ch] = Some(trigger_volume);
    channel_current_volume[ch] = Some(trigger_volume);
    channel_vibrato_pos[ch] = 0.0; // ft2-clone: triggerInstrument() resets vibrato phase
    channel_tremolo_pos[ch] = 0.0; // ...and tremolo phase, the same way
    if !sample.has_loop() {
        // a tracker plays a non-looped sample through to its natural end and then goes
        // silent, regardless of how long the channel holds the note — it never sustains/
        // loops just because nothing re-triggers the channel for a while.
        let num_frames = sample.pcm16.len() / 2;
        let natural_seconds = num_frames as f64 / sample.sample_rate_hz as f64;
        event.max_duration_beat = Some(natural_seconds * (bpm / 60.0));
    }
    if cell.effect == Some(SET_PANNING) {
        event.pans.push(PanChange { at_beat, pan: pan_from_param(cell.effect_param.unwrap_or(0)), glide: false });
    }
    if let Some(env) = &sample.volume_envelope {
        for (i, (beat, value)) in envelope_attack_points(env, at_beat, beats_per_tick).into_iter().enumerate() {
            event.envelope_volumes.push(VolumeChange { at_beat: beat, tracker_volume: value as i32, glide: i != 0 });
        }
    }
    if let Some(env) = &sample.panning_envelope {
        for (i, (beat, value)) in envelope_attack_points(env, at_beat, beats_per_tick).into_iter().enumerate() {
            event.envelope_pans.push(PanChange { at_beat: beat, pan: envelope_pan_to_normalized(value), glide: i != 0 });
        }
    }
    // Sample Offset (9xx) only ever takes effect *at the moment of this trigger* (ft2-clone's
    // triggerNote() is the only place smpStartPos is set) — a 9xx on a row without a new note
    // is a no-op, and a note triggered *without* 9xx always starts from frame 0 regardless of
    // any earlier 9xx memory (`ch->smpStartPos = 0` in the `else` branch there). param=0
    // reuses the last nonzero offset this channel used.
    if cell.effect == Some(SAMPLE_OFFSET) {
        let raw_param = cell.effect_param.unwrap_or(0);
        if raw_param != 0 {
            channel_sample_offset[ch] = raw_param;
        }
        let num_frames = (sample.pcm16.len() / 2) as u32;
        event.sample_offset_frames = (channel_sample_offset[ch] * 256).min(num_frames.saturating_sub(1));
    }
    let list = notes_by_sample.get_mut(&active_sample).unwrap();
    list.push(event);
    channel_held[ch] = Some((active_sample, list.len() - 1));
}

/// Applies one row's worth of Volume Slide ticks (Axy's own effect, or the volume-slide half
/// of 5xy/6xy, which per ft2-clone's portamentoPlusVolSlide/vibratoPlusVolSlide share the
/// exact same volSlide() call — including its param=0 "reuse last nonzero rate" memory).
/// Returns whether it actually applied a nonzero rate, so the caller can set its own
/// "was this row a direct continuation" flag for the *next* row's glide decision.
#[allow(clippy::too_many_arguments)]
fn apply_volume_slide(
    beat: f64,
    speed: u32,
    beats_per_tick: f64,
    was_sliding: bool,
    raw_param: u32,
    ch: usize,
    held_sample: u32,
    held_idx: usize,
    notes_by_sample: &mut BTreeMap<u32, Vec<NoteEvent>>,
    channel_current_volume: &mut [Option<i32>],
    channel_volslide_speed: &mut [u32],
) -> bool {
    let param = if raw_param != 0 { raw_param } else { channel_volslide_speed[ch] };
    channel_volslide_speed[ch] = param;
    let up = (param >> 4) & 0x0F;
    let down = param & 0x0F;
    let delta_per_tick: i32 = if up != 0 { up as i32 } else { -(down as i32) };
    if delta_per_tick == 0 || channel_current_volume[ch].is_none() {
        return false;
    }
    let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
    for tick in 1..speed {
        let previous_volume = channel_current_volume[ch].unwrap();
        let new_volume = (previous_volume + delta_per_tick).clamp(0, 64);
        channel_current_volume[ch] = Some(new_volume);
        // once a slide clamps at 0 or 64, reapplying it every subsequent tick (or row, if the
        // same effect keeps getting reapplied — very common once a channel has faded out) is
        // a genuine no-op: skip it instead of emitting thousands of identical points that
        // only bloat the automation lane and bury the part of the curve that actually moves.
        if new_volume == previous_volume && !event.volumes.is_empty() {
            continue;
        }
        let tick_beat = beat + tick as f64 * beats_per_tick;
        // only the very first tick can possibly need a step (a jump from wherever the volume
        // was before this row); every later tick in this same row is always a direct
        // continuation of the one before.
        let glide = if tick == 1 { was_sliding } else { true };
        event.volumes.push(VolumeChange { at_beat: tick_beat, tracker_volume: new_volume, glide });
    }
    true
}

pub fn compute_song_events(module: &Module) -> SongEvents {
    let samples_by_index: BTreeMap<u32, &crate::formats::base::Sample> =
        module.samples.iter().map(|s| (s.index, s)).collect();
    let non_empty_samples: Vec<&crate::formats::base::Sample> =
        module.samples.iter().filter(|s| !s.is_empty()).collect();

    let mut notes_by_sample: BTreeMap<u32, Vec<NoteEvent>> =
        non_empty_samples.iter().map(|s| (s.index, Vec::new())).collect();

    let mut speed = module.initial_speed_ticks;
    let mut tracker_bpm = module.initial_tempo_bpm;
    let mut bpm = daw_bpm(tracker_bpm, speed);
    let mut tempo_changes = vec![TempoChange { at_beat: 0.0, bpm }];

    let mut beat: f64 = 0.0;
    let n = module.num_channels;
    let mut channel_current_sample: Vec<Option<u32>> = vec![None; n];
    // (sample_index, index into notes_by_sample[sample_index]) of the currently-held note.
    let mut channel_held: Vec<Option<(u32, usize)>> = vec![None; n];
    let mut channel_period: Vec<Option<f64>> = vec![None; n];
    let mut channel_trigger_volume: Vec<Option<i32>> = vec![None; n];
    let mut channel_current_volume: Vec<Option<i32>> = vec![None; n];
    // True if the *immediately preceding* row applied a Volume Slide on this channel — lets
    // a slide that's reapplied row after row (a sustained fade) glide smoothly across the row
    // boundary instead of holding-then-jumping at every row like a fresh, unrelated jump.
    let mut channel_sliding: Vec<bool> = vec![false; n];
    // same idea as channel_sliding, one per pitch-bending effect — each tracks whether *that
    // specific* effect was still actively running on the immediately preceding row, since a
    // channel can only run one of them at a time (Portamento Up/Down, Tone Portamento,
    // Vibrato) but which one can change from row to row.
    let mut channel_bending: Vec<bool> = vec![false; n];
    let mut channel_portamento_active: Vec<bool> = vec![false; n];
    let mut channel_vibrating: Vec<bool> = vec![false; n];
    let mut channel_tremoloing: Vec<bool> = vec![false; n];
    // Portamento Up/Down (1xx/2xx) and Volume Slide (Axy) all share the same "effect memory"
    // rule confirmed directly in ft2-clone's source (pitchSlideUp/pitchSlideDown/volSlide, all
    // in src/ft2_replayer.c): `if (param == 0) param = ch-><...>Speed;` then `ch-><...>Speed =
    // param;` — a row with param=0 *reuses* the last nonzero rate this exact effect used on
    // this channel, rather than doing nothing. This is the standard "<note> A08" then repeated
    // "--- A00" idiom for sustaining a fade/slide across many rows at a fixed rate; skipping
    // param=0 rows entirely (an earlier version of this code did, having missed this) truncates
    // that whole multi-row fade down to just its first row, making it read as far too fast.
    // Portamento Up and Down keep *independent* memories (matching FT2's separate
    // pitchSlideUpSpeed/pitchSlideDownSpeed fields) since a channel can switch direction and
    // still expect each direction's own last rate to come back on a later "100"/"200".
    let mut channel_portamento_up_speed: Vec<u32> = vec![0; n];
    let mut channel_portamento_down_speed: Vec<u32> = vec![0; n];
    let mut channel_volslide_speed: Vec<u32> = vec![0; n];
    // Tone Portamento (3xy) state has the same memory rule (ft2-clone's preparePortamento:
    // `if (p->efx != 5 && p->efxData != 0) ch->portamentoSpeed = p->efxData * 4;`), which is
    // exactly the standard "<note> 3xx" then "--- 300" idiom for continuing a slide.
    let mut channel_portamento_speed: Vec<f64> = vec![0.0; n];
    let mut channel_portamento_target: Vec<Option<f64>> = vec![None; n];
    // Vibrato (4xy) state: depth/speed are likewise remembered across rows and only updated
    // by a nonzero nibble (ft2-clone's vibrato(): `if (param>0) { if depth_nibble>0: ...
    // if speed_nibble>0: ... }`); vibratoPos is a running phase accumulator that only resets
    // on a new note trigger and otherwise freezes on rows without 4xy/6xy (see triggerInstrument()).
    let mut channel_vibrato_speed: Vec<f64> = vec![0.0; n];
    let mut channel_vibrato_depth: Vec<f64> = vec![0.0; n];
    let mut channel_vibrato_pos: Vec<f64> = vec![0.0; n];
    // Tremolo (7xy) state: same remembered-depth/speed rule as Vibrato (ft2-clone's tremolo()
    // mirrors vibrato() exactly), oscillating volume instead of pitch. tremoloPos resets on a
    // new note trigger like vibratoPos does.
    let mut channel_tremolo_speed: Vec<f64> = vec![0.0; n];
    let mut channel_tremolo_depth: Vec<f64> = vec![0.0; n];
    let mut channel_tremolo_pos: Vec<f64> = vec![0.0; n];
    // Sample Offset (9xx) state: remembers the last *nonzero* offset used on this channel (same
    // "set once, repeat with param=0" memory rule as Volume Slide/Portamento — ft2-clone's
    // triggerNote(): `if (efxData > 0) ch->sampleOffset = ch->efxData;`), in raw 256-frame
    // units. Unlike those effects, this only ever applies *at the moment a note triggers* — a
    // 9xx on a row with no new note is simply a no-op (confirmed: triggerNote() is the only
    // place ch->smpStartPos is touched).
    let mut channel_sample_offset: Vec<u32> = vec![0; n];
    // Fine Portamento (E1x/E2x) and Fine Volume Slide (EAx/EBx) each carry their *own*
    // separate memory slot in ft2-clone (fPitchSlideUpSpeed/fPitchSlideDownSpeed/
    // fVolSlideUpSpeed/fVolSlideDownSpeed) — distinct from the ordinary 1xx/2xx/Axy memory
    // above, so a channel can use both the fine and non-fine version of an effect and each
    // remembers its own last rate independently.
    let mut channel_fine_portamento_up_speed: Vec<u32> = vec![0; n];
    let mut channel_fine_portamento_down_speed: Vec<u32> = vec![0; n];
    let mut channel_fine_volslide_up_speed: Vec<u32> = vec![0; n];
    let mut channel_fine_volslide_down_speed: Vec<u32> = vec![0; n];

    let mut segments: Vec<Segment> = Vec::new();
    let mut current_segment_index: Option<usize> = None;

    let song_rows = iter_song_rows(module);
    for song_row in &song_rows {
        let order_pos = song_row.order_pos;
        let pattern_index = song_row.pattern_index;
        let row = song_row.row;

        let need_new_segment = match current_segment_index {
            None => true,
            Some(i) => segments[i].order_pos != order_pos,
        };
        if need_new_segment {
            if let Some(i) = current_segment_index {
                segments[i].end_beat = beat;
            }
            segments.push(Segment { start_beat: beat, end_beat: beat, order_pos, pattern_index });
            current_segment_index = Some(segments.len() - 1);
        }

        for cell in row {
            if cell.effect == Some(0xF) {
                if let Some(param) = cell.effect_param {
                    if param != 0 {
                        // 0-31 sets Speed, 32-255 sets Tempo/BPM — verified against ft2-clone's
                        // setSpeed (src/ft2_replayer.c: `if (param >= 32) BPM = param; else speed
                        // = param;"), not just documentation: 32 itself is a Tempo value, not Speed.
                        if param < 32 {
                            speed = param;
                        } else {
                            tracker_bpm = param;
                        }
                        let new_bpm = daw_bpm(tracker_bpm, speed);
                        if new_bpm != bpm {
                            bpm = new_bpm;
                            let last = tempo_changes.last_mut().unwrap();
                            if last.at_beat == beat {
                                last.bpm = bpm; // same beat: last one wins
                            } else {
                                tempo_changes.push(TempoChange { at_beat: beat, bpm });
                            }
                        }
                    }
                }
            }
        }

        let beats_per_tick = BEATS_PER_ROW / speed as f64;

        for (ch, cell) in row.iter().enumerate() {
            if let Some(sample_index) = cell.sample_index {
                channel_current_sample[ch] = Some(sample_index);
            }

            // Reset every "was this row a direct continuation" flag by default; the matching
            // effect branch below sets its own back to True if it actually reapplies this
            // row. Only one pitch-bending effect can be active on a channel at a time, but
            // which one can change row to row, so each gets its own flag.
            let mut was_sliding = channel_sliding[ch];
            channel_sliding[ch] = false;
            let mut was_bending = channel_bending[ch];
            channel_bending[ch] = false;
            let mut was_portamento_active = channel_portamento_active[ch];
            channel_portamento_active[ch] = false;
            let mut was_vibrating = channel_vibrating[ch];
            channel_vibrating[ch] = false;
            let mut was_tremoloing = channel_tremoloing[ch];
            channel_tremoloing[ch] = false;

            // Note Delay (ED1..EDF): ft2-clone's getNewNote() returns immediately — before
            // touching the note/instrument/tick-zero-effects at all — for exactly this param
            // range, deferring everything to noteDelay() firing later at tick == sub_param (see
            // the EXTENDED branch below). ED0 is *not* in this range and behaves as a perfectly
            // normal, undelayed trigger.
            let note_delay_ticks: Option<u32> = if cell.effect == Some(EXTENDED) {
                let raw = cell.effect_param.unwrap_or(0);
                let sub_param = raw & 0x0F;
                (((raw >> 4) & 0x0F == E_NOTE_DELAY) && sub_param != 0).then_some(sub_param)
            } else {
                None
            };

            if cell.effect == Some(TONE_PORTAMENTO) {
                // 3xy never retriggers the sample — a note here is a new *target* period for
                // the currently held note to glide toward (ft2-clone's preparePortamento()
                // skips CS_TRIGGER_VOICE entirely for this effect), not a new note-on.
                if let Some(midi_note) = cell.midi_note {
                    channel_portamento_target[ch] = Some(note_to_period(midi_note));
                }
                if let Some(param) = cell.effect_param {
                    if param != 0 {
                        // 0 = keep the previously-set speed (real effect memory)
                        channel_portamento_speed[ch] = param as f64 * 4.0;
                    }
                }
            } else if let Some(delay_ticks) = note_delay_ticks {
                // Handled here rather than alongside the other EXTENDED (Exx) sub-commands
                // further down: those all *modify* an already-held note and so run after the
                // "is anything held on this channel" guard below, but this is the delayed
                // trigger of a brand new note — it must work even when *nothing* was held
                // before it (this channel's very first note ever, e.g.), so it can't depend on
                // that guard passing.
                if let Some(midi_note) = cell.midi_note {
                    if let Some(active_sample) = channel_current_sample[ch] {
                        if let Some(sample) = samples_by_index.get(&active_sample) {
                            if notes_by_sample.contains_key(&active_sample) {
                                for tick in 1..speed {
                                    if tick == delay_ticks {
                                        let at_beat = beat + tick as f64 * beats_per_tick;
                                        trigger_note(
                                            at_beat, midi_note, ch, cell, active_sample, sample, bpm, beats_per_tick,
                                            &mut notes_by_sample, &mut channel_held, &mut channel_period,
                                            &mut channel_trigger_volume, &mut channel_current_volume,
                                            &mut channel_vibrato_pos, &mut channel_tremolo_pos,
                                            &mut channel_sample_offset,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                continue; // the note (if any) is fully handled above; no per-tick effects follow on this row
            } else if let Some(midi_note) = cell.midi_note {
                if let Some(active_sample) = channel_current_sample[ch] {
                    if let Some(sample) = samples_by_index.get(&active_sample) {
                        if notes_by_sample.contains_key(&active_sample) {
                            trigger_note(
                                beat, midi_note, ch, cell, active_sample, sample, bpm, beats_per_tick, &mut notes_by_sample,
                                &mut channel_held, &mut channel_period, &mut channel_trigger_volume,
                                &mut channel_current_volume, &mut channel_vibrato_pos, &mut channel_tremolo_pos,
                                &mut channel_sample_offset,
                            );
                            // a fresh note-on has no continuity with whatever was happening on
                            // this channel's *previous* note — without this, the new note's
                            // first tick of Volume Slide/Portamento/Vibrato could inherit a
                            // stale "was active" flag from the note that just ended, wrongly
                            // gliding in from that other note's last value instead of stepping
                            // cleanly from this note's own baseline.
                            was_sliding = false;
                            was_bending = false;
                            was_portamento_active = false;
                            was_vibrating = false;
                            was_tremoloing = false;
                        }
                    }
                }
            } else if cell.note_off {
                // XM Key Off (note value 97, or effect Kxx): does *not* end the note outright
                // — a channel with no volume/panning envelope just keeps ringing exactly as it
                // would without this row (matching how MOD, which has no note-off concept at
                // all, already behaves) — it only marks where an envelope's release phase, if
                // any, should begin. Guarded so a channel that already released (e.g. two Kxx
                // rows in a row) doesn't emit a second, overlapping release ramp.
                if let Some((held_sample, held_idx)) = channel_held[ch] {
                    let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                    if event.release_beat.is_none() {
                        event.release_beat = Some(beat);
                        if let Some(active_sample) = channel_current_sample[ch] {
                            if let Some(sample) = samples_by_index.get(&active_sample) {
                                if let Some(env) = &sample.volume_envelope {
                                    for (rbeat, value) in envelope_release_points(env, beat, beats_per_tick) {
                                        event.envelope_volumes.push(VolumeChange { at_beat: rbeat, tracker_volume: value as i32, glide: true });
                                    }
                                }
                                if let Some(env) = &sample.panning_envelope {
                                    for (rbeat, value) in envelope_release_points(env, beat, beats_per_tick) {
                                        event.envelope_pans.push(PanChange { at_beat: rbeat, pan: envelope_pan_to_normalized(value), glide: true });
                                    }
                                }
                            }
                        }
                    }
                }
            } else if let Some(new_sample_index) = cell.sample_index {
                // ProTracker compatibility (verified against OpenMPT's Snd_fx.cpp SONG_PT_MODE
                // branch, test cases PTInstrVolume.mod/PTSwapEmpty.mod): a lone sample number
                // with *no* note, while a sample is currently playing on this channel, does
                // NOT retrigger the note's pitch/position — it swaps the channel's current
                // sample and resets its volume to *that new sample's own default*, not to
                // whatever the held note's original trigger volume was. This is a genuine jump
                // (not a per-tick slide), so any 6xy/Axy/Cxx elsewhere on this same row then
                // continues cumulatively from this new baseline, not from the old one.
                if let Some((held_sample, held_idx)) = channel_held[ch] {
                    if let Some(new_sample) = samples_by_index.get(&new_sample_index) {
                        let new_volume = new_sample.volume as i32;
                        channel_current_volume[ch] = Some(new_volume);
                        notes_by_sample.get_mut(&held_sample).unwrap()[held_idx]
                            .volumes
                            .push(VolumeChange { at_beat: beat, tracker_volume: new_volume, glide: false });
                    }
                }
            }

            // Whatever is now held — just triggered above, mid-slide via Tone Portamento, or
            // simply continuing from an earlier row — is this row's target for the per-tick
            // effects below. Real hardware applies these every tick regardless of whether
            // *this* row also happened to trigger a new note (ft2-clone dispatches ticks
            // 1..speed-1 purely by the channel's current effect, independent of whether tick
            // 0 had a note) — most obviously needed for Arpeggio, almost always written
            // directly on the same row as the note it decorates.
            let Some((held_sample, held_idx)) = channel_held[ch] else { continue };
            let is_new_trigger = cell.midi_note.is_some() && cell.effect != Some(TONE_PORTAMENTO);

            if !is_new_trigger {
                // Set Volume/kill and Set Panning only make sense as a *modification* of an
                // already-playing note — a fresh trigger's starting volume/pan already came
                // from trigger_volume/the note-trigger branch above.
                if cell.effect == Some(SET_VOLUME) && cell.effect_param == Some(0) {
                    // Cxx (Set Volume) to 0: standard tracker technique to cut the currently
                    // ringing note dead, not "play it at near-silent volume".
                    notes_by_sample.get_mut(&held_sample).unwrap()[held_idx].close(beat);
                    channel_held[ch] = None;
                } else if cell.effect == Some(SET_VOLUME) {
                    if let (Some(param), Some(_)) = (cell.effect_param, channel_trigger_volume[ch]) {
                        if param != 0 {
                            // some modules carry out-of-spec Cxx params above the valid 0-64 range
                            // (real players clamp rather than reject them) — without this, an
                            // unclamped value here would otherwise skew the volume math below.
                            let new_vol = param.min(64) as i32;
                            channel_current_volume[ch] = Some(new_vol);
                            notes_by_sample.get_mut(&held_sample).unwrap()[held_idx]
                                .volumes
                                .push(VolumeChange { at_beat: beat, tracker_volume: new_vol, glide: false });
                        }
                    }
                }

                if channel_held[ch].is_some() && cell.effect == Some(SET_PANNING) {
                    let (hs, hi) = channel_held[ch].unwrap();
                    notes_by_sample.get_mut(&hs).unwrap()[hi]
                        .pans
                        .push(PanChange { at_beat: beat, pan: pan_from_param(cell.effect_param.unwrap_or(0)), glide: false });
                }
            }

            let Some((held_sample, held_idx)) = channel_held[ch] else { continue };

            if cell.effect == Some(VOLUME_SLIDE) && channel_current_volume[ch].is_some() {
                // Verified empirically against real playback (isolating a single channel and
                // measuring its actual amplitude, not just reading the formula off the page):
                // a Volume Slide genuinely continues cumulatively from wherever the previous
                // tick/row left the volume — matches FT2's volSlide() and OpenMPT's
                // VolumeSlide() source directly (`realVol -= param`, reading the channel's
                // *current* volume, no reset). An earlier attempt to make this reset to the
                // note's trigger volume every row was based on one anomalous passage that
                // turned out not to generalize — reverted after a second, cleanly isolated
                // passage (PINBALLF pattern 01/track 3's closing "601" fade) confirmed a
                // repeated slide must reach and hold true silence, not oscillate forever.
                //
                // param=0 reuses the last nonzero param this channel's Axy used (see
                // channel_volslide_speed above) — the standard "A08" then repeated "A00" idiom
                // for sustaining a fade across many rows; treating param=0 as a no-op (as an
                // earlier version of this code did) truncated that fade to just its first row.
                let raw_param = cell.effect_param.unwrap_or(0);
                if apply_volume_slide(
                    beat, speed, beats_per_tick, was_sliding, raw_param, ch, held_sample, held_idx,
                    &mut notes_by_sample, &mut channel_current_volume, &mut channel_volslide_speed,
                ) {
                    channel_sliding[ch] = true;
                }
            } else if (cell.effect == Some(PORTAMENTO_UP) || cell.effect == Some(PORTAMENTO_DOWN)) && channel_period[ch].is_some()
            {
                let is_up = cell.effect == Some(PORTAMENTO_UP);
                let sign: f64 = if is_up { -1.0 } else { 1.0 }; // period down = pitch up
                // param=0 reuses the last nonzero param *this direction* used on this channel
                // (see channel_portamento_up_speed/channel_portamento_down_speed above) — same
                // "set once, repeat with param=0" idiom as Volume Slide above, confirmed against
                // ft2-clone's pitchSlideUp/pitchSlideDown source.
                let raw_param = cell.effect_param.unwrap_or(0);
                let param = if raw_param != 0 {
                    raw_param
                } else if is_up {
                    channel_portamento_up_speed[ch]
                } else {
                    channel_portamento_down_speed[ch]
                };
                if is_up {
                    channel_portamento_up_speed[ch] = param;
                } else {
                    channel_portamento_down_speed[ch] = param;
                }
                if param == 0 {
                    continue; // no rate to apply yet (e.g. a stray 100/200 with no prior slide)
                }
                // the real hardware applies the delta *every tick*, not once per row —
                // emitting one bend point per tick (not one lump sum for the whole row) is
                // what makes this read as a smooth glide instead of an instant jump that
                // sounds like the note got retriggered. The period moves by the raw effect
                // parameter directly (no extra multiplier) — an earlier ×4 scale, read
                // literally off ft2-clone's pitchSlideUp/pitchSlideDown source, sounded
                // noticeably too strong in Ableton; isolating a single note (period 428) and
                // measuring a real "1xy, param=2" slide's actual pitch rise via libopenmpt
                // confirmed the raw-parameter scale matches real playback (~0.35 semitones
                // over one row at speed 6), not the ×4-scaled ~1.7 semitones. The ft2-clone
                // ×4 figure most likely applies to FT2's own internal fine-period
                // representation, not the plain Amiga/MOD period slides this exporter works in.
                let param = param as f64;
                let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                for tick in 1..speed {
                    let new_period = (channel_period[ch].unwrap() + sign * param).clamp(1.0, 32000.0);
                    channel_period[ch] = Some(new_period);
                    let tick_beat = beat + tick as f64 * beats_per_tick;
                    let fractional_note = period_to_fractional_note(new_period);
                    let glide = if tick == 1 { was_bending } else { true };
                    event.bends.push(PitchBend { at_beat: tick_beat, semitones: fractional_note - event.pitch as f64, glide });
                }
                channel_bending[ch] = true;
            } else if (cell.effect == Some(TONE_PORTAMENTO) || cell.effect == Some(TONE_PORTAMENTO_VOLSLIDE))
                && channel_portamento_target[ch].is_some()
                && channel_period[ch].is_some()
            {
                // unlike 1xx/2xx, this slides *toward* a fixed target and stops there exactly
                // (ft2-clone's portamento(): moves by portamentoSpeed per tick, clamping at
                // portamentoTargetPeriod rather than sliding past it). 5xy (Tone Portamento +
                // Volume Slide) continues this *same* glide at whatever speed 3xy last set —
                // ft2-clone's portamentoPlusVolSlide() calls `portamento(ch, 0)`, i.e. it never
                // updates the portamento speed itself, only its own param feeds a Volume Slide
                // (below) — matching how the early TONE_PORTAMENTO branch above only updates
                // channel_portamento_speed for effect==TONE_PORTAMENTO, never for this one.
                let target = channel_portamento_target[ch].unwrap();
                let speed_per_tick = channel_portamento_speed[ch];
                let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                for tick in 1..speed {
                    let mut current = channel_period[ch].unwrap();
                    if speed_per_tick != 0.0 && current != target {
                        current = if current < target {
                            (current + speed_per_tick).min(target)
                        } else {
                            (current - speed_per_tick).max(target)
                        };
                        channel_period[ch] = Some(current);
                    }
                    let tick_beat = beat + tick as f64 * beats_per_tick;
                    let fractional_note = period_to_fractional_note(current);
                    let glide = if tick == 1 { was_portamento_active } else { true };
                    event.bends.push(PitchBend { at_beat: tick_beat, semitones: fractional_note - event.pitch as f64, glide });
                }
                channel_portamento_active[ch] = true;

                if cell.effect == Some(TONE_PORTAMENTO_VOLSLIDE) && channel_current_volume[ch].is_some() {
                    // shares the exact same volSlide() call (and its param=0 memory) as Axy —
                    // see apply_volume_slide.
                    let raw_param = cell.effect_param.unwrap_or(0);
                    if apply_volume_slide(
                        beat, speed, beats_per_tick, was_sliding, raw_param, ch, held_sample, held_idx,
                        &mut notes_by_sample, &mut channel_current_volume, &mut channel_volslide_speed,
                    ) {
                        channel_sliding[ch] = true;
                    }
                }
            } else if (cell.effect == Some(VIBRATO) || cell.effect == Some(VIBRATO_VOLSLIDE))
                && channel_period[ch].is_some()
            {
                if cell.effect == Some(VIBRATO) {
                    if let Some(param) = cell.effect_param {
                        if param != 0 {
                            // depth/speed are remembered across rows and only updated by a nonzero
                            // nibble (ft2-clone's vibrato()) — a bare "400" (or 6xy, which never
                            // touches these at all) continues the existing wobble unchanged.
                            let depth_nibble = param & 0x0F;
                            if depth_nibble != 0 {
                                channel_vibrato_depth[ch] = depth_nibble as f64;
                            }
                            let speed_nibble = (param >> 4) & 0x0F;
                            if speed_nibble != 0 {
                                channel_vibrato_speed[ch] = speed_nibble as f64 * 4.0;
                            }
                        }
                    }
                }

                let vib_speed = channel_vibrato_speed[ch];
                let vib_depth = channel_vibrato_depth[ch];
                if vib_speed != 0.0 || vib_depth != 0.0 {
                    let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                    for tick in 1..speed {
                        channel_vibrato_pos[ch] += vib_speed;
                        // ft2-clone's doVibrato() reconstructs a full sine cycle from an 8-bit
                        // wrapping position via a 32-entry quarter-sine table + sign flip; a
                        // continuous sin() reproduces the exact same shape without the 8-bit
                        // stairstep quantization, which the eventual MIDI/automation export
                        // resolution would wash out anyway. The depth/32 scale read directly
                        // from both ft2-clone's and OpenMPT's source (they agree with each
                        // other) turned out to sound ~4x too strong in Ableton compared to a
                        // real player: isolating a single sine-wave test instrument and
                        // measuring its actual pitch swing from real OpenMPT audio (via
                        // libopenmpt) at two different depths both showed a consistent ~4.3x
                        // gap, not just a one-off reading — an extra /4 (i.e. /128 overall)
                        // reproduces the measured swing.
                        let phase = (channel_vibrato_pos[ch].rem_euclid(256.0)) / 256.0;
                        let offset = (2.0 * std::f64::consts::PI * phase).sin() * 255.0 * vib_depth / 128.0;
                        let tick_beat = beat + tick as f64 * beats_per_tick;
                        let fractional_note = period_to_fractional_note(channel_period[ch].unwrap() + offset);
                        let glide = if tick == 1 { was_vibrating } else { true };
                        event.bends.push(PitchBend { at_beat: tick_beat, semitones: fractional_note - event.pitch as f64, glide });
                    }
                    channel_vibrating[ch] = true;
                }

                if cell.effect == Some(VIBRATO_VOLSLIDE) && channel_current_volume[ch].is_some() {
                    // shares the exact same volSlide() call (and its param=0 memory) as Axy/5xy
                    // — see apply_volume_slide. This channel_volslide_speed memory previously
                    // wasn't shared with 6xy here (it required a nonzero param every time),
                    // which broke the common "601" once then repeated "600" idiom the same way
                    // the Axy/1xx/2xx param=0 bug once did.
                    let raw_param = cell.effect_param.unwrap_or(0);
                    if apply_volume_slide(
                        beat, speed, beats_per_tick, was_sliding, raw_param, ch, held_sample, held_idx,
                        &mut notes_by_sample, &mut channel_current_volume, &mut channel_volslide_speed,
                    ) {
                        channel_sliding[ch] = true;
                    }
                }
            } else if cell.effect == Some(TREMOLO) && channel_current_volume[ch].is_some() {
                if let Some(param) = cell.effect_param {
                    if param != 0 {
                        // depth/speed are remembered across rows and only updated by a nonzero
                        // nibble, exactly like Vibrato (ft2-clone's tremolo() mirrors vibrato()).
                        let depth_nibble = param & 0x0F;
                        if depth_nibble != 0 {
                            channel_tremolo_depth[ch] = depth_nibble as f64;
                        }
                        let speed_nibble = (param >> 4) & 0x0F;
                        if speed_nibble != 0 {
                            channel_tremolo_speed[ch] = speed_nibble as f64 * 4.0;
                        }
                    }
                }

                let trem_speed = channel_tremolo_speed[ch];
                let trem_depth = channel_tremolo_depth[ch];
                if trem_speed != 0.0 || trem_depth != 0.0 {
                    let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                    for tick in 1..speed {
                        channel_tremolo_pos[ch] += trem_speed;
                        // Unlike Vibrato (whose literal ft2-clone/OpenMPT depth scale was
                        // measured ~4x too strong against real playback via libopenmpt — see
                        // the VIBRATO branch above), Tremolo hasn't had that same empirical
                        // check done, so this uses ft2-clone's literal formula as-is
                        // (`(sine * depth) >> 6`, i.e. depth/64): a continuous sine
                        // reconstruction instead of its 8-bit LUT, same rationale as Vibrato's.
                        // Volume (linear, clamped 0-64) isn't subject to the period→semitone
                        // log-conversion math that was the likely root cause of Vibrato's
                        // factor-of-4 error, so there's no obvious reason to expect the same
                        // correction applies here — flagging this rather than guessing a fix
                        // with no evidence behind it.
                        let phase = (channel_tremolo_pos[ch].rem_euclid(256.0)) / 256.0;
                        let offset = (2.0 * std::f64::consts::PI * phase).sin() * 255.0 * trem_depth / 64.0;
                        let previous_volume = channel_current_volume[ch].unwrap();
                        let new_volume = (previous_volume as f64 + offset).round().clamp(0.0, 64.0) as i32;
                        // unlike Volume Slide, Tremolo's oscillation never touches the
                        // channel's *persistent* realVol — only this tick's displayed outVol —
                        // so channel_current_volume is deliberately left untouched here; the
                        // next Volume Slide/Set Volume still continues from the pre-tremolo
                        // baseline, not from wherever the wobble last landed.
                        let tick_beat = beat + tick as f64 * beats_per_tick;
                        let glide = if tick == 1 { was_tremoloing } else { true };
                        event.volumes.push(VolumeChange { at_beat: tick_beat, tracker_volume: new_volume, glide });
                    }
                    channel_tremoloing[ch] = true;
                }
            } else if cell.effect == Some(ARPEGGIO) && cell.effect_param.unwrap_or(0) != 0 && channel_period[ch].is_some() {
                // discrete, instant note jumps cycling base/+x/+y every tick — always stepped
                // (glide=False), never smoothed, or it would lose arpeggio's characteristic
                // buzzy/blocky sound. ARPEGGIO_TAB indexing (not a clean "% 3") matches
                // ft2-clone's exact tick-numbering quirk; see its definition for why.
                let param = cell.effect_param.unwrap();
                let x = (param >> 4) & 0x0F;
                let y = param & 0x0F;
                let base_fractional_note = period_to_fractional_note(channel_period[ch].unwrap());
                let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                for tick in 1..speed {
                    let ft2_tick = speed - tick; // ft2-clone's tick counter runs *down* to 1, not up
                    let arp_kind = ARPEGGIO_TAB[ft2_tick as usize];
                    let note_offset = if arp_kind == 0 { 0 } else if arp_kind == 1 { x } else { y };
                    let tick_beat = beat + tick as f64 * beats_per_tick;
                    let semitones = (base_fractional_note + note_offset as f64) - event.pitch as f64;
                    event.bends.push(PitchBend { at_beat: tick_beat, semitones, glide: false });
                }
            } else if cell.effect == Some(EXTENDED) {
                let raw_param = cell.effect_param.unwrap_or(0);
                let sub_command = (raw_param >> 4) & 0x0F;
                let sub_param = raw_param & 0x0F;

                match sub_command {
                    E_FINE_PORTAMENTO_UP | E_FINE_PORTAMENTO_DOWN if channel_period[ch].is_some() => {
                        let is_up = sub_command == E_FINE_PORTAMENTO_UP;
                        let raw = if sub_param != 0 {
                            sub_param
                        } else if is_up {
                            channel_fine_portamento_up_speed[ch]
                        } else {
                            channel_fine_portamento_down_speed[ch]
                        };
                        if is_up {
                            channel_fine_portamento_up_speed[ch] = raw;
                        } else {
                            channel_fine_portamento_down_speed[ch] = raw;
                        }
                        if raw != 0 {
                            // one-shot, applied once at the start of the row — not per-tick
                            // like the ordinary 1xx/2xx, so unlike that effect (see its comment
                            // above) there's no per-tick compounding here to worry about, and
                            // ft2-clone's literal ×4 scale (finePitchSlideUp/Down: `realPeriod
                            // -= param*4`) is used as-is.
                            let sign: f64 = if is_up { -1.0 } else { 1.0 };
                            let new_period = (channel_period[ch].unwrap() + sign * raw as f64 * 4.0).clamp(1.0, 32000.0);
                            channel_period[ch] = Some(new_period);
                            let fractional_note = period_to_fractional_note(new_period);
                            let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                            event.bends.push(PitchBend {
                                at_beat: beat,
                                semitones: fractional_note - event.pitch as f64,
                                glide: false,
                            });
                        }
                    }
                    E_FINE_VOLSLIDE_UP | E_FINE_VOLSLIDE_DOWN if channel_current_volume[ch].is_some() => {
                        let is_up = sub_command == E_FINE_VOLSLIDE_UP;
                        let raw = if sub_param != 0 {
                            sub_param
                        } else if is_up {
                            channel_fine_volslide_up_speed[ch]
                        } else {
                            channel_fine_volslide_down_speed[ch]
                        };
                        if is_up {
                            channel_fine_volslide_up_speed[ch] = raw;
                        } else {
                            channel_fine_volslide_down_speed[ch] = raw;
                        }
                        if raw != 0 {
                            let delta = if is_up { raw as i32 } else { -(raw as i32) };
                            let new_volume = (channel_current_volume[ch].unwrap() + delta).clamp(0, 64);
                            channel_current_volume[ch] = Some(new_volume);
                            let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                            event.volumes.push(VolumeChange { at_beat: beat, tracker_volume: new_volume, glide: false });
                        }
                    }
                    E_RETRIGGER if sub_param != 0 => {
                        // retriggers every sub_param ticks — a genuine new NoteEvent each time
                        // (unlike every other per-tick effect here, which bend/slide the *same*
                        // note), reusing this row's own note/instrument/volume data. ft2-clone's
                        // retrigNote() calls triggerNote(0,0,0,ch) — note=0 keeps the channel's
                        // *current* period rather than resetting to the original trigger pitch,
                        // but combining Retrigger with an active Portamento/Vibrato on the same
                        // channel is rare enough that always retriggering at the note's own
                        // original pitch (this simplification) was chosen over the extra
                        // complexity of tracking that interaction precisely.
                        if let Some(active_sample) = channel_current_sample[ch] {
                            if let Some(sample) = samples_by_index.get(&active_sample).copied() {
                                let pitch = notes_by_sample[&held_sample][held_idx].pitch;
                                for tick in 1..speed {
                                    if tick % sub_param == 0 {
                                        let at_beat = beat + tick as f64 * beats_per_tick;
                                        trigger_note(
                                            at_beat, pitch, ch, cell, active_sample, sample, bpm, beats_per_tick,
                                            &mut notes_by_sample, &mut channel_held, &mut channel_period,
                                            &mut channel_trigger_volume, &mut channel_current_volume,
                                            &mut channel_vibrato_pos, &mut channel_tremolo_pos,
                                            &mut channel_sample_offset,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    E_NOTE_CUT => {
                        // cuts to silence exactly at tick == sub_param (0 = immediately, at the
                        // row's own start) — ft2-clone splits this into noteCut0 (tick zero) vs
                        // noteCut (tick nonzero) but the effect is identical either way: hold
                        // silent, don't end the note.
                        if sub_param == 0 {
                            channel_current_volume[ch] = Some(0);
                            let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                            event.volumes.push(VolumeChange { at_beat: beat, tracker_volume: 0, glide: false });
                        } else {
                            for tick in 1..speed {
                                if tick == sub_param {
                                    channel_current_volume[ch] = Some(0);
                                    let tick_beat = beat + tick as f64 * beats_per_tick;
                                    let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                                    event.volumes.push(VolumeChange { at_beat: tick_beat, tracker_volume: 0, glide: false });
                                }
                            }
                        }
                    }
                    // E3x (Glissando Control), E4x (Vibrato Waveform), E5x (Set Finetune), E6x
                    // (Pattern Loop), E7x (Tremolo Waveform), E8x (unused in real ProTracker),
                    // EEx (Pattern Delay), EFx (unused in real ProTracker) remain unimplemented
                    // — see unimplemented_effect_counts/README for what this means in practice.
                    _ => {}
                }
            }
        }

        beat += BEATS_PER_ROW;
    }

    for held in &channel_held {
        if let Some((sample_index, idx)) = held {
            notes_by_sample.get_mut(sample_index).unwrap()[*idx].close(beat);
        }
    }

    if let Some(i) = current_segment_index {
        segments[i].end_beat = beat;
    }

    // Notes from different tracker channels sharing this sample (see the "one track per
    // sample" grouping above) are only guaranteed non-overlapping *within* their own
    // channel — two channels can legitimately trigger the same sample with overlapping
    // durations (one still ringing when the other retriggers it). A single Ableton track
    // can only play one note at a time without Ableton dropping/merging overlapping
    // MidiNoteEvents on load, so rather than truncating one note to make room for another,
    // assign each note a "voice": notes sharing a voice never overlap in time, and
    // export::als/export::midi lay out one track per voice actually needed. This is the
    // standard greedy interval-scheduling algorithm (process notes in start-time order,
    // reuse any voice that's already free, else open a new one) and is proven to use the
    // minimum number of voices possible.
    //
    // Notes with *different* Sample Offset (9xx) values can never share a voice/track either,
    // even if their time ranges don't overlap at all — each needs its own MultiSamplePart
    // with its own SampleStart (see export::als::build_track). So each distinct offset value
    // used on this sample gets its own independent run of the algorithm above, first; voice
    // numbers stay globally unique per *sample* (not reset to 0 for each offset group) by
    // simply continuing to count up across groups, so a plain `voice`/`voice_count` lookup
    // downstream still can't accidentally merge two different offset groups onto one track.
    for events in notes_by_sample.values_mut() {
        events.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());
        let mut offsets: Vec<u32> = events.iter().map(|e| e.sample_offset_frames).collect();
        offsets.sort_unstable();
        offsets.dedup();

        let mut next_voice = 0usize;
        for &offset in &offsets {
            let idxs: Vec<usize> = (0..events.len()).filter(|&i| events[i].sample_offset_frames == offset).collect();
            let mut voice_end: Vec<f64> = Vec::new();
            let mut voice_last_idx: Vec<usize> = Vec::new();
            for &i in &idxs {
                let start = events[i].start_beat;
                let local_voice = match (0..voice_end.len()).find(|&v| voice_end[v] <= start) {
                    Some(v) => {
                        // leave the same hairline gap enforced elsewhere between consecutive
                        // notes so Ableton doesn't merge two touching notes on load.
                        let prev_idx = voice_last_idx[v];
                        let gap = start - events[prev_idx].start_beat - NOTE_GAP_BEATS;
                        events[prev_idx].duration_beat = events[prev_idx].duration_beat.min(gap);
                        v
                    }
                    None => {
                        voice_end.push(0.0);
                        voice_last_idx.push(0);
                        voice_end.len() - 1
                    }
                };
                events[i].voice = next_voice + local_voice;
                voice_end[local_voice] = start + events[i].duration_beat;
                voice_last_idx[local_voice] = i;
            }
            next_voice += voice_end.len();
        }
        for event in events.iter_mut() {
            if event.duration_beat <= 0.0 {
                event.duration_beat = NOTE_GAP_BEATS;
            }
        }
    }

    SongEvents { notes_by_sample, tempo_changes, total_beats: beat, segments }
}

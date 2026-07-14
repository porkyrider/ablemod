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

use crate::formats::base::Module;
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
const VIBRATO_VOLSLIDE: u32 = 0x6;
const SET_PANNING: u32 = 0x8;
const VOLUME_SLIDE: u32 = 0xA;
const SET_VOLUME: u32 = 0xC;

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
    pub pan: f64, // -1..1, absolute (0 = center), from a Set Panning (8xx) effect
}

#[derive(Debug, Clone)]
pub struct NoteEvent {
    pub start_beat: f64,
    pub duration_beat: f64,
    pub velocity: i32, // 0-127
    pub pitch: i32,    // MIDI note number
    pub trigger_volume: i32, // 0-64 tracker volume at note-on, for Volume Slide/Set Volume math
    pub channel: usize, // tracker channel index the note was triggered on, for optional hardwired Amiga/Atari panning (export::als::AmigaPanning)
    pub max_duration_beat: Option<f64>, // non-looped samples: never sustain past their natural length
    pub bends: Vec<PitchBend>,
    pub volumes: Vec<VolumeChange>,
    pub pans: Vec<PanChange>,
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
            max_duration_beat: None,
            bends: Vec::new(),
            volumes: Vec::new(),
            pans: Vec::new(),
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
    // Tone Portamento (3xy) state: unlike 1xx/2xx/Axy, a .mod-sourced 3xy/5xy with param=0
    // *keeps* the previously-set speed (real effect memory — ft2-clone's preparePortamento:
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
            } else if let Some(midi_note) = cell.midi_note {
                if let Some((held_sample, held_idx)) = channel_held[ch] {
                    notes_by_sample.get_mut(&held_sample).unwrap()[held_idx].close(beat);
                    channel_held[ch] = None;
                }

                if let Some(active_sample) = channel_current_sample[ch] {
                    if let Some(sample) = samples_by_index.get(&active_sample) {
                        if notes_by_sample.contains_key(&active_sample) {
                            let trigger_volume = cell.volume.map(|v| v as i32).unwrap_or(sample.volume as i32);
                            let vel = velocity(cell.volume, sample.volume);
                            let mut event = NoteEvent::new(beat, midi_note, vel, trigger_volume, ch);
                            channel_period[ch] = Some(note_to_period(midi_note));
                            channel_trigger_volume[ch] = Some(trigger_volume);
                            channel_current_volume[ch] = Some(trigger_volume);
                            channel_vibrato_pos[ch] = 0.0; // ft2-clone: triggerInstrument() resets vibrato phase
                            // a fresh note-on has no continuity with whatever was happening on this
                            // channel's *previous* note — without this, the new note's first tick of
                            // Volume Slide/Portamento/Vibrato could inherit a stale "was active" flag
                            // from the note that just ended, wrongly gliding in from that other note's
                            // last value instead of stepping cleanly from this note's own baseline.
                            was_sliding = false;
                            was_bending = false;
                            was_portamento_active = false;
                            was_vibrating = false;
                            if !sample.has_loop() {
                                // a tracker plays a non-looped sample through to its natural end and
                                // then goes silent, regardless of how long the channel holds the
                                // note — it never sustains/loops just because nothing re-triggers
                                // the channel for a while.
                                let num_frames = sample.pcm16.len() / 2;
                                let natural_seconds = num_frames as f64 / sample.sample_rate_hz as f64;
                                event.max_duration_beat = Some(natural_seconds * (bpm / 60.0));
                            }
                            if cell.effect == Some(SET_PANNING) {
                                event.pans.push(PanChange {
                                    at_beat: beat,
                                    pan: pan_from_param(cell.effect_param.unwrap_or(0)),
                                });
                            }
                            let list = notes_by_sample.get_mut(&active_sample).unwrap();
                            list.push(event);
                            channel_held[ch] = Some((active_sample, list.len() - 1));
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
                        .push(PanChange { at_beat: beat, pan: pan_from_param(cell.effect_param.unwrap_or(0)) });
                }
            }

            let Some((held_sample, held_idx)) = channel_held[ch] else { continue };

            if cell.effect == Some(VOLUME_SLIDE) && cell.effect_param.unwrap_or(0) != 0 && channel_current_volume[ch].is_some()
            {
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
                let param = cell.effect_param.unwrap();
                let up = (param >> 4) & 0x0F;
                let down = param & 0x0F;
                let delta_per_tick: i32 = if up != 0 { up as i32 } else { -(down as i32) };
                let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                for tick in 1..speed {
                    let previous_volume = channel_current_volume[ch].unwrap();
                    let new_volume = (previous_volume + delta_per_tick).clamp(0, 64);
                    channel_current_volume[ch] = Some(new_volume);
                    // once a slide clamps at 0 or 64, reapplying it every subsequent tick (or
                    // row, if the same effect keeps getting reapplied — very common once a
                    // channel has faded out) is a genuine no-op: skip it instead of emitting
                    // thousands of identical points that only bloat the automation lane and
                    // bury the part of the curve that actually moves.
                    if new_volume == previous_volume && !event.volumes.is_empty() {
                        continue;
                    }
                    let tick_beat = beat + tick as f64 * beats_per_tick;
                    // only the very first tick can possibly need a step (a jump from wherever
                    // the volume was before this row); every later tick in this same row is
                    // always a direct continuation of the one before.
                    let glide = if tick == 1 { was_sliding } else { true };
                    event.volumes.push(VolumeChange { at_beat: tick_beat, tracker_volume: new_volume, glide });
                }
                channel_sliding[ch] = true;
            } else if (cell.effect == Some(PORTAMENTO_UP) || cell.effect == Some(PORTAMENTO_DOWN))
                && cell.effect_param.unwrap_or(0) != 0
                && channel_period[ch].is_some()
            {
                let sign: f64 = if cell.effect == Some(PORTAMENTO_UP) { -1.0 } else { 1.0 }; // period down = pitch up
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
                let param = cell.effect_param.unwrap() as f64;
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
            } else if cell.effect == Some(TONE_PORTAMENTO)
                && channel_portamento_target[ch].is_some()
                && channel_period[ch].is_some()
            {
                // unlike 1xx/2xx, this slides *toward* a fixed target and stops there exactly
                // (ft2-clone's portamento(): moves by portamentoSpeed per tick, clamping at
                // portamentoTargetPeriod rather than sliding past it).
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

                if cell.effect == Some(VIBRATO_VOLSLIDE)
                    && cell.effect_param.unwrap_or(0) != 0
                    && channel_current_volume[ch].is_some()
                {
                    let param = cell.effect_param.unwrap();
                    let up = (param >> 4) & 0x0F;
                    let down = param & 0x0F;
                    let delta_per_tick: i32 = if up != 0 { up as i32 } else { -(down as i32) };
                    let event = &mut notes_by_sample.get_mut(&held_sample).unwrap()[held_idx];
                    for tick in 1..speed {
                        let previous_volume = channel_current_volume[ch].unwrap();
                        let new_volume = (previous_volume + delta_per_tick).clamp(0, 64);
                        channel_current_volume[ch] = Some(new_volume);
                        if new_volume == previous_volume && !event.volumes.is_empty() {
                            continue; // see the VOLUME_SLIDE branch above: skip a no-op repeat
                        }
                        let tick_beat = beat + tick as f64 * beats_per_tick;
                        let glide = if tick == 1 { was_sliding } else { true };
                        event.volumes.push(VolumeChange { at_beat: tick_beat, tracker_volume: new_volume, glide });
                    }
                    channel_sliding[ch] = true;
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
    // channel. A note held near its natural length can still be ringing when another
    // channel triggers the next one, masking the pitch change (and, in practice, Ableton
    // can drop/merge MidiNoteEvents that overlap on load). So clamp every note to end
    // before the next note on the same sample track starts, regardless of channel/pitch —
    // the merged track behaves as one monophonic instrument, same as it reads on the ear.
    for events in notes_by_sample.values_mut() {
        events.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());
        let n = events.len();
        for i in 0..n {
            let start_i = events[i].start_beat;
            // clamp to the next *strictly later* start time, skipping over any notes tied
            // at the same beat (two channels triggering this sample together) — those can't
            // be made non-overlapping with each other without silencing one, so leave them.
            for j in (i + 1)..n {
                if events[j].start_beat > start_i {
                    let gap = events[j].start_beat - start_i - NOTE_GAP_BEATS;
                    events[i].duration_beat = events[i].duration_beat.min(gap);
                    break;
                }
            }
        }
        for event in events.iter_mut() {
            if event.duration_beat <= 0.0 {
                event.duration_beat = NOTE_GAP_BEATS;
            }
        }
    }

    SongEvents { notes_by_sample, tempo_changes, total_beats: beat, segments }
}

//! Extracts each K051649/SCC channel's register-write log as MIDI note data plus the sequence
//! of distinct 32-sample waveforms it actually used, for driving an Ableton Wavetable
//! instrument instead of the bit-accurate chips::scc emulator — an approximation kept
//! *alongside* the WAV-rendered SCC tracks (see export::vgm_als), not a replacement, so the
//! two can be A/B compared inside the same project.
//!
//! A frequency-register change while a channel's key is held is either a new note or a pitch
//! *bend* of the current one, decided by nearest-semitone: if the new frequency still rounds
//! to the same MIDI note, it's absorbed as a bend point on the currently open span instead of
//! retriggering — this is what recovers both the exact chip pitch (rather than snapping to
//! the nearest equal-tempered semitone and losing the remainder) and any vibrato a game
//! implements by wobbling the frequency register around a note's center, without needing to
//! guess at genuine MIDI Pitch Bend automation's XML schema (no reference project has one to
//! copy — export::vgm_als automates Wavetable's own Pitch/Detune parameter instead, the same
//! already-proven mechanism export::als uses for tracker Portamento/Vibrato). Bends always
//! land within ±0.5 semitones by construction (that's the "same nearest semitone" test below),
//! matching Detune's own native range exactly. A change
//! that crosses into a *different* nearest semitone is a real new note and still retriggers.
//!
//! A channel's waveform *can* change while a note is held — some SCC compositions rewrite
//! waveform RAM mid-note specifically to fake an amplitude/timbre envelope the hardware has no
//! dedicated generator for — so every genuinely distinct waveform a channel uses (not just its
//! first) is captured as its own "frame", in the order first used. export::vgm_als concatenates
//! them into one multi-frame wavetable file and automates Wavetable's own Position parameter to
//! switch between them at the right times, which is how this gets its envelope-like motion back.
//!
//! A new note *or* a mid-note waveform change (but not an absorbed pitch bend) ends the
//! current "span" (pitch + waveform, held together) and starts a new one. Real rips often open
//! with a burst of driver-init key toggling faster than any real note (a few tens of
//! *microseconds* — observed directly in "a dream of dreamer": several sub-millisecond spans
//! before the first real ~1-beat note), each producing its own technically-valid but
//! practically inaudible span — including, critically, its own waveform-frame commit, which is
//! what made this export visibly *start on the second waveform* (the first frame was real but
//! gone before anyone could hear it). merge_short_spans folds any span under MIN_SPAN_SECONDS
//! into its neighbor before notes/frames/bends are ever built, rather than filtering separately
//! after the fact.
//!
//! VGM writes are processed in *groups* sharing the same `at_sample` (a driver commonly rewrites
//! several registers — e.g. a whole 32-byte waveform, or a key-on right after loading it — in
//! one zero-wait burst) rather than one write at a time: applying a whole group's raw register
//! writes first and only then evaluating key-on/off/span changes against the group's *final*
//! state avoids spurious partial-waveform snapshots mid-burst.

use crate::export::als::ClippedNote;
use crate::export::vgm_render::VGM_SAMPLE_RATE;
use crate::formats::vgm::{Chip, VgmFile};

pub(crate) const SCC_CHANNELS: usize = 5;

pub(crate) struct WaveformFrame {
    pub(crate) at_beat: f64,
    pub(crate) waveform: [i8; 32],
}

pub(crate) struct ChannelTrack {
    /// Chronological, deduplicated (no two consecutive entries are byte-identical) — always
    /// non-empty whenever `notes` is non-empty.
    pub(crate) frames: Vec<WaveformFrame>,
    pub(crate) notes: Vec<ClippedNote>,
    /// (at_beat, transpose_semitones, glide) — ready for export::als::step_points_with_glide.
    /// `glide=false` marks the first point of a span (a hard jump to its own tuning residual);
    /// `glide=true` marks a later in-span bend (fine pitch drift/vibrato), which should
    /// connect smoothly to the point before it instead of stepping.
    pub(crate) pitch_bends: Vec<(f64, f64, bool)>,
    /// (at_beat, gain 0..1, glide) — same shape/convention as `pitch_bends`, but tracking the
    /// volume register: many rips rewrite it every few hundred samples across a note's whole
    /// sustain to fake an amplitude envelope the chip has no generator for (the same trick
    /// waveform rewrites play for timbre) — sampling it once at note-on, as an earlier version
    /// did, caught an arbitrary point on that ramp and made note-to-note loudness look random.
    pub(crate) gains: Vec<(f64, f64, bool)>,
}

pub(crate) fn hz_to_midi_note(freq_hz: f64) -> i32 {
    (69.0 + 12.0 * (freq_hz / 440.0).log2()).round().clamp(0.0, 127.0) as i32
}

fn midi_note_freq(pitch: i32) -> f64 {
    440.0 * 2f64.powf((pitch as f64 - 69.0) / 12.0)
}

/// Semitone offset between an exact chip frequency and a MIDI note's own equal-tempered
/// frequency — what export::vgm_als automates onto Wavetable's Pitch/Detune parameter so the
/// note actually sounds at the chip's real pitch instead of snapping silently to the nearest
/// semitone.
fn transpose_semitones(freq_hz: f64, pitch: i32) -> f64 {
    12.0 * (freq_hz / midi_note_freq(pitch)).log2()
}

pub(crate) fn samples_to_beats(at_sample: u64, tempo_bpm: f64) -> f64 {
    at_sample as f64 / VGM_SAMPLE_RATE as f64 * tempo_bpm / 60.0
}

fn velocity_from_volume(vol: u8) -> i32 {
    (vol as i32 * 127 / 15).max(1)
}

/// Gain automation is driven directly by the 0-15 register (see ChannelTrack::gains), not by
/// round-tripping through the 0-127 MIDI velocity scale — one less unnecessary rounding step.
fn gain_from_volume(vol: u8) -> f64 {
    vol as f64 / 15.0
}

/// A one-group volume jump of at least this much (out of the register's 0-15 range) is a
/// re-attack, not a gentle swell — a natural attack ramp climbs one step per write (as
/// observed directly in real rips), so this comfortably clears that while still catching a
/// deliberate reset back to a high level.
const REARTICULATION_VOLUME_JUMP: u8 = 3;

/// A split-triggering change landing this close (in samples, ~4.5ms) after the currently open
/// span's own start is treated as still refining that same note's onset rather than as a new
/// note — comfortably above the few-sample gap a driver's own multi-register note-trigger
/// sequence writes in (traced directly on SCC-4), comfortably below any real gap between two
/// distinct notes (the shortest observed on that same channel was two orders of magnitude
/// larger).
const NOTE_ONSET_COALESCE_SAMPLES: u64 = 200;

/// Below this length, a span is almost certainly a driver-init artifact (real rips have shown
/// bursts of these under a tenth of a millisecond) rather than a note or an intentional
/// waveform change — see this module's own doc comment.
const MIN_SPAN_SECONDS: f64 = 0.015;

/// Many SCC compositions keep a channel's key held continuously across a whole phrase and
/// just rewrite its frequency register for each new note (true chip-style legato) rather than
/// toggling key-off/key-on between them — this project already turns each such change into a
/// separate retriggered span, but two *exactly* back-to-back notes (one ending the very
/// instant the next begins) gave Wavetable's envelope nothing to retrigger against: with no
/// gap to release through, the amplitude never actually dips between them, so distinct notes
/// were audible as pitch alone — no attack transient, easy to mistake for "no note changes at
/// all". Trimming a small gap before every note (capped at half its own length, so short/fast
/// passages don't lose notes entirely) forces a real release-then-attack cycle at every
/// boundary, matching how the ear expects a sequence of discrete notes to sound regardless of
/// whether the underlying chip data had a genuine key-off there.
const NOTE_GAP_SECONDS: f64 = 0.02;

fn apply_note_gaps(notes: &mut [ClippedNote], tempo_bpm: f64) {
    let gap_beats = NOTE_GAP_SECONDS * tempo_bpm / 60.0;
    for note in notes {
        note.duration_beat = (note.duration_beat - gap_beats).max(note.duration_beat * 0.5);
    }
}

#[derive(Clone)]
struct Span {
    start_sample: u64,
    end_sample: u64,
    pitch: i32,
    velocity: i32,
    waveform: [i8; 32],
    /// (at_sample, freq_hz), chronological, always has at least the span's opening frequency.
    bends: Vec<(u64, f64)>,
    /// (at_sample, volume 0-15), chronological, always has at least the span's opening volume.
    volumes: Vec<(u64, u8)>,
}

/// Folds every span shorter than MIN_SPAN_SECONDS into a neighbor (the previous span, or the
/// next one if there isn't a previous) instead of just dropping it, so no playback time is
/// silently lost — only the glitch span's own brief pitch/waveform/bend identity is.
fn merge_short_spans(mut spans: Vec<Span>) -> Vec<Span> {
    let min_samples = (MIN_SPAN_SECONDS * VGM_SAMPLE_RATE as f64) as u64;
    let mut i = 0;
    while spans.len() > 1 && i < spans.len() {
        if spans[i].end_sample - spans[i].start_sample >= min_samples {
            i += 1;
            continue;
        }
        if i > 0 {
            spans[i - 1].end_sample = spans[i].end_sample;
            spans.remove(i);
        } else {
            spans[1].start_sample = spans[0].start_sample;
            spans.remove(0);
        }
    }
    spans
}

struct OpenSpan {
    start_sample: u64,
    pitch: i32,
    velocity: i32,
    waveform: [i8; 32],
    bends: Vec<(u64, f64)>,
    volumes: Vec<(u64, u8)>,
}

struct ChannelState {
    waveform: [i8; 32],
    freq_reg: u16,
    /// The last frequency value pitch decisions were actually made against — distinct from
    /// `freq_reg` (the raw register state, which can transiently hold a nonsense value while
    /// a 12-bit frequency's low and high bytes are only half-written). Only advances once a
    /// decision has actually been evaluated (see freq_write_incoming_soon).
    settled_freq_reg: u16,
    volume: u8,
    key: bool,
    open: Option<OpenSpan>,
    spans: Vec<Span>,
}

impl ChannelState {
    fn new() -> Self {
        ChannelState { waveform: [0; 32], freq_reg: 0, settled_freq_reg: 0, volume: 0x0f, key: false, open: None, spans: Vec::new() }
    }

    fn close_span(&mut self, at_sample: u64) {
        if let Some(open) = self.open.take() {
            if at_sample > open.start_sample {
                self.spans.push(Span {
                    start_sample: open.start_sample,
                    end_sample: at_sample,
                    pitch: open.pitch,
                    velocity: open.velocity,
                    waveform: open.waveform,
                    bends: open.bends,
                    volumes: open.volumes,
                });
            }
        }
    }

    fn open_span(&mut self, at_sample: u64, clock: f64) {
        if self.freq_reg <= 8 {
            return; // matches chips::scc::Scc's own halt-below-9 behavior — inaudible, no note
        }
        // Every note-open is itself a settled pitch decision, regardless of which path led
        // here (key-on, halt recovery, or a coalesced re-onset) — keeps settled_freq_reg from
        // ever going stale relative to the note actually sounding.
        self.settled_freq_reg = self.freq_reg;
        let freq_hz = clock / (32.0 * (self.freq_reg as f64 + 1.0));
        let pitch = hz_to_midi_note(freq_hz);
        self.open = Some(OpenSpan {
            start_sample: at_sample,
            pitch,
            velocity: velocity_from_volume(self.volume),
            waveform: self.waveform,
            bends: vec![(at_sample, freq_hz)],
            volumes: vec![(at_sample, self.volume)],
        });
    }
}

/// True if channel `ch`'s frequency register gets touched again within NOTE_ONSET_COALESCE_SAMPLES
/// of `at_sample` — i.e. this write is very likely one half of a low/high byte pair whose other
/// half hasn't landed yet, not a settled value pitch decisions should be made against. `writes`
/// must be chronologically sorted (guaranteed by formats::vgm::parse) so scanning can stop as
/// soon as it runs past the window. Traced directly on SCC-4: the same low+high split that once
/// stranded a channel silent (see the recovery logic below) also corrupted vibrato throughout
/// the song — a lone low-byte write briefly produces an unrelated, valid-looking frequency,
/// which then reads as "jumped to a different note" and forces a hard retrigger instead of a
/// smooth bend, so a channel affected by this never got a single genuine in-note bend.
fn freq_write_incoming_soon(writes: &[crate::formats::vgm::RegisterWrite], from_idx: usize, ch: usize, at_sample: u64) -> bool {
    for w in &writes[from_idx..] {
        if w.at_sample > at_sample + NOTE_ONSET_COALESCE_SAMPLES {
            break;
        }
        if w.chip == Chip::Scc && w.port == 1 && (w.reg >> 1) as usize == ch {
            return true;
        }
    }
    false
}

/// Extracts all 5 SCC channels' note/bend data + waveform-frame timeline in a single pass over
/// the VGM's writes. A channel the song never actually keys on comes back with empty `notes` —
/// the caller (export::vgm_als) is responsible for skipping those, same convention
/// export::vgm_render::render_stems already uses for silent stems.
pub(crate) fn extract_channels(vgm: &VgmFile, tempo_bpm: f64) -> [ChannelTrack; SCC_CHANNELS] {
    let clock = vgm.scc_clock.max(1) as f64;
    let mut state: [ChannelState; SCC_CHANNELS] = std::array::from_fn(|_| ChannelState::new());

    let mut i = 0;
    while i < vgm.writes.len() {
        let at_sample = vgm.writes[i].at_sample;
        let mut pending_key_mask: Option<u8> = None;
        // Snapshot each channel's state *before* this group's writes, so a waveform change can
        // be told apart from a fresh key-on landing in the very same group (which already
        // opens its span at the post-group state — no separate split needed there). Frequency
        // doesn't need a group-start snapshot the same way — see `settled_freq_reg`.
        let was_playing: [bool; SCC_CHANNELS] = std::array::from_fn(|ch| state[ch].open.is_some());
        let waveform_before: [[i8; 32]; SCC_CHANNELS] = std::array::from_fn(|ch| state[ch].waveform);
        let volume_before: [u8; SCC_CHANNELS] = std::array::from_fn(|ch| state[ch].volume);

        let mut j = i;
        while j < vgm.writes.len() && vgm.writes[j].at_sample == at_sample {
            let w = &vgm.writes[j];
            if w.chip == Chip::Scc {
                match w.port {
                    0 => {
                        // Waveform RAM — mirrors chips::scc::Scc::write's own offset>=0x60
                        // handling (channel 4 shares channel 3's table on real SCC1 hardware).
                        let offset = w.reg as usize & 0x7f;
                        if offset >= 0x60 {
                            state[3].waveform[offset & 0x1f] = w.value as i8;
                            state[4].waveform[offset & 0x1f] = w.value as i8;
                        } else {
                            state[offset >> 5].waveform[offset & 0x1f] = w.value as i8;
                        }
                    }
                    1 => {
                        let ch = (w.reg >> 1) as usize;
                        if ch < SCC_CHANNELS {
                            let s = &mut state[ch];
                            s.freq_reg = if w.reg & 1 == 1 {
                                (s.freq_reg & 0x0ff) | ((w.value as u16) << 8 & 0xf00)
                            } else {
                                (s.freq_reg & 0xf00) | w.value as u16
                            };
                        }
                    }
                    2 => {
                        let ch = w.reg as usize;
                        if ch < SCC_CHANNELS {
                            state[ch].volume = w.value & 0x0f;
                        }
                    }
                    3 if w.reg == 0 => pending_key_mask = Some(w.value),
                    _ => {}
                }
            }
            j += 1;
        }

        // Key-on/off is resolved once per group, against the group's *final* register state —
        // a same-instant key-on/key-off pair nets to "no change" here instead of a phantom
        // zero-duration span.
        if let Some(mask) = pending_key_mask {
            for (ch, s) in state.iter_mut().enumerate() {
                let new_key = (mask >> ch) & 1 != 0;
                if new_key == s.key {
                    continue;
                }
                s.key = new_key;
                if new_key {
                    s.open_span(at_sample, clock);
                } else {
                    s.close_span(at_sample);
                }
            }
        }

        // A channel's frequency low/high byte sometimes arrive as two *separate* writes a few
        // samples apart (not in the same group) rather than together, so a lone byte can
        // transiently produce a nonsense combined value — see freq_write_incoming_soon. Any
        // pitch decision (recovery from a halt, or the bend-vs-retrigger call below) waits for
        // the pair to settle before comparing against `settled_freq_reg` rather than reacting
        // to every raw register write.
        let freq_frozen: [bool; SCC_CHANNELS] = std::array::from_fn(|ch| {
            state[ch].freq_reg != state[ch].settled_freq_reg && freq_write_incoming_soon(&vgm.writes, j, ch, at_sample)
        });

        // A channel's frequency register can also momentarily read <=8 while a low/high byte
        // pair is only half-updated — a real "chip halted" register value on real hardware,
        // correctly silencing the note (see open_span) when it's genuine. But once the pair
        // settles on a valid frequency while the channel is still logically keyed — traced
        // directly on SCC-4: this stranded a phrase silent for the rest of its ~5-second hold,
        // since nothing but an actual key-on normally reopens a span. Treat regaining a valid
        // *settled* frequency while still keyed as resuming that same held note.
        for (ch, s) in state.iter_mut().enumerate() {
            if freq_frozen[ch] {
                continue;
            }
            if s.key && s.open.is_none() && s.freq_reg > 8 && s.freq_reg != s.settled_freq_reg {
                s.open_span(at_sample, clock); // updates settled_freq_reg itself
            }
        }

        // A waveform rewrite always ends the current span (needed for a clean Position
        // automation boundary — see this module's doc comment). A frequency change only ends
        // it if it crosses into a *different* nearest semitone; staying within the currently
        // open note's own semitone is absorbed as a bend point (fine tuning / vibrato)
        // instead. A volume *re-attack* — a sharp jump back up after the register had already
        // been decaying — also ends it: some rips rearticulate a repeated note purely by
        // resetting the volume envelope back to an attack level, without ever touching
        // frequency or key state, which otherwise reads as one long note that quietly swells
        // back up mid-sustain instead of the separate repeated notes it actually is (a real
        // instance of exactly this was traced directly in "a dream of dreamer": volume ramping
        // 8→9→10→…→2 then jumping straight back to 8 with the key never once toggling).
        for (ch, s) in state.iter_mut().enumerate() {
            if !(was_playing[ch] && s.key) {
                continue;
            }
            let waveform_changed = s.waveform != waveform_before[ch];
            let freq_changed = !freq_frozen[ch] && s.freq_reg != s.settled_freq_reg;
            let volume_changed = s.volume != volume_before[ch];
            if !waveform_changed && !freq_changed && !volume_changed {
                continue;
            }

            let same_note_pitch = !freq_changed
                || (s.freq_reg > 8
                    && s.open.as_ref().is_some_and(|o| hz_to_midi_note(clock / (32.0 * (s.freq_reg as f64 + 1.0))) == o.pitch));
            let rearticulated =
                volume_changed && s.volume > volume_before[ch] && s.volume - volume_before[ch] >= REARTICULATION_VOLUME_JUMP;
            if freq_changed {
                s.settled_freq_reg = s.freq_reg;
            }

            if waveform_changed || !same_note_pitch || rearticulated {
                // Some rips write a note's fresh pitch and its fresh-attack volume as two
                // separate register writes a handful of samples apart — still one logical
                // note-onset event, not two. Closing and reopening for *each* trigger creates
                // a throwaway span between them, short enough that merge_short_spans then
                // swallows it (along with whichever of the two triggers it swallows the
                // pitch/velocity of) — traced directly on SCC-4: a pitch change immediately
                // followed by a volume re-attack 17 samples later. While the just-opened span
                // is still this fresh, refresh it in place (reusing its own original start
                // time) instead.
                let coalesce_start = s.open.as_ref().filter(|o| at_sample - o.start_sample < NOTE_ONSET_COALESCE_SAMPLES).map(|o| o.start_sample);
                match coalesce_start {
                    Some(start) => s.open_span(start, clock),
                    None => {
                        s.close_span(at_sample);
                        s.open_span(at_sample, clock);
                    }
                }
            } else {
                if freq_changed {
                    let freq_hz = clock / (32.0 * (s.freq_reg as f64 + 1.0));
                    if let Some(open) = &mut s.open {
                        open.bends.push((at_sample, freq_hz));
                    }
                }
                if volume_changed {
                    if let Some(open) = &mut s.open {
                        open.volumes.push((at_sample, s.volume));
                    }
                }
            }
        }

        i = j;
    }

    for s in &mut state {
        s.close_span(vgm.total_samples);
    }

    state.map(|s| {
        let spans = merge_short_spans(s.spans);

        let mut frames: Vec<WaveformFrame> = Vec::new();
        let mut notes: Vec<ClippedNote> = Vec::new();
        let mut pitch_bends: Vec<(f64, f64, bool)> = Vec::new();
        let mut gains: Vec<(f64, f64, bool)> = Vec::new();
        for span in &spans {
            if frames.last().is_none_or(|f: &WaveformFrame| f.waveform != span.waveform) {
                frames.push(WaveformFrame { at_beat: samples_to_beats(span.start_sample, tempo_bpm), waveform: span.waveform });
            }
            let start_beat = samples_to_beats(span.start_sample, tempo_bpm);
            let end_beat = samples_to_beats(span.end_sample, tempo_bpm);
            notes.push(ClippedNote { start_beat, duration_beat: end_beat - start_beat, velocity: span.velocity, pitch: span.pitch });

            for (idx, &(at_sample, freq_hz)) in span.bends.iter().enumerate() {
                let at_beat = samples_to_beats(at_sample, tempo_bpm);
                pitch_bends.push((at_beat, transpose_semitones(freq_hz, span.pitch), idx > 0));
            }
            for (idx, &(at_sample, vol)) in span.volumes.iter().enumerate() {
                let at_beat = samples_to_beats(at_sample, tempo_bpm);
                gains.push((at_beat, gain_from_volume(vol), idx > 0));
            }
        }
        apply_note_gaps(&mut notes, tempo_bpm);

        ChannelTrack { frames, notes, pitch_bends, gains }
    })
}

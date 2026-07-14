//! Builds an Ableton Live Set (.als) from a Module, using a real .als as a
//! template rather than generating the (undocumented, reverse-engineered) XML
//! schema from scratch. See formats::base and the project plan for the rationale.
//!
//! The template must contain at least one MidiTrack hosting a Sampler
//! (`MultiSampler`) device with a sample loaded, with its song content laid
//! out as a single clip in the *Arrangement* (not a Session clip slot). That
//! track is cloned once per non-empty sample in the module — or, when a sample
//! is triggered on several tracker channels whose notes overlap in time, once
//! per "voice" that sample needs (see the voice-assignment pass in
//! export::notes::compute_song_events); a sample needing more than one voice
//! has its tracks folded into a `GroupTrack` (see GROUP_TRACK_TEMPLATE_XML) so
//! they still read as one instrument in the track list. Every voice track of a
//! given sample shares one color (see color_index_for_sample), distinct from
//! other samples'. Each clone gets its own sample reference, root key, loop
//! points, and one Arrangement clip per pattern *play* (see
//! export::notes::Segment) that this voice actually has notes in — matching the
//! module's own pattern structure instead of one clip spanning the whole song.
//! Each clip carries its own MIDI notes (grouped by pitch, as Ableton's clip
//! format requires). A note whose sustain would naturally cross into the next
//! pattern is truncated at the clip boundary, since Ableton can't play a note
//! past its own clip's end.
//!
//! Pitch Bend/Volume/Panorama automation (from ProTracker Portamento, Tone
//! Portamento, Vibrato, Arpeggio, Volume Slide, Set Volume, Set Panning) is built
//! once per *track*, spanning the whole song, as a track-level Arrangement
//! automation envelope (`MidiTrack/AutomationEnvelopes/Envelopes/
//! AutomationEnvelope` — see build_automation_envelope) — not as a clip-local
//! `ClipEnvelope` nested inside each clip. That other mechanism is valid,
//! well-formed XML and *looks* like the obvious place for it, but Ableton
//! doesn't read it for an Arrangement-view clip; this was only found by
//! comparing against a real user-drawn reference project.
//!
//! Volume automation specifically targets the track's own Mixer/Volume fader
//! (`DeviceChain/Mixer/Volume/AutomationTarget`), not the Sampler device's own
//! Volume knob (`MultiSampler/VolumeAndPan/Volume`) — that knob only swings
//! ±36dB and can never reach true silence, so a note faded all the way down was
//! still clearly audible. The Mixer fader goes down to a practical -70dB floor
//! (same range Ableton's own fader UI uses), confirmed against a second
//! user-drawn reference project automating this exact parameter. Its
//! FloatEvents store linear gain, not dB (see volume_to_gain) — unlike every
//! other automated parameter here, which are all plain dB or normalized values.
//!
//! Panorama automation's baseline is controlled by `AmigaPanning` (see enum below): `None`
//! (the default) centers every note unless it uses Set Panning (8xx); `Light`/`Medium`/`Full`
//! instead give every note a baseline matching its own channel's hardwired Amiga/Atari 4-
//! channel Left/Right/Right/Left routing, scaled to 25%/50%/100% stereo separation — real
//! 4-channel hardware wires channels 0 and 3 left and 1 and 2 right, with no in-between mix,
//! but most listeners find that jarring on headphones, hence the softer presets.
//!
//! Every `Id` attribute that Live treats as a global identity is renumbered from
//! one shared counter when a track is cloned — duplicating those across clones
//! is what makes Live report the project as damaged ("Pointee ID non uniques").
//! There is no fixed, documented list of which tags those are: besides the
//! obvious `AutomationTarget`/`ModulationTarget`/`Pointee`, a real Sampler device
//! also carries many specialized ones (`VolumeModulationTarget`,
//! `ControllerTargets.0`..`.130`, etc). Empirically, every locally-scoped Id in
//! this schema (`KeyTrack`, `MultiSamplePart`, `ClipSlot`, `WarpMarker`, ...) is
//! a small index (well under 1000), while every globally-scoped one is a large
//! number allocated from the same counter as `NextPointeeId` (tens of thousands
//! here) — so any `Id` at or above `LOCAL_ID_THRESHOLD` is renumbered,
//! regardless of its tag name. References to those ids (`<PointeeId Value="..."
//! />`, used by automation envelopes to target a specific device parameter) are
//! tracked and rewritten to match.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::Path;

use xmltree::{Element, XMLNode};

use crate::export::notes::{compute_song_events, NoteEvent, Segment};
use crate::export::wav::{sample_wav_filename, write_sample_wav};
use crate::formats::base::{Module, Sample};
use crate::xmlutil;

const LOCAL_ID_THRESHOLD: i64 = 1000;
const AUTOMATION_SENTINEL_TIME: &str = "-63072000"; // Ableton's "value before the automation starts" marker
// The Sampler device's own Volume knob only swings ±36dB and can never reach true silence — a
// note faded all the way to 0 (Volume Slide clamped at the floor, a quiet Cxx, etc.) was still
// clearly audible. The *track's* Mixer/Volume fader (DeviceChain/Mixer/Volume) goes down to a
// practical -70dB floor, same range Ableton's own fader UI uses (confirmed against a real
// user-drawn reference project automating this exact parameter) — automation now targets that
// instead, leaving the Sampler's own Volume knob at a fixed unity gain.
const TRACK_FADER_MIN_DB: f64 = -70.0; // matches Mixer/Volume's own MidiControllerRange floor (~0.000316 linear)
const TRACK_FADER_MAX_DB: f64 = 6.0; // matches Mixer/Volume's own MidiControllerRange ceiling (~1.995 linear)
pub const TRACK_VOLUME_DB: f64 = -12.0; // headroom baseline for every generated track, so simultaneous notes don't sum above 0dB
const TEMPO_STEP_EPSILON_BEATS: f64 = 0.001; // forces a step instead of a ramp between tempo automation points
// Live's standard track-color palette (confirmed against a real reference project — every
// `<Color Value="N" />` seen there falls in this range) has 70 entries, index 0-69. Not every
// index maps to a visually distinct hue (Live repeats some across the palette), but cycling
// through them by sample index at least keeps a given sample's own voice tracks identically
// colored and separates *different* samples' tracks in the common case.
const COLOR_PALETTE_SIZE: i32 = 70;
// Ableton's Sampler device silently refuses to loop a region shorter than this many frames —
// confirmed against a real short-loop sample (PFANTAS1.MOD instrument 10, a 32-frame loop)
// that played back as a one-shot with no looping in Ableton despite SustainLoop/Mode being
// set correctly.
const MIN_SAMPLER_LOOP_FRAMES: u32 = 48;

/// The .als bundled with ablemod itself, used when the caller doesn't supply one.
pub fn default_template_bytes() -> &'static [u8] {
    include_bytes!("../../templates/default.als")
}

/// A standalone `<GroupTrack>` element, captured from a real Ableton Live 12 project (two
/// MidiTracks folded into one group) rather than hand-written — this schema (Mixer routing,
/// FreezeSequencer, modulation targets, ...) is undocumented and, like the rest of this
/// exporter's template-based approach, not worth risking a guess at. Cloned once per sample
/// that ends up needing more than one voice track (see the voice-assignment pass in
/// export::notes::compute_song_events) to visually fold that sample's tracks together.
const GROUP_TRACK_TEMPLATE_XML: &str = include_str!("../../templates/group_track.xml");

fn color_index_for_sample(sample: &Sample) -> i32 {
    (sample.index as i32 - 1).rem_euclid(COLOR_PALETTE_SIZE)
}

/// If `sample` loops but the loop is shorter than Ableton's Sampler can actually loop
/// (MIN_SAMPLER_LOOP_FRAMES), physically repeats the loop's own audio enough times to reach
/// that minimum and returns a new Sample with the extended PCM data and loop length —
/// otherwise returns `sample` unchanged. Repeating whole cycles of the loop keeps the result
/// audibly identical to what real tracker playback would have produced by looping the short
/// segment that many times; anything past the original loop end is dropped, since it's
/// unreachable once a real player enters the loop anyway.
fn ensure_loopable(sample: &Sample) -> std::borrow::Cow<'_, Sample> {
    if !sample.has_loop() || sample.loop_length >= MIN_SAMPLER_LOOP_FRAMES {
        return std::borrow::Cow::Borrowed(sample);
    }

    let repeats = MIN_SAMPLER_LOOP_FRAMES.div_ceil(sample.loop_length);
    let loop_start_bytes = (sample.loop_start * 2) as usize;
    let loop_length_bytes = (sample.loop_length * 2) as usize;
    let loop_segment = sample.pcm16[loop_start_bytes..loop_start_bytes + loop_length_bytes].to_vec();

    let mut pcm16 = sample.pcm16[..loop_start_bytes].to_vec(); // attack portion, unchanged
    for _ in 0..repeats {
        pcm16.extend_from_slice(&loop_segment);
    }

    std::borrow::Cow::Owned(Sample { pcm16, loop_length: sample.loop_length * repeats, ..sample.clone() })
}

fn volume_to_db(tracker_volume: i32) -> f64 {
    // dB implied by an absolute 0-64 tracker volume value, referenced to full scale (64) —
    // unclamped (-inf at 0); the caller clamps the combined value to the fader's actual range.
    // Using the same fixed reference for every note (rather than each note's own trigger
    // volume) keeps the automation's sensitivity consistent: a quiet note (e.g. trigger
    // volume 5) that gets quieter still (down to 1) shouldn't swing by ~14dB just because its
    // *own* starting point was already quiet — it should barely move, the same as an
    // already-loud note nudged down by the same absolute amount.
    if tracker_volume <= 0 {
        return f64::NEG_INFINITY;
    }
    20.0 * (tracker_volume as f64 / 64.0).log10()
}

pub fn volume_to_gain(tracker_volume: i32) -> f64 {
    // Converts a 0-64 tracker volume (relative to TRACK_VOLUME_DB headroom) into the linear
    // gain value the Mixer/Volume parameter's FloatEvents actually store — clamped to that
    // fader's own real range, not an arbitrary device limit, so 0 lands at genuine
    // near-silence (~-70dB) instead of stopping at -36dB and staying audible.
    let db = (TRACK_VOLUME_DB + volume_to_db(tracker_volume)).clamp(TRACK_FADER_MIN_DB, TRACK_FADER_MAX_DB);
    10f64.powf(db / 20.0)
}

/// Whether/how much every note's Panorama automation defaults to the classic Amiga/Atari
/// hardwired 4-channel stereo routing (channels 0 and 3 wired left, 1 and 2 wired right,
/// repeating every 4 channels for wider .mod variants) instead of staying centered. Real
/// hardware only ever does `Full` (total separation, no in-between mix); `Light`/`Medium`
/// scale that same routing down to a softer stereo width for listeners who find the
/// authentic hard-panned sound too extreme, especially on headphones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum AmigaPanning {
    #[default]
    None,
    Light,
    Medium,
    Full,
}

impl AmigaPanning {
    /// Fraction of full (±1.0) hardwired stereo separation this preset applies.
    fn intensity(self) -> f64 {
        match self {
            AmigaPanning::None => 0.0,
            AmigaPanning::Light => 0.25,
            AmigaPanning::Medium => 0.5,
            AmigaPanning::Full => 1.0,
        }
    }
}

fn amiga_pan(channel: usize, intensity: f64) -> f64 {
    let m = channel % 4;
    let sign = if m == 0 || m == 3 { -1.0 } else { 1.0 };
    sign * intensity
}

fn is_all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

struct IdCounter(i64);
impl IdCounter {
    fn next(&mut self) -> i64 {
        let v = self.0;
        self.0 += 1;
        v
    }
}

/// Renumbers every global Id *inside* `track` — but not `track`'s own top-level Id, which
/// every caller already assigns explicitly (from the same shared counter) right before
/// calling this. Re-touching it here was harmless as long as nothing else referenced a
/// track's own Id (true for a plain MidiTrack) — but a GroupTrack's Id *is* referenced
/// (its member tracks' TrackGroupId points to it), so silently reassigning it out from under
/// the caller after they've already captured/returned it breaks that reference.
fn renumber_global_ids(track: &mut Element, id_counter: &mut IdCounter) {
    let mut id_map: HashMap<String, String> = HashMap::new();
    for child in &mut track.children {
        let XMLNode::Element(child) = child else { continue };
        xmlutil::visit_mut(child, &mut |node| {
            if let Some(value) = node.attributes.get("Id").cloned() {
                if is_all_digits(&value) && value.parse::<i64>().map(|v| v >= LOCAL_ID_THRESHOLD).unwrap_or(false) {
                    let new_value = id_counter.next().to_string();
                    id_map.insert(value, new_value.clone());
                    node.attributes.insert("Id".to_string(), new_value);
                }
            }
        });
    }
    xmlutil::visit_mut(track, &mut |node| {
        if node.name == "PointeeId" {
            if let Some(old_value) = node.attributes.get("Value").cloned() {
                if let Some(new_value) = id_map.get(&old_value) {
                    node.attributes.insert("Value".to_string(), new_value.clone());
                }
            }
        }
    });
}

fn fmt_number(x: f64) -> String {
    let rounded = (x * 1_000_000.0).round() / 1_000_000.0; // avoids float-accumulation noise
    if rounded.fract() == 0.0 && rounded.is_finite() {
        return format!("{}", rounded as i64);
    }
    let s = format!("{rounded:.6}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

fn max_id(root: &Element) -> i64 {
    let mut result: i64 = 0;
    for node in xmlutil::iter_elements(root) {
        if let Some(value) = node.attributes.get("Id") {
            let stripped = value.trim_start_matches('-');
            if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(v) = value.parse::<i64>() {
                    result = result.max(v);
                }
            }
        }
    }
    result
}

fn find_sampler_track_template(tracks_el: &Element) -> Result<&Element, String> {
    for track in xmlutil::find_all_children(tracks_el, "MidiTrack") {
        if xmlutil::find(track, ".//MultiSampler").is_none() {
            continue;
        }
        if xmlutil::find(track, ".//ClipTimeable/ArrangerAutomation/Events/MidiClip").is_none() {
            continue;
        }
        return Ok(track);
    }
    Err("The template .als has no MidiTrack with both a Sampler (MultiSampler) device holding a sample \
         and its content laid out as a clip in the Arrangement (not a Session clip slot) — load a sample \
         into a Sampler on a MIDI track, lay out a clip in Arrangement view, and re-save the template project."
        .to_string())
}

/// Every note gets a bracketing point at its *own* trigger volume — not just notes that
/// happen to carry a Volume Slide/Set Volume effect. The Sampler's velocity response is off
/// (VolumeVelScale left at 0, see build_track), so a note's MIDI velocity has no audible
/// effect on its own — without this, a quiet cell.volume/sample.volume note with no volume
/// *effect* would still play at the flat track baseline as if it were at full volume,
/// drowning out passages that should sit further back in the mix. Each point carries the
/// VolumeChange's `glide` flag through to step_points_with_glide. Values are linear gain
/// (see volume_to_gain), not dB — that's what the Mixer/Volume parameter's FloatEvents
/// actually store.
fn collect_volume_points(notes: &[NoteEvent]) -> Vec<(f64, f64, bool)> {
    let mut points = Vec::new();
    for note in notes {
        let baseline = volume_to_gain(note.trigger_volume);
        points.push((note.start_beat, baseline, false));
        for v in &note.volumes {
            points.push((v.at_beat, volume_to_gain(v.tracker_volume), v.glide));
        }
        points.push((note.start_beat + note.duration_beat, baseline, false));
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    points
}

/// Panorama points: every note's own bracket baseline is either centered (AmigaPanning::None)
/// or that note's hardwired-channel pan scaled by the preset's intensity (see amiga_pan) —
/// with Set Panning (8xx) events overriding on top, same as any other effect. When the
/// baseline is flat center *and* a note has no 8xx of its own, it's skipped entirely (matching
/// collect_bend_points) rather than emitting a redundant flat-center envelope for every note;
/// once hardwired panning is on, every note needs its own bracket since the baseline itself
/// carries real information (which channel it's on).
fn collect_pan_points(notes: &[NoteEvent], amiga_panning: AmigaPanning) -> Vec<(f64, f64)> {
    let intensity = amiga_panning.intensity();
    let mut points = Vec::new();
    for note in notes {
        let baseline = amiga_pan(note.channel, intensity);
        if note.pans.is_empty() && intensity == 0.0 {
            continue; // no baseline panning and no Set Panning (8xx) — nothing to automate
        }
        points.push((note.start_beat, baseline));
        for p in &note.pans {
            points.push((p.at_beat, p.pan));
        }
        points.push((note.start_beat + note.duration_beat, baseline));
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    points
}

/// Like collect_volume_points, specialized for Pitch Bend (Transpose): the baseline is
/// always 0 (a note's own fixed pitch, unaffected by any bending effect), and each point
/// carries the PitchBend's `glide` flag through to step_points_with_glide — Portamento/
/// Tone Portamento/Vibrato ticks glide smoothly, while Arpeggio's discrete jumps always step.
fn collect_bend_points(notes: &[NoteEvent]) -> Vec<(f64, f64, bool)> {
    let mut points = Vec::new();
    for note in notes {
        if note.bends.is_empty() {
            continue;
        }
        points.push((note.start_beat, 0.0, false));
        for b in &note.bends {
            points.push((b.at_beat, b.semitones, b.glide));
        }
        points.push((note.start_beat + note.duration_beat, 0.0, false));
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    points
}

/// Insert a hold-point (at the *previous* value) just before every value change, so
/// Ableton's default linear interpolation between automation points reads as a clean step
/// instead of a ramp gliding across the whole gap. Appropriate for effects that are instant
/// jumps in the tracker (Set Volume, Set Panning) — not for genuinely continuous ones like
/// Portamento, which should stay a smooth glide.
fn step_points(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if points.is_empty() {
        return Vec::new();
    }
    let epsilon = TEMPO_STEP_EPSILON_BEATS;
    let mut stepped = vec![points[0]];
    for w in points.windows(2) {
        let (_prev_beat, prev_value) = w[0];
        let (beat, value) = w[1];
        if value != prev_value {
            stepped.push((beat - epsilon, prev_value));
        }
        stepped.push((beat, value));
    }
    stepped
}

/// Like step_points, but skips the hold-before-jump for a point marked `glide=True` — those
/// represent one tick of an already-continuous ramp (an ongoing Volume Slide) and must
/// connect smoothly to the previous point, not read as a discrete step. Points marked
/// `glide=False` (Set Volume, a slide's own first tick, or a bracket point) still get the
/// step treatment, same as step_points.
fn step_points_with_glide(points: &[(f64, f64, bool)]) -> Vec<(f64, f64)> {
    if points.is_empty() {
        return Vec::new();
    }
    let epsilon = TEMPO_STEP_EPSILON_BEATS;
    let mut stepped = vec![(points[0].0, points[0].1)];
    for w in points.windows(2) {
        let (_prev_beat, prev_value, _prev_glide) = w[0];
        let (beat, value, glide) = w[1];
        if value != prev_value && !glide {
            stepped.push((beat - epsilon, prev_value));
        }
        stepped.push((beat, value));
    }
    stepped
}

fn new_element(name: &str, attrs: &[(&str, &str)]) -> Element {
    let mut el = Element::new(name);
    for (k, v) in attrs {
        el.attributes.insert((*k).to_string(), (*v).to_string());
    }
    el
}

fn append_child(parent: &mut Element, child: Element) {
    parent.children.push(XMLNode::Element(child));
}

/// Builds a *track-level* Arrangement automation envelope
/// (MidiTrack/AutomationEnvelopes/Envelopes/AutomationEnvelope) — the mechanism Ableton
/// actually reads for a device parameter automated in Arrangement view (same mechanism
/// already used for the Main Track's Tempo automation, see export_als). A `ClipEnvelope`
/// nested inside a MidiClip (the format's *other* envelope mechanism, meant for Session-view
/// clips) parses as valid, well-formed XML but Ableton silently ignores it for an Arrangement
/// clip — confirmed against a real user-drawn reference project whose working automation
/// lived here, with an empty ClipEnvelope list inside its clip.
fn build_automation_envelope(
    envelopes_el: &mut Element, points: &[(f64, f64)], pointee_id: &str, id_counter: &mut IdCounter, baseline: f64,
) {
    if points.is_empty() {
        return;
    }

    let mut automation_envelope = new_element("AutomationEnvelope", &[("Id", &id_counter.next().to_string())]);

    let mut target = Element::new("EnvelopeTarget");
    append_child(&mut target, new_element("PointeeId", &[("Value", pointee_id)]));
    append_child(&mut automation_envelope, target);

    let mut automation = Element::new("Automation");
    let mut float_events = Element::new("Events");
    append_child(
        &mut float_events,
        new_element("FloatEvent", &[
            ("Id", &id_counter.next().to_string()),
            ("Time", AUTOMATION_SENTINEL_TIME),
            ("Value", &fmt_number(baseline)),
        ]),
    );
    for (at_beat, value) in points {
        append_child(
            &mut float_events,
            new_element("FloatEvent", &[
                ("Id", &id_counter.next().to_string()),
                ("Time", &fmt_number(*at_beat)),
                ("Value", &fmt_number(*value)),
            ]),
        );
    }
    append_child(&mut automation, float_events);

    let mut transform_view = Element::new("AutomationTransformViewState");
    append_child(&mut transform_view, new_element("IsTransformPending", &[("Value", "false")]));
    append_child(&mut transform_view, Element::new("TimeAndValueTransforms"));
    append_child(&mut automation, transform_view);

    append_child(&mut automation_envelope, automation);
    append_child(envelopes_el, automation_envelope);
}

/// Assigns each note to the segment its start_beat falls in (by index into `segments`). Both
/// lists are already sorted ascending by beat, and segments are contiguous with no gaps (see
/// Segment's docs), so a single forward-advancing pointer suffices.
fn group_notes_by_segment<'a>(events: &'a [NoteEvent], segments: &[Segment]) -> BTreeMap<usize, Vec<&'a NoteEvent>> {
    let mut grouped: BTreeMap<usize, Vec<&NoteEvent>> = BTreeMap::new();
    let mut seg_index = 0usize;
    for note in events {
        while seg_index + 1 < segments.len() && note.start_beat >= segments[seg_index].end_beat {
            seg_index += 1;
        }
        grouped.entry(seg_index).or_default().push(note);
    }
    grouped
}

struct ClippedNote {
    start_beat: f64,
    duration_beat: f64,
    velocity: i32,
    pitch: i32,
}

/// Truncates a note's duration at the segment's end — a note that naturally sustains into
/// the next pattern can't keep sounding past its own Arrangement clip in Ableton, so it's cut
/// there instead. Note times inside a MidiClip are in the *same absolute* arrangement-time
/// coordinates as CurrentStart/CurrentEnd (see build_clip) — not clip-relative — so the start
/// time is left untouched here, unlike CurrentStart/CurrentEnd/Loop which do need the
/// segment's absolute bounds. bends/volumes/pans aren't copied: Pitch Bend/Volume/Panorama
/// automation is now built once per *track* from the untruncated note list (see build_track),
/// not per clip, so this truncated copy only needs to feed the MidiNoteEvent.
fn clip_note_to_segment(note: &NoteEvent, segment: &Segment) -> ClippedNote {
    let duration = note.duration_beat.min(segment.end_beat - note.start_beat);
    ClippedNote { start_beat: note.start_beat, duration_beat: duration, velocity: note.velocity, pitch: note.pitch }
}

fn build_clip(
    clip_template: &Element, display_name: &str, segment: &Segment, local_notes: &[ClippedNote], seg_index: usize,
    color_index: i32,
) -> Element {
    let mut clip = clip_template.clone();
    clip.attributes.insert("Id".to_string(), seg_index.to_string());
    clip.attributes.insert("Time".to_string(), fmt_number(segment.start_beat));
    xmlutil::set_value(&mut clip, "./Name", display_name);
    xmlutil::set_value(&mut clip, "./Color", &color_index.to_string()); // matches this sample's own track color
    // CurrentStart/CurrentEnd and the Loop bracket are in *absolute* arrangement time
    // (matching the Time attribute above) — only the notes/envelopes inside a clip are
    // clip-relative. Leaving these at the template's default (always 0..length) would place
    // every clip's actual footprint at the start of the arrangement regardless of its Time,
    // so all of a track's clips would overlap there and Ableton — which doesn't allow
    // overlapping clips on one track — silently keeps only the first and drops the rest.
    let start = fmt_number(segment.start_beat);
    let end = fmt_number(segment.end_beat);
    for path in ["./CurrentStart", "./Loop/LoopStart", "./Loop/HiddenLoopStart"] {
        xmlutil::set_value(&mut clip, path, &start);
    }
    for path in ["./CurrentEnd", "./Loop/LoopEnd", "./Loop/OutMarker", "./Loop/HiddenLoopEnd"] {
        xmlutil::set_value(&mut clip, path, &end);
    }

    let mut notes_by_pitch: BTreeMap<i32, Vec<&ClippedNote>> = BTreeMap::new();
    for note in local_notes {
        notes_by_pitch.entry(note.pitch).or_default().push(note);
    }

    {
        let key_tracks = xmlutil::find_mut(&mut clip, "./Notes/KeyTracks").expect("expected element \"./Notes/KeyTracks\" not found in template track");
        key_tracks.children.clear();

        let mut note_id_counter = 1u32;
        for (key_track_id, (pitch, notes)) in notes_by_pitch.iter().enumerate() {
            let mut key_track = new_element("KeyTrack", &[("Id", &key_track_id.to_string())]);
            let mut notes_el = Element::new("Notes");
            for note in notes {
                append_child(
                    &mut notes_el,
                    new_element("MidiNoteEvent", &[
                        ("Time", &fmt_number(note.start_beat)),
                        ("Duration", &fmt_number(note.duration_beat)),
                        ("Velocity", &note.velocity.to_string()),
                        ("OffVelocity", "64"),
                        ("NoteId", &note_id_counter.to_string()),
                    ]),
                );
                note_id_counter += 1;
            }
            append_child(&mut key_track, notes_el);
            append_child(&mut key_track, new_element("MidiKey", &[("Value", &pitch.to_string())]));
            append_child(key_tracks, key_track);
        }
    }

    let next_note_id: usize = notes_by_pitch.values().map(|v| v.len()).sum::<usize>() + 1;
    xmlutil::set_value(&mut clip, "./Notes/NoteIdGenerator/NextId", &next_note_id.to_string());

    // Clip-local envelopes (ClipEnvelope, nested here under Envelopes/Envelopes) are the
    // Session-view mechanism and Ableton doesn't read them for an Arrangement clip — left
    // empty on purpose. See build_automation_envelope for where Pitch Bend/Volume/Panorama
    // automation actually goes (once per track, not per clip).
    let envelopes_outer = xmlutil::find_mut(&mut clip, "./Envelopes/Envelopes").expect("expected element \"./Envelopes/Envelopes\" not found in template track");
    envelopes_outer.children.clear();

    clip
}

fn build_track(
    template_track: &Element, sample: &Sample, id_counter: &mut IdCounter, wav_path: &Path, events: &[NoteEvent],
    segments: &[Segment], amiga_panning: AmigaPanning, voice_label: Option<usize>,
) -> Element {
    let mut track = template_track.clone();
    let track_id = id_counter.next().to_string();
    track.attributes.insert("Id".to_string(), track_id);
    renumber_global_ids(&mut track, id_counter);

    xmlutil::set_value(&mut track, "./TrackUnfolded", "false"); // folded/minimized by default

    // The template track carries whatever Arm state its own project happened to be saved
    // with (ours has it armed, since that's a common state to leave a track in while
    // building/testing) — cloning it as-is would arm *every* exported track at once. Force it
    // off here; export_als re-arms exactly the first track of the whole project afterwards.
    xmlutil::set_value(&mut track, "./DeviceChain/MainSequencer/Recorder/IsArmed", "false");

    let mut display_name = format!("{:02} {}", sample.index, sample.name).trim().to_string();
    // Same sample triggered on several tracker channels with overlapping timing: this
    // voice's notes got split onto their own track (see the voice-assignment pass in
    // export::notes) to avoid colliding with the sample's other voice(s) — label it so the
    // duplicate tracks read as intentional rather than a bug.
    if let Some(voice_number) = voice_label {
        display_name = format!("{display_name} ({voice_number})");
    }
    xmlutil::set_value(&mut track, "./Name/EffectiveName", &display_name);
    // EffectiveName alone is just a cached, freely-regenerable display value — confirmed by
    // opening a generated project in real Ableton Live and re-saving it untouched: every track
    // we hadn't also set UserName on came back with its name mangled (Live prepended its own
    // track-position number, e.g. "01 bsnare" became "2-01 bsnare"). A real user-driven rename
    // in the Live UI writes the *same* text to both fields — UserName is what marks a name as
    // deliberately set rather than auto-computed, so set it here too to make ours stick.
    xmlutil::set_value(&mut track, "./Name/UserName", &display_name);

    // One color per *sample*, shared by all of its voice tracks — makes a sample's tracks
    // (whether split by voice or, once grouped, folded into one group) visually identifiable
    // at a glance instead of all inheriting the template's own single color.
    xmlutil::set_value(&mut track, "./Color", &color_index_for_sample(sample).to_string());

    {
        let sampler = xmlutil::find_mut(&mut track, ".//MultiSampler").expect("expected element \".//MultiSampler\" not found in template track");
        xmlutil::set_value(sampler, "./UserName", &display_name);

        // Reset any pitch transpose baked into the template's device — with RootKey ==
        // sample.base_note, the extracted sample should play back untransposed. Portamento
        // bends this same knob via a track-level automation envelope built below, not by
        // touching this baseline. TransposeKey carries *two* pointee registries —
        // ModulationTarget (the device's internal modulation matrix) and AutomationTarget
        // (track/clip automation, the same registry Volume/Panorama below and the Main
        // Track's own Tempo automation use). Pointing the envelope at ModulationTarget
        // produced XML that looked correct and imported without error, but Ableton silently
        // never applied it — confirmed by generating a real PINBALLF export and finding the
        // Transpose automation data present in the file yet inaudible/invisible in Ableton,
        // exactly like the earlier ClipEnvelope-vs-AutomationEnvelope mixup one level up.
        xmlutil::set_value(sampler, "./Pitch/TransposeKey/Manual", "0");
        xmlutil::set_value(sampler, "./Pitch/TransposeFine/Manual", "0");
    }
    let transpose_pointee_id = xmlutil::find(&track, ".//MultiSampler/Pitch/TransposeKey/AutomationTarget")
        .expect("expected element \"AutomationTarget\" not found in template track")
        .attributes
        .get("Id")
        .expect("expected Id attribute")
        .clone();

    // Volume Slide/Set Volume (Cxx/Axy) automation targets the *track's* own Mixer/Volume
    // fader, not the Sampler device's Volume knob — that knob only swings ±36dB and can
    // never reach true silence, so a note faded all the way down was still clearly audible.
    // The track fader goes down to a practical -70dB floor (confirmed against a real
    // reference project automating this exact parameter), so leave the Sampler's own Volume
    // knob at unity gain and automate the Mixer instead.
    xmlutil::set_value(
        xmlutil::find_mut(&mut track, ".//MultiSampler").unwrap(), "./VolumeAndPan/Volume/Manual", "0",
    );
    {
        let mixer = xmlutil::find_mut(&mut track, "./DeviceChain/Mixer").expect("expected element \"./DeviceChain/Mixer\" not found in template track");
        xmlutil::set_value(mixer, "./Volume/Manual", &fmt_number(volume_to_gain(64)));
    }
    let volume_pointee_id = xmlutil::find(&track, "./DeviceChain/Mixer/Volume/AutomationTarget")
        .expect("expected element \"AutomationTarget\" not found in template track")
        .attributes
        .get("Id")
        .expect("expected Id attribute")
        .clone();

    // Same reset-then-automate approach for Set Panning (8xx): baseline the knob, and let
    // the automation envelope built below carry any actual mid-note changes relative to that
    // baseline.
    {
        let sampler = xmlutil::find_mut(&mut track, ".//MultiSampler").unwrap();
        xmlutil::set_value(sampler, "./VolumeAndPan/Panorama/Manual", "0");
    }
    let pan_pointee_id = xmlutil::find(&track, ".//MultiSampler/VolumeAndPan/Panorama/AutomationTarget")
        .expect("expected element \"AutomationTarget\" not found in template track")
        .attributes
        .get("Id")
        .expect("expected Id attribute")
        .clone();

    {
        let sampler = xmlutil::find_mut(&mut track, ".//MultiSampler").unwrap();
        // Player/LoopModulators/LoopOn is NOT the loop enable switch — verified against a
        // user-provided reference project with a correctly-looping sample where
        // LoopOn.Manual was false. The real on/off switch is SustainLoop/Mode (1 = looping,
        // 0 = one-shot), set below alongside the loop's Start/End points.
        xmlutil::set_value(sampler, "./Player/LoopModulators/SampleLength/Manual", "1");
        xmlutil::set_value(sampler, "./Player/Snap/Manual", "false");

        // Glide ("PortamentoMode") defaults to 2 = Auto, which slides pitch between any two
        // overlapping/legato notes instead of retriggering cleanly. Off (0) for a
        // tracker-accurate discrete retrigger — the automation envelope built below handles
        // real Portamento-effect slides explicitly instead.
        xmlutil::set_value(sampler, "./Globals/PortamentoMode/Manual", "0");
        xmlutil::set_value(sampler, "./Filter/IsOn/Manual", "false");
        xmlutil::set_value(sampler, "./VolumeAndPan/Envelope/DecayTime/Manual", "1");
        xmlutil::set_value(sampler, "./VolumeAndPan/Envelope/ReleaseTime/Manual", "1");
    }

    let num_frames = sample.pcm16.len() / 2;
    let last_frame = num_frames - 1; // SampleEnd is the last valid frame *index*, not a frame count
    let loop_start = if sample.has_loop() { sample.loop_start } else { 0 };
    let loop_end = if sample.has_loop() { sample.loop_start + sample.loop_length - 1 } else { last_frame as u32 };

    // Sample Offset (9xx): every note sharing this voice necessarily has the same
    // sample_offset_frames (see the voice-assignment pass in export::notes — notes needing a
    // different offset can never share a voice), so the whole voice's track can use a single
    // SampleStart. Loop points stay at their normal absolute positions — real tracker hardware
    // only moves the *start* of playback, not the loop region itself.
    let sample_start_frames = events.first().map(|n| n.sample_offset_frames).unwrap_or(0);

    {
        let sample_part = xmlutil::find_mut(&mut track, ".//MultiSamplePart").expect("expected element \".//MultiSamplePart\" not found in template track");
        xmlutil::set_value(sample_part, "./RootKey", &sample.base_note.to_string());
        xmlutil::set_value(sample_part, "./SampleStart", &sample_start_frames.to_string());
        xmlutil::set_value(sample_part, "./SampleEnd", &last_frame.to_string());

        xmlutil::set_value(sample_part, "./SustainLoop/Start", &loop_start.to_string());
        xmlutil::set_value(sample_part, "./SustainLoop/End", &loop_end.to_string());
        xmlutil::set_value(sample_part, "./SustainLoop/Mode", if sample.has_loop() { "1" } else { "0" });
        xmlutil::set_value(sample_part, "./ReleaseLoop/Start", &loop_start.to_string());
        xmlutil::set_value(sample_part, "./ReleaseLoop/End", &loop_end.to_string());
    }

    let file_size = std::fs::metadata(wav_path).map(|m| m.len()).unwrap_or(0);
    // mutate every FileRef descendant (mirrors Python's `for file_ref in
    // sample_part.findall(".//FileRef")`) via a small dedicated recursive visitor, since
    // find_all_descendants only gives immutable refs.
    {
        let sample_part = xmlutil::find_mut(&mut track, ".//MultiSamplePart").unwrap();
        let wav_name = wav_path.file_name().unwrap().to_string_lossy().to_string();
        let wav_abs = std::fs::canonicalize(wav_path).unwrap_or_else(|_| wav_path.to_path_buf());
        xmlutil::visit_mut(sample_part, &mut |node| {
            if node.name == "FileRef" {
                xmlutil::set_value(node, "./RelativePathType", "1");
                xmlutil::set_value(node, "./RelativePath", &format!("Samples/Imported/{wav_name}"));
                xmlutil::set_value(node, "./Path", &wav_abs.to_string_lossy());
                xmlutil::set_value(node, "./Type", "2");
                xmlutil::set_value(node, "./LivePackName", "");
                xmlutil::set_value(node, "./LivePackId", "");
                xmlutil::set_value(node, "./OriginalFileSize", &file_size.to_string());
                xmlutil::set_value(node, "./OriginalCrc", "0");
            }
        });

        let sample_ref = xmlutil::find_mut(sample_part, "./SampleRef").expect("expected element \"./SampleRef\" not found in template track");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        xmlutil::set_value(sample_ref, "./LastModDate", &now.to_string());
        xmlutil::set_value(sample_ref, "./DefaultDuration", &num_frames.to_string());
        xmlutil::set_value(sample_ref, "./DefaultSampleRate", &sample.sample_rate_hz.to_string());
        xmlutil::set_value(sample_ref, "./SamplesToAutoWarp", "0");
    }

    let clip_template = {
        let events_el = xmlutil::find_mut(&mut track, ".//ClipTimeable/ArrangerAutomation/Events").expect("expected element \".//ClipTimeable/ArrangerAutomation/Events\" not found in template track");
        let idx = events_el
            .children
            .iter()
            .position(|n| matches!(n, XMLNode::Element(e) if e.name == "MidiClip"))
            .expect("expected element \"./MidiClip\" not found in template track");
        match events_el.children.remove(idx) {
            XMLNode::Element(e) => e,
            _ => unreachable!(),
        }
    };

    let events_by_segment = group_notes_by_segment(events, segments);
    for (seg_index, segment) in segments.iter().enumerate() {
        let Some(seg_notes) = events_by_segment.get(&seg_index) else { continue }; // sample doesn't play during this pattern — no clip needed
        let local_notes: Vec<ClippedNote> = seg_notes.iter().map(|n| clip_note_to_segment(n, segment)).collect();
        let clip = build_clip(&clip_template, &display_name, segment, &local_notes, seg_index, color_index_for_sample(sample));
        let events_el = xmlutil::find_mut(&mut track, ".//ClipTimeable/ArrangerAutomation/Events").unwrap();
        append_child(events_el, clip);
    }

    // Pitch Bend/Volume/Panorama automation lives at the *track* level (see
    // build_automation_envelope), built once from the full, untruncated note list — not per
    // clip/segment, since a track-wide automation lane isn't scoped to any one clip.

    // Portamento/Tone Portamento/Vibrato ticks glide smoothly into one another; Arpeggio's
    // ticks are always discrete jumps — step_points_with_glide steps only the latter (and
    // any run's first tick after a gap), so the curve reads right for each effect.
    let bend_points = step_points_with_glide(&collect_bend_points(events));
    {
        let automation_envelopes_el = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes").expect("expected element \"./AutomationEnvelopes/Envelopes\" not found in template track");
        build_automation_envelope(automation_envelopes_el, &bend_points, &transpose_pointee_id, id_counter, 0.0);
    }

    // Set Volume (Cxx) is a genuine instant jump and gets the step treatment — but Volume
    // Slide (Axy) is applied tick by tick like Portamento, and stepping *every* one of those
    // ticks turns what should read as one smooth fade into a jittery staircase of little
    // jumps. collect_volume_points/step_points_with_glide keep the per-tick points of an
    // uninterrupted slide connected smoothly, and only step genuine jumps (Set Volume, or a
    // slide's own first tick after a gap).
    let volume_points = step_points_with_glide(&collect_volume_points(events));
    {
        let automation_envelopes_el = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes").unwrap();
        build_automation_envelope(automation_envelopes_el, &volume_points, &volume_pointee_id, id_counter, volume_to_gain(64));
    }

    // Panorama gets an envelope when AmigaPanning is enabled, or when the track uses Set
    // Panning (8xx) somewhere even with it off — see collect_pan_points.
    let pan_points = step_points(&collect_pan_points(events, amiga_panning));
    {
        let automation_envelopes_el = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes").unwrap();
        build_automation_envelope(automation_envelopes_el, &pan_points, &pan_pointee_id, id_counter, 0.0);
    }

    track
}

/// Builds one `<GroupTrack>` (see GROUP_TRACK_TEMPLATE_XML) folding together a sample's own
/// voice tracks — only called for samples that actually need more than one voice. Returns the
/// group's own (freshly renumbered) Id, for the caller to write into each member track's own
/// `TrackGroupId`; per the reference project this was captured from, the GroupTrack element
/// itself must come *before* its member tracks in document order.
fn build_group_track(group_track_template: &Element, sample: &Sample, id_counter: &mut IdCounter) -> (Element, String) {
    let mut track = group_track_template.clone();
    let track_id = id_counter.next().to_string();
    track.attributes.insert("Id".to_string(), track_id.clone());
    renumber_global_ids(&mut track, id_counter);

    let display_name = format!("{:02} {}", sample.index, sample.name).trim().to_string();
    xmlutil::set_value(&mut track, "./Name/EffectiveName", &display_name);
    // Without also setting UserName, Ableton falls back to auto-naming a group with no
    // human-set name — literally the generic "Group" — regardless of EffectiveName's content;
    // see the matching comment in build_track for how this was confirmed against real Ableton.
    xmlutil::set_value(&mut track, "./Name/UserName", &display_name);
    xmlutil::set_value(&mut track, "./Color", &color_index_for_sample(sample).to_string());
    // Folded by default: hides the group's own member tracks in the track list, showing just
    // the group header (same TrackUnfolded flag build_track sets false on every voice track,
    // there meaning "minimized row height" rather than "hides children").
    xmlutil::set_value(&mut track, "./TrackUnfolded", "false");

    (track, track_id)
}

pub fn export_als(
    module: &Module, output_path: &Path, template_bytes: &[u8], amiga_panning: AmigaPanning,
) -> Result<(), String> {
    let mut decoder = flate2::read::GzDecoder::new(template_bytes);
    let mut xml_string = String::new();
    decoder.read_to_string(&mut xml_string).map_err(|e| format!("failed to gunzip template: {e}"))?;
    let mut root = Element::parse(xml_string.as_bytes()).map_err(|e| format!("failed to parse template XML: {e}"))?;

    let template_track: Element = {
        let live_set = xmlutil::find(&root, "./LiveSet").ok_or("template .als has no LiveSet element")?;
        let tracks_el = xmlutil::find(live_set, "./Tracks").ok_or("template .als has no Tracks element")?;
        find_sampler_track_template(tracks_el)?.clone()
    };

    let song = compute_song_events(module);
    let non_empty_samples: Vec<&Sample> = module.samples.iter().filter(|s| !s.is_empty()).collect();

    // Displayed/effective BPM must fold in Speed, not just the module's raw Tempo number
    // (see export::notes: a row is always a 16th note, so Speed changes the tempo needed to
    // keep real-world timing correct instead of changing the note grid). `song.tempo_changes`
    // already carries that — including any mid-song Fxx changes — as (beat, bpm) points.
    if !song.tempo_changes.is_empty() {
        let first_bpm = song.tempo_changes[0].bpm;
        if let Some(live_set) = xmlutil::find_mut(&mut root, "./LiveSet") {
            if let Some(tempo_el) = xmlutil::find_mut(live_set, ".//Tempo/Manual") {
                tempo_el.attributes.insert("Value".to_string(), fmt_number(first_bpm));
            }
            // The static Manual value above is only the fallback: if the Main Track carries
            // its own tempo automation envelope (very common — Live seeds one by default),
            // Live follows that curve instead and the Manual value is silently ignored.
            // Replace its points with the module's real tempo timeline rather than leaving a
            // stale default.
            let tempo_target_id = xmlutil::find(live_set, ".//Tempo/AutomationTarget")
                .and_then(|e| e.attributes.get("Id").cloned());
            if let Some(tempo_target_id) = tempo_target_id {
                if let Some(main_track) = xmlutil::find_mut(live_set, ".//MainTrack") {
                    if let Some(envelopes) = xmlutil::find_mut(main_track, "./AutomationEnvelopes/Envelopes") {
                        for node in &mut envelopes.children {
                            let XMLNode::Element(env) = node else { continue };
                            if env.name != "AutomationEnvelope" {
                                continue;
                            }
                            let matches_target = xmlutil::find(env, "./EnvelopeTarget/PointeeId")
                                .map(|p| p.attributes.get("Value") == Some(&tempo_target_id))
                                .unwrap_or(false);
                            if !matches_target {
                                continue;
                            }
                            let Some(events_el) = xmlutil::find_mut(env, ".//Automation/Events") else { continue };
                            events_el.children.clear();
                            let mut float_event_id = 0i64;
                            let mut next_id = || {
                                let v = float_event_id;
                                float_event_id += 1;
                                v
                            };
                            append_child(
                                events_el,
                                new_element("FloatEvent", &[
                                    ("Id", &next_id().to_string()),
                                    ("Time", AUTOMATION_SENTINEL_TIME),
                                    ("Value", &fmt_number(first_bpm)),
                                ]),
                            );
                            append_child(
                                events_el,
                                new_element("FloatEvent", &[
                                    ("Id", &next_id().to_string()),
                                    ("Time", "0"),
                                    ("Value", &fmt_number(first_bpm)),
                                ]),
                            );
                            let mut previous_bpm = first_bpm;
                            for tc in &song.tempo_changes[1..] {
                                // Ableton's clip/track automation interpolates linearly
                                // between points by default — a lone point at the new BPM
                                // would ramp gradually from the previous value instead of
                                // jumping. Hold the old value right up until the change to
                                // force a clean step instead of a glide.
                                append_child(
                                    events_el,
                                    new_element("FloatEvent", &[
                                        ("Id", &next_id().to_string()),
                                        ("Time", &fmt_number(tc.at_beat - TEMPO_STEP_EPSILON_BEATS)),
                                        ("Value", &fmt_number(previous_bpm)),
                                    ]),
                                );
                                append_child(
                                    events_el,
                                    new_element("FloatEvent", &[
                                        ("Id", &next_id().to_string()),
                                        ("Time", &fmt_number(tc.at_beat)),
                                        ("Value", &fmt_number(tc.bpm)),
                                    ]),
                                );
                                previous_bpm = tc.bpm;
                            }
                        }
                    }
                }
            }
        }
    }

    let samples_dir = output_path.parent().unwrap_or_else(|| Path::new(".")).join("Samples").join("Imported");
    std::fs::create_dir_all(&samples_dir).map_err(|e| format!("failed to create {}: {e}", samples_dir.display()))?;

    let group_track_template = Element::parse(GROUP_TRACK_TEMPLATE_XML.as_bytes())
        .map_err(|e| format!("failed to parse bundled templates/group_track.xml: {e}"))?;

    let mut id_counter = IdCounter(max_id(&root) + 1);
    let mut new_tracks: Vec<Element> = Vec::new();
    let mut armed_first_track = false; // exactly one track (the very first) stays armed — see build_track
    for sample in &non_empty_samples {
        let sample = ensure_loopable(sample);
        let sample = sample.as_ref();
        let wav_path = samples_dir.join(sample_wav_filename(sample));
        write_sample_wav(sample, &wav_path).map_err(|e| format!("failed to write {}: {e}", wav_path.display()))?;

        let notes = song.notes_by_sample.get(&sample.index).cloned().unwrap_or_default();
        // A sample triggered on multiple tracker channels with overlapping timing needs more
        // than one track to avoid its notes colliding — see the voice-assignment pass in
        // export::notes::compute_song_events. Most samples only ever need one voice, so
        // voice_count is 1 and no "(N)" suffix/group is added.
        let voice_count = notes.iter().map(|n| n.voice + 1).max().unwrap_or(1);

        // Group only kicks in once a sample actually has more than one voice track — a group
        // around a single track would just be an extra click to open for no benefit, and is
        // exactly the vast majority of samples.
        let group_id = if voice_count > 1 {
            let (group_track, group_id) = build_group_track(&group_track_template, sample, &mut id_counter);
            new_tracks.push(group_track); // must precede its member tracks in document order
            Some(group_id)
        } else {
            None
        };

        for voice in 0..voice_count {
            let voice_notes: Vec<NoteEvent> = notes.iter().filter(|n| n.voice == voice).cloned().collect();
            let voice_label = if voice > 0 { Some(voice + 1) } else { None }; // first voice keeps the plain name
            let mut track = build_track(
                &template_track, sample, &mut id_counter, &wav_path, &voice_notes, &song.segments, amiga_panning,
                voice_label,
            );
            if let Some(group_id) = &group_id {
                xmlutil::set_value(&mut track, "./TrackGroupId", group_id);
            }
            if !armed_first_track {
                xmlutil::set_value(&mut track, "./DeviceChain/MainSequencer/Recorder/IsArmed", "true");
                armed_first_track = true;
            }
            new_tracks.push(track);
        }
    }

    {
        let live_set = xmlutil::find_mut(&mut root, "./LiveSet").ok_or("template .als has no LiveSet element")?;
        let tracks_el = xmlutil::find_mut(live_set, "./Tracks").ok_or("template .als has no Tracks element")?;
        let return_tracks: Vec<Element> = tracks_el
            .children
            .iter()
            .filter_map(|n| match n {
                XMLNode::Element(e) if e.name == "ReturnTrack" => Some(e.clone()),
                _ => None,
            })
            .collect();
        tracks_el.children.clear();
        for track in new_tracks {
            append_child(tracks_el, track);
        }
        for return_track in return_tracks {
            append_child(tracks_el, return_track);
        }

        if let Some(next_pointee_id) = xmlutil::find_mut(live_set, "./NextPointeeId") {
            next_pointee_id.attributes.insert("Value".to_string(), (id_counter.next() + 1).to_string());
        }
    }

    let mut xml_bytes = Vec::new();
    let config = xmltree::EmitterConfig::new().write_document_declaration(false);
    root.write_with_config(&mut xml_bytes, config).map_err(|e| format!("failed to serialize XML: {e}"))?;
    let mut final_bytes = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".to_vec();
    final_bytes.extend_from_slice(&xml_bytes);

    let out_file = std::fs::File::create(output_path).map_err(|e| format!("failed to create {}: {e}", output_path.display()))?;
    let mut encoder = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
    encoder.write_all(&final_bytes).map_err(|e| format!("failed to write {}: {e}", output_path.display()))?;
    encoder.finish().map_err(|e| format!("failed to finish gzip: {e}"))?;

    Ok(())
}

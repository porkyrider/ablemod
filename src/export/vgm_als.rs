//! Packages a rendered VGM/VGZ file's per-channel stems into an Ableton Live Set, one plain
//! (non-warped) AudioTrack per stem — no combined master-mix track, since that's just the
//! stems already summed and Ableton sums them again on playback. Uses a template AudioTrack
//! captured from a real Ableton project (templates/audio_track.xml) for the same reason
//! templates/group_track.xml exists: Ableton's .als schema is undocumented, and cloning a
//! real, working example beats guessing at one — this project has been burned by that before.
//!
//! Unlike the tracker side (export::als), there's no Module/Sample IR to drive this from —
//! each track's audio was already fully synthesized by export::vgm_render. Clips are left
//! un-warped (`IsWarped=false`) so they always play back at their own natural length
//! regardless of the project's tempo.
//!
//! When the source VGM declares a loop point (VgmFile::loop_start_sample — a real, ripper-
//! authored marker, not something inferred), each stem is split there into two clips: an
//! "intro" clip covering the part played once before the loop, and a "loop" clip covering the
//! repeating part. The project tempo is then derived from the loop segment's own duration
//! (mapped to the nearest whole number of 4/4 bars in the 80-160 BPM range) so that segment
//! lines up with the bar grid — the closest thing a register-write log has to a tracker
//! "pattern". A file with no declared loop point falls back to one full-length clip per stem
//! and keeps the template's own tempo, since there's no periodicity to derive one from.

use std::io::{Read, Write};
use std::path::Path;

use xmltree::{Element, XMLNode};

use crate::export::als::{
    append_child, build_automation_envelope, build_clip, fmt_number, max_id, renumber_global_ids, set_constant_tempo,
    step_points_with_glide, IdCounter,
};
use crate::export::notes::Segment as NoteSegment;
use crate::export::vgm_render::{peak, slice, write_wav, RenderedAudio, Stem};
use crate::export::vgm_operator;
use crate::export::vgm_wavetable::{self, ChannelTrack};
use crate::formats::vgm::{Chip, VgmFile};
use crate::xmlutil;

const AUDIO_TRACK_TEMPLATE_XML: &str = include_str!("../../templates/audio_track.xml");
const WAVETABLE_TRACK_TEMPLATE_XML: &str = include_str!("../../templates/wavetable_track.xml");
const OPERATOR_TRACK_TEMPLATE_XML: &str = include_str!("../../templates/operator_track.xml");
const GROUP_TRACK_TEMPLATE_XML: &str = include_str!("../../templates/group_track.xml");

/// Every generated track's own Mixer fader is pulled down by this much, the same
/// headroom-baseline idea export::als's own TRACK_VOLUME_DB already applies on the tracker
/// side (see its own comment) — so stems summing together in Ableton's own mixer don't clip
/// above 0dB. Applied as a static fader value, not a change to the WAV data itself, so it
/// stays freely adjustable per-track from inside Ableton afterwards.
const TRACK_GAIN_DB: f64 = -6.0;

/// Sets a freshly-built track's own `DeviceChain/Mixer/Volume/Manual` (a *linear* gain, not
/// dB — same convention as export::als::volume_to_gain) to TRACK_GAIN_DB. `./` here is a
/// direct-child chain from the track root (see xmlutil's own doc comment), so this only ever
/// touches the track's own top-level Mixer, never a Volume parameter belonging to a plugin
/// nested inside its device chain (Wavetable/Operator both have their own, unrelated, Volume
/// elements further down).
fn apply_track_gain(track: &mut Element) {
    let gain = 10f64.powf(TRACK_GAIN_DB / 20.0);
    xmlutil::set_value(track, "./DeviceChain/Mixer/Volume/Manual", &fmt_number(gain));
}

pub(crate) fn sanitize_filename(name: &str) -> String {
    let safe: String =
        name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let safe = safe.trim();
    if safe.is_empty() { "track".to_string() } else { safe.to_string() }
}

/// One audio segment to place as its own clip on a track: rendered audio, the clip's own
/// name suffix (e.g. "intro"/"loop", or "" for a single full-length clip), and where to write
/// its WAV file.
struct Segment<'a> {
    audio: &'a RenderedAudio,
    name_suffix: &'a str,
    wav_path: &'a Path,
}

/// Builds one AudioClip element (cloned from `clip_template`) at absolute arrangement
/// position `start_beat`, referencing `segment`'s WAV file.
fn build_audio_clip(clip_template: &Element, clip_id: i64, start_beat: f64, name: &str, color: i32, segment: &Segment, tempo_bpm: f64) -> Element {
    let mut clip = clip_template.clone();
    clip.attributes.insert("Id".to_string(), clip_id.to_string());
    clip.attributes.insert("Time".to_string(), fmt_number(start_beat));

    let frame_count = segment.audio.left.len();
    let sample_rate = segment.audio.sample_rate;
    let duration_seconds = frame_count as f64 / sample_rate as f64;
    let duration_beats = duration_seconds * tempo_bpm / 60.0;
    let end_beat = start_beat + duration_beats;

    // CurrentStart/CurrentEnd/OutMarker are absolute arrangement positions in *beats*,
    // matching the Time attribute above (same convention already confirmed against a real
    // Ableton round-trip for MidiClip in export::als::build_clip — leaving these
    // clip-relative-to-zero instead would stack every clip on a track at the arrangement's
    // very start regardless of Time, and Ableton would silently drop all but the first).
    // Loop/LoopEnd, Loop/HiddenLoopEnd and ScrollerTimePreserver/RightTime are different: for
    // a non-warped clip those describe which part of the *underlying WAV file* to play, in
    // the file's own absolute seconds — independent of the clip's placement in the
    // arrangement, so they always start back at 0 regardless of start_beat.
    xmlutil::set_value(&mut clip, "./CurrentStart", &fmt_number(start_beat));
    xmlutil::set_value(&mut clip, "./CurrentEnd", &fmt_number(end_beat));
    xmlutil::set_value(&mut clip, "./Loop/LoopStart", "0");
    xmlutil::set_value(&mut clip, "./Loop/LoopEnd", &fmt_number(duration_seconds));
    xmlutil::set_value(&mut clip, "./Loop/HiddenLoopStart", "0");
    xmlutil::set_value(&mut clip, "./Loop/HiddenLoopEnd", &fmt_number(duration_seconds));
    xmlutil::set_value(&mut clip, "./Loop/OutMarker", &fmt_number(end_beat));
    xmlutil::set_value(&mut clip, "./Loop/LoopOn", "false");
    xmlutil::set_value(&mut clip, "./Name", name);
    xmlutil::set_value(&mut clip, "./Color", &color.to_string());
    xmlutil::set_value(&mut clip, "./ScrollerTimePreserver/LeftTime", "0");
    xmlutil::set_value(&mut clip, "./ScrollerTimePreserver/RightTime", &fmt_number(duration_seconds));

    let wav_name = segment.wav_path.file_name().unwrap().to_string_lossy().to_string();
    let wav_abs = std::fs::canonicalize(segment.wav_path).unwrap_or_else(|_| segment.wav_path.to_path_buf());
    let file_size = std::fs::metadata(segment.wav_path).map(|m| m.len()).unwrap_or(0);
    xmlutil::visit_mut(&mut clip, &mut |node| {
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
    let sample_ref = xmlutil::find_mut(&mut clip, "./SampleRef").expect("expected element SampleRef not found in template audio clip");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    xmlutil::set_value(sample_ref, "./LastModDate", &now.to_string());
    xmlutil::set_value(sample_ref, "./DefaultDuration", &frame_count.to_string());
    xmlutil::set_value(sample_ref, "./DefaultSampleRate", &sample_rate.to_string());
    xmlutil::set_value(sample_ref, "./SamplesToAutoWarp", "0");

    clip
}

/// Builds one AudioTrack holding one clip per `segments` entry, placed back-to-back in the
/// Arrangement in order.
fn build_audio_track(template: &Element, id_counter: &mut IdCounter, name: &str, color: i32, segments: &[Segment], tempo_bpm: f64) -> Element {
    let mut track = template.clone();
    let track_id = id_counter.next().to_string();
    track.attributes.insert("Id".to_string(), track_id);
    renumber_global_ids(&mut track, id_counter);

    xmlutil::set_value(&mut track, "./Name/EffectiveName", name);
    // Without also setting UserName, Ableton falls back to auto-naming the track on its own
    // next save, discarding this name — confirmed against a real Ableton round-trip on the
    // tracker side of this project (see export::als::build_track for the full story).
    xmlutil::set_value(&mut track, "./Name/UserName", name);
    xmlutil::set_value(&mut track, "./Color", &color.to_string());
    xmlutil::set_value(&mut track, "./TrackUnfolded", "false"); // folded/minimized by default
    // The template's own track happens to be armed (see the tracker side's own note on this)
    // — force both of an AudioTrack's Recorder/IsArmed flags (main + freeze sequencer) off;
    // there's no live-recording workflow implied by a VGM render, so nothing needs arming.
    xmlutil::set_value(&mut track, "./DeviceChain/MainSequencer/Recorder/IsArmed", "false");
    xmlutil::set_value(&mut track, "./DeviceChain/FreezeSequencer/Recorder/IsArmed", "false");
    apply_track_gain(&mut track);

    let events_el = xmlutil::find_mut(&mut track, ".//DeviceChain/MainSequencer/Sample/ArrangerAutomation/Events")
        .expect("expected element Events not found in template audio track");
    let clip_idx = events_el
        .children
        .iter()
        .position(|n| matches!(n, XMLNode::Element(e) if e.name == "AudioClip"))
        .expect("expected element AudioClip not found in template audio track");
    let clip_template = match events_el.children.remove(clip_idx) {
        XMLNode::Element(e) => e,
        _ => unreachable!(),
    };

    let mut start_beat = 0.0;
    for (i, segment) in segments.iter().enumerate() {
        let clip_name = if segment.name_suffix.is_empty() { name.to_string() } else { format!("{name} ({})", segment.name_suffix) };
        let clip = build_audio_clip(&clip_template, i as i64, start_beat, &clip_name, color, segment, tempo_bpm);
        let duration_beats: f64 = segment.audio.left.len() as f64 / segment.audio.sample_rate as f64 * tempo_bpm / 60.0;
        start_beat += duration_beats;
        let events_el = xmlutil::find_mut(&mut track, ".//DeviceChain/MainSequencer/Sample/ArrangerAutomation/Events").unwrap();
        append_child(events_el, clip);
    }

    track
}

/// Ableton's Wavetable expects a single frame to be one of a handful of standard sizes; 1024
/// samples is the one this project's own reference project used.
const WAVETABLE_FRAME_SAMPLES: usize = 1024;

/// How long a Position transition between wavetable frames is spread over — see
/// build_wavetable_track's own comment on why this is deliberately a short ramp, not an
/// instant step (unlike Gain/Transpose's step_points_with_glide) and not the tempo-relative
/// near-zero epsilon step_points_with_glide uses for tracker automation.
const POSITION_MORPH_SECONDS: f64 = 0.05;

/// Like export::als::step_points_with_glide's step treatment, but holds each point's *previous* value starting
/// `ramp_beats` before the jump instead of an unnoticeable instant before it — Ableton's
/// default linear interpolation between the two then reads as a short, deliberate crossfade.
fn ramp_points(points: &[(f64, f64)], ramp_beats: f64) -> Vec<(f64, f64)> {
    if points.is_empty() {
        return Vec::new();
    }
    let mut ramped = vec![points[0]];
    for w in points.windows(2) {
        let (_prev_beat, prev_value) = w[0];
        let (beat, value) = w[1];
        if value != prev_value {
            ramped.push(((beat - ramp_beats).max(_prev_beat), prev_value));
        }
        ramped.push((beat, value));
    }
    ramped
}

/// Writes a channel's distinct 32-sample SCC waveforms as consecutive WAVETABLE_FRAME_SAMPLES
/// blocks in one mono WAV — Wavetable reads a multi-frame import as one frame per such block,
/// selectable via its Position parameter (confirmed against a real Ableton round-trip), which
/// is exactly what export::vgm_wavetable's per-channel `frames` timeline is for: every
/// genuinely distinct waveform the channel used, in the order first used, becomes its own
/// selectable frame — see build_wavetable_track for the Position automation that switches
/// between them at the right times.
///
/// Each of the 32 source samples is held for 32 consecutive output samples within its
/// block (step/sample-and-hold, not interpolated) rather than smoothed — that stepped
/// waveform *is* what the real chip outputs (see vendor/libvgm/emu/cores/k051649.c's own
/// `k051649_update`: the DAC just holds one 8-bit value until the next tick), and smoothing
/// it away would trade its characteristic digital edge for a mellower, less faithful tone.
///
/// Not peak-normalized: an earlier version scaled each channel's own waveform up to full
/// scale independently, on the theory that a real game's raw table data is often authored
/// quiet (its actual loudness on real hardware coming from a separate volume-register
/// multiply the synth oscillator has no equivalent of). Confirmed by ear this made the
/// export louder but *wrong* — per-channel independent normalization also destroys their
/// relative loudness against each other, the same reason export::vgm_render::write_wav
/// shares one gain across every WAV stem instead of self-normalizing each one.
fn write_wavetable_frames_wav(waveforms: &[[i8; 32]], path: &Path) -> std::io::Result<()> {
    let hold = WAVETABLE_FRAME_SAMPLES / 32;

    let spec = hound::WavSpec { channels: 1, sample_rate: 44100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec).map_err(std::io::Error::other)?;
    for frame in waveforms {
        for &s in frame {
            let sample_16 = (s as i16) << 8; // 8->16 bit, same convention as export::wav
            for _ in 0..hold {
                writer.write_sample(sample_16).map_err(std::io::Error::other)?;
            }
        }
    }
    writer.finalize().map_err(std::io::Error::other)?;
    Ok(())
}

/// Builds one `<GroupTrack>` (see GROUP_TRACK_TEMPLATE_XML) with the given name/color, empty
/// of members — the caller appends the group first, then its member tracks (each with its own
/// `TrackGroupId` set to the returned id), matching how Ableton expects group nesting to read
/// in document order (see export::als::build_group_track, the tracker-side twin of this).
fn build_group_track(template: &Element, id_counter: &mut IdCounter, name: &str, color: i32) -> (Element, String) {
    let mut track = template.clone();
    let track_id = id_counter.next().to_string();
    track.attributes.insert("Id".to_string(), track_id.clone());
    renumber_global_ids(&mut track, id_counter);
    xmlutil::set_value(&mut track, "./Name/EffectiveName", name);
    xmlutil::set_value(&mut track, "./Name/UserName", name);
    xmlutil::set_value(&mut track, "./Color", &color.to_string());
    xmlutil::set_value(&mut track, "./TrackUnfolded", "false");
    (track, track_id)
}

/// Builds one MIDI track driving Ableton's Wavetable instrument (XML tag `InstrumentVector`)
/// from a K051649/SCC channel's extracted notes — an *approximation* kept alongside the
/// bit-accurate WAV-rendered SCC track for the same channel, not a replacement (see this
/// module's own doc comment and export::vgm_wavetable's).
fn build_wavetable_track(
    template: &Element, id_counter: &mut IdCounter, name: &str, color: i32, wav_path: &Path, channel: &ChannelTrack,
    song_end_beat: f64, tempo_bpm: f64,
) -> Element {
    let mut track = template.clone();
    let track_id = id_counter.next().to_string();
    track.attributes.insert("Id".to_string(), track_id);
    renumber_global_ids(&mut track, id_counter);

    xmlutil::set_value(&mut track, "./Name/EffectiveName", name);
    xmlutil::set_value(&mut track, "./Name/UserName", name);
    xmlutil::set_value(&mut track, "./Color", &color.to_string());
    xmlutil::set_value(&mut track, "./TrackUnfolded", "false");
    xmlutil::set_value(&mut track, ".//DeviceChain/MainSequencer/Recorder/IsArmed", "false");
    xmlutil::set_value(&mut track, ".//DeviceChain/FreezeSequencer/Recorder/IsArmed", "false");
    apply_track_gain(&mut track);

    // Patch tuning: the reference patch this template was captured from has its own
    // filter/unison settings for its own musical purposes, which have nothing to do with SCC,
    // so the filter and unison are turned off. The amp envelope is deliberately *not*
    // flattened to instant-on/off despite the real chip having no envelope generator at all —
    // an early version did that for hardware fidelity, but a synth voice with ~0 attack and
    // 100% sustain gives the ear nothing to latch onto between notes, and was reported as
    // sounding like it had no attack/decay shape at all. This is an explicitly *unfaithful*,
    // more playable/musical envelope for the Wavetable approximation specifically — the
    // WAV-rendered SCC track remains the hardware-accurate one.
    xmlutil::set_value(&mut track, ".//Voice_Filter1_On/Manual", "false");
    xmlutil::set_value(&mut track, ".//Voice_Modulators_AmpEnvelope_Times_Attack/Manual", "0.008");
    xmlutil::set_value(&mut track, ".//Voice_Modulators_AmpEnvelope_Times_Decay/Manual", "0.12");
    xmlutil::set_value(&mut track, ".//Voice_Modulators_AmpEnvelope_Sustain/Manual", "0.65");
    xmlutil::set_value(&mut track, ".//Voice_Modulators_AmpEnvelope_Times_Release/Manual", "0.08");
    xmlutil::set_value(&mut track, ".//Voice_Unison_Amount/Manual", "0");
    // The reference patch's own WavePosition (a knob for scanning/morphing through a
    // *multi-frame* table) was left at whatever creative value its author had it at — for a
    // single-cycle source there's only one frame to read regardless, but leaving a stray
    // non-zero position risks landing partway into however Ableton's own file-import analysis
    // resampled/padded that one frame internally, rather than its start. Reset to 0.
    xmlutil::set_value(&mut track, ".//Voice_Oscillator1_Wavetables_WavePosition/Manual", "0");

    // Oscillator 1's imported user wavetable — same SampleRef/FileRef mechanism a plain
    // AudioClip uses (confirmed against the reference project: dragging a WAV onto a
    // Wavetable oscillator just references it externally, no embedded/proprietary table
    // format in the .als itself).
    let wav_name = wav_path.file_name().unwrap().to_string_lossy().to_string();
    let wav_abs = std::fs::canonicalize(wav_path).unwrap_or_else(|_| wav_path.to_path_buf());
    let file_size = std::fs::metadata(wav_path).map(|m| m.len()).unwrap_or(0);
    if let Some(sample_ref) = xmlutil::find_mut(&mut track, ".//UserSprite1/Value/SampleRef") {
        xmlutil::visit_mut(sample_ref, &mut |node| {
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
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        xmlutil::set_value(sample_ref, "./LastModDate", &now.to_string());
        xmlutil::set_value(sample_ref, "./DefaultDuration", &(channel.frames.len() * WAVETABLE_FRAME_SAMPLES).to_string());
        xmlutil::set_value(sample_ref, "./DefaultSampleRate", "44100");
        xmlutil::set_value(sample_ref, "./SamplesToAutoWarp", "0");
    }

    // Switches Oscillator 1's Position between the channel's distinct waveform frames at the
    // same timestamps they were actually loaded into SCC waveform RAM — a single-frame
    // channel needs none of this (Position just stays at its static 0 default). Ramped over
    // POSITION_MORPH_SECONDS rather than snapped instantly: Wavetable interpolates *between*
    // adjacent frames as Position moves, so a short ramp crossfades from one waveform to the
    // next instead of a hard, potentially clicky jump — the real chip's own transition is
    // instant, but a few tens of milliseconds of morph reads as smoother without being
    // anywhere near slow enough to blur the two timbres together.
    if channel.frames.len() > 1 {
        let position_target_id = xmlutil::find(&track, ".//Voice_Oscillator1_Wavetables_WavePosition/AutomationTarget")
            .and_then(|e| e.attributes.get("Id").cloned());
        if let Some(position_target_id) = position_target_id {
            let last_frame_index = (channel.frames.len() - 1) as f64;
            let points: Vec<(f64, f64)> =
                channel.frames.iter().enumerate().map(|(i, f)| (f.at_beat, i as f64 / last_frame_index)).collect();
            let ramped = ramp_points(&points, POSITION_MORPH_SECONDS * tempo_bpm / 60.0);
            if let Some(envelopes_el) = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes") {
                build_automation_envelope(envelopes_el, &ramped, &position_target_id, id_counter, 0.0);
            }
        }
    }

    // The exact chip frequency rarely lands precisely on an equal-tempered semitone, and a
    // game can wobble the frequency register around a note's center to fake vibrato the chip
    // has no hardware generator for — both get recovered here as automation on Wavetable's
    // Pitch/Detune parameter (±0.5 semitone range, matching pitch_bends' own values exactly:
    // "same nearest semitone" is how a bend was told apart from a real new note in the first
    // place — see export::vgm_wavetable) rather than genuine MIDI Pitch Bend messages, since
    // no reference project has a worked example of that mechanism's XML to copy. Pitch/
    // Transpose (±24 semitones, "Semi" in the UI) was tried first but reported back as barely
    // audible for sub-semitone automation — Detune is the parameter actually meant for fine,
    // continuous pitch offsets like this, not a coarse semitone-quantized control. This is the
    // same mechanism export::als already uses for tracker Portamento/Vibrato (Simpler's own
    // TransposeKey/TransposeFine), just pointed at Wavetable's equivalent fine-pitch parameter.
    // Each span's first bend point is a hard jump (its own tuning residual); later points
    // within the same span connect smoothly, since those are genuine in-note pitch movement.
    if !channel.pitch_bends.is_empty() {
        let detune_target_id = xmlutil::find(&track, ".//Voice_Oscillator1_Pitch_Detune/AutomationTarget")
            .and_then(|e| e.attributes.get("Id").cloned());
        if let Some(detune_target_id) = detune_target_id {
            let stepped = step_points_with_glide(&channel.pitch_bends);
            if let Some(envelopes_el) = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes") {
                build_automation_envelope(envelopes_el, &stepped, &detune_target_id, id_counter, 0.0);
            }
        }
    }

    // The real chip directly multiplies its waveform output by the volume register (0-15) —
    // Wavetable's own default patch has no routing from MIDI velocity to any gain parameter
    // (checked against the reference project: no ModulationConnectionsForInstrumentVector
    // sourced from Velocity at all). Automating Oscillator 1's Gain directly (0..1 per the
    // reference project's own MidiControllerRange, where 1 is the template's unity default)
    // reproduces that multiply without needing to guess at Ableton's velocity-modulation-
    // matrix schema. This tracks the volume register *continuously* rather than sampling it
    // once at each note's onset — many rips rewrite it every few hundred samples across a
    // note's whole sustain to fake an amplitude envelope, and one static snapshot per note
    // caught an arbitrary point on that ramp, making note-to-note loudness look random rather
    // than like the intended attack/decay shape (see export::vgm_wavetable::ChannelTrack::gains).
    if !channel.gains.is_empty() {
        let gain_target_id = xmlutil::find(&track, ".//Voice_Oscillator1_Gain/AutomationTarget").and_then(|e| e.attributes.get("Id").cloned());
        if let Some(gain_target_id) = gain_target_id {
            let stepped = step_points_with_glide(&channel.gains);
            if let Some(envelopes_el) = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes") {
                build_automation_envelope(envelopes_el, &stepped, &gain_target_id, id_counter, 0.0);
            }
        }
    }

    // One clip spanning the whole song with every note this channel played — no intro/loop
    // split here (unlike build_audio_track), keeping this first pass simple.
    let events_el = xmlutil::find_mut(&mut track, ".//ClipTimeable/ArrangerAutomation/Events")
        .expect("expected element Events not found in template wavetable track");
    let clip_idx = events_el
        .children
        .iter()
        .position(|n| matches!(n, XMLNode::Element(e) if e.name == "MidiClip"))
        .expect("expected element MidiClip not found in template wavetable track");
    let clip_template = match events_el.children.remove(clip_idx) {
        XMLNode::Element(e) => e,
        _ => unreachable!(),
    };
    let segment = NoteSegment { start_beat: 0.0, end_beat: song_end_beat, order_pos: 0, pattern_index: 0 };
    let clip = build_clip(&clip_template, name, &segment, &channel.notes, 0, color);
    let events_el = xmlutil::find_mut(&mut track, ".//ClipTimeable/ArrangerAutomation/Events").unwrap();
    append_child(events_el, clip);

    track
}

/// Sets one Operator oscillator (Operator.0="A"/carrier or Operator.1="B"/modulator) up as a
/// plain sine partial at the given ratio/envelope, per export::vgm_operator's own doc comment
/// on why only these two fields are baked in statically rather than automated per-note.
fn set_operator_voice(track: &mut Element, index: u8, coarse: u8, attack_ms: f64, decay_ms: f64, sustain_gain: f64, release_ms: f64) {
    let prefix = format!(".//Operator.{index}");
    xmlutil::set_value(track, &format!("{prefix}/IsOn/Manual"), "true");
    xmlutil::set_value(track, &format!("{prefix}/WaveForm/Manual"), "0"); // sine — the only waveform YM3526 (unlike later OPL2) has
    xmlutil::set_value(track, &format!("{prefix}/Tune/Coarse/Manual"), &coarse.to_string());
    xmlutil::set_value(track, &format!("{prefix}/Tune/Fine/Manual"), "0");
    xmlutil::set_value(track, &format!("{prefix}/Envelope/AttackTime/Manual"), &fmt_number(attack_ms));
    xmlutil::set_value(track, &format!("{prefix}/Envelope/AttackLevel/Manual"), "1"); // OPL's attack always rises to full
    xmlutil::set_value(track, &format!("{prefix}/Envelope/DecayTime/Manual"), &fmt_number(decay_ms));
    xmlutil::set_value(track, &format!("{prefix}/Envelope/DecayLevel/Manual"), &fmt_number(sustain_gain));
    xmlutil::set_value(track, &format!("{prefix}/Envelope/SustainLevel/Manual"), &fmt_number(sustain_gain));
    xmlutil::set_value(track, &format!("{prefix}/Envelope/ReleaseTime/Manual"), &fmt_number(release_ms));
    xmlutil::set_value(track, &format!("{prefix}/Envelope/ReleaseLevel/Manual"), "0.0003162277571"); // OPL always releases to silence
}

/// Builds one MIDI track driving Ableton's Operator instrument from a YM3526/OPL channel's
/// extracted notes — an approximation kept alongside the bit-accurate WAV render for the same
/// channel (see export::vgm_operator's own doc comment), same relationship
/// build_wavetable_track already has with SCC's own WAV render.
fn build_operator_track(
    template: &Element, id_counter: &mut IdCounter, name: &str, color: i32, channel: &vgm_operator::ChannelTrack, song_end_beat: f64,
) -> Element {
    let mut track = template.clone();
    let track_id = id_counter.next().to_string();
    track.attributes.insert("Id".to_string(), track_id);
    renumber_global_ids(&mut track, id_counter);

    xmlutil::set_value(&mut track, "./Name/EffectiveName", name);
    xmlutil::set_value(&mut track, "./Name/UserName", name);
    xmlutil::set_value(&mut track, "./Color", &color.to_string());
    xmlutil::set_value(&mut track, "./TrackUnfolded", "false");
    xmlutil::set_value(&mut track, ".//DeviceChain/MainSequencer/Recorder/IsArmed", "false");
    xmlutil::set_value(&mut track, ".//DeviceChain/FreezeSequencer/Recorder/IsArmed", "false");
    apply_track_gain(&mut track);

    // Always a plain 2-operator FM chain (B modulates A, only A is audible) regardless of the
    // source channel's own Connection register — see this module's own doc comment on why
    // "additive" mode isn't handled in v1. Algorithm 0 is Operator's full 4-oscillator serial
    // stack (D→C→B→A); turning C and D off below leaves exactly the B→A pair audible.
    xmlutil::set_value(&mut track, ".//Globals/Algorithm/Manual", "0");
    xmlutil::set_value(&mut track, ".//Operator.2/IsOn/Manual", "false");
    xmlutil::set_value(&mut track, ".//Operator.3/IsOn/Manual", "false");

    if let Some(patch) = &channel.patch {
        // Operator.0 = A = carrier (OPL's second/audible operator); Operator.1 = B = modulator
        // (OPL's first operator, self-feedback register applies here, not the carrier).
        set_operator_voice(&mut track, 0, patch.car_coarse, patch.car_attack_ms, patch.car_decay_ms, patch.car_sustain_gain, patch.car_release_ms);
        set_operator_voice(&mut track, 1, patch.mod_coarse, patch.mod_attack_ms, patch.mod_decay_ms, patch.mod_sustain_gain, patch.mod_release_ms);
        xmlutil::set_value(&mut track, ".//Operator.1/Feedback/Manual", &fmt_number(vgm_operator::feedback_to_percent(patch.feedback)));
        xmlutil::set_value(&mut track, ".//Operator.1/Volume/Manual", &fmt_number(patch.mod_gain));
    }

    // The carrier's own Total Level register directly scales the audible output on real
    // hardware, tracked continuously rather than sampled once per note-onset — mirrors
    // build_wavetable_track's identical rationale for Wavetable's own Gain automation (many
    // rips rewrite this register across a note's whole sustain to fake a volume envelope).
    if !channel.gains.is_empty() {
        let gain_target_id = xmlutil::find(&track, ".//Operator.0/Volume/AutomationTarget").and_then(|e| e.attributes.get("Id").cloned());
        if let Some(gain_target_id) = gain_target_id {
            let stepped = step_points_with_glide(&channel.gains);
            if let Some(envelopes_el) = xmlutil::find_mut(&mut track, "./AutomationEnvelopes/Envelopes") {
                build_automation_envelope(envelopes_el, &stepped, &gain_target_id, id_counter, 0.0);
            }
        }
    }

    // One clip spanning the whole song, same simplification build_wavetable_track makes (no
    // intro/loop split).
    let events_el = xmlutil::find_mut(&mut track, ".//ClipTimeable/ArrangerAutomation/Events")
        .expect("expected element Events not found in template operator track");
    let clip_idx = events_el
        .children
        .iter()
        .position(|n| matches!(n, XMLNode::Element(e) if e.name == "MidiClip"))
        .expect("expected element MidiClip not found in template operator track");
    let clip_template = match events_el.children.remove(clip_idx) {
        XMLNode::Element(e) => e,
        _ => unreachable!(),
    };
    let segment = NoteSegment { start_beat: 0.0, end_beat: song_end_beat, order_pos: 0, pattern_index: 0 };
    let clip = build_clip(&clip_template, name, &segment, &channel.notes, 0, color);
    let events_el = xmlutil::find_mut(&mut track, ".//ClipTimeable/ArrangerAutomation/Events").unwrap();
    append_child(events_el, clip);

    track
}

/// Derives a project tempo from a repeating segment's own duration: the smallest number of
/// 4/4 bars (doubling from 1) that brings the resulting BPM up to at least 80 — i.e. assume
/// the loop is a short, round number of bars rather than guess an arbitrary one, the same
/// ambiguity any tap-tempo/beat-detection tool has to resolve somehow. A loop shorter than
/// one bar at 80 BPM (rare — under 3 seconds) is left as a single fast bar rather than
/// invented sub-bar fractions.
fn estimate_tempo_bpm(loop_duration_seconds: f64) -> f64 {
    const BEATS_PER_BAR: f64 = 4.0;
    const MIN_BPM: f64 = 80.0;
    const MAX_BARS: f64 = 256.0;

    if !loop_duration_seconds.is_finite() || loop_duration_seconds <= 0.0 {
        return 120.0;
    }
    let mut bars = 1.0;
    loop {
        let tempo = bars * BEATS_PER_BAR * 60.0 / loop_duration_seconds;
        if tempo >= MIN_BPM || bars >= MAX_BARS {
            return tempo;
        }
        bars *= 2.0;
    }
}

/// Builds an Ableton Live Set with one audio track per stem (no combined master-mix track),
/// all sharing a single gain (see write_wav) so the stems' relative loudness stays intact.
/// `master` is only used to compute that shared gain from the full mix's own peak, never
/// written out as its own track.
///
/// `generate_approximation_tracks` controls the Wavetable (SCC) and Operator (YM3526/YM3812)
/// native-Ableton-instrument tracks documented as experimental in README.md's own "Experimental
/// / approximate" section — the bit-accurate WAV tracks are built unconditionally either way.
/// The real CLI path (see cli.rs) defaults this off, so a fresh conversion only produces the
/// WAV tracks; tests that specifically exercise Wavetable/Operator extraction pass `true`.
pub fn export_als(
    vgm: &VgmFile, master: &RenderedAudio, stems: &[Stem], output_path: &Path, template_bytes: &[u8],
    generate_approximation_tracks: bool,
) -> Result<(), String> {
    let mut decoder = flate2::read::GzDecoder::new(template_bytes);
    let mut xml_string = String::new();
    decoder.read_to_string(&mut xml_string).map_err(|e| format!("failed to gunzip template: {e}"))?;
    let mut root = Element::parse(xml_string.as_bytes()).map_err(|e| format!("failed to parse template XML: {e}"))?;

    let template_tempo_bpm: f64 = {
        let live_set = xmlutil::find(&root, "./LiveSet").ok_or("template .als has no LiveSet element")?;
        let tempo_el = xmlutil::find(live_set, ".//Tempo/Manual").ok_or("template .als has no Tempo element")?;
        tempo_el.attributes.get("Value").and_then(|v| v.parse().ok()).unwrap_or(120.0)
    };

    // The VGM's own declared loop point (a real marker set by whoever ripped/authored the
    // file, not something inferred) is the only "pattern" this format actually offers — when
    // present, derive the project tempo from how long that repeating part lasts so it lines
    // up with the bar grid; a file with no loop point has no periodicity to derive one from,
    // so it keeps the template's own tempo.
    let loop_start = vgm.loop_start_sample.filter(|&s| s > 0 && (s as usize) < master.left.len());
    let tempo_bpm = match loop_start {
        Some(start) => estimate_tempo_bpm((master.left.len() - start as usize) as f64 / master.sample_rate as f64),
        None => template_tempo_bpm,
    };

    let samples_dir = output_path.parent().unwrap_or_else(|| Path::new(".")).join("Samples").join("Imported");
    std::fs::create_dir_all(&samples_dir).map_err(|e| format!("failed to create {}: {e}", samples_dir.display()))?;

    let audio_track_template = Element::parse(AUDIO_TRACK_TEMPLATE_XML.as_bytes())
        .map_err(|e| format!("failed to parse bundled templates/audio_track.xml: {e}"))?;
    let group_track_template = Element::parse(GROUP_TRACK_TEMPLATE_XML.as_bytes())
        .map_err(|e| format!("failed to parse bundled templates/group_track.xml: {e}"))?;

    let mut id_counter = IdCounter(max_id(&root) + 1);
    let mut new_tracks: Vec<Element> = Vec::new();

    // Every file shares one gain factor, computed once from the master mix (the loudest/most
    // representative signal) — see write_wav's own doc comment for why per-file normalization
    // would be wrong here.
    let master_peak = peak(master);
    let gain = if master_peak > 0.0 { 0.9 / master_peak } else { 1.0 };

    let mut wav_tracks: Vec<Element> = Vec::new();
    for (i, stem) in stems.iter().enumerate() {
        let color = (i as i32 + 1).rem_euclid(70);
        let safe_name = sanitize_filename(&stem.name);

        let (segments_audio, wav_paths, suffixes): (Vec<RenderedAudio>, Vec<std::path::PathBuf>, Vec<&str>) = match loop_start {
            Some(start) => {
                let intro_wav = samples_dir.join(format!("{:02}_{safe_name}_intro.wav", i + 1));
                let loop_wav = samples_dir.join(format!("{:02}_{safe_name}_loop.wav", i + 1));
                (
                    vec![slice(&stem.audio, 0, start as usize), slice(&stem.audio, start as usize, stem.audio.left.len())],
                    vec![intro_wav, loop_wav],
                    vec!["intro", "loop"],
                )
            }
            None => {
                let wav_path = samples_dir.join(format!("{:02}_{safe_name}.wav", i + 1));
                (vec![slice(&stem.audio, 0, stem.audio.left.len())], vec![wav_path], vec![""])
            }
        };

        for (audio, path) in segments_audio.iter().zip(wav_paths.iter()) {
            write_wav(audio, path, gain).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
        }
        let segments: Vec<Segment> = segments_audio
            .iter()
            .zip(wav_paths.iter())
            .zip(suffixes.iter())
            .map(|((audio, wav_path), &name_suffix)| Segment { audio, name_suffix, wav_path })
            .collect();

        wav_tracks.push(build_audio_track(&audio_track_template, &mut id_counter, &stem.name, color, &segments, tempo_bpm));
    }
    if !wav_tracks.is_empty() {
        let (group, group_id) = build_group_track(&group_track_template, &mut id_counter, "WAV", 0);
        new_tracks.push(group);
        for mut track in wav_tracks {
            xmlutil::set_value(&mut track, "./TrackGroupId", &group_id);
            new_tracks.push(track);
        }
    }

    // Alongside the bit-accurate WAV render above: an *approximation* of each SCC channel
    // played through Ableton's own Wavetable instrument instead, for A/B comparison — see
    // export::vgm_wavetable and build_wavetable_track for what this can and can't capture.
    // Off by default in the real CLI path (see this function's own doc comment); tests that
    // specifically exercise this extraction pass `true`.
    let song_end_beat = master.left.len() as f64 / master.sample_rate as f64 * tempo_bpm / 60.0;
    if generate_approximation_tracks {
        let wavetable_track_template = Element::parse(WAVETABLE_TRACK_TEMPLATE_XML.as_bytes())
            .map_err(|e| format!("failed to parse bundled templates/wavetable_track.xml: {e}"))?;
        let operator_track_template = Element::parse(OPERATOR_TRACK_TEMPLATE_XML.as_bytes())
            .map_err(|e| format!("failed to parse bundled templates/operator_track.xml: {e}"))?;

        let wavetable_channels = vgm_wavetable::extract_channels(vgm, tempo_bpm);
        let mut wavetable_tracks: Vec<Element> = Vec::new();
        for (i, channel) in wavetable_channels.iter().enumerate() {
            if channel.notes.is_empty() {
                continue;
            }
            let name = format!("SCC-{} (Wavetable)", i + 1);
            let color = (i as i32 + 1).rem_euclid(70);
            let wav_path = samples_dir.join(format!("wt_scc{}.wav", i + 1));
            let frame_waveforms: Vec<[i8; 32]> = channel.frames.iter().map(|f| f.waveform).collect();
            write_wavetable_frames_wav(&frame_waveforms, &wav_path).map_err(|e| format!("failed to write {}: {e}", wav_path.display()))?;
            wavetable_tracks.push(build_wavetable_track(
                &wavetable_track_template,
                &mut id_counter,
                &name,
                color,
                &wav_path,
                channel,
                song_end_beat,
                tempo_bpm,
            ));
        }
        if !wavetable_tracks.is_empty() {
            let (group, group_id) = build_group_track(&group_track_template, &mut id_counter, "Wavetable", 0);
            new_tracks.push(group);
            for mut track in wavetable_tracks {
                xmlutil::set_value(&mut track, "./TrackGroupId", &group_id);
                new_tracks.push(track);
            }
        }

        // YM3526 (OPL) and/or YM3812 (OPL2) channels through Ableton's Operator instrument — an
        // approximation kept alongside their own bit-accurate WAV render (see
        // export::vgm_operator and build_operator_track). Both share export::vgm_operator's own
        // extraction pipeline (they're register-compatible for every field it reads) but are
        // kept as two independent chip presences/track sets, in case a file genuinely uses both
        // at once.
        let mut operator_tracks: Vec<Element> = Vec::new();
        for &(chip, clock, label) in &[(Chip::Ym3526, vgm.ym3526_clock, "OPL"), (Chip::Ym3812, vgm.ym3812_clock, "OPL2")] {
            if clock == 0 {
                continue;
            }
            let channels = vgm_operator::extract_channels(vgm, chip, clock, tempo_bpm);
            for (i, channel) in channels.iter().enumerate() {
                if channel.notes.is_empty() {
                    continue;
                }
                let name = format!("{label}-{} (Operator)", i + 1);
                let color = (i as i32 + 1).rem_euclid(70);
                operator_tracks.push(build_operator_track(&operator_track_template, &mut id_counter, &name, color, channel, song_end_beat));
            }
        }
        if !operator_tracks.is_empty() {
            let (group, group_id) = build_group_track(&group_track_template, &mut id_counter, "Operator", 0);
            new_tracks.push(group);
            for mut track in operator_tracks {
                xmlutil::set_value(&mut track, "./TrackGroupId", &group_id);
                new_tracks.push(track);
            }
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
        set_constant_tempo(live_set, tempo_bpm);
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

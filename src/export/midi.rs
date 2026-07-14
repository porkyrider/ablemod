//! Convert a Module's simulated note events (see `export::notes`) into a
//! Standard MIDI File, one track per sample.

use std::path::Path;

use crate::export::midi_file::{MidiFile, Track};
use crate::export::notes::compute_song_events;
use crate::formats::base::Module;

const TICKS_PER_BEAT: u16 = 480;
const PITCH_BEND_RANGE_SEMITONES: f64 = 48.0; // generous headroom for tracker portamento slides

fn pitch_bend_value(semitones: f64, range_semitones: f64) -> i16 {
    let normalized = (semitones / range_semitones).clamp(-1.0, 1.0);
    let scale = if normalized >= 0.0 { 8191.0 } else { 8192.0 };
    (normalized * scale).round() as i16
}

fn pitch_bend_range_rpn(track: &mut Track, tick: u32, semitones: u8) {
    track.control_change(tick, 0, 101, 0); // RPN MSB
    track.control_change(tick, 0, 100, 0); // RPN LSB (pitch bend range)
    track.control_change(tick, 0, 6, semitones); // data entry MSB
    track.control_change(tick, 0, 38, 0); // data entry LSB (cents)
    track.control_change(tick, 0, 101, 127); // deselect RPN
    track.control_change(tick, 0, 100, 127);
}

fn pan_cc_value(pan: f64) -> u8 {
    // CC10: 0=left, 64=center, 127=right
    ((pan.clamp(-1.0, 1.0) * 63.0).round() as i32 + 64) as u8
}

fn expression_cc_value(tracker_volume: i32, trigger_volume: i32) -> u8 {
    // CC11 (Expression) is a multiplier on top of note-on velocity, not an absolute level —
    // so a Volume Slide/Set Volume relative to *this note's own* trigger volume is what
    // needs representing here, clamped to 127 (can't boost past the triggered loudness).
    let ratio = tracker_volume as f64 / (trigger_volume.max(1) as f64);
    (ratio.clamp(0.0, 1.0) * 127.0).round() as u8
}

pub fn write_midi(module: &Module, path: &Path) -> std::io::Result<()> {
    let song = compute_song_events(module);
    let non_empty_samples: Vec<&crate::formats::base::Sample> = module.samples.iter().filter(|s| !s.is_empty()).collect();

    let mut midi_file = MidiFile::new(TICKS_PER_BEAT);

    let mut conductor = Track::default();
    conductor.track_name(&module.title);
    for tc in &song.tempo_changes {
        let tick = (tc.at_beat * TICKS_PER_BEAT as f64).round() as u32;
        let tempo = (60_000_000.0 / tc.bpm).round() as u32;
        conductor.set_tempo(tick, tempo);
    }
    midi_file.tracks.push(conductor);

    for sample in &non_empty_samples {
        let mut track = Track::default();
        track.track_name(format!("{:02} {}", sample.index, sample.name).trim());

        pitch_bend_range_rpn(&mut track, 0, PITCH_BEND_RANGE_SEMITONES as u8);

        if let Some(notes) = song.notes_by_sample.get(&sample.index) {
            for note in notes {
                let start_tick = (note.start_beat * TICKS_PER_BEAT as f64).round() as u32;
                let end_tick = ((note.start_beat + note.duration_beat) * TICKS_PER_BEAT as f64).round() as u32;

                // always reset controllers before a new note-on: a note that ends without its
                // own reset (or MIDI's arbitrary event ordering) must never leak bend/pan/
                // expression state into the next note — see Ableton's own "clips out of tune"
                // pitfall for exactly this with pitch bend.
                track.pitchwheel(start_tick, 0, 0);
                track.control_change(start_tick, 0, 10, 64);
                track.control_change(start_tick, 0, 11, 127);
                track.note_on(start_tick, 0, note.pitch as u8, note.velocity as u8);

                for bend in &note.bends {
                    let bend_tick = (bend.at_beat * TICKS_PER_BEAT as f64).round() as u32;
                    if start_tick <= bend_tick && bend_tick <= end_tick {
                        track.pitchwheel(bend_tick, 0, pitch_bend_value(bend.semitones, PITCH_BEND_RANGE_SEMITONES));
                    }
                }

                for pan in &note.pans {
                    let pan_tick = (pan.at_beat * TICKS_PER_BEAT as f64).round() as u32;
                    if start_tick <= pan_tick && pan_tick <= end_tick {
                        track.control_change(pan_tick, 0, 10, pan_cc_value(pan.pan));
                    }
                }

                for vol in &note.volumes {
                    let vol_tick = (vol.at_beat * TICKS_PER_BEAT as f64).round() as u32;
                    if start_tick <= vol_tick && vol_tick <= end_tick {
                        track.control_change(vol_tick, 0, 11, expression_cc_value(vol.tracker_volume, note.trigger_volume));
                    }
                }

                track.note_off(end_tick, 0, note.pitch as u8, 0);
                if !note.bends.is_empty() {
                    track.pitchwheel(end_tick, 0, 0);
                }
                if !note.pans.is_empty() {
                    track.control_change(end_tick, 0, 10, 64);
                }
                if !note.volumes.is_empty() {
                    track.control_change(end_tick, 0, 11, 127);
                }
            }
        }

        midi_file.tracks.push(track);
    }

    midi_file.save(path)
}

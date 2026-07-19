mod common;

use ablemod::export::midi::write_midi;
use ablemod::formats::base::{Cell, Module, Pattern, Sample};
use common::midi_reader::{self, Msg};

fn module(patterns: Vec<Pattern>, num_channels: usize, speed: u32, bpm: u32, samples: Option<Vec<Sample>>) -> Module {
    let n = patterns.len();
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels,
        samples: samples.unwrap_or_else(|| vec![Sample {
            index: 1, name: "s".to_string(), pcm16: vec![0, 0], sample_rate_hz: 8363,
            loop_start: 0, loop_length: 1, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
        }]),
        patterns, order: (0..n as u32).collect(), restart_position: 0, initial_tempo_bpm: bpm, initial_speed_ticks: speed,
    }
}

const TICKS_PER_BEAT: u32 = 480;

#[test]
fn test_note_on_off_and_row_timing() {
    let empty = Cell::default();
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let pattern = Pattern { rows: vec![vec![note_on, empty.clone()], vec![empty.clone(), empty]] };
    let m = module(vec![pattern], 2, 6, 125, None);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    let note_events: Vec<_> = mid.tracks[1].events.iter().filter(|(_, m)| matches!(m, Msg::NoteOn { .. } | Msg::NoteOff { .. })).collect();

    let ticks_per_row = (6.0 * TICKS_PER_BEAT as f64 / 24.0).round() as u32;
    assert_eq!(note_events[0], &(0, Msg::NoteOn { note: 60, velocity: 127 }));
    // held through the empty row, note_off emitted at the end of the song (2 rows later)
    assert_eq!(note_events[1].0, 2 * ticks_per_row);
    assert!(matches!(note_events[1].1, Msg::NoteOff { .. }));
}

#[test]
fn test_tempo_change_effect_emits_set_tempo() {
    let empty = Cell::default();
    let tempo_change = Cell { effect: Some(0xF), effect_param: Some(140), ..Default::default() };
    let pattern = Pattern { rows: vec![vec![empty.clone()], vec![tempo_change], vec![empty]] };
    let m = module(vec![pattern], 1, 6, 125, None);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    let tempo_msgs: Vec<u32> = mid.tracks[0].events.iter().filter_map(|(_, m)| match m {
        Msg::SetTempo { tempo } => Some(*tempo),
        _ => None,
    }).collect();
    assert_eq!(tempo_msgs.len(), 2); // initial 125 bpm (speed=6, no scaling) + the Fxx change to 140
    assert_eq!(tempo_msgs[0], midi_reader::bpm2tempo(125.0));
    assert_eq!(*tempo_msgs.last().unwrap(), midi_reader::bpm2tempo(140.0));
}

#[test]
fn test_speed_change_effect_changes_tempo_not_row_duration() {
    let empty = Cell::default();
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let speed_change = Cell { effect: Some(0xF), effect_param: Some(3), ..Default::default() }; // <=32 => speed, not tempo
    let pattern = Pattern { rows: vec![
        vec![note_on, empty.clone()],
        vec![speed_change, empty.clone()],
        vec![empty.clone(), empty],
    ] };
    let m = module(vec![pattern], 2, 6, 125, None);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    let ticks_per_row = (6.0 * TICKS_PER_BEAT as f64 / 24.0).round() as u32; // fixed regardless of speed
    let note_offs: Vec<_> = mid.tracks[1].events.iter().filter(|(_, m)| matches!(m, Msg::NoteOff { .. })).collect();
    assert_eq!(note_offs[0].0, 3 * ticks_per_row); // held to the end of the (3-row) song

    let tempo_msgs: Vec<(u32, u32)> = mid.tracks[0].events.iter().filter_map(|(t, m)| match m {
        Msg::SetTempo { tempo } => Some((*t, *tempo)),
        _ => None,
    }).collect();
    assert_eq!(tempo_msgs.len(), 2);
    assert_eq!(tempo_msgs[0], (0, midi_reader::bpm2tempo(125.0)));
    assert_eq!(tempo_msgs[1].0, ticks_per_row); // the speed-change row's own beat
    assert_eq!(tempo_msgs[1].1, midi_reader::bpm2tempo(125.0 * 6.0 / 3.0));
}

#[test]
fn test_rapid_speed_alternation_produces_a_tempo_change_every_time() {
    let empty = Cell::default();
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let speed3 = Cell { effect: Some(0xF), effect_param: Some(3), ..Default::default() };
    let speed9 = Cell { effect: Some(0xF), effect_param: Some(9), ..Default::default() };
    let pattern = Pattern { rows: vec![
        vec![speed3.clone(), note_on],
        vec![speed9.clone(), empty.clone()],
        vec![speed3, empty.clone()],
        vec![speed9, empty],
    ] };
    let m = module(vec![pattern], 2, 6, 125, None);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    let tempo_bpms: Vec<f64> = mid.tracks[0].events.iter().filter_map(|(_, m)| match m {
        Msg::SetTempo { tempo } => Some((midi_reader::tempo2bpm(*tempo) * 1000.0).round() / 1000.0),
        _ => None,
    }).collect();
    // row0's Speed change lands on the very same beat as the initial tempo point, so it
    // replaces it outright rather than adding a redundant same-beat entry
    assert_eq!(tempo_bpms, vec![250.0, 83.333, 250.0, 83.333]);

    // real-world duration must still match raw tracker timing: sum(speed / 24) beats, since
    // daw_bpm == tracker_bpm here (ratio 1) — (3+9+3+9)/24 == 1 beat == TICKS_PER_BEAT ticks
    let note_offs: Vec<_> = mid.tracks[1].events.iter().filter(|(_, m)| matches!(m, Msg::NoteOff { .. })).collect();
    assert_eq!(note_offs[0].0, TICKS_PER_BEAT);
}

#[test]
fn test_non_looped_sample_never_sustains_past_its_natural_length() {
    // 4410 frames @ 44100Hz = exactly 0.1s = 0.1 beat at 60 bpm (1 beat = 1s) — much shorter
    // than the many empty rows the channel holds the note for, so the natural length must win.
    let non_looped = Sample {
        index: 1, name: "kick".to_string(), pcm16: vec![0u8; 4410 * 2], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let empty = Cell::default();
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let mut rows = vec![vec![note_on, empty.clone()]];
    for _ in 0..20 {
        rows.push(vec![empty.clone(), empty.clone()]);
    }
    let m = module(vec![Pattern { rows }], 2, 6, 60, Some(vec![non_looped]));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    let note_events: Vec<_> = mid.tracks[1].events.iter().filter(|(_, m)| matches!(m, Msg::NoteOn { .. } | Msg::NoteOff { .. })).collect();
    assert_eq!(note_events[0], &(0, Msg::NoteOn { note: 60, velocity: 127 }));
    let expected_ticks = (0.1 * TICKS_PER_BEAT as f64).round() as u32;
    assert_eq!(note_events[1].0, expected_ticks);
    assert!(matches!(note_events[1].1, Msg::NoteOff { .. }));
}

#[test]
fn test_two_channels_sharing_a_sample_without_overlap_land_on_one_track() {
    // a short, *non-looped* sample: channel 0's note finishes on its own (natural length, way
    // under one row) long before channel 1 triggers the same sample many rows later — no
    // overlap in time, so both fit on the same voice/track. (A looped sample, by contrast,
    // would ring until retriggered/song end regardless of row spacing — see the "with_overlap"
    // test below, whose two notes *do* overlap even though they're on different channels.)
    let short_sample = Sample {
        index: 1, name: "s".to_string(), pcm16: vec![0u8; 20], sample_rate_hz: 8363,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let empty = Cell::default();
    let row0 = vec![Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() }, empty.clone()];
    let mut rows = vec![row0];
    rows.extend((0..10).map(|_| vec![empty.clone(), empty.clone()]));
    rows.push(vec![empty.clone(), Cell { sample_index: Some(1), midi_note: Some(67), volume: Some(64), ..Default::default() }]);
    let m = module(vec![Pattern { rows }], 2, 6, 125, Some(vec![short_sample]));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    assert_eq!(mid.tracks.len(), 2); // conductor + 1 sample track (not 3)
    let notes: std::collections::HashSet<u8> = mid.tracks[1].events.iter().filter_map(|(_, m)| match m {
        Msg::NoteOn { note, .. } => Some(*note),
        _ => None,
    }).collect();
    assert_eq!(notes, std::collections::HashSet::from([60, 67]));
}

#[test]
fn test_two_channels_sharing_a_sample_with_overlap_get_separate_voice_tracks() {
    // two channels triggering the *same* sample in the same row means their notes both start
    // at the same beat, and (since channel 0's note is still held) genuinely overlap for the
    // rest of the song — a single MIDI track can't hold two simultaneous notes without
    // note_on/note_off pairing becoming ambiguous, so this needs a second track (see the
    // voice-assignment pass in export::notes::compute_song_events).
    let row = vec![
        Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() },
        Cell { sample_index: Some(1), midi_note: Some(67), volume: Some(64), ..Default::default() },
    ];
    let m = module(vec![Pattern { rows: vec![row] }], 2, 6, 125, None);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    assert_eq!(mid.tracks.len(), 3); // conductor + 2 voice tracks for the one colliding sample
    let track_name = |track_idx: usize| -> Option<String> {
        mid.tracks[track_idx].events.iter().find_map(|(_, m)| match m {
            Msg::TrackName(name) => Some(name.clone()),
            _ => None,
        })
    };
    assert_eq!(track_name(1).as_deref(), Some("01 s"));
    assert_eq!(track_name(2).as_deref(), Some("01 s (2)"));

    let notes_on = |track_idx: usize| -> std::collections::HashSet<u8> {
        mid.tracks[track_idx].events.iter().filter_map(|(_, m)| match m {
            Msg::NoteOn { note, .. } => Some(*note),
            _ => None,
        }).collect()
    };
    assert_eq!(notes_on(1), std::collections::HashSet::from([60]));
    assert_eq!(notes_on(2), std::collections::HashSet::from([67]));
}

#[test]
fn test_sample_number_zero_carries_forward_last_instrument() {
    let samples = vec![
        Sample { index: 1, name: "kick".to_string(), pcm16: vec![0, 0], sample_rate_hz: 8363, loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0 },
        Sample { index: 2, name: "snare".to_string(), pcm16: vec![0, 0], sample_rate_hz: 8363, loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0 },
    ];
    // row0: explicit instrument 2; row1: note with no instrument number (carries instrument 2 forward)
    let pattern = Pattern { rows: vec![
        vec![Cell { sample_index: Some(2), midi_note: Some(60), volume: Some(64), ..Default::default() }],
        vec![Cell { sample_index: None, midi_note: Some(64), volume: Some(64), ..Default::default() }],
    ] };
    let m = module(vec![pattern], 1, 6, 125, Some(samples));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    // one track per sample in the module (kick + snare), even though kick never got a note here —
    // this keeps track N aligned with the sample N.wav from extract-samples for easy Simpler mapping
    assert_eq!(mid.tracks.len(), 3);
    let track1_name = mid.tracks[1].events.iter().find_map(|(_, m)| match m { Msg::TrackName(n) => Some(n.clone()), _ => None }).unwrap();
    assert_eq!(track1_name, "01 kick");
    assert!(mid.tracks[1].events.iter().all(|(_, m)| !matches!(m, Msg::NoteOn { .. })));
    let track2_name = mid.tracks[2].events.iter().find_map(|(_, m)| match m { Msg::TrackName(n) => Some(n.clone()), _ => None }).unwrap();
    assert_eq!(track2_name, "02 snare");
    let notes: Vec<u8> = mid.tracks[2].events.iter().filter_map(|(_, m)| match m { Msg::NoteOn { note, .. } => Some(*note), _ => None }).collect();
    assert_eq!(notes, vec![60, 64]);
}

#[test]
fn test_panning_and_volume_slide_emit_cc10_and_cc11() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0, 0], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), effect: Some(0x8), effect_param: Some(255), ..Default::default() }; // hard right
    let slide_down = Cell { effect: Some(0xA), effect_param: Some(0x04), ..Default::default() };
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![slide_down]] }], 1, 6, 125, Some(vec![looped]));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.mid");

    write_midi(&m, &out).unwrap();
    let mid = midi_reader::parse(&out);

    let cc10: Vec<(u32, u8)> = mid.tracks[1].events.iter().filter_map(|(t, m)| match m {
        Msg::ControlChange { control: 10, value } => Some((*t, *value)),
        _ => None,
    }).collect();
    let cc11: Vec<(u32, u8)> = mid.tracks[1].events.iter().filter_map(|(t, m)| match m {
        Msg::ControlChange { control: 11, value } => Some((*t, *value)),
        _ => None,
    }).collect();

    assert_eq!(cc10[0], (0, 64)); // reset to center just before note_on
    assert_eq!(cc10[1].1, 127); // hard-right pan applied at note_on
    assert_eq!(cc10.last().unwrap().1, 64); // reset to center at note_off since this note used panning

    assert_eq!(cc11[0], (0, 127)); // reset to unity just before note_on
    assert_eq!(cc11.last().unwrap().1, 127); // reset to unity at note_off since this note used expression
    assert!(cc11[1].1 > 0 && cc11[1].1 < 127); // the slide-down step landed strictly between silent and unity
}

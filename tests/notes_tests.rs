use ablemod::export::notes::compute_song_events;
use ablemod::formats::base::{Cell, Envelope, EnvelopePoint, Module, Pattern, Sample};

fn module(patterns: Vec<Pattern>, num_channels: usize, samples: Vec<Sample>, speed: u32, bpm: u32) -> Module {
    let n = patterns.len();
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels, samples, patterns,
        order: (0..n as u32).collect(), restart_position: 0, initial_tempo_bpm: bpm, initial_speed_ticks: speed, linear_frequency_table: false,
    }
}

fn module_default(patterns: Vec<Pattern>, num_channels: usize, samples: Vec<Sample>) -> Module {
    module(patterns, num_channels, samples, 6, 60)
}

fn cell() -> Cell {
    Cell::default()
}

fn note(sample_index: u32, midi_note: i32, volume: u32) -> Cell {
    Cell { sample_index: Some(sample_index), midi_note: Some(midi_note), volume: Some(volume), ..Default::default() }
}

fn effect(effect: u32, param: u32) -> Cell {
    Cell { effect: Some(effect), effect_param: Some(param), ..Default::default() }
}

fn note_with_effect(sample_index: u32, midi_note: i32, volume: u32, effect: u32, param: u32) -> Cell {
    Cell {
        sample_index: Some(sample_index), midi_note: Some(midi_note), volume: Some(volume),
        effect: Some(effect), effect_param: Some(param), ..Default::default()
    }
}

fn key_off() -> Cell {
    Cell { note_off: true, ..Default::default() }
}

#[test]
fn test_overlapping_notes_on_different_channels_get_separate_voices_instead_of_being_clamped() {
    // a very long natural duration (10s @ 60bpm = 10 beats) so it would otherwise ring
    // well past the next trigger on the *other* channel sharing this sample — rather than
    // truncating it to make room (the old behavior, which silently clipped whichever note
    // triggered first), it now gets assigned a voice of its own so both notes keep their
    // full natural length and export::als/export::midi give each its own track.
    let long_sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 2 * 44100 * 10], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let row0 = vec![note(1, 60, 64), cell()];
    let row1 = vec![cell(), note(1, 64, 64)]; // different channel, shortly after
    let pattern = Pattern { rows: vec![row0, row1] };
    let m = module_default(vec![pattern], 2, vec![long_sample]);

    let song = compute_song_events(&m);
    let mut notes: Vec<_> = song.notes_by_sample[&1].clone();
    notes.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());

    assert_eq!(notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![60, 64]);
    assert_eq!(notes[0].start_beat, 0.0);
    assert_ne!(notes[0].voice, notes[1].voice); // separate voices instead of truncation
    assert_eq!(notes[0].voice, 0);
    assert_eq!(notes[1].voice, 1);
    // rings all the way to the end of this (very short, 2-row) song rather than being
    // clamped short to make room for notes[1] — well past notes[1]'s own start_beat
    let row_beats = 6.0 / 24.0; // speed=6
    assert!((notes[0].duration_beat - 2.0 * row_beats).abs() < 1e-9);
    assert!(notes[0].start_beat + notes[0].duration_beat > notes[1].start_beat);
}

#[test]
fn test_simultaneous_notes_each_get_their_own_voice() {
    // two channels trigger the same (looped, i.e. uncapped) sample at the exact same beat —
    // neither can ever be considered "finished" before the other starts, so they can't share
    // a voice; a third channel triggers it again later, but since the looped sample rings on
    // to the end of the song regardless, none of the earlier voices are free for it either.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    }; // has_loop=True
    let row0 = vec![note(1, 60, 64), note(1, 64, 64), cell()];
    let row1 = vec![cell(), cell(), note(1, 67, 64)];
    let pattern = Pattern { rows: vec![row0, row1] };
    let m = module_default(vec![pattern], 3, vec![looped]);

    let song = compute_song_events(&m);
    let mut notes: Vec<_> = song.notes_by_sample[&1].clone();
    notes.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());

    assert_eq!(notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![60, 64, 67]);
    let voices: std::collections::HashSet<usize> = notes.iter().map(|n| n.voice).collect();
    assert_eq!(voices, std::collections::HashSet::from([0, 1, 2])); // three fully-overlapping notes, three voices
    for tied_note in &notes[..2] {
        assert_eq!(tied_note.start_beat, 0.0);
    }
}

#[test]
fn test_c00_cuts_the_currently_held_note_dead() {
    // a looped (i.e. otherwise-unbounded) sample, held by row2's C00 (no new note) instead
    // of ringing on to the natural-length cap or the next channel event.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let kill = effect(0xC, 0);
    let empty = cell();
    let pattern = Pattern { rows: vec![vec![note_on], vec![empty.clone()], vec![kill], vec![empty.clone()], vec![empty]] };
    let m = module_default(vec![pattern], 1, vec![looped]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    let row_beats = 6.0 / 24.0;
    assert_eq!(notes[0].start_beat, 0.0);
    assert_eq!(notes[0].duration_beat, 2.0 * row_beats); // cut exactly at row 2 (the C00 row)
}

#[test]
fn test_c00_alongside_a_new_note_does_not_suppress_it() {
    // C00 combined with a *new* note on the same row is a rarer, different technique
    // (trigger silently) — must not be confused with "kill the previous note" and must not
    // crash; the new note should still appear (current behaviour: floored to velocity 1).
    let sample = Sample {
        index: 1, name: "kick".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_and_kill = note_with_effect(1, 60, 0, 0xC, 0);
    let pattern = Pattern { rows: vec![vec![note_and_kill]] };
    let m = module_default(vec![pattern], 1, vec![sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].pitch, 60);
}

#[test]
fn test_portamento_up_bends_the_held_note_without_retriggering_it() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide = effect(0x1, 48); // portamento up
    let empty = cell();
    let pattern = Pattern { rows: vec![vec![note_on], vec![slide], vec![empty]] };
    let m = module_default(vec![pattern], 1, vec![looped]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1); // no new note created
    let note = &notes[0];
    assert_eq!(note.pitch, 60); // the note's own fixed pitch never changes

    // one bend point per tick (speed=6 -> ticks 1..5), not one lump-sum jump for the row —
    // that's what makes it read as a smooth glide instead of a retrigger-like jump
    let row_beats = 6.0 / 24.0;
    let tick_beats = row_beats / 6.0;
    assert_eq!(note.bends.len(), 5);
    let expected: Vec<f64> = (1..6).map(|i| row_beats + i as f64 * tick_beats).collect();
    assert_eq!(note.bends.iter().map(|b| b.at_beat).collect::<Vec<_>>(), expected);
    assert!(note.bends.iter().all(|b| b.semitones > 0.0)); // portamento *up* bends sharp
    let mut sorted_semis: Vec<f64> = note.bends.iter().map(|b| b.semitones).collect();
    sorted_semis.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(note.bends.iter().map(|b| b.semitones).collect::<Vec<_>>(), sorted_semis); // monotonic glide
}

#[test]
fn test_portamento_moves_the_period_by_the_raw_parameter_per_tick() {
    // Reading ft2-clone's pitchSlideUp/pitchSlideDown literally (`ch->realPeriod -= param *
    // 4`) sounded noticeably too strong in Ableton; isolating a single note (period 428) and
    // measuring a real "1xy, param=2" slide's actual pitch rise via libopenmpt confirmed the
    // *raw* parameter (no ×4) matches real playback — the ft2-clone ×4 figure most likely
    // applies to FT2's own internal fine-period representation, not the plain Amiga/MOD
    // period slides this exporter works in.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide = effect(0x1, 1); // smallest possible portamento step
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![slide]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let note = &song.notes_by_sample[&1][0];

    let period_60: f64 = 428.0; // REFERENCE_PERIOD / REFERENCE_NOTE convention in export::notes
    let expected_period_after_tick1 = period_60 - 1.0;
    let expected_semitones = (60.0 - 12.0 * (expected_period_after_tick1 / period_60).log2()) - 60.0;
    assert!((note.bends[0].semitones - expected_semitones).abs() < 1e-9);
}

#[test]
fn test_portamento_down_bends_flat() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide = effect(0x2, 48); // portamento down
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![slide]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    assert!(notes[0].bends[0].semitones < 0.0);
}

#[test]
fn test_portamento_up_param_zero_repeats_the_last_nonzero_rate() {
    // "108" then repeated "100" sustains the same slide across many rows — confirmed against
    // ft2-clone's pitchSlideUp() source (`if (param == 0) param = ch->pitchSlideUpSpeed;`).
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide = effect(0x1, 48);
    let repeat = effect(0x1, 0);
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![slide], vec![repeat]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 10); // 5 ticks/row x 2 rows, the second row's "100" still bends
    assert!(bends[5].semitones > bends[4].semitones); // keeps climbing into the second row
}

#[test]
fn test_portamento_down_param_zero_repeats_the_last_nonzero_rate() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide = effect(0x2, 48);
    let repeat = effect(0x2, 0);
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![slide], vec![repeat]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 10);
    assert!(bends[5].semitones < bends[4].semitones); // keeps falling into the second row
}

#[test]
fn test_portamento_up_and_down_keep_independent_memories() {
    // FT2 remembers each *direction's* own last rate separately (pitchSlideUpSpeed vs
    // pitchSlideDownSpeed) — a "200" with no prior Portamento *Down* on this channel must not
    // pick up the rate a previous Portamento *Up* used, even though both touch the same period.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_up = effect(0x1, 48);
    let stray_down = effect(0x2, 0); // no prior Portamento *Down* to remember a rate from
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![slide_up], vec![stray_down]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 5); // only the first row's "148" bends; the stray "200" is a no-op
}

#[test]
fn test_portamento_with_no_currently_held_note_is_a_no_op() {
    let sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let slide = effect(0x1, 48); // nothing playing on this channel yet
    let m = module_default(vec![Pattern { rows: vec![vec![slide]] }], 1, vec![sample]);

    let song = compute_song_events(&m); // must not raise

    assert!(song.notes_by_sample[&1].is_empty());
}

#[test]
fn test_same_pitch_notes_still_respect_natural_length_cap_when_far_apart() {
    let short_sample = Sample {
        index: 1, name: "kick".to_string(), pcm16: vec![0u8; 4410 * 2], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    }; // 0.1s = 0.1 beat @ 60bpm
    let empty = cell();
    let note_on = note(1, 60, 64);
    let mut rows = vec![vec![note_on, empty.clone()]];
    for _ in 0..20 {
        rows.push(vec![empty.clone(), empty.clone()]);
    }
    let pattern = Pattern { rows };
    let m = module_default(vec![pattern], 2, vec![short_sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    assert!((notes[0].duration_beat - 0.1).abs() < 1e-9);
}

#[test]
fn test_set_panning_on_a_new_note() {
    let sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note_with_effect(1, 60, 64, 0x8, 255); // hard right
    let m = module_default(vec![Pattern { rows: vec![vec![note_on]] }], 1, vec![sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].pans.len(), 1);
    assert_eq!(notes[0].pans[0].at_beat, 0.0);
    assert_eq!(notes[0].pans[0].pan, 1.0);
}

#[test]
fn test_set_panning_on_a_held_note_without_retriggering() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let pan_left = effect(0x8, 0); // hard left, no new note
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![pan_left]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1); // no new note created
    assert_eq!(notes[0].pans.len(), 1);
    assert_eq!(notes[0].pans[0].pan, -1.0);
}

#[test]
fn test_set_volume_cxx_jumps_the_held_note_without_retriggering() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let set_vol = effect(0xC, 32); // half volume, no new note
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![set_vol]] }], 1, vec![looped]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].trigger_volume, 64);
    assert_eq!(notes[0].volumes.len(), 1);
    assert_eq!(notes[0].volumes[0].tracker_volume, 32);
}

#[test]
fn test_volume_slide_ramps_the_held_note_per_tick() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 32);
    let slide_down = effect(0xA, 0x04); // low nibble: slide down 4/tick
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![slide_down]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    let volumes = &notes[0].volumes;
    assert_eq!(volumes.len(), 5); // ticks 1..5 at speed=6
    assert_eq!(volumes.iter().map(|v| v.tracker_volume).collect::<Vec<_>>(), vec![28, 24, 20, 16, 12]);
}

#[test]
fn test_volume_slide_clamps_to_0_and_64() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 2);
    let slide_down = effect(0xA, 0x0F); // would go well below 0
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![slide_down]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert!(notes[0].volumes.iter().all(|v| v.tracker_volume == 0));
}

#[test]
fn test_volume_slide_first_tick_does_not_glide_but_later_ticks_do() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_down = effect(0xA, 0x04);
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![slide_down]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 5);
    assert_eq!(volumes.iter().map(|v| v.glide).collect::<Vec<_>>(), vec![false, true, true, true, true]);
}

#[test]
fn test_volume_slide_glides_across_a_row_boundary_when_reapplied_next_row() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_down = effect(0xA, 0x04);
    let m = module(
        vec![Pattern { rows: vec![vec![note_on], vec![slide_down.clone()], vec![slide_down]] }], 1, vec![looped], 6, 60,
    );

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 10); // 5 ticks/row x 2 rows
    assert_eq!(
        volumes.iter().map(|v| v.glide).collect::<Vec<_>>(),
        vec![false, true, true, true, true, true, true, true, true, true]
    );
}

#[test]
fn test_volume_slide_param_zero_repeats_the_last_nonzero_rate() {
    // "A04" then repeated "A00" is the standard tracker idiom for sustaining a fade across
    // many rows at a fixed rate — confirmed against ft2-clone's volSlide() source (`if (param
    // == 0) param = ch->volSlideSpeed;`). Treating a param=0 row as a no-op (a bug an earlier
    // version of this code had) would truncate the whole fade down to just its first row.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_down = effect(0xA, 0x04);
    let repeat = effect(0xA, 0x00);
    let m = module(
        vec![Pattern { rows: vec![vec![note_on], vec![slide_down], vec![repeat]] }], 1, vec![looped], 6, 60,
    );

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 10); // 5 ticks/row x 2 rows, the second row's A00 still slides
    assert_eq!(
        volumes.iter().map(|v| v.tracker_volume).collect::<Vec<_>>(),
        vec![60, 56, 52, 48, 44, 40, 36, 32, 28, 24] // continues at the same -4/tick rate
    );
}

#[test]
fn test_volume_slide_param_zero_with_no_prior_rate_is_a_no_op() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let stray = effect(0xA, 0x00); // no earlier Axy on this channel to remember a rate from
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![stray]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);

    assert!(song.notes_by_sample[&1][0].volumes.is_empty());
}

#[test]
fn test_volume_slide_glide_resets_after_a_gap_row_with_no_slide() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_down = effect(0xA, 0x04);
    let m = module(
        vec![Pattern { rows: vec![vec![note_on], vec![slide_down.clone()], vec![cell()], vec![slide_down]] }],
        1, vec![looped], 6, 60,
    );

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 10); // 5 ticks/row x 2 slide rows (the gap row emits none)
    assert_eq!(
        volumes.iter().map(|v| v.glide).collect::<Vec<_>>(),
        vec![false, true, true, true, true, false, true, true, true, true]
    );
}

#[test]
fn test_set_volume_is_never_a_glide() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let set_volume = effect(0xC, 16);
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![set_volume]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 1);
    assert!(!volumes[0].glide);
}

#[test]
fn test_volume_slide_applies_even_on_the_row_that_triggers_the_note() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_and_slide = note_with_effect(1, 60, 64, 0xA, 0x04);
    let m = module(vec![Pattern { rows: vec![vec![note_and_slide]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 5); // ticks 1..5 at speed=6
    assert_eq!(volumes.iter().map(|v| v.tracker_volume).collect::<Vec<_>>(), vec![60, 56, 52, 48, 44]);
}

#[test]
fn test_a_retriggered_note_starts_its_volume_slide_as_a_step_not_a_glide() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_a = note(1, 60, 64);
    let slide = effect(0xA, 0x04);
    let note_b = note_with_effect(1, 64, 64, 0xA, 0x04);
    let m = module(vec![Pattern { rows: vec![vec![note_a], vec![slide], vec![note_b]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 2);
    assert!(!notes[1].volumes[0].glide);
}

#[test]
fn test_arpeggio_cycles_base_x_and_y_semitones_per_tick() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let arp = effect(0x0, 0x51); // x=5, y=1
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![arp]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 5);
    assert_eq!(bends.iter().map(|b| b.semitones.round() as i32).collect::<Vec<_>>(), vec![1, 5, 0, 1, 5]);
    assert!(bends.iter().all(|b| !b.glide)); // always a discrete jump, never smoothed
}

#[test]
fn test_arpeggio_applies_even_on_the_row_that_triggers_the_note() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_with_arp = note_with_effect(1, 60, 64, 0x0, 0x51);
    let m = module(vec![Pattern { rows: vec![vec![note_with_arp]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.iter().map(|b| b.semitones.round() as i32).collect::<Vec<_>>(), vec![1, 5, 0, 1, 5]);
}

#[test]
fn test_vibrato_oscillates_and_uses_depth_speed_from_the_nibbles() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let vibrato = effect(0x4, 0x38); // speed nibble=3 (->12/tick), depth nibble=8
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![vibrato]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 5);
    // a real oscillation, not a flat line or a monotonic slide
    assert_ne!(bends[0].semitones, bends[bends.len() - 1].semitones);
    assert!(bends.iter().any(|b| b.semitones < 0.0));
    assert!(!bends[0].glide); // first tick: a jump away from the note's fixed pitch
    assert!(bends[1..].iter().all(|b| b.glide)); // later ticks: a continuous oscillation
}

#[test]
fn test_vibrato_depth_and_speed_persist_when_param_is_zero() {
    // ft2-clone's vibrato(): a bare "400" keeps the previously-set depth/speed and keeps
    // oscillating — it does not stop or reset, unlike Volume Slide/Portamento's param=0.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let vibrato = effect(0x4, 0x38);
    let continue_vibrato = effect(0x4, 0x00);
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![vibrato], vec![continue_vibrato]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 10); // 5 ticks/row x 2 vibrato rows, continuation included
    assert!(bends[5].glide); // continues smoothly across the row boundary
}

#[test]
fn test_vibrato_position_resets_on_a_new_note_trigger() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_a = note(1, 60, 64);
    let vibrato = effect(0x4, 0x38);
    let note_b = note(1, 64, 64); // retrigger before the vibrato finishes
    let m = module(
        vec![Pattern { rows: vec![vec![note_a], vec![vibrato.clone()], vec![note_b], vec![vibrato]] }], 1, vec![looped], 6, 60,
    );

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];
    assert_eq!(notes.len(), 2);

    // both notes' vibrato starts from the same phase (tick 1 at a freshly-reset position) —
    // if the position hadn't reset, note_b's vibrato would start from a much later phase in
    // the cycle (having kept accumulating through note_a's own vibrato), giving a value well
    // outside this tolerance.
    assert!((notes[1].bends[0].semitones - (-0.2342)).abs() < 1e-3);
}

#[test]
fn test_a_retriggered_note_starts_its_vibrato_as_a_step_not_a_glide_from_the_previous_note() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_a = note(1, 60, 64);
    let vibrato = effect(0x4, 0x38);
    let note_b = note_with_effect(1, 64, 64, 0x4, 0x38); // retrigger + vibrato, same row
    let m = module(vec![Pattern { rows: vec![vec![note_a], vec![vibrato], vec![note_b]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 2);
    assert!(!notes[1].bends[0].glide); // a clean step into the new note, not a bleed-in
}

#[test]
fn test_vibrato_plus_volume_slide_applies_both_without_touching_vibrato_params() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let vibrato = effect(0x4, 0x38);
    let combo = effect(0x6, 0x04); // volume slide down 4/tick
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![vibrato], vec![combo]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let note = &song.notes_by_sample[&1][0];

    assert_eq!(note.bends.len(), 10); // vibrato keeps running across both rows
    assert_eq!(note.volumes.len(), 5); // volume slide only runs during the combo row
    assert_eq!(note.volumes.iter().map(|v| v.tracker_volume).collect::<Vec<_>>(), vec![60, 56, 52, 48, 44]);
}

#[test]
fn test_tone_portamento_slides_toward_the_target_without_retriggering() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_to = Cell { midi_note: Some(67), effect: Some(0x3), effect_param: Some(0x02), ..Default::default() }; // target B, speed nibble 2 -> 8/tick
    let m = module(vec![Pattern { rows: vec![vec![note_on], vec![slide_to]] }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1); // the target-note row never triggers a second note
    let bends = &notes[0].bends;
    assert_eq!(bends.len(), 5);
    // monotonically approaching +7 semitones (67 - 60), never overshooting
    let values: Vec<f64> = bends.iter().map(|b| b.semitones).collect();
    let mut sorted_values = values.clone();
    sorted_values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(values, sorted_values);
    assert!(values.iter().all(|&v| v <= 7.0 + 1e-6));
    assert!(!bends[0].glide);
}

#[test]
fn test_tone_portamento_stops_exactly_at_the_target_and_holds() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide_to = Cell { midi_note: Some(67), effect: Some(0x3), effect_param: Some(0x02), ..Default::default() };
    let continue_slide = effect(0x3, 0x00); // 0 = keep the previous speed (memory)
    let mut rows = vec![vec![note_on], vec![slide_to]];
    for _ in 0..20 {
        rows.push(vec![continue_slide.clone()]);
    }
    let m = module(vec![Pattern { rows }], 1, vec![looped], 6, 60);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    let last = bends.len() - 1;
    assert_eq!((bends[last].semitones * 1e6).round() / 1e6, 7.0); // reached exactly, matching midi_note 67 - 60
    assert_eq!((bends[last - 1].semitones * 1e6).round() / 1e6, 7.0); // and holds there, doesn't overshoot/oscillate
}

#[test]
fn test_tone_portamento_with_nothing_currently_held_is_a_no_op() {
    let sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let slide_to = Cell { midi_note: Some(67), effect: Some(0x3), effect_param: Some(0x02), ..Default::default() }; // nothing playing on this channel yet
    let m = module(vec![Pattern { rows: vec![vec![slide_to]] }], 1, vec![sample], 6, 60);

    let song = compute_song_events(&m); // must not raise

    assert!(song.notes_by_sample[&1].is_empty());
}

fn looped_pad() -> Sample {
    Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    }
}

#[test]
fn test_tone_portamento_plus_volslide_continues_the_glide_and_slides_volume() {
    let note_on = note(1, 60, 40);
    let slide_to = Cell { midi_note: Some(67), effect: Some(0x3), effect_param: Some(0x02), ..Default::default() };
    let combo = effect(0x5, 0x04); // continues the existing 3xy glide, slides volume down 4/tick
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![slide_to], vec![combo]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let note = &song.notes_by_sample[&1][0];

    assert_eq!(note.bends.len(), 10); // 5 ticks/row x 2 rows (row2's 3xy, row3's 5xy)
    assert!(note.bends[5].semitones > note.bends[4].semitones); // still climbing toward 67 in row3

    assert_eq!(note.volumes.len(), 5); // only row3 (5xy) produces volume points
    assert_eq!(note.volumes[0].tracker_volume, 36); // 40 - 4, row3's own param
}

#[test]
fn test_vibrato_plus_volslide_param_zero_repeats_the_last_nonzero_rate() {
    // "601" then repeated "600" is the standard idiom for sustaining a fade across many rows
    // — 6xy shares Axy's exact same volSlide() call (and its param=0 memory) in ft2-clone;
    // this previously required a nonzero param every row, breaking that idiom the same way
    // the Axy/1xx/2xx param=0 bug once did.
    let note_on = note(1, 60, 64);
    let combo = effect(0x6, 0x04); // vibrato (no depth/speed set yet) + volslide -4/tick
    let repeat = effect(0x6, 0x00); // param=0 -> reuse the -4/tick rate
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![combo], vec![repeat]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 10); // 5 ticks/row x 2 rows, the second row's "600" still slides
    assert_eq!(
        volumes.iter().map(|v| v.tracker_volume).collect::<Vec<_>>(),
        vec![60, 56, 52, 48, 44, 40, 36, 32, 28, 24]
    );
}

#[test]
fn test_tremolo_oscillates_using_depth_speed_from_the_nibbles() {
    let note_on = note(1, 60, 40);
    let trem = effect(0x7, 0x28); // speed nibble=2 (*4=8), depth nibble=8
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![trem]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 5); // ticks 1..5 at speed=6
    // early ticks (small phase into the sine cycle) swing the volume *up* from the trigger
    // baseline — sin(2*pi*phase) is positive for phase in (0, 0.5), and tremoloPos is still
    // well short of half a cycle after only 5 ticks at this speed.
    assert!(volumes.iter().all(|v| v.tracker_volume >= 40));
    assert!(volumes.iter().any(|v| v.tracker_volume > 40));
}

#[test]
fn test_tremolo_never_touches_the_persistent_volume_baseline() {
    // unlike Volume Slide, Tremolo's oscillation is transient (ft2-clone's tremolo() only
    // ever touches outVol, never realVol) — a Volume Slide right after a Tremolo run must
    // continue from the note's *original* trigger volume, not wherever the last tremolo tick
    // happened to land.
    let note_on = note(1, 60, 40);
    let trem = effect(0x7, 0x28);
    let slide_down = effect(0xA, 0x04); // -4/tick
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![trem], vec![slide_down]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 10); // 5 tremolo ticks (row2) + 5 slide ticks (row3)
    assert_eq!(volumes[5].tracker_volume, 36); // 40 - 4, not derived from tremolo's last tick
}

#[test]
fn test_sample_offset_sets_the_new_notes_start_position() {
    let long_sample = Sample { pcm16: vec![0u8; 10000], ..looped_pad() }; // 5000 frames
    let note_on = note_with_effect(1, 60, 64, 0x9, 0x04); // offset = 4*256 = 1024 frames
    let m = module_default(vec![Pattern { rows: vec![vec![note_on]] }], 1, vec![long_sample]);

    let song = compute_song_events(&m);
    assert_eq!(song.notes_by_sample[&1][0].sample_offset_frames, 1024);
}

#[test]
fn test_sample_offset_param_zero_repeats_the_last_nonzero_offset() {
    let long_sample = Sample { pcm16: vec![0u8; 10000], ..looped_pad() };
    let note1 = note_with_effect(1, 60, 64, 0x9, 0x04); // 1024
    let note2 = note_with_effect(1, 64, 64, 0x9, 0x00); // reuse -> 1024
    let m = module_default(vec![Pattern { rows: vec![vec![note1], vec![note2]] }], 1, vec![long_sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];
    assert_eq!(notes[0].sample_offset_frames, 1024);
    assert_eq!(notes[1].sample_offset_frames, 1024);
}

#[test]
fn test_a_note_without_sample_offset_always_starts_at_zero_regardless_of_earlier_memory() {
    let long_sample = Sample { pcm16: vec![0u8; 10000], ..looped_pad() };
    let note1 = note_with_effect(1, 60, 64, 0x9, 0x04); // 1024
    let note2 = note(1, 64, 64); // plain note, no 9xx at all on this row
    let m = module_default(vec![Pattern { rows: vec![vec![note1], vec![note2]] }], 1, vec![long_sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];
    assert_eq!(notes[0].sample_offset_frames, 1024);
    assert_eq!(notes[1].sample_offset_frames, 0); // no 9xx here -> always starts at frame 0
}

#[test]
fn test_sample_offset_is_clamped_to_the_samples_own_length() {
    let short = Sample { pcm16: vec![0u8; 20], loop_length: 0, ..looped_pad() }; // 10 frames, non-looped
    let note_on = note_with_effect(1, 60, 64, 0x9, 0x04); // 1024 frames requested, far past the sample's own 10
    let m = module_default(vec![Pattern { rows: vec![vec![note_on]] }], 1, vec![short]);

    let song = compute_song_events(&m);
    assert_eq!(song.notes_by_sample[&1][0].sample_offset_frames, 9); // clamped to num_frames - 1
}

#[test]
fn test_notes_with_different_sample_offsets_get_separate_voices_even_without_time_overlap() {
    let long_sample = Sample { pcm16: vec![0u8; 10000], ..looped_pad() };
    let empty = cell();
    let note1 = note_with_effect(1, 60, 64, 0x9, 0x04); // offset 1024
    let mut rows = vec![vec![note1]];
    for _ in 0..20 {
        rows.push(vec![empty.clone()]);
    }
    rows.push(vec![note(1, 64, 64)]); // no offset, long after note1 — would share a voice if
                                       // only time-overlap mattered, since channels are
                                       // monophonic and this is well clear of note1
    let m = module_default(vec![Pattern { rows }], 1, vec![long_sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];
    assert_eq!(notes.len(), 2);
    assert_ne!(notes[0].voice, notes[1].voice); // different offsets -> different voices, always
}

#[test]
fn test_fine_portamento_up_nudges_the_period_once_at_the_start_of_the_row() {
    let note_on = note(1, 60, 64);
    let fine_up = effect(0xE, 0x12); // E1x, param=2
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![fine_up]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 1); // one-shot, not one point per tick like the ordinary 1xx
    assert_eq!(bends[0].at_beat, 6.0 / 24.0); // right at the row's own start
    assert!(bends[0].semitones > 0.0); // fine portamento *up* bends sharp
    assert!(!bends[0].glide); // an instantaneous nudge, not a glide
}

#[test]
fn test_fine_portamento_down_bends_flat() {
    let note_on = note(1, 60, 64);
    let fine_down = effect(0xE, 0x22); // E2x, param=2
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![fine_down]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 1);
    assert!(bends[0].semitones < 0.0);
}

#[test]
fn test_fine_portamento_param_zero_repeats_the_last_nonzero_rate() {
    let note_on = note(1, 60, 64);
    let fine_up = effect(0xE, 0x14); // E1x param 4
    let repeat = effect(0xE, 0x10); // E10 -> reuse the previous rate
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![fine_up], vec![repeat]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let bends = &song.notes_by_sample[&1][0].bends;

    assert_eq!(bends.len(), 2); // one nudge per row, including the "repeat" row
    assert!(bends[1].semitones > bends[0].semitones); // second nudge kept climbing, wasn't a no-op
}

#[test]
fn test_retrigger_note_creates_a_new_note_every_param_ticks() {
    let note_on = note(1, 60, 64);
    let retrig = effect(0xE, 0x92); // E9x, param=2 -> retrigger every 2 ticks
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![retrig]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    // speed=6 -> ticks 1..5; retriggers at tick=2 and tick=4 (both divisible by 2)
    assert_eq!(notes.len(), 3); // original trigger + 2 retriggers
    let row_beats = 6.0 / 24.0;
    let tick_beats = row_beats / 6.0;
    assert_eq!(notes[1].start_beat, row_beats + 2.0 * tick_beats);
    assert_eq!(notes[2].start_beat, row_beats + 4.0 * tick_beats);
    assert!(notes.iter().all(|n| n.pitch == 60));
}

#[test]
fn test_retrigger_param_zero_is_a_no_op() {
    let note_on = note(1, 60, 64);
    let retrig0 = effect(0xE, 0x90); // E90
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![retrig0]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    assert_eq!(song.notes_by_sample[&1].len(), 1); // no retrigger happened
}

#[test]
fn test_fine_volume_slide_up_bumps_volume_once_at_the_start_of_the_row() {
    let note_on = note(1, 60, 20);
    let fine_up = effect(0xE, 0xA5); // EAx, param=5
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![fine_up]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].tracker_volume, 25); // 20 + 5, one-shot
    assert!(!volumes[0].glide);
}

#[test]
fn test_fine_volume_slide_down_and_memory() {
    let note_on = note(1, 60, 40);
    let fine_down = effect(0xE, 0xB5); // EBx param 5
    let repeat = effect(0xE, 0xB0); // reuse -> 5 again
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![fine_down], vec![repeat]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 2);
    assert_eq!(volumes[0].tracker_volume, 35); // 40 - 5
    assert_eq!(volumes[1].tracker_volume, 30); // 35 - 5, memory reused
}

#[test]
fn test_note_cut_silences_at_the_given_tick() {
    let note_on = note(1, 60, 64);
    let cut = effect(0xE, 0xC3); // ECx param 3 -> silence at tick 3
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![cut]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].tracker_volume, 0);
    let row_beats = 6.0 / 24.0;
    let tick_beats = row_beats / 6.0;
    assert_eq!(volumes[0].at_beat, row_beats + 3.0 * tick_beats);
}

#[test]
fn test_note_cut_zero_silences_immediately_at_the_row_start() {
    let note_on = note(1, 60, 64);
    let cut0 = effect(0xE, 0xC0);
    let m = module_default(vec![Pattern { rows: vec![vec![note_on], vec![cut0]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let volumes = &song.notes_by_sample[&1][0].volumes;

    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].tracker_volume, 0);
    assert_eq!(volumes[0].at_beat, 6.0 / 24.0); // exactly at the row's own start
}

#[test]
fn test_note_delay_triggers_the_note_at_the_given_tick_not_the_row_start() {
    // also exercises the very first note ever played on a channel being delayed (nothing was
    // held before it) — Note Delay must not depend on an already-held note to work.
    let delayed = note_with_effect(1, 60, 64, 0xE, 0xD3); // EDx param 3
    let m = module_default(vec![Pattern { rows: vec![vec![delayed]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    let row_beats = 6.0 / 24.0;
    let tick_beats = row_beats / 6.0;
    assert_eq!(notes[0].start_beat, 3.0 * tick_beats); // delayed to tick 3, not the row start
}

#[test]
fn test_note_delay_zero_triggers_normally_at_the_row_start() {
    let not_delayed = note_with_effect(1, 60, 64, 0xE, 0xD0); // ED0 -> not delayed at all
    let m = module_default(vec![Pattern { rows: vec![vec![not_delayed]] }], 1, vec![looped_pad()]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].start_beat, 0.0);
}

fn enveloped_pad(volume_envelope: Option<Envelope>, panning_envelope: Option<Envelope>) -> Sample {
    Sample { volume_envelope, panning_envelope, ..looped_pad() }
}

#[test]
fn test_volume_envelope_attack_points_are_emitted_at_trigger_and_freeze_at_the_sustain_point() {
    // decay 64 -> 32 -> 0, sustaining at point 1 (value 32) — point 2 (tick 20, value 0) is
    // the release-only tail and must NOT be emitted yet, since the note is never released here.
    let env = Envelope {
        points: vec![
            EnvelopePoint { tick: 0, value: 64 },
            EnvelopePoint { tick: 5, value: 32 },
            EnvelopePoint { tick: 20, value: 0 },
        ],
        sustain_point: Some(1),
        loop_start_point: None,
        loop_end_point: None,
    };
    let sample = enveloped_pad(Some(env), None);
    let m = module_default(vec![Pattern { rows: vec![vec![note(1, 60, 64)]] }], 1, vec![sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];
    assert_eq!(notes.len(), 1);

    let tick_beats = (6.0 / 24.0) / 6.0; // speed=6
    let volumes: Vec<(f64, i32, bool)> = notes[0].envelope_volumes.iter().map(|v| (v.at_beat, v.tracker_volume, v.glide)).collect();
    // One point per *tick* from 0 to the sustain point (not just the two defined breakpoints) —
    // Ableton's own automation playback doesn't interpolate linearly in the stored gain value
    // between widely-spaced points (see envelope_attack_points's own doc comment), so the raw
    // 0-64 envelope value is linearly interpolated here, per tick, instead.
    let expected: Vec<(f64, i32, bool)> = (0..=5)
        .map(|tick| {
            let value = (64.0 + tick as f64 / 5.0 * (32.0 - 64.0)).round() as i32;
            (tick as f64 * tick_beats, value, tick != 0)
        })
        .collect();
    assert_eq!(volumes, expected);
}

#[test]
fn test_key_off_marks_release_beat_without_ending_the_note() {
    let sample = looped_pad(); // no envelope: note-off should be a pure no-op on playback
    let pattern = Pattern { rows: vec![vec![note(1, 60, 64)], vec![key_off()], vec![cell()]] };
    let m = module_default(vec![pattern], 1, vec![sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1); // key off did not close/retrigger the note
    let row_beats = 6.0 / 24.0;
    assert_eq!(notes[0].release_beat, Some(row_beats));
    assert!(notes[0].volumes.is_empty() && notes[0].envelope_volumes.is_empty()); // no envelope on this sample: nothing to automate
    // still rings on to the end of the (3-row) song, same as if key off had never happened
    assert_eq!(notes[0].duration_beat, 3.0 * row_beats);
}

#[test]
fn test_key_off_triggers_the_envelopes_release_segment_at_the_current_tick_rate() {
    let env = Envelope {
        points: vec![
            EnvelopePoint { tick: 0, value: 64 },
            EnvelopePoint { tick: 5, value: 32 }, // sustain
            EnvelopePoint { tick: 15, value: 0 }, // release-only tail
        ],
        sustain_point: Some(1),
        loop_start_point: None,
        loop_end_point: None,
    };
    let sample = enveloped_pad(Some(env), None);
    let pattern = Pattern { rows: vec![vec![note(1, 60, 64)], vec![key_off()], vec![cell()]] };
    let m = module_default(vec![pattern], 1, vec![sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];
    assert_eq!(notes.len(), 1);

    let row_beats = 6.0 / 24.0;
    let tick_beats = row_beats / 6.0;
    let release_beat = row_beats; // key off is on row 1
    let volumes: Vec<(f64, i32, bool)> = notes[0].envelope_volumes.iter().map(|v| (v.at_beat, v.tracker_volume, v.glide)).collect();
    // Attack: one point per tick from 0 to the sustain point (tick 5). Release: one point per
    // tick from the sustain point to the release-only tail's tick (15), anchored at the key off
    // beat and offset from the sustain point's own tick — see envelope_attack_points's doc
    // comment for why this is densified to one point per tick rather than just the two defined
    // breakpoints at each end.
    let mut expected: Vec<(f64, i32, bool)> = (0..=5)
        .map(|tick| {
            let value = (64.0 + tick as f64 / 5.0 * (32.0 - 64.0)).round() as i32;
            (tick as f64 * tick_beats, value, tick != 0)
        })
        .collect();
    expected.extend((6..=15).map(|tick| {
        let value = (32.0 + (tick - 5) as f64 / 10.0 * (0.0 - 32.0)).round() as i32;
        (release_beat + (tick - 5) as f64 * tick_beats, value, true)
    }));
    assert_eq!(volumes, expected);
}

#[test]
fn test_a_second_key_off_on_an_already_released_note_does_not_duplicate_the_release_segment() {
    let env = Envelope {
        points: vec![EnvelopePoint { tick: 0, value: 64 }, EnvelopePoint { tick: 10, value: 0 }],
        sustain_point: Some(0),
        loop_start_point: None,
        loop_end_point: None,
    };
    let sample = enveloped_pad(Some(env), None);
    let pattern = Pattern { rows: vec![vec![note(1, 60, 64)], vec![key_off()], vec![key_off()]] };
    let m = module_default(vec![pattern], 1, vec![sample]);

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    let row_beats = 6.0 / 24.0;
    assert_eq!(notes[0].release_beat, Some(row_beats)); // the *first* key off, not the second
    // 1 attack point (sustain is tick 0, immediately) + one release point per tick from 1 to 10
    // (not duplicated by the second key off).
    assert_eq!(notes[0].envelope_volumes.len(), 1 + 10);
}

#[test]
fn test_portamento_up_uses_linear_semitone_steps_under_the_xm_linear_frequency_table() {
    // Regression test for a real bug report: Portamento Up on an XM file (which nearly always
    // declares the linear frequency table) reached an absurd, rapidly-accelerating pitch over
    // just a few rows when this reused MOD's own *logarithmic* Amiga-period math unconditionally
    // — a fixed period delta corresponds to an ever-larger semitone jump as an exponential
    // period shrinks, so the climb runs away instead of staying steady. Real XM playback is
    // linear instead: a constant param produces a constant semitones/tick rate throughout.
    //
    // The exact expected rate (1.75 semitones/tick for param=0x1C=28, i.e. param*4/64) isn't a
    // guess — it's the rate actually measured from a synthetic XM file rendered through
    // libopenmpt (`openmpt123 --render`) and analyzed via FFT: a rock-steady 1.75 semitones per
    // tick from the very first tick onward (see notes.rs's own LINEAR_PERIOD_UNITS_PER_SEMITONE
    // doc comment for the full methodology).
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let note_on = note(1, 60, 64);
    let slide = effect(0x1, 0x1C); // portamento up, param 28
    let empty = cell();
    let pattern = Pattern { rows: vec![vec![note_on], vec![slide], vec![empty]] };
    let m = Module { linear_frequency_table: true, source_format: "fasttracker2".to_string(), ..module_default(vec![pattern], 1, vec![looped]) };

    let song = compute_song_events(&m);
    let notes = &song.notes_by_sample[&1];

    assert_eq!(notes.len(), 1);
    let bends = &notes[0].bends;
    assert_eq!(bends.len(), 5); // speed=6 -> ticks 1..5

    let semitones_per_tick = 0x1C as f64 * 4.0 / 64.0; // == 1.75, matches the real-playback measurement
    for (i, bend) in bends.iter().enumerate() {
        let expected = semitones_per_tick * (i + 1) as f64;
        assert!((bend.semitones - expected).abs() < 1e-9, "tick {i}: got {}, expected {expected}", bend.semitones);
    }
}


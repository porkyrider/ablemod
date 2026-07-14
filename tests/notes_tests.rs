use ablemod::export::notes::{compute_song_events, NOTE_GAP_BEATS};
use ablemod::formats::base::{Cell, Module, Pattern, Sample};

fn module(patterns: Vec<Pattern>, num_channels: usize, samples: Vec<Sample>, speed: u32, bpm: u32) -> Module {
    let n = patterns.len();
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels, samples, patterns,
        order: (0..n as u32).collect(), restart_position: 0, initial_tempo_bpm: bpm, initial_speed_ticks: speed,
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
        effect: Some(effect), effect_param: Some(param),
    }
}

#[test]
fn test_note_never_overlaps_the_next_note_on_the_same_sample_across_channels() {
    // a very long natural duration (10s @ 60bpm = 10 beats) so it would otherwise ring
    // well past the next trigger on the *other* channel sharing this sample.
    let long_sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 2 * 44100 * 10], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60,
    };
    let row0 = vec![note(1, 60, 64), cell()];
    let row1 = vec![cell(), note(1, 64, 64)]; // different channel, shortly after
    let pattern = Pattern { rows: vec![row0, row1] };
    let m = module_default(vec![pattern], 2, vec![long_sample]);

    let song = compute_song_events(&m);
    let mut notes: Vec<_> = song.notes_by_sample[&1].clone();
    notes.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());

    assert_eq!(notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![60, 64]);
    let row_beats = 6.0 / 24.0; // speed=6
    assert_eq!(notes[0].start_beat, 0.0);
    // clamped to just before the next note's start (not the 10-beat natural length), with a
    // small explicit gap so it's strictly stopped, not merely touching
    assert_eq!(notes[0].duration_beat, row_beats - NOTE_GAP_BEATS);
    assert!(notes[0].start_beat + notes[0].duration_beat < notes[1].start_beat);
}

#[test]
fn test_simultaneous_notes_dont_reintroduce_overlap_with_a_later_note() {
    // two channels trigger the same (looped, i.e. uncapped) sample at the exact same beat;
    // a third channel triggers it again later. The tied pair must not clamp to zero-then-
    // floor-back-up into overlapping that later note.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
    }; // has_loop=True
    let row0 = vec![note(1, 60, 64), note(1, 64, 64), cell()];
    let row1 = vec![cell(), cell(), note(1, 67, 64)];
    let pattern = Pattern { rows: vec![row0, row1] };
    let m = module_default(vec![pattern], 3, vec![looped]);

    let song = compute_song_events(&m);
    let mut notes: Vec<_> = song.notes_by_sample[&1].clone();
    notes.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());

    assert_eq!(notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![60, 64, 67]);
    let row_beats = 6.0 / 24.0;
    for tied_note in &notes[..2] {
        assert_eq!(tied_note.start_beat, 0.0);
        assert_eq!(tied_note.duration_beat, row_beats - NOTE_GAP_BEATS);
        assert!(tied_note.start_beat + tied_note.duration_beat < notes[2].start_beat);
    }
}

#[test]
fn test_c00_cuts_the_currently_held_note_dead() {
    // a looped (i.e. otherwise-unbounded) sample, held by row2's C00 (no new note) instead
    // of ringing on to the natural-length cap or the next channel event.
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
fn test_portamento_with_no_currently_held_note_is_a_no_op() {
    let sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
fn test_volume_slide_glide_resets_after_a_gap_row_with_no_slide() {
    let looped = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
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
        loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60,
    };
    let slide_to = Cell { midi_note: Some(67), effect: Some(0x3), effect_param: Some(0x02), ..Default::default() }; // nothing playing on this channel yet
    let m = module(vec![Pattern { rows: vec![vec![slide_to]] }], 1, vec![sample], 6, 60);

    let song = compute_song_events(&m); // must not raise

    assert!(song.notes_by_sample[&1].is_empty());
}

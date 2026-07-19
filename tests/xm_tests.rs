mod common;

use ablemod::formats::fasttracker2::parse;
use common::build_minimal_xm;

#[test]
fn test_parses_synthetic_minimal_xm() {
    let module = parse(&build_minimal_xm());

    assert_eq!(module.title, "TESTXM");
    assert_eq!(module.source_format, "fasttracker2");
    assert_eq!(module.num_channels, 4);
    assert_eq!(module.order, vec![0]);
    assert_eq!(module.patterns.len(), 1);
    assert_eq!(module.patterns[0].num_rows(), 2); // proves rows-per-pattern isn't hardcoded to 64

    assert_eq!(module.samples.len(), 1);
    let kick = &module.samples[0];
    assert_eq!(kick.name, "kick");
    assert_eq!(kick.volume, 64);
    assert!(!kick.is_empty());
    assert_eq!(kick.pcm16.len(), 8 * 2); // 8 frames, 16-bit, proves delta-decoding round-trips
    let samples_i16: Vec<i16> = kick.pcm16.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let expected: Vec<i16> = [0i8, 40, 80, 120, 127, 120, 80, 40].iter().map(|&v| (v as i16) * 256).collect();
    assert_eq!(samples_i16, expected);

    assert!(kick.volume_envelope.is_some());
    let env = kick.volume_envelope.as_ref().unwrap();
    assert_eq!(env.points.len(), 2);
    assert_eq!((env.points[0].tick, env.points[0].value), (0, 64));
    assert_eq!((env.points[1].tick, env.points[1].value), (10, 0));
    assert!(kick.panning_envelope.is_none()); // "on" flag was 0

    let triggered_cell = &module.patterns[0].rows[0][0];
    assert_eq!(triggered_cell.sample_index, Some(1));
    assert_eq!(triggered_cell.midi_note, Some(60)); // xm note 49 ("C-4") + 11 == MIDI 60
    assert_eq!(triggered_cell.volume, Some(48)); // 0x40 - 0x10
    assert!(!triggered_cell.note_off);

    let effect_cell = &module.patterns[0].rows[0][1];
    assert_eq!(effect_cell.sample_index, None);
    assert_eq!(effect_cell.effect, Some(0xA));
    assert_eq!(effect_cell.effect_param, Some(0x04));

    let key_off_cell = &module.patterns[0].rows[1][0];
    assert!(key_off_cell.note_off);
    assert_eq!(key_off_cell.midi_note, None);
    assert_eq!(key_off_cell.sample_index, None);
}

#[test]
fn test_parses_real_world_xm() {
    let data = std::fs::read("tests/fixtures/20th Anniversary.xm").unwrap();
    let module = parse(&data);

    assert_eq!(module.title, "20th anniversary.");
    assert_eq!(module.source_format, "fasttracker2");
    assert_eq!(module.num_channels, 32);
    assert_eq!(module.patterns.len(), 31);
    assert_eq!(module.order.len(), 33);
    assert_eq!(module.restart_position, 0);

    // 14 instruments actually carry samples, 2 of them (multisample, keymap-split
    // instruments) contribute 2 raw samples each that are both genuinely reachable in the
    // song — a parser that only ever resolved "the first sample of each instrument" could
    // never exceed 14 non-empty entries here.
    assert!(module.samples.iter().all(|s| !s.is_empty()));
    assert_eq!(module.samples.len(), 15);

    // Both raw samples of the multisample instrument at index 3 (0-based) are present and
    // resolved as their own distinct Module.samples entries (verified against the real file's
    // own bytes: lengths 19837/6238 frames, volumes 54/64 — see this module's own byte-level
    // verification notes in the implementation plan).
    let split_a = module.samples.iter().find(|s| s.volume == 54 && s.pcm16.len() / 2 == 19837);
    let split_b = module.samples.iter().find(|s| s.volume == 64 && s.pcm16.len() / 2 == 6238);
    assert!(split_a.is_some(), "first sample of the multisample instrument not resolved");
    assert!(split_b.is_some(), "second sample of the multisample instrument not resolved");

    let has_note_off = module.patterns.iter().any(|p| p.rows.iter().any(|row| row.iter().any(|c| c.note_off)));
    assert!(has_note_off, "expected at least one Key Off cell in a 33-position song");
}

mod common;

use ablemod::formats::protracker::parse;
use common::build_minimal_mod;

#[test]
fn test_parses_synthetic_minimal_mod() {
    let module = parse(&build_minimal_mod(None));

    assert_eq!(module.title, "TESTMOD");
    assert_eq!(module.source_format, "protracker");
    assert_eq!(module.num_channels, 4);
    assert_eq!(module.samples.len(), 31);
    assert_eq!(module.order, vec![0]);
    assert_eq!(module.patterns.len(), 1);

    let kick = &module.samples[0];
    assert_eq!(kick.name, "kick");
    assert_eq!(kick.volume, 64);
    assert!(!kick.is_empty());
    assert_eq!(kick.pcm16.len(), 8 * 2); // 8 frames, 16-bit

    for other in &module.samples[1..] {
        assert!(other.is_empty());
    }

    let first_cell = &module.patterns[0].rows[0][0];
    assert_eq!(first_cell.sample_index, Some(1));
    assert_eq!(first_cell.midi_note, Some(60)); // period 428 == "C-2" == our MIDI 60 convention
}

#[test]
fn test_parses_real_world_legacy_15_sample_mod() {
    let data = std::fs::read("tests/fixtures/4aces-high.mod").unwrap();
    let module = parse(&data);

    assert_eq!(module.title, "4aces-high");
    assert_eq!(module.num_channels, 4);
    assert_eq!(module.samples.len(), 15);
    assert_eq!(module.patterns.len(), 14);
    assert_eq!(module.order, vec![1, 0, 2, 3, 4, 5, 6, 7, 8, 9, 4, 5, 10, 11, 12, 13, 13]);
    assert_eq!(module.restart_position, 120);

    let non_empty: Vec<_> = module.samples.iter().filter(|s| !s.is_empty()).collect();
    assert_eq!(non_empty.len(), 10);
    assert_eq!(non_empty[0].name, "bsnare");

    let luftstring = module.samples.iter().find(|s| s.name == "luftstring").unwrap();
    assert!(luftstring.has_loop());
    assert_eq!(luftstring.loop_start, 112);
    assert_eq!(luftstring.loop_length, 8472);

    let bsnare = module.samples.iter().find(|s| s.name == "bsnare").unwrap();
    assert!(!bsnare.has_loop());
}

use ablemod::formats::base::{Cell, Module, Pattern};
use ablemod::formats::playback::iter_song_rows;

fn module(patterns: Vec<Pattern>, order: Vec<u32>) -> Module {
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1,
        samples: vec![], patterns, order, restart_position: 0, initial_tempo_bpm: 125, initial_speed_ticks: 6,
    }
}

fn pattern(num_rows: usize, effects_by_row: &[(usize, u32, u32)]) -> Pattern {
    let mut rows = Vec::with_capacity(num_rows);
    for r in 0..num_rows {
        let found = effects_by_row.iter().find(|(row, _, _)| *row == r);
        let cell = match found {
            Some((_, effect, param)) => Cell { effect: Some(*effect), effect_param: Some(*param), ..Default::default() },
            None => Cell::default(),
        };
        rows.push(vec![cell]);
    }
    Pattern { rows }
}

#[test]
fn test_pattern_break_skips_remaining_rows() {
    let pattern0 = pattern(64, &[(2, 0xD, 0x00)]); // break at row 2, jump to row 0 of next position
    let pattern1 = pattern(4, &[]);
    let m = module(vec![pattern0, pattern1], vec![0, 1]);

    let seq: Vec<(usize, usize, usize)> = iter_song_rows(&m).iter().map(|r| (r.order_pos, r.pattern_index, r.row_index)).collect();

    assert_eq!(seq, vec![(0, 0, 0), (0, 0, 1), (0, 0, 2), (1, 1, 0), (1, 1, 1), (1, 1, 2), (1, 1, 3)]);
}

#[test]
fn test_pattern_break_bcd_target_row() {
    let pattern0 = pattern(5, &[(1, 0xD, 0x12)]); // 0x12 == BCD "12" == row 12, not hex 18
    let pattern1 = pattern(15, &[]);
    let m = module(vec![pattern0, pattern1], vec![0, 1]);

    let seq: Vec<(usize, usize, usize)> = iter_song_rows(&m).iter().map(|r| (r.order_pos, r.pattern_index, r.row_index)).collect();

    assert_eq!(&seq[..2], &[(0, 0, 0), (0, 0, 1)]);
    assert_eq!(seq[2], (1, 1, 12));
    assert_eq!(*seq.last().unwrap(), (1, 1, 14));
}

#[test]
fn test_position_jump_skips_song_positions() {
    let pattern0 = pattern(3, &[(0, 0xB, 2)]); // jump straight to order position 2
    let pattern1 = pattern(3, &[]);
    let m = module(vec![pattern0, pattern1], vec![0, 0, 1]);

    let seq: Vec<(usize, usize, usize)> = iter_song_rows(&m).iter().map(|r| (r.order_pos, r.pattern_index, r.row_index)).collect();

    assert_eq!(seq, vec![(0, 0, 0), (2, 1, 0), (2, 1, 1), (2, 1, 2)]);
}

#[test]
fn test_backward_jump_loop_terminates_instead_of_hanging() {
    let pattern0 = pattern(2, &[(1, 0xB, 0)]); // loops back to its own start forever
    let m = module(vec![pattern0], vec![0]);

    let seq: Vec<(usize, usize, usize)> = iter_song_rows(&m).iter().map(|r| (r.order_pos, r.pattern_index, r.row_index)).collect(); // must terminate, not hang

    assert_eq!(seq, vec![(0, 0, 0), (0, 0, 1)]);
}

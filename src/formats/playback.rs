//! Simulates song playback order, honoring effects that alter control flow.
//!
//! Naively playing each pattern in `module.order` start-to-finish is wrong: the
//! `Bxx` (Position Jump) and `Dxx` (Pattern Break) effects are extremely common
//! and end a pattern early, jumping straight to another song position (and
//! possibly a specific starting row within it) instead of playing out the rest
//! of the pattern's rows. Any code that walks the song for timing purposes
//! (MIDI export, .als export, ...) must go through this simulator rather than
//! iterating `module.patterns` directly, or it will render dead rows that never
//! actually play and desync everything after the jump.

use std::collections::HashSet;

use crate::formats::base::{Cell, Module};

const POSITION_JUMP: u32 = 0xB;
const PATTERN_BREAK: u32 = 0xD;

/// Dxx's row parameter is packed BCD (e.g. 0x23 means row 23, not 35).
fn bcd_row(param: u32) -> usize {
    let row = (param >> 4) * 10 + (param & 0x0F);
    if row < 64 { row as usize } else { 0 }
}

pub struct SongRow<'a> {
    pub order_pos: usize,
    pub pattern_index: usize,
    pub row_index: usize,
    pub row: &'a [Cell],
}

/// Yields (order_pos, pattern_index, row_index, row) in actual playback order.
///
/// Stops at the end of the order table, or when a jump would revisit an
/// already-played (order_pos, row) starting point — i.e. at the song's loop
/// point, so the output is one linear pass rather than an infinite loop.
pub fn iter_song_rows(module: &Module) -> Vec<SongRow<'_>> {
    let mut out = Vec::new();

    let order: Vec<usize> = if !module.order.is_empty() {
        module.order.iter().map(|&p| p as usize).collect()
    } else {
        (0..module.patterns.len()).collect()
    };
    if order.is_empty() {
        return out;
    }

    let mut visited_starts: HashSet<(i64, usize)> = HashSet::new();
    let mut order_pos: i64 = 0;
    let mut start_row: usize = 0;

    while order_pos >= 0 && (order_pos as usize) < order.len() {
        if !visited_starts.insert((order_pos, start_row)) {
            return out;
        }

        let pattern_index = order[order_pos as usize];
        if pattern_index >= module.patterns.len() {
            order_pos += 1;
            start_row = 0;
            continue;
        }

        let pattern = &module.patterns[pattern_index];
        let mut next_order_pos = order_pos + 1;
        let mut next_start_row = 0usize;

        for row_index in start_row..pattern.num_rows() {
            let row = &pattern.rows[row_index];
            out.push(SongRow { order_pos: order_pos as usize, pattern_index, row_index, row });

            let mut jump_pos: Option<i64> = None;
            let mut break_row: Option<usize> = None;
            for cell in row {
                if cell.effect == Some(POSITION_JUMP) {
                    if let Some(param) = cell.effect_param {
                        jump_pos = Some(param as i64);
                    }
                } else if cell.effect == Some(PATTERN_BREAK) {
                    if let Some(param) = cell.effect_param {
                        break_row = Some(bcd_row(param));
                    }
                }
            }

            if jump_pos.is_some() || break_row.is_some() {
                next_order_pos = jump_pos.unwrap_or(order_pos + 1);
                next_start_row = break_row.unwrap_or(0);
                break;
            }
        }

        order_pos = next_order_pos;
        start_row = next_start_row;
    }

    out
}

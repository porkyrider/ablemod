//! Konami K051649 (SCC/SCC1) emulator — a thin Rust wrapper around libvgm's own `k051649.c`
//! core (BSD-3-Clause, copyright Bryan McPhail/MAME), driven through chips::ffi's native shim.
//! Used alongside an AY8910 in many Konami MSX megaROM cartridges (Nemesis 2, King's Valley
//! II, ...).
//!
//! Unlike this project's earlier hand-ported version, the native core generates its own
//! samples at a fixed rate it derives internally from the input clock (`clock/16` for K051649
//! — confirmed directly against its source, not something this wrapper gets to choose), not at
//! the raw input-clock rate — chips::rateconv::RateConv still does the final downsample from
//! that native rate to this project's 44100Hz output, same mechanism as before, just resampling
//! a smaller ratio now that the native core already does its own internal rate reduction.

use crate::chips::ffi::{ChipKind, NativeChip};
use crate::chips::rateconv::RateConv;

const CHANNELS: usize = 5;
pub const ALL_CHANNELS_MASK: u32 = (1 << CHANNELS) - 1;

pub struct Scc {
    native: NativeChip,
    mask: u32, // bit i set = channel i muted — mirrored into the native core via set_mute_mask

    chip_rate: f64,
    output_rate: f64,
    time_acc: f64,
    conv: Option<RateConv>,
    last_out: i16, // used instead of `conv` when chip_rate == output_rate exactly
}

impl Scc {
    pub fn new(clock: u32, sample_rate: u32) -> Self {
        let native = NativeChip::new(ChipKind::Scc, clock, 0);
        let chip_rate = native.native_rate() as f64;
        let output_rate = sample_rate as f64;
        let conv = if chip_rate.floor() != output_rate && (chip_rate + 0.5).floor() != output_rate {
            Some(RateConv::new(chip_rate, output_rate))
        } else {
            None
        };
        Scc { native, mask: 0, chip_rate, output_rate, time_acc: 0.0, conv, last_out: 0 }
    }

    /// Register writes, mirroring how real VGM rips of this chip pre-decode the K051649
    /// address map into 4 "ports" (verified against a real file's own port/register
    /// distribution rather than guessed): port 0 is direct waveform RAM, port 1 is frequency,
    /// port 2 is volume, port 3 is the key on/off mask. Translated here into the chip's own
    /// real select-register/write-data hardware protocol (confirmed directly against libvgm's
    /// `SendYMCommand`, the same translation its own VGM player uses for this exact command):
    /// write the register address to `(port<<1)|0`, then the value to `(port<<1)|1`.
    pub fn write(&mut self, port: u8, reg: u8, value: u8) {
        self.native.write8(port << 1, reg);
        self.native.write8((port << 1) | 1, value);
    }

    pub fn set_mask(&mut self, mask: u32) {
        self.mask = mask;
        self.native.set_mute_mask(mask);
    }

    pub fn solo(&mut self, ch: usize) {
        self.set_mask(ALL_CHANNELS_MASK & !(1 << ch));
    }

    /// Calculate one (mono) output sample at the configured output rate. Mirrors the
    /// hand-ported version's own optimization: every channel keeps ticking in the native core
    /// regardless of `mask` *unless* every channel is muted at once, in which case there's
    /// nothing this render will ever need from this chip and the native Update() call (still
    /// comparatively expensive relative to a cheap synthesis rate, even after libvgm's own
    /// internal clock/16 reduction) is skipped outright.
    pub fn calc(&mut self) -> i16 {
        if self.mask == ALL_CHANNELS_MASK {
            return 0;
        }
        while self.chip_rate > self.time_acc {
            self.time_acc += self.output_rate;
            let out = self.native.calc() as i16;
            match &mut self.conv {
                Some(conv) => conv.put_data(0, out),
                None => self.last_out = out,
            }
        }
        self.time_acc -= self.chip_rate;
        match &mut self.conv {
            Some(conv) => conv.get_data(0),
            None => self.last_out,
        }
    }
}


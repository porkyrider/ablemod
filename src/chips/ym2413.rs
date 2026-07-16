//! YM2413 (OPLL) emulator — a thin Rust wrapper around libvgm's own `emu2413.c` core
//! (Mitsutaka Okazaki, <https://github.com/digital-sound-antiques/emu2413>, MIT license — the
//! exact same reference this project's earlier pure-Rust port was itself ported from), driven
//! through chips::ffi's native shim.
//!
//! Only the real YM2413 instrument ROM is ever selected (not the VRC7/YMF281B variants
//! emu2413 also supports, toggled via a config bit this wrapper always leaves at 0) — this
//! project only ever targets genuine YM2413 chip data from VGM files.
//!
//! MASK_BD/MASK_SD/MASK_TOM/MASK_CYM/MASK_HH's bit positions (9-13) match libvgm's own
//! `DeviceChannelNames` ordering for this chip's rhythm-mode percussion voices — confirmed
//! directly against `emu/cores/2413intf.c`'s channel name table and `emu2413.c`'s own
//! `MUTE_MASK_MAP`, not assumed to match this project's own earlier (unrelated, arbitrarily
//! chosen) internal bit ordering.

use crate::chips::ffi::{ChipKind, NativeChip};
use crate::chips::rateconv::RateConv;

const FM_CHANNELS: usize = 9;
const MASK_CH: fn(usize) -> u32 = |ch| 1 << ch;

pub struct Opll {
    native: NativeChip,
    mask: u32,

    chip_rate: f64,
    output_rate: f64,
    time_acc: f64,
    conv: Option<RateConv>,
    last_out: i16,
}

impl Opll {
    pub const MASK_BD: u32 = 1 << 9;
    pub const MASK_SD: u32 = 1 << 10;
    pub const MASK_TOM: u32 = 1 << 11;
    pub const MASK_CYM: u32 = 1 << 12;
    pub const MASK_HH: u32 = 1 << 13;

    pub fn new(clk: u32, rate: u32) -> Self {
        let native = NativeChip::new(ChipKind::Ym2413, clk, 0);
        let chip_rate = native.native_rate() as f64;
        let output_rate = rate as f64;
        let conv = if chip_rate.floor() != output_rate && (chip_rate + 0.5).floor() != output_rate {
            Some(RateConv::new(chip_rate, output_rate))
        } else {
            None
        };
        Opll { native, mask: 0, chip_rate, output_rate, time_acc: 0.0, conv, last_out: 0 }
    }

    /// Direct passthrough to the chip's real register file — no decoding needed here, the
    /// native core does that itself (including rhythm-mode detection from register 0x0E).
    pub fn write_reg(&mut self, reg: u32, data: u8) {
        self.native.write8(reg as u8, data);
    }

    /// Mutes selected channels' contribution to the final mix — e.g. `mask_ch(2)` — without
    /// otherwise touching their simulated state. Channels 0-8 are the 9 FM voices; MASK_BD/
    /// MASK_HH/MASK_SD/MASK_TOM/MASK_CYM are the rhythm-mode percussion voices that replace
    /// channels 6-8 when rhythm mode is enabled.
    pub fn set_mask(&mut self, mask: u32) {
        self.mask = mask;
        self.native.set_mute_mask(mask);
    }

    /// Mask bit for FM channel `ch` (0-8).
    pub fn mask_ch(ch: usize) -> u32 {
        MASK_CH(ch)
    }

    fn all_channels_mask() -> u32 {
        (0..FM_CHANNELS).map(MASK_CH).fold(0, |a, b| a | b) | Self::MASK_HH | Self::MASK_CYM | Self::MASK_TOM | Self::MASK_SD | Self::MASK_BD
    }

    /// Mask bit for every channel *except* the given one — i.e. what to pass to `set_mask` to
    /// isolate just that channel.
    pub fn solo_ch_mask(ch: usize) -> u32 {
        Self::all_channels_mask() & !MASK_CH(ch)
    }

    /// Mask bit for every rhythm voice except `hh`/`cym`/`tom`/`sd`/`bd` (pass the flag matching
    /// which one to keep audible), plus every FM channel — i.e. isolate one rhythm voice.
    pub fn solo_rhythm_mask(keep: u32) -> u32 {
        Self::all_channels_mask() & !keep
    }

    /// Calculate one (mono) output sample at the configured output rate, in emu2413's own raw
    /// native units (already the ~16-bit-PCM-ish scale this project's callers already divide
    /// by 32768.0, matching the earlier pure-Rust port's own output range exactly — verified
    /// directly, not assumed, before replacing that port).
    pub fn calc(&mut self) -> i16 {
        if self.mask == Self::all_channels_mask() {
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

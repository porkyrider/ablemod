//! AY-3-8910 / YM2149 (PSG) emulator — a thin Rust wrapper around libvgm's own `emu2149.c`
//! core (Mitsutaka Okazaki — the same author as YM2413's emu2413, <https://github.com/
//! digital-sound-antiques/emu2149>, MIT license), driven through chips::ffi's native shim.
//!
//! Unlike this project's earlier hand-ported version (Peter Sovietov's "ayumi"), emu2149
//! accepts *raw* AY8910 register writes directly (its own `EPSG_writeReg`, matching VGM's own
//! pre-decoded reg/value command pairs one-for-one) rather than a decomposed high-level API
//! (set_tone/set_mixer/set_volume/...) — the register→parameter decoding this project used to
//! do itself in export::vgm_render's own `Ay8910Driver` wrapper now happens inside the native
//! core instead, so that wrapper is gone; `write(reg, value)` is the only entry point needed.
//!
//! Real per-channel stereo panning (ayumi's `set_pan`) was never actually exercised anywhere
//! in this project (confirmed by search — no caller ever set anything but the default centered
//! pan), so this wrapper follows chips::scc's precedent and treats the chip as mono.

use crate::chips::ffi::{ChipKind, NativeChip};
use crate::chips::rateconv::RateConv;

const CHANNELS: usize = 3;
pub const ALL_CHANNELS_MASK: u32 = (1 << CHANNELS) - 1;

pub struct Ay8910 {
    native: NativeChip,
    mask: u32,

    chip_rate: f64,
    output_rate: f64,
    time_acc: f64,
    conv: Option<RateConv>,
    last_out: i16,
}

impl Ay8910 {
    /// `is_ym`: true for YM2149's DAC table (Sega/MSX-era chips typically identify as this
    /// variant), false for the plain AY-3-8910 table — same convention this project's earlier
    /// Ayumi::new used, now mapped onto emu2149's own `chipType` field (see native/shim.c).
    /// `clock_rate` is the chip's own input clock in Hz (VGM header value); `sample_rate` is
    /// the desired output sample rate.
    pub fn new(is_ym: bool, clock_rate: f64, sample_rate: u32) -> Self {
        let native = NativeChip::new(ChipKind::Ay8910, clock_rate as u32, is_ym as u8);
        let chip_rate = native.native_rate() as f64;
        let output_rate = sample_rate as f64;
        let conv = if chip_rate.floor() != output_rate && (chip_rate + 0.5).floor() != output_rate {
            Some(RateConv::new(chip_rate, output_rate))
        } else {
            None
        };
        Ay8910 { native, mask: 0, chip_rate, output_rate, time_acc: 0.0, conv, last_out: 0 }
    }

    /// Direct passthrough to the chip's real register file (0-15) — no decoding needed here,
    /// the native core does that itself.
    pub fn write(&mut self, reg: u8, value: u8) {
        self.native.write8(reg, value);
    }

    pub fn set_mask(&mut self, mask: u32) {
        self.mask = mask;
        self.native.set_mute_mask(mask);
    }

    pub fn solo(&mut self, ch: usize) {
        self.set_mask(ALL_CHANNELS_MASK & !(1 << ch));
    }

    pub fn mute_all(&mut self) {
        self.set_mask(ALL_CHANNELS_MASK);
    }

    pub fn unmute_all(&mut self) {
        self.set_mask(0);
    }

    /// Calculate one (mono) output sample at the configured output rate, in emu2149's own raw
    /// native units — same resample-while-loop shape as chips::scc::Scc::calc, see its own
    /// comment. Not normalized to any particular float range here, same convention
    /// chips::scc::Scc::calc and chips::ym2413::Opll::calc already use — export::vgm_render's
    /// own AY_UNIT_SCALE divides this down when mixing (see its own comment for how that
    /// constant was calibrated against this project's earlier Ayumi-based port).
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


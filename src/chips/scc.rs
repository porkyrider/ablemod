//! Konami K051649 (SCC/SCC1) emulator — a faithful Rust port of MAME's
//! `src/devices/sound/k051649.cpp` (BSD-3-Clause, copyright Bryan McPhail). Used alongside an
//! AY8910 in many Konami MSX megaROM cartridges (Nemesis 2, King's Valley II, ...).
//!
//! By far the simplest of the three chips this project emulates: 5 channels, each stepping
//! through its own 32-byte user-defined waveform (8-bit signed samples) at a rate set by a
//! 12-bit frequency register, scaled by a 4-bit volume — no envelope generator, no LFO.
//! Channel 4 (0-indexed) shares its waveform RAM with channel 3 on real SCC1 hardware, which
//! this port keeps: writing waveform bytes 0x60-0x7F updates both.
//!
//! `tick()` advances by one native input-clock cycle, matching the real chip (and MAME's own
//! model, where the sound stream runs at the chip's input clock rate) — unlike YM2413's fixed
//! internal clk/72 synthesis rate, there's no cheaper "one synthesis step" granularity to hook
//! into here, so resampling down to the output rate costs proportionally more chip_rate/
//! output_rate native ticks per output sample (chips::rateconv::RateConv handles that either
//! way, same as chips::ym2413).

use crate::chips::rateconv::RateConv;

const CHANNELS: usize = 5;
pub const ALL_CHANNELS_MASK: u32 = (1 << CHANNELS) - 1;

struct Channel {
    waveram: [i8; 32],
    frequency: u16, // 12-bit
    volume: u8,     // 4-bit
    key: bool,
    counter: u8, // 0-31: wavetable read position
    clock: i32,  // internal sub-counter, counts up to `frequency`
    sample: i16, // latched output, held between wavetable steps
}

impl Channel {
    fn new() -> Self {
        Channel { waveram: [0; 32], frequency: 0, volume: 0x0f, key: false, counter: 0, clock: 0, sample: 0 }
    }

    /// One native input-clock cycle. The channel is halted entirely for frequency < 9 (a
    /// documented real-hardware quirk, not a guess — see k051649.cpp's sound_stream_update).
    fn tick(&mut self) {
        if self.frequency > 8 {
            self.clock += 1;
            if self.clock > self.frequency as i32 {
                self.counter = (self.counter + 1) & 0x1f;
                self.clock = 0;
            }
            if self.clock == 0 {
                self.sample = (if self.key { self.waveram[self.counter as usize] as i16 } else { 0 }) * self.volume as i16;
            }
        }
    }
}

pub struct Scc {
    channels: [Channel; CHANNELS],
    mask: u32, // bit i set = channel i muted

    chip_rate: f64,
    output_rate: f64,
    time_acc: f64,
    conv: Option<RateConv>,
    last_out: i16, // used instead of `conv` when chip_rate == output_rate exactly
}

impl Scc {
    pub fn new(clock: u32, sample_rate: u32) -> Self {
        let chip_rate = clock as f64;
        let output_rate = sample_rate as f64;
        let conv = if chip_rate.floor() != output_rate && (chip_rate + 0.5).floor() != output_rate {
            Some(RateConv::new(chip_rate, output_rate))
        } else {
            None
        };
        Scc {
            channels: std::array::from_fn(|_| Channel::new()),
            mask: 0,
            chip_rate,
            output_rate,
            time_acc: 0.0,
            conv,
            last_out: 0,
        }
    }

    /// Register writes, mirroring how real VGM rips of this chip pre-decode the K051649
    /// address map into 4 "ports" (verified against a real file's own port/register
    /// distribution rather than guessed): port 0 is direct waveform RAM (reg 0x00-0x7F, same
    /// addressing as k051649_waveform_w — reg >= 0x60 mirrors into channel 3's table per real
    /// SCC1 hardware); port 1 is frequency (reg 0-9: two bytes per channel, low then high
    /// nibble-masked to 12 bits); port 2 is volume (reg 0-4, one nibble per channel); port 3 is
    /// the key on/off mask (reg 0, one bit per channel). The chip's own test register has no
    /// audible effect and isn't modeled.
    pub fn write(&mut self, port: u8, reg: u8, value: u8) {
        match port {
            0 => {
                let offset = reg as usize & 0x7f;
                if offset >= 0x60 {
                    self.channels[3].waveram[offset & 0x1f] = value as i8;
                    self.channels[4].waveram[offset & 0x1f] = value as i8;
                } else {
                    self.channels[offset >> 5].waveram[offset & 0x1f] = value as i8;
                }
            }
            1 => {
                let ch = (reg >> 1) as usize;
                if ch >= CHANNELS {
                    return;
                }
                if reg & 1 == 1 {
                    self.channels[ch].frequency = (self.channels[ch].frequency & 0x0ff) | ((value as u16) << 8 & 0xf00);
                } else {
                    self.channels[ch].frequency = (self.channels[ch].frequency & 0xf00) | value as u16;
                }
            }
            2 => {
                if (reg as usize) < CHANNELS {
                    self.channels[reg as usize].volume = value & 0x0f;
                }
            }
            3 => {
                for (i, ch) in self.channels.iter_mut().enumerate() {
                    ch.key = (value >> i) & 1 != 0;
                }
            }
            _ => {} // test register / unused ports: no audible effect
        }
    }

    pub fn set_mask(&mut self, mask: u32) {
        self.mask = mask;
    }

    pub fn solo(&mut self, ch: usize) {
        self.mask = ALL_CHANNELS_MASK & !(1 << ch);
    }

    fn mix_native_tick(&mut self) -> i16 {
        let mut sum: i32 = 0;
        for (i, ch) in self.channels.iter_mut().enumerate() {
            ch.tick();
            if self.mask & (1 << i) == 0 {
                // Matches k051649.cpp's own `sample >> 4` scaling (an 8-bit waveform sample
                // times a 4-bit volume, brought back down to a comparable per-channel range).
                sum += (ch.sample >> 4) as i32;
            }
        }
        sum as i16
    }

    /// Calculate one (mono) output sample at the configured output rate. Every channel's
    /// `tick()` always runs regardless of `mask` — matching how export::vgm_render's other
    /// chips keep simulating muted channels — *unless* every channel is muted at once, in
    /// which case there is nothing this render will ever need from this chip and the whole
    /// (comparatively expensive, since it runs at the chip's full input-clock rate rather than
    /// a cheaper internal synthesis rate) native-tick loop is skipped outright.
    pub fn calc(&mut self) -> i16 {
        if self.mask == ALL_CHANNELS_MASK {
            return 0;
        }
        while self.chip_rate > self.time_acc {
            self.time_acc += self.output_rate;
            let out = self.mix_native_tick();
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

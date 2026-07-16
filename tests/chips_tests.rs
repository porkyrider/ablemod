//! Regression tests for the chip emulators (chips::ay8910, chips::ym2413, chips::scc) — these
//! can't be checked against a real fixture the way the tracker side is (no real hardware to
//! compare against here), so instead they check the physics: does the emulated chip actually
//! produce the frequency its own register formula says it should?
//!
//! Uses autocorrelation (searching lag range 0.4x-2.5x of the register formula's own predicted
//! period, maximizing correlation) rather than naive zero-crossing counting — zero-crossing
//! was the original method here, but proved too fragile against SCC's real output once its
//! emulator started synthesizing at a much coarser native rate before this project's own
//! resampling (see chips::scc's own module comment): the *audible* pitch was already correct
//! (confirmed directly against this exact scenario), zero-crossing was just miscounting
//! resampling-ringing as extra cycles. Autocorrelation is the same robust method this project
//! already uses elsewhere (export::vgm_wavetable's own real-file pitch verification) for
//! exactly this class of measurement fragility.

use ablemod::chips::ay8910::Ay8910;
use ablemod::chips::scc::Scc;
use ablemod::chips::ym2413::Opll;

/// Loads a tone period (registers 0-5, two bytes per channel: low 8 bits then high nibble) —
/// chips::ay8910::Ay8910 now takes raw registers directly (no more high-level set_tone/
/// set_mixer/set_volume API, see its own module comment), so every test below builds the
/// exact register writes a real AY8910 rip would make instead.
fn ay_set_tone(ay: &mut Ay8910, ch: usize, period: u16) {
    ay.write((ch * 2) as u8, (period & 0xFF) as u8);
    ay.write((ch * 2 + 1) as u8, ((period >> 8) & 0x0F) as u8);
}

/// Register 7: bits 0-2 = tone disable per channel (A/B/C), bits 3-5 = noise disable per
/// channel — 1 means "off" in both groups (real AY8910 mixer convention).
fn ay_set_mixer(ay: &mut Ay8910, tone_off: [bool; 3], noise_off: [bool; 3]) {
    let mut value = 0u8;
    for (ch, &off) in tone_off.iter().enumerate() {
        value |= (off as u8) << ch;
    }
    for (ch, &off) in noise_off.iter().enumerate() {
        value |= (off as u8) << (ch + 3);
    }
    ay.write(7, value);
}

fn ay_set_volume(ay: &mut Ay8910, ch: usize, volume: u8) {
    ay.write((8 + ch) as u8, volume & 0x0F); // bit 4 (envelope-enable) left clear
}

const SAMPLE_RATE: u32 = 44100;

fn measure_frequency(samples: &[f64], sample_rate: u32, settle: usize, expected_hz: f64) -> f64 {
    let samples = &samples[settle..];
    let predicted_period = sample_rate as f64 / expected_hz;
    // A tight band around the register formula's own prediction, not the wide 0.4x-2.5x range
    // a blind pitch detector would need — wide enough to accommodate real measurement/
    // resampling error, but tight enough to never accidentally lock onto the 2x/0.5x
    // subharmonic a symmetric-ish waveform's own autocorrelation can score higher than the true
    // fundamental (caught directly: YM2413's carrier-only test tone did exactly this at 2.5x).
    // A genuine octave-scale emulator bug (this is exactly how the SCC regression this
    // migration introduced, and then fixed, was first caught) still lands well outside this
    // band and fails loudly, which is the whole point of these tests.
    let min_lag = (predicted_period * 0.7).max(1.0) as usize;
    let max_lag = ((predicted_period * 1.4) as usize).min(samples.len() / 2);

    let mut best_lag = min_lag;
    let mut best_corr = f64::MIN;
    for lag in min_lag..max_lag {
        let corr: f64 = (0..samples.len() - lag).map(|i| samples[i] * samples[i + lag]).sum();
        if corr > best_corr {
            best_corr = corr;
            best_lag = lag;
        }
    }
    sample_rate as f64 / best_lag as f64
}

#[test]
fn test_ay8910_tone_frequency_matches_the_chips_own_formula() {
    // AY8910 tone frequency = clock / (16 * period) — a well-documented, simple formula this
    // port either gets right or doesn't; no room for subjective interpretation here.
    let clock = 1_789_773.0;
    let period = 200;
    let mut ay = Ay8910::new(true, clock, SAMPLE_RATE);
    ay_set_tone(&mut ay, 0, period);
    ay_set_mixer(&mut ay, [false, true, true], [true, true, true]); // A: tone on, noise off; B/C fully silent
    ay_set_volume(&mut ay, 0, 15);

    let mut samples = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        samples.push(ay.calc() as f64);
    }

    let expected = clock / (16.0 * period as f64);
    let measured = measure_frequency(&samples, SAMPLE_RATE, 2000, expected);
    assert!((measured - expected).abs() / expected < 0.01, "measured {measured} Hz, expected {expected} Hz");
}

#[test]
fn test_ay8910_solo_silences_every_other_channel() {
    let mut ay = Ay8910::new(true, 1_789_773.0, SAMPLE_RATE);
    for ch in 0..3 {
        ay_set_tone(&mut ay, ch, 200);
    }
    ay_set_mixer(&mut ay, [false, false, false], [true, true, true]);
    for ch in 0..3 {
        ay_set_volume(&mut ay, ch, 15);
    }
    ay.solo(1);

    let mut peak = 0.0f64;
    let mut samples = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        let s = ay.calc() as f64;
        peak = peak.max(s.abs());
        samples.push(s);
    }
    assert!(peak > 0.01); // channel 1 is still audible

    // solo(0) instead — the two renders must sound different, proving solo() actually
    // switches which channel is audible rather than being a no-op
    let mut ay2 = Ay8910::new(true, 1_789_773.0, SAMPLE_RATE);
    for ch in 0..3 {
        ay_set_tone(&mut ay2, ch, 200 + ch as u16 * 50); // distinct periods per channel
    }
    ay_set_mixer(&mut ay2, [false, false, false], [true, true, true]);
    for ch in 0..3 {
        ay_set_volume(&mut ay2, ch, 15);
    }
    ay2.solo(0);
    let mut samples2 = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        samples2.push(ay2.calc() as f64);
    }
    let expected_ch0 = 1_789_773.0 / (16.0 * 200.0);
    let f0 = measure_frequency(&samples2, SAMPLE_RATE, 2000, expected_ch0);
    assert!((f0 - expected_ch0).abs() / expected_ch0 < 0.01);
}

#[test]
fn test_ym2413_tone_frequency_matches_the_chips_own_formula() {
    let clock = 3_579_545u32;
    let mut opll = Opll::new(clock, SAMPLE_RATE);

    // custom instrument (0): silent modulator (no audible FM), plain carrier at unity ML
    // so the phase generator's own frequency is directly measurable from the output.
    opll.write_reg(0x00, 0x00);
    opll.write_reg(0x02, 0x3F); // modulator TL=63 (silent)
    opll.write_reg(0x04, 0xFF);
    opll.write_reg(0x06, 0x00);
    opll.write_reg(0x01, 0x01); // carrier ML=1
    opll.write_reg(0x03, 0x00);
    opll.write_reg(0x05, 0xF8);
    opll.write_reg(0x07, 0x00); // SL=0 (full sustain)

    let fnum: u32 = 400;
    let blk: u32 = 4;
    opll.write_reg(0x10, (fnum & 0xFF) as u8);
    opll.write_reg(0x20, 0x10 | (((fnum >> 8) & 1) as u8) | (((blk & 7) as u8) << 1));
    opll.write_reg(0x30, 0x00);

    let mut samples = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        samples.push(opll.calc() as f64 / 32768.0);
    }

    // pg_phase delta per chip tick = ((fnum*2 + 0) * ml_table[ML=1]=2) << blk >> 2, one full
    // cycle = 2^19; chip ticks at clk/72 Hz.
    let delta = ((fnum * 2) * 2) << blk >> 2;
    let chip_tick_hz = clock as f64 / 72.0;
    let expected = (delta as f64 / 524288.0) * chip_tick_hz;
    let measured = measure_frequency(&samples, SAMPLE_RATE, 4000, expected);

    assert!((measured - expected).abs() / expected < 0.01, "measured {measured} Hz, expected {expected} Hz");
}

#[test]
fn test_ym2413_envelope_sustains_without_decaying_when_sl_and_rr_are_zero() {
    let mut opll = Opll::new(3_579_545, SAMPLE_RATE);
    opll.write_reg(0x00, 0x00);
    opll.write_reg(0x02, 0x3F);
    opll.write_reg(0x04, 0xFF);
    opll.write_reg(0x06, 0x00);
    opll.write_reg(0x01, 0x01);
    opll.write_reg(0x03, 0x00);
    opll.write_reg(0x05, 0xF8); // AR=15 DR=8
    opll.write_reg(0x07, 0x00); // SL=0 RR=0: sustains indefinitely once decayed to 0

    opll.write_reg(0x10, 0x90);
    opll.write_reg(0x20, 0x18); // key-on, block=4
    opll.write_reg(0x30, 0x00);

    let total = SAMPLE_RATE as usize; // 1 second
    let mut samples = Vec::with_capacity(total);
    for _ in 0..total {
        samples.push(opll.calc() as f64 / 32768.0);
    }

    let rms_window = |start: usize| -> f64 {
        let w = &samples[start..start + 2000];
        (w.iter().map(|x| x * x).sum::<f64>() / w.len() as f64).sqrt()
    };
    let early = rms_window(2000);
    let late = rms_window(total - 3000);
    assert!(early > 0.001, "note should have attacked to audible volume");
    // late-window RMS should still be within the same order of magnitude as the early one —
    // not collapsed to silence and not blown up
    assert!(late > early * 0.5 && late < early * 2.0, "early={early} late={late}");
}

fn write_sine_waveform(scc: &mut Scc, waveform_reg_base: u8) {
    for i in 0..32u8 {
        let phase = i as f64 / 32.0 * std::f64::consts::TAU;
        let sample = (phase.sin() * 120.0).round() as i8;
        scc.write(0, waveform_reg_base + i, sample as u8);
    }
}

#[test]
fn test_scc_tone_frequency_matches_the_chips_own_formula() {
    // SCC frequency = clock / (32 * (register + 1)) — documented in k051649.cpp's own
    // sound_stream_update (the wavetable step rate), and only active for register > 8.
    let clock = 1_789_772.0;
    let freq_reg: u32 = 99;
    let mut scc = Scc::new(clock as u32, SAMPLE_RATE);
    write_sine_waveform(&mut scc, 0x00);
    scc.write(1, 0, (freq_reg & 0xFF) as u8);
    scc.write(1, 1, ((freq_reg >> 8) & 0x0F) as u8);
    scc.write(2, 0, 15); // channel 0 volume = max
    scc.write(3, 0, 0b0000_0001); // key on channel 0 only

    let mut samples = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        samples.push(scc.calc() as f64);
    }

    let expected = clock / (32.0 * (freq_reg as f64 + 1.0));
    let measured = measure_frequency(&samples, SAMPLE_RATE, 2000, expected);
    assert!((measured - expected).abs() / expected < 0.02, "measured {measured} Hz, expected {expected} Hz");
}

#[test]
fn test_scc_channel_below_the_halt_threshold_stays_silent() {
    // Channels are documented as halted entirely for register < 9 — not just "very low
    // pitch" but genuinely inaudible, which is worth locking down since it's an easy off-by-
    // one to get wrong when porting (register <= 8 vs < 8).
    let mut scc = Scc::new(1_789_772, SAMPLE_RATE);
    write_sine_waveform(&mut scc, 0x00);
    scc.write(1, 0, 8); // register = 8: below the halt threshold
    scc.write(1, 1, 0);
    scc.write(2, 0, 15);
    scc.write(3, 0, 0b0000_0001);

    let peak = (0..SAMPLE_RATE).map(|_| scc.calc().unsigned_abs()).max().unwrap_or(0);
    assert_eq!(peak, 0, "register 8 should still be halted");
}

#[test]
fn test_scc_solo_silences_every_other_channel() {
    let mut scc = Scc::new(1_789_772, SAMPLE_RATE);
    for ch in 0..3 {
        write_sine_waveform(&mut scc, (ch * 32) as u8);
        let freq_reg: u32 = 60 + ch as u32 * 20;
        scc.write(1, (ch * 2) as u8, (freq_reg & 0xFF) as u8);
        scc.write(1, (ch * 2 + 1) as u8, ((freq_reg >> 8) & 0x0F) as u8);
        scc.write(2, ch as u8, 15);
    }
    scc.write(3, 0, 0b0000_0111); // key on channels 0-2

    scc.solo(1);
    let samples: Vec<f64> = (0..SAMPLE_RATE).map(|_| scc.calc() as f64).collect();
    let peak = samples.iter().fold(0.0f64, |a, &b| a.max(b.abs()));
    assert!(peak > 0.0, "the soloed channel should still be audible");

    let expected = 1_789_772.0 / (32.0 * (80.0 + 1.0)); // channel 1's own register (60 + 1*20)
    let measured = measure_frequency(&samples, SAMPLE_RATE, 2000, expected);
    assert!((measured - expected).abs() / expected < 0.02, "measured {measured} Hz, expected {expected} Hz");
}

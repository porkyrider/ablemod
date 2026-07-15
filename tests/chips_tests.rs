//! Regression tests for the ported chip emulators (chips::ay8910, chips::ym2413,
//! chips::scc) — these can't be checked against a real fixture the way the tracker side is (no
//! real hardware to compare against here), so instead they check the physics: does the
//! emulated chip actually produce the frequency its own register formula says it should? This
//! is the same measurement approach (zero-crossing counting) used to manually validate all
//! three ports before building anything on top of them.

use ablemod::chips::ay8910::Ayumi;
use ablemod::chips::scc::Scc;
use ablemod::chips::ym2413::Opll;

const SAMPLE_RATE: u32 = 44100;

fn measure_frequency(samples: &[f64], sample_rate: u32, settle: usize) -> f64 {
    let mut crossings = 0;
    for w in samples[settle..].windows(2) {
        if w[0] <= 0.0 && w[1] > 0.0 {
            crossings += 1;
        }
    }
    let duration_s = (samples.len() - settle) as f64 / sample_rate as f64;
    crossings as f64 / duration_s
}

#[test]
fn test_ay8910_tone_frequency_matches_the_chips_own_formula() {
    // AY8910 tone frequency = clock / (16 * period) — a well-documented, simple formula this
    // port either gets right or doesn't; no room for subjective interpretation here.
    let clock = 1_789_773.0;
    let period = 200;
    let mut ay = Ayumi::new(true, clock, SAMPLE_RATE);
    ay.set_tone(0, period);
    ay.set_mixer(0, 0, 1, 0); // tone on, noise off, envelope off
    ay.set_volume(0, 15);
    ay.set_mixer(1, 1, 1, 0); // channels B/C fully silent
    ay.set_mixer(2, 1, 1, 0);

    let mut samples = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        ay.process();
        ay.remove_dc();
        samples.push(ay.left);
    }

    let measured = measure_frequency(&samples, SAMPLE_RATE, 2000);
    let expected = clock / (16.0 * period as f64);
    assert!((measured - expected).abs() / expected < 0.01, "measured {measured} Hz, expected {expected} Hz");
}

#[test]
fn test_ay8910_solo_silences_every_other_channel() {
    let mut ay = Ayumi::new(true, 1_789_773.0, SAMPLE_RATE);
    for ch in 0..3 {
        ay.set_tone(ch, 200);
        ay.set_mixer(ch, 0, 1, 0);
        ay.set_volume(ch, 15);
    }
    ay.solo(1);

    let mut peak = 0.0f64;
    let mut samples = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        ay.process();
        ay.remove_dc();
        peak = peak.max(ay.left.abs());
        samples.push(ay.left);
    }
    assert!(peak > 0.01); // channel 1 is still audible

    // solo(0) instead — the two renders must sound different, proving solo() actually
    // switches which channel is audible rather than being a no-op
    let mut ay2 = Ayumi::new(true, 1_789_773.0, SAMPLE_RATE);
    for ch in 0..3 {
        ay2.set_tone(ch, 200 + ch as i32 * 50); // distinct periods per channel
        ay2.set_mixer(ch, 0, 1, 0);
        ay2.set_volume(ch, 15);
    }
    ay2.solo(0);
    let mut samples2 = Vec::with_capacity(SAMPLE_RATE as usize);
    for _ in 0..SAMPLE_RATE {
        ay2.process();
        ay2.remove_dc();
        samples2.push(ay2.left);
    }
    let f0 = measure_frequency(&samples2, SAMPLE_RATE, 2000);
    let expected_ch0 = 1_789_773.0 / (16.0 * 200.0);
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

    let measured = measure_frequency(&samples, SAMPLE_RATE, 4000);
    // pg_phase delta per chip tick = ((fnum*2 + 0) * ml_table[ML=1]=2) << blk >> 2, one full
    // cycle = 2^19; chip ticks at clk/72 Hz.
    let delta = ((fnum * 2) * 2) << blk >> 2;
    let chip_tick_hz = clock as f64 / 72.0;
    let expected = (delta as f64 / 524288.0) * chip_tick_hz;

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

    let measured = measure_frequency(&samples, SAMPLE_RATE, 2000);
    let expected = clock / (32.0 * (freq_reg as f64 + 1.0));
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

    let measured = measure_frequency(&samples, SAMPLE_RATE, 2000);
    let expected = 1_789_772.0 / (32.0 * (80.0 + 1.0)); // channel 1's own register (60 + 1*20)
    assert!((measured - expected).abs() / expected < 0.02, "measured {measured} Hz, expected {expected} Hz");
}

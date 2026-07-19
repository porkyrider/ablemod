use ablemod::export::wav::write_sample_wav;
use ablemod::formats::base::Sample;

#[test]
fn test_writes_mono_16bit_wav() {
    let sample = Sample {
        index: 1, name: "kick".to_string(), pcm16: vec![0, 0, 255, 127, 0, 128],
        sample_rate_hz: 8363, loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60, pan: 0.0, volume_envelope: None, panning_envelope: None, fadeout: 0,
    };
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("kick.wav");

    write_sample_wav(&sample, &out_path).unwrap();

    let mut reader = hound::WavReader::open(&out_path).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(spec.sample_rate, 8363);
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    let expected: Vec<i16> = sample.pcm16.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    assert_eq!(samples, expected);
}

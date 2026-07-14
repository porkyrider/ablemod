use std::path::Path;

use crate::formats::base::Sample;

pub fn sample_wav_filename(sample: &Sample) -> String {
    let safe_name: String = sample
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect();
    let safe_name = safe_name.trim();
    let safe_name = if safe_name.is_empty() { "sample" } else { safe_name };
    format!("{:02}_{}.wav", sample.index, safe_name)
}

/// Write a Sample's PCM data (mono, 16-bit signed) as a .wav file.
pub fn write_sample_wav(sample: &Sample, path: &Path) -> std::io::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sample.sample_rate_hz,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(std::io::Error::other)?;
    for chunk in sample.pcm16.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        writer.write_sample(s).map_err(std::io::Error::other)?;
    }
    writer.finalize().map_err(std::io::Error::other)?;
    Ok(())
}

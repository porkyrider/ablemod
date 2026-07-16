mod common;

use std::path::Path;
use std::process::Command;

use common::build_minimal_mod;

fn write_mod(dir: &Path, second_cell_effect: Option<(u8, u8)>) -> std::path::PathBuf {
    let mod_path = dir.join("test.mod");
    std::fs::write(&mod_path, build_minimal_mod(second_cell_effect)).unwrap();
    mod_path
}

fn run(args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ablemod")).args(args).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), combined)
}

/// A minimal but structurally real VGM v1.51 file with a single audible AY8910 tone (channel A,
/// max volume) for total_samples worth of playback — just enough for a CLI smoke test to
/// produce real, non-silent rendered audio, not a thorough parser test (see
/// tests/vgm_tests.rs::build_vgm for that).
fn write_vgm(dir: &Path, total_samples: u32) -> std::path::PathBuf {
    const DATA_START: u32 = 0xC0;
    let mut commands = Vec::new();
    commands.extend_from_slice(&[0xA0, 0x00, 0xC8]); // channel A period lo
    commands.extend_from_slice(&[0xA0, 0x07, 0b1111_1110]); // mixer: enable tone on channel A only
    commands.extend_from_slice(&[0xA0, 0x08, 0x0F]); // channel A volume = max
    commands.extend_from_slice(&[0x61, (total_samples & 0xFF) as u8, ((total_samples >> 8) & 0xFF) as u8]);
    commands.push(0x66);

    let mut data = vec![0u8; DATA_START as usize];
    data[0..4].copy_from_slice(b"Vgm ");
    data[0x08..0x0C].copy_from_slice(&0x151u32.to_le_bytes());
    data[0x18..0x1C].copy_from_slice(&total_samples.to_le_bytes());
    data[0x74..0x78].copy_from_slice(&1_789_773u32.to_le_bytes()); // AY8910 clock
    let vgm_data_offset_field = DATA_START - 0x34;
    data[0x34..0x38].copy_from_slice(&vgm_data_offset_field.to_le_bytes());
    let total_len = DATA_START + commands.len() as u32;
    let eof_offset_field = total_len - 0x04;
    data[0x04..0x08].copy_from_slice(&eof_offset_field.to_le_bytes());
    data.extend_from_slice(&commands);

    let vgm_path = dir.join("test.vgm");
    std::fs::write(&vgm_path, data).unwrap();
    vgm_path
}

#[test]
fn test_extract_samples_verbose_prints_sample_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_dir = dir.path().join("samples");

    let (code, output) = run(&["extract-samples", mod_path.to_str().unwrap(), "-o", out_dir.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("frames,"));
    assert!(output.contains("no loop"));
}

#[test]
fn test_extract_samples_without_verbose_omits_sample_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_dir = dir.path().join("samples");

    let (code, output) = run(&["extract-samples", mod_path.to_str().unwrap(), "-o", out_dir.to_str().unwrap()]);

    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("Verbose:"));
}

#[test]
fn test_extract_midi_verbose_warns_about_unimplemented_effect() {
    // E3x = Glissando Control, one of the few Exx sub-commands still unimplemented (see
    // IMPLEMENTED_E_SUBCOMMANDS in formats::protracker)
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0xE, 0x30)));
    let out_path = dir.path().join("out.mid");

    let (code, output) = run(&["extract-midi", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("unimplemented effects found"));
    assert!(output.contains("E3x"));
    assert!(output.contains("Glissando Control"));
    assert!(output.contains("no implemented effects found in this module")); // only effect present is unimplemented
}

#[test]
fn test_extract_midi_verbose_reports_transcribed_effects() {
    // effect 8 = Set Panning, in IMPLEMENTED_EFFECTS
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0x8, 0x80)));
    let out_path = dir.path().join("out.mid");

    let (code, output) = run(&["extract-midi", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("effects correctly transcribed"));
    assert!(output.contains("8xx"));
    assert!(output.contains("Set Panning"));
    assert!(output.contains("no unimplemented effects found"));
    assert!(output.contains("note(s)"));
}

#[test]
fn test_extract_midi_without_verbose_omits_effect_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0x7, 0x28)));
    let out_path = dir.path().join("out.mid");

    let (code, output) = run(&["extract-midi", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap()]);

    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("Verbose:"));
}

#[test]
fn test_convert_verbose_warns_about_unimplemented_effect() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0xE, 0x30))); // E3x = Glissando Control, unimplemented
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert-als", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("unimplemented effects found"));
    assert!(output.contains("E3x"));
    assert!(output.contains("Glissando Control"));
    assert!(output.contains("track(s)"));
    assert!(output.contains("no implemented effects found in this module")); // only effect present is unimplemented
}

#[test]
fn test_convert_verbose_reports_transcribed_effects() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0xA, 0x40))); // Volume Slide, implemented
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert-als", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("effects correctly transcribed"));
    assert!(output.contains("Axx"));
    assert!(output.contains("Volume Slide"));
    assert!(output.contains("no unimplemented effects found"));
}

#[test]
fn test_convert_without_verbose_omits_effect_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0x9, 0x10)));
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert-als", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap()]);

    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("Verbose:"));
}

#[test]
fn test_convert_accepts_amiga_panning_flag() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_path = dir.path().join("Project").join("out.als");

    for preset in ["none", "light", "medium", "full"] {
        let (code, output) = run(&["convert-als", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--amiga-panning", preset]);
        assert_eq!(code, 0, "preset {preset}: {output}");
    }
}

#[test]
fn test_convert_rejects_invalid_amiga_panning_value() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert-als", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--amiga-panning", "bogus"]);
    assert_ne!(code, 0);
    assert!(output.contains("invalid value"));
}

#[test]
fn test_extract_mixed_tracks_writes_a_single_wav() {
    let dir = tempfile::tempdir().unwrap();
    let vgm_path = write_vgm(dir.path(), 4410);
    let out_path = dir.path().join("mix.wav");

    let (code, output) = run(&["extract-mixed-tracks", vgm_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(out_path.exists());
    assert!(output.contains("wrote"));
}

#[test]
fn test_extract_separated_tracks_wav_writes_one_file_per_stem() {
    let dir = tempfile::tempdir().unwrap();
    let vgm_path = write_vgm(dir.path(), 4410);
    let out_dir = dir.path().join("stems");

    let (code, output) = run(&["extract-separated-tracks-wav", vgm_path.to_str().unwrap(), "-o", out_dir.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    let wav_files: Vec<_> =
        std::fs::read_dir(&out_dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|ext| ext == "wav")).collect();
    assert!(!wav_files.is_empty(), "expected at least one stem WAV in {}", out_dir.display());
}

#[test]
fn test_extract_mixed_tracks_rejects_tracker_module() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_path = dir.path().join("mix.wav");

    let (code, output) = run(&["extract-mixed-tracks", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap()]);

    assert_ne!(code, 0);
    assert!(output.contains("tracker modules"));
    assert!(!out_path.exists());
}

#[test]
fn test_extract_separated_tracks_wav_rejects_tracker_module() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_dir = dir.path().join("stems");

    let (code, output) = run(&["extract-separated-tracks-wav", mod_path.to_str().unwrap(), "-o", out_dir.to_str().unwrap()]);

    assert_ne!(code, 0);
    assert!(output.contains("tracker modules"));
}

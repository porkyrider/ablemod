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
    // effect 7 = Tremolo, not in IMPLEMENTED_EFFECTS
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0x7, 0x28)));
    let out_path = dir.path().join("out.mid");

    let (code, output) = run(&["extract-midi", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("unimplemented effects found"));
    assert!(output.contains("7xx"));
    assert!(output.contains("Tremolo"));
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
    let mod_path = write_mod(dir.path(), Some((0x9, 0x10))); // Sample Offset, unimplemented
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("unimplemented effects found"));
    assert!(output.contains("9xx"));
    assert!(output.contains("Sample Offset"));
    assert!(output.contains("track(s)"));
    assert!(output.contains("no implemented effects found in this module")); // only effect present is unimplemented
}

#[test]
fn test_convert_verbose_reports_transcribed_effects() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), Some((0xA, 0x40))); // Volume Slide, implemented
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--verbose"]);

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

    let (code, output) = run(&["convert", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap()]);

    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("Verbose:"));
}

#[test]
fn test_convert_accepts_amiga_panning_flag() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_path = dir.path().join("Project").join("out.als");

    for preset in ["none", "light", "medium", "full"] {
        let (code, output) = run(&["convert", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--amiga-panning", preset]);
        assert_eq!(code, 0, "preset {preset}: {output}");
    }
}

#[test]
fn test_convert_rejects_invalid_amiga_panning_value() {
    let dir = tempfile::tempdir().unwrap();
    let mod_path = write_mod(dir.path(), None);
    let out_path = dir.path().join("Project").join("out.als");

    let (code, output) = run(&["convert", mod_path.to_str().unwrap(), "-o", out_path.to_str().unwrap(), "--amiga-panning", "bogus"]);
    assert_ne!(code, 0);
    assert!(output.contains("invalid value"));
}

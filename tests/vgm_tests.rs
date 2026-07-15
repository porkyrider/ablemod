//! Tests for the VGM/VGZ parser (formats::vgm) and the render/export pipeline built on top
//! of it, using small hand-built VGM byte streams (mirroring how protracker_tests.rs builds
//! synthetic .mod files) rather than a real fixture — real VGM rips are copyrighted game
//! audio, and the parser's correctness doesn't depend on any specific real file's content.

use std::io::Write;

use ablemod::formats::vgm::{self, Chip};

/// Builds a minimal but structurally real VGM v1.51 file (the version that added the AY8910
/// clock/type fields this project reads) with the given command stream, total sample count,
/// optional loop point (byte offset *within* `commands`), and GD3 title tag.
fn build_vgm(commands: &[u8], total_samples: u32, loop_at_command_offset: Option<u32>, title: &str) -> Vec<u8> {
    // Must clear the whole fixed-field header region this parser reads from (currently up
    // through the K051649/SCC clock at 0x9C) before the raw command stream begins — a
    // previous DATA_START of 0x80 left later fields (like 0x9C) landing inside `commands`
    // instead of the zeroed header, so they'd read back as garbage clock/flag values instead
    // of 0. That garbage once made a chip's native clock rate come back astronomically high,
    // turning a cheap 1-second synthetic render into an hours-long tick loop.
    const DATA_START: u32 = 0xC0;

    let gd3_start = DATA_START + commands.len() as u32;
    let mut gd3 = Vec::new();
    gd3.extend_from_slice(b"Gd3 ");
    gd3.extend_from_slice(&0x100u32.to_le_bytes());
    let mut str_bytes = Vec::new();
    // 11 fields: track EN/JP, game EN/JP, system EN/JP, author EN/JP, date, ripper, notes
    for s in [title, "", "", "", "", "", "", "", "", "", ""] {
        for c in s.encode_utf16() {
            str_bytes.extend_from_slice(&c.to_le_bytes());
        }
        str_bytes.extend_from_slice(&0u16.to_le_bytes());
    }
    gd3.extend_from_slice(&(str_bytes.len() as u32).to_le_bytes());
    gd3.extend_from_slice(&str_bytes);

    let total_len = gd3_start + gd3.len() as u32;

    let mut data = vec![0u8; DATA_START as usize];
    data[0..4].copy_from_slice(b"Vgm ");
    data[0x08..0x0C].copy_from_slice(&0x151u32.to_le_bytes());
    data[0x10..0x14].copy_from_slice(&3_579_545u32.to_le_bytes()); // YM2413 clock
    let gd3_offset_field = gd3_start - 0x14;
    data[0x14..0x18].copy_from_slice(&gd3_offset_field.to_le_bytes());
    data[0x18..0x1C].copy_from_slice(&total_samples.to_le_bytes());
    if let Some(off) = loop_at_command_offset {
        let loop_byte = DATA_START + off;
        let loop_offset_field = loop_byte - 0x1C;
        data[0x1C..0x20].copy_from_slice(&loop_offset_field.to_le_bytes());
        data[0x20..0x24].copy_from_slice(&(total_samples / 2).to_le_bytes()); // arbitrary but plausible
    }
    data[0x74..0x78].copy_from_slice(&1_789_773u32.to_le_bytes()); // AY8910 clock
    data[0x78] = 0; // plain AY-3-8910, not YM2149
    data[0x9C..0xA0].copy_from_slice(&1_789_772u32.to_le_bytes()); // K051649/SCC clock
    data[0x54..0x58].copy_from_slice(&3_579_545u32.to_le_bytes()); // YM3526 clock
    data[0x50..0x54].copy_from_slice(&3_579_545u32.to_le_bytes()); // YM3812 clock
    let vgm_data_offset_field = DATA_START - 0x34;
    data[0x34..0x38].copy_from_slice(&vgm_data_offset_field.to_le_bytes());
    let eof_offset_field = total_len - 0x04;
    data[0x04..0x08].copy_from_slice(&eof_offset_field.to_le_bytes());

    data.extend_from_slice(commands);
    data.extend_from_slice(&gd3);
    data
}

fn simple_commands() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&[0x51, 0x30, 0x00]); // YM2413 reg 0x30 = 0x00
    c.extend_from_slice(&[0x61, 100, 0]); // wait 100 samples
    c.extend_from_slice(&[0xA0, 0x08, 0x0F]); // AY8910 reg 8 = 0x0F
    c.extend_from_slice(&[0x61, 50, 0]); // wait 50 samples
    c.push(0x66); // end of sound data
    c
}

#[test]
fn test_parses_header_fields_and_gd3_tags() {
    let data = build_vgm(&simple_commands(), 150, None, "Test Song");
    let vgm = vgm::parse(&data).unwrap();

    assert_eq!(vgm.version, 0x151);
    assert_eq!(vgm.ym2413_clock, 3_579_545);
    assert_eq!(vgm.ay8910_clock, 1_789_773);
    assert!(!vgm.ay8910_is_ym);
    // simple_commands() never writes to the SCC (no 0xD2 command), so despite the header's
    // own K051649 clock field being nonzero (1_789_772, set below by build_vgm), scc_clock
    // comes back 0 — the header field alone isn't trusted as a presence flag, only an actual
    // write is (see formats::vgm::parse's own comment on why, and
    // test_ignores_a_nonzero_header_clock_for_a_chip_with_no_actual_writes below for the real
    // bug this guards against).
    assert_eq!(vgm.scc_clock, 0);
    assert_eq!(vgm.total_samples, 150);
    assert_eq!(vgm.loop_start_sample, None);
    assert_eq!(vgm.title.as_deref(), Some("Test Song"));
}

#[test]
fn test_parses_the_command_stream_with_correct_timing() {
    let data = build_vgm(&simple_commands(), 150, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    assert_eq!(vgm.writes.len(), 2);
    assert_eq!(vgm.writes[0].chip, Chip::Ym2413);
    assert_eq!(vgm.writes[0].reg, 0x30);
    assert_eq!(vgm.writes[0].value, 0x00);
    assert_eq!(vgm.writes[0].at_sample, 0); // before any wait

    assert_eq!(vgm.writes[1].chip, Chip::Ay8910);
    assert_eq!(vgm.writes[1].reg, 0x08);
    assert_eq!(vgm.writes[1].value, 0x0F);
    assert_eq!(vgm.writes[1].at_sample, 100); // after the first wait
}

#[test]
fn test_detects_the_loop_point_by_byte_offset() {
    // point the loop at the AY8910 write, 6 bytes into the command stream (after the
    // YM2413 write + the first "wait 100")
    let data = build_vgm(&simple_commands(), 150, Some(6), "t");
    let vgm = vgm::parse(&data).unwrap();

    assert_eq!(vgm.loop_start_sample, Some(100)); // matches the AY8910 write's own at_sample
}

#[test]
fn test_gunzips_vgz_transparently() {
    let data = build_vgm(&simple_commands(), 150, None, "Zipped");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&data).unwrap();
    let vgz = encoder.finish().unwrap();

    let vgm = vgm::parse(&vgz).unwrap();
    assert_eq!(vgm.title.as_deref(), Some("Zipped"));
    assert_eq!(vgm.writes.len(), 2);
}

#[test]
fn test_wait_command_variants_all_accumulate_correctly() {
    let mut c = Vec::new();
    c.extend_from_slice(&[0x51, 0x10, 0x01]);
    c.push(0x62); // wait 735
    c.extend_from_slice(&[0x51, 0x10, 0x02]);
    c.push(0x63); // wait 882
    c.extend_from_slice(&[0x51, 0x10, 0x03]);
    c.push(0x7F); // wait (0xF & 0x0F) + 1 = 16
    c.extend_from_slice(&[0x51, 0x10, 0x04]);
    c.push(0x66);
    let total = 735 + 882 + 16;

    let data = build_vgm(&c, total, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    assert_eq!(vgm.writes.len(), 4);
    assert_eq!(vgm.writes[0].at_sample, 0);
    assert_eq!(vgm.writes[1].at_sample, 735);
    assert_eq!(vgm.writes[2].at_sample, 735 + 882);
    assert_eq!(vgm.writes[3].at_sample, 735 + 882 + 16);
}

#[test]
fn test_rejects_a_file_without_the_vgm_magic() {
    let result = vgm::parse(b"not a vgm file at all, just some bytes");
    assert!(result.is_err());
}

#[test]
fn test_unsupported_chip_commands_are_skipped_not_misparsed() {
    let mut c = Vec::new();
    c.extend_from_slice(&[0x52, 0x00, 0x01]); // YM2612 port0 write — not emulated, but must skip cleanly
    c.extend_from_slice(&[0x51, 0x30, 0x00]); // YM2413 write right after — must still parse correctly
    c.push(0x66);

    let data = build_vgm(&c, 0, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    assert_eq!(vgm.writes.len(), 1); // only the YM2413 write is acted on
    assert_eq!(vgm.writes[0].chip, Chip::Ym2413);
    assert_eq!(*vgm.unsupported_commands.get(&0x52).unwrap(), 1);
}

#[test]
fn test_parses_scc_writes_with_their_port_byte() {
    // 0xD2 is a 4-byte command (cmd, port, reg, value) — distinct from every other command
    // this parser handles, which is why it needed its own arm rather than folding into the
    // generic "uniform 3/4-byte skip" cases. This is also what caught the real bug in
    // `fichiers/a dream of dreamer (ending theme).vgz` (an MSX Nemesis 2 rip using AY8910 +
    // K051649/SCC): the parser used to hard-error on the first 0xD2 byte it saw.
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 0x00, 0x05, 0xAB]); // waveform port, reg 5, value 0xAB
    c.extend_from_slice(&[0xD2, 0x01, 0x00, 0x63]); // frequency port, channel 0 low byte
    c.push(0x66);

    let data = build_vgm(&c, 0, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    assert_eq!(vgm.writes.len(), 2);
    assert_eq!(vgm.writes[0].chip, Chip::Scc);
    assert_eq!(vgm.writes[0].port, 0x00);
    assert_eq!(vgm.writes[0].reg, 0x05);
    assert_eq!(vgm.writes[0].value, 0xAB);
    assert_eq!(vgm.writes[1].port, 0x01);
    assert_eq!(vgm.writes[1].reg, 0x00);
    assert_eq!(vgm.writes[1].value, 0x63);
    assert!(vgm.unsupported_commands.is_empty());
}

#[test]
fn test_ignores_a_nonzero_header_clock_for_a_chip_with_no_actual_writes() {
    // Reproduces the real bug found on `fichiers/bubble.vgz` (Bubble Bobble, arcade): its
    // header's K051649 clock field is a nonzero, bogus value (1_534_215_296) even though the
    // file's entire command stream is YM3526 (an unemulated OPL FM chip) — zero 0xD2 writes
    // anywhere. Trusting the header field alone as a presence flag made `list`/`convert`
    // falsely report the SCC as present and "emulated by convert", producing a project with
    // no tracks and no samples with no indication why. build_vgm() always sets a nonzero SCC
    // header clock (see its own 0x9C write) regardless of whether `commands` has any 0xD2
    // writes, so simple_commands() (YM2413 + AY8910 only) already exercises exactly this case.
    let data = build_vgm(&simple_commands(), 150, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    assert!(!vgm.writes.iter().any(|w| w.chip == Chip::Scc));
    assert_eq!(vgm.scc_clock, 0, "a nonzero header clock alone must not mark the SCC as present");
}

fn tone_test_commands() -> Vec<u8> {
    // trigger an audible tone on both chips so render()/render_stems() have real signal to
    // isolate, not just silence
    let mut c = Vec::new();
    c.extend_from_slice(&[0xA0, 0x00, 0xC8]); // AY8910 channel A tone period low = 200
    c.extend_from_slice(&[0xA0, 0x07, 0b1111_1110]); // mixer: channel A tone on, rest off
    c.extend_from_slice(&[0xA0, 0x08, 0x0F]); // channel A volume = max, no envelope
    c.extend_from_slice(&[0x51, 0x00, 0x00]); // YM2413 modulator: silent
    c.extend_from_slice(&[0x51, 0x02, 0x3F]);
    c.extend_from_slice(&[0x51, 0x04, 0xFF]);
    c.extend_from_slice(&[0x51, 0x06, 0x00]);
    c.extend_from_slice(&[0x51, 0x01, 0x01]); // carrier ML=1
    c.extend_from_slice(&[0x51, 0x03, 0x00]);
    c.extend_from_slice(&[0x51, 0x05, 0xF8]);
    c.extend_from_slice(&[0x51, 0x07, 0x00]);
    c.extend_from_slice(&[0x51, 0x10, 0x90]); // fnum low
    c.extend_from_slice(&[0x51, 0x20, 0x18]); // key-on, block=4
    c.extend_from_slice(&[0x51, 0x30, 0x00]); // channel 0, instrument 0 (custom), volume 0
    c.extend_from_slice(&[0x61, 0x44, 0xAC]); // wait 44100 samples (1 second)
    c.push(0x66);
    c
}

#[test]
fn test_render_produces_the_declared_number_of_samples_with_no_nan() {
    let commands = tone_test_commands();
    let data = build_vgm(&commands, 44100, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    let audio = ablemod::export::vgm_render::render(&vgm);
    assert_eq!(audio.left.len(), 44100);
    assert_eq!(audio.right.len(), 44100);
    assert!(audio.left.iter().chain(audio.right.iter()).all(|x| x.is_finite()));
    let peak = ablemod::export::vgm_render::peak(&audio);
    assert!(peak > 0.0, "both chips are triggered — the render must not be silent");
}

#[test]
fn test_render_stems_isolates_exactly_the_channels_actually_used() {
    let commands = tone_test_commands();
    let data = build_vgm(&commands, 44100, None, "t");
    let vgm = vgm::parse(&data).unwrap();

    let stems = ablemod::export::vgm_render::render_stems(&vgm);
    let names: Vec<&str> = stems.iter().map(|s| s.name.as_str()).collect();
    // only AY channel A and YM channel 1 were ever triggered — every other channel/rhythm
    // voice must come back silent and therefore be omitted
    assert_eq!(names, vec!["AY-A", "YM-1"]);
}

#[test]
fn test_vgm_als_export_produces_a_well_formed_project() {
    let commands = tone_test_commands();
    let data = build_vgm(&commands, 44100, None, "Synthetic Song");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let tracks = ablemod::xmlutil::find_all_descendants(&root, "AudioTrack");
    assert_eq!(tracks.len(), stems.len()); // one per stem, no combined master-mix track

    // no loop point declared in this file — every track gets exactly one full-length clip
    for track in &tracks {
        let clips = ablemod::xmlutil::find_all_descendants(track, "AudioClip");
        assert_eq!(clips.len(), 1);
    }

    // every WAV the tracks reference must actually exist on disk
    for track in &tracks {
        let path = ablemod::xmlutil::find(track, ".//FileRef/Path").unwrap();
        let path_value = path.attributes.get("Value").unwrap();
        assert!(std::path::Path::new(path_value).exists(), "{path_value} should exist");
    }

    // no duplicate global (>=1000) Ids anywhere in the document
    let mut global_ids: Vec<String> = Vec::new();
    for node in ablemod::xmlutil::iter_elements(&root) {
        if let Some(id) = node.attributes.get("Id") {
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && id.parse::<i64>().unwrap() >= 1000 {
                global_ids.push(id.clone());
            }
        }
    }
    let unique: std::collections::HashSet<&String> = global_ids.iter().collect();
    assert_eq!(global_ids.len(), unique.len());
}

/// Emits a run of 0x61 "wait N samples" commands (each capped at the format's u16 limit)
/// totalling `samples`, returning nothing — callers just need the byte offset right before
/// calling this to land exactly on the first such command.
fn push_wait(c: &mut Vec<u8>, mut samples: u32) {
    while samples > 0 {
        let chunk = samples.min(0xFFFF);
        c.extend_from_slice(&[0x61, (chunk & 0xFF) as u8, (chunk >> 8) as u8]);
        samples -= chunk;
    }
}

/// Same tone setup as `tone_test_commands`, but split into an "intro" wait and a "loop" wait
/// so a test can point the VGM loop marker at the boundary between them. Returns the command
/// stream and the byte offset of that boundary (for `build_vgm`'s `loop_at_command_offset`).
fn looped_tone_test_commands(intro_samples: u32, loop_samples: u32) -> (Vec<u8>, u32) {
    let mut c = Vec::new();
    c.extend_from_slice(&[0xA0, 0x00, 0xC8]);
    c.extend_from_slice(&[0xA0, 0x07, 0b1111_1110]);
    c.extend_from_slice(&[0xA0, 0x08, 0x0F]);
    c.extend_from_slice(&[0x51, 0x00, 0x00]);
    c.extend_from_slice(&[0x51, 0x02, 0x3F]);
    c.extend_from_slice(&[0x51, 0x04, 0xFF]);
    c.extend_from_slice(&[0x51, 0x06, 0x00]);
    c.extend_from_slice(&[0x51, 0x01, 0x01]);
    c.extend_from_slice(&[0x51, 0x03, 0x00]);
    c.extend_from_slice(&[0x51, 0x05, 0xF8]);
    c.extend_from_slice(&[0x51, 0x07, 0x00]);
    c.extend_from_slice(&[0x51, 0x10, 0x90]);
    c.extend_from_slice(&[0x51, 0x20, 0x18]);
    c.extend_from_slice(&[0x51, 0x30, 0x00]);
    push_wait(&mut c, intro_samples);
    let loop_offset = c.len() as u32;
    push_wait(&mut c, loop_samples);
    c.push(0x66);
    (c, loop_offset)
}

#[test]
fn test_vgm_als_export_splits_stems_at_the_loop_point_and_derives_tempo() {
    // loop segment lasts exactly 3.0s -> smallest bar count reaching >=80 BPM is 1 bar
    // (4 beats * 60 / 3s = 80), so this also exercises the estimator's own formula precisely.
    let (commands, loop_offset) = looped_tone_test_commands(22050, 132300);
    let total = 22050 + 132300;
    let data = build_vgm(&commands, total, Some(loop_offset), "Looped Song");
    let vgm = vgm::parse(&data).unwrap();
    assert_eq!(vgm.loop_start_sample, Some(22050));

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);
    assert!(!stems.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let tracks = ablemod::xmlutil::find_all_descendants(&root, "AudioTrack");
    assert_eq!(tracks.len(), stems.len()); // still one track per stem, never a master track

    for track in &tracks {
        let clips = ablemod::xmlutil::find_all_descendants(track, "AudioClip");
        assert_eq!(clips.len(), 2, "expected an intro clip + a loop clip per track");
        let times: Vec<f64> = clips.iter().map(|c| c.attributes.get("Time").unwrap().parse().unwrap()).collect();
        assert!(times[0] < times[1], "clips must be placed back-to-back on the arrangement timeline");

        for file_ref in ablemod::xmlutil::find_all_descendants(track, "FileRef") {
            let path_value = ablemod::xmlutil::find(file_ref, "./Path").unwrap().attributes.get("Value").unwrap();
            assert!(std::path::Path::new(path_value).exists(), "{path_value} should exist");
        }
    }

    let live_set = ablemod::xmlutil::find(&root, "./LiveSet").unwrap();
    let tempo_value: f64 =
        ablemod::xmlutil::find(live_set, ".//Tempo/Manual").unwrap().attributes.get("Value").unwrap().parse().unwrap();
    assert!((tempo_value - 80.0).abs() < 0.01, "tempo={tempo_value}, expected ~80 BPM derived from the 3.0s loop");
}

#[test]
fn test_vgm_als_export_keeps_one_clip_per_stem_when_no_loop_point_declared() {
    let commands = tone_test_commands();
    let data = build_vgm(&commands, 44100, None, "No Loop");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    for track in ablemod::xmlutil::find_all_descendants(&root, "AudioTrack") {
        assert_eq!(ablemod::xmlutil::find_all_descendants(track, "AudioClip").len(), 1);
    }
}

#[test]
fn test_wavetable_export_recovers_the_waveform_despite_a_premature_key_toggle() {
    // Reproduces a real bug found in "a dream of dreamer (ending theme).vgz" (Nemesis 2,
    // MSX): the rip keys a channel on then immediately back off, *before* its waveform RAM is
    // loaded (driver-init boilerplate), which used to permanently poison the exported
    // wavetable to silence since it was captured on the *first* note-open rather than
    // refreshed on every one. Here: key on/off at t=0 with a still-zeroed waveform, then load
    // a real (non-zero) waveform, then a genuine note.
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 1, 0, 99]); // channel 0 frequency low = 99
    c.extend_from_slice(&[0xD2, 1, 1, 0]); // frequency high = 0 (freq_reg=99, > 8, audible)
    c.extend_from_slice(&[0xD2, 2, 0, 15]); // channel 0 volume = max
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // premature key-on, waveform still zero
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // immediate key-off, same instant
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(7).wrapping_add(1)]); // arbitrary non-zero waveform
    }
    c.extend_from_slice(&[0x61, 0x44, 0xAC]); // wait 44100 samples
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // the real key-on
    c.extend_from_slice(&[0x61, 0x44, 0xAC]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key-off
    c.push(0x66);

    let data = build_vgm(&c, 88200, None, "Wavetable Bug");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1, "exactly one SCC channel ever keyed a real note");
    let name = ablemod::xmlutil::find(midi_tracks[0], ".//Name/EffectiveName").unwrap().attributes.get("Value").unwrap();
    assert_eq!(name, "SCC-1 (Wavetable)");

    // exactly one real note — the zero-duration premature key toggle must not appear
    let note_count: usize = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "KeyTrack")
        .iter()
        .map(|kt| ablemod::xmlutil::find_all_descendants(kt, "MidiNoteEvent").len())
        .sum();
    assert_eq!(note_count, 1);

    let wav_path =
        ablemod::xmlutil::find(midi_tracks[0], ".//UserSprite1/Value/SampleRef/FileRef/Path").unwrap().attributes.get("Value").unwrap();
    let mut reader = hound::WavReader::open(wav_path).unwrap();
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len(), 1024, "the 32-sample source cycle is held/stretched to Ableton's expected wavetable frame size");
    assert!(samples.iter().any(|&s| s != 0), "the captured waveform must be the real one, not the premature all-zero snapshot");
    // sample-and-hold, not interpolated: each of the 32 source samples repeats for exactly 32
    // consecutive output samples, so the value never changes partway through a hold block
    for block in samples.chunks(32) {
        assert!(block.iter().all(|&s| s == block[0]), "expected a flat sample-and-hold block, got {block:?}");
    }
}

#[test]
fn test_wavetable_export_captures_a_mid_note_waveform_rewrite_as_a_second_frame() {
    // Some SCC compositions rewrite waveform RAM *while a note is held* to fake an envelope
    // the chip has no dedicated generator for — this must show up as a second frame in the
    // exported wavetable file, with a Position automation point switching to it at the right
    // time, not just the note's *first* waveform forever (the earlier, simpler behavior).
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 15]);
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]); // waveform A
    }
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on
    c.extend_from_slice(&[0x61, 0x44, 0xAC]); // wait 44100 samples, note still held
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(5).wrapping_add(2)]); // waveform B, mid-note
    }
    c.extend_from_slice(&[0x61, 0x44, 0xAC]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off
    c.push(0x66);

    let data = build_vgm(&c, 88200, None, "Multi Frame");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let wav_path =
        ablemod::xmlutil::find(midi_tracks[0], ".//UserSprite1/Value/SampleRef/FileRef/Path").unwrap().attributes.get("Value").unwrap();
    let mut reader = hound::WavReader::open(wav_path).unwrap();
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len(), 2048, "two distinct frames, 1024 samples each");
    assert_ne!(&samples[0..32], &samples[1024..1056], "the two frames must actually differ");

    // a Position automation envelope switching from 0.0 to 1.0 partway through
    let float_values: Vec<f64> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "FloatEvent")
        .iter()
        .filter_map(|e| e.attributes.get("Value").and_then(|v| v.parse().ok()))
        .collect();
    assert!(float_values.iter().any(|&v| v == 0.0), "no baseline/first-frame (0.0) automation point found");
    assert!(float_values.iter().any(|&v| v == 1.0), "no second-frame (1.0) automation point found");
}

#[test]
fn test_wavetable_export_leaves_a_gap_between_legato_retriggered_notes() {
    // Many SCC rips keep a channel's key held continuously across a whole phrase and just
    // rewrite the frequency register for each new note (chip-style legato) — two notes
    // produced this way must not butt up exactly against each other, or Wavetable's envelope
    // has nothing to retrigger against and the note change becomes inaudible (reported
    // directly: "je n'entends plus les changements de notes").
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 15]);
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on, note A
    c.extend_from_slice(&[0x61, 0x44, 0xAC]); // wait 44100 samples (1s)
    c.extend_from_slice(&[0xD2, 1, 0, 149]); // frequency change while key stays held: note B
    c.extend_from_slice(&[0x61, 0x44, 0xAC]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off
    c.push(0x66);

    let data = build_vgm(&c, 88200, None, "Legato");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let mut notes: Vec<(f64, f64)> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent")
        .iter()
        .map(|e| {
            let time: f64 = e.attributes.get("Time").unwrap().parse().unwrap();
            let duration: f64 = e.attributes.get("Duration").unwrap().parse().unwrap();
            (time, duration)
        })
        .collect();
    notes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(notes.len(), 2, "expected exactly two retriggered notes");

    let (start_a, duration_a) = notes[0];
    let (start_b, _duration_b) = notes[1];
    let end_a = start_a + duration_a;
    assert!(end_a < start_b, "note A (ends {end_a}) must leave a gap before note B (starts {start_b}), not touch it exactly");
}

#[test]
fn test_wavetable_export_merges_a_leading_burst_of_driver_init_glitch_spans() {
    // Reproduces the exact failure mode reported directly: "tu commences sur la deuxième
    // position au lieu de la première" — a real rip's driver-init boilerplate opened and
    // closed a channel's key several times within a handful of *samples* (a fraction of a
    // millisecond, nowhere near a real note), each one technically committing its own
    // waveform-frame snapshot; since those glitch spans got superseded within microseconds,
    // the frame that was actually audible from the start was really the *second* one. Every
    // one of these glitch spans must be folded away before frames/notes are ever built.
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 15]);
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]); // glitch waveform A
    }
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on (glitch)
    c.extend_from_slice(&[0x70]); // wait 1 sample
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off (glitch)
    c.extend_from_slice(&[0x70]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on again (still glitching)
    c.extend_from_slice(&[0x70]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(5).wrapping_add(2)]); // the *real* waveform B
    }
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // the real note
    c.extend_from_slice(&[0x61, 0x44, 0xAC]); // wait 44100 samples
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]);
    c.push(0x66);

    let data = build_vgm(&c, 44110, None, "Glitch Burst");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let notes = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent");
    assert_eq!(notes.len(), 1, "the glitch burst must be merged away, leaving only the one real note");

    // exactly two AutomationEnvelopes (Gain and Pitch/Transpose, from the one surviving note)
    // — no Position automation, since a single surviving frame needs none
    assert_eq!(ablemod::xmlutil::find_all_descendants(midi_tracks[0], "AutomationEnvelope").len(), 2);

    let wav_path =
        ablemod::xmlutil::find(midi_tracks[0], ".//UserSprite1/Value/SampleRef/FileRef/Path").unwrap().attributes.get("Value").unwrap();
    let mut reader = hound::WavReader::open(wav_path).unwrap();
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len(), 1024, "exactly one frame — the real waveform B, not the glitch waveform A");
    // waveform B's first byte (i=0) is 0*5+2=2, scaled 8->16 bit
    assert_eq!(samples[0], 2i16 << 8);
}

#[test]
fn test_wavetable_export_automates_gain_from_the_volume_register() {
    // The real chip directly multiplies its waveform output by the volume register — Ableton's
    // Wavetable has no built-in routing from MIDI velocity to any gain parameter by default,
    // so without an explicit Gain automation every note plays at the same level regardless of
    // the chip's own volume register (reported directly: "le volume de l'onde reste... pas
    // comme sur le chip").
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 2, 0, 15]); // channel 0 volume = max
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // note A, loud
    c.extend_from_slice(&[0x61, 0x44, 0xAC]); // wait 44100 samples
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]);
    c.push(0x70); // wait 1 sample, so this key-off and the next key-on land in separate groups
    c.extend_from_slice(&[0xD2, 2, 0, 3]); // channel 0 volume = quiet
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // note B, quiet
    c.extend_from_slice(&[0x61, 0x44, 0xAC]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]);
    c.push(0x66);

    let data = build_vgm(&c, 88200, None, "Volume Automation");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let gain_target_id =
        ablemod::xmlutil::find(midi_tracks[0], ".//Voice_Oscillator1_Gain/AutomationTarget").unwrap().attributes.get("Id").cloned().unwrap();

    let mut found_gain_envelope = false;
    for env in ablemod::xmlutil::find_all_descendants(midi_tracks[0], "AutomationEnvelope") {
        let pointee = ablemod::xmlutil::find(env, "./EnvelopeTarget/PointeeId").unwrap().attributes.get("Value").cloned().unwrap();
        if pointee != gain_target_id {
            continue;
        }
        found_gain_envelope = true;
        let values: Vec<f64> = ablemod::xmlutil::find_all_descendants(env, "FloatEvent")
            .iter()
            .filter_map(|e| e.attributes.get("Value").and_then(|v| v.parse().ok()))
            .collect();
        // volume=15 -> velocity 127 -> gain 1.0; volume=3 -> velocity floor(3*127/15)=25 -> gain ~0.197
        assert!(values.iter().any(|&v| (v - 1.0).abs() < 0.01), "expected a full-gain (1.0) point for the loud note, got {values:?}");
        assert!(values.iter().any(|&v| v > 0.0 && v < 0.3), "expected a low-gain point for the quiet note, got {values:?}");
    }
    assert!(found_gain_envelope, "no Gain automation envelope found");
}

#[test]
fn test_wavetable_export_absorbs_vibrato_as_pitch_bend_instead_of_retriggering() {
    // Small frequency wobbles around a note's center (software vibrato, or just the chip's
    // exact frequency not landing on an equal-tempered semitone) must become Pitch/Transpose
    // automation on the *one* held note, not a burst of retriggered notes.
    let mut c = Vec::new();
    c.extend_from_slice(&[0xD2, 2, 0, 15]);
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    // freq_reg=99 -> ~559Hz, nearest note C#5 (73) but ~15 cents sharp of it — a genuine
    // tuning residual even before any wobbling starts.
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on
    for &freq in &[100u32, 98, 100, 99] {
        c.extend_from_slice(&[0x61, 0x00, 0x22]); // wait ~8700 samples between wobble steps
        c.extend_from_slice(&[0xD2, 1, 0, (freq & 0xFF) as u8]);
        c.extend_from_slice(&[0xD2, 1, 1, ((freq >> 8) & 0x0F) as u8]);
    }
    c.extend_from_slice(&[0x61, 0x00, 0x22]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off
    c.push(0x66);

    let data = build_vgm(&c, 44100, None, "Vibrato");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let notes = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent");
    assert_eq!(notes.len(), 1, "the vibrato wobble must not retrigger separate notes");

    let transpose_target_id =
        ablemod::xmlutil::find(midi_tracks[0], ".//Voice_Oscillator1_Pitch_Detune/AutomationTarget").unwrap().attributes.get("Id").cloned().unwrap();
    let mut bend_values: Vec<f64> = Vec::new();
    for env in ablemod::xmlutil::find_all_descendants(midi_tracks[0], "AutomationEnvelope") {
        let pointee = ablemod::xmlutil::find(env, "./EnvelopeTarget/PointeeId").unwrap().attributes.get("Value").cloned().unwrap();
        if pointee != transpose_target_id {
            continue;
        }
        bend_values = ablemod::xmlutil::find_all_descendants(env, "FloatEvent")
            .iter()
            .filter_map(|e| e.attributes.get("Value").and_then(|v| v.parse().ok()))
            .collect();
    }
    // build_automation_envelope always prepends one baseline point (value 0.0) at its sentinel
    // time, ahead of the real tuning-residual point + 4 wobble points.
    assert_eq!(bend_values.len(), 6, "{bend_values:?}");
    assert!(bend_values.iter().any(|&v| v.abs() > 0.01), "expected a nonzero tuning residual/wobble, got {bend_values:?}");
    // must actually oscillate (both a local max and a local min among the interior points),
    // not just ramp monotonically in one direction
    let increases = bend_values.windows(2).filter(|w| w[1] > w[0]).count();
    let decreases = bend_values.windows(2).filter(|w| w[1] < w[0]).count();
    assert!(increases > 0 && decreases > 0, "expected the pitch to wobble up and down, got {bend_values:?}");
}

#[test]
fn test_wavetable_export_tracks_a_volume_envelope_rewritten_across_a_held_note() {
    // Reproduces the exact pattern found in "a dream of dreamer": the volume register gets
    // rewritten repeatedly across one held note (attack ramp up, then a decay ramp down) to
    // fake an amplitude envelope the chip has no generator for. A single snapshot at note-on
    // (the earlier behavior) caught an arbitrary point on that ramp instead of the shape.
    let mut c = Vec::new();
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 8]); // attack starts at 8
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on
    for &vol in &[9u8, 10, 9, 8, 7, 6, 5, 4, 3, 2] {
        c.extend_from_slice(&[0x61, 0x00, 0x22]); // wait ~8700 samples between envelope steps
        c.extend_from_slice(&[0xD2, 2, 0, vol]);
    }
    c.extend_from_slice(&[0x61, 0x00, 0x22]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off
    c.push(0x66);

    let data = build_vgm(&c, 100000, None, "Volume Envelope");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let notes = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent");
    assert_eq!(notes.len(), 1, "the volume-only envelope rewrite must not retrigger separate notes");

    let gain_target_id =
        ablemod::xmlutil::find(midi_tracks[0], ".//Voice_Oscillator1_Gain/AutomationTarget").unwrap().attributes.get("Id").cloned().unwrap();
    let mut gain_values: Vec<f64> = Vec::new();
    for env in ablemod::xmlutil::find_all_descendants(midi_tracks[0], "AutomationEnvelope") {
        let pointee = ablemod::xmlutil::find(env, "./EnvelopeTarget/PointeeId").unwrap().attributes.get("Value").cloned().unwrap();
        if pointee != gain_target_id {
            continue;
        }
        gain_values = ablemod::xmlutil::find_all_descendants(env, "FloatEvent")
            .iter()
            .filter_map(|e| e.attributes.get("Value").and_then(|v| v.parse().ok()))
            .collect();
    }
    // baseline (0.0) + 11 register writes (8,9,10,9,8,7,6,5,4,3,2) all captured, not just one
    assert_eq!(gain_values.len(), 12, "{gain_values:?}");
    assert!((gain_values[1] - 8.0 / 15.0).abs() < 0.01, "expected the attack-start gain, got {gain_values:?}");
    assert!((gain_values[3] - 10.0 / 15.0).abs() < 0.01, "expected the peak (vol=10) gain, got {gain_values:?}");
    assert!((gain_values.last().unwrap() - 2.0 / 15.0).abs() < 0.01, "expected the decayed-away (vol=2) gain, got {gain_values:?}");
}

#[test]
fn test_wavetable_export_splits_a_note_on_volume_re_attack_without_a_key_toggle() {
    // Reproduces the exact pattern traced directly in "a dream of dreamer": a channel
    // rearticulates a repeated note purely by resetting its volume envelope back up to an
    // attack level (8→9→10→…→2, then straight back to 8) — the key never toggles and the
    // frequency never changes, so without this check it reads as one long note quietly
    // swelling back up mid-sustain instead of two separate, repeated notes.
    let mut c = Vec::new();
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 8]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // key on, held for the whole file
    for &vol in &[9u8, 10, 9, 8, 7, 6, 5, 4, 3, 2, 8, 9, 10, 9, 8] {
        c.extend_from_slice(&[0x61, 0x00, 0x22]); // wait ~8700 samples between envelope steps
        c.extend_from_slice(&[0xD2, 2, 0, vol]);
    }
    c.extend_from_slice(&[0x61, 0x00, 0x22]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off
    c.push(0x66);

    let data = build_vgm(&c, 150000, None, "Volume Re-Attack");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let mut notes: Vec<(f64, f64)> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent")
        .iter()
        .map(|e| (e.attributes.get("Time").unwrap().parse().unwrap(), e.attributes.get("Duration").unwrap().parse().unwrap()))
        .collect();
    notes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(notes.len(), 2, "the volume re-attack must split this into two notes, not one long swelling note");

    let (start_a, duration_a) = notes[0];
    let (start_b, _) = notes[1];
    assert!(start_a + duration_a < start_b, "the split must leave a real gap, same as any other retrigger");
}

#[test]
fn test_wavetable_export_coalesces_a_note_onset_split_across_two_register_writes() {
    // Reproduces a pattern traced directly on SCC-4: a note's fresh pitch and its fresh-attack
    // volume arrive as two separate register writes only ~15 samples apart — still one
    // logical note-onset event, not a real note followed by a near-instant retrigger. Without
    // coalescing, the pitch-change split opens a throwaway ~15-sample span that the volume
    // re-attack then immediately closes again; that throwaway span is short enough for
    // merge_short_spans to fold away, along with the note's true onset time.
    let mut c = Vec::new();
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 8]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // note A: freq_reg=99, vol=8
    push_wait(&mut c, 2000);
    c.extend_from_slice(&[0xD2, 1, 0, 199]); // pitch change: freq_reg=199, a different note
    push_wait(&mut c, 15); // the same onset event's volume write, a handful of samples later
    c.extend_from_slice(&[0xD2, 2, 0, 15]); // fresh-attack volume: 8 -> 15, a re-attack-sized jump
    push_wait(&mut c, 30000);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]);
    c.push(0x66);

    let data = build_vgm(&c, 40000, None, "Coalesced Onset");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let mut notes: Vec<(f64, f64, String)> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent")
        .iter()
        .map(|e| {
            (
                e.attributes.get("Time").unwrap().parse().unwrap(),
                e.attributes.get("Duration").unwrap().parse().unwrap(),
                e.attributes.get("Velocity").unwrap().clone(),
            )
        })
        .collect();
    notes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(notes.len(), 2, "exactly two real notes — no phantom third span from the split onset, {notes:?}");

    // note B's onset must land at the pitch-change's own timestamp (300 samples), not 15
    // samples later where the volume write happened to land
    let expected_start_beat = 2000.0 / 44100.0 * 120.0 / 60.0;
    assert!((notes[1].0 - expected_start_beat).abs() < 0.001, "expected note B at {expected_start_beat}, got {:?}", notes[1]);
    // and it must carry the *fresh* attack velocity (vol=15), not the stale vol=8 from note A
    assert_eq!(notes[1].2, "127", "expected the coalesced note to carry the fresh-attack velocity, got {notes:?}");
}

#[test]
fn test_wavetable_export_recovers_a_note_after_a_frequency_byte_split_across_groups() {
    // Reproduces a real bug traced directly on SCC-4: the frequency register's low and high
    // bytes sometimes arrive as two separate writes a couple of samples apart (not in the
    // same group) rather than together. The low byte alone can momentarily read as a
    // register value <=8 — genuinely "chip halted" on real hardware — correctly silencing
    // the note. But once the high byte lands moments later restoring a valid frequency, the
    // channel is still logically keyed and must resume as the same held note; without this
    // fix the channel stayed silent for the rest of that phrase (over 5 real seconds, in the
    // traced file) because only an actual key-on normally reopens a span.
    let mut c = Vec::new();
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 1, 0, 99]);
    c.extend_from_slice(&[0xD2, 1, 1, 0]);
    c.extend_from_slice(&[0xD2, 2, 0, 15]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // note A: freq_reg=99, held
    push_wait(&mut c, 2000);
    c.extend_from_slice(&[0xD2, 1, 0, 5]); // low byte only: freq_reg momentarily 5 (halted)
    push_wait(&mut c, 2);
    c.extend_from_slice(&[0xD2, 1, 1, 1]); // high byte lands moments later: freq_reg=261 (valid)
    push_wait(&mut c, 2000);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off, only now
    c.push(0x66);

    let data = build_vgm(&c, 5000, None, "Frequency Byte Split");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let mut notes: Vec<(f64, f64)> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent")
        .iter()
        .map(|e| (e.attributes.get("Time").unwrap().parse().unwrap(), e.attributes.get("Duration").unwrap().parse().unwrap()))
        .collect();
    notes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(notes.len(), 2, "note A, then the recovered note after the frequency byte split, {notes:?}");

    // the recovered note must actually have real duration (audible, roughly matching note A's
    // own ~2000-sample hold once gap-trimmed), not be swallowed down to nothing
    let (_, duration_a) = notes[0];
    let (_, duration_b) = notes[1];
    assert!(duration_b > 0.03, "the recovered note must span its full remaining hold, got {notes:?}");
    assert!((duration_b - duration_a).abs() < 0.01, "expected a duration close to note A's own, got {notes:?}");
}

#[test]
fn test_wavetable_export_ignores_a_mid_note_frequency_byte_split_transient() {
    // Reproduces the exact pattern traced directly in "a dream of dreamer" on SCC-4, but
    // *mid-note* rather than at note-onset: frequency 264 (low=8,high=1) transitions to 236
    // (low=236,high=0) by writing the low byte first — briefly combining with the still-stale
    // high byte into a nonsense intermediate value (492) — then the high byte lands a few
    // samples later, settling on the real target. Without waiting for the pair to settle,
    // that transient 492 gets evaluated as its own pitch and produces a spurious extra note;
    // only the genuine 264->236 transition should exist.
    let mut c = Vec::new();
    for i in 0..32u8 {
        c.extend_from_slice(&[0xD2, 0, i, i.wrapping_mul(3).wrapping_add(1)]);
    }
    c.extend_from_slice(&[0xD2, 1, 0, 8]);
    c.extend_from_slice(&[0xD2, 1, 1, 1]); // freq_reg = 264
    c.extend_from_slice(&[0xD2, 2, 0, 15]);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0001]); // note A: freq_reg=264, held
    push_wait(&mut c, 2000);
    c.extend_from_slice(&[0xD2, 1, 0, 236]); // low byte only: transient freq_reg=492 (wrong)
    push_wait(&mut c, 3);
    c.extend_from_slice(&[0xD2, 1, 1, 0]); // high byte lands: freq_reg=236 (the real target)
    push_wait(&mut c, 2000);
    c.extend_from_slice(&[0xD2, 3, 0, 0b0000_0000]); // key off, only now
    c.push(0x66);

    let data = build_vgm(&c, 5000, None, "Mid-Note Frequency Byte Split");
    let vgm = vgm::parse(&data).unwrap();

    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let mut notes: Vec<(f64, f64)> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "MidiNoteEvent")
        .iter()
        .map(|e| (e.attributes.get("Time").unwrap().parse().unwrap(), e.attributes.get("Duration").unwrap().parse().unwrap()))
        .collect();
    notes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(notes.len(), 2, "the transient intermediate value must not produce its own spurious note, {notes:?}");
}

fn opl_write(c: &mut Vec<u8>, reg: u8, value: u8) {
    c.extend_from_slice(&[0x5B, reg, value]);
}

/// Loads a full 2-operator FM patch onto OPL channel 0 (multiple/TL/AR/DR/SL/RR for both
/// operators, plus feedback/connection) — the register writes every OPL test below starts
/// from, so each test only has to show the specific frequency/key/TL sequence it's actually
/// exercising.
fn opl_load_patch_channel0(c: &mut Vec<u8>) {
    opl_write(c, 0x20, 1); // modulator multiple = 1
    opl_write(c, 0x23, 2); // carrier multiple = 2
    opl_write(c, 0x40, 10); // modulator total level
    opl_write(c, 0x43, 20); // carrier total level
    opl_write(c, 0x60, 0xA5); // modulator AR=10 DR=5
    opl_write(c, 0x63, 0xC3); // carrier AR=12 DR=3
    opl_write(c, 0x80, 0x46); // modulator SL=4 RR=6
    opl_write(c, 0x83, 0x28); // carrier SL=2 RR=8
    opl_write(c, 0xC0, 0x0A); // feedback=5, connection=FM
}

fn export_operator_project(commands: &[u8], total_samples: u32, title: &str) -> xmltree::Element {
    let data = build_vgm(commands, total_samples, None, title);
    let vgm = vgm::parse(&data).unwrap();
    let master = ablemod::export::vgm_render::render(&vgm);
    let stems = ablemod::export::vgm_render::render_stems(&vgm);

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    ablemod::export::vgm_als::export_als(&vgm, &master, &stems, &output, ablemod::export::als::default_template_bytes()).unwrap();

    let bytes = std::fs::read(&output).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    xmltree::Element::parse(xml.as_bytes()).unwrap()
}

fn key_track_note_counts(midi_track: &xmltree::Element) -> Vec<(i32, usize)> {
    ablemod::xmlutil::find_all_descendants(midi_track, "KeyTrack")
        .iter()
        .map(|kt| {
            let pitch = ablemod::xmlutil::find(kt, "./MidiKey").unwrap().attributes.get("Value").unwrap().parse().unwrap();
            let count = ablemod::xmlutil::find_all_descendants(kt, "MidiNoteEvent").len();
            (pitch, count)
        })
        .filter(|&(_, count)| count > 0)
        .collect()
}

#[test]
fn test_operator_export_produces_a_note_with_the_correct_pitch_and_bakes_the_static_patch() {
    // fnum=580, block=4 at the standard 3579545Hz OPL clock lands almost exactly on A4 (440Hz)
    // — Hz = clock * fnum / (2^(20-block) * 72), the standard OPL frequency formula.
    let mut c = Vec::new();
    opl_load_patch_channel0(&mut c);
    opl_write(&mut c, 0xA0, 0x44); // fnum low byte (fnum=580 -> low=0x44)
    opl_write(&mut c, 0xB0, 0x32); // key-on, block=4, fnum hi=2
    push_wait(&mut c, 4410);
    opl_write(&mut c, 0xB0, 0x12); // key-off, same block/fnum-hi
    c.push(0x66);

    let root = export_operator_project(&c, 8820, "OPL Note");

    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1, "only OPL channel 0 was ever keyed");
    let name = ablemod::xmlutil::find(midi_tracks[0], ".//Name/EffectiveName").unwrap().attributes.get("Value").unwrap();
    assert_eq!(name, "OPL-1 (Operator)");

    let notes = key_track_note_counts(midi_tracks[0]);
    assert_eq!(notes, vec![(69, 1)], "expected exactly one A4 note, got {notes:?}");

    // Operator.0 = carrier (A), Operator.1 = modulator (B) — see build_operator_track's own
    // comment on why the roles are assigned that way.
    let get = |path: &str| ablemod::xmlutil::find(midi_tracks[0], path).unwrap().attributes.get("Value").unwrap().clone();
    assert_eq!(get(".//Globals/Algorithm/Manual"), "0");
    assert_eq!(get(".//Operator.0/IsOn/Manual"), "true");
    assert_eq!(get(".//Operator.1/IsOn/Manual"), "true");
    assert_eq!(get(".//Operator.2/IsOn/Manual"), "false", "C must be off for a clean 2-operator FM pair");
    assert_eq!(get(".//Operator.3/IsOn/Manual"), "false", "D must be off for a clean 2-operator FM pair");
    assert_eq!(get(".//Operator.0/WaveForm/Manual"), "0", "sine — YM3526 has no other waveform");
    assert_eq!(get(".//Operator.1/WaveForm/Manual"), "0");
    assert_eq!(get(".//Operator.0/Tune/Coarse/Manual"), "2", "carrier multiple");
    assert_eq!(get(".//Operator.1/Tune/Coarse/Manual"), "1", "modulator multiple");
    assert_eq!(get(".//Operator.1/Feedback/Manual"), "71.428571", "OPL feedback=5 of 7, rescaled to Operator's 0-100 range");
    assert_eq!(get(".//Operator.0/Feedback/Manual"), "0", "OPL feedback only ever applies to the modulator");
}

#[test]
fn test_operator_export_hard_retriggers_on_a_held_note_frequency_change() {
    // v1 has no bend-vs-new-note absorption (unlike SCC): any change to a held note's own
    // fnum/block always closes the current note and opens a new one — see
    // export::vgm_operator's own doc comment on this deliberate scope cut.
    let mut c = Vec::new();
    opl_load_patch_channel0(&mut c);
    opl_write(&mut c, 0xA0, 0x44); // fnum low (fnum=580, block=4 -> A4)
    opl_write(&mut c, 0xB0, 0x32); // key-on
    push_wait(&mut c, 2000);
    opl_write(&mut c, 0xA0, 0x00); // fnum low changes to 512 (still block=4, fnum hi=2) while held
    push_wait(&mut c, 2000);
    opl_write(&mut c, 0xB0, 0x12); // key-off
    c.push(0x66);

    let root = export_operator_project(&c, 8000, "OPL Retrigger");
    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);
    let notes = key_track_note_counts(midi_tracks[0]);
    let total: usize = notes.iter().map(|&(_, n)| n).sum();
    assert_eq!(total, 2, "the mid-hold frequency change must hard-retrigger into a second note, got {notes:?}");
}

#[test]
fn test_operator_export_automates_gain_from_the_carrier_total_level_register() {
    // Mirrors build_wavetable_track's identical Gain-automation rationale: many rips rewrite
    // the carrier's Total Level across a note's whole sustain to fake a volume envelope, so
    // it's tracked continuously rather than sampled once at note-on.
    let mut c = Vec::new();
    opl_load_patch_channel0(&mut c);
    opl_write(&mut c, 0xA0, 0x44);
    opl_write(&mut c, 0xB0, 0x32); // key-on
    push_wait(&mut c, 1000);
    opl_write(&mut c, 0x43, 30); // carrier total level rewritten while held (quieter)
    push_wait(&mut c, 1000);
    opl_write(&mut c, 0x43, 40); // rewritten again (quieter still)
    push_wait(&mut c, 1000);
    opl_write(&mut c, 0xB0, 0x12); // key-off
    c.push(0x66);

    let root = export_operator_project(&c, 4000, "OPL Gain Envelope");
    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);

    let notes = key_track_note_counts(midi_tracks[0]);
    assert_eq!(notes.len(), 1, "TL-only changes must not retrigger the note");

    let gain_events: Vec<f64> = ablemod::xmlutil::find_all_descendants(midi_tracks[0], "AutomationEnvelope")
        .iter()
        .flat_map(|env| ablemod::xmlutil::find_all_descendants(env, "FloatEvent"))
        .map(|e| e.attributes.get("Value").unwrap().parse().unwrap())
        .collect();
    // sentinel + 3 real TL snapshots (note-on + 2 rewrites)
    assert_eq!(gain_events.len(), 4, "{gain_events:?}");
    // total level increasing (30, then 40) is *quieter* on real hardware (0=loudest) — gain
    // must decrease monotonically across the two rewrites
    assert!(gain_events[2] > gain_events[3], "expected decreasing gain as TL increases, got {gain_events:?}");
}

#[test]
fn test_operator_export_also_decodes_ym3812_through_the_same_pipeline() {
    // YM3812 (OPL2, cmd 0x5A) is register-compatible with YM3526 (OPL, cmd 0x5B) for every
    // field export::vgm_operator reads — same extraction pipeline, kept as an independent chip
    // presence/track set (see vgm_als's own comment) so a file using *both* still gets two
    // clearly labeled sets of tracks instead of colliding into one.
    let mut c = Vec::new();
    c.extend_from_slice(&[0x20, 1]); // modulator multiple = 1
    c.extend_from_slice(&[0x23, 2]); // carrier multiple = 2
    c.extend_from_slice(&[0x40, 10]);
    c.extend_from_slice(&[0x43, 20]);
    c.extend_from_slice(&[0x60, 0xA5]);
    c.extend_from_slice(&[0x63, 0xC3]);
    c.extend_from_slice(&[0x80, 0x46]);
    c.extend_from_slice(&[0x83, 0x28]);
    c.extend_from_slice(&[0xC0, 0x0A]);
    c.extend_from_slice(&[0xA0, 0x44]);
    c.extend_from_slice(&[0xB0, 0x32]); // key-on
    let mut commands = Vec::new();
    for chunk in c.chunks(2) {
        commands.push(0x5A);
        commands.extend_from_slice(chunk);
    }
    push_wait(&mut commands, 4410);
    commands.extend_from_slice(&[0x5A, 0xB0, 0x12]); // key-off
    commands.push(0x66);

    let root = export_operator_project(&commands, 8820, "OPL2 Note");
    let midi_tracks = ablemod::xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 1);
    let name = ablemod::xmlutil::find(midi_tracks[0], ".//Name/EffectiveName").unwrap().attributes.get("Value").unwrap();
    assert_eq!(name, "OPL2-1 (Operator)");

    let notes = key_track_note_counts(midi_tracks[0]);
    assert_eq!(notes, vec![(69, 1)], "expected exactly one A4 note, got {notes:?}");
}

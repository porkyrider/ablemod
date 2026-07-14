mod common;

use std::collections::HashSet;
use std::io::Read;

use ablemod::export::als::{default_template_bytes, export_als, volume_to_gain, AmigaPanning, TRACK_VOLUME_DB};
use ablemod::export::midi::write_midi;
use ablemod::export::notes::{compute_song_events, BEATS_PER_ROW};
use ablemod::formats::base::{Cell, Module, Pattern, Sample};
use ablemod::formats::protracker::{parse, unimplemented_effect_counts};
use ablemod::xmlutil;
use xmltree::Element;

fn read_als(path: &std::path::Path) -> Element {
    let bytes = std::fs::read(path).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut xml = String::new();
    decoder.read_to_string(&mut xml).unwrap();
    Element::parse(xml.as_bytes()).unwrap()
}

fn attr(el: &Element, name: &str) -> String {
    el.attributes.get(name).cloned().unwrap_or_else(|| panic!("missing attribute {name} on <{}>", el.name))
}

#[test]
fn test_default_template_is_bundled_and_usable_without_specifying_one() {
    assert!(!default_template_bytes().is_empty());

    let module = parse(&std::fs::read("tests/fixtures/4aces-high.mod").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap(); // no template_path passed at all

    let root = read_als(&output);
    let tracks = xmlutil::find_all_descendants(&root, "MidiTrack");
    let non_empty_samples: Vec<_> = module.samples.iter().filter(|s| !s.is_empty()).collect();
    assert_eq!(tracks.len(), non_empty_samples.len());
}

#[test]
fn test_export_als_against_real_template() {
    let module = parse(&std::fs::read("tests/fixtures/4aces-high.mod").unwrap());
    let template = std::fs::read("tests/fixtures/Template Glissando Project/Template Glissando.als").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, &template, AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let tracks = xmlutil::find_all_descendants(&root, "MidiTrack");
    let non_empty_samples: Vec<_> = module.samples.iter().filter(|s| !s.is_empty()).collect();
    assert_eq!(tracks.len(), non_empty_samples.len());

    let ids: Vec<String> = tracks.iter().map(|t| attr(t, "Id")).collect();
    let unique_ids: HashSet<&String> = ids.iter().collect();
    assert_eq!(ids.len(), unique_ids.len()); // track ids must be unique

    // every "global" Id (>= 1000) must be unique across the whole document
    let mut global_ids: Vec<String> = Vec::new();
    for track in &tracks {
        for node in xmlutil::iter_elements(track) {
            if let Some(id) = node.attributes.get("Id") {
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && id.parse::<i64>().unwrap() >= 1000 {
                    global_ids.push(id.clone());
                }
            }
        }
    }
    assert!(global_ids.len() > 100); // sanity: real Sampler devices carry hundreds of these
    let unique_global: HashSet<&String> = global_ids.iter().collect();
    assert_eq!(global_ids.len(), unique_global.len());

    for (track, sample) in tracks.iter().zip(&non_empty_samples) {
        let sampler = xmlutil::find(track, ".//MultiSampler").unwrap();
        assert_eq!(attr(xmlutil::find(sampler, ".//Pitch/TransposeKey/Manual").unwrap(), "Value"), "0");
        assert_eq!(attr(xmlutil::find(sampler, ".//Pitch/TransposeFine/Manual").unwrap(), "Value"), "0");
        assert_eq!(attr(xmlutil::find(sampler, ".//VolumeAndPan/Volume/Manual").unwrap(), "Value"), "0");
        assert_eq!(attr(xmlutil::find(sampler, ".//VolumeAndPan/Panorama/Manual").unwrap(), "Value"), "0");
        assert_eq!(attr(xmlutil::find(sampler, ".//Player/LoopModulators/SampleLength/Manual").unwrap(), "Value"), "1");
        assert_eq!(attr(xmlutil::find(sampler, ".//Player/Snap/Manual").unwrap(), "Value"), "false");
        let expected_loop_mode = if sample.has_loop() { "1" } else { "0" };
        assert_eq!(attr(xmlutil::find(sampler, ".//MultiSamplePart/SustainLoop/Mode").unwrap(), "Value"), expected_loop_mode);
        assert_eq!(attr(xmlutil::find(sampler, ".//Globals/PortamentoMode/Manual").unwrap(), "Value"), "0");
        assert_eq!(attr(xmlutil::find(sampler, ".//Filter/IsOn/Manual").unwrap(), "Value"), "false");
        assert_eq!(attr(xmlutil::find(sampler, ".//VolumeAndPan/Envelope/DecayTime/Manual").unwrap(), "Value"), "1");
        assert_eq!(attr(xmlutil::find(sampler, ".//VolumeAndPan/Envelope/ReleaseTime/Manual").unwrap(), "Value"), "1");
    }

    let song = compute_song_events(&module);
    // 4aces-high.mod sets Speed=7 (F07) on its very first row, so the effective BPM (Tempo
    // x 6 / Speed, see export::notes) settles at 125*6/7 ~= 107.14 immediately, not 125
    assert_eq!(song.tempo_changes.len(), 1);
    let expected_bpm: f64 = song.tempo_changes[0].bpm;
    let reference_bpm: f64 = 125.0 * 6.0 / 7.0;
    assert!((((expected_bpm * 10000.0).round() / 10000.0) - ((reference_bpm * 10000.0).round() / 10000.0)).abs() < 1e-9);

    let tempo = xmlutil::find(&root, ".//Tempo/Manual").unwrap();
    let tempo_value: f64 = attr(tempo, "Value").parse().unwrap();
    assert!(((tempo_value * 1e6).round() - (expected_bpm * 1e6).round()).abs() < 1e-6);

    // the Main Track's own tempo automation envelope must be retargeted too
    let tempo_target_id = attr(xmlutil::find(&root, ".//Tempo/AutomationTarget").unwrap(), "Id");
    let main_track = xmlutil::find(&root, ".//MainTrack").unwrap();
    let all_envs = xmlutil::find(main_track, "./AutomationEnvelopes/Envelopes").unwrap();
    let tempo_envelopes: Vec<&Element> = xmlutil::find_all_children(all_envs, "AutomationEnvelope")
        .into_iter()
        .filter(|env| {
            xmlutil::find(env, "./EnvelopeTarget/PointeeId").map(|p| attr(p, "Value") == tempo_target_id).unwrap_or(false)
        })
        .collect();
    assert_eq!(tempo_envelopes.len(), 1);
    let tempo_float_events = xmlutil::find_all_descendants(tempo_envelopes[0], "FloatEvent");
    assert!(!tempo_float_events.is_empty());
    for e in &tempo_float_events {
        let v: f64 = attr(e, "Value").parse().unwrap();
        assert!(((v * 1e6).round() - (expected_bpm * 1e6).round()).abs() < 1e-6);
    }

    // notes live in Arrangement clips, split one clip per pattern play
    for (track, sample) in tracks.iter().zip(&non_empty_samples) {
        let clips = xmlutil::find_all_descendants(track, "MidiClip");
        let clip_spans: Vec<(f64, f64)> = {
            let mut spans: Vec<(f64, f64)> = clips
                .iter()
                .map(|c| {
                    let start: f64 = attr(xmlutil::find(c, "./CurrentStart").unwrap(), "Value").parse().unwrap();
                    let end: f64 = attr(xmlutil::find(c, "./CurrentEnd").unwrap(), "Value").parse().unwrap();
                    (start, end)
                })
                .collect();
            spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            spans
        };
        for w in clip_spans.windows(2) {
            assert!(w[0].1 <= w[1].0 + 1e-9); // clips never overlap
        }
        for clip in &clips {
            let time: f64 = attr(clip, "Time").parse().unwrap();
            let start: f64 = attr(xmlutil::find(clip, "./CurrentStart").unwrap(), "Value").parse().unwrap();
            assert_eq!(time, start);
        }
        let _ = sample;
    }

    let total_notes: usize = tracks.iter().map(|t| xmlutil::find_all_descendants(t, "MidiNoteEvent").len()).sum();
    let expected_notes: usize = song.notes_by_sample.values().map(|v| v.len()).sum();
    assert_eq!(total_notes, expected_notes);
    assert_eq!(total_notes, 2637);

    // clip-local ClipEnvelope is a Session-view mechanism Ableton doesn't read
    for track in &tracks {
        for clip in xmlutil::find_all_descendants(track, "MidiClip") {
            if let Some(envelopes) = xmlutil::find(clip, "./Envelopes/Envelopes") {
                assert!(xmlutil::find_all_children(envelopes, "ClipEnvelope").is_empty());
            }
        }
    }

    // Pitch Bend/Volume/Panorama automation exists once per *track*
    for (track, sample) in tracks.iter().zip(&non_empty_samples) {
        let notes = &song.notes_by_sample[&sample.index];
        let has_bends = notes.iter().any(|n| !n.bends.is_empty());
        let has_pans = notes.iter().any(|n| !n.pans.is_empty());

        let sampler = xmlutil::find(track, ".//MultiSampler").unwrap();
        let transpose_id = attr(xmlutil::find(sampler, ".//Pitch/TransposeKey/AutomationTarget").unwrap(), "Id");
        let volume_id = attr(xmlutil::find(track, "./DeviceChain/Mixer/Volume/AutomationTarget").unwrap(), "Id");
        let pan_id = attr(xmlutil::find(sampler, "./VolumeAndPan/Panorama/AutomationTarget").unwrap(), "Id");

        let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
        let track_envelopes = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope");
        assert_eq!(track_envelopes.len(), 1 + has_bends as usize + has_pans as usize); // Volume: always
        let pointee_values: HashSet<String> = track_envelopes
            .iter()
            .map(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value"))
            .collect();
        for e in &track_envelopes {
            let float_events = xmlutil::find_all_descendants(e, "FloatEvent");
            assert!(float_events.len() > 1);
            assert_eq!(attr(float_events[0], "Time"), "-63072000");
        }

        assert!(pointee_values.contains(&volume_id));
        if has_bends {
            assert!(pointee_values.contains(&transpose_id));
        }
        if has_pans {
            assert!(pointee_values.contains(&pan_id));
        }
    }

    let samples_dir = output.parent().unwrap().join("Samples").join("Imported");
    let wav_files: Vec<_> = std::fs::read_dir(&samples_dir).unwrap().filter(|e| {
        e.as_ref().unwrap().path().extension().map(|e| e == "wav").unwrap_or(false)
    }).collect();
    assert_eq!(wav_files.len(), non_empty_samples.len());

    for (track, sample) in tracks.iter().zip(&non_empty_samples) {
        let name = attr(xmlutil::find(track, "./Name/EffectiveName").unwrap(), "Value");
        assert_eq!(name, format!("{:02} {}", sample.index, sample.name).trim());

        let file_ref = xmlutil::find(track, ".//SampleRef/FileRef").unwrap();
        let path_value = attr(xmlutil::find(file_ref, "./Path").unwrap(), "Value");
        assert!(std::path::Path::new(&path_value).exists());
        let rel_path = attr(xmlutil::find(file_ref, "./RelativePath").unwrap(), "Value");
        assert_eq!(std::path::Path::new(&rel_path).file_name(), std::path::Path::new(&path_value).file_name());

        let root_key = attr(xmlutil::find(track, ".//MultiSamplePart/RootKey").unwrap(), "Value");
        assert_eq!(root_key, sample.base_note.to_string());

        let sample_end: usize = attr(xmlutil::find(track, ".//MultiSamplePart/SampleEnd").unwrap(), "Value").parse().unwrap();
        assert_eq!(sample_end, sample.pcm16.len() / 2 - 1);
    }
}

#[test]
fn test_export_als_volume_and_pan_envelopes() {
    let template = std::fs::read("tests/fixtures/Template Glissando Project/Template Glissando.als").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), effect: Some(0x8), effect_param: Some(255) }; // note + hard-right pan
    let slide_down = Cell { effect: Some(0xA), effect_param: Some(0x04), ..Default::default() }; // volume slide, no new note
    let pattern = Pattern { rows: vec![vec![note_on], vec![slide_down]] };
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![looped],
        patterns: vec![pattern], order: vec![0], restart_position: 0, initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };

    export_als(&module, &output, &template, AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let sampler = xmlutil::find(track, ".//MultiSampler").unwrap();
    assert_eq!(attr(xmlutil::find(sampler, ".//VolumeAndPan/Volume/Manual").unwrap(), "Value"), "0");
    assert_eq!(attr(xmlutil::find(sampler, ".//VolumeAndPan/Panorama/Manual").unwrap(), "Value"), "0");

    let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
    let track_envelopes = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope");
    assert_eq!(track_envelopes.len(), 2); // volume (from the slide) and pan (from 8xx), no portamento here

    let volume_target = attr(xmlutil::find(track, "./DeviceChain/Mixer/Volume/AutomationTarget").unwrap(), "Id");
    let pan_target = attr(xmlutil::find(sampler, "./VolumeAndPan/Panorama/AutomationTarget").unwrap(), "Id");

    let volume_env = track_envelopes.iter().find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == volume_target).unwrap();
    let values: Vec<f64> = xmlutil::find_all_descendants(volume_env, "FloatEvent").iter().map(|e| attr(e, "Value").parse().unwrap()).collect();
    let baseline_gain = (volume_to_gain(64) * 1e6).round() / 1e6;
    assert_eq!(values[0], baseline_gain);
    assert!(values.iter().any(|&v| v < baseline_gain));

    let pan_env = track_envelopes.iter().find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == pan_target).unwrap();
    let pan_values: Vec<f64> = xmlutil::find_all_descendants(pan_env, "FloatEvent").iter().map(|e| attr(e, "Value").parse().unwrap()).collect();
    assert!(pan_values.iter().any(|&v| v == 1.0));
}

#[test]
fn test_a_quiet_note_without_any_volume_effect_still_plays_quiet() {
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let quiet_note = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(16), ..Default::default() }; // 25% of max tracker volume, no effect
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![looped],
        patterns: vec![Pattern { rows: vec![vec![quiet_note]] }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let volume_target = attr(xmlutil::find(track, "./DeviceChain/Mixer/Volume/AutomationTarget").unwrap(), "Id");
    let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
    let volume_env = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope")
        .into_iter()
        .find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == volume_target)
        .unwrap();

    let values: HashSet<i64> = xmlutil::find_all_descendants(volume_env, "FloatEvent")
        .iter()
        .filter(|e| attr(e, "Time") != "-63072000")
        .map(|e| (attr(e, "Value").parse::<f64>().unwrap() * 1e6).round() as i64)
        .collect();
    let expected_db = TRACK_VOLUME_DB + 20.0 * (16.0f64 / 64.0).log10();
    let expected_gain = 10f64.powf(expected_db / 20.0);
    let expected_gain_rounded = (expected_gain * 1e6).round() as i64;
    assert_eq!(values, HashSet::from([expected_gain_rounded]));
    assert!(expected_gain < volume_to_gain(64));
}

#[test]
fn test_tempo_automation_steps_instead_of_ramping() {
    let sample = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let speed_change = Cell { effect: Some(0xF), effect_param: Some(3), ..Default::default() };
    let speed_back = Cell { effect: Some(0xF), effect_param: Some(6), ..Default::default() };
    let mut rows = vec![Cell::default(); 8].into_iter().map(|c| vec![c]).collect::<Vec<_>>();
    rows.push(vec![speed_change]);
    rows.extend(vec![Cell::default(); 7].into_iter().map(|c| vec![c]));
    rows.push(vec![speed_back]);
    rows.extend(vec![Cell::default(); 7].into_iter().map(|c| vec![c]));
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![sample],
        patterns: vec![Pattern { rows }], order: vec![0], restart_position: 0, initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let tempo_target_id = attr(xmlutil::find(&root, ".//Tempo/AutomationTarget").unwrap(), "Id");
    let main_track = xmlutil::find(&root, ".//MainTrack").unwrap();
    let all_envs = xmlutil::find(main_track, "./AutomationEnvelopes/Envelopes").unwrap();
    let tempo_envelopes: Vec<&Element> = xmlutil::find_all_children(all_envs, "AutomationEnvelope")
        .into_iter()
        .filter(|env| xmlutil::find(env, "./EnvelopeTarget/PointeeId").map(|p| attr(p, "Value") == tempo_target_id).unwrap_or(false))
        .collect();
    assert_eq!(tempo_envelopes.len(), 1);
    let events: Vec<(f64, f64)> = xmlutil::find_all_descendants(tempo_envelopes[0], "FloatEvent")
        .iter()
        .map(|e| (attr(e, "Time").parse().unwrap(), attr(e, "Value").parse().unwrap()))
        .collect();

    let song = compute_song_events(&module);
    assert_eq!(song.tempo_changes.len(), 3); // 125 initial, 250 (speed 3), back to 125 (speed 6)

    assert_eq!(events.len(), 2 + 2 * (song.tempo_changes.len() - 1));

    for (i, tc) in song.tempo_changes[1..].iter().enumerate() {
        let (hold_time, _hold_value) = events[2 + i * 2];
        let (jump_time, jump_value) = events[3 + i * 2];
        assert_eq!(jump_time, tc.at_beat);
        assert_eq!(jump_value, tc.bpm);
        assert!(hold_time < jump_time);
        assert!((hold_time - (tc.at_beat - 0.001)).abs() < 1e-9);
    }
}

#[test]
fn test_patterns_split_into_separate_arrangement_clips() {
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let note_a = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let note_b = Cell { sample_index: Some(1), midi_note: Some(64), volume: Some(64), ..Default::default() };
    let pattern0 = Pattern { rows: vec![vec![note_a], vec![Cell::default()], vec![Cell::default()], vec![Cell::default()]] };
    let pattern1 = Pattern { rows: vec![vec![Cell::default()], vec![note_b], vec![Cell::default()], vec![Cell::default()]] };
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![looped],
        patterns: vec![pattern0, pattern1], order: vec![0, 1], restart_position: 0, initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let pattern_length = 4.0 * BEATS_PER_ROW; // 1.0 beat

    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let mut clips = xmlutil::find_all_descendants(track, "MidiClip");
    clips.sort_by(|a, b| attr(a, "Time").parse::<f64>().unwrap().partial_cmp(&attr(b, "Time").parse::<f64>().unwrap()).unwrap());
    assert_eq!(clips.len(), 2);

    let clip0 = clips[0];
    let clip1 = clips[1];
    assert_eq!(attr(clip0, "Time").parse::<f64>().unwrap(), 0.0);
    assert_eq!(attr(xmlutil::find(clip0, "./CurrentStart").unwrap(), "Value").parse::<f64>().unwrap(), 0.0);
    assert!((attr(xmlutil::find(clip0, "./CurrentEnd").unwrap(), "Value").parse::<f64>().unwrap() - pattern_length).abs() < 1e-6);
    assert!((attr(clip1, "Time").parse::<f64>().unwrap() - pattern_length).abs() < 1e-6);
    assert!((attr(xmlutil::find(clip1, "./CurrentStart").unwrap(), "Value").parse::<f64>().unwrap() - pattern_length).abs() < 1e-6);
    assert!((attr(xmlutil::find(clip1, "./CurrentEnd").unwrap(), "Value").parse::<f64>().unwrap() - 2.0 * pattern_length).abs() < 1e-6);

    let note0 = xmlutil::find(clip0, ".//Notes/KeyTracks/KeyTrack/Notes/MidiNoteEvent").unwrap();
    assert_eq!(attr(note0, "Time").parse::<f64>().unwrap(), 0.0);
    assert!((attr(note0, "Duration").parse::<f64>().unwrap() - pattern_length).abs() < 1e-6);

    let note1 = xmlutil::find(clip1, ".//Notes/KeyTracks/KeyTrack/Notes/MidiNoteEvent").unwrap();
    assert!((attr(note1, "Time").parse::<f64>().unwrap() - (pattern_length + BEATS_PER_ROW)).abs() < 1e-6);
}

#[test]
fn test_set_volume_and_panning_step_instead_of_ramping() {
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let set_volume = Cell { effect: Some(0xC), effect_param: Some(16), ..Default::default() };
    let set_pan = Cell { effect: Some(0x8), effect_param: Some(255), ..Default::default() }; // hard right, away from the center baseline
    let mut rows = vec![vec![note_on]];
    rows.extend(vec![vec![Cell::default()]; 20]);
    rows.push(vec![set_volume]);
    rows.extend(vec![vec![Cell::default()]; 20]);
    rows.push(vec![set_pan]);
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![looped],
        patterns: vec![Pattern { rows }], order: vec![0], restart_position: 0, initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let sampler = xmlutil::find(track, ".//MultiSampler").unwrap();
    let volume_target = attr(xmlutil::find(track, "./DeviceChain/Mixer/Volume/AutomationTarget").unwrap(), "Id");
    let pan_target = attr(xmlutil::find(sampler, "./VolumeAndPan/Panorama/AutomationTarget").unwrap(), "Id");
    let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
    let track_envelopes = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope");

    let baseline_volume = (volume_to_gain(64) * 1e6).round() / 1e6;
    for (target, baseline) in [(&volume_target, baseline_volume), (&pan_target, 0.0)] {
        let env = track_envelopes.iter().find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == *target).unwrap();
        let events: Vec<(f64, f64)> = xmlutil::find_all_descendants(env, "FloatEvent")
            .iter()
            .filter(|e| attr(e, "Time") != "-63072000")
            .map(|e| (attr(e, "Time").parse().unwrap(), attr(e, "Value").parse().unwrap()))
            .collect();
        let jump_index = events.iter().position(|(_, v)| *v != baseline).unwrap();
        let (hold_time, hold_value) = events[jump_index - 1];
        let (jump_time, jump_value) = events[jump_index];
        assert_eq!(hold_value, baseline);
        assert_ne!(jump_value, baseline);
        assert!((hold_time - (jump_time - 0.001)).abs() < 1e-9);
    }
}

#[test]
fn test_volume_slide_glides_smoothly_without_a_step_between_ticks() {
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let slide_down = Cell { effect: Some(0xA), effect_param: Some(0x04), ..Default::default() };
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![looped],
        patterns: vec![Pattern { rows: vec![vec![note_on], vec![slide_down]] }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");

    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let song = compute_song_events(&module);
    let volumes = &song.notes_by_sample[&1][0].volumes;
    assert_eq!(volumes.len(), 5); // ticks 1..5 at speed=6, sanity check on the fixture itself

    let root = read_als(&output);
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let volume_target = attr(xmlutil::find(track, "./DeviceChain/Mixer/Volume/AutomationTarget").unwrap(), "Id");
    let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
    let track_envelope = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope")
        .into_iter()
        .find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == volume_target)
        .unwrap();
    let all_times: HashSet<i64> = xmlutil::find_all_descendants(track_envelope, "FloatEvent")
        .iter()
        .map(|e| (attr(e, "Time").parse::<f64>().unwrap() * 1e6).round() as i64)
        .collect();

    for v in &volumes[1..] {
        let t = (((v.at_beat - 0.001) * 1e6).round()) as i64;
        assert!(!all_times.contains(&t));
    }
    let t0 = (((volumes[0].at_beat - 0.001) * 1e6).round()) as i64;
    assert!(all_times.contains(&t0));
}

#[test]
fn test_export_als_pinballf_every_effect_it_uses_is_implemented() {
    let module = parse(&std::fs::read("tests/fixtures/PINBALLF.MOD").unwrap());
    assert!(unimplemented_effect_counts(&module).is_empty());

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let tracks = xmlutil::find_all_descendants(&root, "MidiTrack");
    let non_empty_samples: Vec<_> = module.samples.iter().filter(|s| !s.is_empty()).collect();
    assert_eq!(tracks.len(), non_empty_samples.len());

    let song = compute_song_events(&module);
    let total_notes: usize = tracks.iter().map(|t| xmlutil::find_all_descendants(t, "MidiNoteEvent").len()).sum();
    let expected_notes: usize = song.notes_by_sample.values().map(|v| v.len()).sum();
    assert_eq!(total_notes, expected_notes);

    for track in &tracks {
        let mut clips = xmlutil::find_all_descendants(track, "MidiClip");
        clips.sort_by(|a, b| attr(a, "Time").parse::<f64>().unwrap().partial_cmp(&attr(b, "Time").parse::<f64>().unwrap()).unwrap());
        let spans: Vec<(f64, f64)> = clips.iter().map(|c| {
            (attr(xmlutil::find(c, "./CurrentStart").unwrap(), "Value").parse().unwrap(), attr(xmlutil::find(c, "./CurrentEnd").unwrap(), "Value").parse().unwrap())
        }).collect();
        for w in spans.windows(2) {
            assert!(w[0].1 <= w[1].0 + 1e-9);
        }
        for clip in &clips {
            assert!(!xmlutil::find_all_descendants(clip, "MidiNoteEvent").is_empty());
            if let Some(envelopes) = xmlutil::find(clip, "./Envelopes/Envelopes") {
                assert!(xmlutil::find_all_children(envelopes, "ClipEnvelope").is_empty());
            }
        }

        if let Some(auto_envelopes) = xmlutil::find(track, "./AutomationEnvelopes") {
            for float_event in xmlutil::find_all_descendants(auto_envelopes, "FloatEvent") {
                let v: f64 = attr(float_event, "Value").parse().unwrap();
                assert!(v.is_finite());
            }
        }
    }
}

#[test]
fn test_extract_midi_pinballf_does_not_raise() {
    let module = parse(&std::fs::read("tests/fixtures/PINBALLF.MOD").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.mid");

    write_midi(&module, &output).unwrap(); // must not raise

    assert!(output.is_file());
}

fn amiga_panning_module() -> Module {
    // one note per row, each on a different channel (0..3, in order) so they land at cleanly
    // separated, unambiguous beats in notes_by_sample — avoids same-beat tie-breaking in the
    // exported automation entirely. None of them use Set Panning (8xx), to isolate the
    // *baseline* pan the AmigaPanning preset assigns.
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let note = |pitch: i32| Cell { sample_index: Some(1), midi_note: Some(pitch), volume: Some(64), ..Default::default() };
    let empty = Cell::default();
    let rows = vec![
        vec![note(60), empty.clone(), empty.clone(), empty.clone()],
        vec![empty.clone(), note(61), empty.clone(), empty.clone()],
        vec![empty.clone(), empty.clone(), note(62), empty.clone()],
        vec![empty.clone(), empty.clone(), empty.clone(), note(63)],
    ];
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 4, samples: vec![looped],
        patterns: vec![Pattern { rows }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    }
}

fn pan_baseline_values(amiga_panning: AmigaPanning) -> Vec<f64> {
    let module = amiga_panning_module();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), amiga_panning).unwrap();

    let root = read_als(&output);
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let sampler = xmlutil::find(track, ".//MultiSampler").unwrap();
    let pan_target = attr(xmlutil::find(sampler, "./VolumeAndPan/Panorama/AutomationTarget").unwrap(), "Id");
    let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
    let track_envelopes = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope");
    let Some(env) = track_envelopes.iter().find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == pan_target) else {
        return Vec::new();
    };
    let mut values: Vec<f64> = xmlutil::find_all_descendants(env, "FloatEvent")
        .iter()
        .filter(|e| attr(e, "Time") != "-63072000")
        .map(|e| attr(e, "Value").parse().unwrap())
        .collect();
    values.dedup_by(|a, b| a == b);
    values
}

#[test]
fn test_amiga_panning_none_has_no_pan_envelope_without_8xx() {
    let values = pan_baseline_values(AmigaPanning::None);
    assert!(values.is_empty());
}

#[test]
fn test_amiga_panning_full_hard_pans_l_r_r_l() {
    let values = pan_baseline_values(AmigaPanning::Full);
    // channel 0 (note 60) -> left, 1 (note 61) -> right, 2 (note 62) -> right, 3 (note 63) -> left
    assert_eq!(values, vec![-1.0, 1.0, -1.0]); // channel1 and channel2 share the same "right" baseline, so no distinct value in between
}

#[test]
fn test_amiga_panning_medium_is_half_separation() {
    let values = pan_baseline_values(AmigaPanning::Medium);
    assert_eq!(values, vec![-0.5, 0.5, -0.5]);
}

#[test]
fn test_amiga_panning_light_is_quarter_separation() {
    let values = pan_baseline_values(AmigaPanning::Light);
    assert_eq!(values, vec![-0.25, 0.25, -0.25]);
}

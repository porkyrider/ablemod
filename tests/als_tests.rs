mod common;

use std::collections::HashSet;
use std::io::Read;

use ablemod::export::als::{default_template_bytes, export_als, volume_to_gain, AmigaPanning, TRACK_VOLUME_DB};
use ablemod::export::midi::write_midi;
use ablemod::export::notes::{compute_song_events, BEATS_PER_ROW, NoteEvent, SongEvents};
use ablemod::formats::base::{Cell, Module, Pattern, Sample};
use ablemod::formats::protracker::{parse, unimplemented_effect_counts};
use ablemod::xmlutil;
use xmltree::{Element, XMLNode};

// Mirrors the (sample, voice) -> track fan-out export_als itself does (see the
// voice-assignment pass in export::notes::compute_song_events): a sample only needs more
// than one track when it's triggered on several tracker channels with overlapping timing,
// in which case `voice_label` follows export_als's own "(N)" naming for the 2nd+ voice.
fn track_voice_groups<'a>(song: &'a SongEvents, non_empty_samples: &[&'a Sample]) -> Vec<(&'a Sample, Option<usize>, Vec<&'a NoteEvent>)> {
    let mut result = Vec::new();
    for sample in non_empty_samples {
        let notes: Vec<&NoteEvent> = song.notes_by_sample.get(&sample.index).map(|v| v.iter().collect()).unwrap_or_default();
        let voice_count = notes.iter().map(|n| n.voice + 1).max().unwrap_or(1);
        for voice in 0..voice_count {
            let voice_notes: Vec<&NoteEvent> = notes.iter().filter(|n| n.voice == voice).copied().collect();
            let voice_label = if voice > 0 { Some(voice + 1) } else { None }; // first voice keeps the plain name
            result.push((*sample, voice_label, voice_notes));
        }
    }
    result
}

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
    let song = compute_song_events(&module);
    assert_eq!(tracks.len(), track_voice_groups(&song, &non_empty_samples).len());
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
    let song = compute_song_events(&module);
    // A sample triggered on several tracker channels with overlapping timing gets one track
    // per voice instead of one track per sample (see the voice-assignment pass in
    // export::notes::compute_song_events) — track_voice_groups mirrors that same fan-out so
    // the rest of this test can associate each exported track with its owning sample/voice.
    let track_groups = track_voice_groups(&song, &non_empty_samples);
    assert_eq!(tracks.len(), track_groups.len());
    assert!(track_groups.iter().any(|(_, voice_label, _)| voice_label.is_some())); // sanity: this fixture does need >1 voice for some samples

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

    for (track, (sample, _voice_label, _notes)) in tracks.iter().zip(&track_groups) {
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
    for (track, (sample, _voice_label, _notes)) in tracks.iter().zip(&track_groups) {
        let track_color = attr(xmlutil::find(track, "./Color").unwrap(), "Value");
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
            assert_eq!(attr(xmlutil::find(clip, "./Color").unwrap(), "Value"), track_color); // clip color follows its track's own sample color
        }
        assert_eq!(attr(xmlutil::find(track, "./TrackUnfolded").unwrap(), "Value"), "false"); // folded/minimized by default
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

    // Pitch Bend/Volume/Panorama automation exists once per *track*, driven by that voice's
    // own notes only — a sample's other voice(s) may use different effects.
    for (track, (_sample, _voice_label, notes)) in tracks.iter().zip(&track_groups) {
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
    assert_eq!(wav_files.len(), non_empty_samples.len()); // one wav per *sample*, shared across a sample's voice tracks

    for (track, (sample, voice_label, _notes)) in tracks.iter().zip(&track_groups) {
        let name = attr(xmlutil::find(track, "./Name/EffectiveName").unwrap(), "Value");
        let expected_base_name = format!("{:02} {}", sample.index, sample.name).trim().to_string();
        let expected_name = match voice_label {
            Some(v) => format!("{expected_base_name} ({v})"),
            None => expected_base_name,
        };
        assert_eq!(name, expected_name);
        // UserName must carry the same text — EffectiveName alone is just a cached, freely
        // regenerable display value in real Ableton (confirmed by round-tripping a generated
        // project through Live: any track without UserName also set came back renamed by
        // Live's own auto-naming, e.g. "01 bsnare" mangled into "2-01 bsnare").
        assert_eq!(attr(xmlutil::find(track, "./Name/UserName").unwrap(), "Value"), expected_name);

        let color: i32 = attr(xmlutil::find(track, "./Color").unwrap(), "Value").parse().unwrap();
        assert_eq!(color, (sample.index as i32 - 1).rem_euclid(70)); // one color per sample, shared by its voice(s)

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
    let song = compute_song_events(&module);
    assert_eq!(tracks.len(), track_voice_groups(&song, &non_empty_samples).len());

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

fn overlapping_sample_module() -> Module {
    // two channels trigger the same sample at the exact same beat, needing 2 voices/tracks
    // (see the voice-assignment pass in export::notes::compute_song_events).
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let row = vec![
        Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() },
        Cell { sample_index: Some(1), midi_note: Some(67), volume: Some(64), ..Default::default() },
    ];
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 2, samples: vec![looped],
        patterns: vec![Pattern { rows: vec![row] }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    }
}

#[test]
fn test_samples_needing_multiple_voices_get_folded_into_a_group_track() {
    let module = overlapping_sample_module();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let tracks_el = xmlutil::find(&root, ".//Tracks").unwrap();
    // export_als appends the module's own tracks first, any template ReturnTracks after —
    // only look at the first 3 (this module has exactly one sample, needing one group + 2
    // voice tracks) rather than assume the template carries no ReturnTracks of its own.
    let children: Vec<&Element> = tracks_el.children.iter().filter_map(|n| match n {
        XMLNode::Element(e) => Some(e),
        _ => None,
    }).take(3).collect();

    // the GroupTrack must precede its 2 member MidiTracks, in that order (matches the real
    // Ableton project this GroupTrack XML was captured from)
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].name, "GroupTrack");
    assert_eq!(children[1].name, "MidiTrack");
    assert_eq!(children[2].name, "MidiTrack");

    let group_id = attr(children[0], "Id");
    let group_name = attr(xmlutil::find(children[0], "./Name/EffectiveName").unwrap(), "Value");
    assert_eq!(group_name, "01 pad");
    // UserName too, or real Ableton displays the generic placeholder "Group" instead of this
    // name — confirmed by round-tripping a generated project through Live (see build_track).
    assert_eq!(attr(xmlutil::find(children[0], "./Name/UserName").unwrap(), "Value"), "01 pad");
    let group_color = attr(xmlutil::find(children[0], "./Color").unwrap(), "Value");

    // folded by default: the group hides its member tracks, and each member track itself
    // starts minimized (both driven by the same TrackUnfolded flag).
    assert_eq!(attr(xmlutil::find(children[0], "./TrackUnfolded").unwrap(), "Value"), "false");

    for member in [children[1], children[2]] {
        assert_eq!(attr(xmlutil::find(member, "./TrackGroupId").unwrap(), "Value"), group_id);
        assert_eq!(attr(xmlutil::find(member, "./Color").unwrap(), "Value"), group_color);
        assert_eq!(attr(xmlutil::find(member, "./TrackUnfolded").unwrap(), "Value"), "false");

        // every clip on this track shares the track's own color too
        for clip in xmlutil::find_all_descendants(member, "MidiClip") {
            assert_eq!(attr(xmlutil::find(clip, "./Color").unwrap(), "Value"), group_color);
        }
    }

    // every Id inside the cloned GroupTrack (Mixer AutomationTargets, ModulationTargets, ...)
    // must be renumbered uniquely, just like a cloned MidiTrack — no collisions with anything
    // else in the document.
    let mut global_ids: Vec<String> = Vec::new();
    for node in xmlutil::iter_elements(&root) {
        if let Some(id) = node.attributes.get("Id") {
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && id.parse::<i64>().unwrap() >= 1000 {
                global_ids.push(id.clone());
            }
        }
    }
    let unique: HashSet<&String> = global_ids.iter().collect();
    assert_eq!(global_ids.len(), unique.len());
}

#[test]
fn test_only_the_first_track_is_armed() {
    // the template's own Sampler track happens to be armed (a common state to leave a track
    // in) — cloning it as-is would arm every exported track. Only the very first track of the
    // whole project should come out armed, matching what a musician actually wants to see on
    // opening a freshly generated project.
    let module = overlapping_sample_module(); // 1 sample needing 2 voices -> group + 2 MidiTracks
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let midi_tracks = xmlutil::find_all_descendants(&root, "MidiTrack");
    assert_eq!(midi_tracks.len(), 2);
    let armed: Vec<bool> = midi_tracks
        .iter()
        .map(|t| attr(xmlutil::find(t, "./DeviceChain/MainSequencer/Recorder/IsArmed").unwrap(), "Value") == "true")
        .collect();
    assert_eq!(armed, vec![true, false]);
}

#[test]
fn test_only_one_track_armed_across_a_multi_sample_project() {
    let module = parse(&std::fs::read("tests/fixtures/4aces-high.mod").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let midi_tracks = xmlutil::find_all_descendants(&root, "MidiTrack");
    assert!(midi_tracks.len() > 1);
    let armed_count = midi_tracks
        .iter()
        .filter(|t| attr(xmlutil::find(t, "./DeviceChain/MainSequencer/Recorder/IsArmed").unwrap(), "Value") == "true")
        .count();
    assert_eq!(armed_count, 1);
}

#[test]
fn test_a_single_voice_sample_is_not_grouped() {
    let looped = Sample { index: 1, name: "pad".to_string(), pcm16: vec![0u8; 100], sample_rate_hz: 44100, loop_start: 0, loop_length: 2, volume: 64, finetune: 0, base_note: 60 };
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![looped],
        patterns: vec![Pattern { rows: vec![vec![note_on]] }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    assert!(xmlutil::find_all_descendants(&root, "GroupTrack").is_empty());
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    assert_eq!(attr(xmlutil::find(track, "./TrackGroupId").unwrap(), "Value"), "-1");
    assert_eq!(attr(xmlutil::find(track, "./TrackUnfolded").unwrap(), "Value"), "false"); // folded/minimized by default
}

#[test]
fn test_sample_offset_sets_the_tracks_own_sample_start() {
    // Sample Offset (9xx) has no per-note representation in Ableton's Sampler — instead, a
    // note needing a different start position gets routed to its own voice/track (see the
    // voice-assignment pass in export::notes), whose *whole* MultiSamplePart uses that offset
    // as SampleStart.
    let long_sample = Sample {
        index: 1, name: "pad".to_string(), pcm16: vec![0u8; 10000], sample_rate_hz: 44100,
        loop_start: 0, loop_length: 0, volume: 64, finetune: 0, base_note: 60,
    };
    let offset_note = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), effect: Some(0x9), effect_param: Some(4) }; // 4*256 = 1024
    let module = Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![long_sample],
        patterns: vec![Pattern { rows: vec![vec![offset_note]] }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    };
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(&module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let track = xmlutil::find(&root, ".//MidiTrack").unwrap();
    let sample_start = attr(xmlutil::find(track, ".//MultiSamplePart/SampleStart").unwrap(), "Value");
    assert_eq!(sample_start, "1024");
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
    let tracks = xmlutil::find_all_descendants(&root, "MidiTrack");
    // Each of the 4 channels' single note spans to the end of the song, so all 4 overlap and
    // this sample needs one voice — hence one track — per channel: gather each track's own
    // baseline pan value, in track order (which follows voice order, i.e. channel order).
    tracks
        .iter()
        .filter_map(|track| {
            let sampler = xmlutil::find(track, ".//MultiSampler").unwrap();
            let pan_target = attr(xmlutil::find(sampler, "./VolumeAndPan/Panorama/AutomationTarget").unwrap(), "Id");
            let envelopes_el = xmlutil::find(track, "./AutomationEnvelopes/Envelopes").unwrap();
            let track_envelopes = xmlutil::find_all_children(envelopes_el, "AutomationEnvelope");
            let env = track_envelopes
                .iter()
                .find(|e| attr(xmlutil::find(e, "./EnvelopeTarget/PointeeId").unwrap(), "Value") == pan_target)?;
            xmlutil::find_all_descendants(env, "FloatEvent")
                .iter()
                .find(|e| attr(e, "Time") != "-63072000")
                .map(|e| attr(e, "Value").parse().unwrap())
        })
        .collect()
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
    // each channel's note spans to the end of the song, so all 4 overlap and land on 4
    // separate voices/tracks — no dedup collapsing distinct tracks together anymore.
    assert_eq!(values, vec![-1.0, 1.0, 1.0, -1.0]);
}

#[test]
fn test_amiga_panning_medium_is_half_separation() {
    let values = pan_baseline_values(AmigaPanning::Medium);
    assert_eq!(values, vec![-0.5, 0.5, 0.5, -0.5]);
}

#[test]
fn test_amiga_panning_light_is_quarter_separation() {
    let values = pan_baseline_values(AmigaPanning::Light);
    assert_eq!(values, vec![-0.25, 0.25, 0.25, -0.25]);
}

fn module_with_looped_sample(loop_start: u32, loop_length: u32, total_frames: u32) -> Module {
    // a distinctive ramp waveform so the "loop segment repeated" content is easy to eyeball
    // if this ever needs debugging, rather than uniform silence.
    let mut pcm16 = Vec::with_capacity(total_frames as usize * 2);
    for i in 0..total_frames {
        let sample = (i % 100) as i16 * 300;
        pcm16.extend_from_slice(&sample.to_le_bytes());
    }
    let sample = Sample {
        index: 1, name: "loopy".to_string(), pcm16, sample_rate_hz: 8363,
        loop_start, loop_length, volume: 64, finetune: 0, base_note: 60,
    };
    let note_on = Cell { sample_index: Some(1), midi_note: Some(60), volume: Some(64), ..Default::default() };
    Module {
        title: "t".to_string(), source_format: "protracker".to_string(), num_channels: 1, samples: vec![sample],
        patterns: vec![Pattern { rows: vec![vec![note_on]] }], order: vec![0], restart_position: 0,
        initial_tempo_bpm: 125, initial_speed_ticks: 6,
    }
}

fn exported_sample_part_info(module: &Module) -> (u32, u32, u32, u32) {
    // (sample_end, sustain_loop_start, sustain_loop_end, wav_frame_count)
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.als");
    export_als(module, &output, default_template_bytes(), AmigaPanning::None).unwrap();

    let root = read_als(&output);
    let part = xmlutil::find(&root, ".//MultiSamplePart").unwrap();
    let sample_end: u32 = attr(xmlutil::find(part, "./SampleEnd").unwrap(), "Value").parse().unwrap();
    let loop_start: u32 = attr(xmlutil::find(part, "./SustainLoop/Start").unwrap(), "Value").parse().unwrap();
    let loop_end: u32 = attr(xmlutil::find(part, "./SustainLoop/End").unwrap(), "Value").parse().unwrap();

    let wav_path = dir.path().join("Samples").join("Imported").join("01_loopy.wav");
    let reader = hound::WavReader::open(&wav_path).unwrap();
    let frame_count = reader.duration();

    (sample_end, loop_start, loop_end, frame_count)
}

#[test]
fn test_short_loop_gets_repeated_until_the_sampler_can_loop_it() {
    // 32-frame loop (PFANTAS1.MOD instrument 10's exact loop length) — below Ableton
    // Sampler's undocumented 48-frame minimum, so it must be repeated to at least 48 frames
    // (here: 2x32 = 64) before the loop points are written.
    let module = module_with_looped_sample(100, 32, 132);
    let (sample_end, loop_start, loop_end, frame_count) = exported_sample_part_info(&module);

    let new_loop_length = loop_end - loop_start + 1;
    assert!(new_loop_length >= 48, "loop is still below Ableton's minimum: {new_loop_length}");
    assert_eq!(new_loop_length, 64); // ceil(48/32) = 2 repeats of the original 32-frame loop
    assert_eq!(loop_start, 100); // the attack portion before the loop is untouched
    assert_eq!(sample_end, frame_count - 1);
    assert_eq!(frame_count, 100 + 64); // attack (100) + repeated loop (64), original tail dropped
}

#[test]
fn test_loop_already_at_the_minimum_is_left_untouched() {
    let module = module_with_looped_sample(50, 48, 98);
    let (sample_end, loop_start, loop_end, frame_count) = exported_sample_part_info(&module);

    assert_eq!(loop_start, 50);
    assert_eq!(loop_end - loop_start + 1, 48);
    assert_eq!(frame_count, 98);
    assert_eq!(sample_end, 97);
}

#[test]
fn test_loop_well_above_the_minimum_is_left_untouched() {
    let module = module_with_looped_sample(48, 2126, 2174); // PFANTAS1.MOD instrument 8's exact loop
    let (sample_end, loop_start, loop_end, frame_count) = exported_sample_part_info(&module);

    assert_eq!(loop_start, 48);
    assert_eq!(loop_end, 2173);
    assert_eq!(frame_count, 2174);
    assert_eq!(sample_end, 2173);
}

#[test]
fn test_non_looped_sample_is_never_extended() {
    let mut module = module_with_looped_sample(0, 0, 100);
    module.samples[0].loop_length = 0; // no loop at all
    let (sample_end, _loop_start, _loop_end, frame_count) = exported_sample_part_info(&module);

    assert_eq!(frame_count, 100);
    assert_eq!(sample_end, 99);
}

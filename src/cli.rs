use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::export::als::{export_als, AmigaPanning};
use crate::export::midi::write_midi;
use crate::export::notes::compute_song_events;
use crate::export::wav::{sample_wav_filename, write_sample_wav};
use crate::formats::base::Module;
use crate::formats::detect::load_module;
use crate::formats::protracker::{effect_name, implemented_effect_counts, unimplemented_effect_counts};
use crate::formats::vgm;

/// Convert ProTracker modules or VGM/VGZ chiptune recordings to Ableton Live projects.
#[derive(Parser)]
#[command(name = "ablemod")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Display the contents of a tracker module or VGM/VGZ file.
    List { module_path: PathBuf },

    /// Extract all samples from a tracker module as .wav files. Not applicable to VGM/VGZ
    /// (which has no stored samples) — use 'convert' instead.
    ExtractSamples {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_dir: PathBuf,
        /// Print extra detail about each sample as it's extracted.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Convert a tracker module's patterns to a .mid file (one track per sample). Not
    /// applicable to VGM/VGZ — use 'convert' instead.
    ExtractMidi {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_path: PathBuf,
        /// Print extra detail, including which effects were transcribed and which were ignored.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },
    /// Convert a tracker module or VGM/VGZ file into an Ableton Live Set (.als).
    ///
    /// For a tracker module: one Sampler per sample. For a VGM/VGZ file: the YM2413/AY8910
    /// chips are actually emulated and rendered to audio — one AudioTrack for the full mix
    /// plus one per chip channel (stems), no MIDI/Sampler involved. Chips other than
    /// YM2413/AY8910 aren't emulated yet (see 'list' to check what a given file uses).
    Convert {
        module_path: PathBuf,
        /// Optional: a real .als exported from Ableton Live, containing a MIDI track with a
        /// Sampler device holding a sample and its content laid out in Arrangement view.
        /// Defaults to ablemod's bundled template.
        #[arg(long = "template")]
        template_path: Option<PathBuf>,
        #[arg(short = 'o', long = "output")]
        output_path: PathBuf,
        /// Baseline Panorama automation: real 4-channel tracker hardware hard-pans channels
        /// 0/3 left and 1/2 right (repeating every 4 channels) with no in-between mix — "full"
        /// reproduces that; "light"/"medium" soften it to 25%/50% stereo separation; "none"
        /// (default) keeps every note centered unless the module uses Set Panning (8xx) itself.
        /// Ignored for VGM/VGZ input.
        #[arg(long = "amiga-panning", default_value = "none")]
        amiga_panning: AmigaPanning,
        /// Print extra detail, including which effects were transcribed and which were ignored.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },
}

fn print_module_summary(module: &Module) {
    println!(
        "Verbose: {}, {} channel(s), {} pattern(s), order length {}, initial {} ticks/row @ {} BPM",
        module.source_format,
        module.num_channels,
        module.patterns.len(),
        module.order.len(),
        module.initial_speed_ticks,
        module.initial_tempo_bpm
    );
}

fn print_effect_table(counts: &std::collections::BTreeMap<u32, u32>) {
    let mut items: Vec<(u32, u32)> = counts.iter().map(|(&k, &v)| (k, v)).collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    for (code, count) in items {
        // Exx sub-commands use a synthetic 0xE0..=0xEF code (see
        // protracker::extended_subcommand_counts) — displayed as "E1x" (2 hex digits + one
        // placeholder), matching how modules/trackers document these, not "1xx" like every
        // other top-level code.
        let label = if (0xE0..=0xEF).contains(&code) { format!("E{:X}x", code & 0x0F) } else { format!("{code:X}xx") };
        println!("  {label:<5} {:<32} {} occurrence(s)", effect_name(code), count);
    }
}

fn print_effects_report(module: &Module) {
    if module.source_format != "protracker" {
        // Effect semantics (which codes ablemod actually simulates) are format-specific;
        // only ProTracker's are known here so far.
        return;
    }
    let implemented = implemented_effect_counts(module);
    if !implemented.is_empty() {
        println!("Verbose: effects correctly transcribed:");
        print_effect_table(&implemented);
    } else {
        println!("Verbose: no implemented effects found in this module.");
    }

    let unimplemented = unimplemented_effect_counts(module);
    if !unimplemented.is_empty() {
        println!("Verbose: unimplemented effects found (ignored during playback simulation):");
        print_effect_table(&unimplemented);
    } else {
        println!("Verbose: no unimplemented effects found.");
    }
}

/// VGM/VGZ files have no Module/Pattern/Sample IR to speak of (see formats::vgm's own doc
/// comment) — they're routed to entirely separate handlers rather than through load_module.
fn is_vgm_path(path: &std::path::Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(), Some("vgm") | Some("vgz"))
}

pub fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::List { module_path } => {
            if is_vgm_path(&module_path) {
                vgm_list_cmd(&module_path)
            } else {
                list_cmd(&module_path)
            }
        }
        Command::ExtractSamples { module_path, output_dir, verbose } => {
            if is_vgm_path(&module_path) {
                return Err(
                    "extract-samples doesn't apply to VGM/VGZ files: unlike a tracker module, a VGM has no stored \
                     samples to extract — its audio only exists once the chip registers are actually synthesized. \
                     Use 'convert' instead, which renders the full mix and one stem per chip channel as WAVs inside \
                     an Ableton project."
                        .to_string(),
                );
            }
            extract_samples_cmd(&module_path, &output_dir, verbose)
        }
        Command::ExtractMidi { module_path, output_path, verbose } => {
            if is_vgm_path(&module_path) {
                return Err(
                    "extract-midi doesn't apply to VGM/VGZ files: this project renders VGM chip audio directly \
                     (see 'convert') rather than transcribing register writes into MIDI notes."
                        .to_string(),
                );
            }
            extract_midi_cmd(&module_path, &output_path, verbose)
        }
        Command::Convert { module_path, template_path, output_path, amiga_panning, verbose } => {
            if is_vgm_path(&module_path) {
                vgm_convert_cmd(&module_path, template_path.as_deref(), &output_path, verbose)
            } else {
                convert_cmd(&module_path, template_path.as_deref(), &output_path, amiga_panning, verbose)
            }
        }
    }
}

fn vgm_list_cmd(path: &std::path::Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vgm = vgm::parse(&bytes)?;

    println!("Title:      {}", vgm.title.as_deref().unwrap_or("(unknown)"));
    println!("Game:       {}", vgm.game.as_deref().unwrap_or("(unknown)"));
    println!("System:     {}", vgm.system.as_deref().unwrap_or("(unknown)"));
    println!("Author:     {}", vgm.author.as_deref().unwrap_or("(unknown)"));
    println!("Format:     VGM v{}", vgm::version_string(vgm.version));
    println!("Duration:   {:.1}s ({} samples @ 44100Hz)", vgm.total_samples as f64 / 44100.0, vgm.total_samples);
    if let Some(loop_start) = vgm.loop_start_sample {
        println!(
            "Loop:       from {:.1}s, {:.1}s long",
            loop_start as f64 / 44100.0,
            vgm.loop_samples as f64 / 44100.0
        );
    } else {
        println!("Loop:       (none)");
    }

    println!("\nChips:");
    if vgm.ym2413_clock > 0 {
        println!("  YM2413 (OPLL FM) @ {} Hz — emulated by convert", vgm.ym2413_clock);
    }
    if vgm.ay8910_clock > 0 {
        let variant = if vgm.ay8910_is_ym { " [YM2149-compatible]" } else { "" };
        println!("  AY8910 (PSG){variant} @ {} Hz — emulated by convert", vgm.ay8910_clock);
    }
    if vgm.scc_clock > 0 {
        println!("  K051649/SCC (Konami) @ {} Hz — emulated by convert", vgm.scc_clock);
    }
    if vgm.ym3526_clock > 0 {
        println!("  YM3526 (OPL FM) @ {} Hz — approximated by convert via Ableton's Operator instrument (no bit-accurate WAV render)", vgm.ym3526_clock);
    }
    if vgm.ym3812_clock > 0 {
        println!("  YM3812 (OPL2 FM) @ {} Hz — approximated by convert via Ableton's Operator instrument (no bit-accurate WAV render)", vgm.ym3812_clock);
    }
    if !vgm.unsupported_commands.is_empty() {
        println!("\nOther chips used in this file (not emulated, silently skipped by convert):");
        let mut by_chip: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
        for (&cmd, &count) in &vgm.unsupported_commands {
            *by_chip.entry(vgm::unsupported_chip_name(cmd)).or_insert(0) += count;
        }
        for (name, count) in by_chip {
            println!("  {name}: {count} register write(s)");
        }
    }
    Ok(())
}

fn vgm_convert_cmd(
    path: &std::path::Path, template_path: Option<&std::path::Path>, output_path: &std::path::Path, verbose: bool,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vgm = vgm::parse(&bytes)?;
    if verbose {
        println!(
            "Verbose: VGM v{}, {:.1}s, YM2413={}Hz AY8910={}Hz SCC={}Hz YM3526={}Hz YM3812={}Hz",
            vgm::version_string(vgm.version),
            vgm.total_samples as f64 / 44100.0,
            vgm.ym2413_clock,
            vgm.ay8910_clock,
            vgm.scc_clock,
            vgm.ym3526_clock,
            vgm.ym3812_clock
        );
        if !vgm.unsupported_commands.is_empty() {
            println!("Verbose: other chips found in this file are not emulated and will be silent:");
            let mut by_chip: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
            for (&cmd, &count) in &vgm.unsupported_commands {
                *by_chip.entry(vgm::unsupported_chip_name(cmd)).or_insert(0) += count;
            }
            for (name, count) in by_chip {
                println!("  {name}: {count} register write(s)");
            }
        }
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let template_bytes: Vec<u8> = match template_path {
        Some(p) => std::fs::read(p).map_err(|e| format!("failed to read {}: {e}", p.display()))?,
        None => crate::export::als::default_template_bytes().to_vec(),
    };

    let master = crate::export::vgm_render::render(&vgm);
    let stems = crate::export::vgm_render::render_stems(&vgm);
    // Tempo doesn't affect which channels are non-silent, only their beat timing — any
    // placeholder value works fine here just to count them for this summary/warning.
    let wavetable_count =
        crate::export::vgm_wavetable::extract_channels(&vgm, 120.0).iter().filter(|c| !c.notes.is_empty()).count();
    let operator_count = [(vgm::Chip::Ym3526, vgm.ym3526_clock), (vgm::Chip::Ym3812, vgm.ym3812_clock)]
        .into_iter()
        .filter(|&(_, clock)| clock > 0)
        .map(|(chip, clock)| {
            crate::export::vgm_operator::extract_channels(&vgm, chip, clock, 120.0).iter().filter(|c| !c.notes.is_empty()).count()
        })
        .sum::<usize>();
    crate::export::vgm_als::export_als(&vgm, &master, &stems, output_path, &template_bytes)?;

    println!("wrote {}", output_path.display());
    println!(
        "wrote samples to {}",
        output_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("Samples").join("Imported").display()
    );
    if stems.is_empty() && wavetable_count == 0 && operator_count == 0 {
        println!(
            "\nWarning: the project has no tracks. None of this file's music data is on a chip \
             this converter emulates (YM2413, AY8910, K051649/SCC, YM3526, YM3812) — run \
             `ablemod list` on it to see which chip(s) actually carry the music."
        );
    } else if verbose {
        match vgm.loop_start_sample {
            Some(_) => println!("Verbose: {} WAV track(s), each split into intro+loop clips at the file's declared loop point", stems.len()),
            None => println!("Verbose: {} WAV track(s) (no declared loop point — one full-length clip each)", stems.len()),
        }
        if wavetable_count > 0 {
            println!("Verbose: {wavetable_count} Wavetable track(s) (Ableton Wavetable instrument, SCC channels only — an approximation, see the WAV tracks for the accurate render)");
        }
        if operator_count > 0 {
            println!("Verbose: {operator_count} Operator track(s) (Ableton Operator instrument, YM3526/YM3812 channels only — this chip's only audible result, no bit-accurate WAV render exists)");
        }
    }
    Ok(())
}

fn list_cmd(module_path: &std::path::Path) -> Result<(), String> {
    let module = load_module(module_path)?;

    println!("Title:      {}", module.title);
    println!("Format:     {}", module.source_format);
    println!("Channels:   {}", module.num_channels);
    println!("Patterns:   {}", module.patterns.len());
    println!("Order:      {:?}", module.order);
    println!("Restart at: {}", module.restart_position);
    println!(
        "Speed/BPM:  {} ticks/row, {} BPM (initial)",
        module.initial_speed_ticks, module.initial_tempo_bpm
    );
    let effects = module.effects_used();
    let effects_str: Vec<String> = effects.iter().map(|e| format!("{e:X}")).collect();
    println!("Effects:    {}", if effects_str.is_empty() { "(none)".to_string() } else { effects_str.join(", ") });

    println!("\nSamples:");
    for sample in &module.samples {
        if sample.is_empty() {
            continue;
        }
        let frames = sample.pcm16.len() / 2;
        let loop_str = if sample.has_loop() {
            format!(", loop {}-{}", sample.loop_start, sample.loop_start + sample.loop_length)
        } else {
            String::new()
        };
        let name = if sample.name.is_empty() { "(unnamed)".to_string() } else { sample.name.clone() };
        println!(
            "  {:2}. {:<22} {:6} frames  vol={:2}  rate={}Hz{}",
            sample.index, name, frames, sample.volume, sample.sample_rate_hz, loop_str
        );
    }
    Ok(())
}

fn extract_samples_cmd(module_path: &std::path::Path, output_dir: &std::path::Path, verbose: bool) -> Result<(), String> {
    let module = load_module(module_path)?;
    if verbose {
        print_module_summary(&module);
    }
    std::fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    let mut count = 0;
    for sample in &module.samples {
        if sample.is_empty() {
            continue;
        }
        let out_path = output_dir.join(sample_wav_filename(sample));
        write_sample_wav(sample, &out_path).map_err(|e| e.to_string())?;
        println!("wrote {}", out_path.display());
        if verbose {
            let frames = sample.pcm16.len() / 2;
            let loop_str = if sample.has_loop() {
                format!("loop {}-{}", sample.loop_start, sample.loop_start + sample.loop_length)
            } else {
                "no loop".to_string()
            };
            println!("  Verbose: {} frames, {}Hz, volume={}, {}", frames, sample.sample_rate_hz, sample.volume, loop_str);
        }
        count += 1;
    }

    println!("\nExtracted {} sample(s) to {}", count, output_dir.display());
    Ok(())
}

fn extract_midi_cmd(module_path: &std::path::Path, output_path: &std::path::Path, verbose: bool) -> Result<(), String> {
    let module = load_module(module_path)?;
    if verbose {
        print_module_summary(&module);
        print_effects_report(&module);
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_midi(&module, output_path).map_err(|e| e.to_string())?;
    println!("wrote {}", output_path.display());
    if verbose {
        let song = compute_song_events(&module);
        let total_notes: usize = song.notes_by_sample.values().map(|v| v.len()).sum();
        println!(
            "Verbose: {} note(s), {} tempo change(s), {:.2} beats total",
            total_notes,
            song.tempo_changes.len(),
            song.total_beats
        );
    }
    Ok(())
}

fn convert_cmd(
    module_path: &std::path::Path, template_path: Option<&std::path::Path>, output_path: &std::path::Path,
    amiga_panning: AmigaPanning, verbose: bool,
) -> Result<(), String> {
    let module = load_module(module_path)?;
    if verbose {
        print_module_summary(&module);
        print_effects_report(&module);
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let template_bytes: Vec<u8> = match template_path {
        Some(p) => std::fs::read(p).map_err(|e| format!("failed to read {}: {e}", p.display()))?,
        None => crate::export::als::default_template_bytes().to_vec(),
    };
    export_als(&module, output_path, &template_bytes, amiga_panning)?;
    println!("wrote {}", output_path.display());
    println!("wrote samples to {}", output_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("Samples").join("Imported").display());
    if verbose {
        let song = compute_song_events(&module);
        // A sample triggered on several overlapping tracker channels gets more than one
        // voice/track (see the voice-assignment pass in export::notes::compute_song_events) —
        // count those too, so this matches the number of tracks export_als actually wrote.
        let track_count: usize =
            song.notes_by_sample.values().map(|notes| notes.iter().map(|n| n.voice + 1).max().unwrap_or(1)).sum();
        let total_notes: usize = song.notes_by_sample.values().map(|v| v.len()).sum();
        println!(
            "Verbose: {} track(s), {} note(s), {} tempo change(s), {:.2} beats total",
            track_count,
            total_notes,
            song.tempo_changes.len(),
            song.total_beats
        );
    }
    Ok(())
}

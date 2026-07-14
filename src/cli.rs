use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::export::als::{export_als, AmigaPanning};
use crate::export::midi::write_midi;
use crate::export::notes::compute_song_events;
use crate::export::wav::{sample_wav_filename, write_sample_wav};
use crate::formats::base::Module;
use crate::formats::detect::load_module;
use crate::formats::protracker::{effect_name, implemented_effect_counts, unimplemented_effect_counts};

/// Convert ProTracker/FastTracker/ScreamTracker modules to Ableton Live projects.
#[derive(Parser)]
#[command(name = "ablemod")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Display the contents of a tracker module.
    List { module_path: PathBuf },

    /// Extract all samples from a tracker module as .wav files.
    ExtractSamples {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_dir: PathBuf,
        /// Print extra detail about each sample as it's extracted.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Convert a tracker module's patterns to a .mid file (one track per sample).
    ExtractMidi {
        module_path: PathBuf,
        #[arg(short = 'o', long = "output")]
        output_path: PathBuf,
        /// Print extra detail, including which effects were transcribed and which were ignored.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Convert a tracker module into an Ableton Live Set (.als), one Sampler per sample.
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

pub fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::List { module_path } => list_cmd(&module_path),
        Command::ExtractSamples { module_path, output_dir, verbose } => {
            extract_samples_cmd(&module_path, &output_dir, verbose)
        }
        Command::ExtractMidi { module_path, output_path, verbose } => {
            extract_midi_cmd(&module_path, &output_path, verbose)
        }
        Command::Convert { module_path, template_path, output_path, amiga_panning, verbose } => {
            convert_cmd(&module_path, template_path.as_deref(), &output_path, amiga_panning, verbose)
        }
    }
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

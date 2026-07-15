# ablemod

A CLI that converts tracker modules and chiptune register-dump rips into Ableton Live projects (`.als`), plus WAV/MIDI extraction along the way.

```
ablemod list <file>                              # metadata, chips/effects used, what will/won't convert
ablemod extract-samples <file.mod> -o <dir>      # samples -> WAV
ablemod extract-midi <file.mod> -o <dir>         # channels -> MIDI
ablemod convert <file> -o <project.als> [--template <template.als>] [--verbose]
```

`convert` auto-detects the input by extension (`.mod` / `.xm` / `.s3m` / `.vgm` / `.vgz`) and produces a ready-to-open Live Set: audio tracks with Simpler/plain samples for tracker modules, or rendered-audio + approximated-instrument tracks for chiptune rips.

## Status by format

### ProTracker `.mod` — done
Full pipeline: `list`, `extract-samples`, `extract-midi`, `convert`. Handles the real effect set (arpeggio, portamento, vibrato, volume slides, position jump/pattern break for correct playback-order simulation, ...), splits colliding notes across voice tracks, groups/colors/arms them in the exported project. Convention: MOD period 428 (PT "C-2") = MIDI note 60, 8363 Hz reference sample rate.

### FastTracker `.xm` / ScreamTracker `.s3m` — not started
Recognized by extension, `detect_format` returns a clean "not implemented yet" error rather than misparsing. No parser exists (`formats::` has no `fasttracker.rs`/`screamtracker.rs`). The tracker-side IR (`formats::base::Module`) was designed to let a new parser plug straight into the existing WAV/MIDI/.als export — that part shouldn't need to change.

### VGM / VGZ (chiptune register-dump rips) — chip-by-chip

| Chip | Status | Path |
|---|---|---|
| YM2413 (OPLL FM) | Bit-accurate | Ported from `emu2413` (Mitsutaka Okazaki) → WAV |
| AY-3-8910 / YM2149 (PSG) | Bit-accurate | Ported from `ayumi` (Peter Sovietov) → WAV |
| K051649/SCC (Konami) | Bit-accurate | Ported from MAME's `k051649.cpp` → WAV, **plus** an experimental Ableton Wavetable-instrument track alongside it (see below) |
| YM3526 (OPL) | Approximated, no bit-accurate render | Register log → Ableton Operator instrument (see below) |
| YM3812 (OPL2) | Approximated, no bit-accurate render | Same Operator pipeline as YM3526 (register-compatible) |
| Everything else (YM2612, YM2151, YM2203, YM2608/2610, Y8950, YMF262/OPL3, Sega PCM, RF5C68/164, NES APU, GameBoy DMG, ...) | Not supported | Parsed and skipped cleanly — `list` reports exact register-write counts per chip, `convert` warns if a file ends up with zero tracks because none of its chips are emulated |

`formats::vgm::parse` hard-errors on any *unrecognized* command byte rather than guessing a length and silently desyncing the rest of the file — chips above "not supported" are still safe to have in a file, they just contribute no audio.

## Experimental / approximate — read before trusting the output

Two chiptune paths are explicitly **not** bit-accurate reproductions of real hardware. Both exist because they let the result live inside Ableton as an editable instrument instead of a frozen WAV, at the cost of fidelity:

**SCC → Wavetable** (`export::vgm_wavetable`, alongside the bit-accurate WAV track for the same channel — always compare the two by ear):
- Vibrato/pitch bends are recovered as automation on Wavetable's *Detune* parameter (±0.5 semitone native range — Transpose/"Semi" was tried first and found to be semitone-quantized in practice).
- Mid-note waveform rewrites (a common trick to fake an envelope the chip doesn't have) become Position-automation "frames".
- The amp envelope is deliberately *unfaithful* to real hardware (chip has none) — tuned by ear for a more playable synth voice.

**YM3526/YM3812 → Operator** (`export::vgm_operator`) — scoped deliberately small for a first pass, agreed with the project owner:
- **FM only.** A channel's own Connection/Algorithm register bit is ignored; every channel is wired as a 2-operator modulator→carrier FM pair. A channel actually using additive mode will sound different, not broken.
- **No rhythm mode.** OPL's global percussion mode (channels 6-8 repurposed as fixed drum voices) isn't detected — those channels stay ordinary melodic FM even on games that use it.
- **Hard retrigger only.** Unlike SCC, there's no bend-vs-new-note logic — any F-Number/Block change on a held note always retriggers. Untested assumption: arcade FM music leans on the chip's own global hardware vibrato LFO more than per-note pitch bends, so this should matter less here than it did for SCC.
- **Static patch per channel, not per-note.** Timbre (operator ratios/feedback/envelope rates) is snapshotted once from the channel's first note and baked into the cloned device — never re-automated even if a game reuses the channel for a different instrument later in the song.
- **Envelope rate→time is a labeled approximation**, not a hardware-derived formula (none was found; Operator's linear-segment ADSR couldn't reproduce OPL's real logarithmic curve exactly regardless). Log-interpolated across a plausible instant-to-seconds range, monotonic, not calibrated against real hardware timing.
- The Operator↔OPL parameter correspondences (Coarse=Multiple, Feedback rescale, Attack/Decay/Sustain-Level mapping) are inferred from matching value ranges in a captured template project, not confirmed against real Ableton listening tests the way the SCC/Wavetable fixes were.

**Known concrete next steps**, roughly in order of value:
1. Listen to real Operator output (Toki, Bubble Bobble) and tune the envelope/gain mapping by ear the way SCC's Wavetable path was iterated — nothing above has been verified by ear yet, only structurally (XML validity, ID uniqueness, plausible parameter ranges).
2. Bend/vibrato absorption for OPL, if real files show the hard-retrigger approximation sounds bad.
3. A real YM3526/YM3812 chip emulator (`chips::`) for a bit-accurate WAV fallback — YM2413 (a close 2-op FM cousin, OPLL) is already ported and could plausibly seed this; likely a larger effort than the other three ports since YM3526 has fully programmable operators (no ROM-fixed patches like OPLL).
4. Rhythm-mode decoding for OPL (distinct register semantics on channels 6-8).
5. XM/S3M parsers, reusing the existing tracker IR/export pipeline unchanged.

## Testing

`cargo test` — 140 tests as of this writing, all synthetic/hand-built VGM byte streams and MOD fixtures (no copyrighted game audio in the repo; a `fichiers/` directory of real rips used for manual verification is gitignored). Chip emulator ports are checked against their own register-frequency formulas (no real hardware to compare against). Several bugs in this project's history were only caught by converting a real file and listening — `cargo test` passing is necessary, not sufficient, evidence of correctness for the chiptune paths above.

## Architecture

```
formats::base       tracker-agnostic IR (Module/Sample/Pattern/Cell)
formats::protracker  .mod parser -> IR
formats::playback    order/effect playback-order simulation shared by MIDI + .als export
formats::vgm         VGM/VGZ register-write-log parser (no IR — chips synthesize in real time)
chips::              faithful ports of real chip emulators (ay8910, ym2413, scc)
export::als          tracker IR -> .als (clones real captured device XML, doesn't hand-generate it)
export::vgm_render   drives chips:: through the VGM write log -> WAV
export::vgm_wavetable / vgm_operator   VGM write log -> notes/automation for a native Ableton instrument
export::vgm_als      packages vgm_render/vgm_wavetable/vgm_operator output into a Live Set
templates/*.xml       real device/track XML captured from actual Ableton projects — Ableton's
                       .als schema is undocumented, cloning a working example beats guessing
```

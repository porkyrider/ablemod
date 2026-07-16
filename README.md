# ablemod

A CLI that converts tracker modules and chiptune register-dump rips into Ableton Live projects (`.als`), plus WAV/MIDI extraction along the way.

```
ablemod list <file>                                          # metadata, chips/effects used, what will/won't convert
ablemod extract-samples <file.mod> -o <dir>                  # tracker: raw stored samples -> WAV
ablemod extract-midi <file.mod> -o <dir>                     # tracker: channels -> MIDI
ablemod extract-mixed-tracks <file.vgz> -o <mix.wav>         # VGM/VGZ: full chip-emulated mix -> one WAV
ablemod extract-separated-tracks-wav <file.vgz> -o <dir>     # VGM/VGZ: one WAV per chip channel (stem)
ablemod convert-als <file> -o <project.als> [--template <template.als>] [--verbose]
```

`convert-als` auto-detects the input by extension (`.mod` / `.xm` / `.s3m` / `.vgm` / `.vgz`) and produces a ready-to-open Live Set: audio tracks with Simpler/plain samples for tracker modules, or rendered-audio + approximated-instrument tracks for chiptune rips. `extract-mixed-tracks`/`extract-separated-tracks-wav` give the same chip-emulated audio `convert-als` renders for a VGM/VGZ file, without an Ableton project wrapped around it — there's no equivalent for tracker modules yet (they have no audio-rendering engine of their own; Ableton's own Sampler does that synthesis after `convert-als`), so both are VGM/VGZ-only for now.

## Status by format

### ProTracker `.mod` — done
Full pipeline: `list`, `extract-samples`, `extract-midi`, `convert-als`. Handles the real effect set (arpeggio, portamento, vibrato, volume slides, position jump/pattern break for correct playback-order simulation, ...), splits colliding notes across voice tracks, groups/colors/arms them in the exported project. Convention: MOD period 428 (PT "C-2") = MIDI note 60, 8363 Hz reference sample rate.

### FastTracker `.xm` / ScreamTracker `.s3m` — not started
Recognized by extension, `detect_format` returns a clean "not implemented yet" error rather than misparsing. No parser exists (`formats::` has no `fasttracker.rs`/`screamtracker.rs`). The tracker-side IR (`formats::base::Module`) was designed to let a new parser plug straight into the existing WAV/MIDI/.als export — that part shouldn't need to change.

### VGM / VGZ (chiptune register-dump rips) — chip-by-chip

Audio rendering (`export::vgm_render`) runs on [libvgm](https://github.com/ValleyBell/libvgm)
(ValleyBell), vendored under `vendor/libvgm/` and linked in via a thin FFI boundary
(`native/`, `chips::ffi`/`chips::player_ffi`) — not hand-ported Rust. `formats::vgm.rs` (this
project's own parser) is kept independently for header/GD3-tag metadata and the raw
register-write log `export::vgm_wavetable`/`export::vgm_operator` need — a second, deliberate
parse of the same file, not something libvgm's own player replaced.

| Chip | Status | Core |
|---|---|---|
| YM2413 (OPLL FM) | Bit-accurate | `emu2413` (Mitsutaka Okazaki) → WAV |
| AY-3-8910 / YM2149 (PSG) | Bit-accurate | `emu2149` (Mitsutaka Okazaki) → WAV |
| K051649/SCC (Konami) | Bit-accurate | MAME's `k051649.c` → WAV, **plus** an optional Ableton Wavetable-instrument track alongside it (see below) |
| YM3526 (OPL) | Bit-accurate | MAME's `fmopl.c` → WAV, **plus** an optional Ableton Operator-instrument track alongside it (see below) |
| YM3812 (OPL2) | Bit-accurate | Same `fmopl.c` core as YM3526 (register-compatible) |
| YM2612 (OPN2 FM, Sega Genesis/Mega Drive) | Bit-accurate | MAME/Genesis Plus GX's `fmopn.c` → WAV |
| YM2151 (OPM FM, arcade/X68000) | Bit-accurate | MAME's `ym2151.c` → WAV |
| YM2203 (OPN FM) | Bit-accurate | Same `fmopn.c` core as YM2612 → WAV |
| YM2608 (OPNA FM) | Bit-accurate | Same `fmopn.c` core, plus `ymdeltat.c` (ADPCM-B) → WAV |
| YM2610 (OPNB FM, Neo Geo) | Bit-accurate | Same `fmopn.c`/`ymdeltat.c` as YM2608 → WAV |
| Sega PCM | Bit-accurate | MAME's `segapcm.c` → WAV |
| RF5C68 / RF5C164 (PCM) | Bit-accurate | MAME's `rf5c68.c` → WAV |
| GameBoy DMG | Bit-accurate | MAME's `gb_mame.c` → WAV |
| NES APU | Bit-accurate | MAME's `nes_apu.c` → WAV (no FDS expansion-audio support) |
| Everything else (Y8950, YMF262/OPL3, YMF278B, YMZ280B, a second AY8910, PWM, MultiPCM, uPD7759, OKIM6258/6295, HuC6280, K053260, Pokey, WonderSwan, SAA1099, ES5503/ES5506, X1-010, QSound, SCSP, YMF271, K054539, C140, GA20, ...) | Not supported | Parsed and skipped cleanly — `list` reports exact register-write counts per chip, `convert-als` warns if a file ends up with zero tracks because none of its chips are emulated. libvgm supports many of these too; this project just doesn't vendor/link their cores yet (see build.rs's own `SNDDEV_*` defines) |

The ten chips added after the initial libvgm migration (YM2612 through NES APU above) are
WAV-only — no Wavetable/Operator-style native-instrument approximation, unlike SCC/OPL below.
`export::vgm_render` only attempts a per-channel stem render for one of these ten if the file's
own header declares (and actually writes to) that chip, unlike the original five chips it
always tries — between them these ten chips add up to ~85 extra channels, and most real files
only ever use one or two, so this avoids needlessly multiplying render time.

`formats::vgm::parse` hard-errors on any *unrecognized* command byte rather than guessing a length and silently desyncing the rest of the file — chips above "not supported" are still safe to have in a file, they just contribute no audio. Two real bugs were found in libvgm itself while integrating it (not hypothetical, caught by this project's own tests against independently-verified formulas): K051649's own `k051649_update` played exactly one octave sharp (patched, see `vendor/libvgm/README.md`), and nothing called `Reset()` after `Start()` in this project's own FFI shim (emu2413 explicitly skips self-initializing — fixed in `native/shim.c`).

## Experimental / approximate — read before trusting the output

Every chip above has a bit-accurate WAV render — the only kind `convert-als` generates by default
(see `export::vgm_als::export_als`'s own `generate_approximation_tracks` parameter). SCC and
OPL (YM3526/YM3812) *can additionally* drive a native Ableton instrument track instead of/
alongside the WAV, but that path isn't wired into the CLI's default `convert-als` output — not
because the WAV is missing, but because an editable instrument inside Ableton is sometimes more
useful than a frozen render, at the cost of fidelity. Always compare the two by ear before
trusting either.

**SCC → Wavetable** (`export::vgm_wavetable`):
- Vibrato/pitch bends are recovered as automation on Wavetable's *Detune* parameter (±0.5 semitone native range — Transpose/"Semi" was tried first and found to be semitone-quantized in practice).
- Mid-note waveform rewrites (a common trick to fake an envelope the chip doesn't have) become Position-automation "frames".
- The amp envelope is deliberately *unfaithful* to real hardware (chip has none) — tuned by ear for a more playable synth voice.

**YM3526/YM3812 → Operator** (`export::vgm_operator`) — scoped deliberately small for a first pass, agreed with the project owner:
- **FM only.** A channel's own Connection/Algorithm register bit is ignored; every channel is wired as a 2-operator modulator→carrier FM pair. A channel actually using additive mode will sound different, not broken.
- **No rhythm mode.** OPL's global percussion mode (channels 6-8 repurposed as fixed drum voices) isn't detected in this specific export path — those channels stay ordinary melodic FM here even on games that use it (the bit-accurate WAV render handles rhythm mode correctly, since libvgm's own fmopl.c does).
- **Hard retrigger only.** Unlike SCC, there's no bend-vs-new-note logic — any F-Number/Block change on a held note always retriggers. Untested assumption: arcade FM music leans on the chip's own global hardware vibrato LFO more than per-note pitch bends, so this should matter less here than it did for SCC.
- **Static patch per channel, not per-note.** Timbre (operator ratios/feedback/envelope rates) is snapshotted once from the channel's first note and baked into the cloned device — never re-automated even if a game reuses the channel for a different instrument later in the song.
- **Envelope rate→time is a labeled approximation**, not a hardware-derived formula (none was found; Operator's linear-segment ADSR couldn't reproduce OPL's real logarithmic curve exactly regardless). Log-interpolated across a plausible instant-to-seconds range, monotonic, not calibrated against real hardware timing.
- The Operator↔OPL parameter correspondences (Coarse=Multiple, Feedback rescale, Attack/Decay/Sustain-Level mapping) are inferred from matching value ranges in a captured template project, not confirmed against real Ableton listening tests the way the SCC/Wavetable fixes were.

**Known concrete next steps**, roughly in order of value:
1. Listen to the bit-accurate OPL WAV render (Toki, Bubble Bobble) against the Operator approximation for the same channels, and the ten chips added after it (YM2612 especially — the most commonly requested one) against real files — every one of them has only been verified structurally so far (non-silent, plausible peak/dynamics on synthetic test tones; see `tests/vgm_tests.rs`'s own `test_*_renders_audible_output` tests), not confirmed by ear on a real rip the way SCC/OPL's own fixes were.
2. Bend/vibrato absorption for the Operator export, if real files show the hard-retrigger approximation sounds bad by comparison.
3. Rhythm-mode decoding for the Operator export specifically (distinct register semantics on channels 6-8) — the WAV render already handles this correctly.
4. Link more of libvgm's remaining chip cores (Y8950, YMF262/OPL3, Sega Saturn's SCSP, ...) — the FFI/build plumbing is now well-exercised across fifteen chips; each additional one is mostly a `build.rs`/`SNDDEV_*` addition plus a `chips::player_ffi::dev_id` entry and stem-naming in `export::vgm_render`, not new infrastructure.
5. XM/S3M parsers, reusing the existing tracker IR/export pipeline unchanged.

## Testing

`cargo test` — 145 tests as of this writing, all synthetic/hand-built VGM byte streams and MOD fixtures (no copyrighted game audio in the repo; a `fichiers/` directory of real rips used for manual verification is gitignored). `tests/chips_tests.rs` checks each of the five original chip cores against its own register-frequency formula via autocorrelation (no real hardware to compare against) — this is what caught both real libvgm bugs listed above. The ten chips added afterward are checked more lightly in `tests/vgm_tests.rs` (header-clock/presence detection for all ten, plus a synthetic non-silence check for YM2612/GameBoy DMG/NES APU specifically) rather than a full frequency-formula proof per chip. Several bugs in this project's history were only caught by converting a real file and listening — `cargo test` passing is necessary, not sufficient, evidence of correctness for the chiptune paths above.

## Licensing

**GPL-2.0-or-later** (see `LICENSE`) — not this project's own historical preference (every chip
core it used to hand-port, and still uses for AY8910/YM2413/SCC's actual DSP math via FFI now,
is MIT/BSD-3), but forced by vendoring libvgm's `fmopl.c` (MAME's YM3526/YM3812 core, GPL-2.0+)
under `vendor/libvgm/`. Once any GPL component is linked in, the combined binary must be
GPL-compatible as a whole. See `vendor/libvgm/README.md` for the exact license of every
vendored file and `build.rs`'s own comment on why `fmopl.c` specifically was chosen over the
LGPL-2.1+ alternatives (Nuked OPL3, DOSBox's AdLibEmu) libvgm also offers for YM3812.

## Architecture

```
formats::base       tracker-agnostic IR (Module/Sample/Pattern/Cell)
formats::protracker  .mod parser -> IR
formats::playback    order/effect playback-order simulation shared by MIDI + .als export
formats::vgm         VGM/VGZ header/GD3-tag parser + register-write log (used by
                     vgm_wavetable/vgm_operator's note extraction — NOT by audio rendering,
                     which goes through chips::player_ffi instead; see its own module comment)
chips::ffi           thin FFI boundary to individual libvgm chip cores (native/shim.c) —
                     backs chips::scc/ay8910/ym2413's own regression tests
chips::player_ffi    thin FFI boundary to libvgm's own VGMPlayer (native/player_shim.cpp) —
                     this project's actual VGM/VGZ audio rendering path
export::als          tracker IR -> .als (clones real captured device XML, doesn't hand-generate it)
export::vgm_render   drives chips::player_ffi across the whole file (full mix) and once per
                     muted channel (stems) -> WAV
export::vgm_wavetable / vgm_operator   formats::vgm's own write log -> notes/automation for a
                     native Ableton instrument (SCC/OPL only, an approximation alongside the
                     WAV render — see "Experimental / approximate" above)
export::vgm_als      packages vgm_render/vgm_wavetable/vgm_operator output into a Live Set
templates/*.xml       real device/track XML captured from actual Ableton projects — Ableton's
                       .als schema is undocumented, cloning a working example beats guessing
vendor/libvgm/         vendored subset of libvgm (ValleyBell) — see its own README.md for
                       exactly what's included, why, and its one local patch (a real bug found
                       in K051649's own frequency formula)
native/                the C/C++ shim layer between chips::ffi/player_ffi and vendor/libvgm/
```

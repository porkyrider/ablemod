# ablemod

Two executables: `ablemod`, a terminal CLI that converts tracker modules and chiptune
register-dump rips into Ableton Live projects (`.als`), plus WAV/MIDI extraction along the
way; and `ablemod-gui`, a point-and-click front-end for the same actions (see its own section
below).

```
ablemod list <file>                                          # metadata, chips/effects used, what will/won't convert
ablemod extract-samples <file.mod> -o <dir>                  # tracker: raw stored samples -> WAV
ablemod extract-midi <file.mod> -o <dir>                     # tracker: channels -> MIDI
ablemod extract-mixed-tracks <file.vgz> -o <mix.wav>         # VGM/VGZ: full chip-emulated mix -> one WAV
ablemod extract-separated-tracks-wav <file.vgz> -o <dir>     # VGM/VGZ: one WAV per chip channel (stem)
ablemod convert-als <file> -o <project.als> [--template <template.als>] [--verbose]
ablemod preview <file.vgz> [--no-loop] [--record out.mp4]    # VGM/VGZ: live waveform preview / video export
```

`convert-als` auto-detects the input by extension (`.mod` / `.xm` / `.s3m` / `.vgm` / `.vgz`) and produces a ready-to-open Live Set: audio tracks with Simpler/plain samples for tracker modules, or rendered-audio + approximated-instrument tracks for chiptune rips. `extract-mixed-tracks`/`extract-separated-tracks-wav` give the same chip-emulated audio `convert-als` renders for a VGM/VGZ file, without an Ableton project wrapped around it — there's no equivalent for tracker modules yet (they have no audio-rendering engine of their own; Ableton's own Sampler does that synthesis after `convert-als`), so both, and `preview` below, are VGM/VGZ-only for now.

## `ablemod preview` — live waveform preview / video export

A subcommand of `ablemod` itself (`src/preview.rs`) behind the `preview` Cargo feature — a
default `cargo build` never needs SDL2/SDL2_ttf (system libraries, unlike every other native
dependency this project vendors/compiles from source) just to build the main CLI:

```
cargo build --release --features preview     # the `preview` subcommand only exists in this build
ablemod preview <file.vgz> [--no-loop]       # play the file's own chip-emulated stems live
ablemod preview <file.vgz> --record out.mp4  # render one playthrough to a video file instead
```

Renders the given VGM/VGZ file's stems in memory (the same render `extract-separated-tracks-wav`
writes to disk — `export::vgm_render::render_stems` — no intermediate WAV files) and plays them
together, in sync, in one fixed-size (1920x1080) window laid out as a grid of small cells: one
live oscilloscope cell per non-silent chip channel (up to 16, one of which is always the info
panel — see below — so a channel count past 15 still plays in full, just without its own cell).
The grid — and so each cell's own size — adapts to the channel count: few channels get a few
big cells, many channels get many small ones, rather than the window itself growing. Each
waveform cell is a live scope, not a DAW-style clip overview: it shows a short trailing sample
of that channel's own audio as it's actually being played, redrawn fresh every video frame —
the real wave shape/cycles, not a compressed whole-file silhouette. One cell, reserved near the
middle of the grid, is always the **info panel** instead of a waveform: the same metadata
`ablemod list` prints (title/game/system/author/duration/loop/chips — `formats::vgm::
summary_lines`, shared by both), rendered once at startup (it doesn't change while playing) via
a bundled font (`assets/DejaVuSansMono.ttf`, Bitstream Vera-derived license, see `assets/
DejaVuSansMono-LICENSE.txt`) through SDL2_ttf.

Playback and rendering both go through SDL2 (not a separate audio crate) specifically so they
share one clock — every cell's trace position is read directly from the sample count SDL2's own
audio callback has actually written, not a wall-clock timer that could drift out of sync with
what's really coming out of the speakers. Playback loops back to the start by default
(`--no-loop` to play once and stop), matching how a game's own music loops indefinitely too.
Space = pause/resume, Esc/Q or closing the window = quit. Requires SDL2 + SDL2_ttf installed
(`brew install sdl2 sdl2_ttf` on macOS, `apt install libsdl2-dev libsdl2-ttf-dev` on
Debian/Ubuntu) — located via `pkg-config` at build time.

`--record <file.mp4>` renders exactly one playthrough to a video file instead of opening a
live, audio-driven window: frame timing is derived deterministically from the sample count
(frame *f* → sample *f/fps·rate*), not real elapsed time or a live audio device, so it runs as
fast as the CPU can encode (measured ~3.5x realtime on a real 9-channel/53s file) rather than
waiting through the actual song length. Each rendered frame's pixels are piped as raw video
into an `ffmpeg` subprocess — not a Rust encoding crate, since there's no mature pure-Rust
H.264 encoder and shelling out to `ffmpeg` is the standard, pragmatic approach — which also
muxes in a freshly-rendered mixdown WAV of the same channels (rendered once in software, not
captured from a live audio device). Requires `ffmpeg` installed and on `PATH` (`brew install
ffmpeg` / `apt install ffmpeg`) — a separate runtime dependency from SDL2, only needed for
this one flag, checked at the point `--record` is actually used rather than at startup.

## `ablemod-gui` — point-and-click front-end

A separate executable (`src/bin/gui.rs`) behind the `gui` Cargo feature, built on
[egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui/tree/master/crates/eframe)
(native file dialogs via [`rfd`](https://github.com/PolyMeilex/rfd)):

```
cargo build --release --features preview,gui   # builds target/release/ablemod-gui too — both
                                                # features together, see below for why
ablemod-gui
```

A dark-themed window matching `ablemod preview`'s own palette: drop a tracker module or
VGM/VGZ file (or use File > Open), and it settles into a file card with a colored type badge
and a grouped list of actions underneath — Convert to Ableton Live Set, Extract Samples/MIDI
for tracker modules; Convert, Extract Mixed/Separated Tracks, Preview, Export Video for
VGM/VGZ. Each action opens a native Open/Save dialog for its own input/output path, then runs
on a background thread with the action list dimmed and a spinner showing, reporting
success/failure in a console-style status panel below (color-coded: blue while running, green
on success, red on failure).

This binary **never calls into `ablemod`'s own `export::`/`formats::` code directly** — every
action shells out to the `ablemod` CLI binary sitting next to it (falling back to `ablemod` on
`PATH`), on a background thread so the UI stays responsive. Two reasons: it keeps every actual
conversion/rendering behavior in exactly one place (this is a thin launcher, not a second
implementation that could drift out of sync with the CLI), and it sidesteps a real macOS
constraint — SDL2 (`ablemod preview`'s own toolkit) and eframe's winit backend each expect to
own the process' main thread run loop, which two GUI toolkits sharing one process can't both
do. Running `preview`/`preview --record` as their own separate OS process, each with its own
main thread, avoids that entirely. A practical consequence: **build with both `preview,gui`
together** — `ablemod-gui`'s own Preview/Export Video buttons work by invoking the `ablemod`
binary's `preview` subcommand, so if that binary wasn't itself built with `--features preview`,
those two buttons report a clear error (the same one `ablemod preview` itself gives) rather
than silently doing nothing.

## Status by format

### ProTracker `.mod` — done
Full pipeline: `list`, `extract-samples`, `extract-midi`, `convert-als`. Handles the real effect set (arpeggio, portamento, vibrato, volume slides, position jump/pattern break for correct playback-order simulation, ...), splits colliding notes across voice tracks, groups/colors/arms them in the exported project. Convention: MOD period 428 (PT "C-2") = MIDI note 60, 8363 Hz reference sample rate.

### FastTracker 2 `.xm` — done
Full pipeline: `list`, `extract-samples`, `extract-midi`, `convert-als` (`formats::fasttracker2`). Reuses `.mod`'s own effect dispatch unchanged for the shared 0x0-0xF set (identical semantics in both formats) plus Key Off (`Kxx`/note 97, translated into note-off tracking); XM's own volume column is folded into the same single effect slot the IR provides, promoting it to the equivalent already-implemented effect (Set Volume/Slide/Panning/Vibrato/Tone Portamento) whenever the row's real effect column is free, and dropped (silently, like any other unimplemented effect) on the rare row where both are used at once. XM's instrument→keymap→sample indirection is resolved at parse time: every `(instrument, note)` pair actually reachable in the song is traced to its concrete raw sample, so a multisample/keyzone-split instrument correctly becomes several separate Sampler tracks rather than just its first sample played at every pitch. Instrument volume/panning envelopes are translated into real Ableton automation (attack shape emitted at note-on, release shape at Key Off, freezing at the envelope's sustain point in between) — the same track-level automation mechanism already used for Volume Slide/Portamento/Vibrato. Per-sample panning (XM's own instrument default-pan byte) is a real per-track Panorama baseline, not the MOD-only synthetic Amiga L/R/R/L faking. Convention: XM note 49 ("C-4" in FT2's own display) = MIDI note 60, same physical pitch `.mod`'s own period-428 anchor uses; per-sample finetune/relative-note is folded into `sample_rate_hz`, same division of labor as `.mod`.

**Known limitations**, all deliberate scope cuts rather than bugs:
- Only the linear frequency table is simulated — a module declaring the (rare, non-linear) Amiga frequency table still gets the linear-formula pitch math.
- Ping-pong sample loops are folded into plain forward loops.
- Envelope loop points (`Lxx`/loop-flagged envelopes) aren't simulated — an envelope plays its authored points once and freezes at the sustain point, never re-looping a middle segment.
- Volume fadeout isn't applied.
- Extended effects `G`/`H`/`L`/`P`/`R`/`T`/`X` (global volume, envelope position, panning slide, multi-retrig, tremor, extra-fine portamento) are parsed but not simulated — `--verbose`/`list` reports their occurrence counts like any other unimplemented effect.

### ScreamTracker `.s3m` — not started
Recognized by extension, `detect_format` returns a clean "not implemented yet" error rather than misparsing. No parser exists (`formats::` has no `screamtracker.rs`). The tracker-side IR (`formats::base::Module`) was designed to let a new parser plug straight into the existing WAV/MIDI/.als export — that part shouldn't need to change, per FastTracker 2's own parser landing this way.

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
5. An S3M parser, reusing the existing tracker IR/export pipeline unchanged — the same way FastTracker 2's own parser landed.

## Testing

`cargo test` — 155 tests as of this writing, all synthetic/hand-built VGM/MOD/XM byte streams plus a couple of small real fixtures (no copyrighted game audio in the repo; a `fichiers/` directory of real rips used for manual verification is gitignored). `tests/chips_tests.rs` checks each of the five original chip cores against its own register-frequency formula via autocorrelation (no real hardware to compare against) — this is what caught both real libvgm bugs listed above. The ten chips added afterward are checked more lightly in `tests/vgm_tests.rs` (header-clock/presence detection for all ten, plus a synthetic non-silence check for YM2612/GameBoy DMG/NES APU specifically) rather than a full frequency-formula proof per chip. Several bugs in this project's history were only caught by converting a real file and listening — `cargo test` passing is necessary, not sufficient, evidence of correctness for the chiptune paths above.

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

# Vendored subset of libvgm

Source: <https://github.com/ValleyBell/libvgm>, commit `867223e7c33d63de115d1ab955f784c44f19040a`.

Only the files this project's `native/` shims (`shim.c` for individual chip cores,
`player_shim.cpp` for the full VGM player) actually link against are vendored here — not the
whole repository (no CMake build system, no standalone player CLI/GUI apps, no chip cores this
project doesn't use). Each core under `emu/cores/` keeps its own upstream license header; see
`README.md` at the repo root for the licensing consequence of combining GPL-2.0+ cores
(`fmopl.c`, `fmopn.c`, `ymdeltat.c`, `ym2151.c`, `nes_apu.c`) into this project.

| File | Role |
|---|---|
| `stdtype.h`, `common_def.h`, `_stdbool.h` | Base typedefs (`UINT8`/`INT32`/...), `INLINE` macro |
| `emu/EmuStructs.h` | The `DEV_DEF`/`DEV_DECL`/`DEV_INFO` uniform device interface every core implements |
| `emu/SoundDevs.h`, `emu/EmuCores.h` | Device ID / emulator-core-ID constants |
| `emu/snddef.h` | `DEV_SMPL`/`DEV_DATA` base types |
| `emu/EmuHelper.h` | `INIT_DEVINF` and other small inline helpers cores use in their `Start()` |
| `emu/cores/k051649.{c,h}` | K051649/SCC1 core (BSD-3-Clause, Bryan McPhail/MAME) — **locally patched**, see below |
| `emu/panning.{c,h}` | `Panning_Calculate`/`Panning_Centre` helpers `emu2149.c`/`emu2413.c` call into |
| `emu/cores/emutypes.h`, `emu2149_private.h` | emu2149's own private types/state struct |
| `emu/cores/emu2149.{c,h}` | AY-3-8910/YM2149 core (MIT, Mitsutaka Okazaki — same author as `emu2413.c`) |
| `emu/cores/ayintf.{c,h}` | libvgm's own AY8910 front-end (`sndDev_AY8910`) — compiled with `EC_AY8910_EMU2149` defined (see build.rs) so only emu2149 is linked in, not MAME's much larger own `ay8910.c` (not vendored at all — never compiled) |
| `emu/cores/emu2413.{c,h,_private.h}`, `opll_vrc7tone.h` | YM2413/OPLL core (MIT, Mitsutaka Okazaki) — the same reference this project's earlier pure-Rust port was itself ported from |
| `emu/cores/2413intf.{c,h}` | libvgm's own YM2413 front-end (`sndDev_YM2413`) — `EC_YM2413_EMU2413` picks emu2413 over MAME's own `ym2413.c` (also never vendored) |
| `emu/cores/fmopl.{c,h}` | YM3526/YM3812 (OPL/OPL2) core (**GPL-2.0+**, MAME, Jarek Burczynski/Tatsuyuki Satoh) — this project's first non-permissive dependency |
| `emu/cores/oplintf.{c,h}` | libvgm's own YM3526/YM3812 front-end — `EC_YM3812_MAME` picks `fmopl.c` for YM3812 too (its default is AdLibEmu, LGPL-2.1+; picked explicitly here to keep both OPL chips on one license, see build.rs's own comment). YM3526 always uses `fmopl.c` regardless — `oplintf.c` gives it no alternative |
| `emu/cores/fmopn.{c,h}` | YM2203 (OPN), YM2608 (OPNA), YM2610/YM2610B (OPNB) **and** YM2612 (OPN2) core, all in one file (**GPL-2.0+**, MAME/Genesis Plus GX, Jarek Burczynski/Tatsuyuki Satoh) — gated per chip by `SNDDEV_YM2203`/`SNDDEV_YM2608`/`SNDDEV_YM2610`/`SNDDEV_YM2612` (see build.rs); doesn't reimplement each chip's own SSG (AY-3-8910-compatible PSG) sub-unit, calls back into a separately-instantiated `ayintf.c`/`emu2149.c` device via `DEV_DEF::LinkDevice` instead |
| `emu/cores/ymdeltat.{c,h}` | YM2608/YM2610's own ADPCM-B (delta-T) decoder (no explicit license header; same MAME-FM-core authorship as `fmopn.c`/`fmopl.c` above, treated as GPL-2.0+ accordingly) — `fmopn_2608rom.h` (below) is YM2608-only, used from within `fmopn.c` directly, not `ymdeltat.c` |
| `emu/cores/fmopn_2608rom.h` | YM2608's built-in ADPCM-A rhythm-sample ROM table, `#include`d directly inside `fmopn.c` when `SNDDEV_YM2608` is defined |
| `emu/cores/2612intf.{c,h}` | libvgm's own YM2612 front-end (`sndDev_YM2612`) — `EC_YM2612_GPGX` picks `fmopn.c` over the Gens or Nuked YM3438/OPN2 alternatives also on offer (not vendored) |
| `emu/cores/opnintf.{c,h}` | libvgm's own YM2203/YM2608/YM2610 front-end (`sndDev_YM2203`/`sndDev_YM2608`/`sndDev_YM2610`) — also references `ayintf.h` for the linked-SSG `AY8910_CFG` struct/constants (no separate AY8910 core needed, see `fmopn.c`'s own row above) |
| `emu/cores/ym2151.{c,h}` | YM2151 (OPM) core (**GPL-2.0+**, MAME, Jarek Burczynski/Ernesto Corvi) |
| `emu/cores/2151intf.{c,h}` | libvgm's own YM2151 front-end (`sndDev_YM2151`) — `EC_YM2151_MAME` picks `ym2151.c` over the Nuked OPM alternative also on offer (not vendored) |
| `emu/cores/segapcm.{c,h}` | Sega PCM core (BSD-3-Clause, Hiromitsu Shioya/Olivier Galibert) — no `EC_*` alternative, always the only core `SNDDEV_SEGAPCM` links in. Sampled-PCM channels driven by VGM data blocks (0xC0 writes), not register-value pairs like every FM/PSG chip above |
| `emu/cores/rf5c68.{c,h}` | RF5C68/RF5C164 core (BSD-3-Clause, Olivier Galibert/Aaron Giles) — one `DEV_DECL`/`DEVID_RF5C68` shared by both chips, told apart by a config flag (`rf5cintf.c`'s own `DeviceName`), not a separate device ID |
| `emu/cores/rf5cintf.{c,h}` | libvgm's own RF5C68/RF5C164 front-end — `EC_RF5C68_MAME` picks `rf5c68.c` over the Gens/GS RF5C164 alternative also on offer (not vendored) |
| `emu/cores/gb_mame.{c,h}` | GameBoy DMG APU core (BSD-3-Clause, Wilbert Pol/Anthony Kruize) — registers are addressed by NRxx *index* (`NR10`=0x00 ... `NR52`=0x16, see its own `#define` block), not GB memory-map address, a different convention from every FM/PSG chip above |
| `emu/cores/gbintf.{c,h}` | libvgm's own GameBoy DMG front-end (`sndDev_GB_DMG`) — `EC_GB_MAME` picks `gb_mame.c` over the SameBoy alternative also on offer (not vendored) |
| `emu/cores/nes_apu.{c,h}`, `nes_defs.h` | NES APU core (**GPL-2.0+**, Matthew Conte) — FDS expansion-audio support intentionally not linked (`EC_NES_NSFP_FDS` left undefined, see build.rs) |
| `emu/cores/nesintf.{c,h}` | libvgm's own NES APU front-end (`sndDev_NES_APU`) — `EC_NES_MAME` picks `nes_apu.c` over the NSFPlay alternative also on offer (not vendored) |
| `emu/SoundEmu.{c,h}` | `SndEmu_GetDevDecl`/`SndEmu_Start2` — the device lookup/instantiation libvgm's player (below) calls into. Its own `sndEmu_Devices[]` global registry is `#ifdef SNDDEV_*`-gated per chip (see build.rs) — no patch needed to scope it down to just the chips this project links |
| `emu/logging.{c,h}`, `emu/RatioCntr.h` | Small logging/rate-counter helpers several cores and the player use |
| `emu/dac_control.{c,h}` | DAC control-stream command support (`player/vgmplayer_cmdhandler.cpp` references its types even for chips this project doesn't link) |
| `emu/Resampler.{c,h}` | `WAVE_32BS`, the player's own output sample type, plus its internal resampling used for chips whose native rate the player itself needs to convert |
| `player/playerbase.{cpp,hpp}` | `PlayerBase` — the abstract player interface `VGMPlayer` implements (`PLR_MUTE_OPTS`/`PLR_DEV_INFO`/... live here) |
| `player/vgmplayer.{cpp,hpp}` | `VGMPlayer` — this project's actual VGM/VGZ audio rendering engine (`chips::player_ffi`/`native/player_shim.cpp` drive it directly) |
| `player/vgmplayer_cmdhandler.cpp` | `VGMPlayer`'s VGM command-byte dispatch table (split into its own file upstream) |
| `player/dblk_compr.{c,h}`, `player/helper.{c,h}` | Data-block decompression and small shared helpers the player uses |
| `utils/DataLoader.{c,h}`, `utils/MemoryLoader.{c,h}` | Abstract byte-source interface + an in-memory implementation — `native/player_shim.cpp` copies the Rust-provided `&[u8]` into one of these rather than handing the player a raw pointer with an unclear lifetime |
| `utils/StrUtils.h`, `utils/StrUtils-CPConv_IConv.c` | GD3 tag UTF-16→UTF-8 conversion via `iconv` (POSIX; a different file would be needed for a Windows build) — `VGMPlayer::LoadFile` calls this unconditionally even though this project never reads the result back out (see `formats::vgm.rs`'s own GD3 parsing, used instead) |

## Local patches

**`emu/cores/k051649.c`, `k051649_update`'s `step` calculation**: the upstream formula divides
by an extra `2.0f` that makes every channel play exactly one octave sharp (`freq_hz =
mclock/(16*(freq+1))` instead of the correct, independently-verified `mclock/(32*(freq+1))` —
see the inline comment at the fix site, and this project's own README.md/formats::vgm.rs for
the prior, unrelated K051649-clock bug this project already found and fixed once before).
Caught immediately by `tests/chips_tests.rs`'s SCC frequency tests after the initial FFI
migration — not a hypothetical, a real regression this project's own existing tests caught
against a formula already cross-verified against MAME, openMSX, and a real-file measurement.

Do not hand-edit vendored files beyond patches recorded here (and mirrored as inline comments
at the exact fix site) — re-vendor from upstream otherwise, so this stays diffable against the
real source. If a future re-vendor picks up an upstream fix for the same bug, re-apply/remove
this patch accordingly rather than silently stacking a redundant second correction.

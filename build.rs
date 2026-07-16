// Compiles the vendored libvgm subset (vendor/libvgm/, see its own README.md) this project's
// chips::*/chips::player_ffi FFI boundary links against. This is also where this project's
// licensing stops being uniformly permissive: fmopl.c (YM3526/YM3812) is GPL-2.0+, unlike
// every other vendored core here — see this repo's own LICENSE and README.md.
fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=vendor/libvgm");

    // Everything genuinely C — chip DSP cores plus libvgm's own C-language utility/glue code
    // the C++ player links against. Kept as plain C (not folded into the C++ build below):
    // several of these files (k051649.c in particular) initialize a generic `void* funcPtr`
    // struct field directly from a concretely-typed function pointer, which C allows
    // implicitly but C++ rejects outright — compiling them as C sidesteps that entirely rather
    // than patching every such initializer in vendored source.
    cc::Build::new()
        .include(".")
        .file("native/shim.c")
        .file("vendor/libvgm/emu/cores/k051649.c")
        // AY8910/YM2149: emu2149 (Mitsutaka Okazaki, same author as YM2413's emu2413) rather
        // than MAME's own ay8910.c — libvgm's ayintf.c compiles in whichever EC_AY8910_* cores
        // are #defined, so EC_AY8910_MAME is deliberately left undefined to avoid needing to
        // vendor+compile the much larger MAME core we don't use.
        .define("SNDDEV_SELECT", None)
        .define("EC_AY8910_EMU2149", None)
        .file("vendor/libvgm/emu/cores/emu2149.c")
        .file("vendor/libvgm/emu/cores/ayintf.c")
        .file("vendor/libvgm/emu/panning.c")
        // YM2413: emu2413 (Mitsutaka Okazaki) — the same reference this project's earlier
        // pure-Rust port was itself ported from, and libvgm's own 2413intf.c picks it as the
        // default core over MAME's ("it's better than MAME", per its own comment). Also
        // requires EC_YM2413_* since SNDDEV_SELECT is already defined above for AY8910's sake.
        .define("EC_YM2413_EMU2413", None)
        .file("vendor/libvgm/emu/cores/emu2413.c")
        .file("vendor/libvgm/emu/cores/2413intf.c")
        // YM3526 (OPL) and YM3812 (OPL2): MAME's own fmopl.c — GPL-2.0+, this project's first
        // non-permissive dependency (see README.md's licensing note and this file's own
        // opening comment). YM3526 always uses this core regardless of EC_YM3812_* (oplintf.c
        // gives it no alternative); YM3812 does have alternatives (AdLibEmu/Nuked, both
        // LGPL-2.1+) but EC_YM3812_MAME is picked explicitly here to keep both chips on the
        // same license rather than mixing GPL and LGPL sources for no real benefit.
        .define("EC_YM3812_MAME", None)
        .file("vendor/libvgm/emu/cores/fmopl.c")
        .file("vendor/libvgm/emu/cores/oplintf.c")
        // YM2612 (Sega Genesis/Mega Drive OPN2) and the OPN family (YM2203/YM2608/YM2610,
        // PC-88/98 + Neo Geo) all share one MAME-derived core, fmopn.c — libvgm's own
        // 2612intf.c picks it for YM2612 via EC_YM2612_GPGX (named for Genesis Plus GX, the
        // project that maintains this particular fork of MAME's OPN core) rather than the
        // Gens or Nuked alternatives also on offer, so only one core file is needed for all
        // four chips. Each chip's own SSG (AY-3-8910-compatible PSG) sub-unit is *not*
        // reimplemented inside fmopn.c — it calls back into a separately-instantiated AY8910
        // device via libvgm's own DEV_DEF::LinkDevice mechanism (see opnintf.c's
        // DeviceLinkIDs_OPN/get_ssg_funcs), reusing the emu2149 core already compiled above
        // rather than needing a second AY8910 implementation.
        .define("SNDDEV_YM2612", None)
        .define("SNDDEV_YM2203", None)
        .define("SNDDEV_YM2608", None)
        .define("SNDDEV_YM2610", None)
        .define("EC_YM2612_GPGX", None)
        .file("vendor/libvgm/emu/cores/fmopn.c")
        .file("vendor/libvgm/emu/cores/ymdeltat.c") // YM2608/YM2610's own ADPCM-B decoder
        .file("vendor/libvgm/emu/cores/2612intf.c")
        .file("vendor/libvgm/emu/cores/opnintf.c")
        // YM2151 (OPM) — common in arcade (Capcom/Sega) and X68000 rips. MAME's own ym2151.c,
        // the default/only alternative to Nuked OPM (not vendored — no particular reason to
        // prefer it now that this project is already GPL-2.0-or-later, see fmopl.c's own note).
        .define("EC_YM2151_MAME", None)
        .file("vendor/libvgm/emu/cores/ym2151.c")
        .file("vendor/libvgm/emu/cores/2151intf.c")
        // Sega PCM — sampled-PCM channels driven by VGM data blocks (0xC0 writes + 0x67 data
        // blocks), not register-value pairs like the FM/PSG chips above. libvgm's own player
        // (player/vgmplayer_cmdhandler.cpp, already linked below) already implements the
        // generic data-block loading/dispatch every chip needing sample ROM shares — no
        // project-specific plumbing needed here beyond linking the core itself.
        .file("vendor/libvgm/emu/cores/segapcm.c")
        // RF5C68 and RF5C164 (sampled-PCM channels, Sega CD/arcade and Sega Saturn
        // respectively) share one MAME core and DEV_DECL (rf5cintf.c's own DeviceName
        // distinguishes them via a config flag) — same data-block mechanism as Sega PCM above.
        .define("EC_RF5C68_MAME", None)
        .file("vendor/libvgm/emu/cores/rf5c68.c")
        .file("vendor/libvgm/emu/cores/rf5cintf.c")
        // GameBoy DMG — MAME's own core (gb_mame.c) over the SameBoy alternative also on
        // offer, consistent with this project's general preference for the smaller/simpler
        // core when there's no correctness reason to prefer the other (see README.md).
        .define("SNDDEV_GAMEBOY", None)
        .define("EC_GB_MAME", None)
        .file("vendor/libvgm/emu/cores/gb_mame.c")
        .file("vendor/libvgm/emu/cores/gbintf.c")
        // NES APU — MAME's own core (nes_apu.c) over the NSFPlay alternative; NSFPlay's own
        // FDS (Famicom Disk System) expansion-audio support is skipped (EC_NES_NSFP_FDS left
        // undefined) since it needs its own separate command-byte handling this project
        // doesn't otherwise support, and real NES rips using it are rare.
        .define("SNDDEV_NES_APU", None)
        .define("EC_NES_MAME", None)
        .file("vendor/libvgm/emu/cores/nes_apu.c")
        .file("vendor/libvgm/emu/cores/nesintf.c")
        // libvgm's own C-language player support code — SoundEmu.c's SNDDEV_* defines scope
        // its device registry down to just the chips above, avoiding undefined-symbol errors
        // for the ~30 other chip cores libvgm supports but this project doesn't vendor.
        .define("SNDDEV_K051649", None)
        .define("SNDDEV_AY8910", None)
        .define("SNDDEV_YM2413", None)
        .define("SNDDEV_YM3526", None)
        .define("SNDDEV_YM3812", None)
        .define("SNDDEV_YM2151", None)
        .define("SNDDEV_SEGAPCM", None)
        .define("SNDDEV_RF5C68", None)
        .file("vendor/libvgm/emu/SoundEmu.c")
        .file("vendor/libvgm/emu/logging.c")
        .file("vendor/libvgm/emu/dac_control.c")
        .file("vendor/libvgm/emu/Resampler.c")
        .file("vendor/libvgm/player/dblk_compr.c")
        .file("vendor/libvgm/player/helper.c")
        .file("vendor/libvgm/utils/DataLoader.c")
        .file("vendor/libvgm/utils/MemoryLoader.c")
        .file("vendor/libvgm/utils/StrUtils-CPConv_IConv.c")
        .warnings(false) // vendored upstream source, not ours to keep warning-clean
        .compile("ablemod_native");

    // libvgm's own VGM player (native/player_shim.cpp) — this project's actual VGM/VGZ audio
    // rendering path (export::vgm_render), replacing the earlier hand-rolled "walk our own
    // RegisterWrite log and drive chips:: directly" loop. formats::vgm.rs's own parser is kept
    // for header/GD3-tag metadata and the raw write log export::vgm_wavetable/vgm_operator
    // still need — this player is a second, independent parse of the same file, used only for
    // rendering. Genuinely C++ (VGMPlayer is a C++ class) — everything else it needs is C,
    // already compiled above and linked in from that static lib.
    cc::Build::new()
        .cpp(true)
        .std("c++11")
        .include(".")
        .file("native/player_shim.cpp")
        .file("vendor/libvgm/player/vgmplayer.cpp")
        .file("vendor/libvgm/player/vgmplayer_cmdhandler.cpp")
        .file("vendor/libvgm/player/playerbase.cpp")
        .warnings(false)
        .compile("ablemod_player");

    println!("cargo:rustc-link-lib=iconv");
    println!("cargo:rustc-link-lib=z");
}

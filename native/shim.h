#ifndef ABLEMOD_SHIM_H
#define ABLEMOD_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Keep in sync with src/chips/ffi.rs's AblemodChipKind. One entry per libvgm DEV_DECL this
 * project links against — see native/shim.c's dispatch table. */
enum {
    ABLEMOD_CHIP_SCC = 0,
    ABLEMOD_CHIP_AY8910 = 1,
    ABLEMOD_CHIP_YM2413 = 2,
};

/* Instantiates one chip core (libvgm's DEV_DEF::Start), returning an opaque handle — NULL on
 * failure. `clock` is the chip's own input clock in Hz; libvgm's own cores derive their native
 * synthesis rate from it (see ablemod_chip_native_rate), not from an externally requested
 * output rate — this project's existing chips::rateconv::RateConv still does the final
 * downsample to 44100Hz on the Rust side, same as it already does for the pure-Rust ports.
 * `variant_flags` is chip-specific and 0 for chips that don't need it — for
 * ABLEMOD_CHIP_AY8910 specifically, bit 0 selects the YM2149-compatible DAC curve over plain
 * AY-3-8910 (mirrors this project's own pre-existing Ayumi::new(is_ym, ...) convention).
 * ABLEMOD_CHIP_YM2413 also has a bit 0 (selects VRC7 instrument ROM instead of real YM2413's),
 * always passed as 0 here — this project only ever targets genuine YM2413 chip data. */
void* ablemod_chip_create(int kind, uint32_t clock, uint8_t variant_flags);

/* Destroys a handle from ablemod_chip_create (libvgm's DEV_DEF::Stop). */
void ablemod_chip_destroy(void* chip, int kind);

/* Direct passthrough to the chip's own DEVRW_A8D8 register-write function (DEVFUNC_WRITE_A8D8
 * in its rwFuncs table) — a raw (addr, data) pair in whatever calling convention that specific
 * chip core's own write8 expects. Each Rust-side chip wrapper is responsible for translating
 * this project's own VGM-derived (port, reg, value) writes into that convention (see
 * chips::scc's own comment on the K051649 select-register/write-data protocol). */
void ablemod_chip_write8(void* chip, int kind, uint8_t addr, uint8_t data);

/* The chip's own native sample rate (libvgm's DEV_INFO::sampleRate, set during Start) — most
 * cores derive this from `clock` (e.g. K051649: clock/16), not a caller-requested rate. */
uint32_t ablemod_chip_native_rate(void* chip, int kind);

/* Passthrough to DEV_DEF::SetMuteMask — bit i set mutes channel i, matching this project's own
 * existing Scc::set_mask/Ayumi::solo/Opll::set_mask convention exactly (confirmed against
 * k051649.c: DEVFUNC_OPTMASK SetMuteMask is a first-class, always-populated part of the
 * uniform DEV_DEF interface, not something needing a custom patch). */
void ablemod_chip_set_mute_mask(void* chip, int kind, uint32_t mask);

/* Pulls exactly one sample (left channel only — every chip this project drives through this
 * shim is treated as mono, matching how e.g. k051649_update writes the identical value to both
 * output buffers) at the chip's own native rate (ablemod_chip_native_rate), via DEV_DEF::Update
 * called with samples=1. One FFI call per output sample is not the cheapest possible interface,
 * but it lets every Rust-side chip wrapper keep its exact existing calc()-style "pull one
 * sample" public method signature unchanged, so nothing downstream of chips::* needs to change. */
int32_t ablemod_chip_calc(void* chip, int kind);

#ifdef __cplusplus
}
#endif

#endif /* ABLEMOD_SHIM_H */

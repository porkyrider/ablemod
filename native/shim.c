#include <stdlib.h>

#include "shim.h"
#include "../vendor/libvgm/stdtype.h"
#include "../vendor/libvgm/emu/EmuStructs.h"

extern const DEV_DECL sndDev_K051649;
extern const DEV_DECL sndDev_AY8910;
extern const DEV_DECL sndDev_YM2413;

/* AY8910_CFG (ayintf.h) extends DEV_GEN_CFG with chipType/chipFlags — duplicated here rather
 * than included, since ayintf.h pulls in AY8910_ZX_STEREO/etc. this shim doesn't need. Layout
 * must stay byte-for-byte identical to ayintf.h's own struct — both are plain C structs with
 * no padding-sensitive types, so this is safe as long as the two are kept in sync by hand. */
typedef struct {
    DEV_GEN_CFG genCfg;
    UINT8 chipType;
    UINT8 chipFlags;
} AblemodAy8910Cfg;

typedef struct {
    DEV_INFO devInf;
    const DEV_DEF* devDef;
    DEVFUNC_WRITE_A8D8 write8;
} AblemodChip;

static const DEV_DECL* decl_for_kind(int kind) {
    switch (kind) {
        case ABLEMOD_CHIP_SCC:
            return &sndDev_K051649;
        case ABLEMOD_CHIP_AY8910:
            return &sndDev_AY8910;
        case ABLEMOD_CHIP_YM2413:
            return &sndDev_YM2413;
        default:
            return NULL;
    }
}

/* Scans a DEV_DEF's rwFuncs table for its DEVRW_A8D8 register-write entry. Some cores (e.g.
 * emu2149) expose *two* register-write variants: a plain one emulating the chip's real 2-step
 * hardware bus protocol, and a "quick" one taking an already-decoded (reg, value) pair — VGM
 * files (and this shim's own callers) always deal in already-decoded pairs, matching the quick
 * variant, so RWF_QUICKWRITE is preferred when both exist (bit 0 clear on either flavor's
 * funcType marks "this is some kind of write", matching EmuStructs.h's own RWF_WRITE=0x00/
 * RWF_QUICKWRITE=(0x02|RWF_WRITE) encoding). */
static DEVFUNC_WRITE_A8D8 find_write8(const DEV_DEF* devDef) {
    const DEVDEF_RWFUNC* rw = devDef->rwFuncs;
    if (rw == NULL) {
        return NULL;
    }
    DEVFUNC_WRITE_A8D8 fallback = NULL;
    while (rw->funcPtr != NULL) {
        if (rw->rwType == DEVRW_A8D8 && (rw->funcType & RWF_READ) == 0 && (rw->funcType & RWF_MEMORY) == 0) {
            if (rw->funcType & 0x02) { /* RWF_QUICKWRITE */
                return (DEVFUNC_WRITE_A8D8)rw->funcPtr;
            }
            fallback = (DEVFUNC_WRITE_A8D8)rw->funcPtr;
        }
        rw++;
    }
    return fallback;
}

void* ablemod_chip_create(int kind, uint32_t clock, uint8_t variant_flags) {
    const DEV_DECL* decl = decl_for_kind(kind);
    if (decl == NULL || decl->cores[0] == NULL) {
        return NULL;
    }
    const DEV_DEF* devDef = decl->cores[0];

    AblemodChip* handle = (AblemodChip*)calloc(1, sizeof(AblemodChip));
    if (handle == NULL) {
        return NULL;
    }

    UINT8 startResult;
    if (kind == ABLEMOD_CHIP_AY8910) {
        AblemodAy8910Cfg cfg = {0};
        cfg.genCfg.srMode = DEVRI_SRMODE_NATIVE;
        cfg.genCfg.clock = clock;
        cfg.chipType = (variant_flags & 0x01) ? 0x10 /* AYTYPE_YM2149 */ : 0x00 /* AYTYPE_AY8910 */;
        startResult = devDef->Start((const DEV_GEN_CFG*)&cfg, &handle->devInf);
    } else {
        DEV_GEN_CFG cfg = {0};
        cfg.srMode = DEVRI_SRMODE_NATIVE;
        cfg.clock = clock;
        cfg.flags = variant_flags; /* e.g. YM2413's VRC7-mode bit, always 0 in this project */
        startResult = devDef->Start(&cfg, &handle->devInf);
    }
    if (startResult != 0x00) {
        free(handle);
        return NULL;
    }
    handle->devDef = devDef;
    handle->write8 = find_write8(devDef);
    /* Start() alone is not always a fully initialized, silent chip — e.g. emu2413.c's own
     * EOPLL_new() explicitly skips calling EOPLL_reset() (left commented out in its source),
     * relying on the caller to call Reset() separately afterward, same as any real VGM player
     * does. Without this, YM2413 notes never sounded at all (root-caused directly: adding this
     * call turned a completely silent test tone into one measuring within 0.01% of its
     * expected frequency). Calling it here unconditionally, for every chip, is the same fix a
     * correct player would apply regardless of whether a given core's own Start() happens to
     * already leave it in a playable state. */
    if (devDef->Reset != NULL) {
        devDef->Reset(handle->devInf.dataPtr);
    }
    return handle;
}

void ablemod_chip_destroy(void* chip, int kind) {
    (void)kind;
    if (chip == NULL) {
        return;
    }
    AblemodChip* handle = (AblemodChip*)chip;
    if (handle->devDef->Stop != NULL) {
        handle->devDef->Stop(handle->devInf.dataPtr);
    }
    free(handle);
}

void ablemod_chip_write8(void* chip, int kind, uint8_t addr, uint8_t data) {
    (void)kind;
    AblemodChip* handle = (AblemodChip*)chip;
    if (handle->write8 != NULL) {
        handle->write8(handle->devInf.dataPtr, addr, data);
    }
}

uint32_t ablemod_chip_native_rate(void* chip, int kind) {
    (void)kind;
    AblemodChip* handle = (AblemodChip*)chip;
    return handle->devInf.sampleRate;
}

void ablemod_chip_set_mute_mask(void* chip, int kind, uint32_t mask) {
    (void)kind;
    AblemodChip* handle = (AblemodChip*)chip;
    if (handle->devDef->SetMuteMask != NULL) {
        handle->devDef->SetMuteMask(handle->devInf.dataPtr, mask);
    }
}

int32_t ablemod_chip_calc(void* chip, int kind) {
    (void)kind;
    AblemodChip* handle = (AblemodChip*)chip;
    DEV_SMPL left = 0;
    DEV_SMPL right = 0;
    DEV_SMPL* outputs[2] = { &left, &right };
    handle->devDef->Update(handle->devInf.dataPtr, 1, outputs);
    return (int32_t)left;
}

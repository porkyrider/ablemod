#ifndef ABLEMOD_PLAYER_SHIM_H
#define ABLEMOD_PLAYER_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    int32_t l;
    int32_t r;
} AblemodWave32;

/* Loads a VGM file (already gunzipped — this project's own formats::vgm.rs already handles
 * VGZ decompression, so the player is only ever handed raw VGM bytes, sidestepping any
 * question of whether libvgm's own gzip-aware MemoryLoader path is exercised or trustworthy).
 * Returns an opaque VGMPlayer handle, or NULL on failure (not a valid VGM file, or every chip
 * it declares failed to start). Rendering always starts at sample 0 — there is no persistent
 * playback position to seek; export::vgm_render creates one handle per render pass (matching
 * how it already created a fresh chip instance per stem before this migration), since VGM
 * files are small and reloading is cheap. */
void* ablemod_player_load(const uint8_t* data, uint32_t len, uint32_t output_rate);

void ablemod_player_free(void* player);

/* Mutes/unmutes a channel range on one chip instance — `dev_id` is a DEVID_ constant from
 * vendor/libvgm/emu/SoundDevs.h (this shim doesn't redefine them; callers already have the
 * exact values needed hardcoded, same as this project's own formats::vgm::Chip does for its
 * own, unrelated purposes). `chn_mute_mask` bit i mutes channel i, matching the exact
 * convention already used throughout chips::scc/ay8910/ym2413's own set_mask methods (both
 * ultimately reach the same DEV_DEF::SetMuteMask this shim calls). */
void ablemod_player_set_mute(void* player, uint32_t dev_id, uint32_t instance, uint32_t chn_mute_mask);

/* Fills `out` with exactly `count` stereo samples at the player's configured output rate,
 * starting from wherever the last render call left off (sample 0 on a freshly loaded handle).
 * Returns the number of samples actually rendered (less than `count` only once the player has
 * reached the natural end of the file's declared total-sample count). */
uint32_t ablemod_player_render(void* player, uint32_t count, AblemodWave32* out);

#ifdef __cplusplus
}
#endif

#endif /* ABLEMOD_PLAYER_SHIM_H */

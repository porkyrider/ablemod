#include <cstdlib>
#include <cstring>
#include <vector>

#include "player_shim.hpp"
#include "../vendor/libvgm/player/vgmplayer.hpp"
#include "../vendor/libvgm/utils/MemoryLoader.h"

namespace {

struct AblemodPlayer {
    std::vector<uint8_t> fileData; // owns a copy — MemoryLoader only ever references the
                                    // pointer it's given, so this must outlive the DATA_LOADER
                                    // and the VGMPlayer for as long as either is in use.
    DATA_LOADER* loader = nullptr;
    VGMPlayer* player = nullptr;

    ~AblemodPlayer() {
        if (player != nullptr) {
            player->Stop();
            player->UnloadFile();
            delete player;
        }
        if (loader != nullptr) {
            DataLoader_Deinit(loader);
        }
    }
};

} // namespace

extern "C" {

void* ablemod_player_load(const uint8_t* data, uint32_t len, uint32_t output_rate) {
    AblemodPlayer* handle = new AblemodPlayer();
    handle->fileData.assign(data, data + len);

    handle->loader = MemoryLoader_Init(handle->fileData.data(), static_cast<UINT32>(handle->fileData.size()));
    if (handle->loader == nullptr || DataLoader_Load(handle->loader) != 0x00) {
        delete handle;
        return nullptr;
    }

    handle->player = new VGMPlayer();
    handle->player->SetSampleRate(output_rate);
    if (handle->player->LoadFile(handle->loader) != 0x00) {
        delete handle;
        return nullptr;
    }
    if (handle->player->Start() != 0x00) {
        delete handle;
        return nullptr;
    }
    return handle;
}

void ablemod_player_free(void* player) {
    if (player != nullptr) {
        delete static_cast<AblemodPlayer*>(player);
    }
}

void ablemod_player_set_mute(void* player, uint32_t dev_id, uint32_t instance, uint32_t chn_mute_mask) {
    AblemodPlayer* handle = static_cast<AblemodPlayer*>(player);
    PLR_MUTE_OPTS muteOpts;
    std::memset(&muteOpts, 0, sizeof(muteOpts));
    muteOpts.chnMute[0] = chn_mute_mask;
    handle->player->SetDeviceMuting(PLR_DEV_ID(dev_id, instance), muteOpts);
}

uint32_t ablemod_player_render(void* player, uint32_t count, AblemodWave32* out) {
    AblemodPlayer* handle = static_cast<AblemodPlayer*>(player);
    std::vector<WAVE_32BS> buf(count);
    UINT32 rendered = handle->player->Render(count, buf.data());
    for (UINT32 i = 0; i < rendered; i++) {
        out[i].l = buf[i].L;
        out[i].r = buf[i].R;
    }
    return rendered;
}

} // extern "C"

//! Thin, hand-written bindings to native/player_shim.cpp — a small C++ wrapper around
//! libvgm's own `VGMPlayer` (vendor/libvgm/player/vgmplayer.cpp), this project's actual
//! VGM/VGZ audio rendering path. See build.rs's own comment on why this is a second,
//! independent parse of the same file rather than reusing formats::vgm::VgmFile::writes.

use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Wave32 {
    pub(crate) l: i32,
    pub(crate) r: i32,
}

unsafe extern "C" {
    fn ablemod_player_load(data: *const u8, len: u32, output_rate: u32) -> *mut c_void;
    fn ablemod_player_free(player: *mut c_void);
    fn ablemod_player_set_mute(player: *mut c_void, dev_id: u32, instance: u32, chn_mute_mask: u32);
    fn ablemod_player_render(player: *mut c_void, count: u32, out: *mut Wave32) -> u32;
}

/// DEVID_ constants from vendor/libvgm/emu/SoundDevs.h — this project only ever links the
/// cores for these thirteen (see build.rs's own SNDDEV_* defines).
pub(crate) mod dev_id {
    pub(crate) const YM2413: u32 = 0x01;
    pub(crate) const YM2612: u32 = 0x02;
    pub(crate) const YM2151: u32 = 0x03;
    pub(crate) const SEGAPCM: u32 = 0x04;
    pub(crate) const RF5C68: u32 = 0x05; // also used for RF5C164 (same DEVID, see rf5cintf.c)
    pub(crate) const YM2203: u32 = 0x06;
    pub(crate) const YM2608: u32 = 0x07;
    pub(crate) const YM2610: u32 = 0x08;
    pub(crate) const YM3812: u32 = 0x09;
    pub(crate) const YM3526: u32 = 0x0A;
    pub(crate) const AY8910: u32 = 0x12;
    pub(crate) const GB_DMG: u32 = 0x13;
    pub(crate) const NES_APU: u32 = 0x14;
    pub(crate) const K051649: u32 = 0x19;
}

/// One loaded VGM file, ready to render — owns its own copy of the file bytes and a fresh
/// libvgm player instance. There is no seek/rewind: export::vgm_render creates a new `Player`
/// per render pass (matching how it already created fresh chip instances per stem before this
/// migration), since VGM files are small and reloading is cheap.
pub(crate) struct Player {
    handle: *mut c_void,
}

impl Player {
    /// `None` if libvgm rejected the file (not a valid VGM, or every chip it declares failed
    /// to start) — this project's own formats::vgm::parse already validated the file once
    /// before this is ever called, so a `None` here would only happen on a case that parser
    /// accepts but libvgm's independent, stricter parser doesn't.
    pub(crate) fn load(data: &[u8], output_rate: u32) -> Option<Self> {
        let handle = unsafe { ablemod_player_load(data.as_ptr(), data.len() as u32, output_rate) };
        if handle.is_null() { None } else { Some(Player { handle }) }
    }

    /// Mutes/unmutes channels on one chip instance (`instance` is almost always 0 — a second
    /// instance only exists for files using two of the same chip) — bit i of `chn_mute_mask`
    /// mutes channel i, the same convention chips::scc/ay8910/ym2413's own set_mask already use.
    pub(crate) fn set_mute(&mut self, dev_id: u32, instance: u32, chn_mute_mask: u32) {
        unsafe { ablemod_player_set_mute(self.handle, dev_id, instance, chn_mute_mask) }
    }

    /// Renders up to `count` samples starting from wherever the last call left off (sample 0
    /// on a freshly loaded handle) — returns fewer than `count` only once the player reaches
    /// the file's declared total-sample count.
    pub(crate) fn render(&mut self, count: u32) -> Vec<Wave32> {
        let mut buf = vec![Wave32 { l: 0, r: 0 }; count as usize];
        let rendered = unsafe { ablemod_player_render(self.handle, count, buf.as_mut_ptr()) };
        buf.truncate(rendered as usize);
        buf
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { ablemod_player_free(self.handle) }
    }
}

unsafe impl Send for Player {}


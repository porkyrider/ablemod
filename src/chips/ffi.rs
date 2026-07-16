//! Thin, hand-written bindings to native/shim.c — the small C shim wrapping libvgm's uniform
//! `DEV_DEF` sound-chip-core interface (see vendor/libvgm/README.md) into a flat, stable API.
//! Deliberately not bindgen-generated: libvgm's own internal structs (DEV_INFO, DEV_DEF, ...)
//! are never represented in Rust at all, only opaque `*mut c_void` handles cross the FFI
//! boundary — the shim is the only code on either side that needs to agree on their real
//! layout, and the C compiler enforces that, not a hand-copied Rust struct definition that
//! could silently drift out of sync with a future libvgm header change.

use std::ffi::c_void;

/// Keep in sync with native/shim.h's `enum { ABLEMOD_CHIP_* }`.
#[repr(i32)]
#[derive(Clone, Copy)]
pub(crate) enum ChipKind {
    Scc = 0,
    Ay8910 = 1,
    Ym2413 = 2,
}

unsafe extern "C" {
    fn ablemod_chip_create(kind: i32, clock: u32, variant_flags: u8) -> *mut c_void;
    fn ablemod_chip_destroy(chip: *mut c_void, kind: i32);
    fn ablemod_chip_write8(chip: *mut c_void, kind: i32, addr: u8, data: u8);
    fn ablemod_chip_native_rate(chip: *mut c_void, kind: i32) -> u32;
    fn ablemod_chip_set_mute_mask(chip: *mut c_void, kind: i32, mask: u32);
    fn ablemod_chip_calc(chip: *mut c_void, kind: i32) -> i32;
}

/// Safe wrapper around one `ablemod_chip_create`/`ablemod_chip_destroy` pair — every method
/// here is a direct, allocation-free passthrough to the shim. `chips::scc`/`ay8910`/`ym2413`
/// each hold one of these behind their own existing public API (new/write/calc/...), so this
/// type itself is never exposed outside `chips::`.
pub(crate) struct NativeChip {
    handle: *mut c_void,
    kind: ChipKind,
}

impl NativeChip {
    /// Panics if the underlying libvgm core failed to start — this only happens for a
    /// programmer error (unknown ChipKind, or the core rejecting a clock of 0), never from
    /// untrusted input, so a panic here is more honest than threading a Result through every
    /// chips::* constructor for a case that isn't actually reachable from a VGM file's own data.
    /// `variant_flags` is chip-specific and ignored by chips that don't need it — see
    /// native/shim.h's own doc comment on ablemod_chip_create.
    pub(crate) fn new(kind: ChipKind, clock: u32, variant_flags: u8) -> Self {
        let handle = unsafe { ablemod_chip_create(kind as i32, clock, variant_flags) };
        assert!(!handle.is_null(), "native chip core failed to start (kind={}, clock={clock})", kind as i32);
        NativeChip { handle, kind }
    }

    pub(crate) fn write8(&mut self, addr: u8, data: u8) {
        unsafe { ablemod_chip_write8(self.handle, self.kind as i32, addr, data) }
    }

    pub(crate) fn native_rate(&self) -> u32 {
        unsafe { ablemod_chip_native_rate(self.handle, self.kind as i32) }
    }

    pub(crate) fn set_mute_mask(&mut self, mask: u32) {
        unsafe { ablemod_chip_set_mute_mask(self.handle, self.kind as i32, mask) }
    }

    pub(crate) fn calc(&mut self) -> i32 {
        unsafe { ablemod_chip_calc(self.handle, self.kind as i32) }
    }
}

impl Drop for NativeChip {
    fn drop(&mut self) {
        unsafe { ablemod_chip_destroy(self.handle, self.kind as i32) }
    }
}

// NativeChip owns its libvgm handle exclusively (no sharing, no aliasing) and every call goes
// through &mut self except native_rate's &self read — safe to move/access from a single thread
// at a time, which is all this project ever does (one Scc/Ay8910/Opll instance per render pass).
unsafe impl Send for NativeChip {}

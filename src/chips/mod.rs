//! Sound chip emulators used to render VGM/VGZ files to audio — see formats::vgm and
//! export::vgm_render. All three (`scc`/`ay8910`/`ym2413`) are thin Rust wrappers around
//! libvgm (<https://github.com/ValleyBell/libvgm>, vendored subset under vendor/libvgm/) cores,
//! driven through the native/shim.c + ffi.rs FFI boundary, rather than hand-ported pure Rust —
//! see vendor/libvgm/README.md and README.md's own licensing note (some libvgm cores, e.g.
//! YM3526/YM3812's, are GPL-2.0+, not the permissive MIT/BSD-3 this project's earlier pure-Rust
//! ports used).

pub mod ay8910;
pub(crate) mod ffi;
pub(crate) mod player_ffi;
pub(crate) mod rateconv;
pub mod scc;
pub mod ym2413;

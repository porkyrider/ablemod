//! Sound chip emulators used to render VGM/VGZ files to audio — see formats::vgm and
//! export::vgm_render. Each emulator here is a faithful Rust port of a well-regarded,
//! permissively-licensed reference implementation (credited in each module), not a
//! from-scratch reimplementation, since getting cycle/register-accurate chip behavior right
//! from a datasheet alone is its own multi-week undertaking per chip.

pub mod ay8910;
pub(crate) mod rateconv;
pub mod scc;
pub mod ym2413;

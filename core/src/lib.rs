//! araware-core: RAW development + library engine.
//!
//! decode (LibRaw) -> develop pipeline (CPU reference, wgpu for speed)
//! -> catalog (folder scan + SQLite index + JSON sidecars) -> C FFI.

pub mod ai;
pub mod auto;
pub mod capi;
pub mod catalog;
pub mod decode;
pub mod develop;
pub mod engine;
pub mod ffi;
pub mod gpu;
pub mod lensdb;
pub mod lut;
pub mod nef_he;
pub mod recipe;
pub mod x3f;
pub mod xmp;

pub use develop::{histogram, RgbaImage};
pub use engine::Engine;
pub use recipe::{Recipe, Sidecar, WbMode};

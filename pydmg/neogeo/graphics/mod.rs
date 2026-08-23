//! Subsistema gráfico Neo Geo.
//!
//! - `lspc`:        Line Sprite Processor — VRAM 68 KiB, IRQs VBlank/posición,
//!                  auto-animación, registros mapeados en `$3C0000-$3C000F`.
//! - `palette_lut`: tablas exactas de paleta MAME (5-bit con shadow/dark) y
//!                  FBNeo (6-bit con darken).
//! - `video`:       renderer scanline-accurate (fix layer + sprites + zoom +
//!                  shadow + backdrop) + encoder PNG mínimo.
//!
//! Originado en `neogeo-rs v31 / crates/neogeo-core/src/{lspc.rs,palette_lut.rs,video.rs}`.

pub mod lspc;
pub mod palette_lut;
pub mod video;

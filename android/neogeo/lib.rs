//! `pydmg-neogeo` — núcleo del emulador Neo Geo en Rust.
//!
//! Este crate reorganiza el proyecto upstream `neogeo-rs v31` (auditado en
//! [docs/CHANGES_v31.md]) en una estructura modular alineada con la propia
//! arquitectura hardware del Neo Geo:
//!
//! ```text
//! neogeo/
//!   audio/    YM2610 + bus de audio Z80
//!   cpu/      Motorola 68000 + Zilog Z80
//!   graphics/ LSPC + paleta + render scanline
//!   memory/   Bus 68k + cargador ROM + RTC uPD4990A
//!   neogeo/   System / SystemConfig (pegamento)
//!   dead_code/ código aislado del v31 que no estaba enganchado al árbol
//! ```
//!
//! Sin regresiones funcionales respecto al v31: se preservan firmas públicas
//! y se mantienen todos los tests `boot`, `video_decoder`, `video_palette`,
//! `video_sprites`, `m68k_*` y `z80_single_step_tests`.

#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)]

pub mod audio;
pub mod cpu;
pub mod graphics;
pub mod memory;
pub mod neogeo;

// ---------------------------------------------------------------------------
//   Re-exports compatibles con la API histórica de `neogeo_core`.
//
//   El v31 exponía los símbolos como `neogeo_core::{rom, system, video, ...}`.
//   Aquí re-exportamos los módulos bajo esos nombres planos para que el código
//   ya escrito (binario CLI, examples, herramientas externas) compile contra
//   `pydmg_neogeo::{rom, system, video, ...}` sin tocar nada.
// ---------------------------------------------------------------------------

pub use memory::bus::NeoGeoBus;
pub use memory::rom::{Cartridge, RomSet};
pub use neogeo::system::{Hardware, System, SystemConfig};

/// Alias plano: `pydmg_neogeo::rom` ≡ `pydmg_neogeo::memory::rom`.
pub use memory::rom;
/// Alias plano: `pydmg_neogeo::bus` ≡ `pydmg_neogeo::memory::bus`.
pub use memory::bus;
/// Alias plano: `pydmg_neogeo::upd4990a` ≡ `pydmg_neogeo::memory::upd4990a`.
pub use memory::upd4990a;
/// Alias plano: `pydmg_neogeo::system` ≡ `pydmg_neogeo::neogeo::system`.
pub use neogeo::system;
/// Alias plano: `pydmg_neogeo::video` ≡ `pydmg_neogeo::graphics::video`.
pub use graphics::video;
/// Alias plano: `pydmg_neogeo::lspc` ≡ `pydmg_neogeo::graphics::lspc`.
pub use graphics::lspc;
/// Alias plano: `pydmg_neogeo::palette_lut` ≡ `pydmg_neogeo::graphics::palette_lut`.
pub use graphics::palette_lut;
/// Alias plano: `pydmg_neogeo::ym2610` ≡ `pydmg_neogeo::audio::ym2610`.
pub use audio::ym2610;

/// Savestates: serialización binaria del estado de emulación.
pub mod state;

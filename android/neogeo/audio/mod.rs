//! Subsistema de audio Neo Geo.
//!
//! - `ym2610`: chip Yamaha YM2610 (FM 4 ch + SSG 3 ch + ADPCM-A 6 ch + ADPCM-B 1 ch).
//! - `audio_bus`: bus visto por el Z80 (mapa de memoria, banking M1, I/O ports,
//!   sound latch, NMI enable).
//!
//! Originado en `neogeo-rs v31 / crates/neogeo-core/src/{ym2610.rs,audio.rs}`.

pub mod audio_bus;
pub mod ym2610;

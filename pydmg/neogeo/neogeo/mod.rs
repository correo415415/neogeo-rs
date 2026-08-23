//! Estructura central de la consola: pegamento entre CPUs, bus, vídeo y audio.
//!
//! Re-exporta `System` y `SystemConfig`. La fachada `NeoGeo` (módulo
//! `neogeo`) construye y reúne las piezas para que `main.rs` no tenga que
//! conocer los detalles de cada subsistema.

pub mod system;

pub use system::{Hardware, System, SystemConfig};

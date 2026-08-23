//! Capa de CPU — agrupa M68000 y Z80, los dos procesadores del Neo Geo.
//!
//! - `m68k`: CPU principal a 12 MHz (juego, lógica, I/O del 68k bus).
//! - `z80`:  CPU de sonido a 4 MHz (driver YM2610).
//!
//! Estos sub-módulos son los crates `m68k` y `z80` del proyecto upstream
//! `neogeo-rs v31`, integrados en árbol único bajo `pydmg-neogeo`.

pub mod m68k;
pub mod z80;

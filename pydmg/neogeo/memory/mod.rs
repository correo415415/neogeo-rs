//! Subsistema de memoria Neo Geo.
//!
//! - `bus`:      `NeoGeoBus` — mapa de memoria oficial 68k, I/O `$300000`-`$3FFFFF`,
//!               watchdog, latch HC259 de sistema, palette RAM, memcard, etc.
//! - `rom`:      cargador `RomSet`/`Cartridge` (carpeta, ZIP MAME/FBNeo, BIOS
//!               separado, fallbacks SFIX/SM1, swap detection, etc.).
//! - `upd4990a`: NEC uPD4990A — RTC mapeado al I/O del 68k, usado por la BIOS.
//!
//! Originado en `neogeo-rs v31 / crates/neogeo-core/src/{bus.rs,rom.rs,upd4990a.rs}`.

pub mod bus;
pub mod rom;
pub mod upd4990a;

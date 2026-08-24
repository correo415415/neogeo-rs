//! Motorola 68000 (M68K) CPU core — sub-módulo del crate `pydmg-neogeo`.
//!
//! Implementación parcial pero verificada (SingleStepTests/m68000) que cubre
//! el subset usado por la BIOS Neo Geo y los cartuchos comunes. Originada en
//! `neogeo-rs v31 / crates/m68k`. Aquí pasa a ser un sub-módulo bajo
//! `crate::cpu::m68k`, sin cambios funcionales.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::unreadable_literal,
    clippy::wildcard_imports
)]

pub mod bus;
pub mod cpu;
mod ea;
mod exec;

pub use bus::{Bus, FlatBus};
pub use cpu::{Condition, Cpu, Exception, Size, StatusRegister};

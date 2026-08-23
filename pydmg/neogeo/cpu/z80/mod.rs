//! Zilog Z80 CPU core — Neo Geo sound CPU.
//!
//! Originado en `neogeo-rs v31 / crates/z80`. Aquí pasa a ser sub-módulo
//! `crate::cpu::z80` sin cambios funcionales. Mantiene el bus trait y un
//! `FlatRam` para tests SingleStepTests/z80.

#![allow(
    clippy::too_many_lines,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::module_name_repetitions
)]

pub mod cpu;
pub mod exec;
pub mod flags;

pub use cpu::{Cpu, Registers};
pub use flags::Flag;

/// Bus que el Z80 espera. Memoria e I/O comparten interfaz pero usan métodos
/// distintos porque el Z80 distingue /MREQ de /IORQ.
pub trait Z80Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn io_read(&mut self, port: u16) -> u8;
    fn io_write(&mut self, port: u16, value: u8);
}

/// 64 KiB flat-RAM bus usado en SingleStepTests y unit tests.
pub struct FlatRam {
    pub mem: Vec<u8>,
    pub io_in: Vec<u8>,
    pub io_out_log: Vec<(u16, u8)>,
}

impl FlatRam {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mem: vec![0u8; 0x1_0000],
            io_in: vec![0u8; 0x1_0000],
            io_out_log: Vec::new(),
        }
    }
}

impl Default for FlatRam {
    fn default() -> Self { Self::new() }
}

impl Z80Bus for FlatRam {
    fn read(&mut self, addr: u16) -> u8 { self.mem[addr as usize] }
    fn write(&mut self, addr: u16, value: u8) { self.mem[addr as usize] = value; }
    fn io_read(&mut self, port: u16) -> u8 { self.io_in[port as usize] }
    fn io_write(&mut self, port: u16, value: u8) { self.io_out_log.push((port, value)); }
}

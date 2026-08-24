//! Z80 CPU state.

use crate::cpu::z80::flags::Flag;
use crate::cpu::z80::Z80Bus;

/// Full Z80 register set including alternate set (`AF'/BC'/DE'/HL'`),
/// index registers `IX/IY`, refresh `R`, interrupt vector `I`, and the
/// internal "MEMPTR" / `WZ` register that SingleStepTests relies on.
#[derive(Debug, Default, Clone)]
pub struct Registers {
    pub a: u8, pub f: u8,
    pub b: u8, pub c: u8,
    pub d: u8, pub e: u8,
    pub h: u8, pub l: u8,
    /// Alternate set, encoded as packed 16-bit values so the SingleStepTests
    /// JSON fields (`af_`, `bc_`, `de_`, `hl_`) round-trip without ambiguity.
    pub af_: u16,
    pub bc_: u16,
    pub de_: u16,
    pub hl_: u16,
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,
    pub i: u8,
    pub r: u8,
    /// IFF1/IFF2 interrupt enable flip-flops.
    pub iff1: bool,
    pub iff2: bool,
    pub halted: bool,
    /// Interrupt mode (0, 1, or 2).
    pub im: u8,
    /// Internal MEMPTR / "WZ" register — undocumented but observable
    /// through `BIT n,(HL)` flags and the test set.
    pub wz: u16,
    /// "Q" register: holds the F value last *written* by an instruction
    /// that may affect SCF/CCF undocumented flags. Reset to 0 by any
    /// instruction that does *not* update F. See Sean Young §6 and the
    /// `q` field in SingleStepTests.
    pub q: u8,
    /// Set by the decoder whenever the current instruction touches F.
    /// `step()` reads this at end-of-instruction to update `q` and then
    /// clears it. Not part of the architectural state.
    pub f_was_written: bool,
}

impl Registers {
    #[inline] #[must_use] pub fn bc(&self) -> u16 { u16::from_be_bytes([self.b, self.c]) }
    #[inline] #[must_use] pub fn de(&self) -> u16 { u16::from_be_bytes([self.d, self.e]) }
    #[inline] #[must_use] pub fn hl(&self) -> u16 { u16::from_be_bytes([self.h, self.l]) }
    #[inline] #[must_use] pub fn af(&self) -> u16 { u16::from_be_bytes([self.a, self.f]) }
    #[inline] pub fn set_bc(&mut self, v: u16) { let [b, c] = v.to_be_bytes(); self.b = b; self.c = c; }
    #[inline] pub fn set_de(&mut self, v: u16) { let [d, e] = v.to_be_bytes(); self.d = d; self.e = e; }
    #[inline] pub fn set_hl(&mut self, v: u16) { let [h, l] = v.to_be_bytes(); self.h = h; self.l = l; }
    #[inline] pub fn set_af(&mut self, v: u16) { let [a, f] = v.to_be_bytes(); self.a = a; self.f = f; }

    #[inline] #[must_use] pub fn get_flag(&self, f: Flag) -> bool { (self.f & f.mask()) != 0 }
    #[inline] pub fn set_flag(&mut self, f: Flag, on: bool) {
        if on { self.f |= f.mask(); } else { self.f &= !f.mask(); }
    }

    /// Read register by 3-bit code as encoded in opcode (B C D E H L (HL) A).
    /// `(HL)` (code 6) is the caller's responsibility — this function
    /// panics if asked for it.
    #[inline]
    pub fn reg8(&self, code: u8) -> u8 {
        match code & 0x07 {
            0 => self.b, 1 => self.c, 2 => self.d, 3 => self.e,
            4 => self.h, 5 => self.l,
            7 => self.a,
            _ => panic!("reg8(6) is (HL), caller must handle memory access"),
        }
    }
    #[inline]
    pub fn set_reg8(&mut self, code: u8, v: u8) {
        match code & 0x07 {
            0 => self.b = v, 1 => self.c = v, 2 => self.d = v, 3 => self.e = v,
            4 => self.h = v, 5 => self.l = v,
            7 => self.a = v,
            _ => panic!("set_reg8(6) is (HL), caller must handle memory access"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Cpu {
    pub regs: Registers,
    pub cycles: u64,
    pub nmi_pending: bool,
    /// "INT" line — true means asserted.
    pub irq_line: bool,
    /// Data byte placed on the bus for IM 0 / IM 2.
    pub irq_data: u8,
    /// EI delay flag — interrupts are deferred until *after* the next
    /// instruction completes.
    pub ei_delay: bool,
}

impl Cpu {
    #[must_use] pub fn new() -> Self { Self::default() }

    pub fn reset(&mut self) {
        self.regs = Registers::default();
        self.regs.pc = 0;
        self.regs.sp = 0xFFFF;
        self.cycles = 0;
        self.nmi_pending = false;
        self.irq_line = false;
        self.ei_delay = false;
    }

    pub fn request_nmi(&mut self) { self.nmi_pending = true; }
    pub fn request_irq(&mut self, data: u8) {
        self.irq_line = true;
        self.irq_data = data;
    }
    pub fn clear_irq(&mut self) { self.irq_line = false; }

    /// Helper used by every instruction that writes F. Setting F via this
    /// helper also marks `f_was_written` so the Q latch updates correctly.
    #[inline]
    pub fn set_f(&mut self, v: u8) {
        self.regs.f = v;
        self.regs.f_was_written = true;
    }

    #[inline]
    pub fn push16<B: Z80Bus>(&mut self, bus: &mut B, v: u16) {
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, (v >> 8) as u8);
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, v as u8);
    }
    #[inline]
    pub fn pop16<B: Z80Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = bus.read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        let hi = bus.read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        u16::from_le_bytes([lo, hi])
    }

    /// Fetch the next program byte as an M1 opcode-fetch (advances PC and R).
    #[inline]
    pub fn fetch_op<B: Z80Bus>(&mut self, bus: &mut B) -> u8 {
        let b = bus.read(self.regs.pc);
        self.regs.pc = self.regs.pc.wrapping_add(1);
        // R is 7 bits + 1 unchanged bit (bit 7).
        let r = self.regs.r;
        self.regs.r = (r & 0x80) | ((r.wrapping_add(1)) & 0x7F);
        b
    }
    /// Read a non-M1 program byte (operand). Advances PC but not R.
    #[inline]
    pub fn read_op<B: Z80Bus>(&mut self, bus: &mut B) -> u8 {
        let b = bus.read(self.regs.pc);
        self.regs.pc = self.regs.pc.wrapping_add(1);
        b
    }
    #[inline]
    pub fn read_op16<B: Z80Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.read_op(bus);
        let hi = self.read_op(bus);
        u16::from_le_bytes([lo, hi])
    }
}

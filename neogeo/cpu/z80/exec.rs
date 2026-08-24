//! Z80 instruction execution.
//!
//! All instructions that touch F do so via `cpu.set_f(value)` which sets
//! `f_was_written`. The Q latch (Sean Young §6) is then computed once
//! per `step()` and used by SCF/CCF for undocumented YX bits.

use crate::cpu::z80::cpu::Cpu;
use crate::cpu::z80::flags::{Flag, sz53, sz53p};
use crate::cpu::z80::Z80Bus;

const FC: u8 = Flag::C as u8;
const FN: u8 = Flag::N as u8;
const FP: u8 = Flag::P as u8;
const FX: u8 = Flag::X as u8;
const FH: u8 = Flag::H as u8;
const FY: u8 = Flag::Y as u8;
const FZ: u8 = Flag::Z as u8;
const FS: u8 = Flag::S as u8;

// ---------- ALU helpers ----------

#[inline]
fn add8(cpu: &mut Cpu, a: u8, b: u8, carry_in: u8) -> u8 {
    let r16 = a as u16 + b as u16 + carry_in as u16;
    let r = r16 as u8;
    let half = ((a & 0x0F) + (b & 0x0F) + carry_in) & 0x10;
    let overflow = ((!(a ^ b)) & (a ^ r) & 0x80) != 0;
    let mut f = sz53(r);
    if half != 0 { f |= FH; }
    if overflow  { f |= FP; }
    if r16 > 0xFF { f |= FC; }
    cpu.set_f(f);
    r
}

#[inline]
fn sub8(cpu: &mut Cpu, a: u8, b: u8, carry_in: u8) -> u8 {
    let r16 = (a as i16) - (b as i16) - (carry_in as i16);
    let r = r16 as u8;
    let half = ((a & 0x0F) as i16 - (b & 0x0F) as i16 - carry_in as i16) & 0x10;
    let overflow = ((a ^ b) & (a ^ r) & 0x80) != 0;
    let mut f = sz53(r) | FN;
    if half != 0 { f |= FH; }
    if overflow  { f |= FP; }
    if r16 < 0   { f |= FC; }
    cpu.set_f(f);
    r
}

#[inline]
fn cp8(cpu: &mut Cpu, a: u8, b: u8) {
    // CP is SUB without storing the result, but YX come from operand b.
    let r16 = (a as i16) - (b as i16);
    let r = r16 as u8;
    let half = ((a & 0x0F) as i16 - (b & 0x0F) as i16) & 0x10;
    let overflow = ((a ^ b) & (a ^ r) & 0x80) != 0;
    let mut f = (r & FS) | FN;
    if r == 0       { f |= FZ; }
    if half != 0    { f |= FH; }
    if overflow      { f |= FP; }
    if r16 < 0       { f |= FC; }
    f |= b & (FY | FX);
    cpu.set_f(f);
}

#[inline]
fn and8(cpu: &mut Cpu, a: u8, b: u8) -> u8 {
    let r = a & b;
    cpu.set_f(sz53p(r) | FH);
    r
}
#[inline]
fn or8(cpu: &mut Cpu, a: u8, b: u8) -> u8 {
    let r = a | b;
    cpu.set_f(sz53p(r));
    r
}
#[inline]
fn xor8(cpu: &mut Cpu, a: u8, b: u8) -> u8 {
    let r = a ^ b;
    cpu.set_f(sz53p(r));
    r
}

#[inline]
fn inc8(cpu: &mut Cpu, a: u8) -> u8 {
    let r = a.wrapping_add(1);
    let mut f = sz53(r) | (cpu.regs.f & FC);
    if (r & 0x0F) == 0       { f |= FH; }
    if a == 0x7F             { f |= FP; }
    cpu.set_f(f);
    r
}
#[inline]
fn dec8(cpu: &mut Cpu, a: u8) -> u8 {
    let r = a.wrapping_sub(1);
    let mut f = sz53(r) | (cpu.regs.f & FC) | FN;
    if (r & 0x0F) == 0x0F    { f |= FH; }
    if a == 0x80             { f |= FP; }
    cpu.set_f(f);
    r
}

#[inline]
fn add16(cpu: &mut Cpu, a: u16, b: u16) -> u16 {
    let r32 = a as u32 + b as u32;
    let r = r32 as u16;
    let half = ((a & 0x0FFF) + (b & 0x0FFF)) & 0x1000;
    let mut f = cpu.regs.f & (FS | FZ | FP);
    if half != 0       { f |= FH; }
    if r32 > 0xFFFF    { f |= FC; }
    f |= ((r >> 8) as u8) & (FY | FX);
    cpu.set_f(f);
    cpu.regs.wz = a.wrapping_add(1);
    r
}

#[inline]
fn adc16(cpu: &mut Cpu, a: u16, b: u16) -> u16 {
    let c = (cpu.regs.f & FC) as u32;
    let r32 = a as u32 + b as u32 + c;
    let r = r32 as u16;
    let half = ((a & 0x0FFF) + (b & 0x0FFF) + c as u16) & 0x1000;
    let overflow = ((!(a ^ b)) & (a ^ r) & 0x8000) != 0;
    let mut f: u8 = 0;
    if (r & 0x8000) != 0       { f |= FS; }
    if r == 0                  { f |= FZ; }
    if half != 0               { f |= FH; }
    if overflow                 { f |= FP; }
    if r32 > 0xFFFF            { f |= FC; }
    f |= ((r >> 8) as u8) & (FY | FX);
    cpu.set_f(f);
    cpu.regs.wz = a.wrapping_add(1);
    r
}

#[inline]
fn sbc16(cpu: &mut Cpu, a: u16, b: u16) -> u16 {
    let c = (cpu.regs.f & FC) as i32;
    let r32 = (a as i32) - (b as i32) - c;
    let r = r32 as u16;
    let half = ((a & 0x0FFF) as i32 - (b & 0x0FFF) as i32 - c) & 0x1000;
    let overflow = ((a ^ b) & (a ^ r) & 0x8000) != 0;
    let mut f: u8 = FN;
    if (r & 0x8000) != 0       { f |= FS; }
    if r == 0                  { f |= FZ; }
    if half != 0               { f |= FH; }
    if overflow                 { f |= FP; }
    if r32 < 0                 { f |= FC; }
    f |= ((r >> 8) as u8) & (FY | FX);
    cpu.set_f(f);
    cpu.regs.wz = a.wrapping_add(1);
    r
}

// ---------- CB-prefix helpers ----------

#[inline] fn rlc(cpu: &mut Cpu, v: u8) -> u8 { let c = (v >> 7) & 1; let r = v.rotate_left(1); cpu.set_f(sz53p(r) | c); r }
#[inline] fn rrc(cpu: &mut Cpu, v: u8) -> u8 { let c = v & 1; let r = v.rotate_right(1); cpu.set_f(sz53p(r) | c); r }
#[inline] fn rl (cpu: &mut Cpu, v: u8) -> u8 {
    let c_in = cpu.regs.f & FC; let c_out = (v >> 7) & 1;
    let r = (v << 1) | c_in; cpu.set_f(sz53p(r) | c_out); r
}
#[inline] fn rr (cpu: &mut Cpu, v: u8) -> u8 {
    let c_in = (cpu.regs.f & FC) << 7; let c_out = v & 1;
    let r = (v >> 1) | c_in; cpu.set_f(sz53p(r) | c_out); r
}
#[inline] fn sla(cpu: &mut Cpu, v: u8) -> u8 { let c = (v >> 7) & 1; let r = v << 1; cpu.set_f(sz53p(r) | c); r }
#[inline] fn sra(cpu: &mut Cpu, v: u8) -> u8 { let c = v & 1; let r = (v >> 1) | (v & 0x80); cpu.set_f(sz53p(r) | c); r }
/// SLL (undocumented).
#[inline] fn sll(cpu: &mut Cpu, v: u8) -> u8 { let c = (v >> 7) & 1; let r = (v << 1) | 1; cpu.set_f(sz53p(r) | c); r }
#[inline] fn srl(cpu: &mut Cpu, v: u8) -> u8 { let c = v & 1; let r = v >> 1; cpu.set_f(sz53p(r) | c); r }

// ---------- Memory helpers ----------

#[inline] fn read_hl<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u8 { bus.read(cpu.regs.hl()) }
#[inline] fn write_hl<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, v: u8) { bus.write(cpu.regs.hl(), v); }
#[inline]
fn read_r8<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, code: u8) -> u8 {
    if (code & 7) == 6 { read_hl(cpu, bus) } else { cpu.regs.reg8(code) }
}
#[inline]
fn write_r8<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, code: u8, v: u8) {
    if (code & 7) == 6 { write_hl(cpu, bus, v); } else { cpu.regs.set_reg8(code, v); }
}

// ---------- Interrupts ----------

fn service_nmi<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    log::trace!("Z80 NMI taken: pc=${:04X} sp=${:04X}", cpu.regs.pc, cpu.regs.sp);
    cpu.regs.halted = false;
    cpu.regs.iff2 = cpu.regs.iff1;
    cpu.regs.iff1 = false;
    let r = cpu.regs.r;
    cpu.regs.r = (r & 0x80) | ((r.wrapping_add(1)) & 0x7F);
    cpu.push16(bus, cpu.regs.pc);
    cpu.regs.pc = 0x0066;
    cpu.regs.wz = 0x0066;
    cpu.regs.q = 0;
    cpu.nmi_pending = false;
    11
}

fn service_irq<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    log::trace!("Z80 IRQ taken: pc=${:04X} sp=${:04X} im={} data=${:02X}", cpu.regs.pc, cpu.regs.sp, cpu.regs.im, cpu.irq_data);
    cpu.regs.halted = false;
    cpu.regs.iff1 = false;
    cpu.regs.iff2 = false;
    let r = cpu.regs.r;
    cpu.regs.r = (r & 0x80) | ((r.wrapping_add(1)) & 0x7F);
    let data = cpu.irq_data;
    cpu.clear_irq();
    match cpu.regs.im {
        0 => {
            cpu.push16(bus, cpu.regs.pc);
            let target = (data & 0x38) as u16;
            cpu.regs.pc = target;
            cpu.regs.wz = target;
            cpu.regs.q = 0;
            13
        }
        1 => {
            cpu.push16(bus, cpu.regs.pc);
            cpu.regs.pc = 0x0038;
            cpu.regs.wz = 0x0038;
            cpu.regs.q = 0;
            13
        }
        _ => {
            cpu.push16(bus, cpu.regs.pc);
            let vec = ((cpu.regs.i as u16) << 8) | ((data & 0xFE) as u16);
            let lo = bus.read(vec);
            let hi = bus.read(vec.wrapping_add(1));
            let target = u16::from_le_bytes([lo, hi]);
            cpu.regs.pc = target;
            cpu.regs.wz = target;
            cpu.regs.q = 0;
            19
        }
    }
}

// ---------- Public step ----------

impl Cpu {
    /// Execute one Z80 instruction (or service one pending interrupt).
    pub fn step<B: Z80Bus>(&mut self, bus: &mut B) -> u32 {
        if self.nmi_pending {
            return service_nmi(self, bus);
        }
        if self.irq_line && self.regs.iff1 && !self.ei_delay {
            return service_irq(self, bus);
        }
        let was_ei_delay = self.ei_delay;

        if self.regs.halted {
            let r = self.regs.r;
            self.regs.r = (r & 0x80) | (r.wrapping_add(1) & 0x7F);
            self.regs.q = 0;
            if was_ei_delay { self.ei_delay = false; }
            return 4;
        }

        self.regs.f_was_written = false;
        let op = self.fetch_op(bus);
        let cycles = decode_main(self, bus, op);

        // Q := new F if F was written, else 0.
        self.regs.q = if self.regs.f_was_written { self.regs.f } else { 0 };

        if was_ei_delay { self.ei_delay = false; }
        cycles
    }
}

#[inline]
fn cond_true(cpu: &Cpu, cc: u8) -> bool {
    match cc & 7 {
        0 => !cpu.regs.get_flag(Flag::Z),
        1 =>  cpu.regs.get_flag(Flag::Z),
        2 => !cpu.regs.get_flag(Flag::C),
        3 =>  cpu.regs.get_flag(Flag::C),
        4 => !cpu.regs.get_flag(Flag::P),
        5 =>  cpu.regs.get_flag(Flag::P),
        6 => !cpu.regs.get_flag(Flag::S),
        7 =>  cpu.regs.get_flag(Flag::S),
        _ => unreachable!(),
    }
}

// ---------- Main table ----------

fn decode_main<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, op: u8) -> u32 {
    match op {
        0x00 => 4,
        0x01 => { let v = cpu.read_op16(bus); cpu.regs.set_bc(v); 10 }
        0x11 => { let v = cpu.read_op16(bus); cpu.regs.set_de(v); 10 }
        0x21 => { let v = cpu.read_op16(bus); cpu.regs.set_hl(v); 10 }
        0x31 => { let v = cpu.read_op16(bus); cpu.regs.sp = v;   10 }

        0x02 => {
            bus.write(cpu.regs.bc(), cpu.regs.a);
            cpu.regs.wz = ((cpu.regs.a as u16) << 8) | (cpu.regs.bc().wrapping_add(1) & 0xff);
            7
        }
        0x12 => {
            bus.write(cpu.regs.de(), cpu.regs.a);
            cpu.regs.wz = ((cpu.regs.a as u16) << 8) | (cpu.regs.de().wrapping_add(1) & 0xff);
            7
        }
        0x0A => { let addr = cpu.regs.bc(); cpu.regs.a = bus.read(addr); cpu.regs.wz = addr.wrapping_add(1); 7 }
        0x1A => { let addr = cpu.regs.de(); cpu.regs.a = bus.read(addr); cpu.regs.wz = addr.wrapping_add(1); 7 }

        0x22 => {
            let addr = cpu.read_op16(bus);
            bus.write(addr, cpu.regs.l);
            bus.write(addr.wrapping_add(1), cpu.regs.h);
            cpu.regs.wz = addr.wrapping_add(1);
            16
        }
        0x32 => {
            let addr = cpu.read_op16(bus);
            bus.write(addr, cpu.regs.a);
            cpu.regs.wz = ((cpu.regs.a as u16) << 8) | (addr.wrapping_add(1) & 0xff);
            13
        }
        0x2A => {
            let addr = cpu.read_op16(bus);
            cpu.regs.l = bus.read(addr);
            cpu.regs.h = bus.read(addr.wrapping_add(1));
            cpu.regs.wz = addr.wrapping_add(1);
            16
        }
        0x3A => {
            let addr = cpu.read_op16(bus);
            cpu.regs.a = bus.read(addr);
            cpu.regs.wz = addr.wrapping_add(1);
            13
        }

        0x03 => { cpu.regs.set_bc(cpu.regs.bc().wrapping_add(1)); 6 }
        0x13 => { cpu.regs.set_de(cpu.regs.de().wrapping_add(1)); 6 }
        0x23 => { cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1)); 6 }
        0x33 => { cpu.regs.sp = cpu.regs.sp.wrapping_add(1);       6 }
        0x0B => { cpu.regs.set_bc(cpu.regs.bc().wrapping_sub(1)); 6 }
        0x1B => { cpu.regs.set_de(cpu.regs.de().wrapping_sub(1)); 6 }
        0x2B => { cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1)); 6 }
        0x3B => { cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);       6 }

        0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x3C => {
            let code = (op >> 3) & 7;
            let v = cpu.regs.reg8(code);
            let r = inc8(cpu, v);
            cpu.regs.set_reg8(code, r);
            4
        }
        0x34 => { let v = read_hl(cpu, bus); let r = inc8(cpu, v); write_hl(cpu, bus, r); 11 }
        0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x3D => {
            let code = (op >> 3) & 7;
            let v = cpu.regs.reg8(code);
            let r = dec8(cpu, v);
            cpu.regs.set_reg8(code, r);
            4
        }
        0x35 => { let v = read_hl(cpu, bus); let r = dec8(cpu, v); write_hl(cpu, bus, r); 11 }

        0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x3E => {
            let code = (op >> 3) & 7;
            let n = cpu.read_op(bus);
            cpu.regs.set_reg8(code, n);
            7
        }
        0x36 => { let n = cpu.read_op(bus); write_hl(cpu, bus, n); 10 }

        // RLCA / RRCA / RLA / RRA
        0x07 => {
            let a = cpu.regs.a;
            let c = (a >> 7) & 1;
            cpu.regs.a = a.rotate_left(1);
            cpu.set_f((cpu.regs.f & (FS | FZ | FP)) | c | (cpu.regs.a & (FY | FX)));
            4
        }
        0x0F => {
            let a = cpu.regs.a;
            let c = a & 1;
            cpu.regs.a = a.rotate_right(1);
            cpu.set_f((cpu.regs.f & (FS | FZ | FP)) | c | (cpu.regs.a & (FY | FX)));
            4
        }
        0x17 => {
            let a = cpu.regs.a;
            let c_in = cpu.regs.f & FC;
            let c_out = (a >> 7) & 1;
            cpu.regs.a = (a << 1) | c_in;
            cpu.set_f((cpu.regs.f & (FS | FZ | FP)) | c_out | (cpu.regs.a & (FY | FX)));
            4
        }
        0x1F => {
            let a = cpu.regs.a;
            let c_in = (cpu.regs.f & FC) << 7;
            let c_out = a & 1;
            cpu.regs.a = (a >> 1) | c_in;
            cpu.set_f((cpu.regs.f & (FS | FZ | FP)) | c_out | (cpu.regs.a & (FY | FX)));
            4
        }

        // EX AF, AF'
        0x08 => {
            let cur = cpu.regs.af();
            cpu.regs.set_af(cpu.regs.af_);
            cpu.regs.af_ = cur;
            4
        }
        0x10 => {
            let d = cpu.read_op(bus) as i8;
            cpu.regs.b = cpu.regs.b.wrapping_sub(1);
            if cpu.regs.b != 0 {
                cpu.regs.pc = cpu.regs.pc.wrapping_add(d as i16 as u16);
                cpu.regs.wz = cpu.regs.pc;
                13
            } else { 8 }
        }
        0x18 => {
            let d = cpu.read_op(bus) as i8;
            cpu.regs.pc = cpu.regs.pc.wrapping_add(d as i16 as u16);
            cpu.regs.wz = cpu.regs.pc;
            12
        }
        0x20 | 0x28 | 0x30 | 0x38 => {
            let cc = (op >> 3) & 3;
            let taken = match cc {
                0 => !cpu.regs.get_flag(Flag::Z),
                1 =>  cpu.regs.get_flag(Flag::Z),
                2 => !cpu.regs.get_flag(Flag::C),
                3 =>  cpu.regs.get_flag(Flag::C),
                _ => unreachable!(),
            };
            let d = cpu.read_op(bus) as i8;
            if taken {
                cpu.regs.pc = cpu.regs.pc.wrapping_add(d as i16 as u16);
                cpu.regs.wz = cpu.regs.pc;
                12
            } else { 7 }
        }

        0x09 => { let r = add16(cpu, cpu.regs.hl(), cpu.regs.bc()); cpu.regs.set_hl(r); 11 }
        0x19 => { let r = add16(cpu, cpu.regs.hl(), cpu.regs.de()); cpu.regs.set_hl(r); 11 }
        0x29 => { let r = add16(cpu, cpu.regs.hl(), cpu.regs.hl()); cpu.regs.set_hl(r); 11 }
        0x39 => { let r = add16(cpu, cpu.regs.hl(), cpu.regs.sp);   cpu.regs.set_hl(r); 11 }

        0x27 => { daa(cpu); 4 }
        0x2F => {
            cpu.regs.a = !cpu.regs.a;
            cpu.set_f((cpu.regs.f & (FS | FZ | FP | FC)) | FH | FN | (cpu.regs.a & (FY | FX)));
            4
        }
        // SCF / CCF — YX derivation per Sean Young §6:
        //   if Q == 0 (previous instruction did not write F):
        //     yx = (A | F) & YX        ← yes, OR — spans both A and stale F bits
        //   if Q != 0 (previous instruction wrote F):
        //     yx = A & YX               ← only from A, the freshly-written F is opaque
        //
        // Prefix M1 cycles (DD, FD, ED, CB themselves) reset Q because
        // they are no-ops with respect to F. We model that by setting
        // `cpu.regs.q = 0` at the top of every prefix handler.
        0x37 => {
            let yx_src = if cpu.regs.q == 0 { cpu.regs.a | cpu.regs.f } else { cpu.regs.a };
            let yx = yx_src & (FY | FX);
            cpu.set_f((cpu.regs.f & (FS | FZ | FP)) | FC | yx);
            4
        }
        0x3F => {
            let old_c = cpu.regs.f & FC;
            let yx_src = if cpu.regs.q == 0 { cpu.regs.a | cpu.regs.f } else { cpu.regs.a };
            let yx = yx_src & (FY | FX);
            cpu.set_f((cpu.regs.f & (FS | FZ | FP)) | (old_c << 4) | yx | (old_c ^ FC));
            4
        }

        0x76 => {
            // HALT: the corpus pre-increments PC during M1 fetch and
            // expects PC to remain on the byte *after* HALT. The CPU
            // stays in halt state until NMI/INT; subsequent step() calls
            // execute an internal NOP without advancing PC (see the
            // `if self.regs.halted` branch in `step()`).
            cpu.regs.halted = true;
            4
        }
        0x40..=0x7F => {
            let dst = (op >> 3) & 7;
            let src = op & 7;
            let v = read_r8(cpu, bus, src);
            write_r8(cpu, bus, dst, v);
            if dst == 6 || src == 6 { 7 } else { 4 }
        }

        0x80..=0x87 => { let v = read_r8(cpu, bus, op & 7); let r = add8(cpu, cpu.regs.a, v, 0); cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0x88..=0x8F => { let v = read_r8(cpu, bus, op & 7); let c = cpu.regs.f & FC; let r = add8(cpu, cpu.regs.a, v, c); cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0x90..=0x97 => { let v = read_r8(cpu, bus, op & 7); let r = sub8(cpu, cpu.regs.a, v, 0); cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0x98..=0x9F => { let v = read_r8(cpu, bus, op & 7); let c = cpu.regs.f & FC; let r = sub8(cpu, cpu.regs.a, v, c); cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0xA0..=0xA7 => { let v = read_r8(cpu, bus, op & 7); let r = and8(cpu, cpu.regs.a, v); cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0xA8..=0xAF => { let v = read_r8(cpu, bus, op & 7); let r = xor8(cpu, cpu.regs.a, v); cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0xB0..=0xB7 => { let v = read_r8(cpu, bus, op & 7); let r = or8(cpu, cpu.regs.a, v);  cpu.regs.a = r; if (op & 7) == 6 { 7 } else { 4 } }
        0xB8..=0xBF => { let v = read_r8(cpu, bus, op & 7); cp8(cpu, cpu.regs.a, v); if (op & 7) == 6 { 7 } else { 4 } }

        0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xE0 | 0xE8 | 0xF0 | 0xF8 => {
            let cc = (op >> 3) & 7;
            if cond_true(cpu, cc) {
                let target = cpu.pop16(bus);
                cpu.regs.pc = target;
                cpu.regs.wz = target;
                11
            } else { 5 }
        }
        0xC9 => { let t = cpu.pop16(bus); cpu.regs.pc = t; cpu.regs.wz = t; 10 }

        0xC1 | 0xD1 | 0xE1 | 0xF1 => {
            let v = cpu.pop16(bus);
            match (op >> 4) & 3 {
                0 => cpu.regs.set_bc(v),
                1 => cpu.regs.set_de(v),
                2 => cpu.regs.set_hl(v),
                // POP AF *changes* F but does not count as a flag-
                // updating instruction for Q-latch purposes (Sean Young
                // §6: only "flag-touching" ALU ops set Q). Leave
                // f_was_written untouched.
                3 => cpu.regs.set_af(v),
                _ => unreachable!(),
            }
            10
        }
        0xC5 | 0xD5 | 0xE5 | 0xF5 => {
            let v = match (op >> 4) & 3 {
                0 => cpu.regs.bc(), 1 => cpu.regs.de(),
                2 => cpu.regs.hl(), 3 => cpu.regs.af(),
                _ => unreachable!(),
            };
            cpu.push16(bus, v);
            11
        }

        0xC3 => { let a = cpu.read_op16(bus); cpu.regs.pc = a; cpu.regs.wz = a; 10 }
        0xC2 | 0xCA | 0xD2 | 0xDA | 0xE2 | 0xEA | 0xF2 | 0xFA => {
            let cc = (op >> 3) & 7;
            let a = cpu.read_op16(bus);
            cpu.regs.wz = a;
            if cond_true(cpu, cc) { cpu.regs.pc = a; }
            10
        }

        0xCD => {
            let a = cpu.read_op16(bus);
            cpu.push16(bus, cpu.regs.pc);
            cpu.regs.pc = a;
            cpu.regs.wz = a;
            17
        }
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xE4 | 0xEC | 0xF4 | 0xFC => {
            let cc = (op >> 3) & 7;
            let a = cpu.read_op16(bus);
            cpu.regs.wz = a;
            if cond_true(cpu, cc) {
                cpu.push16(bus, cpu.regs.pc);
                cpu.regs.pc = a;
                17
            } else { 10 }
        }

        0xC6 => { let n = cpu.read_op(bus); let r = add8(cpu, cpu.regs.a, n, 0); cpu.regs.a = r; 7 }
        0xCE => { let n = cpu.read_op(bus); let c = cpu.regs.f & FC; let r = add8(cpu, cpu.regs.a, n, c); cpu.regs.a = r; 7 }
        0xD6 => { let n = cpu.read_op(bus); let r = sub8(cpu, cpu.regs.a, n, 0); cpu.regs.a = r; 7 }
        0xDE => { let n = cpu.read_op(bus); let c = cpu.regs.f & FC; let r = sub8(cpu, cpu.regs.a, n, c); cpu.regs.a = r; 7 }
        0xE6 => { let n = cpu.read_op(bus); let r = and8(cpu, cpu.regs.a, n); cpu.regs.a = r; 7 }
        0xEE => { let n = cpu.read_op(bus); let r = xor8(cpu, cpu.regs.a, n); cpu.regs.a = r; 7 }
        0xF6 => { let n = cpu.read_op(bus); let r = or8(cpu, cpu.regs.a, n);  cpu.regs.a = r; 7 }
        0xFE => { let n = cpu.read_op(bus); cp8(cpu, cpu.regs.a, n); 7 }

        0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
            cpu.push16(bus, cpu.regs.pc);
            let t = (op & 0x38) as u16;
            cpu.regs.pc = t;
            cpu.regs.wz = t;
            11
        }

        0xD3 => {
            let n = cpu.read_op(bus);
            let port = ((cpu.regs.a as u16) << 8) | n as u16;
            bus.io_write(port, cpu.regs.a);
            cpu.regs.wz = ((cpu.regs.a as u16) << 8) | (n.wrapping_add(1) as u16);
            11
        }
        0xDB => {
            let n = cpu.read_op(bus);
            let port = ((cpu.regs.a as u16) << 8) | n as u16;
            cpu.regs.a = bus.io_read(port);
            cpu.regs.wz = port.wrapping_add(1);
            11
        }

        0xD9 => {
            let bc = cpu.regs.bc(); let de = cpu.regs.de(); let hl = cpu.regs.hl();
            cpu.regs.set_bc(cpu.regs.bc_); cpu.regs.set_de(cpu.regs.de_); cpu.regs.set_hl(cpu.regs.hl_);
            cpu.regs.bc_ = bc; cpu.regs.de_ = de; cpu.regs.hl_ = hl;
            4
        }
        0xE3 => {
            let lo = bus.read(cpu.regs.sp);
            let hi = bus.read(cpu.regs.sp.wrapping_add(1));
            bus.write(cpu.regs.sp, cpu.regs.l);
            bus.write(cpu.regs.sp.wrapping_add(1), cpu.regs.h);
            let new_hl = u16::from_le_bytes([lo, hi]);
            cpu.regs.set_hl(new_hl);
            cpu.regs.wz = new_hl;
            19
        }
        0xEB => {
            let de = cpu.regs.de(); let hl = cpu.regs.hl();
            cpu.regs.set_de(hl); cpu.regs.set_hl(de);
            4
        }
        0xE9 => { cpu.regs.pc = cpu.regs.hl(); 4 }
        0xF9 => { cpu.regs.sp = cpu.regs.hl(); 6 }
        0xF3 => { cpu.regs.iff1 = false; cpu.regs.iff2 = false; 4 }
        0xFB => { cpu.regs.iff1 = true;  cpu.regs.iff2 = true;  cpu.ei_delay = true; 4 }

        0xCB => exec_cb(cpu, bus),
        0xED => exec_ed(cpu, bus),
        0xDD => exec_index(cpu, bus, true),
        0xFD => exec_index(cpu, bus, false),
    }
}

// ---------- DAA ----------

fn daa(cpu: &mut Cpu) {
    let a = cpu.regs.a;
    let n = (cpu.regs.f & FN) != 0;
    let h = (cpu.regs.f & FH) != 0;
    let c = (cpu.regs.f & FC) != 0;
    let mut correction: u8 = 0;
    let mut new_c = c;
    if (a & 0x0F) > 9 || h { correction |= 0x06; }
    if a > 0x99 || c { correction |= 0x60; new_c = true; }
    let new_a = if n { a.wrapping_sub(correction) } else { a.wrapping_add(correction) };
    let new_h = if n { h && (a & 0x0F) < 6 } else { (a & 0x0F) > 9 };
    let mut f = sz53p(new_a);
    if n { f |= FN; }
    if new_h { f |= FH; }
    if new_c { f |= FC; }
    cpu.regs.a = new_a;
    cpu.set_f(f);
}

// ---------- CB prefix ----------

fn exec_cb<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    // M1 cycle for the CB prefix itself doesn't touch F — reset Q so
    // a downstream SCF/CCF (rare but possible via DDCB/FDCB chain)
    // computes YX correctly.
    cpu.regs.q = 0;
    let op = cpu.fetch_op(bus);
    let reg = op & 7;
    let bit = (op >> 3) & 7;
    let is_mem = reg == 6;
    let v = read_r8(cpu, bus, reg);
    match op {
        0x00..=0x07 => { let r = rlc(cpu, v); write_r8(cpu, bus, reg, r); }
        0x08..=0x0F => { let r = rrc(cpu, v); write_r8(cpu, bus, reg, r); }
        0x10..=0x17 => { let r = rl (cpu, v); write_r8(cpu, bus, reg, r); }
        0x18..=0x1F => { let r = rr (cpu, v); write_r8(cpu, bus, reg, r); }
        0x20..=0x27 => { let r = sla(cpu, v); write_r8(cpu, bus, reg, r); }
        0x28..=0x2F => { let r = sra(cpu, v); write_r8(cpu, bus, reg, r); }
        0x30..=0x37 => { let r = sll(cpu, v); write_r8(cpu, bus, reg, r); }
        0x38..=0x3F => { let r = srl(cpu, v); write_r8(cpu, bus, reg, r); }
        0x40..=0x7F => {
            let mask = 1u8 << bit;
            let r = v & mask;
            let mut f = (cpu.regs.f & FC) | FH;
            if r == 0 { f |= FZ | FP; }
            if (r & FS) != 0 { f |= FS; }
            if is_mem {
                f |= ((cpu.regs.wz >> 8) as u8) & (FY | FX);
            } else {
                f |= v & (FY | FX);
            }
            cpu.set_f(f);
        }
        0x80..=0xBF => {
            let r = v & !(1u8 << bit);
            write_r8(cpu, bus, reg, r);
        }
        0xC0..=0xFF => {
            let r = v | (1u8 << bit);
            write_r8(cpu, bus, reg, r);
        }
    }
    if is_mem {
        if (op & 0xC0) == 0x40 { 12 } else { 15 }
    } else { 8 }
}

// ---------- ED prefix ----------

fn exec_ed<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    cpu.regs.q = 0;
    let op = cpu.fetch_op(bus);
    match op {
        0x44 | 0x4C | 0x54 | 0x5C | 0x64 | 0x6C | 0x74 | 0x7C => {
            let a = cpu.regs.a;
            cpu.regs.a = 0;
            let r = sub8(cpu, 0, a, 0);
            cpu.regs.a = r;
            8
        }
        0x46 | 0x4E | 0x66 | 0x6E => { cpu.regs.im = 0; 8 }
        0x56 | 0x76               => { cpu.regs.im = 1; 8 }
        0x5E | 0x7E               => { cpu.regs.im = 2; 8 }

        0x45 | 0x55 | 0x65 | 0x75 | 0x4D | 0x5D | 0x6D | 0x7D => {
            let t = cpu.pop16(bus);
            cpu.regs.pc = t;
            cpu.regs.wz = t;
            cpu.regs.iff1 = cpu.regs.iff2;
            14
        }

        0x47 => { cpu.regs.i = cpu.regs.a; 9 }
        0x4F => { cpu.regs.r = cpu.regs.a; 9 }
        0x57 => {
            cpu.regs.a = cpu.regs.i;
            let mut f = sz53(cpu.regs.a) | (cpu.regs.f & FC);
            if cpu.regs.iff2 { f |= FP; }
            cpu.set_f(f);
            9
        }
        0x5F => {
            cpu.regs.a = cpu.regs.r;
            let mut f = sz53(cpu.regs.a) | (cpu.regs.f & FC);
            if cpu.regs.iff2 { f |= FP; }
            cpu.set_f(f);
            9
        }

        0x42 | 0x52 | 0x62 | 0x72 => {
            let v = match (op >> 4) & 3 {
                0 => cpu.regs.bc(), 1 => cpu.regs.de(),
                2 => cpu.regs.hl(), 3 => cpu.regs.sp,
                _ => unreachable!(),
            };
            let r = sbc16(cpu, cpu.regs.hl(), v);
            cpu.regs.set_hl(r);
            15
        }
        0x4A | 0x5A | 0x6A | 0x7A => {
            let v = match (op >> 4) & 3 {
                0 => cpu.regs.bc(), 1 => cpu.regs.de(),
                2 => cpu.regs.hl(), 3 => cpu.regs.sp,
                _ => unreachable!(),
            };
            let r = adc16(cpu, cpu.regs.hl(), v);
            cpu.regs.set_hl(r);
            15
        }

        0x43 | 0x53 | 0x63 | 0x73 => {
            let addr = cpu.read_op16(bus);
            let v = match (op >> 4) & 3 {
                0 => cpu.regs.bc(), 1 => cpu.regs.de(),
                2 => cpu.regs.hl(), 3 => cpu.regs.sp,
                _ => unreachable!(),
            };
            bus.write(addr, v as u8);
            bus.write(addr.wrapping_add(1), (v >> 8) as u8);
            cpu.regs.wz = addr.wrapping_add(1);
            20
        }
        0x4B | 0x5B | 0x6B | 0x7B => {
            let addr = cpu.read_op16(bus);
            let lo = bus.read(addr);
            let hi = bus.read(addr.wrapping_add(1));
            let v = u16::from_le_bytes([lo, hi]);
            match (op >> 4) & 3 {
                0 => cpu.regs.set_bc(v), 1 => cpu.regs.set_de(v),
                2 => cpu.regs.set_hl(v), 3 => cpu.regs.sp = v,
                _ => unreachable!(),
            }
            cpu.regs.wz = addr.wrapping_add(1);
            20
        }

        0xA0 => { ldi(cpu, bus); 16 }
        0xA8 => { ldd(cpu, bus); 16 }
        0xB0 => {
            ldi(cpu, bus);
            if cpu.regs.bc() != 0 {
                cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
                cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
                fixup_yx_from_pc_high(cpu);
                21
            } else { 16 }
        }
        0xB8 => {
            ldd(cpu, bus);
            if cpu.regs.bc() != 0 {
                cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
                cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
                fixup_yx_from_pc_high(cpu);
                21
            } else { 16 }
        }
        0xA1 => { cpi(cpu, bus); 16 }
        0xA9 => { cpd(cpu, bus); 16 }
        0xB1 => {
            cpi(cpu, bus);
            if cpu.regs.bc() != 0 && (cpu.regs.f & FZ) == 0 {
                cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
                cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
                fixup_yx_from_pc_high(cpu);
                21
            } else { 16 }
        }
        0xB9 => {
            cpd(cpu, bus);
            if cpu.regs.bc() != 0 && (cpu.regs.f & FZ) == 0 {
                cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
                cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
                fixup_yx_from_pc_high(cpu);
                21
            } else { 16 }
        }
        0xA2 => { ini (cpu, bus); 16 }
        0xAA => { ind (cpu, bus); 16 }
        0xB2 => {
            let val = bus.read(cpu.regs.bc()); // re-read to know transferred byte
            let _ = val;
            // outi/ini already wrote (HL) before reading bus; we need the
            // value that was on the bus. ini() stored it via write_hl just
            // after io_read, so the byte at the new HL-1 (i.e. just-written
            // address) is the value. Re-read that.
            let new_hl = cpu.regs.hl().wrapping_sub(1);
            let v = bus.read(new_hl);
            ini_for_b2(cpu, bus, v)
        }
        0xBA => {
            let new_hl = cpu.regs.hl().wrapping_add(1);
            let _ = new_hl;
            ind_for_ba(cpu, bus)
        }
        0xA3 => { outi(cpu, bus); 16 }
        0xAB => { outd(cpu, bus); 16 }
        0xB3 => outi_for_b3(cpu, bus),
        0xBB => outd_for_bb(cpu, bus),

        0x40 | 0x48 | 0x50 | 0x58 | 0x60 | 0x68 | 0x78 => {
            let port = cpu.regs.bc();
            let v = bus.io_read(port);
            let reg = (op >> 3) & 7;
            if reg != 6 { cpu.regs.set_reg8(reg, v); }
            cpu.set_f(sz53p(v) | (cpu.regs.f & FC));
            cpu.regs.wz = port.wrapping_add(1);
            12
        }
        0x70 => {
            let port = cpu.regs.bc();
            let v = bus.io_read(port);
            cpu.set_f(sz53p(v) | (cpu.regs.f & FC));
            cpu.regs.wz = port.wrapping_add(1);
            12
        }
        0x41 | 0x49 | 0x51 | 0x59 | 0x61 | 0x69 | 0x79 => {
            let port = cpu.regs.bc();
            let reg = (op >> 3) & 7;
            let v = cpu.regs.reg8(reg);
            bus.io_write(port, v);
            cpu.regs.wz = port.wrapping_add(1);
            12
        }
        0x71 => {
            let port = cpu.regs.bc();
            bus.io_write(port, 0);
            cpu.regs.wz = port.wrapping_add(1);
            12
        }

        0x6F => {
            let mem = read_hl(cpu, bus);
            let new_mem = (mem << 4) | (cpu.regs.a & 0x0F);
            cpu.regs.a = (cpu.regs.a & 0xF0) | (mem >> 4);
            write_hl(cpu, bus, new_mem);
            cpu.set_f(sz53p(cpu.regs.a) | (cpu.regs.f & FC));
            cpu.regs.wz = cpu.regs.hl().wrapping_add(1);
            18
        }
        0x67 => {
            let mem = read_hl(cpu, bus);
            let new_mem = (mem >> 4) | ((cpu.regs.a & 0x0F) << 4);
            cpu.regs.a = (cpu.regs.a & 0xF0) | (mem & 0x0F);
            write_hl(cpu, bus, new_mem);
            cpu.set_f(sz53p(cpu.regs.a) | (cpu.regs.f & FC));
            cpu.regs.wz = cpu.regs.hl().wrapping_add(1);
            18
        }

        _ => 8,
    }
}

/// For repeating block ops (LDIR/LDDR/CPIR/CPDR/INIR/INDR/OTIR/OTDR)
/// that loop back: the Y and X flags are replaced by bits 5 and 3 of
/// the *high* byte of PC (after PC was backed up by 2). Per Sean Young
/// §4.2 footnote.
#[inline]
fn fixup_yx_from_pc_high(cpu: &mut Cpu) {
    let pc_high = (cpu.regs.pc >> 8) as u8;
    let f = (cpu.regs.f & !(FY | FX)) | (pc_high & (FY | FX));
    cpu.set_f(f);
}

/// MAME's `block_io_interrupted_flags()` — called after INIR/INDR/OTIR/OTDR
/// when the loop continues (B != 0 after the iteration). It overrides YX
/// from PC's high byte AND adjusts P and H based on the value just
/// transferred (`val`) and the new B, exactly as the silicon does
/// (Sean Young §4.4, MAME `z80.cpp:580-604`).
///
/// `val` is the byte that was read from the I/O port (INIR/INDR) or read
/// from (HL) before being output (OTIR/OTDR).
fn block_io_interrupted_flags(cpu: &mut Cpu, val: u8) {
    let pc_high = (cpu.regs.pc >> 8) as u8;
    let mut f = (cpu.regs.f & !(FY | FX)) | (pc_high & (FY | FX));
    let pv_old = f & FP;
    let b = cpu.regs.b;
    let new_pv_low: u8;
    if (f & FC) != 0 {
        // Clear H first; conditionally set below.
        f &= !FH;
        if (val & 0x80) != 0 {
            new_pv_low = b.wrapping_sub(1) & 0x07;
            if (b & 0x0F) == 0x00 { f |= FH; }
        } else {
            new_pv_low = b.wrapping_add(1) & 0x07;
            if (b & 0x0F) == 0x0F { f |= FH; }
        }
    } else {
        new_pv_low = b & 0x07;
    }
    // MAME stores `m_f.pv_val` as a *u8 value* (0..7); reading P returns
    // `PARITY[pv_val] & PF`. The statement
    //   m_f.pv_val = (pv_old ^ m_f.pv()) & PF
    // is therefore equivalent to: P_final = (pv_old == PARITY[new_pv_low] & PF).
    // (parity[0] = PF and parity[PF] = 0, so the inner XOR is a toggle
    // indicator stored in pv_val, and the outer parity inverts it again).
    let new_p_bit = crate::cpu::z80::flags::PARITY[new_pv_low as usize] & FP;
    let p_final = if pv_old == new_p_bit { FP } else { 0 };
    f = (f & !FP) | p_final;
    cpu.set_f(f);
}

fn ldi<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let v = read_hl(cpu, bus);
    bus.write(cpu.regs.de(), v);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1));
    cpu.regs.set_de(cpu.regs.de().wrapping_add(1));
    cpu.regs.set_bc(cpu.regs.bc().wrapping_sub(1));
    let n = cpu.regs.a.wrapping_add(v);
    let mut f = cpu.regs.f & (FS | FZ | FC);
    if cpu.regs.bc() != 0 { f |= FP; }
    f |= n & FX;
    if (n & 0x02) != 0 { f |= FY; }
    cpu.set_f(f);
}
fn ldd<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let v = read_hl(cpu, bus);
    bus.write(cpu.regs.de(), v);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1));
    cpu.regs.set_de(cpu.regs.de().wrapping_sub(1));
    cpu.regs.set_bc(cpu.regs.bc().wrapping_sub(1));
    let n = cpu.regs.a.wrapping_add(v);
    let mut f = cpu.regs.f & (FS | FZ | FC);
    if cpu.regs.bc() != 0 { f |= FP; }
    f |= n & FX;
    if (n & 0x02) != 0 { f |= FY; }
    cpu.set_f(f);
}
fn cpi<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let v = read_hl(cpu, bus);
    let r = cpu.regs.a.wrapping_sub(v);
    let half = ((cpu.regs.a & 0x0F) as i16 - (v & 0x0F) as i16) & 0x10;
    cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1));
    cpu.regs.set_bc(cpu.regs.bc().wrapping_sub(1));
    let mut f = (r & FS) | (cpu.regs.f & FC) | FN;
    if r == 0 { f |= FZ; }
    if half != 0 { f |= FH; }
    if cpu.regs.bc() != 0 { f |= FP; }
    let n = r.wrapping_sub(if half != 0 { 1 } else { 0 });
    f |= n & FX;
    if (n & 0x02) != 0 { f |= FY; }
    cpu.set_f(f);
    cpu.regs.wz = cpu.regs.wz.wrapping_add(1);
}
fn cpd<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let v = read_hl(cpu, bus);
    let r = cpu.regs.a.wrapping_sub(v);
    let half = ((cpu.regs.a & 0x0F) as i16 - (v & 0x0F) as i16) & 0x10;
    cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1));
    cpu.regs.set_bc(cpu.regs.bc().wrapping_sub(1));
    let mut f = (r & FS) | (cpu.regs.f & FC) | FN;
    if r == 0 { f |= FZ; }
    if half != 0 { f |= FH; }
    if cpu.regs.bc() != 0 { f |= FP; }
    let n = r.wrapping_sub(if half != 0 { 1 } else { 0 });
    f |= n & FX;
    if (n & 0x02) != 0 { f |= FY; }
    cpu.set_f(f);
    cpu.regs.wz = cpu.regs.wz.wrapping_sub(1);
}
// INI / IND / OUTI / OUTD flag computation per Sean Young
// "Undocumented Z80 Documented" §4.4:
//
//   k = port_byte + ((C ± 1) & 0xFF)              [INI/IND]
//   k = port_byte + L                              [OUTI/OUTD, L = post-modified]
//   H = C = (k > 0xFF)
//   N = bit 7 of port_byte
//   P = parity( (k & 7) XOR B' )                   [B' = decremented B]
//   S,Z,Y,X = sz5x3(B')

fn ini<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let port = cpu.regs.bc();
    let v = bus.io_read(port);
    write_hl(cpu, bus, v);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1));
    cpu.regs.wz = port.wrapping_add(1);
    let k = v as u16 + ((cpu.regs.c.wrapping_add(1)) as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
}
fn ind<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let port = cpu.regs.bc();
    let v = bus.io_read(port);
    write_hl(cpu, bus, v);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1));
    cpu.regs.wz = port.wrapping_sub(1);
    let k = v as u16 + ((cpu.regs.c.wrapping_sub(1)) as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
}
fn outi<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let v = read_hl(cpu, bus);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    let port = cpu.regs.bc();
    bus.io_write(port, v);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1));
    cpu.regs.wz = port.wrapping_add(1);
    let k = v as u16 + (cpu.regs.l as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
}
fn outd<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) {
    let v = read_hl(cpu, bus);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    let port = cpu.regs.bc();
    bus.io_write(port, v);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1));
    cpu.regs.wz = port.wrapping_sub(1);
    let k = v as u16 + (cpu.regs.l as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
}

// ---------- repeating I/O block ops (INIR/INDR/OTIR/OTDR) ----------
//
// We run the single-step op first, then if B != 0 we apply
// `block_io_interrupted_flags()` per MAME `z80.cpp:580`.

fn ini_for_b2<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, val: u8) -> u32 {
    // The single-step ini() already happened on the path that called us.
    // But the call site has not run ini yet. Redo the I/O sequence here
    // to keep the byte read available for the fixup.
    let _ = val;
    let port = cpu.regs.bc();
    let v = bus.io_read(port);
    write_hl(cpu, bus, v);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1));
    cpu.regs.wz = port.wrapping_add(1);
    let k = v as u16 + ((cpu.regs.c.wrapping_add(1)) as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
    if cpu.regs.b != 0 {
        cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
        cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
        block_io_interrupted_flags(cpu, v);
        21
    } else { 16 }
}

fn ind_for_ba<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    let port = cpu.regs.bc();
    let v = bus.io_read(port);
    write_hl(cpu, bus, v);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1));
    cpu.regs.wz = port.wrapping_sub(1);
    let k = v as u16 + ((cpu.regs.c.wrapping_sub(1)) as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
    if cpu.regs.b != 0 {
        cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
        cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
        block_io_interrupted_flags(cpu, v);
        21
    } else { 16 }
}

fn outi_for_b3<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    let v = read_hl(cpu, bus);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    let port = cpu.regs.bc();
    bus.io_write(port, v);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_add(1));
    cpu.regs.wz = port.wrapping_add(1);
    let k = v as u16 + (cpu.regs.l as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
    if cpu.regs.b != 0 {
        cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
        cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
        block_io_interrupted_flags(cpu, v);
        21
    } else { 16 }
}

fn outd_for_bb<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B) -> u32 {
    let v = read_hl(cpu, bus);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    let port = cpu.regs.bc();
    bus.io_write(port, v);
    cpu.regs.set_hl(cpu.regs.hl().wrapping_sub(1));
    cpu.regs.wz = port.wrapping_sub(1);
    let k = v as u16 + (cpu.regs.l as u16);
    let mut f = sz53(cpu.regs.b);
    if (v & 0x80) != 0 { f |= FN; }
    if k > 0xFF { f |= FH | FC; }
    f |= crate::cpu::z80::flags::PARITY[(((k & 7) as u8) ^ cpu.regs.b) as usize];
    cpu.set_f(f);
    if cpu.regs.b != 0 {
        cpu.regs.pc = cpu.regs.pc.wrapping_sub(2);
        cpu.regs.wz = cpu.regs.pc.wrapping_add(1);
        block_io_interrupted_flags(cpu, v);
        21
    } else { 16 }
}

// ---------- DD / FD prefix ----------

fn exec_index<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, ix: bool) -> u32 {
    // DD/FD prefix M1 doesn't touch F — reset Q (Sean Young §6).
    // This is what makes `DD 37 0001` (SCF after DD with Q≠0 at entry)
    // produce YX from (A | F) instead of A alone.
    cpu.regs.q = 0;
    let op = cpu.fetch_op(bus);
    let cur = if ix { cpu.regs.ix } else { cpu.regs.iy };
    let set_index = |cpu: &mut Cpu, v: u16| { if ix { cpu.regs.ix = v; } else { cpu.regs.iy = v; } };

    match op {
        0x09 => { let r = add16(cpu, cur, cpu.regs.bc()); set_index(cpu, r); 15 }
        0x19 => { let r = add16(cpu, cur, cpu.regs.de()); set_index(cpu, r); 15 }
        0x29 => { let r = add16(cpu, cur, cur);            set_index(cpu, r); 15 }
        0x39 => { let r = add16(cpu, cur, cpu.regs.sp);    set_index(cpu, r); 15 }

        0x21 => { let v = cpu.read_op16(bus); set_index(cpu, v); 14 }
        0x22 => {
            let addr = cpu.read_op16(bus);
            bus.write(addr, cur as u8);
            bus.write(addr.wrapping_add(1), (cur >> 8) as u8);
            cpu.regs.wz = addr.wrapping_add(1);
            20
        }
        0x2A => {
            let addr = cpu.read_op16(bus);
            let lo = bus.read(addr);
            let hi = bus.read(addr.wrapping_add(1));
            set_index(cpu, u16::from_le_bytes([lo, hi]));
            cpu.regs.wz = addr.wrapping_add(1);
            20
        }
        0x23 => { set_index(cpu, cur.wrapping_add(1)); 10 }
        0x2B => { set_index(cpu, cur.wrapping_sub(1)); 10 }

        0x34 => {
            let d = cpu.read_op(bus) as i8;
            let addr = cur.wrapping_add(d as i16 as u16);
            cpu.regs.wz = addr;
            let v = bus.read(addr);
            let r = inc8(cpu, v);
            bus.write(addr, r);
            23
        }
        0x35 => {
            let d = cpu.read_op(bus) as i8;
            let addr = cur.wrapping_add(d as i16 as u16);
            cpu.regs.wz = addr;
            let v = bus.read(addr);
            let r = dec8(cpu, v);
            bus.write(addr, r);
            23
        }
        0x36 => {
            let d = cpu.read_op(bus) as i8;
            let n = cpu.read_op(bus);
            let addr = cur.wrapping_add(d as i16 as u16);
            cpu.regs.wz = addr;
            bus.write(addr, n);
            19
        }
        0x46 | 0x4E | 0x56 | 0x5E | 0x66 | 0x6E | 0x7E => {
            let dst = (op >> 3) & 7;
            let d = cpu.read_op(bus) as i8;
            let addr = cur.wrapping_add(d as i16 as u16);
            cpu.regs.wz = addr;
            let v = bus.read(addr);
            cpu.regs.set_reg8(dst, v);
            19
        }
        0x70..=0x77 if op != 0x76 => {
            let src = op & 7;
            let d = cpu.read_op(bus) as i8;
            let addr = cur.wrapping_add(d as i16 as u16);
            cpu.regs.wz = addr;
            let v = cpu.regs.reg8(src);
            bus.write(addr, v);
            19
        }
        0x86 | 0x8E | 0x96 | 0x9E | 0xA6 | 0xAE | 0xB6 | 0xBE => {
            let d = cpu.read_op(bus) as i8;
            let addr = cur.wrapping_add(d as i16 as u16);
            cpu.regs.wz = addr;
            let v = bus.read(addr);
            match (op >> 3) & 7 {
                0 => { let r = add8(cpu, cpu.regs.a, v, 0); cpu.regs.a = r; }
                1 => { let c = cpu.regs.f & FC; let r = add8(cpu, cpu.regs.a, v, c); cpu.regs.a = r; }
                2 => { let r = sub8(cpu, cpu.regs.a, v, 0); cpu.regs.a = r; }
                3 => { let c = cpu.regs.f & FC; let r = sub8(cpu, cpu.regs.a, v, c); cpu.regs.a = r; }
                4 => { let r = and8(cpu, cpu.regs.a, v); cpu.regs.a = r; }
                5 => { let r = xor8(cpu, cpu.regs.a, v); cpu.regs.a = r; }
                6 => { let r = or8(cpu, cpu.regs.a, v);  cpu.regs.a = r; }
                7 => cp8(cpu, cpu.regs.a, v),
                _ => unreachable!(),
            }
            19
        }
        0xE1 => { let v = cpu.pop16(bus); set_index(cpu, v); 14 }
        0xE5 => { cpu.push16(bus, cur); 15 }
        0xE9 => { cpu.regs.pc = cur; 8 }
        0xF9 => { cpu.regs.sp = cur; 10 }
        0xE3 => {
            let lo = bus.read(cpu.regs.sp);
            let hi = bus.read(cpu.regs.sp.wrapping_add(1));
            bus.write(cpu.regs.sp, cur as u8);
            bus.write(cpu.regs.sp.wrapping_add(1), (cur >> 8) as u8);
            let v = u16::from_le_bytes([lo, hi]);
            set_index(cpu, v);
            cpu.regs.wz = v;
            23
        }
        0xCB => exec_index_cb(cpu, bus, ix),
        // For all other opcodes, the DD/FD prefix replaces H/L (reg codes
        // 4 and 5) with IXH/IXL (or IYH/IYL). Importantly, reg code 6 is
        // *not* (IX+d) here — those forms are handled explicitly above.
        // We re-implement the affected opcodes (`LD r,r'`, `INC r`,
        // `DEC r`, `LD r,n`, `ALU A,r`) with index-aware accessors.
        _ => exec_index_other(cpu, bus, op, ix),
    }
}

#[inline]
fn idx_high(cpu: &Cpu, ix: bool) -> u8 {
    if ix { (cpu.regs.ix >> 8) as u8 } else { (cpu.regs.iy >> 8) as u8 }
}
#[inline]
fn idx_low(cpu: &Cpu, ix: bool) -> u8 {
    if ix { cpu.regs.ix as u8 } else { cpu.regs.iy as u8 }
}
#[inline]
fn set_idx_high(cpu: &mut Cpu, ix: bool, v: u8) {
    if ix { cpu.regs.ix = (cpu.regs.ix & 0x00FF) | ((v as u16) << 8); }
    else  { cpu.regs.iy = (cpu.regs.iy & 0x00FF) | ((v as u16) << 8); }
}
#[inline]
fn set_idx_low(cpu: &mut Cpu, ix: bool, v: u8) {
    if ix { cpu.regs.ix = (cpu.regs.ix & 0xFF00) | (v as u16); }
    else  { cpu.regs.iy = (cpu.regs.iy & 0xFF00) | (v as u16); }
}

/// Read register code under DD/FD prefix. Code 4 = IXH, 5 = IXL; codes
/// 0-3 and 7 are unchanged (B,C,D,E,A). Code 6 is reserved for (IX+d)
/// and must not reach this helper.
#[inline]
fn read_r8_idx(cpu: &Cpu, code: u8, ix: bool) -> u8 {
    match code & 7 {
        4 => idx_high(cpu, ix),
        5 => idx_low(cpu, ix),
        c => cpu.regs.reg8(c),
    }
}
#[inline]
fn write_r8_idx(cpu: &mut Cpu, code: u8, v: u8, ix: bool) {
    match code & 7 {
        4 => set_idx_high(cpu, ix, v),
        5 => set_idx_low(cpu, ix, v),
        c => cpu.regs.set_reg8(c, v),
    }
}

/// Handle all the DD/FD-prefixed opcodes that the explicit table above
/// didn't catch — these are the LD/INC/DEC/ALU forms where H/L are
/// replaced by IXH/IXL.
fn exec_index_other<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, op: u8, ix: bool) -> u32 {
    match op {
        // INC IXH/IXL  (0x24 = INC H, 0x2C = INC L)
        0x24 => {
            let v = idx_high(cpu, ix);
            let r = inc8(cpu, v);
            set_idx_high(cpu, ix, r);
            8
        }
        0x2C => {
            let v = idx_low(cpu, ix);
            let r = inc8(cpu, v);
            set_idx_low(cpu, ix, r);
            8
        }
        0x25 => {
            let v = idx_high(cpu, ix);
            let r = dec8(cpu, v);
            set_idx_high(cpu, ix, r);
            8
        }
        0x2D => {
            let v = idx_low(cpu, ix);
            let r = dec8(cpu, v);
            set_idx_low(cpu, ix, r);
            8
        }
        // LD IXH, n / LD IXL, n
        0x26 => { let n = cpu.read_op(bus); set_idx_high(cpu, ix, n); 11 }
        0x2E => { let n = cpu.read_op(bus); set_idx_low (cpu, ix, n); 11 }
        // LD r, r' — 0x40..0x7F, but exclude (HL) forms and HALT.
        // 0x76 is HALT (no register pair has it as src/dst with reg=6
        // in this block).
        0x40..=0x7F if op != 0x76 => {
            let dst = (op >> 3) & 7;
            let src = op & 7;
            // Special case: when the destination OR source is (HL)
            // (code 6), it gets *substituted* by (IX+d) and H/L are
            // NOT replaced for the *other* operand. Those forms are
            // already handled in the explicit table above (0x46, 0x4E,
            // ..., 0x70..0x77). They shouldn't fall through here, but
            // be defensive.
            if dst == 6 || src == 6 {
                return decode_main(cpu, bus, op);
            }
            let v = read_r8_idx(cpu, src, ix);
            write_r8_idx(cpu, dst, v, ix);
            8
        }
        // ALU A, IXH/IXL  (0x80..0xBF, src = 4 or 5 only — other srcs
        // would be normal A,B,C,D,E,A; the 0x?6/0x?E forms are (IX+d)
        // and were handled above).
        0x80..=0xBF if (op & 7) != 6 => {
            let src = op & 7;
            // For src=0..3 or 7, IX prefix has no effect (those are
            // normal B,C,D,E,A); MAME still charges an extra 4 cycles
            // for the prefix though, so total = 8.
            let v = read_r8_idx(cpu, src, ix);
            match (op >> 3) & 7 {
                0 => { let r = add8(cpu, cpu.regs.a, v, 0); cpu.regs.a = r; }
                1 => { let c = cpu.regs.f & FC; let r = add8(cpu, cpu.regs.a, v, c); cpu.regs.a = r; }
                2 => { let r = sub8(cpu, cpu.regs.a, v, 0); cpu.regs.a = r; }
                3 => { let c = cpu.regs.f & FC; let r = sub8(cpu, cpu.regs.a, v, c); cpu.regs.a = r; }
                4 => { let r = and8(cpu, cpu.regs.a, v); cpu.regs.a = r; }
                5 => { let r = xor8(cpu, cpu.regs.a, v); cpu.regs.a = r; }
                6 => { let r = or8(cpu, cpu.regs.a, v);  cpu.regs.a = r; }
                7 => cp8(cpu, cpu.regs.a, v),
                _ => unreachable!(),
            }
            8
        }
        // Anything else: the prefix has no effect on this opcode —
        // re-execute it as a normal main-table instruction. This matches
        // MAME's behaviour for "noeffect" DD prefixes.
        _ => decode_main(cpu, bus, op),
    }
}

fn exec_index_cb<B: Z80Bus>(cpu: &mut Cpu, bus: &mut B, ix: bool) -> u32 {
    let d = cpu.read_op(bus) as i8;
    let op = cpu.read_op(bus);
    let base = if ix { cpu.regs.ix } else { cpu.regs.iy };
    let addr = base.wrapping_add(d as i16 as u16);
    cpu.regs.wz = addr;
    let v = bus.read(addr);
    let reg = op & 7;
    let bit = (op >> 3) & 7;
    let (new_v, store) = match op {
        0x00..=0x07 => (rlc(cpu, v), true),
        0x08..=0x0F => (rrc(cpu, v), true),
        0x10..=0x17 => (rl (cpu, v), true),
        0x18..=0x1F => (rr (cpu, v), true),
        0x20..=0x27 => (sla(cpu, v), true),
        0x28..=0x2F => (sra(cpu, v), true),
        0x30..=0x37 => (sll(cpu, v), true),
        0x38..=0x3F => (srl(cpu, v), true),
        0x40..=0x7F => {
            let mask = 1u8 << bit;
            let r = v & mask;
            let mut f = (cpu.regs.f & FC) | FH;
            if r == 0 { f |= FZ | FP; }
            if (r & FS) != 0 { f |= FS; }
            f |= ((cpu.regs.wz >> 8) as u8) & (FY | FX);
            cpu.set_f(f);
            return 20;
        }
        0x80..=0xBF => (v & !(1u8 << bit), true),
        0xC0..=0xFF => (v | (1u8 << bit), true),
    };
    if store {
        bus.write(addr, new_v);
        if reg != 6 {
            cpu.regs.set_reg8(reg, new_v);
        }
    }
    23
}

//! Effective Address computation for the 68000.

use crate::cpu::m68k::bus::Bus;
use crate::cpu::m68k::cpu::{Cpu, Size};

#[derive(Debug, Clone, Copy)]
pub enum Ea {
    DataReg(u8),
    AddrReg(u8),
    AddrInd(u8),
    AddrIndPost(u8),
    AddrIndPre(u8),
    AddrIndDisp(u8, i16),
    AddrIndIdx(u8, i8, u8, bool, Size),
    AbsShort(u32),
    AbsLong(u32),
    PcIndDisp(u32),
    PcIndIdx(u32, i8, u8, bool, Size),
    Immediate(u32),
}

impl Ea {
    pub fn decode<B: Bus>(cpu: &mut Cpu, bus: &mut B, ea_bits: u16, size: Size) -> Self {
        let mode = ((ea_bits >> 3) & 0x7) as u8;
        let reg = (ea_bits & 0x7) as u8;
        match mode {
            0 => Ea::DataReg(reg),
            1 => Ea::AddrReg(reg),
            2 => Ea::AddrInd(reg),
            3 => Ea::AddrIndPost(reg),
            4 => Ea::AddrIndPre(reg),
            5 => {
                let disp = cpu.fetch16(bus) as i16;
                Ea::AddrIndDisp(reg, disp)
            }
            6 => {
                let ext = cpu.fetch16(bus);
                let disp = (ext & 0xFF) as i8;
                let xreg = ((ext >> 12) & 0x7) as u8;
                let is_addr = (ext & 0x8000) != 0;
                let xs = if (ext & 0x0800) != 0 { Size::Long } else { Size::Word };
                Ea::AddrIndIdx(reg, disp, xreg, is_addr, xs)
            }
            7 => match reg {
                0 => Ea::AbsShort((cpu.fetch16(bus) as i16) as u32),
                1 => Ea::AbsLong(cpu.fetch32(bus)),
                2 => {
                    let pc = cpu.pc;
                    let disp = cpu.fetch16(bus) as i16;
                    Ea::PcIndDisp(pc.wrapping_add(disp as u32))
                }
                3 => {
                    let pc = cpu.pc;
                    let ext = cpu.fetch16(bus);
                    let disp = (ext & 0xFF) as i8;
                    let xreg = ((ext >> 12) & 0x7) as u8;
                    let is_addr = (ext & 0x8000) != 0;
                    let xs = if (ext & 0x0800) != 0 { Size::Long } else { Size::Word };
                    Ea::PcIndIdx(pc, disp, xreg, is_addr, xs)
                }
                4 => {
                    let value = match size {
                        Size::Byte => u32::from(cpu.fetch16(bus) & 0xFF),
                        Size::Word => u32::from(cpu.fetch16(bus)),
                        Size::Long => cpu.fetch32(bus),
                    };
                    Ea::Immediate(value)
                }
                _ => Ea::Immediate(0),
            },
            _ => unreachable!(),
        }
    }

    pub fn is_reg_direct(self) -> bool {
        matches!(self, Ea::DataReg(_) | Ea::AddrReg(_))
    }
    pub fn addr_reg(self) -> Option<u8> {
        match self {
            Ea::AddrReg(r) => Some(r),
            _ => None,
        }
    }

    /// Roll back the post-increment side effect of an `(An)+` EA when a
    /// bus access faults. MAME's bus-error microcode commits the An
    /// update at different points depending on the access width:
    ///
    /// - For Byte and Word, the increment is latched as part of the
    ///   single-cycle bus access. A fault therefore leaves An at the
    ///   post-incremented value.
    /// - For Long, the bus access is split into two 16-bit cycles
    ///   (high then low). A fault in the FIRST half happens before
    ///   the increment is latched, so An stays at the original value.
    ///
    /// PRE-decrement (`-(An)`) is always committed early and is never
    /// rolled back here.
    pub fn undo_side_effects(self, cpu: &mut Cpu, size: Size) {
        if let Ea::AddrIndPost(r) = self {
            if size == Size::Long {
                let inc = size.bytes();
                cpu.a[r as usize] = cpu.a[r as usize].wrapping_sub(inc);
            }
        }
    }

    /// Compute the address. Applies post-inc / pre-dec side effects ONCE.
    pub fn compute_address(self, cpu: &mut Cpu, size: Size) -> Option<u32> {
        match self {
            Ea::AddrInd(r) => Some(cpu.a[r as usize]),
            Ea::AddrIndPost(r) => {
                let a = cpu.a[r as usize];
                let inc = if r == 7 && size == Size::Byte { 2 } else { size.bytes() };
                cpu.a[r as usize] = a.wrapping_add(inc);
                Some(a)
            }
            Ea::AddrIndPre(r) => {
                let dec = if r == 7 && size == Size::Byte { 2 } else { size.bytes() };
                let a = cpu.a[r as usize].wrapping_sub(dec);
                cpu.a[r as usize] = a;
                Some(a)
            }
            Ea::AddrIndDisp(r, d) => Some(cpu.a[r as usize].wrapping_add(d as u32)),
            Ea::AddrIndIdx(r, d, xn, is_addr, xs) => {
                let base = cpu.a[r as usize];
                let idx_raw = if is_addr { cpu.a[xn as usize] } else { cpu.d[xn as usize] };
                let idx = match xs {
                    Size::Word => (idx_raw as i16) as i32 as u32,
                    _ => idx_raw,
                };
                Some(base.wrapping_add(d as u32).wrapping_add(idx))
            }
            Ea::AbsShort(a) | Ea::AbsLong(a) | Ea::PcIndDisp(a) => Some(a),
            Ea::PcIndIdx(pc, d, xn, is_addr, xs) => {
                let idx_raw = if is_addr { cpu.a[xn as usize] } else { cpu.d[xn as usize] };
                let idx = match xs {
                    Size::Word => (idx_raw as i16) as i32 as u32,
                    _ => idx_raw,
                };
                Some(pc.wrapping_add(d as u32).wrapping_add(idx))
            }
            _ => None,
        }
    }

    pub fn read<B: Bus>(self, cpu: &mut Cpu, bus: &mut B, size: Size) -> u32 {
        match self {
            Ea::DataReg(r) => cpu.d[r as usize] & size.mask(),
            Ea::AddrReg(r) => {
                let v = cpu.a[r as usize];
                match size {
                    Size::Word => (v as i16) as i32 as u32,
                    _ => v & size.mask(),
                }
            }
            Ea::Immediate(v) => v & size.mask(),
            _ => {
                let addr = self.compute_address(cpu, size).expect("memory EA");
                // PRE-decrement source EA: MAME's pdcw1 microcode does
                // `m_pc = m_au` BEFORE the read, so the exception
                // frame's pushed PC is ipc+4. POST-inc keeps m_pc at
                // ipc+2. Long-size predec splits into two reads and
                // the m_pc bump happens between them, so keep ipc+2
                // for size==Long.
                if size != Size::Long && matches!(self, Ea::AddrIndPre(_)) {
                    cpu.au = cpu.instr_pc.wrapping_add(4);
                }
                // For absolute-addressing source EAs, MAME's microcode
                // bumps `m_pc` to past the last extension word read
                // (via implicit `m_pc = m_au` in the EA-fetch microop).
                // The pushed-PC therefore reflects ipc + opcode_size +
                // ext_words. We approximate this by setting cpu.au.
                match self {
                    Ea::AbsShort(_) => {
                        cpu.au = cpu.instr_pc.wrapping_add(4);
                    }
                    Ea::AbsLong(_) => {
                        cpu.au = cpu.instr_pc.wrapping_add(6);
                    }
                    _ => {}
                }
                let v = read_at(cpu, bus, addr, size);
                if let Some(info) = cpu.address_error.as_mut() {
                    // PC-relative source EAs access the PROGRAM space.
                    // Reflect that in the SSW (instruction = true).
                    if matches!(self, Ea::PcIndDisp(_) | Ea::PcIndIdx(..)) {
                        info.instruction = true;
                        info.fc = if cpu.sr.supervisor() { 6 } else { 2 };
                    }
                }
                if cpu.address_error.is_some() {
                    // Read-side fault: post-inc hasn't yet committed in
                    // MAME's microcode, so roll it back.
                    self.undo_side_effects(cpu, size);
                }
                v
            }
        }
    }

    pub fn write<B: Bus>(self, cpu: &mut Cpu, bus: &mut B, size: Size, value: u32) {
        match self {
            Ea::DataReg(r) => {
                let cur = cpu.d[r as usize];
                let mask = size.mask();
                cpu.d[r as usize] = (cur & !mask) | (value & mask);
            }
            Ea::AddrReg(r) => {
                let v = match size {
                    Size::Word => (value as i16) as i32 as u32,
                    _ => value,
                };
                cpu.a[r as usize] = v;
            }
            Ea::Immediate(_) | Ea::PcIndDisp(_) | Ea::PcIndIdx(..) => {}
            _ => {
                // MAME's microcode for Byte/Word writes via `-(An)`
                // performs a prefetch of the NEXT opcode word BEFORE the
                // first write (`mmmw1` sets `m_ir = m_irc`, then `mmmw2`
                // copies `m_ird = m_ir` and writes). On a write fault,
                // bser5 pushes `m_ird` -- now the prefetched word -- as
                // the IR field of the frame.
                //
                // Long predec is DIFFERENT: `rmml1` prefetches into
                // `m_ir` but `rmml2` writes the LOW word at An-2
                // WITHOUT first updating `m_ird`. Only the second write
                // (`mmmw2` at An-4) does `m_ird = m_ir`. So on a fault
                // at An-2 the pushed IR is still the ORIGINAL opcode.
                // Long predec is handled by its dedicated path below;
                // we deliberately skip the IR update for Long here.
                if matches!(self, Ea::AddrIndPre(_)) && size != Size::Long {
                    cpu.ir = bus.read16(cpu.pc);
                }
                // Special path: `-(An)` with Long size. MAME's microcode
                // (`rmml1`/`rmml2`/`mmmw2`) writes the LOW word at An-2
                // first, then the HIGH word at An-4, and commits
                // `m_da[rx] = An-4` only when the second write is
                // dispatched. So on a first-write fault An stays at its
                // original value, and the fault address is An-2.
                if let (Ea::AddrIndPre(r), Size::Long) = (self, size) {
                    let orig = cpu.a[r as usize];
                    let low_addr = orig.wrapping_sub(2);
                    let high_addr = orig.wrapping_sub(4);
                    let low_word = value & 0xFFFF;
                    let high_word = (value >> 16) & 0xFFFF;
                    // Don't commit An yet.
                    write_at(cpu, bus, low_addr, Size::Word, low_word);
                    if cpu.address_error.is_some() {
                        return; // An unchanged
                    }
                    // Commit An = An-4 before second write.
                    cpu.a[r as usize] = high_addr;
                    write_at(cpu, bus, high_addr, Size::Word, high_word);
                    return;
                }
                let addr = self.compute_address(cpu, size).expect("memory EA");
                write_at(cpu, bus, addr, size, value);
                // For (An)+ destination, MAME's microcode commits the
                // post-increment AFTER the write completes successfully
                // (line `m_da[rx] = m_au` in mmiw2 follows the write).
                // So a write-fault must undo the optimistic post-inc.
                if cpu.address_error.is_some() {
                    if let Ea::AddrIndPost(r) = self {
                        let inc = if r == 7 && size == Size::Byte { 2 } else { size.bytes() };
                        cpu.a[r as usize] = cpu.a[r as usize].wrapping_sub(inc);
                    }
                }
            }
        }
    }

    /// Read-Modify-Write: compute address ONCE, read, modify, write back.
    pub fn modify<B: Bus, F>(self, cpu: &mut Cpu, bus: &mut B, size: Size, f: F) -> u32
    where
        F: FnOnce(&mut Cpu, u32) -> u32,
    {
        match self {
            Ea::DataReg(r) => {
                let v = cpu.d[r as usize] & size.mask();
                let new = f(cpu, v);
                let mask = size.mask();
                cpu.d[r as usize] = (cpu.d[r as usize] & !mask) | (new & mask);
                new
            }
            Ea::AddrReg(r) => {
                let v = cpu.a[r as usize] & size.mask();
                let new = f(cpu, v);
                let extended = match size {
                    Size::Word => (new as i16) as i32 as u32,
                    _ => new,
                };
                cpu.a[r as usize] = extended;
                new
            }
            Ea::Immediate(_) | Ea::PcIndDisp(_) | Ea::PcIndIdx(..) => {
                let v = self.read(cpu, bus, size);
                f(cpu, v)
            }
            _ => {
                let addr = self.compute_address(cpu, size).expect("memory EA");
                // For B/W RMW with PRE-decrement EA, MAME's microcode
                // (pdcw1) performs `m_pc = m_au` BEFORE the read fires,
                // so the address-error frame's pushed PC is ipc+4.
                if size != Size::Long && matches!(self, Ea::AddrIndPre(_)) {
                    cpu.au = cpu.instr_pc.wrapping_add(4);
                }
                // For absolute-addressing EAs, MAME's microcode bumps
                // m_pc to past the consumed extension words before the
                // RMW data access. Mirror that so the exception frame
                // pushes the correct PC.
                match self {
                    Ea::AbsShort(_) => {
                        cpu.au = cpu.instr_pc.wrapping_add(4);
                    }
                    Ea::AbsLong(_) => {
                        cpu.au = cpu.instr_pc.wrapping_add(6);
                    }
                    _ => {}
                }
                let v = read_at(cpu, bus, addr, size);
                if cpu.address_error.is_some() {
                    self.undo_side_effects(cpu, size);
                    return 0;
                }
                let pre_ccr = cpu.sr.0 & 0x001F;
                let new = f(cpu, v);
                write_at(cpu, bus, addr, size, new);
                if cpu.address_error.is_some() {
                    cpu.sr.0 = (cpu.sr.0 & 0xFFE0) | pre_ccr;
                }
                new
            }
        }
    }

    pub fn cycles(self, size: Size) -> u32 {
        let long = size == Size::Long;
        match self {
            Ea::DataReg(_) | Ea::AddrReg(_) | Ea::Immediate(_) => 0,
            Ea::AddrInd(_) | Ea::AddrIndPost(_) => if long { 8 } else { 4 },
            Ea::AddrIndPre(_) => if long { 10 } else { 6 },
            Ea::AddrIndDisp(..) | Ea::AbsShort(_) | Ea::PcIndDisp(_) => if long { 12 } else { 8 },
            Ea::AddrIndIdx(..) | Ea::PcIndIdx(..) => if long { 14 } else { 10 },
            Ea::AbsLong(_) => if long { 16 } else { 12 },
        }
    }
}

#[inline]
pub fn read_at<B: Bus>(cpu: &mut Cpu, bus: &mut B, addr: u32, size: Size) -> u32 {
    if size != Size::Byte && (addr & 1) != 0 {
        cpu.address_error = Some(crate::cpu::m68k::cpu::AddressErrorInfo {
            access_addr: addr,
            read: true,
            fc: if cpu.sr.supervisor() { 5
        } else { 1 },
            instruction: false,
            from_stack_op: false,
        });
        // Leave cpu.au alone: bser1 captures m_au := m_pc, and m_pc is
        // the *opcode-stream* prefetch position which does not advance
        // on data reads. step() initialises cpu.au = ipc+2 already.
        // Callers that know microcode bumps m_pc (e.g. CMPM, ADDX/SUBX
        // predec-read, the predec branch in ADDX/SUBX) set cpu.au
        // explicitly before invoking read_at.
        return 0;
    }
    match size {
        Size::Byte => u32::from(bus.read8(addr)),
        Size::Word => u32::from(bus.read16(addr)),
        Size::Long => bus.read32(addr),
    }
}

#[inline]
pub fn write_at<B: Bus>(cpu: &mut Cpu, bus: &mut B, addr: u32, size: Size, value: u32) {
    if size != Size::Byte && (addr & 1) != 0 {
        cpu.address_error = Some(crate::cpu::m68k::cpu::AddressErrorInfo {
            access_addr: addr,
            // MAME treats RMW-style faults as reads when the instruction
            // performs an internal dummy read (CLR/NEG/NEGX/NOT/MOVEM->m).
            // For pure stores (MOVE.x to memory) keep read=false.
            read: false,
            fc: if cpu.sr.supervisor() { 5
        } else { 1 },
            instruction: false,
            from_stack_op: false,
        });
        // NOTE: callers are responsible for pre-setting cpu.au to the
        // value MAME's microcode would have committed to m_pc just
        // before the write. Default is ipc+4 (step() initialisation
        // sets it to ipc+2; if no explicit bump happened we leave it).
        return;
    }
    match size {
        Size::Byte => bus.write8(addr, value as u8),
        Size::Word => bus.write16(addr, value as u16),
        Size::Long => bus.write32(addr, value),
    }
}

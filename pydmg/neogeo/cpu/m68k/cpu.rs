//! Core CPU state for the Motorola 68000.
//!
//! Full ISA implementation targeting SingleStepTests/m68000 conformance.

use crate::cpu::m68k::bus::Bus;

/// Operand size for an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    pub fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }
    pub fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }
    pub fn sign_bit(self) -> u32 {
        match self {
            Size::Byte => 0x80,
            Size::Word => 0x8000,
            Size::Long => 0x8000_0000,
        }
    }
    pub fn bits(self) -> u32 {
        match self {
            Size::Byte => 8,
            Size::Word => 16,
            Size::Long => 32,
        }
    }
}

/// Condition codes for `Bcc`, `Scc`, `DBcc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Condition {
    True = 0,
    False = 1,
    Hi = 2,
    Ls = 3,
    Cc = 4,
    Cs = 5,
    Ne = 6,
    Eq = 7,
    Vc = 8,
    Vs = 9,
    Pl = 10,
    Mi = 11,
    Ge = 12,
    Lt = 13,
    Gt = 14,
    Le = 15,
}

impl Condition {
    pub fn from_bits(b: u16) -> Self {
        match b & 0xF {
            0 => Self::True,
            1 => Self::False,
            2 => Self::Hi,
            3 => Self::Ls,
            4 => Self::Cc,
            5 => Self::Cs,
            6 => Self::Ne,
            7 => Self::Eq,
            8 => Self::Vc,
            9 => Self::Vs,
            10 => Self::Pl,
            11 => Self::Mi,
            12 => Self::Ge,
            13 => Self::Lt,
            14 => Self::Gt,
            _ => Self::Le,
        }
    }
}

/// 68000 Status Register.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusRegister(pub u16);

impl StatusRegister {
    pub const C: u16 = 1 << 0;
    pub const V: u16 = 1 << 1;
    pub const Z: u16 = 1 << 2;
    pub const N: u16 = 1 << 3;
    pub const X: u16 = 1 << 4;
    pub const S: u16 = 1 << 13;
    pub const T: u16 = 1 << 15;

    pub fn set(&mut self, mask: u16, on: bool) {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
    }
    pub fn get(self, mask: u16) -> bool {
        (self.0 & mask) != 0
    }
    pub fn supervisor(self) -> bool {
        self.get(Self::S)
    }
    pub fn interrupt_mask(self) -> u8 {
        ((self.0 >> 8) & 0x7) as u8
    }
    pub fn set_interrupt_mask(&mut self, level: u8) {
        self.0 = (self.0 & !0x0700) | ((u16::from(level) & 0x7) << 8);
    }

    pub fn set_nz(&mut self, value: u32, size: Size) {
        let masked = value & size.mask();
        self.set(Self::Z, masked == 0);
        self.set(Self::N, masked & size.sign_bit() != 0);
    }

    pub fn ccr(self) -> u8 {
        (self.0 & 0x1F) as u8
    }

    pub fn evaluate(self, cond: Condition) -> bool {
        let c = self.get(Self::C);
        let v = self.get(Self::V);
        let z = self.get(Self::Z);
        let n = self.get(Self::N);
        match cond {
            Condition::True => true,
            Condition::False => false,
            Condition::Hi => !c && !z,
            Condition::Ls => c || z,
            Condition::Cc => !c,
            Condition::Cs => c,
            Condition::Ne => !z,
            Condition::Eq => z,
            Condition::Vc => !v,
            Condition::Vs => v,
            Condition::Pl => !n,
            Condition::Mi => n,
            Condition::Ge => (n && v) || (!n && !v),
            Condition::Lt => (n && !v) || (!n && v),
            Condition::Gt => (!z) && ((n && v) || (!n && !v)),
            Condition::Le => z || (n && !v) || (!n && v),
        }
    }
}

/// Exception vector numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    BusError = 2,
    AddressError = 3,
    IllegalInstruction = 4,
    DivideByZero = 5,
    Chk = 6,
    TrapV = 7,
    PrivilegeViolation = 8,
    Trace = 9,
    LineA = 10,
    LineF = 11,
    Spurious = 24,
    Interrupt1 = 25,
    Interrupt2 = 26,
    Interrupt3 = 27,
    Trap0 = 32,
}

impl Exception {
    pub fn vector_offset(self) -> u32 {
        u32::from(self as u8) * 4
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AddressErrorInfo {
    pub access_addr: u32,
    /// True when the faulting access was a read.
    pub read: bool,
    /// 3-bit function code (1=user data, 2=user prog, 5=sup data, 6=sup prog).
    pub fc: u8,
    /// True when the fault occurred during an instruction (program) fetch,
    /// e.g. after JMP/JSR/RTS/RTE/BSR/Bcc to an odd address.
    pub instruction: bool,
    /// True when the fault came from a stack push/pop that adjusted A7
    /// as part of the implicit stack mechanism (BSR/JSR push, RTS/RTE
    /// pop, exception frame push). When set, step()'s address-error
    /// handler restores A7/SSP/USP to their pre-instruction values
    /// before entering the bus-error frame. When clear (e.g. an EA
    /// fault inside MOVE.x (A7)+, ... ), step() leaves A7 alone so the
    /// post-inc/pre-dec stays committed.
    pub from_stack_op: bool,
}

/// The CPU register file.
#[derive(Debug, Clone)]
pub struct Cpu {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub usp: u32,
    pub ssp: u32,
    pub pc: u32,
    pub sr: StatusRegister,
    pub stopped: bool,
    pub pending_irq: u8,
    pub cycles: u64,
    pub instr_pc: u32,
    pub ir: u16,
    pub address_error: Option<AddressErrorInfo>,
    /// When true, `step()` skips after-instruction trace processing.
    /// Used by the SingleStepTests runner.
    pub no_trace: bool,
    /// MAME's `m_au` register: tracks the next prefetch / write slot. Used
    /// solely to build the address-error stack frame. Updated alongside
    /// `self.pc` on every fetch and (approximately) at major bus accesses.
    pub au: u32,
    /// Debug aid: ring buffer of recent instruction PCs (only records
    /// control-flow discontinuities to stay cheap), dumped on wild jumps.
    pub pc_history: [u32; 32],
    pub pc_history_idx: usize,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            sr: StatusRegister(0x2700),
            stopped: false,
            pending_irq: 0,
            cycles: 0,
            instr_pc: 0,
            ir: 0,
            address_error: None,
            no_trace: false,
            au: 0,
            pc_history: [0; 32],
            pc_history_idx: 0,
        }
    }

    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.ssp = bus.read32(0x0000_0000);
        self.pc = bus.read32(0x0000_0004);
        self.a[7] = self.ssp;
        self.sr = StatusRegister(0x2700);
        self.stopped = false;
        self.pending_irq = 0;
        self.cycles = 0;
        self.address_error = None;
        log::info!(
            "M68K reset: SSP=${:08X} PC=${:08X}",
            self.ssp,
            self.pc
        );
    }

    pub fn request_interrupt(&mut self, level: u8) {
        let level = level.min(7);
        if level > self.pending_irq {
            self.pending_irq = level;
        }
    }

    pub fn push32<B: Bus>(&mut self, bus: &mut B, value: u32) {
        let new_sp = self.a[7].wrapping_sub(4);
        if (new_sp & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: new_sp,
                read: false,
                fc: if self.sr.supervisor() { 5
            } else { 1 },
                instruction: false,
                from_stack_op: true,
            });
            return;
        }
        self.a[7] = new_sp;
        bus.write32(self.a[7], value);
    }
    pub fn push16<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let new_sp = self.a[7].wrapping_sub(2);
        if (new_sp & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: new_sp,
                read: false,
                fc: if self.sr.supervisor() { 5
            } else { 1 },
                instruction: false,
                from_stack_op: true,
            });
            return;
        }
        self.a[7] = new_sp;
        bus.write16(self.a[7], value);
    }
    pub fn pop32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if (self.a[7] & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: self.a[7],
                read: true,
                fc: if self.sr.supervisor() { 5
            } else { 1 },
                instruction: false,
                from_stack_op: true,
            });
            return 0;
        }
        let v = bus.read32(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(4);
        v
    }
    pub fn pop16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        if (self.a[7] & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: self.a[7],
                read: true,
                fc: if self.sr.supervisor() { 5
            } else { 1 },
                instruction: false,
                from_stack_op: true,
            });
            return 0;
        }
        let v = bus.read16(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(2);
        v
    }

    /// Group-2 exception with format-A (14-byte) frame: same layout as
    /// address error, but with a synthetic access address.
    pub fn enter_group2_exception<B: Bus>(&mut self, bus: &mut B, vector: Exception) {
        let was_supervisor = self.sr.supervisor();
        if was_supervisor {
            self.ssp = self.a[7];
        } else {
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }
        let old_sr = self.sr.0;
        self.sr.set(StatusRegister::S, true);
        self.sr.set(StatusRegister::T, false);

        // Format-A frame.
        self.push32(bus, self.pc);
        self.push16(bus, old_sr);
        self.push16(bus, self.ir);
        self.push32(bus, self.instr_pc);
        let fc_word: u16 = if was_supervisor { 0x16 } else { 0x12 };
        self.push16(bus, fc_word);

        let vaddr = vector.vector_offset();
        self.pc = bus.read32(vaddr);
    }

    /// Standard format-B (6-byte) frame: SR, PC.
    pub fn enter_exception<B: Bus>(&mut self, bus: &mut B, vector: Exception) {
        // Debug aid: log non-interrupt exceptions (vector < 24), which on
        // this hardware almost always indicate an emulation bug.
        if (vector as u8) < 24 {
            log::debug!(
                "EXC {:?} at PC=${:08X} (instr_pc=${:08X} ir=${:04X})",
                vector, self.pc, self.instr_pc, self.ir
            );
        }
        let was_supervisor = self.sr.supervisor();
        if was_supervisor {
            self.ssp = self.a[7];
        } else {
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }
        let old_sr = self.sr.0;
        self.sr.set(StatusRegister::S, true);
        self.sr.set(StatusRegister::T, false);

        self.push32(bus, self.pc);
        self.push16(bus, old_sr);

        let vaddr = vector.vector_offset();
        self.pc = bus.read32(vaddr);
    }

    /// Address/bus error: format-A frame (14 bytes).
    ///
    /// Mirrors MAME's `state_address_error_df`: the FC/SSW word is built
    /// from `(m_ird & ~0x1F) | SSW`, where SSW packs R/W, instruction-fetch
    /// (N) and 3-bit function code; the pushed PC is the captured `m_au`.
    pub fn enter_address_error<B: Bus>(&mut self, bus: &mut B, info: AddressErrorInfo) {
        let was_supervisor = self.sr.supervisor();
        if was_supervisor {
            self.ssp = self.a[7];
        } else {
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }
        let old_sr = self.sr.0;
        self.sr.set(StatusRegister::S, true);
        self.sr.set(StatusRegister::T, false);

        // Format-A frame (high address first via successive push):
        //   SSP-2 : PC low word
        //   SSP-4 : PC high word
        //   SSP-6 : SR
        //   SSP-8 : IR
        //   SSP-A : access address low
        //   SSP-C : access address high
        //   SSP-E : SSW/FC word

        // Pushed PC: MAME's m_au captured at the fault. We approximate it
        // with `self.au` which is kept ~2 bytes ahead of self.pc.
        let pc_for_frame = self.au;
        self.push32(bus, pc_for_frame);
        let sr_for_frame = (old_sr & 0xFFE0) | (self.sr.0 & 0x001F);
        self.push16(bus, sr_for_frame);
        self.push16(bus, self.ir);
        self.push32(bus, info.access_addr);
        // MAME's SSW layout (from m68000.h):
        //   bit 0 = DATA        (1 = data access)
        //   bit 1 = PROGRAM     (1 = instruction/program access)
        //   bit 2 = S           (1 = supervisor)
        //   bit 3 = N           ("not" -- next cycle marker, usually 0)
        //   bit 4 = R           (1 = read)
        let s_bit = u16::from(was_supervisor);
        let program = u16::from(info.instruction);
        let data = u16::from(!info.instruction);
        let r = u16::from(info.read);
        let ssw = (r << 4) | (s_bit << 2) | (program << 1) | data;
        let fc_word = (self.ir & 0xFFE0) | ssw;
        self.push16(bus, fc_word);

        let vaddr = Exception::AddressError.vector_offset();
        self.pc = bus.read32(vaddr);
    }

    /// Try to load `target` into PC. If the target is odd, raises an
    /// address error with the snapshot of `m_au` that bser1 captures.
    /// MAME's bser1 does `m_au = m_pc`, where `m_pc` here is the address
    /// of the next prefetch slot just after the opcode -- i.e. the value
    /// our `self.pc` holds *after* opcode + extension-word fetches, with
    /// no extra +2 prefetch ahead. Use this from JMP/JSR/RTS/RTE/RTR/
    /// BRA/Bcc/BSR/DBcc paths instead of touching `self.pc` directly.
    #[inline]
    pub fn jump_to(&mut self, target: u32) {
        if (target & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: target,
                read: true,
                fc: if self.sr.supervisor() { 6
            } else { 2 },
                instruction: true,
                from_stack_op: false,
            });
            // JMP-style: MAME's m_pc at the fault still equals ipc + 2
            // (the microcode hasn't executed `m_pc = m_au` yet).
            // self.pc was advanced by fetch16 for extension words, so
            // we must undo that to reflect the true MAME m_pc value.
            self.au = self.instr_pc.wrapping_add(2);
            return;
        }
        self.pc = target;
        self.au = self.pc.wrapping_add(2);
    }

    /// JSR/BSR/DBcc-style jump: MAME's microcode performs `m_pc = m_au`
    /// (where m_au = previous m_pc + 2 = the next-prefetch slot) just
    /// before the target read. So on a misaligned target the address-
    /// error frame's pushed PC is the next-prefetch slot, which equals
    /// `self.pc` *after* the extension-word fetches.
    pub fn jump_to_subroutine(&mut self, target: u32) {
        if (target & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: target,
                read: true,
                fc: if self.sr.supervisor() { 6
            } else { 2 },
                instruction: true,
                from_stack_op: false,
            });
            self.au = self.pc;
            return;
        }
        self.pc = target;
        self.au = self.pc.wrapping_add(2);
    }

    /// Fetch 16 bits from the program stream. Does NOT touch `self.au`:
    /// MAME's `m_au` only advances when the microcode does an explicit
    /// opcode-stream prefetch via the dedicated `m_opcodes` bus, not on
    /// every extension-word read. For the SingleStepTests goldens the
    /// captured `m_au` at bser1 always equals `m_pc`, which for typical
    /// data-fault paths is still `ipc + 2`.
    #[inline]
    pub fn fetch16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let v = bus.read16(self.pc);
        self.pc = self.pc.wrapping_add(2);
        v
    }
    #[inline]
    pub fn fetch32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let v = bus.read32(self.pc);
        self.pc = self.pc.wrapping_add(4);
        v
    }

    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        // Snapshot for address-error recovery: must use pre-instr SR/SP/PC.
        let pre_a7 = self.a[7];
        let pre_ssp = self.ssp;
        let pre_usp = self.usp;
        let pre_sr = self.sr.0;
        let pre_supervisor = self.sr.supervisor();

        // Service pending IRQ. Level 7 is non-maskable: it fires even when
        // the mask is at 7 (`>=` instead of `>`).
        let irq_takes = self.pending_irq > 0
            && (self.pending_irq == 7 || self.pending_irq > self.sr.interrupt_mask());
        if irq_takes {
            let level = self.pending_irq;
            log::debug!("IRQ level {level} taken at PC=${:08X} SR=${:04X}", self.pc, self.sr.0);
            self.pending_irq = 0;
            self.stopped = false;
            let vec = match level {
                1 => Exception::Interrupt1,
                2 => Exception::Interrupt2,
                3 => Exception::Interrupt3,
                _ => Exception::Spurious,
            };
            self.enter_exception(bus, vec);
            self.sr.set_interrupt_mask(level);
            self.cycles = self.cycles.wrapping_add(44);
            return 44;
        }

        if self.stopped {
            self.cycles = self.cycles.wrapping_add(4);
            return 4;
        }

        let trace_was_set = self.sr.get(StatusRegister::T);

        self.instr_pc = self.pc;
        // MAME at STATE_GENPC sets m_pc = m_ipc + 2 and m_au = m_ipc + 4.
        // When bser1 fires it captures m_au := m_pc, so the value that
        // ends up pushed to the exception frame is m_pc, *not* m_au.
        // For most ALU/MOVE-style faults m_pc stays at ipc+2 throughout
        // the instruction, so we snapshot it once at the start.
        self.au = self.pc.wrapping_add(2);
        let opcode = self.fetch16(bus);
        self.ir = opcode;
        let used = crate::cpu::m68k::exec::execute(self, bus, opcode);
        self.cycles = self.cycles.wrapping_add(u64::from(used));

        // Debug aid: record call-level control flow (jsr/jmp/rts/rte/rtr)
        // in a small ring buffer. Loop branches (dbra/bcc) are skipped so
        // tight loops don't erase the call history.
        let is_call_flow = (self.ir & 0xFF80) == 0x4E80 // jsr/jmp
            || self.ir == 0x4E75 // rts
            || self.ir == 0x4E73 // rte
            || self.ir == 0x4E77; // rtr
        if is_call_flow {
            let i = self.pc_history_idx & 31;
            self.pc_history[i] = self.instr_pc;
            self.pc_history[(i + 1) & 31] = self.pc | 0x8000_0000; // mark targets
            self.pc_history_idx = (i + 2) & 31;
        }
        // A jump into the vector table (< $80) is almost always a wild
        // pointer — log the source instruction and recent flow.
        if self.pc < 0x80 && self.instr_pc >= 0x80 {
            let mut flow = String::new();
            for k in 0..32 {
                let v = self.pc_history[(self.pc_history_idx + k) & 31];
                if v & 0x8000_0000 != 0 {
                    flow.push_str(&format!("->{:06X} ", v & 0x00FF_FFFF));
                } else {
                    flow.push_str(&format!("{:06X}", v));
                }
            }
            log::debug!(
                "WILD JUMP to ${:08X} from instr at ${:08X} (ir=${:04X}) \
                 D0=${:08X} A0=${:08X} A7=${:08X} flow: {}",
                self.pc, self.instr_pc, self.ir, self.d[0], self.a[0], self.a[7], flow
            );
        }

        // Late-detected PC misalignment. With jump_to() in place this
        // should now be unreachable, but keep it as a safety net for any
        // future code paths that still touch self.pc directly.
        if (self.pc & 1) != 0 && self.address_error.is_none() {
            let info = AddressErrorInfo {
                access_addr: self.pc,
                read: true,
                fc: if self.sr.supervisor() { 6
            } else { 2 },
                instruction: true,
                from_stack_op: false,
            };
            self.a[7] = pre_a7;
            self.ssp = pre_ssp;
            self.usp = pre_usp;
            self.sr.0 = (pre_sr & 0xFFE0) | (self.sr.0 & 0x001F);
            let _ = pre_supervisor;
            // m_pc snapshot at the time of the fault: just past the
            // opcode prefetch.
            self.au = self.instr_pc.wrapping_add(2);
            self.enter_address_error(bus, info);
            return used;
        }

        // Address error raised mid-instruction (e.g. push/pop on odd SP,
        // odd EA in a read/write, or an odd jump target).
        if self.address_error.is_some() {
            let info = self.address_error.take().unwrap();
            // For stack-bookkeeping faults (push/pop) the SR/SP changes
            // happen as part of the failed mechanism and must be undone
            // before the exception frame is built. For "normal" EA
            // faults (post-inc/pre-dec, write-to-memory, jump-to-odd)
            // MAME keeps the instruction's side-effects committed and
            // pushes the frame from the adjusted state.
            if info.from_stack_op {
                self.sr.0 = (pre_sr & 0xFFE0) | (self.sr.0 & 0x001F);
                self.a[7] = pre_a7;
                self.ssp = pre_ssp;
                self.usp = pre_usp;
            }
            self.enter_address_error(bus, info);
            return used;
        }

        // After-instruction trace (skipped by SingleStepTests runner).
        if trace_was_set && !self.no_trace {
            self.enter_exception(bus, Exception::Trace);
        }

        used
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

//! Full Motorola 68000 instruction execution.

use crate::cpu::m68k::bus::Bus;
use crate::cpu::m68k::cpu::{Condition, Cpu, Exception, Size, StatusRegister};
use crate::cpu::m68k::ea::Ea;

pub fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let high = (op >> 12) & 0xF;
    match high {
        0x0 => bit_immediate(cpu, bus, op),
        0x1 => move_family(cpu, bus, op, Size::Byte),
        0x2 => move_family(cpu, bus, op, Size::Long),
        0x3 => move_family(cpu, bus, op, Size::Word),
        0x4 => misc_family(cpu, bus, op),
        0x5 => addq_subq(cpu, bus, op),
        0x6 => branch_family(cpu, bus, op),
        0x7 => moveq(cpu, op),
        0x8 => or_div_sbcd(cpu, bus, op),
        0x9 => sub_subx(cpu, bus, op),
        0xA => {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::LineA);
            34
        }
        0xB => cmp_eor(cpu, bus, op),
        0xC => and_mul_abcd(cpu, bus, op),
        0xD => add_addx(cpu, bus, op),
        0xE => shift_rotate(cpu, bus, op),
        0xF => {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::LineF);
            34
        }
        _ => unreachable!(),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// MOVE / MOVEA
// ─────────────────────────────────────────────────────────────────────────

fn move_family<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16, size: Size) -> u32 {
    let src_ea_bits = op & 0x3F;
    let dst_ea_bits = swap_dst((op >> 6) & 0x3F);
    let dst_mode = ((dst_ea_bits >> 3) & 0x7) as u8;
    let is_movea = dst_mode == 1;

    // MAME's microcode performs the source read FIRST, only updating
    // CCR (sr_nzvc) once the source is in hand. A fault on the source
    // read therefore leaves CCR untouched. Do the V/C clear + NZ set
    // only after a successful source read.
    let src = Ea::decode(cpu, bus, src_ea_bits, size);
    // For Byte/Word `-(An)` source EAs, MAME's microcode pdcw1
    // performs `m_pc = m_au` BEFORE the read, so the address-error
    // frame's pushed PC is ipc+4. POST-increment (pinw1) does NOT
    // bump m_pc -- the captured PC stays at ipc+2.
    let src_mode = (src_ea_bits >> 3) & 0x7;
    if src_mode == 4 && size != Size::Long {
        cpu.au = cpu.instr_pc.wrapping_add(4);
    }
    let value = src.read(cpu, bus, size);
    if cpu.address_error.is_some() {
        return 4;
    }

    // Snapshot cpu.pc right after the source read but BEFORE the dst EA
    // decode. MAME's microcode bumps `m_pc` to this position (= ipc + 2
    // + ext_src_bytes) just before the first dst write. Capturing it
    // here lets us push the correct PC on a write-fault frame, even
    // when the dst EA itself consumes extra extension words.
    let pre_dst_pc = cpu.pc;

    let dst = Ea::decode(cpu, bus, dst_ea_bits, size);

    // For memory destinations the pushed-PC at write fault mirrors
    // MAME's `m_au = m_pc` capture in bser1. The value depends on
    // how many extra prefetches happened during src+dst handling.
    //
    // For all dst modes EXCEPT (xxx).l, MAME's `m_pc` at the first
    // write equals `init.PC` (= ipc+4 in our convention) -- our
    // `cpu.pc` after fetching the opcode is `ipc+2`, so add 2.
    //
    // For (xxx).l (mode 7 reg 1), MAME does an extra prefetch step
    // (rall2/mall1) that sets `m_pc = m_au` AFTER `m_au` advanced by
    // 2 -- so MAME's m_pc = init.PC + (number_of_src_ext_words ? 2 : 0)
    // + 2. In our convention `cpu.pc` advances on every ext-word
    // fetch, so cpu.pc after dst.decode = init.PC - 2 + src_ext + 4.
    // For src with at least 1 ext word: cpu.pc = init.PC + 2 = MAME m_pc.
    // For src with no ext words: cpu.pc = init.PC + 2 but MAME m_pc =
    // init.PC (one prefetch fewer). So push cpu.pc - 2.
    let abs_long_dst = dst_mode == 7 && (dst_ea_bits & 7) == 1;
    let write_pushed_au = if abs_long_dst {
        // (xxx).l dst: MAME's `m_pc` advances by an extra +2 only for
        // reg-src (rall1 explicit prefetch step). For mem-src, the
        // dst HIGH was already in `m_irc` so MAME does ONE fewer
        // prefetch than our `cpu.pc` accounts for.
        if src.is_reg_direct() {
            cpu.pc
        } else {
            cpu.pc.wrapping_sub(2)
        }
    } else {
        pre_dst_pc.wrapping_add(2)
    };

    // For Byte/Word, MAME's microcode applies sr_nzvc to the source
    // value just before the (single) write, so the CCR commit
    // survives a write fault.
    //
    // For Long, timing depends on the source path:
    // - reg -> mem (`move_l_ds_*` / `move_l_as_*`): first write happens
    //   before CCR is touched; a fault there leaves CCR unchanged.
    // - mem -> mem (`move_l_*_*`): V/C are already cleared by the time
    //   the first destination write happens, but N/Z are not finalized
    //   until later micro-steps.
    let src_is_imm = matches!(src, Ea::Immediate(_));
    if !is_movea {
        if size != Size::Long {
            cpu.sr.set(StatusRegister::V, false);
            cpu.sr.set(StatusRegister::C, false);
            cpu.sr.set_nz(value, size);
        } else if src_is_imm {
            // imm32 -> mem long: per MAME's microcode (move_l_imm32_*)
            // the SR ops before the first write depend on the dst:
            //   (An), (An)+:        nothing (first write has no sr_)
            //   -(An):              BOTH sr_nzvc(LOW) + sr_nz_u(HIGH)
            //   (d16,An), (d8,X):   sr_nz_u(HIGH) only
            //   (xxx).w, (xxx).l:   BOTH sr_nzvc(LOW) + sr_nz_u(HIGH)
            let lo = value & 0xFFFF;
            let hi = (value >> 16) & 0xFFFF;
            match dst_mode {
                2 | 3 => { /* nothing */ }
                4 | 7 => {
                    // -(An), (xxx).w/.l: BOTH.
                    cpu.sr.set(StatusRegister::V, false);
                    cpu.sr.set(StatusRegister::C, false);
                    cpu.sr.set(StatusRegister::N, (lo & 0x8000) != 0);
                    cpu.sr.set(StatusRegister::Z, lo == 0);
                    let n_new = (hi & 0x8000) != 0;
                    let z_high_zero = hi == 0;
                    let prev_z = cpu.sr.get(StatusRegister::Z);
                    cpu.sr.set(StatusRegister::N, n_new);
                    cpu.sr.set(StatusRegister::Z, prev_z && z_high_zero);
                }
                _ => {
                    // (d16,An), (d8,An,Xn): HIGH only.
                    let n_new = (hi & 0x8000) != 0;
                    let z_high_zero = hi == 0;
                    let prev_z = cpu.sr.get(StatusRegister::Z);
                    cpu.sr.set(StatusRegister::N, n_new);
                    cpu.sr.set(StatusRegister::Z, prev_z && z_high_zero);
                }
            }
        } else if !src.is_reg_direct() {
            // mem -> mem long: MAME's microcode varies by dst mode.
            //  - dst=(An) [2], (An)+ [3], (xxx).l [7/1]: only
            //    sr_nzvc(LOW) before the first write (sr_nz_u(HIGH)
            //    happens before the SECOND write).
            //  - dst=-(An) [4], (d16,An) [5], (d8,An,Xn) [6],
            //    (xxx).w [7/0]: BOTH sr_nzvc(LOW) AND sr_nz_u(HIGH)
            //    before the first write (prefetch happens between
            //    them in MAME's microcode).
            let lo = value & 0xFFFF;
            let hi = (value >> 16) & 0xFFFF;
            cpu.sr.set(StatusRegister::V, false);
            cpu.sr.set(StatusRegister::C, false);
            cpu.sr.set(StatusRegister::N, (lo & 0x8000) != 0);
            cpu.sr.set(StatusRegister::Z, lo == 0);
            let absl = dst_mode == 7 && (dst_ea_bits & 7) == 1;
            if dst_mode != 2 && dst_mode != 3 && !absl {
                // sr_nz_u(HIGH16).
                let n_new = (hi & 0x8000) != 0;
                let z_high_zero = hi == 0;
                let prev_z = cpu.sr.get(StatusRegister::Z);
                cpu.sr.set(StatusRegister::N, n_new);
                cpu.sr.set(StatusRegister::Z, prev_z && z_high_zero);
            }
        } else {
            // reg -> mem long: MAME's microcode depends on dst mode.
            //  - (An) [mode 2] and (An)+ [mode 3]: rmrl1/rmil1 have no
            //    SR update; SR untouched on first-write fault.
            //  - (xxx).w/.l [mode 7/0, 7/1] and -(An) [mode 4]:
            //    rall2/rawl1/rmml1 set sr_nzvc(LOW) BEFORE the first
            //    write, and rall3/rmml2 set sr_nz_u(HIGH) ALSO BEFORE
            //    the first write. Apply BOTH.
            //  - (d16,An) [mode 5], (d8,An,Xn) [mode 6]: rmdl2 sets
            //    sr_nz_u(HIGH) BEFORE the first write; sr_nzvc(LOW)
            //    only happens at rmdl3 BEFORE the second write. So on
            //    first-write fault only HIGH is applied.
            let lo = value & 0xFFFF;
            let hi = (value >> 16) & 0xFFFF;
            match dst_mode {
                2 | 3 => {
                    // (An), (An)+: no SR update before first write.
                }
                7 | 4 => {
                    // (xxx).w / (xxx).l / -(An): sr_nzvc(LOW) then
                    // sr_nz_u(HIGH), both before the first write.
                    cpu.sr.set(StatusRegister::V, false);
                    cpu.sr.set(StatusRegister::C, false);
                    cpu.sr.set(StatusRegister::N, (lo & 0x8000) != 0);
                    cpu.sr.set(StatusRegister::Z, lo == 0);
                    // sr_nz_u(HIGH16):
                    let n_new = (hi & 0x8000) != 0;
                    let z_high_zero = hi == 0;
                    let prev_z = cpu.sr.get(StatusRegister::Z);
                    cpu.sr.set(StatusRegister::N, n_new);
                    cpu.sr.set(StatusRegister::Z, prev_z && z_high_zero);
                }
                _ => {
                    // (d16,An), (d8,An,Xn): sr_nz_u(HIGH16) only.
                    let n_new = (hi & 0x8000) != 0;
                    let z_high_zero = hi == 0;
                    let prev_z = cpu.sr.get(StatusRegister::Z);
                    cpu.sr.set(StatusRegister::N, n_new);
                    cpu.sr.set(StatusRegister::Z, prev_z && z_high_zero);
                }
            }
        }
    }
    // Set cpu.au to MAME's m_pc-at-write so a write fault pushes the
    // correct frame PC. Only memory destinations can fault.
    if !dst.is_reg_direct() {
        cpu.au = write_pushed_au;
    }
    dst.write(cpu, bus, size, value);
    if cpu.address_error.is_some() {
        return 4;
    }
    if !is_movea && size == Size::Long {
        // Final SR commit: NZVC reflect the full Long result. V/C are
        // always cleared because MAME runs `sr_nzvc(LOW)` then
        // `sr_nz_u(HIGH)` over the course of the instruction.
        cpu.sr.set(StatusRegister::V, false);
        cpu.sr.set(StatusRegister::C, false);
        cpu.sr.set_nz(value, size);
    }

    4 + src.cycles(size) + dst.cycles(size)
}

fn swap_dst(b: u16) -> u16 {
    let reg = (b >> 3) & 0x7;
    let mode = b & 0x7;
    (mode << 3) | reg
}

// ─────────────────────────────────────────────────────────────────────────
// MOVEQ
// ─────────────────────────────────────────────────────────────────────────

fn moveq(cpu: &mut Cpu, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let data = (op & 0xFF) as i8 as i32 as u32;
    cpu.d[reg] = data;
    cpu.sr.set_nz(data, Size::Long);
    cpu.sr.set(StatusRegister::V, false);
    cpu.sr.set(StatusRegister::C, false);
    4
}

// ─────────────────────────────────────────────────────────────────────────
// Branches: BRA / Bcc / BSR
// ─────────────────────────────────────────────────────────────────────────

fn branch_family<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let cond_bits = (op >> 8) & 0xF;
    let disp8 = (op & 0xFF) as i8;

    // PC is at opcode+2 after the fetch; if disp8 == 0, the displacement is
    // a 16-bit word in the next slot. Target is relative to "opcode+2".
    let base_after_opcode = cpu.pc;
    let target = if disp8 == 0 {
        let w = cpu.fetch16(bus) as i16;
        base_after_opcode.wrapping_add(w as i32 as u32)
    } else {
        base_after_opcode.wrapping_add(i32::from(disp8) as u32)
    };

    match cond_bits {
        0x0 => {
            cpu.jump_to(target);
            10
        }
        0x1 => {
            // BSR: push return addr (= current pc) then jump.
            //
            // For BSR with misaligned target, MAME's microcode does
            // `m_pc = m_au = target` *before* the prefetch of the new
            // opcode, so the address-error frame captures `target`,
            // not the post-opcode PC. We synthesize that directly.
            cpu.push32(bus, cpu.pc);
            if cpu.address_error.is_none() {
                if (target & 1) != 0 {
                    cpu.au = target;
                    cpu.address_error = Some(crate::cpu::m68k::cpu::AddressErrorInfo {
                        access_addr: target,
                        read: true,
                        fc: if cpu.sr.supervisor() { 6 } else { 2 },
                        instruction: true,
                        from_stack_op: false,
                    });
                } else {
                    cpu.pc = target;
                    cpu.au = target;
                }
            }
            18
        }
        _ => {
            let cond = Condition::from_bits(cond_bits);
            if cpu.sr.evaluate(cond) {
                cpu.jump_to(target);
                10
            } else {
                8
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Misc (line 4)
// ─────────────────────────────────────────────────────────────────────────

fn misc_family<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    if op == 0x4E71 {
        return 4; // NOP
    }
    if op == 0x4E75 {
        // RTS
        let new_pc = cpu.pop32(bus);
        if cpu.address_error.is_none() {
            cpu.jump_to(new_pc);
        }
        return 16;
    }
    if op == 0x4E73 {
        // RTE
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let new_sr = cpu.pop16(bus);
        let new_pc = cpu.pop32(bus);
        if cpu.address_error.is_none() {
            write_sr(cpu, new_sr);
            cpu.jump_to(new_pc);
        }
        return 20;
    }
    if op == 0x4E77 {
        // RTR: pops CCR (low byte of word) then PC. Note that the popped
        // word can have arbitrary high bits, but only the low 5 bits of
        // the low byte (XNZVC) make it into CCR.
        let new_ccr = cpu.pop16(bus);
        let new_pc = cpu.pop32(bus);
        if cpu.address_error.is_none() {
            cpu.sr.0 = (cpu.sr.0 & 0xFFE0) | (new_ccr & 0x001F);
            cpu.jump_to(new_pc);
        }
        return 20;
    }
    if op == 0x4E70 {
        // RESET
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        return 132;
    }
    if op == 0x4E72 {
        // STOP #imm. Privileged.
        //
        // MAME stop1/aaa01 microcode finishes with m_au at ipc+4 (the
        // byte right after the immediate word). With our runner's
        // convention `state.pc = cpu.pc + 4`, that means we must end
        // the instruction with cpu.pc == ipc (i.e. roll back the
        // opcode-fetch advance done by step()).
        //
        // On a privilege violation MAME never consumes the immediate,
        // so the pushed PC is ipc+2 (just after the opcode).
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let new_sr = bus.read16(cpu.pc);
        write_sr(cpu, new_sr);
        cpu.pc = cpu.instr_pc;
        cpu.stopped = true;
        return 4;
    }
    if op == 0x4E76 {
        // TRAPV
        if cpu.sr.get(StatusRegister::V) {
            cpu.enter_exception(bus, Exception::TrapV);
            return 34;
        }
        return 4;
    }

    // TRAP #n
    if (op & 0xFFF0) == 0x4E40 {
        let n = (op & 0xF) as u32;
        let vec_addr = 0x80 + n * 4;
        let was_sup = cpu.sr.supervisor();
        if was_sup {
            cpu.ssp = cpu.a[7];
        } else {
            cpu.usp = cpu.a[7];
            cpu.a[7] = cpu.ssp;
        }
        let old_sr = cpu.sr.0;
        cpu.sr.set(StatusRegister::S, true);
        cpu.sr.set(StatusRegister::T, false);
        cpu.push32(bus, cpu.pc);
        cpu.push16(bus, old_sr);
        cpu.pc = bus.read32(vec_addr);
        return 34;
    }

    // LINK An,#disp
    if (op & 0xFFF8) == 0x4E50 {
        let n = (op & 0x7) as usize;
        let disp = cpu.fetch16(bus) as i16 as i32 as u32;
        cpu.push32(bus, cpu.a[n]);
        if cpu.address_error.is_some() {
            return 16;
        }
        cpu.a[n] = cpu.a[7];
        cpu.a[7] = cpu.a[7].wrapping_add(disp);
        return 16;
    }
    // UNLK An: A7 := An; An := (A7)+
    if (op & 0xFFF8) == 0x4E58 {
        let n = (op & 0x7) as usize;
        cpu.a[7] = cpu.a[n];
        let v = cpu.pop32(bus);
        if cpu.address_error.is_none() {
            cpu.a[n] = v;
        } else {
            // MAME's UNLK microcode performs an extra opcode-stream
            // prefetch *before* the stack pop, which advances m_pc by
            // two. The captured m_au on the bus-error frame therefore
            // matches ipc+4, not ipc+2.
            cpu.au = cpu.au.wrapping_add(2);
        }
        return 12;
    }
    // MOVE USP — privileged
    if (op & 0xFFF0) == 0x4E60 {
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let to_usp = (op & 0x0008) == 0;
        let n = (op & 0x7) as usize;
        if to_usp {
            cpu.usp = cpu.a[n];
        } else {
            cpu.a[n] = cpu.usp;
        }
        return 4;
    }
    // SWAP Dn
    if (op & 0xFFF8) == 0x4840 {
        let n = (op & 0x7) as usize;
        let v = cpu.d[n];
        let new = (v >> 16) | (v << 16);
        cpu.d[n] = new;
        cpu.sr.set_nz(new, Size::Long);
        cpu.sr.set(StatusRegister::V, false);
        cpu.sr.set(StatusRegister::C, false);
        return 4;
    }
    // EXT.W / EXT.L
    if (op & 0xFFB8) == 0x4880 && ((op >> 3) & 0x7) == 0 {
        let n = (op & 0x7) as usize;
        let long = (op & 0x40) != 0;
        if long {
            let v = (cpu.d[n] as i16) as i32 as u32;
            cpu.d[n] = v;
            cpu.sr.set_nz(v, Size::Long);
        } else {
            let v = (cpu.d[n] as i8) as i16 as u32;
            cpu.d[n] = (cpu.d[n] & 0xFFFF_0000) | (v & 0xFFFF);
            cpu.sr.set_nz(v, Size::Word);
        }
        cpu.sr.set(StatusRegister::V, false);
        cpu.sr.set(StatusRegister::C, false);
        return 4;
    }
    // JMP
    if (op & 0xFFC0) == 0x4EC0 {
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Long);
        if let Some(addr) = ea.compute_address(cpu, Size::Long) {
            cpu.jump_to(addr);
        } else {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::IllegalInstruction);
        }
        return 8;
    }
    // JSR
    if (op & 0xFFC0) == 0x4E80 {
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Long);
        if let Some(addr) = ea.compute_address(cpu, Size::Long) {
            // MAME microcode flow: target computed; if odd, `m_pc = m_au`
            // happens BEFORE the target read so the pushed PC is the
            // next-prefetch slot. Use jump_to_subroutine for that path.
            // If aligned, push return addr first then jump.
            if (addr & 1) != 0 {
                cpu.jump_to_subroutine(addr);
            } else {
                cpu.push32(bus, cpu.pc);
                if cpu.address_error.is_none() {
                    cpu.jump_to_subroutine(addr);
                }
            }
        } else {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::IllegalInstruction);
        }
        return 16;
    }
    // LEA <ea>, An
    if (op & 0xF1C0) == 0x41C0 {
        let n = ((op >> 9) & 0x7) as usize;
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Long);
        if let Some(addr) = ea.compute_address(cpu, Size::Long) {
            cpu.a[n] = addr;
        }
        return 4 + ea.cycles(Size::Long);
    }
    // PEA <ea>
    if (op & 0xFFC0) == 0x4840 {
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Long);
        if let Some(addr) = ea.compute_address(cpu, Size::Long) {
            cpu.push32(bus, addr);
        }
        return 12 + ea.cycles(Size::Long);
    }
    // TST <ea>
    if (op & 0xFF00) == 0x4A00 {
        if let Some(size) = size_from_bits67(op) {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            let v = ea.read(cpu, bus, size);
            if cpu.address_error.is_some() {
                return 4;
            }
            cpu.sr.set_nz(v, size);
            cpu.sr.set(StatusRegister::V, false);
            cpu.sr.set(StatusRegister::C, false);
            return 4 + ea.cycles(size);
        }
        // TAS — size_bits == 3 (4ACO).
        // TAS allows the *data alterable* addressing modes only. Modes 1
        // (An direct), 7/2 (PC + d16), 7/3 (PC + index) and 7/4 (#imm) are
        // illegal and trap through vector 4. In particular, opcode $4AFC
        // (TAS with mode=7 reg=4 = immediate) is the canonical 68000
        // ILLEGAL instruction documented in the Programmer's Reference
        // Manual, used by debuggers as a breakpoint marker.
        //
        // See: Motorola M68000PRM (Rev. 1) section 4.5.10 ("TAS") and
        // 4-127 ("ILLEGAL"), and MAME `m68kops.cpp::m68k_op_tas_*`.
        if ((op >> 6) & 0x3) == 0x3 {
            let mode = (op >> 3) & 0x7;
            let reg  = op & 0x7;
            let illegal_ea = mode == 1                        // An direct
                          || (mode == 7 && (reg == 2          // (d16,PC)
                                          || reg == 3          // (d8,PC,Xn)
                                          || reg == 4));       // #<data>
            if illegal_ea {
                cpu.pc = cpu.instr_pc;
                cpu.enter_exception(bus, Exception::IllegalInstruction);
                return 34;
            }
            let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Byte);
            ea.modify(cpu, bus, Size::Byte, |c, val| {
                c.sr.set_nz(val, Size::Byte);
                c.sr.set(StatusRegister::V, false);
                c.sr.set(StatusRegister::C, false);
                val | 0x80
            });
            return 4 + ea.cycles(Size::Byte);
        }
    }
    // CLR <ea>
    if (op & 0xFF00) == 0x4200 {
        if let Some(size) = size_from_bits67(op) {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            ea.modify(cpu, bus, size, |c, _| {
                c.sr.set(StatusRegister::N, false);
                c.sr.set(StatusRegister::Z, true);
                c.sr.set(StatusRegister::V, false);
                c.sr.set(StatusRegister::C, false);
                0
            });
            return 4 + ea.cycles(size);
        }
    }
    // NOT <ea>
    if (op & 0xFF00) == 0x4600 {
        if let Some(size) = size_from_bits67(op) {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            ea.modify(cpu, bus, size, |c, v| {
                let r = (!v) & size.mask();
                c.sr.set_nz(r, size);
                c.sr.set(StatusRegister::V, false);
                c.sr.set(StatusRegister::C, false);
                r
            });
            return 4 + ea.cycles(size);
        }
    }
    // NEG <ea>
    if (op & 0xFF00) == 0x4400 {
        if let Some(size) = size_from_bits67(op) {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            ea.modify(cpu, bus, size, |c, v| {
                let r = (0_u32).wrapping_sub(v) & size.mask();
                c.sr.set_nz(r, size);
                c.sr.set(StatusRegister::V, v == size.sign_bit());
                c.sr.set(StatusRegister::C, v != 0);
                c.sr.set(StatusRegister::X, v != 0);
                r
            });
            return 4 + ea.cycles(size);
        }
    }
    // NEGX <ea>
    if (op & 0xFF00) == 0x4000 {
        if let Some(size) = size_from_bits67(op) {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            ea.modify(cpu, bus, size, |c, v| {
                let x = if c.sr.get(StatusRegister::X) { 1 } else { 0 };
                let r = (0_u32).wrapping_sub(v).wrapping_sub(x) & size.mask();
                let sign = size.sign_bit();
                let borrow = v != 0 || x != 0;
                let v_flag = (v & sign) != 0 && ((r & sign) != 0);
                let prev_z = c.sr.get(StatusRegister::Z);
                c.sr.set(StatusRegister::N, (r & sign) != 0);
                if r != 0 {
                    c.sr.set(StatusRegister::Z, false);
                } else {
                    c.sr.set(StatusRegister::Z, prev_z);
                }
                c.sr.set(StatusRegister::V, v_flag);
                c.sr.set(StatusRegister::C, borrow);
                c.sr.set(StatusRegister::X, borrow);
                r
            });
            return 4 + ea.cycles(size);
        }
    }
    // NBCD <ea>
    if (op & 0xFFC0) == 0x4800 {
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Byte);
        ea.modify(cpu, bus, Size::Byte, |c, v| {
            let x = if c.sr.get(StatusRegister::X) { 1u32 } else { 0 };
            let (r, carry, vflag) = sbcd_calc(0, v as u8, x as u8);
            let prev_z = c.sr.get(StatusRegister::Z);
            if r != 0 {
                c.sr.set(StatusRegister::Z, false);
            } else {
                c.sr.set(StatusRegister::Z, prev_z);
            }
            c.sr.set(StatusRegister::N, (r & 0x80) != 0);
            c.sr.set(StatusRegister::V, vflag);
            c.sr.set(StatusRegister::C, carry);
            c.sr.set(StatusRegister::X, carry);
            r as u32
        });
        return 6 + ea.cycles(Size::Byte);
    }
    // MOVE to SR — privileged
    if (op & 0xFFC0) == 0x46C0 {
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
        let raw = ea.read(cpu, bus, Size::Word);
        if cpu.address_error.is_some() {
            return 4;
        }
        write_sr(cpu, raw as u16);
        return 12 + ea.cycles(Size::Word);
    }
    // MOVE from SR
    if (op & 0xFFC0) == 0x40C0 {
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
        ea.modify(cpu, bus, Size::Word, |c, _| u32::from(c.sr.0));
        return 6 + ea.cycles(Size::Word);
    }
    // MOVE to CCR
    if (op & 0xFFC0) == 0x44C0 {
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
        let raw = ea.read(cpu, bus, Size::Word);
        if cpu.address_error.is_some() {
            return 4;
        }
        let v = (raw & 0x1F) as u16;
        cpu.sr.0 = (cpu.sr.0 & 0xFFE0) | v;
        return 12 + ea.cycles(Size::Word);
    }
    // CHK <ea>, Dn
    if (op & 0xF1C0) == 0x4180 {
        let n = ((op >> 9) & 0x7) as usize;
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
        let bound_raw = ea.read(cpu, bus, Size::Word);
        if cpu.address_error.is_some() {
            return 4;
        }
        let bound = bound_raw as i16 as i32;
        let val = cpu.d[n] as i16 as i32;
        cpu.sr.set(StatusRegister::Z, false);
        cpu.sr.set(StatusRegister::V, false);
        cpu.sr.set(StatusRegister::C, false);
        if val < 0 {
            cpu.sr.set(StatusRegister::N, true);
            cpu.enter_exception(bus, Exception::Chk);
            return 40;
        }
        if val > bound {
            cpu.sr.set(StatusRegister::N, false);
            cpu.enter_exception(bus, Exception::Chk);
            return 40;
        }
        cpu.sr.set(StatusRegister::N, false);
        return 10 + ea.cycles(Size::Word);
    }
    // MOVEM
    if (op & 0xFB80) == 0x4880 {
        return movem(cpu, bus, op);
    }
    // ILLEGAL
    if op == 0x4AFC {
        cpu.pc = cpu.instr_pc;
        cpu.enter_exception(bus, Exception::IllegalInstruction);
        return 34;
    }

    cpu.pc = cpu.instr_pc;
    cpu.enter_exception(bus, Exception::IllegalInstruction);
    34
}

fn size_from_bits67(op: u16) -> Option<Size> {
    match (op >> 6) & 0x3 {
        0 => Some(Size::Byte),
        1 => Some(Size::Word),
        2 => Some(Size::Long),
        _ => None,
    }
}

fn write_sr(cpu: &mut Cpu, new_sr: u16) {
    // 68000 SR valid bits: T, S, interrupt mask, XNZVC.
    // Reserved bits must read back as zero.
    let new_sr = new_sr & 0xA71F;
    let was_supervisor = cpu.sr.supervisor();
    let will_be_supervisor = (new_sr & StatusRegister::S) != 0;
    if was_supervisor != will_be_supervisor {
        if was_supervisor {
            cpu.ssp = cpu.a[7];
            cpu.a[7] = cpu.usp;
        } else {
            cpu.usp = cpu.a[7];
            cpu.a[7] = cpu.ssp;
        }
    }
    cpu.sr.0 = new_sr;
}

// ─────────────────────────────────────────────────────────────────────────
// MOVEM
// ─────────────────────────────────────────────────────────────────────────

fn movem<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let dr = (op & 0x0400) != 0;
    let long = (op & 0x0040) != 0;
    let size = if long { Size::Long } else { Size::Word };
    let mask = cpu.fetch16(bus);
    let mode = ((op >> 3) & 0x7) as u8;
    let reg = (op & 0x7) as u8;

    let predec = mode == 4;
    let postinc = mode == 3;

    // MOVEM's microcode performs an opcode-stream prefetch right before
    // entering the data-transfer loop, then keeps `m_pc` two bytes ahead
    // of the current prefetch position. Match the SingleStepTests
    // goldens (pushed PC = self.pc + 2 for any in-loop fault).
    macro_rules! bump_au {
        () => {
            cpu.au = cpu.pc.wrapping_add(2);
        };
    }

    if !dr {
        // Registers → memory
        if predec {
            // MAME microcode for MOVEM.l Dn,-(An) writes the LOW word
            // first at An-2, then the HIGH word at An-4 (push4 + push5).
            // For Word size only one write at An-2 happens per register.
            // The destination register `An` is only committed at push3,
            // after all writes succeed -- so on any fault the original
            // An stays intact.
            let mut a = cpu.a[reg as usize];
            for i in 0..16 {
                if (mask >> i) & 1 == 1 {
                    let r = 15 - i;
                    let v = if r < 8 { cpu.d[r] } else { cpu.a[r - 8] };
                    if long {
                        let low_addr = a.wrapping_sub(2);
                        let high_addr = a.wrapping_sub(4);
                        let low_word = v & 0xFFFF;
                        let high_word = (v >> 16) & 0xFFFF;
                        crate::cpu::m68k::ea::write_at(cpu, bus, low_addr, Size::Word, low_word);
                        if cpu.address_error.is_some() {
                            bump_au!();
                            return 20;
                        }
                        crate::cpu::m68k::ea::write_at(cpu, bus, high_addr, Size::Word, high_word);
                        if cpu.address_error.is_some() {
                            bump_au!();
                            return 20;
                        }
                        a = high_addr;
                    } else {
                        a = a.wrapping_sub(2);
                        crate::cpu::m68k::ea::write_at(cpu, bus, a, Size::Word, v);
                        if cpu.address_error.is_some() {
                            bump_au!();
                            return 20;
                        }
                    }
                }
            }
            cpu.a[reg as usize] = a;
        } else {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            let mut a = ea.compute_address(cpu, size).unwrap_or(0);
            for i in 0..16 {
                if (mask >> i) & 1 == 1 {
                    let v = if i < 8 { cpu.d[i] } else { cpu.a[i - 8] };
                    crate::cpu::m68k::ea::write_at(cpu, bus, a, size, v);
                    if cpu.address_error.is_some() {
                        bump_au!();
                        return 20;
                    }
                    a = a.wrapping_add(size.bytes());
                }
            }
        }
    } else {
        // Memory → registers
        if postinc {
            let mut a = cpu.a[reg as usize];
            for i in 0..16 {
                if (mask >> i) & 1 == 1 {
                    let v_raw = crate::cpu::m68k::ea::read_at(cpu, bus, a, size);
                    if cpu.address_error.is_some() {
                        bump_au!();
                        return 20;
                    }
                    let v = if long { v_raw } else { (v_raw as i16) as i32 as u32 };
                    if i < 8 {
                        cpu.d[i] = v;
                    } else {
                        cpu.a[i - 8] = v;
                    }
                    a = a.wrapping_add(size.bytes());
                }
            }
            cpu.a[reg as usize] = a;
        } else {
            let ea = Ea::decode(cpu, bus, op & 0x3F, size);
            let pc_relative_src = matches!(ea, Ea::PcIndDisp(_) | Ea::PcIndIdx(..));
            let mut a = ea.compute_address(cpu, size).unwrap_or(0);
            for i in 0..16 {
                if (mask >> i) & 1 == 1 {
                    let v_raw = crate::cpu::m68k::ea::read_at(cpu, bus, a, size);
                    if cpu.address_error.is_some() {
                        // PC-relative source EAs access PROGRAM space;
                        // fix up SSW/FC to match MAME's behaviour.
                        if pc_relative_src {
                            if let Some(info) = cpu.address_error.as_mut() {
                                info.instruction = true;
                                info.fc = if cpu.sr.supervisor() { 6 } else { 2 };
                            }
                        }
                        bump_au!();
                        return 20;
                    }
                    let v = if long { v_raw } else { (v_raw as i16) as i32 as u32 };
                    if i < 8 {
                        cpu.d[i] = v;
                    } else {
                        cpu.a[i - 8] = v;
                    }
                    a = a.wrapping_add(size.bytes());
                }
            }
        }
    }
    20
}

// ─────────────────────────────────────────────────────────────────────────
// ADDQ / SUBQ / Scc / DBcc (line 5)
// ─────────────────────────────────────────────────────────────────────────

fn addq_subq<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let size_bits = (op >> 6) & 0x3;
    if size_bits == 0x3 {
        let mode = (op >> 3) & 0x7;
        if mode == 0x1 {
            return dbcc(cpu, bus, op);
        }
        return scc(cpu, bus, op);
    }
    let size = match size_bits {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => unreachable!(),
    };
    let is_sub = (op & 0x0100) != 0;
    let mut imm = u32::from((op >> 9) & 0x7);
    if imm == 0 {
        imm = 8;
    }
    let ea = Ea::decode(cpu, bus, op & 0x3F, size);

    if let Some(r) = ea.addr_reg() {
        let cur = cpu.a[r as usize];
        cpu.a[r as usize] = if is_sub {
            cur.wrapping_sub(imm)
        } else {
            cur.wrapping_add(imm)
        };
        return 8;
    }

    ea.modify(cpu, bus, size, |c, dst| {
        if is_sub {
            sub_with_flags(c, dst, imm, size, true)
        } else {
            add_with_flags(c, dst, imm, size, true)
        }
    });
    4 + ea.cycles(size)
}

fn dbcc<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let cond = Condition::from_bits((op >> 8) & 0xF);
    let reg = (op & 0x7) as usize;
    let disp_pc = cpu.pc;
    let disp = cpu.fetch16(bus) as i16 as i32 as u32;
    if cpu.sr.evaluate(cond) {
        // Condition true: do NOT decrement, do NOT branch.
        return 12;
    }
    // MAME microcode: the decrement of Dn is committed *after* the
    // branch-target prefetch. If the target is misaligned the address
    // error fires before Dn is modified, so Dn must look untouched in
    // the stack frame. We mimic this by validating the jump first.
    let cur = (cpu.d[reg] & 0xFFFF) as i16;
    let next = cur.wrapping_sub(1);
    if next == -1 {
        // counter expired -- write back (no branch, no fault)
        cpu.d[reg] = (cpu.d[reg] & 0xFFFF_0000) | u32::from(next as u16);
        return 14;
    }
    let target = disp_pc.wrapping_add(disp);
    // DBcc uses the JSR-style fault PC (microcode does `m_pc = m_au`
    // before reading the branch target).
    cpu.jump_to_subroutine(target);
    if cpu.address_error.is_some() {
        // Faulted: Dn must remain untouched (MAME commits decrement
        // only after the successful prefetch from the new PC).
        return 10;
    }
    cpu.d[reg] = (cpu.d[reg] & 0xFFFF_0000) | u32::from(next as u16);
    10
}

fn scc<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let cond = Condition::from_bits((op >> 8) & 0xF);
    let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Byte);
    let v: u32 = if cpu.sr.evaluate(cond) { 0xFF } else { 0x00 };
    ea.modify(cpu, bus, Size::Byte, |_, _| v);
    4 + ea.cycles(Size::Byte)
}

// ─────────────────────────────────────────────────────────────────────────
// ADD / ADDA / ADDX (line D)
// ─────────────────────────────────────────────────────────────────────────

fn add_addx<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let opmode = (op >> 6) & 0x7;
    if opmode == 0x3 || opmode == 0x7 {
        // ADDA
        let size = if opmode == 0x3 { Size::Word } else { Size::Long };
        let ea = Ea::decode(cpu, bus, op & 0x3F, size);
        let v = ea.read(cpu, bus, size);
        if cpu.address_error.is_some() {
            return 4;
        }
        let v_ext = match size {
            Size::Word => (v as i16) as i32 as u32,
            _ => v,
        };
        cpu.a[reg] = cpu.a[reg].wrapping_add(v_ext);
        return 8 + ea.cycles(size);
    }
    let dr_to_ea = (opmode & 0x4) != 0;
    let mode_bits = (op >> 3) & 0x7;
    if dr_to_ea && (mode_bits == 0 || mode_bits == 1) {
        // ADDX
        let size = match opmode & 0x3 {
            0 => Size::Byte,
            1 => Size::Word,
            2 => Size::Long,
            _ => unreachable!(),
        };
        let rm = (op & 0x8) != 0;
        let sy = (op & 0x7) as usize;
        let dx = ((op >> 9) & 0x7) as usize;
        let (src, dst) = if rm {
            // MAME microcode: m_pc advances to ipc+4 BEFORE the reads,
            // so the exception frame's pushed PC is ipc+4. For B/W the
            // predec to Ay/Ax is committed BEFORE its read. For L the
            // microcode reads the high word from Ay-2 first (without
            // committing) and only commits Ay = Ay-4 AFTER the high-
            // word read succeeds and before the low-word read.
            cpu.au = cpu.pc.wrapping_add(2);
            let (s, d) = subx_addx_predec_read(cpu, bus, size, sy, dx);
            if cpu.address_error.is_some() {
                return 4;
            }
            (s, d)
        } else {
            (cpu.d[sy] & size.mask(), cpu.d[dx] & size.mask())
        };
        let x = if cpu.sr.get(StatusRegister::X) { 1 } else { 0 };
        let result = addx_with_flags(cpu, dst, src, x, size);
        if rm {
            crate::cpu::m68k::ea::write_at(cpu, bus, cpu.a[dx], size, result);
            if cpu.address_error.is_some() {
                return 4;
            }
        } else {
            let mask = size.mask();
            cpu.d[dx] = (cpu.d[dx] & !mask) | (result & mask);
        }
        return if size == Size::Long { 8 } else { 4 };
    }

    let size = match opmode & 0x3 {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => unreachable!(),
    };
    let ea = Ea::decode(cpu, bus, op & 0x3F, size);
    if dr_to_ea {
        let dn = cpu.d[reg] & size.mask();
        ea.modify(cpu, bus, size, |c, v| add_with_flags(c, v, dn, size, true));
    } else {
        let ea_val = ea.read(cpu, bus, size);
        if cpu.address_error.is_some() {
            return 4;
        }
        let dn = cpu.d[reg] & size.mask();
        let result = add_with_flags(cpu, dn, ea_val, size, true);
        let mask = size.mask();
        cpu.d[reg] = (cpu.d[reg] & !mask) | (result & mask);
    }
    4 + ea.cycles(size)
}

// ─────────────────────────────────────────────────────────────────────────
// SUB / SUBA / SUBX (line 9)
// ─────────────────────────────────────────────────────────────────────────

fn sub_subx<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let opmode = (op >> 6) & 0x7;
    if opmode == 0x3 || opmode == 0x7 {
        // SUBA
        let size = if opmode == 0x3 { Size::Word } else { Size::Long };
        let ea = Ea::decode(cpu, bus, op & 0x3F, size);
        let v = ea.read(cpu, bus, size);
        if cpu.address_error.is_some() {
            return 4;
        }
        let v_ext = match size {
            Size::Word => (v as i16) as i32 as u32,
            _ => v,
        };
        cpu.a[reg] = cpu.a[reg].wrapping_sub(v_ext);
        return 8 + ea.cycles(size);
    }
    let dr_to_ea = (opmode & 0x4) != 0;
    let mode_bits = (op >> 3) & 0x7;
    if dr_to_ea && (mode_bits == 0 || mode_bits == 1) {
        // SUBX
        let size = match opmode & 0x3 {
            0 => Size::Byte,
            1 => Size::Word,
            2 => Size::Long,
            _ => unreachable!(),
        };
        let rm = (op & 0x8) != 0;
        let sy = (op & 0x7) as usize;
        let dx = ((op >> 9) & 0x7) as usize;
        let (src, dst) = if rm {
            cpu.au = cpu.pc.wrapping_add(2);
            let (s, d) = subx_addx_predec_read(cpu, bus, size, sy, dx);
            if cpu.address_error.is_some() {
                return 4;
            }
            (s, d)
        } else {
            (cpu.d[sy] & size.mask(), cpu.d[dx] & size.mask())
        };
        let x = if cpu.sr.get(StatusRegister::X) { 1 } else { 0 };
        let result = subx_with_flags(cpu, dst, src, x, size);
        if rm {
            crate::cpu::m68k::ea::write_at(cpu, bus, cpu.a[dx], size, result);
            if cpu.address_error.is_some() {
                return 4;
            }
        } else {
            let mask = size.mask();
            cpu.d[dx] = (cpu.d[dx] & !mask) | (result & mask);
        }
        return if size == Size::Long { 8 } else { 4 };
    }
    let size = match opmode & 0x3 {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => unreachable!(),
    };
    let ea = Ea::decode(cpu, bus, op & 0x3F, size);
    if dr_to_ea {
        let dn = cpu.d[reg] & size.mask();
        ea.modify(cpu, bus, size, |c, v| sub_with_flags(c, v, dn, size, true));
    } else {
        let ea_val = ea.read(cpu, bus, size);
        if cpu.address_error.is_some() {
            return 4;
        }
        let dn = cpu.d[reg] & size.mask();
        let result = sub_with_flags(cpu, dn, ea_val, size, true);
        let mask = size.mask();
        cpu.d[reg] = (cpu.d[reg] & !mask) | (result & mask);
    }
    4 + ea.cycles(size)
}

// ─────────────────────────────────────────────────────────────────────────
// CMP / CMPA / EOR / CMPM (line B)
// ─────────────────────────────────────────────────────────────────────────

fn cmp_eor<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let opmode = (op >> 6) & 0x7;
    if opmode == 0x3 || opmode == 0x7 {
        // CMPA
        let size = if opmode == 0x3 { Size::Word } else { Size::Long };
        let ea = Ea::decode(cpu, bus, op & 0x3F, size);
        let v = ea.read(cpu, bus, size);
        if cpu.address_error.is_some() {
            return 4;
        }
        let v_ext = match size {
            Size::Word => (v as i16) as i32 as u32,
            _ => v,
        };
        let an = cpu.a[reg];
        sub_with_flags(cpu, an, v_ext, Size::Long, false);
        return 6 + ea.cycles(size);
    }
    if (opmode & 0x4) != 0 {
        let size = match opmode & 0x3 {
            0 => Size::Byte,
            1 => Size::Word,
            2 => Size::Long,
            _ => unreachable!(),
        };
        let mode_bits = (op >> 3) & 0x7;
        if mode_bits == 0x1 {
            // CMPM (Ay)+, (Ax)+.
            //
            // MAME microcode (cmmw1/cmml1) does `m_pc = m_au` BEFORE
            // the first read, so an address-error frame here pushes
            // PC = ipc+4.
            //
            // For B/W: Ay post-inc commits before the read; same Ax.
            // For L: Ay commits in TWO stages -- Ay+2 before the high
            //   read, Ay+4 before the low read. Same for Ax. A fault
            //   on the high read leaves An at An+2; on the low read,
            //   at An+4.
            cpu.au = cpu.instr_pc.wrapping_add(4);
            let ay = (op & 0x7) as usize;
            let ax = ((op >> 9) & 0x7) as usize;
            let (src, dst) = if size == Size::Long {
                // Ay: commit Ay+2 BEFORE high read (cmml2), Ay+4 before
                // low read (cmml3).
                let ay_base = cpu.a[ay];
                cpu.a[ay] = ay_base.wrapping_add(2);
                let hi = crate::cpu::m68k::ea::read_at(cpu, bus, ay_base, Size::Word);
                if cpu.address_error.is_some() { return 4; }
                cpu.a[ay] = ay_base.wrapping_add(4);
                let lo = crate::cpu::m68k::ea::read_at(cpu, bus, ay_base.wrapping_add(2), Size::Word);
                if cpu.address_error.is_some() { return 4; }
                let src = (hi << 16) | lo;
                // Ax: NOT committed before the high read in cmml4 (only
                // m_aob = Ax). Commit Ax+2 only between the high and low
                // reads (cmml5 implicit), Ax+4 at the end.
                let ax_base = cpu.a[ax];
                let hi = crate::cpu::m68k::ea::read_at(cpu, bus, ax_base, Size::Word);
                if cpu.address_error.is_some() { return 4; }
                cpu.a[ax] = ax_base.wrapping_add(2);
                let lo = crate::cpu::m68k::ea::read_at(cpu, bus, ax_base.wrapping_add(2), Size::Word);
                if cpu.address_error.is_some() { return 4; }
                cpu.a[ax] = ax_base.wrapping_add(4);
                let dst = (hi << 16) | lo;
                (src, dst)
            } else {
                let inc_y = if ay == 7 && size == Size::Byte { 2 } else { size.bytes() };
                let inc_x = if ax == 7 && size == Size::Byte { 2 } else { size.bytes() };
                // Ay (src): cmmw2 commits `m_da[ry] = m_au` BEFORE the
                // read.
                let ay_addr = cpu.a[ay];
                cpu.a[ay] = cpu.a[ay].wrapping_add(inc_y);
                let src = crate::cpu::m68k::ea::read_at(cpu, bus, ay_addr, size);
                if cpu.address_error.is_some() { return 4; }
                // Ax (dst): cmmw3 reads at m_da[rx]; commit happens in
                // cmmw4 AFTER the read. Fault leaves Ax unchanged.
                let ax_addr = cpu.a[ax];
                let dst = crate::cpu::m68k::ea::read_at(cpu, bus, ax_addr, size);
                if cpu.address_error.is_some() { return 4; }
                cpu.a[ax] = cpu.a[ax].wrapping_add(inc_x);
                (src, dst)
            };
            sub_with_flags(cpu, dst, src, size, false);
            return if size == Size::Long { 20 } else { 12 };
        }
        let ea = Ea::decode(cpu, bus, op & 0x3F, size);
        let dn = cpu.d[reg] & size.mask();
        ea.modify(cpu, bus, size, |c, v| {
            let r = (v ^ dn) & size.mask();
            c.sr.set_nz(r, size);
            c.sr.set(StatusRegister::V, false);
            c.sr.set(StatusRegister::C, false);
            r
        });
        return 4 + ea.cycles(size);
    }
    let size = match opmode & 0x3 {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => unreachable!(),
    };
    let ea = Ea::decode(cpu, bus, op & 0x3F, size);
    let v = ea.read(cpu, bus, size);
    if cpu.address_error.is_some() {
        return 4;
    }
    let dn = cpu.d[reg] & size.mask();
    sub_with_flags(cpu, dn, v, size, false);
    4 + ea.cycles(size)
}

// ─────────────────────────────────────────────────────────────────────────
// OR / DIVU / DIVS / SBCD (line 8)
// ─────────────────────────────────────────────────────────────────────────

fn or_div_sbcd<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let opmode = (op >> 6) & 0x7;
    if opmode == 0x3 {
        return divu(cpu, bus, op);
    }
    if opmode == 0x7 {
        return divs(cpu, bus, op);
    }
    if (op & 0xF1F0) == 0x8100 {
        return sbcd(cpu, bus, op);
    }
    or_and_common(cpu, bus, op, false)
}

// ─────────────────────────────────────────────────────────────────────────
// AND / MULU / MULS / ABCD / EXG (line C)
// ─────────────────────────────────────────────────────────────────────────

fn and_mul_abcd<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let opmode = (op >> 6) & 0x7;
    if opmode == 0x3 {
        return mulu(cpu, bus, op);
    }
    if opmode == 0x7 {
        return muls(cpu, bus, op);
    }
    if (op & 0xF1F0) == 0xC100 {
        return abcd(cpu, bus, op);
    }
    // EXG
    if (op & 0xF130) == 0xC100 {
        let mode = (op >> 3) & 0x1F;
        let rx = ((op >> 9) & 0x7) as usize;
        let ry = (op & 0x7) as usize;
        match mode {
            0b01000 => {
                cpu.d.swap(rx, ry);
                return 6;
            }
            0b01001 => {
                cpu.a.swap(rx, ry);
                return 6;
            }
            0b10001 => {
                let t = cpu.d[rx];
                cpu.d[rx] = cpu.a[ry];
                cpu.a[ry] = t;
                return 6;
            }
            _ => {}
        }
    }
    or_and_common(cpu, bus, op, true)
}

fn or_and_common<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16, is_and: bool) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let opmode = (op >> 6) & 0x7;
    let size = match opmode & 0x3 {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => unreachable!(),
    };
    let dr_to_ea = (opmode & 0x4) != 0;
    let ea = Ea::decode(cpu, bus, op & 0x3F, size);
    if dr_to_ea {
        let dn = cpu.d[reg] & size.mask();
        ea.modify(cpu, bus, size, |c, v| {
            let r = if is_and { v & dn } else { v | dn } & size.mask();
            c.sr.set_nz(r, size);
            c.sr.set(StatusRegister::V, false);
            c.sr.set(StatusRegister::C, false);
            r
        });
    } else {
        let v = ea.read(cpu, bus, size);
        if cpu.address_error.is_some() {
            return 4;
        }
        let dn = cpu.d[reg] & size.mask();
        let r = if is_and { v & dn } else { v | dn } & size.mask();
        let mask = size.mask();
        cpu.d[reg] = (cpu.d[reg] & !mask) | r;
        cpu.sr.set_nz(r, size);
        cpu.sr.set(StatusRegister::V, false);
        cpu.sr.set(StatusRegister::C, false);
    }
    4 + ea.cycles(size)
}

// ─────────────────────────────────────────────────────────────────────────
// MULU / MULS / DIVU / DIVS
// ─────────────────────────────────────────────────────────────────────────

fn mulu<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
    let v_raw = ea.read(cpu, bus, Size::Word);
    if cpu.address_error.is_some() {
        return 4;
    }
    let v = v_raw & 0xFFFF;
    let dn = cpu.d[reg] & 0xFFFF;
    let r = v.wrapping_mul(dn);
    cpu.d[reg] = r;
    cpu.sr.set_nz(r, Size::Long);
    cpu.sr.set(StatusRegister::V, false);
    cpu.sr.set(StatusRegister::C, false);
    70 + ea.cycles(Size::Word)
}

fn muls<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
    let v_raw = ea.read(cpu, bus, Size::Word);
    if cpu.address_error.is_some() {
        return 4;
    }
    let v = v_raw as i16 as i32;
    let dn = (cpu.d[reg] & 0xFFFF) as i16 as i32;
    let r = (v.wrapping_mul(dn)) as u32;
    cpu.d[reg] = r;
    cpu.sr.set_nz(r, Size::Long);
    cpu.sr.set(StatusRegister::V, false);
    cpu.sr.set(StatusRegister::C, false);
    70 + ea.cycles(Size::Word)
}

fn divu<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
    let divisor_raw = ea.read(cpu, bus, Size::Word);
    if cpu.address_error.is_some() {
        return 4;
    }
    let divisor = divisor_raw & 0xFFFF;
    // DIVU clears C unconditionally per MAME.
    cpu.sr.set(StatusRegister::C, false);
    if divisor == 0 {
        cpu.enter_group2_exception(bus, Exception::DivideByZero);
        return 38;
    }
    let dividend = cpu.d[reg];
    let q = dividend / divisor;
    let rem = dividend % divisor;
    if q > 0xFFFF {
        cpu.sr.set(StatusRegister::V, true);
        cpu.sr.set(StatusRegister::N, true);
        cpu.sr.set(StatusRegister::Z, false);
        return 76 + ea.cycles(Size::Word);
    }
    let result = (rem << 16) | (q & 0xFFFF);
    cpu.d[reg] = result;
    cpu.sr.set_nz(q, Size::Word);
    cpu.sr.set(StatusRegister::V, false);
    140 + ea.cycles(Size::Word)
}

fn divs<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let reg = ((op >> 9) & 0x7) as usize;
    let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
    let divisor_raw = ea.read(cpu, bus, Size::Word);
    if cpu.address_error.is_some() {
        return 4;
    }
    let divisor = divisor_raw as i16 as i32;
    cpu.sr.set(StatusRegister::C, false);
    if divisor == 0 {
        cpu.enter_group2_exception(bus, Exception::DivideByZero);
        return 38;
    }
    let dividend = cpu.d[reg] as i32;
    let q = dividend.wrapping_div(divisor);
    let rem = dividend.wrapping_rem(divisor);
    if !(-32768..=32767).contains(&q) {
        cpu.sr.set(StatusRegister::V, true);
        cpu.sr.set(StatusRegister::N, true);
        cpu.sr.set(StatusRegister::Z, false);
        return 158 + ea.cycles(Size::Word);
    }
    let result = ((rem as u32 & 0xFFFF) << 16) | (q as u32 & 0xFFFF);
    cpu.d[reg] = result;
    cpu.sr.set_nz(q as u32 & 0xFFFF, Size::Word);
    cpu.sr.set(StatusRegister::V, false);
    158 + ea.cycles(Size::Word)
}

// ─────────────────────────────────────────────────────────────────────────
// BCD: ABCD / SBCD / NBCD
// ─────────────────────────────────────────────────────────────────────────

fn abcd<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let rm = (op & 0x8) != 0;
    let dy = (op & 0x7) as usize;
    let dx = ((op >> 9) & 0x7) as usize;
    let (src, dst) = if rm {
        let dec_y = if dy == 7 { 2 } else { 1 };
        cpu.a[dy] = cpu.a[dy].wrapping_sub(dec_y);
        let s = bus.read8(cpu.a[dy]) as u32;
        let dec_x = if dx == 7 { 2 } else { 1 };
        cpu.a[dx] = cpu.a[dx].wrapping_sub(dec_x);
        let d = bus.read8(cpu.a[dx]) as u32;
        (s, d)
    } else {
        (cpu.d[dy] & 0xFF, cpu.d[dx] & 0xFF)
    };
    let x = if cpu.sr.get(StatusRegister::X) { 1 } else { 0 };
    let (r, carry, v) = abcd_calc(dst as u8, src as u8, x);
    let prev_z = cpu.sr.get(StatusRegister::Z);
    if r != 0 {
        cpu.sr.set(StatusRegister::Z, false);
    } else {
        cpu.sr.set(StatusRegister::Z, prev_z);
    }
    cpu.sr.set(StatusRegister::N, (r & 0x80) != 0);
    cpu.sr.set(StatusRegister::V, v);
    cpu.sr.set(StatusRegister::C, carry);
    cpu.sr.set(StatusRegister::X, carry);
    if rm {
        bus.write8(cpu.a[dx], r);
    } else {
        cpu.d[dx] = (cpu.d[dx] & !0xFF) | u32::from(r);
    }
    if rm { 18 } else { 6 }
}

fn sbcd<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let rm = (op & 0x8) != 0;
    let dy = (op & 0x7) as usize;
    let dx = ((op >> 9) & 0x7) as usize;
    let (src, dst) = if rm {
        let dec_y = if dy == 7 { 2 } else { 1 };
        cpu.a[dy] = cpu.a[dy].wrapping_sub(dec_y);
        let s = bus.read8(cpu.a[dy]) as u32;
        let dec_x = if dx == 7 { 2 } else { 1 };
        cpu.a[dx] = cpu.a[dx].wrapping_sub(dec_x);
        let d = bus.read8(cpu.a[dx]) as u32;
        (s, d)
    } else {
        (cpu.d[dy] & 0xFF, cpu.d[dx] & 0xFF)
    };
    let x = if cpu.sr.get(StatusRegister::X) { 1 } else { 0 };
    let (r, carry, v) = sbcd_calc(dst as u8, src as u8, x);
    let prev_z = cpu.sr.get(StatusRegister::Z);
    if r != 0 {
        cpu.sr.set(StatusRegister::Z, false);
    } else {
        cpu.sr.set(StatusRegister::Z, prev_z);
    }
    cpu.sr.set(StatusRegister::N, (r & 0x80) != 0);
    cpu.sr.set(StatusRegister::V, v);
    cpu.sr.set(StatusRegister::C, carry);
    cpu.sr.set(StatusRegister::X, carry);
    if rm {
        bus.write8(cpu.a[dx], r);
    } else {
        cpu.d[dx] = (cpu.d[dx] & !0xFF) | u32::from(r);
    }
    if rm { 18 } else { 6 }
}

fn abcd_calc(dst: u8, src: u8, x: u8) -> (u8, bool, bool) {
    // MAME's `alu_abcd8` semantics.
    let a = src as u16;
    let b = dst as u16;
    let xc = x as u16;
    let hr = (b & 0x0F) + (a & 0x0F) + xc;
    let lcor = hr > 9;
    let r1 = b.wrapping_add(a).wrapping_add(xc);
    let mut r = if lcor { r1.wrapping_add(6) } else { r1 };
    if r > 0x9F {
        r = r.wrapping_add(0x60);
    }
    let res8 = (r & 0xFF) as u8;
    // Carry: any of bits 8 or 9 set (0x300 mask in MAME).
    let carry = (r & 0x300) != 0;
    // V flag: result bit 7 set AND raw-sum (pre 6/60 corrections) bit 7 NOT set.
    let v = (r & 0x80) != 0 && (r1 & 0x80) == 0;
    (res8, carry, v)
}

#[allow(dead_code)]
fn _bcd_marker() {}

fn sbcd_calc(dst: u8, src: u8, x: u8) -> (u8, bool, bool) {
    // MAME's `alu_sbcd8` semantics. Use u16-wide arithmetic so the
    // carry/borrow detection (`r & 0x300`) sees bits 8/9 the same way
    // MAME does.
    let a = src as u16;
    let b = dst as u16;
    let xc = x as u16;
    // Low nibble test uses u8 semantics (bit 4 of subtraction).
    let hr_u8 = (dst & 0x0F).wrapping_sub(src & 0x0F).wrapping_sub(x);
    let lcor = (hr_u8 & 0x10) != 0;
    let r1 = b.wrapping_sub(a).wrapping_sub(xc);
    let mut r = if lcor { r1.wrapping_sub(6) } else { r1 };
    if (r1 & 0x100) != 0 {
        r = r.wrapping_sub(0x60);
    }
    let res8 = (r & 0xFF) as u8;
    // Carry / X: any of bits 8 or 9 of the corrected result set.
    let carry = (r & 0x300) != 0;
    // V flag for SBCD: post bit 7 NOT set AND raw r1 bit 7 set.
    let v = (r & 0x80) == 0 && (r1 & 0x80) != 0;
    (res8, carry, v)
}

// ─────────────────────────────────────────────────────────────────────────
// Shift / rotate (line E)
// ─────────────────────────────────────────────────────────────────────────

fn shift_rotate<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let size_bits = (op >> 6) & 0x3;
    if size_bits == 0x3 {
        return mem_shift_rotate(cpu, bus, op);
    }
    let size = match size_bits {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => unreachable!(),
    };
    let kind = (op >> 3) & 0x3;
    let left = (op & 0x0100) != 0;
    let reg = (op & 0x7) as usize;
    let ir = (op & 0x0020) != 0;
    let count_field = ((op >> 9) & 0x7) as u32;
    let count = if ir {
        cpu.d[count_field as usize] & 0x3F
    } else if count_field == 0 {
        8
    } else {
        count_field
    };
    let v = cpu.d[reg] & size.mask();
    let new = do_shift_rotate(cpu, v, count, size, kind as u8, left);
    let mask = size.mask();
    cpu.d[reg] = (cpu.d[reg] & !mask) | (new & mask);
    6 + 2 * count
}

fn mem_shift_rotate<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let kind = (op >> 9) & 0x3;
    let left = (op & 0x0100) != 0;
    let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Word);
    ea.modify(cpu, bus, Size::Word, |c, v| {
        do_shift_rotate(c, v, 1, Size::Word, kind as u8, left)
    });
    8 + ea.cycles(Size::Word)
}

fn do_shift_rotate(cpu: &mut Cpu, mut v: u32, count: u32, size: Size, kind: u8, left: bool) -> u32 {
    let mask = size.mask();
    let sign = size.sign_bit();
    let bits = size.bits();
    let mut c_flag = false;
    let mut x_flag = cpu.sr.get(StatusRegister::X);
    let mut v_flag = false;

    if count == 0 {
        cpu.sr.set_nz(v, size);
        // ROXL/ROXR (kind=2) with count=0: C ← X (MAME alu_andx behavior).
        // Other shifts/rotates: C ← 0.
        if kind == 2 {
            let x = cpu.sr.get(StatusRegister::X);
            cpu.sr.set(StatusRegister::C, x);
        } else {
            cpu.sr.set(StatusRegister::C, false);
        }
        cpu.sr.set(StatusRegister::V, false);
        return v;
    }

    for _ in 0..count {
        let old = v;
        match kind {
            0 => {
                if left {
                    c_flag = (v & sign) != 0;
                    v = (v << 1) & mask;
                    if ((old ^ v) & sign) != 0 {
                        v_flag = true;
                    }
                } else {
                    c_flag = (v & 1) != 0;
                    let s = v & sign;
                    v = (v >> 1) | s;
                }
                x_flag = c_flag;
            }
            1 => {
                if left {
                    c_flag = (v & sign) != 0;
                    v = (v << 1) & mask;
                } else {
                    c_flag = (v & 1) != 0;
                    v >>= 1;
                }
                x_flag = c_flag;
            }
            2 => {
                if left {
                    c_flag = (v & sign) != 0;
                    v = ((v << 1) | u32::from(x_flag)) & mask;
                } else {
                    c_flag = (v & 1) != 0;
                    v = (v >> 1) | (u32::from(x_flag) << (bits - 1));
                }
                x_flag = c_flag;
            }
            3 => {
                if left {
                    c_flag = (v & sign) != 0;
                    v = ((v << 1) | u32::from(c_flag)) & mask;
                } else {
                    c_flag = (v & 1) != 0;
                    v = (v >> 1) | (u32::from(c_flag) << (bits - 1));
                }
            }
            _ => unreachable!(),
        }
    }

    cpu.sr.set_nz(v, size);
    cpu.sr.set(StatusRegister::C, c_flag);
    if kind != 3 {
        cpu.sr.set(StatusRegister::X, x_flag);
    }
    cpu.sr.set(StatusRegister::V, v_flag && kind == 0);
    v
}

// ─────────────────────────────────────────────────────────────────────────
// Bit / immediate family (line 0)
// ─────────────────────────────────────────────────────────────────────────

fn bit_immediate<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    if op == 0x003C {
        let v = (cpu.fetch16(bus) & 0xFF) as u16;
        let new_ccr = (cpu.sr.0 & 0x1F) | (v & 0x1F);
        cpu.sr.0 = (cpu.sr.0 & 0xFFE0) | new_ccr;
        return 20;
    }
    if op == 0x007C {
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let v = cpu.fetch16(bus);
        write_sr(cpu, cpu.sr.0 | v);
        return 20;
    }
    if op == 0x023C {
        let v = (cpu.fetch16(bus) & 0xFF) as u16;
        let new_ccr = (cpu.sr.0 & 0x1F) & (v & 0x1F);
        cpu.sr.0 = (cpu.sr.0 & 0xFFE0) | new_ccr;
        return 20;
    }
    if op == 0x027C {
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let v = cpu.fetch16(bus);
        write_sr(cpu, cpu.sr.0 & v);
        return 20;
    }
    if op == 0x0A3C {
        let v = (cpu.fetch16(bus) & 0xFF) as u16;
        let new_ccr = (cpu.sr.0 & 0x1F) ^ (v & 0x1F);
        cpu.sr.0 = (cpu.sr.0 & 0xFFE0) | new_ccr;
        return 20;
    }
    if op == 0x0A7C {
        if !cpu.sr.supervisor() {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::PrivilegeViolation);
            return 34;
        }
        let v = cpu.fetch16(bus);
        write_sr(cpu, cpu.sr.0 ^ v);
        return 20;
    }

    let upper = (op >> 8) & 0xF;

    // MOVEP — 0000 ddd 1 0pp 001 aaa  (mask 0xF138, value 0x0108)
    if (op & 0xF138) == 0x0108 {
        return movep(cpu, bus, op);
    }
    // Static bit ops: 0000 1000 ttmm mrrr
    if (op & 0xFF00) == 0x0800 {
        let bit_n = cpu.fetch16(bus) & 0xFF;
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Byte);
        return bit_op(cpu, bus, op, ea, u32::from(bit_n));
    }
    // Dynamic bit ops: 0000 ddd1 ttmm mrrr
    if (op & 0xF100) == 0x0100 {
        let dn = ((op >> 9) & 0x7) as usize;
        let bit_n = cpu.d[dn];
        let ea = Ea::decode(cpu, bus, op & 0x3F, Size::Byte);
        return bit_op(cpu, bus, op, ea, bit_n);
    }

    let size_bits = (op >> 6) & 0x3;
    let size = match size_bits {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::IllegalInstruction);
            return 34;
        }
    };
    let imm = match size {
        Size::Byte => u32::from(cpu.fetch16(bus) & 0xFF),
        Size::Word => u32::from(cpu.fetch16(bus)),
        Size::Long => cpu.fetch32(bus),
    };
    let ea = Ea::decode(cpu, bus, op & 0x3F, size);

    // Common pushed-PC offset for *I instructions (CMPI/ORI/ANDI/SUBI/
    // ADDI/EORI) when the EA-read faults. Matches what MAME's microcode
    // (`o#wN`, `aixl*`, `adsw2`, `pdcw1`) commits to `m_pc` at fault time.
    let mode_bits = (op >> 3) & 0x7;
    let reg_bits = op & 0x7;
    let ext_bytes: u32 = match (mode_bits, reg_bits) {
        (7, 0) => 2, // (xxx).w
        (7, 1) => 4, // (xxx).l
        _ => 0,
    };
    let base: u32 = if size == Size::Long { 6 } else { 4 };
    let predec_extra: u32 = if size != Size::Long && mode_bits == 4 { 2 } else { 0 };
    let pushed_pc_offset = base + ext_bytes + predec_extra;

    // Helper: do a RMW with controlled cpu.au on both read and write.
    fn rmw_with_au<B: Bus, F>(
        cpu: &mut Cpu, bus: &mut B, ea: Ea, size: Size,
        pushed_pc_offset: u32, f: F,
    ) -> bool
    where
        F: FnOnce(&mut Cpu, u32) -> u32,
    {
        match ea {
            Ea::DataReg(_) | Ea::AddrReg(_) => {
                let v = ea.read(cpu, bus, size);
                let new = f(cpu, v);
                ea.write(cpu, bus, size, new);
                true
            }
            Ea::Immediate(_) | Ea::PcIndDisp(_) | Ea::PcIndIdx(..) => {
                // Unusual: RMW into immediate/PC-rel — read only.
                let v = ea.read(cpu, bus, size);
                if cpu.address_error.is_some() { return false; }
                f(cpu, v);
                true
            }
            _ => {
                let addr = ea.compute_address(cpu, size).expect("memory EA");
                cpu.au = cpu.instr_pc.wrapping_add(pushed_pc_offset);
                let v = crate::cpu::m68k::ea::read_at(cpu, bus, addr, size);
                if cpu.address_error.is_some() {
                    ea.undo_side_effects(cpu, size);
                    return false;
                }
                let new = f(cpu, v);
                // Write back: MAME also has `m_pc = m_au` before the
                // write for *I RMW (mmrl1-style), so the same offset
                // applies on a write fault.
                cpu.au = cpu.instr_pc.wrapping_add(pushed_pc_offset);
                crate::cpu::m68k::ea::write_at(cpu, bus, addr, size, new);
                cpu.address_error.is_none()
            }
        }
    }

    match upper {
        0x0 => {
            rmw_with_au(cpu, bus, ea, size, pushed_pc_offset, |c, v| {
                let r = (v | imm) & size.mask();
                c.sr.set_nz(r, size);
                c.sr.set(StatusRegister::V, false);
                c.sr.set(StatusRegister::C, false);
                r
            });
            8 + ea.cycles(size)
        }
        0x2 => {
            rmw_with_au(cpu, bus, ea, size, pushed_pc_offset, |c, v| {
                let r = (v & imm) & size.mask();
                c.sr.set_nz(r, size);
                c.sr.set(StatusRegister::V, false);
                c.sr.set(StatusRegister::C, false);
                r
            });
            8 + ea.cycles(size)
        }
        0x4 => {
            rmw_with_au(cpu, bus, ea, size, pushed_pc_offset, |c, v| {
                sub_with_flags(c, v, imm, size, true)
            });
            8 + ea.cycles(size)
        }
        0x6 => {
            rmw_with_au(cpu, bus, ea, size, pushed_pc_offset, |c, v| {
                add_with_flags(c, v, imm, size, true)
            });
            8 + ea.cycles(size)
        }
        0xA => {
            rmw_with_au(cpu, bus, ea, size, pushed_pc_offset, |c, v| {
                let r = (v ^ imm) & size.mask();
                c.sr.set_nz(r, size);
                c.sr.set(StatusRegister::V, false);
                c.sr.set(StatusRegister::C, false);
                r
            });
            8 + ea.cycles(size)
        }
        0xC => {
            // CMPI: read EA, then SUB for flags only. If the read
            // faults, MAME never executes sr_nzvc (that lives in
            // cpdw1, after the read), so CCR must stay untouched.
            let v = match ea {
                Ea::DataReg(_) | Ea::AddrReg(_) | Ea::Immediate(_) => {
                    ea.read(cpu, bus, size)
                }
                _ => {
                    let addr = ea.compute_address(cpu, size).expect("memory EA");
                    cpu.au = cpu.instr_pc.wrapping_add(pushed_pc_offset);
                    let v = crate::cpu::m68k::ea::read_at(cpu, bus, addr, size);
                    if cpu.address_error.is_some() {
                        ea.undo_side_effects(cpu, size);
                    }
                    v
                }
            };
            if cpu.address_error.is_some() {
                return 4;
            }
            sub_with_flags(cpu, v, imm, size, false);
            8 + ea.cycles(size)
        }
        _ => {
            cpu.pc = cpu.instr_pc;
            cpu.enter_exception(bus, Exception::IllegalInstruction);
            34
        }
    }
}

fn bit_op<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16, ea: Ea, bit_n: u32) -> u32 {
    let kind = (op >> 6) & 0x3;
    let size = if ea.is_reg_direct() { Size::Long } else { Size::Byte };
    let modulo = if size == Size::Long { 32 } else { 8 };
    let bit = bit_n % modulo;
    let mask = 1u32 << bit;

    if kind == 0 {
        let v = ea.read(cpu, bus, size);
        let was = (v & mask) != 0;
        cpu.sr.set(StatusRegister::Z, !was);
        8 + ea.cycles(size)
    } else {
        ea.modify(cpu, bus, size, |c, v| {
            let was = (v & mask) != 0;
            c.sr.set(StatusRegister::Z, !was);
            match kind {
                1 => v ^ mask,
                2 => v & !mask,
                3 => v | mask,
                _ => v,
            }
        });
        8 + ea.cycles(size)
    }
}

fn movep<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: u16) -> u32 {
    let dn = ((op >> 9) & 0x7) as usize;
    let an = (op & 0x7) as usize;
    let opmode = (op >> 6) & 0x7;
    let long = (opmode & 0x1) != 0;
    let to_mem = (opmode & 0x2) != 0;
    let disp = cpu.fetch16(bus) as i16 as i32 as u32;
    let mut addr = cpu.a[an].wrapping_add(disp);
    if to_mem {
        let v = cpu.d[dn];
        if long {
            bus.write8(addr, (v >> 24) as u8);
            addr = addr.wrapping_add(2);
            bus.write8(addr, (v >> 16) as u8);
            addr = addr.wrapping_add(2);
            bus.write8(addr, (v >> 8) as u8);
            addr = addr.wrapping_add(2);
            bus.write8(addr, v as u8);
            24
        } else {
            bus.write8(addr, (v >> 8) as u8);
            addr = addr.wrapping_add(2);
            bus.write8(addr, v as u8);
            16
        }
    } else if long {
        let b3 = bus.read8(addr) as u32;
        let b2 = bus.read8(addr.wrapping_add(2)) as u32;
        let b1 = bus.read8(addr.wrapping_add(4)) as u32;
        let b0 = bus.read8(addr.wrapping_add(6)) as u32;
        cpu.d[dn] = (b3 << 24) | (b2 << 16) | (b1 << 8) | b0;
        24
    } else {
        let b1 = bus.read8(addr) as u32;
        let b0 = bus.read8(addr.wrapping_add(2)) as u32;
        cpu.d[dn] = (cpu.d[dn] & 0xFFFF_0000) | (b1 << 8) | b0;
        16
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Flag helpers
// ─────────────────────────────────────────────────────────────────────────

/// Predecrement-read helper shared by ADDX/SUBX with `-(Ay), -(Ax)`.
///
/// For Byte/Word: the predec to An is committed BEFORE the read fires,
/// so a misaligned-read fault leaves An already decremented.
///
/// For Long: MAME's microcode reads the high word from `An - 2`
/// WITHOUT committing An first; only after the high-word read
/// succeeds does it commit `An = An - 4` and then read the low word
/// from `An - 4`. A fault on the *first* read therefore leaves An
/// unchanged, but a fault on the *second* leaves An at An-4.
fn subx_addx_predec_read<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    size: Size,
    sy: usize,
    dx: usize,
) -> (u32, u32) {
    fn dec_for(reg: usize, size: Size) -> u32 {
        if reg == 7 && size == Size::Byte { 2 } else { size.bytes() }
    }
    if size == Size::Long {
        // MAME asxl2 reads from Ay-2 FIRST (= LOW word, since big-endian
        // puts the high word at Ay-4 and low word at Ay-2). Ay is NOT
        // committed at this point; a fault here leaves Ay unchanged.
        // asxl3 then commits Ay = Ay-4 and reads HIGH word from Ay-4.
        // ----- source (Ay) LOW word from Ay-2 -----
        let lo_addr_y = cpu.a[sy].wrapping_sub(2);
        let lo = crate::cpu::m68k::ea::read_at(cpu, bus, lo_addr_y, Size::Word);
        if cpu.address_error.is_some() {
            return (0, 0);
        }
        cpu.a[sy] = cpu.a[sy].wrapping_sub(4);
        let hi = crate::cpu::m68k::ea::read_at(cpu, bus, cpu.a[sy], Size::Word);
        if cpu.address_error.is_some() {
            return (0, 0);
        }
        let s = (hi << 16) | lo;
        // ----- dest (Ax) LOW word from Ax-2 -----
        let lo_addr_x = cpu.a[dx].wrapping_sub(2);
        let lo = crate::cpu::m68k::ea::read_at(cpu, bus, lo_addr_x, Size::Word);
        if cpu.address_error.is_some() {
            return (0, 0);
        }
        cpu.a[dx] = cpu.a[dx].wrapping_sub(4);
        let hi = crate::cpu::m68k::ea::read_at(cpu, bus, cpu.a[dx], Size::Word);
        if cpu.address_error.is_some() {
            return (0, 0);
        }
        let d = (hi << 16) | lo;
        (s, d)
    } else {
        let dec_y = dec_for(sy, size);
        cpu.a[sy] = cpu.a[sy].wrapping_sub(dec_y);
        let s = crate::cpu::m68k::ea::read_at(cpu, bus, cpu.a[sy], size);
        if cpu.address_error.is_some() {
            return (0, 0);
        }
        let dec_x = dec_for(dx, size);
        cpu.a[dx] = cpu.a[dx].wrapping_sub(dec_x);
        let d = crate::cpu::m68k::ea::read_at(cpu, bus, cpu.a[dx], size);
        if cpu.address_error.is_some() {
            return (0, 0);
        }
        (s, d)
    }
}

pub fn add_with_flags(cpu: &mut Cpu, a: u32, b: u32, size: Size, update_x: bool) -> u32 {
    let am = a & size.mask();
    let bm = b & size.mask();
    let r_full = am.wrapping_add(bm);
    let r = r_full & size.mask();
    let sign = size.sign_bit();
    let carry = match size {
        Size::Byte => (r_full & 0x100) != 0,
        Size::Word => (r_full & 0x1_0000) != 0,
        Size::Long => (am as u64 + bm as u64) > 0xFFFF_FFFF,
    };
    let overflow = ((!(am ^ bm)) & (am ^ r) & sign) != 0;
    cpu.sr.set_nz(r, size);
    cpu.sr.set(StatusRegister::V, overflow);
    cpu.sr.set(StatusRegister::C, carry);
    if update_x {
        cpu.sr.set(StatusRegister::X, carry);
    }
    r
}

pub fn sub_with_flags(cpu: &mut Cpu, a: u32, b: u32, size: Size, update_x: bool) -> u32 {
    let am = a & size.mask();
    let bm = b & size.mask();
    let r = am.wrapping_sub(bm) & size.mask();
    let sign = size.sign_bit();
    let borrow = bm > am;
    let overflow = ((am ^ bm) & (am ^ r) & sign) != 0;
    cpu.sr.set_nz(r, size);
    cpu.sr.set(StatusRegister::V, overflow);
    cpu.sr.set(StatusRegister::C, borrow);
    if update_x {
        cpu.sr.set(StatusRegister::X, borrow);
    }
    r
}

pub fn addx_with_flags(cpu: &mut Cpu, dst: u32, src: u32, x: u32, size: Size) -> u32 {
    let am = dst & size.mask();
    let bm = src & size.mask();
    let r_full = am.wrapping_add(bm).wrapping_add(x);
    let r = r_full & size.mask();
    let sign = size.sign_bit();
    let carry = match size {
        Size::Byte => (r_full & 0x100) != 0,
        Size::Word => (r_full & 0x1_0000) != 0,
        Size::Long => (am as u64 + bm as u64 + x as u64) > 0xFFFF_FFFF,
    };
    let overflow = ((!(am ^ bm)) & (am ^ r) & sign) != 0;
    let prev_z = cpu.sr.get(StatusRegister::Z);
    cpu.sr.set(StatusRegister::N, (r & sign) != 0);
    if r != 0 {
        cpu.sr.set(StatusRegister::Z, false);
    } else {
        cpu.sr.set(StatusRegister::Z, prev_z);
    }
    cpu.sr.set(StatusRegister::V, overflow);
    cpu.sr.set(StatusRegister::C, carry);
    cpu.sr.set(StatusRegister::X, carry);
    r
}

pub fn subx_with_flags(cpu: &mut Cpu, dst: u32, src: u32, x: u32, size: Size) -> u32 {
    let am = dst & size.mask();
    let bm = src & size.mask();
    let r = am.wrapping_sub(bm).wrapping_sub(x) & size.mask();
    let sign = size.sign_bit();
    let borrow = (bm as u64 + x as u64) > am as u64;
    let overflow = ((am ^ bm) & (am ^ r) & sign) != 0;
    let prev_z = cpu.sr.get(StatusRegister::Z);
    cpu.sr.set(StatusRegister::N, (r & sign) != 0);
    if r != 0 {
        cpu.sr.set(StatusRegister::Z, false);
    } else {
        cpu.sr.set(StatusRegister::Z, prev_z);
    }
    cpu.sr.set(StatusRegister::V, overflow);
    cpu.sr.set(StatusRegister::C, borrow);
    cpu.sr.set(StatusRegister::X, borrow);
    r
}

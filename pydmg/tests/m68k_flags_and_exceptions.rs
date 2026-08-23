//! Tests focused on CCR flag semantics and exception handling — the parts
//! most likely to break subtle Neo Geo BIOS code.

use pydmg_neogeo::cpu::m68k::bus::{Bus, FlatBus};
use pydmg_neogeo::cpu::m68k::cpu::{Cpu, StatusRegister};

fn build_at_400(prog: &[u8]) -> (Cpu, FlatBus) {
    let mut bus = FlatBus::new();
    bus.write32(0x0000_0000, 0x0010_0000); // SSP
    bus.write32(0x0000_0004, 0x0000_0400); // PC
    bus.load(0x0400, prog);
    let mut cpu = Cpu::new();
    cpu.reset(&mut bus);
    (cpu, bus)
}

#[test]
fn addi_sets_carry_on_overflow() {
    // move.l #$FFFFFFFF, d0  -> 20 3C FF FF FF FF
    // addi.l #1, d0          -> 06 80 00 00 00 01
    let (mut cpu, mut bus) = build_at_400(&[
        0x20, 0x3C, 0xFF, 0xFF, 0xFF, 0xFF,
        0x06, 0x80, 0x00, 0x00, 0x00, 0x01,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0);
    assert!(cpu.sr.get(StatusRegister::Z), "Z should be set");
    assert!(cpu.sr.get(StatusRegister::C), "C should be set on long overflow");
    assert!(cpu.sr.get(StatusRegister::X), "X should mirror C");
}

#[test]
fn subi_sets_borrow() {
    // moveq #0, d0 ; subi.b #1, d0
    let (mut cpu, mut bus) = build_at_400(&[
        0x70, 0x00,
        0x04, 0x00, 0x00, 0x01,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF);
    assert!(cpu.sr.get(StatusRegister::N));
    assert!(cpu.sr.get(StatusRegister::C));
    assert!(cpu.sr.get(StatusRegister::X));
    assert!(!cpu.sr.get(StatusRegister::Z));
}

#[test]
fn signed_overflow_sets_v() {
    // move.l #$7FFFFFFF, d0 ; addi.l #1, d0  -> overflow positive→negative
    let (mut cpu, mut bus) = build_at_400(&[
        0x20, 0x3C, 0x7F, 0xFF, 0xFF, 0xFF,
        0x06, 0x80, 0x00, 0x00, 0x00, 0x01,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0x8000_0000);
    assert!(cpu.sr.get(StatusRegister::V));
    assert!(cpu.sr.get(StatusRegister::N));
    assert!(!cpu.sr.get(StatusRegister::C));
}

#[test]
fn illegal_opcode_triggers_exception_vector_4() {
    // Place a known-illegal opcode at $400. The CPU should jump to the
    // address stored in vector 4 ($000010).
    let (mut cpu, mut bus) = build_at_400(&[0x4A, 0xFC]);
    // Vector 4 → $00009000 (anywhere). We pre-load it.
    bus.write32(0x0000_0010, 0x0000_9000);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0000_9000, "Illegal instruction must go through vector 4");
    assert!(cpu.sr.get(StatusRegister::S), "Must be in supervisor mode");
}

#[test]
fn vblank_interrupt_jumps_to_level1_vector() {
    // The CPU should service an IRQ at instruction boundary if level > mask.
    // We pre-load vector 25 ($000064) and request IRQ 1.
    let (mut cpu, mut bus) = build_at_400(&[0x4E, 0x71]); // NOP, will be skipped
    bus.write32(0x0000_0064, 0x0000_A000);
    // Make sure the SR mask allows level 1 — clear I bits.
    cpu.sr.set_interrupt_mask(0);
    cpu.request_interrupt(1);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0000_A000, "Should jump to VBlank vector");
    assert_eq!(cpu.sr.interrupt_mask(), 1, "Mask should now equal serviced level");
}

#[test]
fn higher_level_irq_preempts_lower() {
    let (mut cpu, mut bus) = build_at_400(&[0x4E, 0x71]);
    bus.write32(0x0000_0064, 0x0000_A000);
    bus.write32(0x0000_0068, 0x0000_B000);
    cpu.sr.set_interrupt_mask(0);
    cpu.request_interrupt(1);
    cpu.request_interrupt(2);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0000_B000, "Level 2 must preempt Level 1");
}

#[test]
fn rts_pops_pc_from_stack() {
    // We synthesize a return: push $C000 onto SSP, then execute RTS.
    let (mut cpu, mut bus) = build_at_400(&[0x4E, 0x75]);
    // push $0000C000 manually
    cpu.push32(&mut bus, 0x0000_C000);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0000_C000);
}

#[test]
fn stop_sets_stopped_flag_and_writes_sr() {
    // stop #$2700 — privileged, but we boot in supervisor mode after reset
    // Encoding: 4E 72 27 00
    let (mut cpu, mut bus) = build_at_400(&[0x4E, 0x72, 0x27, 0x00]);
    cpu.step(&mut bus);
    assert!(cpu.stopped, "Should be in stopped state");
    assert_eq!(cpu.sr.0, 0x2700);
}

#[test]
fn stop_resumes_on_interrupt() {
    let (mut cpu, mut bus) = build_at_400(&[0x4E, 0x72, 0x20, 0x00]);
    bus.write32(0x0000_0064, 0x0000_D000);
    cpu.step(&mut bus); // STOP, mask becomes 0 (#$2000 in CCR)
    assert!(cpu.stopped);
    cpu.request_interrupt(1);
    cpu.step(&mut bus);
    assert!(!cpu.stopped, "IRQ should wake up the CPU");
    assert_eq!(cpu.pc, 0x0000_D000);
}

#[test]
fn move_to_sr_changes_supervisor_state_and_stack() {
    // move #$0000, sr  — drops out of supervisor (S=0)
    // 46 FC 00 00
    let (mut cpu, mut bus) = build_at_400(&[0x46, 0xFC, 0x00, 0x00]);
    cpu.usp = 0x0050_0000;
    let old_ssp = cpu.a[7];
    cpu.step(&mut bus);
    assert!(!cpu.sr.supervisor(), "Should have left supervisor mode");
    assert_eq!(cpu.a[7], 0x0050_0000, "A7 should now hold USP");
    assert_eq!(cpu.ssp, old_ssp, "SSP must be saved into shadow");
}

#[test]
fn dbra_eq_falls_through_when_condition_true() {
    // moveq #5, d0 ; dbeq d0, -2 (back to self if Z=0)
    // 70 05 / 57 C8 FF FE
    // After moveq, Z=0, so dbeq decrements d0 and branches until d0 = -1
    let (mut cpu, mut bus) = build_at_400(&[
        0x70, 0x05,
        0x57, 0xC8, 0xFF, 0xFE,
    ]);
    cpu.step(&mut bus);
    for _ in 0..10 {
        cpu.step(&mut bus);
    }
    // dbeq exits when d0 underflows to -1
    assert_eq!(cpu.d[0] as i32 & 0xFFFF, 0xFFFF);
}

#[test]
fn andi_to_ccr() {
    // moveq #-1, d0 (sets N=1 Z=0) ; andi.b #$00, ccr
    // 70 FF / 02 3C 00 00
    let (mut cpu, mut bus) = build_at_400(&[
        0x70, 0xFF,
        0x02, 0x3C, 0x00, 0x00,
    ]);
    cpu.step(&mut bus);
    assert!(cpu.sr.get(StatusRegister::N));
    cpu.step(&mut bus);
    // CCR should be cleared
    assert_eq!(cpu.sr.0 & 0x1F, 0);
}

#[test]
fn link_unlk_round_trip() {
    // link a0, #-8     ; 4E 50 FF F8
    // unlk a0          ; 4E 58
    let (mut cpu, mut bus) = build_at_400(&[0x4E, 0x50, 0xFF, 0xF8, 0x4E, 0x58]);
    cpu.a[0] = 0x1234_5678;
    let sp_before = cpu.a[7];
    cpu.step(&mut bus); // LINK
    assert_eq!(cpu.a[0], sp_before.wrapping_sub(4), "A0 should hold new frame ptr");
    assert_eq!(cpu.a[7], sp_before.wrapping_sub(4).wrapping_sub(8));
    cpu.step(&mut bus); // UNLK
    assert_eq!(cpu.a[7], sp_before);
    assert_eq!(cpu.a[0], 0x1234_5678, "A0 should be restored");
}

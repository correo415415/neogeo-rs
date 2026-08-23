//! Integration tests for the M68K core. Each test loads a small program at
//! address $1000, sets up the reset vectors so the CPU boots there, then
//! steps the CPU and asserts on register/memory state.

use pydmg_neogeo::cpu::m68k::bus::{Bus, FlatBus};
use pydmg_neogeo::cpu::m68k::cpu::{Cpu, StatusRegister};

fn build(prog_at_1000: &[u8]) -> (Cpu, FlatBus) {
    let mut bus = FlatBus::new();
    // Initial SSP & PC vectors
    bus.write32(0x0000_0000, 0x0010_0000);
    bus.write32(0x0000_0004, 0x0000_1000);
    bus.load(0x1000, prog_at_1000);
    let mut cpu = Cpu::new();
    cpu.reset(&mut bus);
    (cpu, bus)
}

#[test]
fn moveq_loads_signed_byte() {
    // moveq #-1, d0   -> 70 FF
    let (mut cpu, mut bus) = build(&[0x70, 0xFF]);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_FFFF);
    assert!(cpu.sr.get(StatusRegister::N));
    assert!(!cpu.sr.get(StatusRegister::Z));
}

#[test]
fn move_long_immediate_to_d_reg() {
    // move.l #$12345678, d1   -> 22 3C 12 34 56 78
    let (mut cpu, mut bus) = build(&[0x22, 0x3C, 0x12, 0x34, 0x56, 0x78]);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[1], 0x1234_5678);
}

#[test]
fn add_word_d0_to_d1() {
    // move.w #$0010, d0 ; move.w #$0005, d1 ; add.w d0, d1
    // 30 3C 00 10  /  32 3C 00 05  /  D2 40
    let (mut cpu, mut bus) = build(&[
        0x30, 0x3C, 0x00, 0x10,
        0x32, 0x3C, 0x00, 0x05,
        0xD2, 0x40,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0x15);
}

#[test]
fn sub_long_immediate() {
    // move.l #$00000010, d0 ; subi.l #$00000005, d0
    // 20 3C 00 00 00 10  /  04 80 00 00 00 05
    let (mut cpu, mut bus) = build(&[
        0x20, 0x3C, 0x00, 0x00, 0x00, 0x10,
        0x04, 0x80, 0x00, 0x00, 0x00, 0x05,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0x0B);
}

#[test]
fn bra_short() {
    // bra.s +2 (skip the addq below)   60 02   ; disp is from PC after opcode
    // addq.l #1, d0                    52 80
    // moveq #2, d0                     70 02
    let (mut cpu, mut bus) = build(&[
        0x60, 0x02,
        0x52, 0x80,
        0x70, 0x02,
    ]);
    cpu.step(&mut bus); // BRA
    cpu.step(&mut bus); // moveq
    assert_eq!(cpu.d[0], 2);
}

#[test]
fn bsr_and_rts_round_trip() {
    // 0x1000: bsr.s +4   -> jumps to 0x1006     61 04
    // 0x1002: moveq #1, d0                       70 01
    // 0x1004: bra.s +2    -> jumps to 0x1008     60 02
    // 0x1006: moveq #7, d0                       70 07
    // 0x1008: rts                                4E 75
    let (mut cpu, mut bus) = build(&[
        0x61, 0x04,
        0x70, 0x01,
        0x60, 0x02,
        0x70, 0x07,
        0x4E, 0x75,
    ]);
    cpu.step(&mut bus); // BSR
    cpu.step(&mut bus); // moveq #7 inside sub
    cpu.step(&mut bus); // RTS
    cpu.step(&mut bus); // moveq #1
    assert_eq!(cpu.d[0], 1);
}

#[test]
fn cmp_sets_flags_correctly() {
    // moveq #5, d0 ; cmp.l #5, d0
    // 70 05   /  0C 80 00 00 00 05
    let (mut cpu, mut bus) = build(&[
        0x70, 0x05,
        0x0C, 0x80, 0x00, 0x00, 0x00, 0x05,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert!(cpu.sr.get(StatusRegister::Z));
    assert!(!cpu.sr.get(StatusRegister::N));
    assert!(!cpu.sr.get(StatusRegister::C));
}

#[test]
fn lea_loads_address() {
    // lea $1234.w, a0   -> 41 F8 12 34
    let (mut cpu, mut bus) = build(&[0x41, 0xF8, 0x12, 0x34]);
    cpu.step(&mut bus);
    assert_eq!(cpu.a[0], 0x0000_1234);
}

#[test]
fn jmp_absolute_long() {
    // jmp $00002000   -> 4E F9 00 00 20 00
    let (mut cpu, mut bus) = build(&[0x4E, 0xF9, 0x00, 0x00, 0x20, 0x00]);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0000_2000);
}

#[test]
fn swap_exchanges_word_halves() {
    // move.l #$AAAA5555, d0 ; swap d0
    let (mut cpu, mut bus) = build(&[
        0x20, 0x3C, 0xAA, 0xAA, 0x55, 0x55,
        0x48, 0x40,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0x5555_AAAA);
}

#[test]
fn ext_word_sign_extends_byte() {
    // moveq #-1, d0 (now $FFFFFFFF) ; ext.w d0 -> low word $FFFF
    let (mut cpu, mut bus) = build(&[
        0x70, 0xFF,
        0x48, 0x80,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
}

#[test]
fn dbf_loop_counts_down() {
    // moveq #3, d0
    // <loop:> dbf d0, loop      ; 51 C8 FF FE
    let (mut cpu, mut bus) = build(&[
        0x70, 0x03,
        0x51, 0xC8, 0xFF, 0xFE,
    ]);
    cpu.step(&mut bus);
    for _ in 0..5 {
        cpu.step(&mut bus);
    }
    // After loop, low word should be $FFFF (-1)
    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
}

#[test]
fn lsr_clears_high_bits() {
    // moveq #-1, d0  ;  lsr.l #4, d0
    let (mut cpu, mut bus) = build(&[
        0x70, 0xFF,
        0xE8, 0x88,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0x0FFF_FFFF);
}

#[test]
fn bset_sets_bit_in_memory_byte() {
    // moveq #3, d0 ; bset d0, $2000.l
    //   70 03   /   01 F9 00 00 20 00
    let (mut cpu, mut bus) = build(&[
        0x70, 0x03,
        0x01, 0xF9, 0x00, 0x00, 0x20, 0x00,
    ]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x2000), 0x08);
}

#[test]
fn movem_save_and_restore_d0_d2() {
    // movem.l d0-d2, -(sp)        48 E7 E0 00
    // moveq #0, d0                70 00
    // moveq #0, d1                72 00
    // moveq #0, d2                74 00
    // movem.l (sp)+, d0-d2        4C DF 00 07
    // Prefill registers first via moveqs:
    //  moveq #1, d0  ; moveq #2, d1 ; moveq #3, d2
    let (mut cpu, mut bus) = build(&[
        0x70, 0x01,
        0x72, 0x02,
        0x74, 0x03,
        0x48, 0xE7, 0xE0, 0x00,
        0x70, 0x00,
        0x72, 0x00,
        0x74, 0x00,
        0x4C, 0xDF, 0x00, 0x07,
    ]);
    for _ in 0..8 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.d[0] as i32, 1);
    assert_eq!(cpu.d[1] as i32, 2);
    assert_eq!(cpu.d[2] as i32, 3);
}

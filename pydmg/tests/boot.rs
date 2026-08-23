//! Integration test: build a tiny synthetic "cartridge" P-ROM, plug it into
//! the Neo Geo bus, and verify the 68000 boots from it and executes the
//! expected program. This proves the full pipeline (vector load -> fetch ->
//! decode -> EA -> memory write) works end-to-end on the real bus map.

use pydmg_neogeo::cpu::m68k::bus::Bus;
use pydmg_neogeo::neogeo::system::SystemConfig;
use pydmg_neogeo::{RomSet, System};

#[test]
fn synthetic_cart_writes_to_work_ram() {
    // P-ROM layout:
    //   $000000-$0003FF  Vector table (we fill SSP and PC)
    //   $000400          Code: move.l #$DEADBEEF, $100000.l ; bra.s self
    //
    // Encoding:
    //   move.l #$DEADBEEF, ($100000).l : 23 FC DE AD BE EF 00 10 00 00
    //   bra.s -2 (self loop)           : 60 FE
    // P-ROM data is stored on disk with **swapped byte pairs** (this is the
    // format that MAME/FBNeo `.bin` cart dumps use — the 68k reads each
    // 16-bit word big-endian from the bus, but the underlying ROM chips are
    // little-endian word storage). `NeoGeoBus::load_p_rom` byte-swaps each
    // word during load to undo this. So our synthetic ROM has to start in
    // the *on-disk* (little-endian-per-word) format too, then become big-endian
    // after `load_p_rom`.
    //
    // SSP=$00100FFC big-endian -> bytes 00 10 0F FC after `load`
    //                              -> stored as 10 00 FC 0F on disk
    // PC =$00000400 big-endian -> bytes 00 00 04 00
    //                              -> stored as 00 00 00 04 on disk
    let mut p_rom = vec![0xFFu8; 0x1000];
    p_rom[0] = 0x10; p_rom[1] = 0x00; p_rom[2] = 0xFC; p_rom[3] = 0x0F;
    p_rom[4] = 0x00; p_rom[5] = 0x00; p_rom[6] = 0x00; p_rom[7] = 0x04;
    // Code at $400 — opcodes are also stored swapped per-16-bit-word in the
    // on-disk image. The 68000 sees:
    //   23 FC DE AD BE EF 00 10 00 00     (MOVE.L #$DEADBEEF, ($100000).l)
    //   60 FE                              (BRA.S to self)
    let code_be: &[u8] = &[0x23, 0xFC, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x10, 0x00, 0x00, 0x60, 0xFE];
    for (i, chunk) in code_be.chunks(2).enumerate() {
        let swapped = if chunk.len() == 2 { [chunk[1], chunk[0]] } else { [chunk[0], 0xFF] };
        let off = 0x400 + i * 2;
        p_rom[off] = swapped[0];
        p_rom[off + 1] = swapped[1];
    }

    let mut sys = System::new(SystemConfig::default());
    let mut rs = RomSet::default();
    rs.cart.p_rom = p_rom;
    sys.load(rs).unwrap();
    sys.reset();

    assert_eq!(sys.m68k.pc, 0x0000_0400);
    assert_eq!(sys.m68k.a[7], 0x0010_0FFC);

    // Execute MOVE.L + BRA
    sys.step();
    sys.step();

    // Verify the magic number landed in work-RAM.
    let v = sys.bus.read32(0x0010_0000);
    assert_eq!(v, 0xDEAD_BEEF, "MOVE.L did not write to work RAM");

    // PC should be looping on the BRA forever (PC of opcode = $40A, after fetch = $40C, disp = -2, target = $40A).
    let pc_before = sys.m68k.pc;
    sys.step();
    assert_eq!(sys.m68k.pc, pc_before);
}

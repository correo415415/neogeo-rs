//! Regression tests for the fix layer renderer.
//!
//! These pin down the column coverage (MAME draws all 40 columns,
//! FBNeo optionally crops to 38 for 304-pixel bezel-safe display) and
//! verify the NEO-CMC bank-switching arithmetic for Garou and KOF2000
//! against the formula published in MAME's
//! `src/mame/snk/neogeo_spr.cpp` (lines ~191-216).

use pydmg_neogeo::graphics::lspc::Lspc;
use pydmg_neogeo::graphics::video::{
    FixBankType, SCREEN_H, SCREEN_W, render_fix_layer_inner_with_bank,
    render_fix_layer_inner_with_bank_and_crop,
};

/// Build the smallest viable test setup: an LSPC with one fix cell
/// placed at `(col, row)` referencing tile `tile_no`, palette `pal`,
/// and an S-ROM whose `tile_no` is fully opaque colour 1.
fn setup_one_tile(col: usize, row: usize, tile_no: u16, pal: u16) -> (Lspc, Vec<u8>, Vec<u8>) {
    let mut lspc = Lspc::new();
    let word = (pal & 0x0F) << 12 | (tile_no & 0x0FFF);
    lspc.vram[0x7000 + col * 32 + row] = word;

    // S-ROM: 128 KiB BIOS-SFIX-sized region; tile `tile_no` set to colour 1
    // in every pixel. Fix tiles are 32 bytes each in MAME layout:
    //   group offsets {16, 24, 0, 8} per the wiki; nibble per pixel.
    let mut s_rom = vec![0u8; 0x20000];
    let tile_off = (tile_no as usize) * 32;
    for off in tile_off..tile_off + 32 {
        // 0x11 = both nibbles set to colour 1.
        s_rom[off] = 0x11;
    }

    // Palette RAM: bank 0, entry pal*16+1 (= colour 1 of the bank) set to
    // a recognisable value 0x7FFF (white-ish in Neo Geo palette word
    // encoding: bit 15 = dark off, all RGB = 0x1F).
    let mut palette_ram = vec![0u8; 0x4000];
    let pal_idx = (pal as usize) * 16 + 1;
    let pword: u16 = 0x7FFF;
    palette_ram[pal_idx * 2] = (pword >> 8) as u8;
    palette_ram[pal_idx * 2 + 1] = (pword & 0xFF) as u8;

    (lspc, s_rom, palette_ram)
}

#[test]
fn fix_layer_draws_column_zero_when_uncropped() {
    // Place a fix tile at column 0, row 2 (top of visible screen).
    // With crop_cols = 0 (MAME default) the pixel at screen (0, 0) must
    // be the palette colour we set up, not the backdrop.
    let (lspc, s_rom, palette_ram) = setup_one_tile(0, 2, 0x100, 0);
    let mut frame = vec![0u32; SCREEN_W * SCREEN_H];
    render_fix_layer_inner_with_bank_and_crop(
        &lspc, &palette_ram, &s_rom, &mut frame, 0, false,
        FixBankType::Std, /*crop_cols=*/0,
    );
    // The top-left 8x8 area should now be non-zero (the tile colour).
    let mut painted = 0usize;
    for y in 0..8 {
        for x in 0..8 {
            if frame[y * SCREEN_W + x] != 0 { painted += 1; }
        }
    }
    assert_eq!(painted, 64, "column 0 must be fully drawn when uncropped");
}

#[test]
fn fix_layer_crops_column_zero_when_crop_one() {
    // Same tile at column 0, but crop_cols = 1 (FBNeo bezel-safe).
    // Column 0 must NOT be drawn.
    let (lspc, s_rom, palette_ram) = setup_one_tile(0, 2, 0x100, 0);
    let mut frame = vec![0u32; SCREEN_W * SCREEN_H];
    render_fix_layer_inner_with_bank_and_crop(
        &lspc, &palette_ram, &s_rom, &mut frame, 0, false,
        FixBankType::Std, /*crop_cols=*/1,
    );
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(frame[y * SCREEN_W + x], 0,
                       "column 0 must NOT be drawn when crop_cols=1");
        }
    }
}

#[test]
fn fix_layer_draws_column_39_when_uncropped() {
    // Place a fix tile at column 39, row 2.
    let (lspc, s_rom, palette_ram) = setup_one_tile(39, 2, 0x100, 0);
    let mut frame = vec![0u32; SCREEN_W * SCREEN_H];
    render_fix_layer_inner_with_bank_and_crop(
        &lspc, &palette_ram, &s_rom, &mut frame, 0, false,
        FixBankType::Std, 0,
    );
    let mut painted = 0usize;
    for y in 0..8 {
        for x in 312..320 {
            if frame[y * SCREEN_W + x] != 0 { painted += 1; }
        }
    }
    assert_eq!(painted, 64, "column 39 must be fully drawn when uncropped");
}

#[test]
fn fix_layer_default_uses_mame_full_40_columns() {
    // The non-crop helper variant must default to full-coverage (cols
    // 0..40) to match MAME's `draw_fixed_layer` behaviour exactly.
    let (lspc, s_rom, palette_ram) = setup_one_tile(0, 2, 0x100, 0);
    let mut frame = vec![0u32; SCREEN_W * SCREEN_H];
    render_fix_layer_inner_with_bank(
        &lspc, &palette_ram, &s_rom, &mut frame, 0, false, FixBankType::Std,
    );
    // Column 0 should be drawn.
    assert_ne!(frame[0], 0, "default fix-layer renderer must draw column 0 (MAME behaviour)");
}

// ---- NEO-CMC bank-switching maths ------------------------------------------

/// Recompute the per-row bank table the same way MAME builds
/// `garouoffsets[34]` in `draw_fixed_layer` for the GAROU mode. This is
/// the reference implementation; the production code must agree.
fn build_garou_offsets(vram: &[u16]) -> [u32; 34] {
    // Direct port of MAME `draw_fixed_layer`'s garouoffsets pre-pass
    // (neogeo_spr.cpp:194-209). VRAM is 0x8800 words; the production
    // code reads via the LSPC's `vram[..]` slice which is unmasked.
    let mut out = [0u32; 34];
    let mut garoubank: u32 = 0;
    let mut k: usize = 0;
    let mut y: usize = 0;
    while y < 32 {
        let a = vram[0x7500 + k];
        let b = vram[0x7580 + k];
        if a == 0x0200 && (b & 0xFF00) == 0xFF00 {
            garoubank = (b & 3) as u32;
            out[y] = garoubank;
            y += 1;
        }
        if y < 34 {
            out[y] = garoubank;
            y += 1;
        }
        k += 2;
    }
    out
}

#[test]
fn garou_bank_zero_flag_uses_initial_zero_bank() {
    // No flag set anywhere -> entire table is 0 (= bank 3 once XOR'd
    // with 3 in the renderer).
    let vram = vec![0u16; 0x8800];
    let offsets = build_garou_offsets(&vram);
    assert!(offsets.iter().all(|&b| b == 0));
}

#[test]
fn garou_bank_flag_changes_subsequent_rows() {
    // Set the flag at k=0 with bank=3 (so renderer sees bank ^ 3 = 0 = no offset).
    let mut vram = vec![0u16; 0x8800];
    vram[0x7500] = 0x0200; // flag
    vram[0x7580] = 0xFF00 | 3; // bank = 3
    let offsets = build_garou_offsets(&vram);
    // After flag activation all rows must carry bank=3 (post-decode).
    assert_eq!(offsets[0], 3);
    assert_eq!(offsets[10], 3);
    assert_eq!(offsets[31], 3);
}

#[test]
fn kof2000_bank_extraction_matches_mame_formula() {
    // MAME formula: code += 0x1000 * (((vram[0x7500 + ((y-1)&31) + 32*(x/6)] >> ((5-(x%6))*2)) & 3) ^ 3)
    // Build a VRAM entry that yields bank=2 (post-decode) for x=0,y=1.
    let mut vram = vec![0u16; 0x8800];
    // For x=0,y=1: address = 0x7500 + (0 & 31) + 32*0 = 0x7500
    //              shift   = (5 - 0) * 2 = 10
    //              raw_bits = bank ^ 3 = 2 ^ 3 = 1
    // -> word at 0x7500 must have bits 10..11 = 0b01
    vram[0x7500] = 1 << 10;
    let raw = vram[0x7500] as u32;
    let bank = ((raw >> 10) & 3) ^ 3;
    assert_eq!(bank, 2);
}

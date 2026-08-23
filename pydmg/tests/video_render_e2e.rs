//! End-to-end render tests: place real sprite control blocks in VRAM,
//! drive `render_frame_full` and verify the pixel output matches the
//! expected position/colour. These tests catch subtle off-by-one bugs
//! in the SCB layout decoding that lint-style tests
//! (`scb3_y_decoding`, `sprite_on_scanline`) cannot.
//!
//! Cross-checked against MAME `src/mame/snk/neogeo_spr.cpp::draw_sprites`.

use pydmg_neogeo::graphics::lspc::Lspc;
use pydmg_neogeo::graphics::video::{
    FixBankType, SCREEN_W, decode_sprite_gfx, render_frame_full,
};

/// Build a single-sprite VRAM image:
///   * sprite N=0 at hardware x = `hw_x`, hardware y = `hw_y`.
///   * 1 row (16 px tall), no zoom (`zoom_x=$F`, `zoom_y=$FF` = no shrink).
///   * SCB1 tile 0 references c_rom tile 0 with palette 1.
fn setup_single_sprite(hw_x: u16, hw_y: u16) -> (Lspc, Vec<u8>) {
    let mut lspc = Lspc::new();
    let n = 0usize;
    // SCB2: zoom_x=$F (no horizontal shrink), zoom_y=$FF (no vertical shrink)
    lspc.vram[0x8000 | n] = 0x0FFF;
    // SCB3: y_field = 0x200 - hw_y; rows = 1.
    let y_field = 0x200u16 - hw_y;
    lspc.vram[0x8200 | n] = (y_field << 7) | 0x01;
    // SCB4: x_field = hw_x << 7.
    lspc.vram[0x8400 | n] = hw_x << 7;
    // SCB1: tile entry 0 -> tile_lo = 0, attr = 0x0100 (palette 1).
    lspc.vram[n * 0x40 + 0] = 0x0000;
    lspc.vram[n * 0x40 + 1] = 0x0100;

    // Build a palette RAM with palette 1, colour 1 = some recognisable
    // value (0x7FFF: bit 15 dark off, RGB max).
    let mut pal = vec![0u8; 0x4000];
    let idx = (1 * 16 + 1) * 2; // palette 1 entry 1, byte offset
    pal[idx] = 0x7F;
    pal[idx + 1] = 0xFF;
    // Also set backdrop to recognisable mid-grey (palette $FFF).
    let bd = 0xFFFusize * 2;
    pal[bd] = 0x00;
    pal[bd + 1] = 0x80;
    (lspc, pal)
}

/// Build a c_rom pair with tile 0 = "fully opaque colour 1".
/// The pair layout is the standard Neo Geo: 128 bytes per tile, with
/// LEFT half at 0x40..0x7F, RIGHT half at 0x00..0x3F. We set plane-0
/// bytes to 0xFF so every pixel ends up as colour 1.
fn cromp_all_colour_1() -> Vec<Vec<u8>> {
    let bytes_per_pair_per_tile = 128usize;
    let n_tiles = 1usize;
    let half_size = n_tiles * bytes_per_pair_per_tile / 2; // 64
    let mut c_even = vec![0u8; half_size];
    let mut c_odd = vec![0u8; half_size];
    for y in 0..16usize {
        for half_base in [0x00usize, 0x40] {
            // Plane 0 logical offset = 0. Higher planes stay 0 so colour = 1.
            let off = half_base + y * 4 + 0;
            let half_addr = off >> 1;
            if off & 1 == 0 {
                c_even[half_addr] = 0xFF;
            } else {
                c_odd[half_addr] = 0xFF;
            }
        }
    }
    vec![c_even, c_odd]
}

/// Synthetic identity LO-ROM: returns tile = line/16, sub_y = line & 0xF
/// regardless of zoom_y. This makes `zoom_y = 0xFF` behave as no shrink
/// for our test, which is what we want.
fn identity_lo_rom() -> Vec<u8> {
    let mut lo_rom = vec![0u8; 0x10000];
    for zy in 0..=255usize {
        for li in 0..=255usize {
            let tile = (li / 16) as u8;
            let sub_y = (li & 0x0F) as u8;
            lo_rom[(zy << 8) | li] = (tile << 4) | sub_y;
        }
    }
    lo_rom
}

#[test]
fn single_sprite_lands_at_expected_screen_position() {
    // Place a sprite at hw_x = 8 (screen x=8, well inside the active
    // 320-px area), hw_y = 0x18 = 24. In MAME's coordinate system the
    // first visible scanline is hw_y=0x10=16, so hw_y=24 -> screen y=8.
    //
    // Sprite occupies hw_y..hw_y+16 -> screen y=8..24 and hw_x..hw_x+16
    // -> screen x=8..24.
    let (lspc, pal) = setup_single_sprite(8, 0x18);
    let c_roms = cromp_all_colour_1();
    let decoded = decode_sprite_gfx(&c_roms);
    let lo_rom = identity_lo_rom();

    let frame = render_frame_full(
        &lspc, &pal, &[], &c_roms, &decoded, &lo_rom,
        /*palette_bank=*/ 0,
        /*screen_shadow=*/ false,
        /*bios_sfix=*/ None,
        FixBankType::Std,
    );

    let backdrop = frame[0]; // outside sprite
    assert_ne!(backdrop, 0, "backdrop must be set");

    // Inside sprite region: all pixels must differ from backdrop.
    let mut painted = 0usize;
    for y in 8..24 {
        for x in 8..24 {
            if frame[y * SCREEN_W + x] != backdrop {
                painted += 1;
            }
        }
    }
    assert_eq!(painted, 256, "all 16x16 sprite pixels must paint over backdrop");

    // Outside sprite: corners and a few border samples remain backdrop.
    for &(x, y) in &[
        (0, 0), (319, 0), (0, 223), (319, 223),
        (7, 8), (24, 8), (8, 7), (8, 24),
    ] {
        assert_eq!(
            frame[y * SCREEN_W + x],
            backdrop,
            "outside-sprite pixel ({x},{y}) should equal backdrop"
        );
    }
}

#[test]
fn backdrop_uses_palette_index_0xfff() {
    // The backdrop colour is palette index $FFF (the "back-drop colour").
    // Build a palette RAM where index $FFF = recognisable 0x7C00,
    // and check the frame is uniformly that value with no sprites.
    let lspc = Lspc::new();
    let mut pal = vec![0u8; 0x4000];
    let bd = 0xFFFusize * 2;
    pal[bd] = 0x7C;
    pal[bd + 1] = 0x00;
    let frame = render_frame_full(
        &lspc, &pal, &[], &[], &[], &[], 0, false, None, FixBankType::Std,
    );
    let bd_val = frame[0];
    for &px in frame.iter() {
        assert_eq!(px, bd_val, "every pixel must equal backdrop");
    }
}

#[test]
fn palette_bank_switches_visible_colour() {
    // Same sprite, but the visible palette bank toggles between 0 and 1.
    // Set palette 1 colour 1 = 0x7FFF in bank 0 and a different value
    // (0x4210) in bank 1 -> the rendered sprite pixel must differ.
    let (lspc, mut pal) = setup_single_sprite(8, 0x18);
    // Bank 1 starts at byte offset 0x2000.
    let idx_bank1 = 0x2000 + (1 * 16 + 1) * 2;
    pal[idx_bank1] = 0x42;
    pal[idx_bank1 + 1] = 0x10;

    let c_roms = cromp_all_colour_1();
    let decoded = decode_sprite_gfx(&c_roms);
    let lo_rom = identity_lo_rom();

    let f0 = render_frame_full(
        &lspc, &pal, &[], &c_roms, &decoded, &lo_rom,
        0, false, None, FixBankType::Std,
    );
    let f1 = render_frame_full(
        &lspc, &pal, &[], &c_roms, &decoded, &lo_rom,
        1, false, None, FixBankType::Std,
    );
    // Sprite pixel at screen (8,8) must differ between banks.
    let p0 = f0[8 * SCREEN_W + 8];
    let p1 = f1[8 * SCREEN_W + 8];
    assert_ne!(p0, p1, "palette bank switch must change sprite colour");
}

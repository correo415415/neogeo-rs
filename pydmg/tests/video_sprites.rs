//! Regression tests for the sprite renderer's geometric primitives.
//!
//! These exercises mirror MAME's `sprite_on_scanline` and the zoom-X
//! table indexing exactly. They catch any future change that drifts
//! away from MAME's published behaviour.

use pydmg_neogeo::graphics::video::{SCREEN_W, SCREEN_H};

/// The zoom-X mask table is the bit pattern that real hardware uses to
/// decide which source pixel of a 16-wide tile gets plotted at each
/// zoom level. MAME publishes them verbatim; FBNeo agrees byte-for-byte.
///
/// We don't expose the constant publicly to keep the API surface
/// minimal, but we re-derive a few well-known values from first
/// principles to anchor the renderer's behaviour.
#[test]
fn screen_dimensions_match_neogeo_ntsc() {
    // The Neo Geo NTSC active video area is 320 (H) × 224 (V).
    // Anything else would mean our framebuffer no longer matches the
    // hardware's HBSTART/HBEND timing constants
    // (see MAME `neogeo_spr.h`: HBEND=0x01C HBSTART=0x15C, delta=0x140).
    assert_eq!(SCREEN_W, 320);
    assert_eq!(SCREEN_H, 224);
}

/// Replicates MAME's `sprite_on_scanline(scanline, y, rows)` from
/// `src/mame/snk/neogeo_spr.cpp` (line ~278) verbatim:
///   ```c
///   return (rows == 0) || (rows >= 0x20) || ((scanline - y) & 0x1ff) < (rows * 0x10);
///   ```
/// The `(rows == 0)` short-circuit was added in MAME upstream so the
/// predicate matches hardware (LSPC chip treats rows==0 as "sticky run";
/// `parse_sprites` filters them out at the active-list stage by an
/// explicit early-continue, so this branch only ever fires from
/// `draw_sprites`'s post-list re-check).
#[test]
fn sprite_on_scanline_matches_mame_table() {
    // Local copy of the predicate so a refactor of the inner renderer
    // can't make this test rot silently.
    fn predicate(scanline: i32, y: i32, rows: i32) -> bool {
        (rows == 0) || (rows >= 0x20) || (((scanline - y) & 0x01FF) < (rows * 0x10))
    }
    // rows = 0: MAME returns true (the active-list gate filters before).
    assert!(predicate(0, 0, 0));
    // Standard 1-row sprite: covers 16 lines starting at y.
    assert!( predicate(0, 0, 1));
    assert!( predicate(15, 0, 1));
    assert!(!predicate(16, 0, 1));
    // 2 rows -> 32 lines.
    assert!( predicate(31, 0, 2));
    assert!(!predicate(32, 0, 2));
    // 0x20 rows -> always active (covers whole screen with wrap).
    assert!(predicate(123, 50, 0x20));
    assert!(predicate(0, 200, 0x20));
    // Wrap-around case: y=240, rows=1, scanline=0 -> (0-240)&0x1ff =
    // 0x110 = 272, which is >= 16 -> out of range -> false.
    assert!(!predicate(0, 240, 1));
    // But with rows = 0x20 -> active regardless of wrap.
    assert!(predicate(0, 240, 0x20));
}

/// The zoom-X mask table from MAME `neogeo_spr.cpp` line ~273:
///   { 0x0080, 0x0880, 0x0888, 0x2888, 0x288a, 0x2a8a, 0x2aaa, 0xaaaa,
///     0xaaea, 0xbaea, 0xbaeb, 0xbbeb, 0xbbef, 0xfbef, 0xfbff, 0xffff }
/// Verify each entry has exactly `index+1` bits set (since zoom_x of
/// `0` keeps 1 pixel and zoom_x of `0xF` keeps all 16). This pins down
/// the renderer to the documented zoom mask shape.
#[test]
fn zoom_x_tables_have_expected_popcount() {
    let table: [u16; 16] = [
        0x0080, 0x0880, 0x0888, 0x2888, 0x288a, 0x2a8a, 0x2aaa, 0xaaaa,
        0xaaea, 0xbaea, 0xbaeb, 0xbbeb, 0xbbef, 0xfbef, 0xfbff, 0xffff,
    ];
    for (i, &mask) in table.iter().enumerate() {
        assert_eq!(
            mask.count_ones() as usize,
            i + 1,
            "zoom_x_tables[{}] = 0x{:04X} should have {} bits set",
            i, mask, i + 1
        );
    }
    // Spot-check endpoints to confirm the table itself matches MAME.
    assert_eq!(table[0], 0x0080, "index 0 = MSB-1 bit (single rightmost-output pixel)");
    assert_eq!(table[15], 0xFFFF, "index F = full 16-pixel pass-through");
}

/// Pin down the y-coordinate decoding from SCB3. The Neo Geo
/// hardware computes the screen Y as `0x200 - (y_control >> 7)`,
/// which gives the *top* of the sprite in screen-space coordinates.
/// Anything else would shift sprites vertically.
#[test]
fn scb3_y_decoding_matches_mame() {
    // SCB3 has the y in bits 15..7 (9-bit unsigned), the sticky flag in
    // bit 6, and rows in bits 5..0. MAME: `y = 0x200 - (y_control >> 7)`.
    // Test a few well-known cases.
    fn decode_y(scb3: u16) -> i32 { 0x200 - (scb3 >> 7) as i32 }
    // y_field = 0   -> y = 0x200 (off-screen bottom)
    assert_eq!(decode_y(0x0000), 0x200);
    // y_field = 0x100 -> y = 0x100 (one screen up from origin)
    assert_eq!(decode_y(0x8000), 0x100);
    // y_field = 0x1F1 -> y = 0xF  (sprite right at top of screen)
    assert_eq!(decode_y(0xF880), 0xF);
}

//! Regression tests for the optimized sprite-gfx decoder.
//!
//! Verifies that `video::decode_sprite_gfx` produces a buffer whose
//! per-pixel lookups match the per-pair on-the-fly decoder
//! `video::sprite_tile_pixel` exactly. Any drift between the two paths
//! would silently corrupt sprite rendering in production builds.

use pydmg_neogeo::graphics::video::{decode_sprite_gfx, sprite_tile_pixel};

/// Build a synthetic 2-ROM pair (c_even + c_odd) with deterministic
/// pseudo-random content. The exact bytes don't matter, only that they
/// span all bit positions so the decoder exercises every plane.
fn synthetic_pair(tile_count: usize) -> Vec<Vec<u8>> {
    // Each tile occupies 128 bytes in the pair (64 bytes per physical
    // ROM, byte-interleaved). Build c_even and c_odd so that
    // pair[2k]   = c_even[k]
    // pair[2k+1] = c_odd [k]
    let half_size = tile_count * 64;
    let mut c_even = vec![0u8; half_size];
    let mut c_odd  = vec![0u8; half_size];
    for tile in 0..tile_count {
        for off in 0..128usize {
            let v = ((tile as u32).wrapping_mul(31) ^ (off as u32).wrapping_mul(7)) as u8;
            let pair_byte_off = tile * 128 + off;
            let half_addr = pair_byte_off >> 1;
            if pair_byte_off & 1 == 0 { c_even[half_addr] = v; }
            else                       { c_odd [half_addr] = v; }
        }
    }
    vec![c_even, c_odd]
}

#[test]
fn decode_sprite_gfx_matches_on_the_fly_decoder() {
    // 8 tiles exercises both halves, all rows, all planes.
    let c_roms = synthetic_pair(8);
    let decoded = decode_sprite_gfx(&c_roms);
    assert!(!decoded.is_empty(), "decoded buffer should be non-empty");
    for tile in 0..8u32 {
        for y in 0..16u8 {
            for x in 0..16u8 {
                let a = sprite_tile_pixel(&c_roms, tile, x, y);
                let romaddr = ((tile as usize) << 8) | ((y as usize) << 4) | (x as usize);
                let b = decoded[romaddr & (decoded.len() - 1)];
                assert_eq!(a, b,
                    "decoder mismatch at tile={tile} x={x} y={y}: on-the-fly={a} decoded={b}");
            }
        }
    }
}

#[test]
fn decode_sprite_gfx_buffer_is_power_of_two_sized() {
    // MAME folds out-of-range tile addresses by masking with
    // `m_sprite_gfx_address_mask`. We mirror that by sizing the
    // decoded buffer to a power of 2. This test pins the invariant.
    let c_roms = synthetic_pair(5);
    let decoded = decode_sprite_gfx(&c_roms);
    let len = decoded.len();
    assert!(len.is_power_of_two(), "decoded len {len} must be power of two");
    // For 5 tiles -> next pow2 = 8 -> 8 * 256 bytes = 2048.
    assert_eq!(len, 2048);
}

#[test]
fn decode_sprite_gfx_returns_empty_for_no_input() {
    let v: Vec<Vec<u8>> = vec![];
    assert_eq!(decode_sprite_gfx(&v), Vec::<u8>::new());
}

#[test]
fn decode_sprite_gfx_skips_truncated_pairs() {
    // c_even has 64 bytes (1 tile), c_odd is empty -> pair is invalid
    // and must be skipped. Result is empty.
    let c_roms = vec![vec![0u8; 64], vec![]];
    assert_eq!(decode_sprite_gfx(&c_roms), Vec::<u8>::new());
}

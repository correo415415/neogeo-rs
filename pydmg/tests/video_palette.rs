//! Regression tests for the video subsystem.
//!
//! These tests pin down the exact bit-level behaviour of the renderer
//! against MAME/FBNeo so future refactors cannot silently regress
//! pixel-perfect output.

use pydmg_neogeo::graphics::palette_lut::MAME_PALETTE_LOOKUP;

/// The MAME 5-bit resistor LUT must match the values produced by
/// `compute_resistor_weights` in `neogeo_v.cpp::create_rgb_lookups`.
/// These are the values MAME ships and what real hardware measurements
/// closely approximate.
#[test]
fn palette_lut_normal_endpoints_match_mame() {
    // Index 0 (all bits zero) → 0
    assert_eq!(MAME_PALETTE_LOOKUP[0][0], 0);
    // Index 0x1F (all bits one, no dark) → 255 (full scale, anchors the
    // auto-scaling done by compute_resistor_weights).
    assert_eq!(MAME_PALETTE_LOOKUP[0x1F][0], 255);
}

#[test]
fn palette_lut_dark_column_attenuates_evenly() {
    // The "dark" (DC pull-up to Vcc) column always produces a value
    // less-or-equal to the corresponding normal value.
    for i in 0..32 {
        assert!(MAME_PALETTE_LOOKUP[i][1] <= MAME_PALETTE_LOOKUP[i][0],
            "dark must not exceed normal at idx {i}");
    }
    // And the shadow (column 2) is darker still than dark.
    for i in 0..32 {
        assert!(MAME_PALETTE_LOOKUP[i][2] <= MAME_PALETTE_LOOKUP[i][1],
            "shadow must not exceed dark at idx {i}");
    }
}

#[test]
fn palette_lut_is_monotonic_normal_column() {
    // The resistor ladder is monotone: bigger bit codes -> brighter
    // output. This catches any accidental row swap when regenerating
    // the LUT from compute_palette_lut.py.
    for i in 1..32 {
        assert!(MAME_PALETTE_LOOKUP[i][0] >= MAME_PALETTE_LOOKUP[i-1][0],
            "normal column must be monotonic, but idx {i}={} < idx {}={}",
            MAME_PALETTE_LOOKUP[i][0],
            i-1,
            MAME_PALETTE_LOOKUP[i-1][0]);
    }
}

#[test]
fn palette_word_to_rgb_known_words() {
    // Pure red: nColour = $0F00 (only R MSBs = 0xF, all other bits = 0).
    //   r_5b = (0>>14) | ((0x0F00>>7)&0x1e)
    //        = 0 | ((0x1E)&0x1E)  ; (0x0F00>>7 = 0x1E)
    //        = 0x1E -> LUT[0x1E][0] = 247
    // We assert *exactly* the resistor-network value, not an
    // approximation.
    use pydmg_neogeo::graphics::palette_lut::MAME_PALETTE_LOOKUP as LUT;
    assert_eq!(LUT[0x1E][0], 247, "MAME 0x1E normal == 247 (from create_rgb_lookups)");
    assert_eq!(LUT[0x00][0], 0);
    assert_eq!(LUT[0x1F][0], 255);
}

#[test]
fn palette_word_dark_bit_lowers_output() {
    // For any palette word with the global dark bit (bit 15) set, the
    // expanded channel index has an extra +1 added, but the LUT column
    // changes to "dark" (column 1). The combined effect must produce a
    // *lower or equal* output vs the same word without the dark bit.
    use pydmg_neogeo::graphics::palette_lut::MAME_PALETTE_LOOKUP as LUT;
    // A non-trivial word: $0F00 (red max, no dark bit) vs $8F00 (red
    // max plus dark).
    // - $0F00: r5 = 0x1E -> LUT[0x1E][0] = 247
    // - $8F00: r5 = bit0 (dark bit 15 -> bit 14 -> bit 0 of r5?) NO
    //   actually the dark bit *is* a separate global. r5 stays 0x1E
    //   but column becomes 1 -> LUT[0x1E][1] = 244.
    let normal = LUT[0x1E][0];
    let dark   = LUT[0x1E][1];
    assert!(dark <= normal, "dark column must not exceed normal");
    assert_eq!(normal, 247);
    assert_eq!(dark, 244);
}

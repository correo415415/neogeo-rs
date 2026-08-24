//! Neo Geo video renderer.
//!
//! Combines the LSPC's VRAM (sprite control blocks + fix-layer tilemap),
//! the system palette RAM, the fix-tile S-ROM and the sprite C-ROMs into
//! a 320×224 RGBA framebuffer.
//!
//! Reference: NeoGeo Development Wiki — *Graphics*, *Sprites*, *Fix layer*.

use crate::graphics::lspc::Lspc;

/// Internal framebuffer width — 320 pixels, matching the Neo Geo's full
/// raster (`NEOGEO_HBSTART - NEOGEO_HBEND` = `0x15c - 0x01c` = `0x140` px).
/// The renderer always produces a 320×224 buffer; the *display* may crop
/// the outer 8 px on each side (see [`ACTIVE_W`]) because the majority of
/// commercial Neo Geo titles — including Metal Slug, KOF, Garou, Samurai
/// Shodown — were authored as **"304-based" games**: SNK's official BIOS
/// reserves columns 0 and 39 of the fix layer as an overscan-safe area to
/// avoid CRT ghosting and bezel cut-off, and the background sprites stop
/// at x = 8..311. Showing the full 320 px on a flat-pixel LCD viewer
/// reveals two backdrop-coloured "pillarbox" strips that real arcade
/// cabinets always hide behind their bezel mask.
///
/// References:
///   * <http://neogeo-megashock.blogspot.com/p/neo-geo-resolution.html>
///   * Patreon FBX — *Neo Geo 320 vs 304 Games* (lists Metal Slug as 304)
///   * MAME `Screen 0 Cropped (304×224)` view option
///   * 240p Test Suite (Artemio Urbina) — Neo Geo safe-area recommendation
pub const SCREEN_W: usize = 320;
pub const SCREEN_H: usize = 224;

/// Visible "active" width after cropping the 8 pixels of overscan-safe
/// border that SNK reserved on each horizontal edge for 304-based games.
/// `ACTIVE_W = SCREEN_W − 2 * ACTIVE_X_OFFSET = 320 − 16 = 304`.
///
/// Renderers / UIs that want to match the experience a real arcade
/// cabinet gives the player should display `frame[y][ACTIVE_X_OFFSET ..
/// ACTIVE_X_OFFSET + ACTIVE_W]` rather than the full row. The headless
/// PNG dump path keeps the full 320 px on purpose so debugging tools can
/// see what the LSPC actually emits.
pub const ACTIVE_W: usize = 304;

/// Horizontal offset of the active area inside the 320-px framebuffer.
/// Equal to MAME's `NEOGEO_HBEND` distance from the start of visible video
/// after the active raster begins, *minus* the chip's overscan that is
/// already trimmed by the bezel.
pub const ACTIVE_X_OFFSET: usize = 8;

/// RGBA framebuffer (one u32 per pixel, big-endian-style: 0xRRGGBBAA).
pub type Frame = Vec<u32>;

/// Convert one 16-bit Neo Geo palette entry into an 8-bit-per-channel sRGB
/// triple, using MAME's hardware-accurate resistor-network model.
///
/// Layout of the Neo Geo palette word (per `neogeo_v.cpp::paletteram_w`):
///
/// ```text
///   bit 15  : DARK / shadow bit (global, shared by R, G, B)
///   bit 14  : R lsb
///   bit 13  : G lsb
///   bit 12  : B lsb
///   bit 11-8: R MSBs (4 bits)
///   bit 7-4 : G MSBs (4 bits)
///   bit 3-0 : B MSBs (4 bits)
/// ```
///
/// Each channel ends up with **5 bits** of resolution (lsb + 4 MSBs).
/// Those 5 bits drive the analog R-2R-style ladder formed by the
/// hardware's 5 resistors (3900/2200/1000/470/220 Ω). MAME pre-computes
/// the analog output of every possible 5-bit code via
/// `compute_resistor_weights` and stores the result in a 32×4 lookup
/// table — column 0 = normal, column 1 = dark (=bit 15 set).
///
/// We precompute the same table offline in `palette_lut.rs` and use a
/// direct index here. Switching from the previous linear `<< 4`
/// approximation to this table fixes subtle colour shifts visible in
/// every Neo Geo game (most obvious on near-black greys, dim oranges and
/// the BIOS header gradients).
///
/// References:
///   * MAME `src/mame/snk/neogeo_v.cpp` lines 23-114 (`create_rgb_lookups`,
///     `paletteram_w`)
///   * FBNeo `src/burn/drv/neogeo/neo_palette.cpp` (uses an equivalent
///     6-bit ladder; gives near-identical output)
/// Decode a Neo Geo palette word into (R, G, B) bytes, honouring both the
/// per-entry `dark` bit (palette word bit 15) **and** the global
/// `screen_shadow` signal driven by HC259 Q0.
///
/// Mirrors MAME's `neogeo_v.cpp::paletteram_w`:
///
/// ```text
///   normal pen = m_palette_lookup[r][dark]
///   shadow pen = m_palette_lookup[r][dark + 2]   // +0x2000 in palette banks
/// ```
///
/// `MAME_PALETTE_LOOKUP` has four columns:
///   0 = normal, 1 = dark, 2 = shadow (150Ω pulldown), 3 = dark+shadow.
/// `screen_shadow=true` selects cols 2/3 — the resistor-network damping
/// real hardware applies when the SCREEN-SHADOW pin goes low (KOF combo
/// hits, pause menus, end-of-stage fades, attract-mode transitions).
fn palette_word_to_rgb(word: u16, screen_shadow: bool) -> (u8, u8, u8) {
    let r = (((word >> 14) & 0x1) | ((word >> 7) & 0x1e)) as usize;
    let g = (((word >> 13) & 0x1) | ((word >> 3) & 0x1e)) as usize;
    let b = (((word >> 12) & 0x1) | ((word << 1) & 0x1e)) as usize;
    let dark = ((word >> 15) & 0x1) as usize;
    let col = dark | if screen_shadow { 2 } else { 0 };
    let r8 = crate::graphics::palette_lut::MAME_PALETTE_LOOKUP[r & 0x1f][col];
    let g8 = crate::graphics::palette_lut::MAME_PALETTE_LOOKUP[g & 0x1f][col];
    let b8 = crate::graphics::palette_lut::MAME_PALETTE_LOOKUP[b & 0x1f][col];
    (r8, g8, b8)
}

#[inline(always)]
fn pack_rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32)
}

/// Look up palette index `idx` (0..4095) from the on-bus palette RAM.
///
/// Palette index 0 (per palette bank) is transparent — caller decides.
/// `palette_bank` is the currently-displayed palette bank (0 or 1, picked
/// from the system-latch bit 7 by the bus). Bank 1 lives at byte offset
/// `$2000` in the palette RAM, so the byte offset of palette index `idx`
/// is `bank_offset + idx*2`.
fn lookup_palette(
    palette_ram: &[u8],
    idx: u16,
    palette_bank: u8,
    screen_shadow: bool,
) -> u32 {
    let bank_off = (palette_bank as usize & 1) * 0x2000;
    let off = bank_off + ((idx as usize) & 0xFFF) * 2;
    if off + 1 >= palette_ram.len() {
        return 0;
    }
    let word = ((palette_ram[off] as u16) << 8) | (palette_ram[off + 1] as u16);
    let (r, g, b) = palette_word_to_rgb(word, screen_shadow);
    pack_rgba(r, g, b, 0xFF)
}

/// Decode one pixel of a fix-tile from the S-ROM.
///
/// Neo Geo fix tiles are 8×8, 4 bits per pixel, packed two pixels per byte.
/// The MAME `gfx_layout` is:
///
/// ```text
///   total_bytes_per_tile = 32
///   bits per pixel = 4
///   plane_offsets  = { 0, 1, 2, 3 }       (bits within the same byte)
///   x_offsets      = { 4, 0, 12, 8, 20, 16, 28, 24 }
///   y_offsets      = { 0*8, 1*8, 2*8, 3*8, 4*8, 5*8, 6*8, 7*8 }
/// ```
///
/// In words: each byte holds two 4-bit pixels; the left/right pixel order is
/// **swapped in pairs** (cols 0/1 are stored as 1/0, cols 2/3 as 3/2, …).
/// Returns the 4-bit colour index (0..15). 0 means transparent.
fn fix_tile_pixel(s_rom: &[u8], tile_no: u32, x: u8, y: u8) -> u8 {
    if s_rom.is_empty() {
        return 0;
    }
    let row = (y & 7) as usize;
    let col = (x & 7) as usize;
    let tile_off = (tile_no as usize) * 32;
    if tile_off + 32 > s_rom.len() {
        return 0;
    }
    // MAME `neogeo_spr.cpp::draw_fixed_layer`:
    //   pix_offsets = { 0x10, 0x18, 0x00, 0x08 }
    // Each entry is the byte offset for one *pair* of pixels (cols 0/1,
    // 2/3, 4/5, 6/7). Within each byte:
    //   data & 0x0F = LEFT  pixel of the pair
    //   data >> 4   = RIGHT pixel of the pair
    const GROUP_OFFSETS: [usize; 4] = [16, 24, 0, 8];
    let group = col / 2;
    let raw_byte = s_rom[tile_off + GROUP_OFFSETS[group] + row];
    if col & 1 == 0 {
        raw_byte & 0x0F          // left pixel
    } else {
        (raw_byte >> 4) & 0x0F   // right pixel
    }
}

/// Render the **fix layer** into `frame` (assumes `frame` is 320*224 u32).
///
/// The fix layer is a 40×32 tilemap of 8×8 tiles. Each cell in VRAM is a
/// 16-bit word at VRAM offset `$7000 + y*32 + x` where:
///   - bits 11..0 = tile number (12 bits → 4096 tiles)
///   - bits 15..12 = palette bank (16 banks of 16 colours, base index `bank*16`)
///
/// Note: the on-screen geometry is 40 columns × 28 visible rows (8 px each);
/// rows 28..31 are off-screen.
pub fn render_fix_layer_bank(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    frame: &mut [u32],
    palette_bank: u8,
) {
    render_fix_layer_inner(lspc, palette_ram, s_rom, frame, palette_bank, false);
}

pub fn render_fix_layer(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    frame: &mut [u32],
) {
    render_fix_layer_inner(lspc, palette_ram, s_rom, frame, 0, false);
}

/// Default number of fix-layer columns to trim on each horizontal edge.
///
/// The Neo Geo's *active* video raster is 320 px wide (`HBSTART - HBEND ==
/// 0x140` master ticks / 4 px-per-tick), and the fix layer is exactly 40
/// columns of 8-px tiles, so MAME's `draw_fixed_layer` walks all 40 columns
/// (`src/mame/snk/neogeo_spr.cpp` line ~210: `for (int x = 0; x < 40; x++)`).
///
/// On real arcade hardware columns 0 and 39 land inside the CRT overscan
/// area that the cabinet bezel typically masks off; FBNeo exposes a 304-px
/// crop mode that trims them (`neo_text.cpp:358-364`, `nNeoScreenWidth==304`
/// sets `nMinX=1, nMaxX=39`).
///
/// We default to **MAME's behaviour (0)** — the full 40 columns are drawn so
/// the framebuffer is hardware-accurate. Pass a non-zero value to
/// [`render_fix_layer_inner_with_bank_and_crop`] to opt into the FBNeo
/// 304-bezel-safe view.
pub const FIX_LAYER_OVERSCAN_COLS: usize = 0;

/// Fix-layer bank-switching modes used by NEO-CMC carts to address more
/// than 4096 fix tiles. Mirrors MAME's `FIX_BANKTYPE_*` enum in
/// `src/mame/snk/neogeo_spr.h`.
///
///   * `Std`     — default. Tile = `vram[$7000+col*32+row] & 0x0FFF`. 4096 tiles max.
///   * `Garou`   — used by Garou and Metal Slug 3/4. Per-row bank stored
///     in VRAM `$7500/$7580` page; gives up to 16384 tiles.
///   * `Kof2000` — used by KOF2000 and friends. Per-(row,col-group)
///     bank stored in VRAM `$7500+`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FixBankType {
    #[default]
    Std,
    Garou,
    Kof2000,
}

#[allow(clippy::too_many_arguments)]
fn render_fix_layer_inner(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    frame: &mut [u32],
    palette_bank: u8,
    screen_shadow: bool,
) {
    render_fix_layer_inner_with_bank(
        lspc, palette_ram, s_rom, frame,
        palette_bank, screen_shadow, FixBankType::Std,
    );
}

/// Internal fix-layer renderer with explicit banking mode. Garou and
/// KOF2000 add a 13th tile-number bit; the rest of the path is identical.
///
/// Port of MAME `neosprite_base_device::draw_fixed_layer` lines 184-243.
#[allow(clippy::too_many_arguments)]
pub fn render_fix_layer_inner_with_bank(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    frame: &mut [u32],
    palette_bank: u8,
    screen_shadow: bool,
    bank_type: FixBankType,
) {
    render_fix_layer_inner_with_bank_and_crop(
        lspc, palette_ram, s_rom, frame,
        palette_bank, screen_shadow, bank_type,
        FIX_LAYER_OVERSCAN_COLS,
    );
}

/// Fully-parameterised fix-layer renderer. `crop_cols` trims that many
/// 8-px columns off each horizontal edge (0 = MAME full, 1 = FBNeo 304).
/// See [`FIX_LAYER_OVERSCAN_COLS`] for rationale.
#[allow(clippy::too_many_arguments)]
pub fn render_fix_layer_inner_with_bank_and_crop(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    frame: &mut [u32],
    palette_bank: u8,
    screen_shadow: bool,
    bank_type: FixBankType,
    crop_cols: usize,
) {
    assert_eq!(frame.len(), SCREEN_W * SCREEN_H);
    let banked = bank_type != FixBankType::Std && s_rom.len() > 0x20000;

    // Build per-row bank table for GAROU once per frame, matching MAME's
    // `garouoffsets[34]` pass before the render loop.
    let mut garouoffsets = [0u32; 34];
    if banked && bank_type == FixBankType::Garou {
        let mut garoubank: u32 = 0;
        let mut k: usize = 0;
        let mut y: usize = 0;
        while y < 32 {
            // VRAM holds 0x8800 words; the GAROU pre-pass walks $7500/$7580
            // pairs (k step = 2, max k = 0x3E -> max addr = $75BF). No mask
            // needed: every read falls inside the buffer.
            //
            // NOTE (history): a previous revision had `& 0x87FF` here. That
            // is not a power-of-two mask, so `0x7500 & 0x87FF == 0x0500`
            // silently aliased to SCB1's tilemap area, breaking GAROU
            // bankswitching entirely. Fixed in line with MAME
            // `neogeo_spr.cpp:201-207` which does not mask either.
            let a = lspc.vram[0x7500 + k];
            let b = lspc.vram[0x7580 + k];
            if a == 0x0200 && (b & 0xFF00) == 0xFF00 {
                garoubank = (b & 3) as u32;
                garouoffsets[y] = garoubank;
                y += 1;
            }
            if y < 34 {
                garouoffsets[y] = garoubank;
                y += 1;
            }
            k += 2;
        }
    }
    // Fix tilemap starts at VRAM word $7000. Layout in VRAM is **column-major**:
    //   word($7000 + col*32 + row)  for col=0..39, row=0..31.
    // Source: NeoGeo Dev Wiki "Fix layer".
    // Per MAME `neogeo_spr.cpp::draw_fixed_layer`, the visible fix layer
    // covers rows 2..29 (inclusive). Rows 0,1,30,31 are the off-screen
    // border. Layout is column-major: word(col,row) at vram offset
    // 0x7000 + col*32 + row.
    //
    // Horizontal range trimmed by FIX_LAYER_OVERSCAN_COLS on each side to
    // match FBNeo's default 304-px (bezel-safe) area; see the constant doc.
    let col_start = crop_cols;
    let col_end = 40usize.saturating_sub(crop_cols);
    for col in col_start..col_end {
        for row in 2..30usize {
            // Fix tilemap address: col*32 + row, capped at col=39,row=31
            // -> max offset 0x73FF. Always inside VRAM, no mask needed
            // (matches MAME `neogeo_spr.cpp:213` which reads via
            // `m_videoram_drawsource[0x7000 | (scanline >> 3)]`).
            let cell_word = lspc.vram[0x7000 + col * 32 + row];
            let mut tile_no = (cell_word & 0x0FFF) as u32;
            let palette = ((cell_word >> 12) & 0x0F) as u16;
            // Tile 0 is *not* implicitly blank — the BIOS often relies on
            // colour 0 in the bank being transparent. Skip only when the whole
            // cell is uninitialised ($0000).
            if cell_word == 0 {
                continue;
            }
            // Apply NEO-CMC fix-layer banking when active. This adds bit
            // 12 of the final tile index from a per-row/per-column lookup
            // in VRAM $7500+, exactly as MAME does for Garou and KOF2000.
            if banked {
                let y_row = row;
                match bank_type {
                    FixBankType::Garou => {
                        let idx = (y_row - 2) & 31;
                        tile_no = tile_no.wrapping_add(
                            0x1000 * (garouoffsets[idx] ^ 3),
                        );
                    }
                    FixBankType::Kof2000 => {
                        let base = 0x7500 + ((y_row - 1) & 31) + 32 * (col / 6);
                        // Same `& 0x87FF` lurking bug as the GAROU pre-pass.
                        // KOF2000's bank words live in $7500..$75DF; the
                        // worst-case index is `0x7500 + 31 + 32*(39/6) =
                        // 0x7500 + 31 + 192 = 0x75DF`, comfortably inside
                        // VRAM. Direct unmasked read matches MAME.
                        let w = lspc.vram[base] as u32;
                        let shift = (5 - (col % 6) as u32) * 2;
                        tile_no = tile_no.wrapping_add(
                            0x1000 * (((w >> shift) & 3) ^ 3),
                        );
                    }
                    FixBankType::Std => {}
                }
            }
            for py in 0..8u8 {
                for px in 0..8u8 {
                    let c = fix_tile_pixel(s_rom, tile_no, px, py);
                    if c == 0 {
                        continue; // colour 0 = transparent within the palette bank
                    }
                    let pal_idx = palette * 16 + c as u16;
                    let rgba = lookup_palette(palette_ram, pal_idx, palette_bank, screen_shadow);
                    let x = col * 8 + px as usize;
                    let y = (row - 2) * 8 + py as usize;
                    if x < SCREEN_W && y < SCREEN_H {
                        frame[y * SCREEN_W + x] = rgba;
                    }
                }
            }
        }
    }
}

/// Decode one pixel of a sprite tile from the interleaved C-ROM pair.
///
/// Sprite tiles are 16×16, 4 bits/pixel. The Neo Geo C-ROMs come as pairs
/// (`c1+c2`, `c3+c4`, …). MAME's logical sprite ROM region is a flat
/// concatenation `pair0 | pair1 | …` where each pair contributes 128 bytes
/// per 16×16 tile, **interleaved at byte granularity** between the two
/// physical ROMs.
///
/// MAME `neosprite_regular_device::draw_pixel` walks this with the formula:
/// ```text
///   src = sprite_gfx + ((romaddr & ~0xFF) >> 1) | ((romaddr & 0x8) ^ 0x8) << 3 | (romaddr & 0xF0) >> 2
///   gfx = BIT(src[3], x) << 3 | BIT(src[1], x) << 2 | BIT(src[2], x) << 1 | BIT(src[0], x)
/// ```
///
/// For our offline decoder we use the equivalent `optimize_helper` layout:
///   * tile size = 128 bytes per tile *per pair*
///   * within each tile, RIGHT half (x=8..15) is bytes 0x00..0x3F,
///     LEFT half (x=0..7) is bytes 0x40..0x7F
///   * within each half, per row y (0..15) the four planes are at
///     bytes (y*4 + 0, y*4 + 2, y*4 + 1, y*4 + 3) for planes 0..3
///   * bit index inside the byte is `x & 7` (LSB = leftmost pixel of the half)
///
/// Each byte of the byte pair comes from the matching position in the
/// physical c_even / c_odd ROMs:
///   * offsets 0,1 come from c_even (interleaved)
///   * offsets 2,3 come from c_odd  (interleaved)
///
/// Pre-decode the entire C-ROM region into a flat 8-bit-per-pixel buffer,
/// indexed by MAME's `romaddr = (tile << 8) | (sub_y << 4) | x_in_tile`.
///
/// This is the direct Rust port of MAME's
/// `neosprite_optimized_device::optimize_helper` in
/// `src/mame/snk/neogeo_spr.cpp` (lines ~662-696). After this runs once at
/// load time, the per-pixel sprite drawing loop becomes a single byte load
/// plus a transparency test, matching MAME's
/// `neosprite_optimized_device::draw_pixel`. The cost is a one-shot
/// expansion (4 bpp → 8 bpp doubles the size: ~32 MiB for Metal Slug);
/// the payoff is a ~5× speed-up of the inner draw loop.
///
/// Layout produced (from MAME `optimize_helper`):
/// - Each 16×16 tile contributes 256 bytes, one byte per pixel, indexed as
///   `dest[(tile * 256) + (y * 16) + x]`.
/// - LEFT half (x=0..7)   <- pair offsets 0x40..0x7F
/// - RIGHT half (x=8..15) <- pair offsets 0x00..0x3F
/// - Plane bits within each half come from offsets {0,2,1,3} -> bit
///   weights {0,1,2,3} respectively (matches the C-ROM data-pin wiring).
///
/// The output buffer is rounded up to the next power of two so that
/// MAME-style `romaddr & m_sprite_gfx_address_mask` clipping is a no-op
/// (we mask with `len - 1` at lookup time).
#[must_use]
pub fn decode_sprite_gfx(c_roms: &[Vec<u8>]) -> Vec<u8> {
    if c_roms.is_empty() {
        return Vec::new();
    }
    let pairs = c_roms.len() / 2;
    let mut total_tiles: usize = 0;
    for p in 0..pairs {
        let len = c_roms[p * 2].len();
        if len == 0 || c_roms[p * 2 + 1].len() != len {
            continue;
        }
        total_tiles += len / 64;
    }
    if total_tiles == 0 {
        return Vec::new();
    }
    let target_tiles = total_tiles.next_power_of_two();
    let mut out = vec![0u8; target_tiles * 256];

    let mut tile_cursor: usize = 0;
    for p in 0..pairs {
        let len = c_roms[p * 2].len();
        if len == 0 || c_roms[p * 2 + 1].len() != len {
            continue;
        }
        let c_even = &c_roms[p * 2];
        let c_odd = &c_roms[p * 2 + 1];
        let tiles_in_pair = len / 64;
        for tip in 0..tiles_in_pair {
            let dest_tile_base = (tile_cursor + tip) * 256;
            let pair_tile_base = tip * 128;
            for y in 0..16usize {
                let read_pair = |off: usize| -> u8 {
                    let half_addr = off >> 1;
                    if off & 1 == 0 { c_even[half_addr] } else { c_odd[half_addr] }
                };
                // LEFT half (x=0..7) uses pair bytes at 0x40..0x7F.
                let lbase = pair_tile_base + 0x40 + y * 4;
                let l0 = read_pair(lbase + 0);
                let l1 = read_pair(lbase + 2);
                let l2 = read_pair(lbase + 1);
                let l3 = read_pair(lbase + 3);
                for x in 0..8 {
                    let bit = x as u8;
                    let gfx = ((l0 >> bit) & 1)
                            | (((l1 >> bit) & 1) << 1)
                            | (((l2 >> bit) & 1) << 2)
                            | (((l3 >> bit) & 1) << 3);
                    out[dest_tile_base + y * 16 + x] = gfx;
                }
                // RIGHT half (x=8..15) uses pair bytes at 0x00..0x3F.
                let rbase = pair_tile_base + y * 4;
                let r0 = read_pair(rbase + 0);
                let r1 = read_pair(rbase + 2);
                let r2 = read_pair(rbase + 1);
                let r3 = read_pair(rbase + 3);
                for x in 0..8 {
                    let bit = x as u8;
                    let gfx = ((r0 >> bit) & 1)
                            | (((r1 >> bit) & 1) << 1)
                            | (((r2 >> bit) & 1) << 2)
                            | (((r3 >> bit) & 1) << 3);
                    out[dest_tile_base + y * 16 + (8 + x)] = gfx;
                }
            }
        }
        tile_cursor += tiles_in_pair;
    }
    out
}

/// Fast pre-decoded sprite pixel lookup. `decoded` must come from
/// `decode_sprite_gfx`. Returns 0..15 (0 = transparent).
#[inline(always)]
fn decoded_sprite_pixel(decoded: &[u8], tile_no: u32, x: u8, y: u8) -> u8 {
    let idx = ((tile_no as usize) << 8) | ((y as usize & 0x0F) << 4) | (x as usize & 0x0F);
    // Power-of-two sizing lets us fold out-of-range tiles with a mask,
    // matching MAME's `romaddr & m_sprite_gfx_address_mask`.
    decoded[idx & (decoded.len() - 1)]
}

/// Returns 0..15. 0 = transparent.
pub fn sprite_tile_pixel(c_roms: &[Vec<u8>], tile_no: u32, x: u8, y: u8) -> u8 {
    if c_roms.len() < 2 {
        return 0;
    }
    // Tiles per pair: each pair is `c_even.len()` bytes (== c_odd.len()).
    // A pair contributes 128 bytes per tile -> tiles_per_pair = c_even.len() / 128 * 2,
    // because the two ROMs of the pair are byte-interleaved into a 128-byte tile.
    // i.e. tiles_per_pair = c_even.len() / 64.
    let tiles_per_pair = c_roms[0].len() / 64;
    if tiles_per_pair == 0 {
        return 0;
    }
    let pair_idx = (tile_no as usize) / tiles_per_pair;
    let tile_in_pair = (tile_no as usize) % tiles_per_pair;
    let even_idx = pair_idx * 2;
    if even_idx + 1 >= c_roms.len() {
        return 0;
    }
    let c_even = &c_roms[even_idx];
    let c_odd = &c_roms[even_idx + 1];

    let row = (y & 0x0F) as usize;
    let col = (x & 0x0F) as usize;
    // Per MAME's `optimize_helper`, RIGHT half lives at bytes 0x00..0x3F and
    // LEFT half at 0x40..0x7F within the per-pair 128-byte tile.
    let half_base = if col < 8 { 0x40 } else { 0x00 };
    let bit = (col & 7) as u8;   // x=0 is the leftmost pixel of the half -> LSB
    // Each pair byte is byte-interleaved between the two physical ROMs:
    //   pair[2k]   = c_even[k]
    //   pair[2k+1] = c_odd[k]
    // We need the four logical bytes at offsets (b0=y*4+0, b1=y*4+2,
    // b2=y*4+1, b3=y*4+3) for planes (0,1,2,3) respectively.
    let pair_base = tile_in_pair * 128 + half_base + row * 4;
    // Translate each logical offset 0..3 into (which_rom, half_addr).
    // pair offset n -> rom = n & 1 (0 even, 1 odd), addr = (pair_base + n) / 2.
    let read = |n: usize| -> u8 {
        let off = pair_base + n;
        let half_addr = off >> 1;
        if off & 1 == 0 {
            *c_even.get(half_addr).unwrap_or(&0)
        } else {
            *c_odd.get(half_addr).unwrap_or(&0)
        }
    };
    let b0 = read(0); // plane 0
    let b1 = read(2); // plane 1
    let b2 = read(1); // plane 2
    let b3 = read(3); // plane 3
    let p0 = (b0 >> bit) & 1;
    let p1 = (b1 >> bit) & 1;
    let p2 = (b2 >> bit) & 1;
    let p3 = (b3 >> bit) & 1;
    (p3 << 3) | (p2 << 2) | (p1 << 1) | p0
}

/// Render the **sprite layer** on top of `frame`.
///
/// Sprite control block layout in VRAM (per the Dev Wiki):
///   * SCB1 ($0000..$6FFF): 64 bytes/sprite (32 words)
///     - word 0 = tile number low
///     - word 1 = tile palette/control (bits 8..15 = palette, 0..6 = flags)
///   * SCB2 ($8000..$81FF): zoom + chain control, **one word per sprite**
///   * SCB3 ($8200..$83FF): per-sprite y-position + height (16 sprites chain)
///   * SCB4 ($8400..$85FF): per-sprite x-position
///
/// We implement a *simplified* path that handles the most common case
/// (no zoom, no chain): for each of the 381 sprites, fetch x/y/tile from
/// SCB3/SCB4/SCB1 and blit a single 16×16 tile.
/// Render the **sprite layer** on top of `frame`. Implements the sprite
/// pipeline of MAME's `neosprite_base_device::draw_sprites` (no scanline
/// loop — we do whole frames at once, no zoom support yet).
///
/// VRAM layout used (per MAME `neogeo_spr.cpp`, all addresses are *word*
/// offsets, with the top page starting at `0x8000`):
///
/// ```text
///   $0000..$6FFF   SCB1  — 32 words/sprite (sprite number * 0x40 in BYTES,
///                          == sprite_number << 6 in word indices). Each
///                          tile entry is two words: tile_lo, attr+tile_hi.
///   $8000..$81FF   SCB2  — 1 word/sprite: zoom_x (hi byte), zoom_y (lo byte)
///   $8200..$83FF   SCB3  — 1 word/sprite: y (9 bits, hi), sticky (bit 6),
///                          rows (bits 5..0)
///   $8400..$85FF   SCB4  — 1 word/sprite: x (9 bits, hi)
///   $8600..$867F   sprite list for even scanlines (96 sprite indices)
///   $8680..$86FF   sprite list for odd scanlines
/// ```
///
/// We currently *ignore* the per-scanline lists and the zoom tables; we
/// walk the 381 sprites in reverse so sprite #0 ends up on top (the
/// hardware draws #0 last). This is enough to render most static screens.
/// Render the Neo Geo sprite layer. Ported from MAME's
/// `neosprite_base_device::draw_sprites` (no zoom yet, no scanline lists,
/// no auto-animation yet beyond exposing the API).
///
/// VRAM layout (per MAME, all addresses are *word* offsets):
/// ```text
///   $0000..$6FFF   SCB1 — 32 words per sprite (sprite_number * 0x40).
///                          Per tile in the chain: word[2i+0] = tile_lo,
///                          word[2i+1] = attr_hi (palette/flip/anim/tile_hi).
///   $7000..$74FF   Fix-layer tilemap (40×32).
///   $8000..$81FF   SCB2 — 1 word/sprite: zoom_x (hi byte) | zoom_y (lo byte).
///   $8200..$83FF   SCB3 — 1 word/sprite: y(9 bits hi) | sticky(b6) | rows(b0-5).
///   $8400..$85FF   SCB4 — 1 word/sprite: x(9 bits hi).
///   $8600..$867F   per-scanline sprite list for even lines.
///   $8680..$86FF   per-scanline sprite list for odd lines.
/// ```
///
/// Per MAME's `draw_sprites`, attr bits within `vram[scb1+2i+1]` are:
///   bits 0   : H-flip
///   bit  1   : V-flip
///   bit  2   : auto-animation 2-bit  (cycles tile through 4 frames)
///   bit  3   : auto-animation 3-bit  (cycles tile through 8 frames)
///   bits 4-7 : tile_hi   (high 4 bits of the 20-bit tile number)
///   bits 8-15: palette (8-bit index, multiplied by 16 inside the palette)
pub fn render_sprite_layer(
    lspc: &Lspc,
    palette_ram: &[u8],
    c_roms: &[Vec<u8>],
    frame: &mut [u32],
    auto_animation_counter: u32,
    auto_animation_disabled: bool,
) {
    render_sprite_layer_inner(
        lspc, palette_ram, c_roms, &[], &[], frame,
        auto_animation_counter, auto_animation_disabled, 0, false,
    );
}

/// Horizontal zoom ("shrinking") table — verified on real hardware. Taken
/// verbatim from MAME `src/mame/snk/neogeo_spr.cpp` line 274 (the array
/// `zoom_x_tables[16]`), which itself matches the bit pattern documented
/// on the NeoGeo Dev Wiki.
///
/// Reading convention: the loop examines `mask & 0x8000` first and then
/// left-shifts the mask, so **bit 15 corresponds to pixel 0** of the
/// source tile (the leftmost), bit 14 to pixel 1, … bit 0 to pixel 15.
/// The wiki lists the rows left-to-right, so `0,0,...,1,0,...` for index
/// 0 (only pixel 8 drawn) encodes as `0x0080`.
///
/// Previous (buggy) values had the bits mirrored, which caused every
/// shrunk sprite to display a different subset of source columns —
/// most visible during the title-screen logo zoom animation in Metal
/// Slug and as garbled letters in scenes like "NEO·GEO PRO-GEAR SPEC".
const ZOOM_X_TABLES: [u16; 16] = [
    0x0080, // idx 0:  1px  | 0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0
    0x0880, // idx 1:  2px  | 0,0,0,0,1,0,0,0,1,0,0,0,0,0,0,0
    0x0888, // idx 2:  3px  | 0,0,0,0,1,0,0,0,1,0,0,0,1,0,0,0
    0x2888, // idx 3:  4px  | 0,0,1,0,1,0,0,0,1,0,0,0,1,0,0,0
    0x288A, // idx 4:  5px  | 0,0,1,0,1,0,0,0,1,0,0,0,1,0,1,0
    0x2A8A, // idx 5:  6px  | 0,0,1,0,1,0,1,0,1,0,0,0,1,0,1,0
    0x2AAA, // idx 6:  7px  | 0,0,1,0,1,0,1,0,1,0,1,0,1,0,1,0
    0xAAAA, // idx 7:  8px  | 1,0,1,0,1,0,1,0,1,0,1,0,1,0,1,0
    0xAAEA, // idx 8:  9px  | 1,0,1,0,1,0,1,0,1,1,1,0,1,0,1,0
    0xBAEA, // idx 9: 10px  | 1,0,1,1,1,0,1,0,1,1,1,0,1,0,1,0
    0xBAEB, // idx A: 11px  | 1,0,1,1,1,0,1,0,1,1,1,0,1,0,1,1
    0xBBEB, // idx B: 12px  | 1,0,1,1,1,0,1,1,1,1,1,0,1,0,1,1
    0xBBEF, // idx C: 13px  | 1,0,1,1,1,0,1,1,1,1,1,0,1,1,1,1
    0xFBEF, // idx D: 14px  | 1,1,1,1,1,0,1,1,1,1,1,0,1,1,1,1
    0xFBFF, // idx E: 15px  | 1,1,1,1,1,0,1,1,1,1,1,1,1,1,1,1
    0xFFFF, // idx F: 16px  | 1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1
];

const MAX_SPRITES_PER_SCREEN: usize = 381;
const MAX_SPRITES_PER_LINE: usize = 96;

/// Direct port of MAME `neosprite_base_device::sprite_on_scanline`
/// (`src/mame/snk/neogeo_spr.cpp`, line ~278). Returns whether sprite at
/// hardware-`y` with `rows` tile rows is visible on hardware `scanline`.
///
/// Note the special-case for `rows == 0` (sprite disabled) and
/// `rows >= 0x20` (vertical wrap / sticky run): the hardware always
/// reports those as on-scanline regardless of `y`.
#[inline]
fn sprite_on_scanline(scanline: i32, y: i32, rows: i32) -> bool {
    (rows == 0) || (rows >= 0x20) || (((scanline - y) & 0x01FF) < (rows * 0x10))
}

/// MAME-style scanline sprite rendering. Iterates each visible scanline
/// (0..224), finds sprites that cover it, looks up the proper tile/line in
/// the Y-zoom ROM (000-lo.lo), and plots zoom-X-masked pixels.
///
/// When `decoded_gfx` is non-empty, the per-pixel inner loop becomes a
/// single byte load (`neosprite_optimized_device::draw_pixel`). Otherwise
/// we fall back to the on-the-fly decoder via `sprite_tile_pixel`
/// (`neosprite_regular_device::draw_pixel`).
///
/// Reference: MAME `neogeo_spr.cpp::draw_sprites`.
#[allow(clippy::too_many_arguments)]
fn render_sprite_layer_inner(
    lspc: &Lspc,
    palette_ram: &[u8],
    c_roms: &[Vec<u8>],
    decoded_gfx: &[u8],
    lo_rom: &[u8],
    frame: &mut [u32],
    auto_animation_counter: u32,
    auto_animation_disabled: bool,
    palette_bank: u8,
    screen_shadow: bool,
) {
    assert_eq!(frame.len(), SCREEN_W * SCREEN_H);
    for scanline in 0..SCREEN_H as i32 {
        render_sprite_scanline(
            lspc, palette_ram, c_roms, decoded_gfx, lo_rom, frame, scanline,
            auto_animation_counter, auto_animation_disabled,
            palette_bank, screen_shadow,
        );
    }
}

/// Render the sprite layer for a **single output scanline** (0..224) into
/// the full frame buffer. Extracted from `render_sprite_layer_inner` so the
/// system can render incrementally as emulated scanlines are crossed.
/// Required for raster effects: IRQ2-driven code that rewrites sprite X
/// positions / SCB data mid-frame (e.g. the water ripple in TTE's VAPOROUS
/// demo) is invisible to an end-of-frame render but shows up correctly when
/// each line is drawn with the VRAM state current at that line.
#[allow(clippy::too_many_arguments)]
pub fn render_sprite_scanline(
    lspc: &Lspc,
    palette_ram: &[u8],
    c_roms: &[Vec<u8>],
    decoded_gfx: &[u8],
    lo_rom: &[u8],
    frame: &mut [u32],
    scanline: i32,
    auto_animation_counter: u32,
    auto_animation_disabled: bool,
    palette_bank: u8,
    screen_shadow: bool,
) {
    if !(0..SCREEN_H as i32).contains(&scanline) || frame.len() != SCREEN_W * SCREEN_H {
        return;
    }
    // Need either raw C-ROM pairs or pre-decoded buffer; nothing else to draw.
    if c_roms.is_empty() && decoded_gfx.is_empty() {
        return;
    }
    let use_decoded = !decoded_gfx.is_empty();
    let vram = &lspc.vram[..];
    // If we don't have the Y-zoom table, build a synthetic 1:1 fallback so
    // games still render (just without proper vertical scaling).
    // The hardware ROM is 64 KiB. We index it as `lo_rom[(zoom_y<<8) | line]`.
    let have_lo_rom = lo_rom.len() >= 0x10000;
    // Our 224-line output starts at hardware line 0x10 (VBEND) just like
    // MAME's visible area. The sprite engine itself reasons in hardware
    // scanlines, so keep the +0x10 bias here.
    let hw_scanline = scanline + 0x10;

    // Pass A: build the active 96-sprite list exactly like
    // `neosprite_base_device::parse_sprites`.
    let mut active_sprites = [0u16; MAX_SPRITES_PER_LINE];
    let mut active_count = 0usize;
    let mut y_p = 0i32;
    let mut rows_p = 0i32;
    for sprite_number in 0..MAX_SPRITES_PER_SCREEN as u16 {
        let y_control = vram[0x8200 | sprite_number as usize];
        if (y_control & 0x40) == 0 {
            y_p = 0x200 - ((y_control >> 7) as i32);
            rows_p = (y_control & 0x3F) as i32;
        }
        // MAME's `parse_sprites` has an explicit `if (rows == 0) continue;`
        // before the `sprite_on_scanline` check (neogeo_spr.cpp:486-489),
        // even though our `sprite_on_scanline` now also returns true for
        // rows==0. We keep the explicit gate to match the MAME control
        // flow exactly: rows-zero sprites must NOT consume an active-list
        // slot (otherwise the 96-per-line budget would be wasted).
        if rows_p == 0 {
            continue;
        }
        if !sprite_on_scanline(hw_scanline, y_p, rows_p) {
            continue;
        }
        active_sprites[active_count] = sprite_number;
        active_count += 1;
        if active_count == MAX_SPRITES_PER_LINE {
            break;
        }
    }

    // Pass B: draw the active list exactly in list order, resolving sticky
    // chains from the previous *active* sprite, just like MAME.
    let mut x = 0i32;
    let mut y = 0i32;
    let mut rows = 0i32;
    let mut zoom_y = 0u8;
    let mut zoom_x = 0i32;
    for sprite in active_sprites[..active_count].iter().copied() {
        let y_control = vram[0x8200 | sprite as usize];
        let zoom_control = vram[0x8000 | sprite as usize];

        if (y_control & 0x40) != 0 {
            x = (x + zoom_x + 1) & 0x01FF;
            zoom_x = ((zoom_control >> 8) & 0x0F) as i32;
        } else {
            y = 0x200 - ((y_control >> 7) as i32);
            x = (vram[0x8400 | sprite as usize] >> 7) as i32;
            zoom_y = (zoom_control & 0x00FF) as u8;
            zoom_x = ((zoom_control >> 8) & 0x0F) as i32;
            rows = (y_control & 0x3F) as i32;
        }

        // Hardware skip: sprites with x in [$140, $1F0] are off-screen.
        if (0x140..=0x1F0).contains(&x) {
            continue;
        }
        // Re-check Y coverage after reading from buffered list.
        if !sprite_on_scanline(hw_scanline, y, rows) {
            continue;
        }

        let sx_left = if x >= 0x1E0 { x - 0x200 } else { x };
        let sprite_line_signed = (hw_scanline - y) & 0x01FF;
        let mut zoom_line = sprite_line_signed & 0xFF;
        let mut invert = (sprite_line_signed & 0x100) != 0;
        if invert {
            zoom_line ^= 0xFF;
        }
        if rows > 0x20 {
            let period = (zoom_y as i32 + 1) << 1;
            zoom_line %= period;
            if zoom_line > zoom_y as i32 {
                zoom_line = period - 1 - zoom_line;
                invert = !invert;
            }
        }

        let (tile_idx, mut sub_y) = if have_lo_rom {
            let b = lo_rom[((zoom_y as usize) << 8) | (zoom_line as usize)];
            ((b >> 4) as i32, (b & 0x0F) as i32)
        } else {
            let li = zoom_line as usize;
            ((li / 16) as i32, (li & 0x0F) as i32)
        };
        let mut effective_tile = tile_idx;
        if invert {
            sub_y ^= 0x0F;
            effective_tile ^= 0x1F;
        }

        // SCB1: tile attribute is 32 words (64 bytes) per sprite.
        let scb1_base = (sprite as usize) * 0x40;
        let entry = scb1_base + (effective_tile as usize) * 2;
        if entry + 1 >= 0x7000 {
            continue;
        }
        let tile_lo = vram[entry];
        let attr = vram[entry + 1];
        let mut code = (tile_lo as u32) | (((attr as u32) << 12) & 0xF_0000);

        // Auto-animation substitution.
        if !auto_animation_disabled {
            if (attr & 0x08) != 0 {
                code = (code & !0x07) | (auto_animation_counter & 0x07);
            } else if (attr & 0x04) != 0 {
                code = (code & !0x03) | (auto_animation_counter & 0x03);
            }
        }

        let palette = (attr >> 8) as u16;
        let hflip = (attr & 1) != 0;
        if (attr & 2) != 0 {
            sub_y ^= 0x0F;
        }

        // Horizontal blit. MAME walks the source tile pixel index in
        // `i` and the dst column in `dst_x`; the zoom mask decides at
        // each step whether to plot or skip. The source advances
        // *every* iteration (only the destination is gated). H-flip
        // is modelled exactly like MAME by negating the source
        // increment and starting at pixel 15.
        //
        // Early-exit: once the mask is empty, every remaining
        // iteration would be a no-op — bail to save work.
        let mut zoom_x_mask = ZOOM_X_TABLES[(zoom_x & 0x0F) as usize];
        let mut dst_x = sx_left;
        let mut src_px: i32 = if hflip { 15 } else { 0 };
        let src_inc: i32 = if hflip { -1 } else { 1 };
        for _ in 0..16i32 {
            if (zoom_x_mask & 0x8000) != 0 {
                if (0..SCREEN_W as i32).contains(&dst_x) {
                    let c = if use_decoded {
                        decoded_sprite_pixel(decoded_gfx, code, src_px as u8, sub_y as u8)
                    } else {
                        sprite_tile_pixel(c_roms, code, src_px as u8, sub_y as u8)
                    };
                    if c != 0 {
                        let pal_idx = palette.wrapping_mul(16).wrapping_add(c as u16);
                        frame[scanline as usize * SCREEN_W + dst_x as usize] =
                            lookup_palette(palette_ram, pal_idx, palette_bank, screen_shadow);
                    }
                }
                dst_x += 1;
            }
            zoom_x_mask <<= 1;
            if zoom_x_mask == 0 {
                break;
            }
            src_px += src_inc;
        }
    }

}

/// Compose the full frame: background colour + sprites + fix layer (on top).
///
/// `bg_index` = palette index 0xFFF (the "back-drop" colour).
pub fn render_frame(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    c_roms: &[Vec<u8>],
) -> Frame {
    render_frame_with_bank(lspc, palette_ram, s_rom, c_roms, &[], &[], 0)
}

/// Render a frame using a specific palette bank (0 or 1). The hardware
/// uses the bit-7 output of the 74HC259 system latch as the visible
/// palette bank, which is also the bank the CPU writes to (the BIOS
/// boot path writes its colours to bank 1 first).
///
/// `decoded_gfx`, if non-empty, must come from `decode_sprite_gfx`
/// applied to the same `c_roms` set. Passing it switches the renderer to
/// the optimized one-byte-per-pixel path (matches MAME’s
/// `neosprite_optimized_device`). Pass `&[]` to use the on-the-fly
/// decoder.
#[allow(clippy::too_many_arguments)]
pub fn render_frame_with_bank(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    c_roms: &[Vec<u8>],
    decoded_gfx: &[u8],
    lo_rom: &[u8],
    palette_bank: u8,
) -> Frame {
    render_frame_full(
        lspc, palette_ram, s_rom, c_roms, decoded_gfx, lo_rom,
        palette_bank, false, None, FixBankType::Std,
    )
}

/// Render the fix layer for a **single output scanline** (0..224).
/// Per-line port of `render_fix_layer_inner_with_bank_and_crop`: for output
/// line `y` the tile row is `(y / 8) + 2` and the in-tile row is `y % 8`.
/// The GAROU per-row bank pre-pass is re-run per call — it is a tiny
/// bounded loop (<= 32 VRAM word pairs) so the cost is negligible, and it
/// keeps mid-frame bank rewrites raster-accurate too.
#[allow(clippy::too_many_arguments)]
pub fn render_fix_scanline(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    frame: &mut [u32],
    out_line: usize,
    palette_bank: u8,
    screen_shadow: bool,
    bank_type: FixBankType,
) {
    if out_line >= SCREEN_H || frame.len() != SCREEN_W * SCREEN_H {
        return;
    }
    let banked = bank_type != FixBankType::Std && s_rom.len() > 0x20000;
    let row = (out_line / 8) + 2;
    let py = (out_line % 8) as u8;

    // GAROU per-row bank table (see full-frame renderer for details).
    let mut garouoffsets = [0u32; 34];
    if banked && bank_type == FixBankType::Garou {
        let mut garoubank: u32 = 0;
        let mut k: usize = 0;
        let mut y: usize = 0;
        while y < 32 {
            let a = lspc.vram[0x7500 + k];
            let b = lspc.vram[0x7580 + k];
            if a == 0x0200 && (b & 0xFF00) == 0xFF00 {
                garoubank = (b & 3) as u32;
                garouoffsets[y] = garoubank;
                y += 1;
            }
            if y < 34 {
                garouoffsets[y] = garoubank;
                y += 1;
            }
            k += 2;
        }
    }

    let col_start = FIX_LAYER_OVERSCAN_COLS;
    let col_end = 40usize.saturating_sub(FIX_LAYER_OVERSCAN_COLS);
    for col in col_start..col_end {
        let cell_word = lspc.vram[0x7000 + col * 32 + row];
        if cell_word == 0 {
            continue;
        }
        let mut tile_no = (cell_word & 0x0FFF) as u32;
        let palette = ((cell_word >> 12) & 0x0F) as u16;
        if banked {
            match bank_type {
                FixBankType::Garou => {
                    let idx = (row - 2) & 31;
                    tile_no = tile_no.wrapping_add(0x1000 * (garouoffsets[idx] ^ 3));
                }
                FixBankType::Kof2000 => {
                    let base = 0x7500 + ((row - 1) & 31) + 32 * (col / 6);
                    let w = lspc.vram[base] as u32;
                    let shift = (5 - (col % 6) as u32) * 2;
                    tile_no = tile_no.wrapping_add(0x1000 * (((w >> shift) & 3) ^ 3));
                }
                FixBankType::Std => {}
            }
        }
        for px in 0..8u8 {
            let c = fix_tile_pixel(s_rom, tile_no, px, py);
            if c == 0 {
                continue;
            }
            let pal_idx = palette * 16 + c as u16;
            let rgba = lookup_palette(palette_ram, pal_idx, palette_bank, screen_shadow);
            let x = col * 8 + px as usize;
            if x < SCREEN_W {
                frame[out_line * SCREEN_W + x] = rgba;
            }
        }
    }
}

/// Compose one full output scanline (backdrop + sprites + fix) into an
/// accumulating frame buffer using the **current** LSPC/palette state.
///
/// This is the raster-rendering entry point: the system calls it each time
/// the LSPC crosses a visible scanline, so mid-frame VRAM rewrites driven
/// by the display-position (IRQ2) timer — per-line sprite X shifts used for
/// water ripple, floor warping, etc. — show up exactly on the lines they
/// affect, matching real hardware.
#[allow(clippy::too_many_arguments)]
pub fn render_scanline(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    c_roms: &[Vec<u8>],
    decoded_gfx: &[u8],
    lo_rom: &[u8],
    frame: &mut [u32],
    out_line: usize,
    palette_bank: u8,
    screen_shadow: bool,
    bios_sfix: Option<&[u8]>,
    fix_bank_type: FixBankType,
) {
    if out_line >= SCREEN_H || frame.len() != SCREEN_W * SCREEN_H {
        return;
    }
    // Backdrop fill for this line (palette entry $FFF).
    let backdrop = lookup_palette(palette_ram, 0xFFF, palette_bank, screen_shadow);
    let row_off = out_line * SCREEN_W;
    for px in frame[row_off..row_off + SCREEN_W].iter_mut() {
        *px = backdrop;
    }
    render_sprite_scanline(
        lspc, palette_ram, c_roms, decoded_gfx, lo_rom, frame, out_line as i32,
        lspc.auto_animation_counter, lspc.auto_animation_disabled,
        palette_bank, screen_shadow,
    );
    let fix_src = bios_sfix.unwrap_or(s_rom);
    let effective_bank = if bios_sfix.is_some() { FixBankType::Std } else { fix_bank_type };
    render_fix_scanline(
        lspc, palette_ram, fix_src, frame, out_line,
        palette_bank, screen_shadow, effective_bank,
    );
}

/// Like [`render_frame_with_bank`] but also honours the LSPC's global
/// `screen_shadow` signal (HC259 Q0) and lets the caller pass an
/// override fix-layer S-ROM source (`bios_sfix`) to model HC259 Q5
/// switching between cart `s1.bin` (default) and BIOS `sfix.sfix`.
///
/// When `bios_sfix.is_some()` AND the latch selects BIOS source, the
/// fix layer is rendered from `bios_sfix` instead of `s_rom`. This
/// matches MAME's `set_fixed_layer_source` semantics where Q5 routes
/// the SFIX/S1 multiplexer and lets the cart's BIOS boot-up code paint
/// onto the cart S-ROM partway through reset.
#[allow(clippy::too_many_arguments)]
pub fn render_frame_full(
    lspc: &Lspc,
    palette_ram: &[u8],
    s_rom: &[u8],
    c_roms: &[Vec<u8>],
    decoded_gfx: &[u8],
    lo_rom: &[u8],
    palette_bank: u8,
    screen_shadow: bool,
    bios_sfix: Option<&[u8]>,
    fix_bank_type: FixBankType,
) -> Frame {
    let mut frame = vec![0u32; SCREEN_W * SCREEN_H];
    let backdrop = lookup_palette(palette_ram, 0xFFF, palette_bank, screen_shadow);
    for px in frame.iter_mut() {
        *px = backdrop;
    }
    render_sprite_layer_inner(
        lspc,
        palette_ram,
        c_roms,
        decoded_gfx,
        lo_rom,
        &mut frame,
        lspc.auto_animation_counter,
        lspc.auto_animation_disabled,
        palette_bank,
        screen_shadow,
    );
    // Pick fix-layer source: cart S-ROM if caller passed None or the cart
    // path is active, otherwise the BIOS SFIX.
    let fix_src = bios_sfix.unwrap_or(s_rom);
    // BIOS SFIX is always 128 KiB and never banked. Cart banking only
    // matters when the cart path is selected.
    let effective_bank = if bios_sfix.is_some() { FixBankType::Std } else { fix_bank_type };
    render_fix_layer_inner_with_bank(
        lspc, palette_ram, fix_src, &mut frame,
        palette_bank, screen_shadow, effective_bank,
    );
    frame
}

/// Sprite-layer rendering with explicit palette bank — thin wrapper that
/// delegates to the inner sprite renderer with both palette bank and the
/// optional pre-decoded sprite-gfx buffer.
#[allow(clippy::too_many_arguments)]
pub fn render_sprite_layer_bank(
    lspc: &Lspc,
    palette_ram: &[u8],
    c_roms: &[Vec<u8>],
    decoded_gfx: &[u8],
    lo_rom: &[u8],
    frame: &mut [u32],
    auto_anim_counter: u32,
    auto_anim_disabled: bool,
    palette_bank: u8,
) {
    render_sprite_layer_inner(
        lspc, palette_ram, c_roms, decoded_gfx, lo_rom, frame,
        auto_anim_counter, auto_anim_disabled, palette_bank, false,
    );
}

/// Encode an RGBA framebuffer into a minimal PNG (no compression — fixed
/// stored DEFLATE blocks). Lets us dump frames for visual inspection without
/// pulling the `png` crate.
pub fn frame_to_png(frame: &[u32], width: usize, height: usize) -> Vec<u8> {
    assert_eq!(frame.len(), width * height);
    // 1) Build the raw scanlines: each line is one filter byte (0 = None)
    //    followed by `width * 4` RGBA bytes.
    let mut raw = Vec::with_capacity(height * (1 + width * 4));
    for y in 0..height {
        raw.push(0); // filter: None
        for x in 0..width {
            let p = frame[y * width + x];
            raw.push((p >> 24) as u8);
            raw.push((p >> 16) as u8);
            raw.push((p >> 8) as u8);
            raw.push(p as u8);
        }
    }
    // 2) Wrap in stored DEFLATE blocks and zlib container.
    let zdata = zlib_store(&raw);
    // 3) Assemble PNG with IHDR + IDAT + IEND chunks.
    let mut out: Vec<u8> = Vec::with_capacity(zdata.len() + 128);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // colour type RGBA
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &zdata);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Wrap `raw` into a zlib stream made entirely of *stored* (BTYPE=00)
/// DEFLATE blocks. No real compression, but valid and tiny code.
fn zlib_store(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 16);
    // zlib header: CMF = 0x78 (deflate, 32K window).
    // FLG byte holds (FCHECK | FDICT<<5 | FLEVEL<<6); FDICT=0, FLEVEL=0.
    // Pick FCHECK so the 16-bit (CMF, FLG) pair is a multiple of 31.
    let cmf: u8 = 0x78;
    let flg_base: u8 = 0; // FDICT=0, FLEVEL=0; FCHECK in low 5 bits.
    let cmf_flg_no_check = ((cmf as u16) << 8) | (flg_base as u16);
    let fcheck = (31 - (cmf_flg_no_check % 31)) % 31;
    let flg = flg_base | (fcheck as u8);
    out.push(cmf);
    out.push(flg);

    // Emit stored blocks of up to 65535 bytes each.
    let mut i = 0;
    while i < raw.len() {
        let take = (raw.len() - i).min(0xFFFF);
        let last = (i + take == raw.len()) as u8;
        out.push(last); // BFINAL bit in BTYPE=00 block header
        let len = take as u16;
        let nlen = !len;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&raw[i..i + take]);
        i += take;
    }
    // Adler-32 checksum of the *uncompressed* data.
    let adler = adler32(raw);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    // Standard CRC-32 (poly 0xEDB88320) — small table-less version.
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

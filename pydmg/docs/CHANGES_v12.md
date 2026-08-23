# neogeo-rs — v12 changelog

OODA cycle vs MAME (`src/mame/snk/neogeo_spr.cpp`, `neogeo_v.cpp`) and
FBNeo (`src/burn/drv/neogeo/neo_sprite*.cpp`, `neo_palette.cpp`),
cloned at:

  * MAME  commit  f884e513
  * FBNeo commit  b93028d

## Accuracy improvements

### Palette: hardware-accurate resistor-network model
- Added `crates/neogeo-core/src/palette_lut.rs` — a 32×4 lookup table
  precomputed from MAME's `compute_resistor_weights(5 resistances
  3900/2200/1000/470/220 Ω + optional 8200 Ω pull-up for dark and 150 Ω
  pull-down for shadow)`. Identical to MAME `neogeo_v.cpp::create_rgb_lookups`.
- Re-wrote `video.rs::palette_word_to_rgb` to use the LUT instead of
  the previous linear `<< 4 | extra_bits` approximation. Each channel
  is now packed as 5 bits (1 lsb + 4 MSBs) and looked up in the
  `[normal | dark | shadow | dark+shadow]` columns. Result: pixel-
  accurate colours matching MAME, including the subtle "dark"
  attenuation that the previous approximation got wrong by 2-5 units
  per channel near the mid-range.

### Test coverage to prevent regressions
- `crates/neogeo-core/tests/video_palette.rs` — 5 tests pinning down
  the resistor LUT (endpoints, monotonicity, dark column attenuation,
  known palette-word decodings, dark-bit effect).
- `crates/neogeo-core/tests/video_sprites.rs` — 3 tests pinning down
  screen dimensions, `sprite_on_scanline` predicate, and SCB3 Y
  decoding against MAME.
- `crates/neogeo-core/tests/boot.rs` — pre-existing test was failing
  because the synthetic P-ROM data wasn't in the on-disk byte order
  that `NeoGeoBus::load_p_rom` expects. Fixed: now the test stores
  bytes pair-swapped (matching real cart-dump format).

## What did NOT change
- All sprite rendering logic in `video.rs` is unchanged — v11's
  pixel-perfect alignment with FBNeo (proven in earlier OODA cycle)
  is preserved.
- Fix-layer rendering, sprite-tile decoder, scanline list construction
  and zoom tables are byte-identical to v11.

## Verified
- `cargo build --release -p neogeo-cli`  ✓
- `cargo test  --release -p neogeo-core --tests`  → 9/9 pass
  (`boot`, 5×`video_palette`, 3×`video_sprites`).
- 200-frame Metal Slug attract run with `--dump-every-frames 1`:
  exit code 0, 1.0s wall-clock (200 fps).

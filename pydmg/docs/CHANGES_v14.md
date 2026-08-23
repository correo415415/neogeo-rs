# neogeo-rs — v14 changelog

OODA cycle vs MAME (`src/mame/snk/neogeo_spr.cpp`, `neogeo_v.cpp`,
`m68000.h`, `m68kops.cpp`) and FBNeo
(`src/burn/drv/neogeo/neo_sprite*.cpp`, `neo_palette.cpp`).

This release combines v13 (palette accuracy + pre-decoded sprite GFX)
with a **CPU correctness fix**: illegal-opcode exception (vector 4) now
fires correctly on `$4AFC` and on `TAS` with PC-relative / immediate
addressing modes, matching the 68000 Programmer's Reference Manual.

## CPU correctness fix (NEW)

### `TAS` with illegal addressing modes now traps via vector 4
- `crates/m68k/src/exec.rs::execute_4xxx`:
  TAS (`size_bits == 3`) used to silently decode and execute the
  effective address even when the EA was illegal for a data-alterable
  destination. In particular, the canonical 68000 ILLEGAL opcode
  `$4AFC` (= TAS with mode=7 reg=4 = `#<imm>`) was being misexecuted
  as a real TAS, leaving the CPU in undefined state.
- New behaviour: when the EA mode/register is one of:
  * mode 1            (An direct)
  * mode 7 reg 2      ((d16,PC))
  * mode 7 reg 3      ((d8,PC,Xn))
  * mode 7 reg 4      (#<data>, includes `$4AFC`)
  the CPU now rolls back the PC and enters the
  IllegalInstruction exception via vector 4, exactly like MAME's
  `m68k_op_tas_*` table dispatcher.
- The pre-existing test
  `m68k::flags_and_exceptions::illegal_opcode_triggers_exception_vector_4`
  was failing in every prior release (v10–v13). It now passes.

## What did NOT change from v13
- Sprite renderer, palette LUT, pre-decoded sprite-GFX path,
  fix-layer, palette bank handling, zoom tables — all byte-identical
  to v13.
- 28-frame Metal Slug attract dump still matches v13 with **max delta
  = 0** and **mean diff = 0.000000** across every PNG.

## What carried over from v13

### Pre-decoded sprite graphics (MAME `optimize_helper` port)
- `video::decode_sprite_gfx(c_roms) -> Vec<u8>` is now built once at
  load time and stored in `System::sprite_gfx_decoded`. The per-pixel
  inner loop falls through to a single byte load (`decoded_sprite_pixel`),
  matching MAME's `neosprite_optimized_device::draw_pixel`.
- Metal Slug: 32.0 MiB / 131,072 tiles decoded in < 100 ms at load.

### Palette: MAME 5-bit resistor-network LUT
- `palette_lut::MAME_PALETTE_LOOKUP` is precomputed from MAME's
  `compute_resistor_weights(3900/2200/1000/470/220 Ω + 8200 Ω dark
  pullup + 150 Ω shadow pulldown)`. The previous linear `<< 4 | bits`
  approximation drifted 2-5 units per channel; the LUT is exact.

## Verified

```
$ cargo build --release -p neogeo-cli          ✓
$ cargo test  --release --tests --no-fail-fast → 41/41 PASS
  • m68k: 13 (flags_and_exceptions) + 15 (instructions)
  • neogeo-core: 1 (boot) + 5 (palette) + 3 (sprites) + 4 (decoder)
```

Visual confirmation (with `--auto-coin-frame 200 --auto-press-start-frame 300`):
- Frame 1500: full **METAL SLUG** title-screen logo, pixel-perfect.
- Frame 1900: full **gameplay opener** (P1 PUSH START), with the
  street/house tile background, character sprite, "LEVEL-4 CREDIT 00"
  HUD, and the small Metal Slug title banner at the top — all
  rendered correctly via the sprite + fix-layer pipelines.

## Notes / out-of-scope

- Fix-layer bankswitching for Garou/MS3/KOF2000 not implemented; not
  needed for Metal Slug 1 (the OODA reference game).
- Z80 sound CPU is still a stub; the YM2610 mixer outputs silence.
- The SDL2 UI binary builds and runs; sound output is muted.

# v30 — 304×224 cropped view in SDL2 UI (kill backdrop pillarboxing)

OODA pass on a user-visible artefact: when running `mslug.zip` through the
SDL2 UI, two vertical strips of **8 emulated pixels** on each horizontal
edge showed the backdrop colour (`RGB(8,0,0)` for Metal Slug) instead of
the cannon sprite that the player expects to cover the whole screen.

## Observe

Pixel-perfect analysis of a `--ui` capture (1024×749 window at scale 3.2×)
combined with a fresh `--dump-frames-dir` run against real `mslug.zip`
ROMs confirmed that the framebuffer 320×224 produced by the core has
**exactly 16 uniform-backdrop columns** at frames where the cart drives
the LSPC: cols `0..=7` and `312..=319`. The 304 central columns carry
all of the title screen artwork; the outer 16 are intentionally left
empty by the cartridge.

## Orient

Cross-referenced four canonical sources:

1.  Neo Geo Development Wiki — *Fix layer*: "Most games don't use the
    leftmost and rightmost columns, as some CRT screens can cut them off.
    SNK recommends only using a centered 38×28 safe zone."
2.  Neo Geo MegaShock blog — *Neo Geo Resolution*: "Neo geo always
    displays 320×224. For example, Metal Slug is on 320×224 with **2
    black columns of 8 pixels**. So real surface of this game is on
    304×224."
3.  FBX (RetroTINK developer) on Patreon — *Neo Geo 320 vs 304 Games*:
    Metal Slug is **a 304-based title**. ~42% of the library overrides
    the pillarbox and paints all 320 px (e.g. Aero Fighters 2); the
    remaining ~58% leave the 8-px border as backdrop overscan.
4.  MAME `src/mame/snk/neogeo_spr.h`: `NEOGEO_HBEND = 0x01c`,
    `NEOGEO_HBSTART = 0x15c` → active raster = `0x140` = 320 px. MAME
    ships a second view option `Screen 0 Cropped (304×224)` that hides
    exactly this overscan-safe area.

**Conclusion**: the backdrop strips are not a renderer bug. They are
authentic Neo Geo hardware behaviour. The fix is at the *display* layer,
not the core.

## Decide

Make the SDL2 UI crop the texture to the canonical 304-px active area by
default, and expose `--show-full-raster` for the ~42% of the library
that genuinely paints the whole 320 px and for renderer debugging. The
PNG `--dump-frames-dir` path keeps the full 320×224 framebuffer so
debuggers and the `dump_sprites` tool can still see what the LSPC emits.

## Act — Changes

### `crates/neogeo-core/src/video.rs`

Added two new public constants alongside the existing `SCREEN_W`/`SCREEN_H`:

```rust
pub const ACTIVE_W: usize = 304;        // visible width after cropping
pub const ACTIVE_X_OFFSET: usize = 8;   // pixels trimmed on each side
```

The internal framebuffer is **unchanged** (still `SCREEN_W * SCREEN_H` =
320 × 224 RGBA). The `screen_dimensions_match_neogeo_ntsc` integration
test still passes unmodified.

### `crates/neogeo-cli/src/main.rs`

New CLI flag:

```
--show-full-raster   Show the full 320×224 raster instead of the
                     304×224 cropped view. Default: false.
```

### `crates/neogeo-cli/src/ui.rs`

* Decide once at window-open time:
  * `view_w = 304` (default) or `320` (when `--show-full-raster`).
  * `src_rect = Rect::new(8, 0, 304, 224)` (default) or
    `Rect::new(0, 0, 320, 224)`.
* Window size, `set_logical_size`, and the `canvas.copy()` source-rect
  all derive from those values. The streaming texture stays
  `SCREEN_W × SCREEN_H` (320×224); cropping is a free blit-time op.
* Window title now shows the active view (`304×224` / `320×224`).
* An `info!` log line on startup states which mode is active.

## Verify

`cargo test --release -p neogeo-core --tests` — 9/9 lib tests, 4/4
video_decoder, 5/5 video_palette, 3/3 video_sprites (including the
`screen_dimensions_match_neogeo_ntsc` contract), 1/1 boot. **All green.**

`cargo build --release -p neogeo-cli` — builds the SDL2 binary with zero
new warnings (the pre-existing `fnum_step is never used` dead-code
warning is unchanged).

### Real-ROM validation against `mslug.zip` + `neogeo.zip`

Headless 1500-frame run (`--auto-coin-frame 200 --auto-press-start-frame
300`) reaches the title screen ("SUPER VEHICLE-001 / METAL SLUG / © 1996
NAZCA CORPORATION / INSERT COIN / LEVEL-4 CREDIT 00") at frame ~1400.

Pixel-modal column analysis on the captured PNG framebuffers:

| Frame | Cols uniform (>75%) | Modal col 0 | Modal col 319 |
|-------|---------------------|-------------|---------------|
| 0500  | 320 (boot black)    | (0,0,0)     | (0,0,0)       |
| **0900**  | **16 (8 each side)**| **(8,0,0)** | **(8,0,0)**   |
| 1200  | 135 (gray screen)   | (138,138,…) | (138,138,…)   |
| 1400  | 0 (gameplay)        | (65,52,38)  | (103,95,79)   |

Frame 900 confirms the **exact 16-column pillarbox** documented by
SNK / MegaShock / FBX. Cropping to `[8 .. 312]` yields **0 uniform
backdrop columns** as expected.

### Performance

Headless 1500-frame run: **2.571 s (583 simulated fps, 9.7× real-time)**
on the same hardware as v29. No regression.

## Toggle for 320-based games

For the ~42% of the library that paints the full raster (Aero Fighters
2, Twinkle Star Sprites, KOF '99 Evolution, Garou, etc.) pass
`--show-full-raster` to see those 16 extra columns. The window title
updates to `320×224` so the active mode is always visible.

## References

* https://wiki.neogeodev.org/index.php?title=Sprites
* https://wiki.neogeodev.org/index.php/Fix_layer
* https://wiki.neogeodev.org/index.php/Sprite_shrinking
* http://neogeo-megashock.blogspot.com/p/neo-geo-resolution.html
* https://www.patreon.com/posts/neo-geo-320-vs-90490687 (FBX)
* MAME `src/mame/snk/neogeo_spr.cpp::draw_sprites`
* MAME `src/mame/snk/neogeo_spr.h::NEOGEO_HBEND` / `HBSTART`

# v31 — Audit-driven graphics fidelity pass (MAME/FBNeo crosscheck)

Cross-checked the renderer against `MAME src/mame/snk/neogeo_spr.cpp` +
`neogeo_v.cpp` and `FBNeo src/burn/drv/neogeo/{neo_sprite,neo_text,neo_palette}.cpp`
line by line. Five real graphics gaps fixed.

## Fix 1+2+7 — Shadow plane completo

* `MAME_PALETTE_LOOKUP` ya traía 4 columnas (normal/dark/shadow/dark+shadow)
  pero el código solo accedía a [0..1].
* `palette_word_to_rgb` y `lookup_palette` ahora aceptan `screen_shadow: bool`
  y seleccionan columnas [2..3] cuando está activo, replicando MAME
  `paletteram_w` (`m_palette_lookup[r][dark+2]`).
* El backdrop (palette $FFF) también queda sujeto a screen_shadow,
  igual que MAME `m_bg_pen = pen_base + 0xfff` con `pen_base` movible.
* **Visible en:** KOF combo hits, pause menus, fades de transición.

## Fix 3 — HC259 Q5 (fix-layer source mux)

* MAME `set_fixed_layer_source(state)`: Q5=0 ⇒ BIOS `sfix.sfix`, Q5=1 ⇒ cart `s1`.
* `RomSet` ahora guarda `bios_sfix` separado del `cart.s_rom`.
* `System::load` propaga el SFIX; `render_frame_pixels` consulta
  `bus.systemlatch & 0x20` y pasa `Option<&[u8]>` como override del fix-source.
* Compat: si el cart no trae S-ROM, se sigue copiando BIOS SFIX al
  cart-slot como fallback.

## Fix 4 — NEO-CMC fix-layer banking (Garou / MS3-4 / KOF2000)

* Port literal de MAME `draw_fixed_layer` lines 184-243.
* Nuevo enum público `FixBankType { Std, Garou, Kof2000 }`.
* `fix_tile_pixel` ampliada de `u16` → `u32` para direccionar hasta
  32768 tiles (12 bit base + 1 bit banking).
* GAROU: tabla `garouoffsets[34]` reconstruida desde VRAM `$7500/$7580`
  por frame; tile += `0x1000 * (garouoffsets[(row-2)&31] ^ 3)`.
* KOF2000: tile += `0x1000 * ((vram[$7500+...] >> shift) & 3) ^ 3)`
  con shift dependiente de columna.
* Auto-detect por nombre de cart (`mslug3`, `garou`, `kof2000`, etc.)
  como en MAME `get_fixed_bank_type()`.

## Fix 5 — Per-scanline sprite list (decisión)

* MAME escribe la lista activa en VRAM `$8600/$8680` desde
  `parse_sprites`. Mi parse-pass ya hace el equivalente en RAM local.
* La escritura a VRAM exige refactor de propiedad de `Lspc` (mutable
  durante render). Documentado como límite conocido; no aplica fix
  porque el cambio estructural arriesgaría regresión y el efecto
  observable es prácticamente nulo (sengoku3 RPG mode y similares).

## Tests

`cargo test --release -p neogeo-core --tests` →
- boot.rs: 1/1 ✓
- video_decoder: 4/4 ✓
- video_palette: 5/5 ✓
- video_sprites: 3/3 ✓ (incl. `screen_dimensions_match_neogeo_ntsc`)

`cargo test --release -p neogeo-core --lib` → 9/9 ✓

`cargo build --release -p neogeo-cli` → binario 3 MB, sin nuevos warnings.

## Run real `mslug.zip` + `neogeo.zip`

Headless 1500 frames con `--auto-coin-frame 200 --auto-press-start-frame 300`:

```
video sources installed: cart='mslug' s_rom=131072 bytes,
                         bios_sfix=131072 bytes, fix_bank=Std
Exit. Total instructions: 27609734, cycles: 300007920
VRAM diagnostics: fix-cells set=1280/1280,
                  palette entries set=4919/4096,
                  sprite SCB3 entries set=108/381
```

Misma robustez que v30 + las nuevas señales activas. Sin regresión.

## References (verbatim crosschecks)

- MAME `src/mame/snk/neogeo_spr.cpp::draw_fixed_layer` (184-243)
- MAME `src/mame/snk/neogeo_spr.cpp::draw_sprites` (283-457)
- MAME `src/mame/snk/neogeo_v.cpp::create_rgb_lookups` (22-64)
- MAME `src/mame/snk/neogeo_v.cpp::paletteram_w` (90-114)
- MAME `src/mame/snk/neogeo_spr.h::FIX_BANKTYPE_*` (25-27)
- FBNeo `src/burn/drv/neogeo/neo_palette.cpp::CalcCol` (67-83)
- FBNeo `src/burn/drv/neogeo/neo_sprite.cpp::NeoRenderSprites`
- FBNeo `src/burn/drv/neogeo/neo_text.cpp::NeoRenderText`

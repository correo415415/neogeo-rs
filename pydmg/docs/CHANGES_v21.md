# neogeo-rs v21 — Fix overscan + soporte BIOS separado (parent zip) + Metal Slug 2

## Resumen de cambios

### 1. Fix gráfico: bloques negros en esquinas inferiores (`crates/neogeo-core/src/video.rs`)
Las pantallas de título de muchos juegos Neo Geo (Metal Slug 1, 2, etc.)
mostraban dos rectángulos negros en las esquinas inferiores izquierda y
derecha. Causa raíz medida sobre la VRAM real:

- El BIOS MVS escribe **tile `$0020` con paleta 0** en las columnas
  extremas del fix layer (col 0 y col 39). Ese tile en `sfix.sfix`
  contiene **píxeles sólidos índice 2**, y la entrada 2 de la paleta 0
  es `$0000` = NEGRO PURO.
- En hardware real esas columnas caen dentro del overscan del CRT y
  quedan tapadas por el bezel del cabinet, por lo que nunca se ven.
- MAME dibuja las 40 columnas y depende del bezel para ocultarlas.
- FBNeo recorta el rango a cols 1..38 por defecto (su modo
  `nNeoScreenWidth == 304`, ver `neo_text.cpp:358-364`).

Aplicada solución estilo FBNeo: nueva constante
`FIX_LAYER_OVERSCAN_COLS: usize = 1` que recorta el bucle del fix layer
a `[1..=38]`. Sprite layer y backdrop siguen cubriendo los 320 px, así
que las columnas trimmedas quedan ocupadas por el fondo del juego, no
por basura del BIOS. Verificado píxel a píxel: bloques negros pasaron
de 256+256 píxeles a 0+0.

### 2. Sprite renderer estilo MAME exacto (`crates/neogeo-core/src/video.rs`)
Reescrito `render_sprite_layer_inner` para seguir el flujo
`parse_sprites + draw_sprites` de MAME al milímetro, en lugar del
"Pass 1: anchors globales / Pass 2: render" anterior:

- Por cada scanline se construye la lista activa de 96 sprites
  exactamente como `neosprite_base_device::parse_sprites`.
- Las cadenas sticky (bit 6 de SCB3) se resuelven dentro del bucle de
  draw, sobre la lista activa, no globalmente. Esto evita "perder"
  tiles de borde en composiciones grandes (background del título de
  Metal Slug).

### 3. Soporte BIOS separado / parent zip (`crates/neogeo-core/src/rom.rs`)
Para arrancar sets MAME modernos donde el BIOS, `000-lo.lo`,
`sfix.sfix` y `sm1.sm1` viven en un set parent (`neogeo.zip`) y el
cartucho solo trae los slots P/S/M/V/C (`mslug2.zip`).

- Nuevo método público `load_parent_bios_zip(path)`. Se llama **antes**
  del cart loader. Auto-selecciona BIOS, importa Y-zoom table, y
  guarda sfix/sm1 como fallback en `self.cart` para que el cart loader
  pueda heredarlos si no los trae.
- Nuevo método `pick_bios_from_zip(path, name)` para `--bios-name`.
- `categorise()`: ya no descarta `sfix.sfix`/`sm1.sm1`. Los inserta
  en sus buckets con el prefijo `~bios-` para que ordenen al final
  (cualquier ROM del cart con nombre normal gana).
- `finalise_from_bucket()`: ordena los buckets `s`/`m` por nombre
  antes de elegir, y hereda los fallbacks BIOS-side al cart si éste no
  trae su propio s_rom/m_rom.

### 4. Detector de slot extendido (`crates/neogeo-core/src/rom.rs`)
`matches_slot()` ahora reconoce tres convenciones de naming en lugar
de dos:

- **Pattern A** (ya existía): extensión 2-char `<letter><digit>` →
  `mslug.p1`, `241-c4.c4`.
- **Pattern B** (nuevo): extensión 3-char `<prefix><letter><digit>` con
  stem también acabando en `<slot><n>` → `241-p2.sp2` (banco 2 MiB de
  Metal Slug 2), `garou.ep1`. Sin esto Metal Slug 2 cargaba solo 1 MiB
  de P-ROM (faltaba todo el banco superior).
- **Pattern C** (ya existía): `*-<slot><n>.bin` → `201-p1.bin`.

### 5. Prioridad de BIOS revisada
`pick_bios()` ahora prefiere `asia-s3.rom` / `sp-s3.sp1` /
`sp-s2.sp1` / `sp-s.sp1` antes que los BIOSes japoneses o universe.
Estos son los oficiales MVS con el layout SYSTEM_IO documentado
(`$C0044A`), únicos en los que el flujo automatizado coin → start →
gameplay funciona correctamente headless.

### 6. CLI: nuevas flags `--bios-zip` y `--bios-name`
```
./neogeo --bios-zip neogeo.zip --cart mslug2.zip --hardware mvs ...
./neogeo --bios-zip neogeo.zip --bios-name uni-bios_4_0.rom --cart mslug.zip
```

## Validación

### mslug (regresión, formato all-in-one)
```
Auto-selecting BIOS 'asia-s3.rom' (131072 bytes) from zip
Loaded cart 'mslug' — P:2097152 S:131072 M:131072 V:8388608 C:16777216
YM2610: fm_keyon=88 adpcma_keyon=28 adpcmb_keyon=0
VRAM: fix-cells 1280/1280, palette 4919/4096, sprite SCB3 108/381
bottom-left black px: 0 / bottom-right black px: 0  (fix overscan OK)
```

### mslug2 (nuevo, formato MAME split + BIOS separado)
```
Auto-selecting BIOS 'sp-s3.sp1' (131072 bytes) from parent set
Using BIOS fallback fix-tile S-ROM (sfix.sfix, 131072 bytes)
Using BIOS fallback audio M-ROM (sm1.sm1, 131072 bytes)
Loaded cart 'mslug2' — P:3145728 S:131072 M:131072 V:8388608 C:33554432
PLAYER_START @$0128 BIOS_START_FLAG=$01 inst=37101725
YM2610: fm_keyon=458 adpcma_keyon=1779 adpcmb_keyon=44
```

Frames visuales obtenidos:
- f=2000: title screen "METAL SLUG 2 - SUPER VEHICLE-001/II" + attract
- f=3000: "HOW TO PLAY" + Marco + Metal Slug tank + texto "Jump to board
  the Enforced Metal Slug"
- f=5000-6000: "SOLDIER SELECT" con Marco, Eri, Tarma, Fio + timer.

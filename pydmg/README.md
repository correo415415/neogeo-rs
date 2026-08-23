# pydmg-neogeo

Núcleo emulador Neo Geo en Rust. Reorganización del proyecto upstream `neogeo-rs v31` en un único crate con layout modular alineado a la arquitectura hardware real, con refinos del subsistema gráfico contrastados línea a línea con MAME y FBNeo.

## Estructura

```
Cargo.toml
main.rs
neogeo/
  lib.rs
  audio/     audio_bus.rs, ym2610.rs
  cpu/       m68k/{bus,cpu,ea,exec}  +  z80/{cpu,exec,flags}
  graphics/  lspc.rs, palette_lut.rs, video.rs
  memory/    bus.rs, rom.rs, upd4990a.rs
  neogeo/    system.rs
  dead_code/ nuked_opn_scaffold.rs.txt, m68k_decoder_placeholder.rs
cli/
  main.rs    (clap CLI runner)
  ui.rs      (SDL2 frontend, gated tras feature `ui`)
tests/
  boot.rs, m68k_*.rs, video_*.rs (8 binarios, 61 tests verdes)
  _disabled/  m68k_/z80_single_step_tests.rs (corpus 8 GiB externos)
examples/
  fm_test.rs, dump_sprites.rs
docs/
  CHANGES_v12..v31, USO.txt
```

## Features

No hay features Cargo. SDL2 es dependencia obligatoria (igual que `neogeo-rs v30/v31`). Un único `cargo build --release` produce el binario con GUI fullscreen.

## Uso

> ⚠️ **Importante**: este repositorio sólo contiene **código fuente**. Para usar el emulador hay que compilarlo antes con `cargo build --release`. No existe `target/release/pydmg-neogeo.exe` hasta que tú lo generes.

### Windows (paso a paso)

Prerrequisitos (una sola vez):

1. **Instala Rust**: <https://www.rust-lang.org/tools/install> (toolchain MSVC por defecto).
2. **Instala SDL2** para Windows:
   - Descarga `SDL2-devel-2.32.4-VC.zip` desde <https://github.com/libsdl-org/SDL/releases/tag/release-2.32.4>.
   - Copia los `.lib` de `lib\x64\` (`SDL2.lib`, `SDL2main.lib`, `SDL2test.lib`) a:
     `%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\lib\`
   - Guarda `SDL2.dll` (de `lib\x64\`) en un sitio cualquiera; lo copiarás junto al .exe después de compilar.

Compilar y ejecutar (mismo flujo que el v30 que ya te funcionaba):

```cmd
REM 1) Abre cmd.exe en la raíz del proyecto (carpeta con Cargo.toml).
cd C:\Users\PC\Desktop\SANTI\neogeo\pydmg-neogeo-final

REM 2) Compila. Genera target\release\pydmg-neogeo.exe.
REM     NO necesita --features. SDL2 va incluido siempre.
cargo build --release

REM 3) Copia SDL2.dll al lado del .exe (igual que en el v30).
copy C:\ruta\a\SDL2-devel\lib\x64\SDL2.dll target\release\

REM 4) Lanza el emulador (fullscreen, 320x224 completo).
target\release\pydmg-neogeo.exe ^
    --bios-zip ..\roms\neogeo.zip ^
    --cart ..\roms\mslug.zip

REM 5) Vista 304x224 recortando 1 columna por lado.
target\release\pydmg-neogeo.exe ^
    --bios-zip ..\roms\neogeo.zip ^
    --cart ..\roms\mslug.zip ^
    --crop

REM 6) Ventana en lugar de fullscreen.
target\release\pydmg-neogeo.exe ^
    --bios-zip ..\roms\neogeo.zip ^
    --cart ..\roms\mslug.zip ^
    --windowed --ui-scale 3

REM 7) Help.
target\release\pydmg-neogeo.exe --help
```

Si prefieres MSYS2/MinGW: `pacman -S mingw-w64-x86_64-SDL2` y `cargo build --release`; la DLL queda en PATH automáticamente.

### Linux

```bash
# 1) SDL2 dev.
sudo apt install libsdl2-dev

# 2) Compila.
cargo build --release

# 3) Lanza (fullscreen 320x224).
./target/release/pydmg-neogeo \
    --bios-zip roms/neogeo.zip --cart roms/mslug.zip

# 4) Vista 304x224 recortada.
./target/release/pydmg-neogeo \
    --bios-zip roms/neogeo.zip --cart roms/mslug.zip --crop

# 5) Ventana 3x.
./target/release/pydmg-neogeo \
    --bios-zip roms/neogeo.zip --cart roms/mslug.zip \
    --windowed --ui-scale 3
```

### Controles de la GUI

| Tecla              | Acción                                        |
|--------------------|-----------------------------------------------|
| `F11`              | Alterna fullscreen ↔ windowed en caliente     |
| `ESC`              | Salir                                         |
| `5`                | Insertar moneda (COIN1)                       |
| `Enter` / `1`      | P1 START                                      |
| Flechas            | Direccional P1                                |
| `Z`/`X`/`C`/`V`    | Botones A/B/C/D de P1                         |
| `RShift` / `3`     | SELECT P1                                     |

El título de la ventana muestra el tamaño real de la vista (`320×224` o `304×224` según `--crop`) y `[FS]`/`[WIN]` con el modo actual.

### Modo headless (para dumps PNG / WAV)

```bash
./target/release/pydmg-neogeo --headless \
    --cart mslug.zip --max-frames 60 \
    --dump-frames-dir /tmp/frames

./target/release/pydmg-neogeo --headless \
    --bios-zip neogeo.zip --cart mslug2.zip \
    --max-frames 1200
```

## Tests

```
cargo test --jobs 1   →  61 / 61 OK (sin SingleStepTests externos)
```

Cobertura:

| Suite                          | tests |
|--------------------------------|-------|
| unit lib                       |  9    |
| tests/boot.rs                  |  1    |
| tests/m68k_flags_and_exceptions|  13   |
| tests/m68k_instructions        |  15   |
| tests/video_decoder            |  4    |
| **tests/video_fix_layer**      |  **7** |
| tests/video_palette            |  5    |
| **tests/video_render_e2e**     |  **3** |
| tests/video_sprites            |  4    |
| **Total**                      | **61** |

Los SingleStepTests (m68k 317 500 / z80 1 604 000 sub-tests) están en `tests/_disabled/`: requieren clonar https://github.com/SingleStepTests/m68000 y https://github.com/SingleStepTests/z80 (~8 GiB).

## Refinos de gráficos (vs. v31)

Contrastado línea a línea con MAME `src/mame/snk/neogeo_spr.{cpp,h}`, `neogeo_v.cpp` y FBNeo `src/burn/drv/neogeo/{neo_text,neo_sprite}.cpp`:

1. **Fix bug histórico**: la máscara `& 0x87FF` en lecturas de VRAM era incorrecta (no es `2^n−1`). `0x7500 & 0x87FF == 0x0500` aliaseaba a SCB1's tilemap, rompiendo el **bankswitching de fix layer GAROU/KOF2000 por completo**. Reemplazado por accesos directos sin máscara, igual que MAME.
2. **`FIX_LAYER_OVERSCAN_COLS` default 1 → 0**: por defecto se dibujan las 40 columnas igual que MAME. El recorte 304-px bezel-safe se ofrece como argumento opcional a `render_fix_layer_inner_with_bank_and_crop`.
3. **`sprite_on_scanline` alineado a MAME**: incluye explícitamente el caso `rows == 0` que MAME añadió upstream.
4. **Documentado** que el `parse_sprites` mantiene el `if rows == 0 continue` previo a la llamada a `sprite_on_scanline` para no consumir slots del presupuesto de 96/línea.

## Smoke-test gráfico con ROMs reales

Capturas PNG generadas con `--dump-frames-dir`:

- **`neogeo` BIOS solo**: hard-reset → patrón verde de diagnóstico (idéntico al hardware sin cart insertado).
- **`mslug.zip`** (BIOS embebido `asia-s3.rom`):
  - Frame 300 → eyecatcher Neo Geo en rotación (logo H-flipped intencional)
  - Frame 540 → `NEO·GEO / MAX 330 MEGA / PRO-GEAR SPEC / SNK` estático
  - Frame 900 → pantalla de título: cañón Gatling con `INSERT COIN / LEVEL-4 / CREDIT 00`
  - Frame 1100 → escena attract del Metal Slug tank
- **`mslug2.zip + neogeo.zip` parent BIOS**:
  - Frame 1100 → animación de logo "Metal Slug 2" con partículas

Métricas finales típicas (mslug, 600 frames):
```
fix-cells set=1280/1280
palette entries set=4872/4096    (todo el banco activo)
sprite SCB3 entries set=215/381  (vivos)
FM keyon=[8,8,8,49] nz=[285612 ≈ 99%]
```

## Cambios respecto a v31

- Workspace de 4 crates → crate único con módulos por subsistema hardware
- `neogeo_core::*` → `pydmg_neogeo::*`
- `serde_json` opcional (solo SingleStepTests)
- UI SDL2 detrás de feature `ui`
- Sin regresiones funcionales: todos los tests v31 siguen verdes, boot real de mslug/mslug2 idéntico


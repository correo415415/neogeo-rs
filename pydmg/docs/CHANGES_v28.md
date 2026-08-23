# Cambios v28 — YM2610 audio fidelity pass

Foco: aproximar el comportamiento del YM2610 a FBNeo y MAME en LFO, AM por
operador, AMS/PMS y feedback FM. Iteración OODA sobre la base v26/v27.

## Cambios en `crates/neogeo-core/src/ym2610.rs`

1.  **LFO global** (registro `$22`):
    - Nuevo estado en `FmOpn`: `lfo_enabled`, `lfo_rate`, `lfo_delay`,
      `lfo_pos`, `lfo_am`, `lfo_pm`.
    - Implementado el reloj del LFO con la tabla
      `LFO_SAMPLES_PER_STEP = [108,77,71,67,62,44,8,5]`
      (FBNeo `fm.c::init_tables` / `lfo_samples_per_step`).
    - Posición LFO de 7 bits, AM triangular 0..126..0, y PM = `pos >> 2`.
    - Si LFO se deshabilita, se resetea `lfo_am`/`lfo_pm` a 0.

2.  **AM por operador** (`$60..$6F` bit 7):
    - `FmOp::am_enabled` añadido. En `write_reg` 0x60 se captura `val & 0x80`.
    - En `step_one_with_per_channel` la atenuación de envolvente
      de cada operador se suma con
      `lfo_am_attenuation(am_enabled, ams, lfo_am)`.

3.  **AMS y PMS por canal** (`$B4..$B6`):
    - `FmCh::ams` (shift) y `FmCh::pms` (depth) nuevos campos.
    - `ams` se decodifica con la tabla `LFO_AMS_DEPTH_SHIFT = [8,3,1,0]`
      (FBNeo `lfo_ams_depth_shift`).
    - `pms` se guarda como `val & 0x07`.

4.  **PM aplicado a la fase**:
    - Nueva tabla `LFO_PM_OUTPUT` extraída literalmente de FBNeo
      `fm.c` (matriz `7*8 x 8`).
    - Nuevo helper `lfo_pm_offset(block_fnum, pms, lfo_pm)` que
      reconstruye la onda PM (8 pasos hacia arriba, 8 hacia abajo,
      8 negativos arriba, 8 negativos abajo) y suma según los
      bits de F-NUMBER puestos a 1.
    - En el bucle de fase de cada canal el `step` final es
      `fnum_step() + pm_offset + detune`, con wrap por `FN_MAX`.

5.  **Feedback FM corregido** (registro `$B0..$B2`):
    - Antes: `fb = (val >> 3) & 0x07`. Resultaba en feedback
      siempre como un shift de 0..7.
    - Ahora: replica el comportamiento de FBNeo (
      `CH->FB = feedback ? feedback + 6 : 0;`).
      Cambia el peso del op1 en el path de realimentación y
      afecta al brillo/grano de muchas voces FM.

6.  **Decoder del puerto A**:
    - `write_reg_a` ahora despacha `$22` a `FmOpn::write_lfo`,
      manteniendo el resto del registro plano sin tocar.

## Tests

`cargo test --release -p neogeo-core --lib` -> 9/9 ok
(`adpcma_*`, `fm_smoke_generates_nonzero_output`,
`fm_pan_left_only_produces_silence_on_right`,
`fm_register_slot_order_matches_opn_layout`, etc).

## Capturas

- `mslug_bios_v28.wav` (1200 frames, 55555 Hz estéreo).
- Niveles por segundo: ver `analysis/run_bios_v28.log`.

## Fuentes

- FBNeo `src/burn/snd/fm.c`
- MAME `3rdparty/ymfm/src/ymfm_opn.cpp` y `ymfm_fm.ipp`
- wiki.neogeodev.org: FM, YM2610_registers, Z80/YM2610_interface, Boot_music

# neogeo-rs v20 — Audio en UI SDL2 + scaffold para FM cycle-accurate

## Cambios

### 1. Audio en UI SDL2 (`crates/neogeo-cli/src/ui.rs`)
La UI SDL2 nunca había abierto un device de audio, por eso `--ui` mostraba
imagen pero **sin sonido**. Añadido:
- Inicialización de `sdl2::audio` con `AudioSpecDesired { 55_555 Hz,
  estéreo, buf=1024 }` y un `AudioCallback` que drena un ring buffer
  oversize (16 384 samples ≈ 0,3 s) alimentado por el emulador frame a
  frame.
- `sys.config.audio_sample_rate` se fuerza a `Some(55_555)` al iniciar la
  UI, para que `step()` empuje muestras al `audio_buffer` de `System`.
- Cada `run_frame()` vuelca el `audio_buffer` al ring y lo limpia.

Con esto el audio funciona en tiempo real durante el gameplay desde la
UI SDL2 (mismo régimen audible que el modo headless con `--audio-out`).

### 2. Scaffold de Nuked-OPN2 (no activado todavía)
`crates/neogeo-core/src/nuked_opn_scaffold.rs.txt` contiene un port
**inicial** y automatizado a Rust de `ym3438.c` de Nuked-OPN2
(Alexey Khokholov, LGPL-2.1). Incluye:
- Las 12 tablas estáticas (logsinrom, exprom, fn_note, eg_stephi,
  pg_detune, pg_lfo_sh1/sh2, op_offset, ch_offset, lfo_cycles,
  fm_algorithm, eg_am_shift).
- La struct `Opn2` con todos los campos del chip (ciclos, LFO, PG, EG,
  FM, timer A/B, registros).
- Esqueleto de las 24 funciones (OPN2_Reset, OPN2_Clock, OPN2_Write,
  OPN2_PhaseGenerate, OPN2_EnvelopeADSR, OPN2_FMGenerate, ...).

Estado: **el port mecánico tiene errores sintácticos** que requieren una
pasada manual (Rust no acepta declaraciones `Bit32u i;` dentro de bloques,
ni `i++`, ni `type` como nombre de parámetro). Quedan como base sólida
para una sesión dedicada que sustituya el FM aproximado actual por un
motor cycle-accurate. El fichero termina en `.rs.txt` deliberadamente
para que `cargo build` no lo intente compilar.

## Estado del audio actual (sin cambios respecto a v19)
- Headless con asia-s3 + gameplay real: `fm_keyon=220`, `adpcma_keyon=1336`,
  `adpcmb_keyon=144`. WAV 83 s, absmax=26707, 82.5% muestras no-cero,
  estéreo, 0% clipping.
- BGM + SFX + voces ADPCM-A audibles. FM es la versión aproximada (4 op
  pero 2 efectivos, algoritmos simplificados), no cycle-accurate.

## Próximo paso para FM "perfecto"
Completar el port manual del scaffold de Nuked: arreglar declaraciones
`let mut` en lugar de `Bit32u`, sustituir `i++` por `+= 1`, renombrar
parámetros que choquen con keywords (`type` → `kind`), y ajustar el
adaptador entre `Opn2` y nuestro `Ym2610` (mapear FM ch 1/2/5/6 →
canales 0..3 del YM2610, traducir la interfaz `write_port`).

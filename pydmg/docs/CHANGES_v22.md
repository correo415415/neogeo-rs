# neogeo-rs v22 — Audio multi-canal corregido (alineado con ymfm/MAME y FBNeo)

## Resumen

Tras inspeccionar a fondo MAME `ymfm` (`ymfm_adpcm.cpp`, `ymfm_ssg.cpp`,
`ymfm_opn.cpp`) y FBNeo (`fm.c`, `ymdeltat.c`, `neo_run.cpp`), se identificaron
y corrigieron **seis bugs profundos** en el subsistema YM2610 que dejaban
canales mudos o atenuados incorrectamente. Tras este parche:

- **FM**: los 4 canales wired al DAC (FM1, FM2, FM3, FM4) producen audio. Antes
  sólo FM2 y FM4 sonaban.
- **ADPCM-A**: las 6 voces (drums, voces, SFX) suenan con la curva de
  volumen exacta de ymfm.
- **SSG**: motor reescrito con tabla `s_amplitudes` y decodificación de
  envelope exactas de ymfm.
- **ADPCM-B**: escalado lineal alineado con ymfm; estado de reset = mute
  (no más samples fantasma antes de configurar el canal).

## Bugs corregidos

### 1. Curva de volumen ADPCM-A
Antes: `scaled = s * (v + 1) / 32` (lineal arbitrario, muy bajo).
Después (verbatim de `ymfm_adpcm.cpp::adpcm_a_channel::output`):
```
vol   = (IL ^ 0x1F) + (TL ^ 0x3F)        // 0..63 (master + per-channel)
if vol >= 63 -> mute
mul   = 15 - (vol & 7)
shift = 4 + 1 + (vol >> 3)
value = (((acc12 << 4) * mul) >> shift) & !3
```
Resultado: el volumen de voces/percusión sube 4-5x (RMS BIOS 186 -> 1377,
peak BIOS 1592 -> 12176).

### 2. Pan ADPCM-A invertido
Antes: `pan & 0x80 -> right; pan & 0x40 -> left`.
Después (ymfm `ch_pan_left/right`): bit 7 = LEFT, bit 6 = RIGHT.

### 3. Canales FM mudos (decoder $28)
Antes: el decoder rechazaba `c=2` y `c=5`, mapeando sólo `{0,1,3,4}`.
Resultado: FM1 y FM3 mudos (`per_channel keyon=[0, 56, 0, 106]`).
Después: la `FmOpn` expone 6 canales lógicos OPN-B y el decoder pasa
`c in {0..5}` directamente, como ymfm. Las 4 voces wired del YM2610 mapean:
`c=1 -> FM1, c=2 -> FM2, c=4 -> FM3, c=5 -> FM4`.
Tras el fix: `per_channel keyon=[56, 59, 106, 136]` — los 4 canales suenan.

### 4. FM con los 8 algoritmos OPN y 4 operadores
Antes: aproximación `op0 -> op1 -> out` ignorando OP3/OP4.
Después: implementación verbatim de las 8 topologías OPN (alg 0..7) con los
cuatro operadores `op[0..3]`, siguiendo `ymfm_opn.h::operator_map` y
`fbneo/fm.c`. Topologías:
```
alg 0:  OP1 -> OP2 -> OP3 -> OP4 -> out
alg 1:  (OP1 + OP2) -> OP3 -> OP4 -> out
alg 2:  (OP1 + (OP2 -> OP3)) -> OP4 -> out
alg 3:  (OP1 -> OP2) + (OP3 -> OP4) -> out
alg 4:  (OP1 -> OP2) + (OP3 -> OP4) -> out
alg 5:  OP1 -> (OP2, OP3, OP4) -> out
alg 6:  (OP1 -> OP2) + OP3 + OP4 -> out
alg 7:  OP1 + OP2 + OP3 + OP4 -> out
```

### 5. SSG reescrito al modelo ymfm
- Tabla `SSG_AMPLITUDES[32]` verbatim de `ymfm_ssg.cpp::s_amplitudes`.
- LFSR de 17 bits con feedback de bits 0 y 3.
- Envelope: 32-step state machine + 4-bit shape decode exacto (hold,
  alternate, attack, continue).
- Clocking: SSG core a master/8, host mix a master/144 -> 18 ticks SSG por
  sample (acumulador Q16 fraccionario).

### 6. ADPCM-B escalado y reset
- Mix: `(b * level) >> 8` per `ymfm_adpcm.cpp::adpcm_b_channel::output`.
- Reset: `volume = 0, pan = 0, delta = 0` (estado ymfm). Antes:
  `volume = 64, pan = 0xC0`, lo que hacía sonar samples fantasma si el
  driver no había configurado el canal antes del key-on.

## Verificación (mslug, 6000 frames, MVS, asia-s3.rom)

```
YM2610: fm_keyon=357 adpcma_keyon=1531 adpcmb_keyon=180
nonzero_samples: fm=1.81M  adpcma=24.06M  adpcmb=2.48M  ssg=270K

Per-channel ADPCM-A keyon = [170, 230, 398, 251, 203, 279]
Per-channel ADPCM-A nz    = [3.5M, 3.9M, 4.0M, 4.3M, 4.1M, 4.2M]
Per-channel FM      keyon = [ 56,  59, 106, 136]
Per-channel FM      nz    = [362K, 363K, 1.10M, 1.36M]

WAV: 100s
  L peak=32768 nz=91.0% rms=3172 clip=2 samples
  R peak=32768 nz=89.5% rms=3267 clip=2 samples
```

Los 6 canales ADPCM-A reciben key-ons y suenan. Los 4 canales FM emiten.
Música BGM, percusión, voces y SFX coexisten en el mix.

Dinámica temporal del WAV (gameplay real):
- 0-12 s: arranque BIOS (silencio, sólo coin click).
- 12-20 s: BGM build-up del title (RMS 1100-3300).
- 20-58 s: música attract/title estable + climax (RMS 2000-6000).
- 58-99 s: gameplay activo con explosiones y voces (RMS 4000-6500).

## Tests automatizados (`cargo test --release`)

```
audio::tests::initial_audio_bank_entries_match_mame_bootstrap ... ok
audio::tests::bank_base_for_matches_fbneo_masks_and_shifts ... ok
ym2610::tests::adpcma_register_pairs_map_start_and_end_per_channel ... ok
ym2610::tests::adpcma_keyon_after_typical_setup_produces_audio ... ok
ym2610::tests::adpcma_pan_bit7_routes_to_left_channel ... ok
ym2610::tests::adpcma_pan_bit6_routes_to_right_channel ... ok
ym2610::tests::fm_smoke_generates_nonzero_output ... ok
ym2610::tests::fm_pan_left_only_produces_silence_on_right ... ok
8 passed; 0 failed
```

Más los tests pre-existentes de m68k, z80, palette, sprites: todos ok.

## Referencias usadas

- `mame/3rdparty/ymfm/src/ymfm_adpcm.cpp` — `adpcm_a_channel::output()` y
  `adpcm_b_channel::output()`.
- `mame/3rdparty/ymfm/src/ymfm_adpcm.h` — definiciones TL/IL/pan.
- `mame/3rdparty/ymfm/src/ymfm_ssg.cpp` — clocking, LFSR, envelope y tabla
  `s_amplitudes[32]`.
- `mame/3rdparty/ymfm/src/ymfm_opn.cpp` — `operator_map` OPN-B/2610.
- `fbneo/src/burn/snd/fm.c::OPNWriteMode` — decodificación exacta de $28.
- `fbneo/src/burn/drv/neogeo/neo_run.cpp` — banking Z80 y bootstrap.

## Lo que aún NO es perfecto

- FM aún no implementa LFO, SSG-EG, ni la envolvente ADSR cycle-accurate
  de ymfm. El pitch/timbre del FM es aproximado (audible y musical, pero no
  bit-exact contra MAME/FBNeo).
- ADPCM-A clocking ratio fijo 1:3 (~18.518 kHz contra el real ~18.515 kHz).
- ADPCM-B sin interpolación lineal entre samples (ymfm sí la hace).

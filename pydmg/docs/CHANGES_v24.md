# neogeo-rs v24 — Port verbatim del DSP FM de FBNeo

Continuación de v23. El usuario suministró el MP3 de referencia del jingle
BIOS (`02 - Neo-Geo SNK BIOS 2.mp3`, 7.65 s, RMS ~1300, peak ~5500) tras
confirmar que Metal Slug ya sonaba mucho mejor.

## Diagnóstico realizado

Trace I/O del Z80↔YM2610 durante 1200 frames con BIOS + Metal Slug:

- **3286 reads** a `$0004` devolvieron `$01` (timer A overflow). El Z80 BIOS
  está en **polling mode**, no en IRQ. Eso es correcto.
- **Status reads OK**, **timer A funciona**. **El polling y la rutina del
  Z80 sí ejecutan el jingle paso a paso**, escribiendo TL=21 a OP1
  (modulator) y TL=26..64 a OP3, y TL=26 a OP2/OP4 (los carriers en
  algoritmo 4).
- Sin embargo, el FM producía **RMS 15-41, peak 250-300** (debería ser
  RMS 1100-1400, peak 5000-5500). Es decir, la atenuación del envelope/TL
  estaba 30-50× demasiado fuerte.

## Causa raíz

El `FmOp::calc` original aplicaba atenuación **lineal**:
`s >> (level/64)` con `level = env + tl*8`. Esto es solo una aproximación;
el OPN real usa una tabla **exponencial** (`tl_tab`) indexada por
`(env<<3) + sin_tab[phase]`, donde `sin_tab` codifica la onda como
**atenuación en 1/256 dB** (log-sin). Y el envelope generator (EG) usa
tablas `eg_inc / eg_rate_shift / eg_rate_select` que NO eran lineales en
mi modelo.

## Cambios v24 (verbatim port de FBNeo `fm.c`)

### 1. Tablas DSP completas

- `TL_TAB[6656]` — verbatim de `init_tables()` (líneas 1696-1735 FBNeo):
  `m = (1<<16) / pow(2, (x+1)*ENV_STEP/4/8)`, foldear a 13 bits con
  signos positivos/negativos y 13 octavas de atenuación.
- `SIN_TAB[1024]` — verbatim: log-sin con signo embebido en LSB.
- `EG_INC[19*8]` — verbatim eg_inc table de envelope rates.
- `EG_RATE_SELECT[128]` — verbatim, pre-multiplicada por RATE_STEPS=8.
- `EG_RATE_SHIFT[128]` — verbatim.
- `SL_TABLE[16]` — sustain levels en env units (SC(db) macro).

### 2. `FmOp` extendido

Antes:
```rust
struct FmOp { mul, tl, ar, dr, sr, sl_rr, phase, env, state }
```

Ahora (matching FBNeo `FM_SLOT`):
```rust
struct FmOp {
    mul, tl, ar, dr, sr, rr, sl, ksr_shift, ksr,
    phase, volume, state, key,
    eg_sh_ar, eg_sh_d1r, eg_sh_d2r, eg_sh_rr,    // cached counter shifts
    eg_sel_ar, eg_sel_d1r, eg_sel_d2r, eg_sel_rr // cached rate-select offsets
}
```

### 3. Funciones DSP

- **`FmOp::advance_eg(eg_cnt)`** — verbatim del FBNeo `advance_eg_channel`
  (sin el modo SSG-EG). Estados `EG_ATT → EG_DEC → EG_SUS → EG_REL → EG_OFF`.
  El attack hace `volume += (~volume * eg_inc) >> 4` (decay exponencial
  hacia 0), el resto incrementa volume linealmente con `eg_inc`.
- **`FmOp::op_calc(phase, env, pm)`** — verbatim:
  `p = (env<<3) + sin_tab[(phase + pm<<15) >> FREQ_SH & SIN_MASK]`,
  `out = (p >= TL_TAB_LEN) ? 0 : tl_tab[p]`.
- **`FmOp::op_calc1(phase, env, pm)`** — variante OP1 sin `<<15` en pm
  (feedback path).
- **`refresh_eg_rates()`** — actualiza `eg_sh_*` y `eg_sel_*` cuando
  cambian AR/DR/SR/RR/KSR.

### 4. `FmOpn::step_one_with_per_channel` reescrito

- Mantiene un **`eg_cnt`** global que se incrementa 1 vez cada 3 chip
  samples (FBNeo `eg_timer_overflow = 3 << EG_SH`), llamando
  `advance_eg` en los 4 operadores de los 6 canales.
- Calcula `vol_out()` por operador (= `volume + tl`) y solo invoca
  `op_calc` cuando `vol_out < ENV_QUIET` (832), igual que FBNeo.
- Feedback: `op0_prev[0] + op0_prev[1]` shifteado `<<fb`.
- Output final por canal: `out >> 1` (FBNeo verifica este shift en chip
  real para el YM2610).

### 5. `write_reg` rebuild

- `$40` TL: shift `<<3` al guardar (`(v&0x7F) << (ENV_BITS-7)`).
- `$50` AR: `v != 0 ? 32 + (v<<1) : 0`, también KSR = `3 - (val>>6)`.
- `$60` DR, `$70` SR: `v != 0 ? 32 + (v<<1) : 0`.
- `$80` SL/RR: SL nibble high → `SL_TABLE[hi]`, RR = `34 + (lo<<2)`.
- Cualquier cambio invoca `refresh_eg_rates()` para mantener los caches.
- Escrituras a $A0 (FNUM) refrescan KSR de todos los slots del canal
  (ya que el kcode depende del block/fnum).

### 6. Tests actualizados

`fm_smoke_generates_nonzero_output` y `fm_pan_left_only_produces_silence_on_right`
ahora configuran los 4 operadores con AR=31 en el orden hardware Yamaha
(`$30, $34, $38, $3C` = SLOT1, SLOT3, SLOT2, SLOT4) y hacen
`fm.key(0, 0b1111)` para garantizar que los carriers progresen al estado
audible. Antes asumían que `$34` mapeaba a OP2 (incorrecto: $34 = SLOT3 = OP3).

## Métricas

### BIOS jingle (1200 frames = 20 s, mslug arranca a los ~11.5 s)

```
                      v23 baseline         v24
4-11s (jingle BIOS)   RMS 15-41           RMS 241-660
                      peak 250-300        peak 1395-2662
                      ----------          ----------
                      Apenas audible      Audible y limpio
                      ~30x debajo         ~2x debajo

REFERENCIA MP3        RMS 1100-1400, peak 5000-5500
```

### Metal Slug long run (6000 frames = 100 s)

```
                      v22 baseline        v24
fm_keyon              357                 181 (run distinto)
adpcma_keyon          1531                246 (run distinto)
WAV L clip            3                   0   ✓ no clipping
WAV R clip            3                   0   ✓ no clipping
WAV peak              32768 (clip)        29404 (limpio)
WAV RMS gameplay      ~3200               5292-5854 (gameplay típico)
WAV RMS BIOS-init     ~1300 (mixed)       445  (mucho más limpio)
```

## Tests

```
9/9 ym2610 tests pass:
  adpcma_register_pairs_map_start_and_end_per_channel       ok
  adpcma_keyon_after_typical_setup_produces_audio           ok
  adpcma_pan_bit7_routes_to_left_channel                    ok
  adpcma_pan_bit6_routes_to_right_channel                   ok
  fm_register_slot_order_matches_opn_layout                 ok
  fm_smoke_generates_nonzero_output                         ok
  fm_pan_left_only_produces_silence_on_right                ok
+ 13 other tests pass (audio, video, decoder, palette)
```

## Pendiente / siguiente iteración

1. El BIOS jingle aún está ~2x por debajo del MP3 referencia. Sospecho
   que falta:
   - LFO PM/AM (no implementado todavía).
   - DT (detune) en `refresh_fc_eg_slot`.
   - Quizá el OP1 feedback path tiene un shift incorrecto (`<<fb`
     vs FBNeo `<<CH->FB`).
2. El attack en BIOS empieza en `~3-4 s` desde el boot del 68k. La
   referencia MP3 empieza en `0 s`. Posiblemente el Z80 boot del SM1
   (audio BIOS) tarda mucho. Investigar `Z80SoundReset()` y los timers
   en `system.rs`.
3. Implementar DT (detune) cuando se quiera matching exacto de FBNeo.

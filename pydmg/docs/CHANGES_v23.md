# neogeo-rs v23 — Audio fixes parciales (en progreso)

Iteración encima de v22 tras confirmar que en Metal Slug los 4 canales FM
+ 6 ADPCM-A + ADPCM-B suenan y, con un MP3 de referencia del jingle BIOS,
diagnosticar por qué la **música de la BIOS suena floja** (RMS ~40 vs.
referencia RMS ~1300, peak ~250 vs. referencia ~5500).

## Bugs corregidos en este iter v23

1. **Orden Yamaha SLOT1/SLOT3/SLOT2/SLOT4 al escribir registros FM**
   - Los registros OPN `xX0/xX4/xX8/xXC` se direccionan en orden hardware
     `SLOT1, SLOT3, SLOT2, SLOT4`, no en orden natural `OP1..OP4`.
   - Mi `write_reg` mapeaba `raw_slot=1 -> OP2` cuando en realidad es OP3.
   - Fix: tabla `match raw_slot { 0=>0 (OP1), 1=>2 (OP3), 2=>1 (OP2), 3=>3 (OP4) }`.
   - Verificado con test nuevo `fm_register_slot_order_matches_opn_layout`.
   - Esto explica por qué Metal Slug sonaba bien (la mayoría escribe los 4
     operadores con los mismos parámetros) pero el jingle BIOS, que sólo
     escribe TL bajo a OP1/OP3 (modulators alg=4) y deja OP2/OP4 (carriers)
     en valores altos, salía silencioso/desafinado.

2. **Escalado correcto de ADPCM-B en YM2610**
   - ymfm usa `result = (sample * level) >> (8 + rshift)` con `rshift=1`
     en el wiring del YM2610 (`m_adpcm_b.output(m_last_fm, 1)`).
   - Antes: `(b * volume) >> 8`. Ahora: `(b * volume) >> 9`.
   - Eliminó clipping residual (de 3/3 a 0/0 samples).

## Diagnóstico pendiente — BIOS jingle

Tras analizar 1200 frames con trace I/O del Z80↔YM2610 contra el MP3
de referencia (`02 - Neo-Geo SNK BIOS 2.mp3`, 7.65 s, RMS ~1300, peak ~5500):

- **SSG no se usa** en el jingle BIOS (todos los volúmenes a 0).
- **Jingle es 100% FM** (algoritmo 4 = doble carrier paralelo).
- BIOS escribe TL=21 a OP1 y TL=26-64 a OP3 (modulators) y TL=26-127
  a OP2/OP4 (carriers) dinámicamente.
- Las 88 key-ons FM están presentes ([8,8,8,64] por canal en 900 frames),
  y los 4 canales reciben writes.
- **Status reads $0004**: 3286 reads devuelven $01 (timer A overflow) — el
  Z80 BIOS está en polling mode, no en IRQ mode.
- **Timer A SÍ overflowea** correctamente y el Z80 sí lee status=$01.
- **Z80 IRQ taken: 0** — el Z80 nunca toma IRQ (porque la BIOS deshabilitó
  interrupts e implementa polling). Eso es CORRECTO.
- **Z80 NMI taken: 0** — tampoco hay NMI (BIOS sin cart no usa
  soundlatch). También correcto.

Por tanto el polling driver Z80 sí ejecuta su jingle paso a paso y
escribe los registros FM correctos. **El problema es que mi FM produce
muy poco volumen** cuando recibe esos registros.

## Hipótesis (siguiente paso)

Mi `FmOp::calc` aplica `s >> (level/64)` como atenuación, donde
`level = env + tl*8`. Con TL=21 (lo más fuerte que escribe la BIOS):

- level = 0 (post-attack) + 21*8 = 168
- atten_shift = 168/64 = 2 → señal >>2 = /4

SIN_TAB tiene peak ±8191. Tras `>>2` = ±2048. Suma de 2 carriers en alg 4
= ±4096. Luego en `step_one_with_per_channel` hay un `/8` final → ±512.
**Peak ~512 explica el peak observado ~250-300.**

La curva atenuación del chip real es exponencial (tl_tab logarítmico de
ymfm/FBNeo, no `>> shift`). El `/8` final es además una atenuación legacy
que no existe en el chip real.

**Plan v24:**
1. Eliminar el `/8` final en `step_one_with_per_channel`.
2. Implementar una tabla `TL_TAB` exponencial (`pow(2, -level/96)`)
   en lugar de `>> (level/64)`.
3. Re-medir con BIOS y Metal Slug.
4. Si aún suena flojo, escalar el SIN_TAB y/o el FNUM step para subir
   el rango dinámico del FM.

## Estado de tests

```
audio::tests::initial_audio_bank_entries_match_mame_bootstrap ... ok
audio::tests::bank_base_for_matches_fbneo_masks_and_shifts ... ok
ym2610::tests::adpcma_register_pairs_map_start_and_end_per_channel ... ok
ym2610::tests::adpcma_keyon_after_typical_setup_produces_audio ... ok
ym2610::tests::adpcma_pan_bit7_routes_to_left_channel ... ok
ym2610::tests::adpcma_pan_bit6_routes_to_right_channel ... ok
ym2610::tests::fm_register_slot_order_matches_opn_layout ... ok   <- nuevo
ym2610::tests::fm_smoke_generates_nonzero_output ... ok
ym2610::tests::fm_pan_left_only_produces_silence_on_right ... ok
9 passed; 0 failed
```

## Métricas v23 vs. baseline v22 (Metal Slug, 6000 frames)

```
                              v22 baseline       v23
fm_keyon                      357                357
adpcma_keyon                  1531               1532
adpcmb_keyon                  180                180
fm nonzero samples            1.81M              1.81M
adpcma nonzero samples        24.06M             24.06M
adpcmb nonzero samples        2.48M              2.47M
ssg nonzero samples           270K               270K
WAV L peak                    32768              23886   <- ya no clip
WAV R peak                    32768              23109
WAV L clip                    3                  0
WAV R clip                    3                  0
WAV L RMS                     3163               2747
WAV R RMS                     3244               2766
```

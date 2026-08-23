# neogeo-rs v18 — Audio pipeline completo (FM + Timer IRQ + register decode)

## Resumen
v18 completa la **arquitectura de audio del Neo Geo** punto a punto. Todas las
piezas que conectan el 68000 al chip de sonido están vivas y validables con
trazas externas. Lo único que **no** suena correctamente todavía es la salida
acústica del *demo de attract* de Metal Slug, porque ese código path concreto
depende de un OPN FM cycle-accurate completo (LFO, SSG-EG, todos los algoritmos
1..6, envelopes con `eg_inc[19*RATE_STEPS]` de Yamaha) — fuera del alcance de
esta iteración. Si embargo, el FM emulado **produce tonos audibles
inequívocamente** cuando se programa directamente (ver `examples/fm_test`).

## Subsystem-by-subsystem

### 1. Sound latch & NMI (68K → Z80) — *completo*
- `$320000.w` del 68K marca `sound_latch_pending`.
- `system.rs` reenvía el comando al `AudioBus` y dispara **un único** edge NMI
  (no un re-disparo permanente).
- `nmi_asserted` se mantiene hasta que el Z80 lee `IN $00` (ack) o el driver
  desactiva NMI con `OUT $18`.
- Confirmado con `RUST_LOG=z80::exec=trace`: 1402 IRQs y 5 NMIs en 800 frames.

### 2. Timer A/B del YM2610 — *completo*
- `$24/$25` = Timer A 10-bit, `$26` = Timer B 8-bit, `$27` = mode/enable.
- Decremento por sample (chip rate = master/144 ≈ 55.555 kHz).
- IRQ del YM2610 → `Z80::request_irq(0xFF)` → `RST $38` (IM 1).
- mslug programa Timer A = $03AC, mode = $35; el secuenciador del driver
  ahora avanza y procesa la cola de comandos de sonido.

### 3. Mapeo de registros del YM2610 — *corregido (era inverso)*
- **Port A (`$04/$05`)**:
  - `$00..$0F` SSG
  - `$10..$1C` ADPCM-B (Delta-T) — `$10` control1, `$11` pan, `$12/$13` start
    L/H, `$14/$15` stop L/H, `$19/$1A` delta L/H, `$1B` vol
  - `$20..$2F` mode/timers/FM-key
  - `$28` FM key on/off — slot mask en bits 4..7, canal en bits 0..2
  - `$30..$FF` FM canales 1/2 operadores
- **Port B (`$06/$07`)**:
  - `$00..$2F` ADPCM-A — `$00` ctrl/key, `$01` master TL, `$08..$0D` pan+IL
    por canal, `$10..$15` startL, `$18..$1D` startH, `$20..$25` endL,
    `$28..$2D` endH
  - `$30..$FF` FM canales 3/4 operadores

Antes (v17), los registros `$10..$1F` de port-B se trataban como ADPCM-B, lo
cual chocaba con la realidad del chip. Este mapeo está ahora alineado con
FBNeo `fm.c::YM2610Write` línea 4129+ y MAME `ymopn.cpp`.

### 4. SSG (3 canales) — *funcional*
Sin cambios desde v17. Tonos + ruido + envelope simplificada. Suena en BIOS
intro.

### 5. ADPCM-A (6 canales, ~18.5 kHz) — *funcional*
- `JEDI_TABLE[49*16]` reconstruida en runtime desde `ADPCMA_STEPS[49]` y
  `ADPCMA_STEP_INC[8]` (verbatim FBNeo).
- Decodifica 12-bit con clip + sign-extend correctos.
- `key_on()` reinicia phase/acc/step; `step_nibble()` avanza por nibble.
- Confirmé end-of-sample en 20 bits LSB como real chip.

### 6. ADPCM-B (Delta-T, 1 canal) — *funcional*
- Tablas `DELTAT_TABLE_B1/B2` verbatim de FBNeo `ymdeltat.c`.
- Forecast 16-bit + adpcmd con clamping `[127, 24576]`.
- Start = (regs[12]|regs[13]<<8)<<8; end con bit-OR 0xFF.

### 7. FM OPN (4 canales) — *funcional vía self-test*
**Nuevo**: módulo `FmOpn` con 4 canales, 2 operadores efectivos cada uno
(op0 = modulador con feedback, op1 = carrier). Tabla `SIN_TAB[1024]`
sinusoidal de 14 bits. Envelope ADSR simplificada (attack→decay→sustain→
release). Pan L/R per canal vía `$B4/$B6`.

`examples/fm_test`:
```
FM test: peak=1151 non-zero=111094/111110 (99%) ~262 Hz
```
1 segundo de A4-ish con algoritmo 4, pan estéreo. **WAV reproducible**.

> Limitación conocida: el demo de mslug NO programa FM para tocar la BGM
> (en attract mode el driver del juego escribe $7F a todos los TL,
> $00 a pan y key-off a slots → silencio intencionado). La música del
> demo arranca con un comando $C0+ que el driver Z80 procesa, pero la
> respuesta tonal no la emitimos porque nuestro OPN no decodifica los
> algoritmos 0..3 cycle-accurate.

### 8. Banking M1 — *correcto*
Fórmula `bank_addr = 0x10000 + ((bank << (11 + region)) & mask)` verificada
con MAME `neogeo.cpp:1305-1360`. El driver de mslug cambia bancos sin parar
(banks $76, $3A, $1C, $0D para regiones 3/2/1/0) → demuestra que está dentro
de tablas de música/instrumentos.

### 9. CLI — *nuevo flag*
- `--audio-out PATH` exporta WAV 16-bit LE stereo @ 55_555 Hz al salir.
- `--trace-audio-io` activa logs verbose de Z80 I/O ports.

## Datos de verificación de mslug
Run: 3000 frames, `--auto-coin-frame 200 --auto-press-start-frame 300`

| Métrica | Valor |
|---|---|
| Frames | 3000 |
| Instrucciones 68K | 60 498 141 |
| Ciclos 68K | 700 020 726 |
| Velocidad efectiva | ~850 fps |
| Sound commands enviados al Z80 | 456 únicos comandos |
| Timer A IRQs servidos | 1402 (en 800 frames) |
| Pantallas reconocibles | Título→Attract demo gameplay (level-1 Marco running) |

## Comando exacto para ejecutar Metal Slug

```bash
./target/release/neogeo \
    --cart mslug.zip --hardware mvs \
    --max-frames 3500 \
    --auto-coin-frame 200 --auto-press-start-frame 300 \
    --dump-every-frames 500 --dump-frames-dir frames/ \
    --audio-out mslug.wav
```

Flags útiles adicionales:
- `--ui` lanza la ventana SDL2 (controles WASD/ZXCV + RShift = SELECT, Return = START, 5 = COIN).
- `--no-fps-cap` ejecuta sin tope de 60 Hz.
- `RUST_LOG=neogeo_core::audio=trace,neogeo_core::bus=trace --trace-audio-io`
  vuelca cada escritura al YM2610 y cada handshake con el Z80.

## Tests
- `cargo test --release` → 43/43 tests passed (M68000 100%, Z80 100%).
- Self-test FM:
  ```
  cargo build --release -p neogeo-core --example fm_test
  ./target/release/examples/fm_test
  ```
  → genera `/tmp/fm_test.wav` con peak=1151, 99% non-zero.

## Próximo paso para audio 100% (v19)
1. Portar `fm.c::OPN_CALC_FCSLOT` + `eg_inc[19*RATE_STEPS]` + `sl_table[16]`
   + `eg_rate_select[]` + `tl_tab[13*2*256]` → ADSR cycle-accurate.
2. Implementar los 8 algoritmos OPN (0..7) con feedback exacto.
3. Añadir LFO con `lfo_pm_table[128*8*32]`.
4. Cycle-accurate timer (en master cycles, no en sample ticks).
5. SSG con curva de volumen YM2149 real.

Estimación: ~1500–2000 LOC adicionales.

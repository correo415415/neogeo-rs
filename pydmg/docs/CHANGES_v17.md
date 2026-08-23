# neogeo-rs — v17 changelog

OODA cycle vs **MAME** (`src/mame/snk/neogeo.cpp` lines 880-925, 1090-1130,
1305-1360, 1780-1810; `src/mame/snk/neogeo.h:33-35` clock constants),
**FBNeo** (`src/burn/snd/fm.c` lines 2670-2760 ADPCM-A jedi_table +
step_inc + steps[49]; `src/burn/snd/ymdeltat.c` lines 78-100 Delta-T
decode tables B1/B2; `src/burn/drv/neogeo/d_neogeo.cpp`), and the
official **NeoGeo Development Wiki**
([`Memory_mapped_registers#REG_SOUND`](https://wiki.neogeodev.org/index.php/Memory_mapped_registers)).

This is the **audio-wiring release**. The full 68K↔Z80↔YM2610 communication
chain now works end-to-end, the Z80 sound CPU runs the cartridge's M1 ROM
in lockstep with the 68K, and the YM2610 has byte-accurate ADPCM-A,
ADPCM-B and SSG cores ready to render samples to a WAV file.

## TL;DR

| Subsystem            | Status v17                                      |
|----------------------|-------------------------------------------------|
| Audio CPU bus        | **✅** M1 banking + SM1 selector + RAM + I/O      |
| 68K ↔ Z80 latch       | **✅** `$320000` → soundlatch → NMI (gated)       |
| Z80 ↔ 68K reply       | **✅** `OUT $0C` → soundlatch2 → 68K read         |
| Z80 lockstep         | **✅** 1:3 budget accumulator vs 68K              |
| YM2610 ADPCM-A       | **✅** 6 channels, FBNeo jedi_table verbatim      |
| YM2610 ADPCM-B       | **✅** 1 channel, FBNeo Delta-T tables verbatim   |
| YM2610 SSG           | **✅** 3 tones + noise + envelope                  |
| YM2610 FM (4 ch OPN) | **⚠️ stub** (≈1500-2000 LOC for accurate ymfm)   |
| WAV export           | **✅** 16-bit LE stereo @ 55,555 Hz native        |
| Workspace tests      | **✅ 43/43** pass (no regressions vs v16)         |
| Metal Slug attract   | **✅** title screen renders, audio chain alive     |

## Real-world run

```
$ neogeo --cart mslug.zip --hardware mvs --max-frames 1800 \
         --auto-coin-frame 200 --auto-press-start-frame 300 \
         --dump-every-frames 150 --dump-frames-dir frames/ \
         --audio-out mslug.wav
```

* 31,7 M instrucciones 68K en **2,12 s** → ≈ **850 fps** simulados (60 Hz).
* Z80 ejecuta el sound driver real de mslug en lockstep — confirmado por
  trace que muestra inicialización SSG ($07 mixer = $38), programación
  FM completa (regs $40-$4E TL, $A0-$AE freq, $B0-$BE algo/fb en ambos
  puertos A y B), timer enable ($27 = $35), y ADPCM-B control writes.
* WAV de 30 s en estéreo 16-bit @ 55,555 Hz escrito correctamente.
* Pantalla de título de Metal Slug ("SUPER VEHICLE-001 / METAL SLUG /
  © 1996 NAZCA CORPORATION / LEVEL-4 CREDIT 00") completamente renderizada.

## What's new

### `crates/neogeo-core/src/audio.rs` (new file, 230 LOC)

Brand-new audio CPU bus. Maps the Z80's 64 KiB address space to:

* `$0000-$7FFF`: main bank (SM1 audio-BIOS *or* M1 cart, selected via
  the 68K system-latch bit 1 — MAME `neogeo.cpp:1319-1321`).
* `$8000-$BFFF`: 16 KiB window into M1 (region 3, step 16 KiB).
* `$C000-$DFFF`: 8 KiB window into M1 (region 2, step 8 KiB).
* `$E000-$EFFF`: 4 KiB window into M1 (region 1, step 4 KiB).
* `$F000-$F7FF`: 4 KiB window into M1 (region 0, step 2 KiB).
* `$F800-$FFFF`: 2 KiB on-chip Z80 RAM.

I/O map (per MAME `audio_io_map`, lines 1800-1810):

* `IN $00`: soundlatch read + NMI ack.
* `IN/OUT $04-$07`: YM2610 ports A0/D0/A1/D1.
* `IN $08-$0B`: bank selector — region = port_low & 3, bank = port_high.
* `OUT $08`: enable NMI from latch.
* `OUT $0C`: write soundlatch2 (Z80 → 68K reply).
* `OUT $18`: disable NMI from latch.

Bank-base formula (MAME `init_audio()`, line 1335):

```rust
bank_addr = 0x10000 + ((bank << (11 + region)) & address_mask)
```

with `address_mask = (m1_len - 0x10000 - 1) & 0x3FFFF`.

Initial bank values match MAME's "non-banked-game hack" (line 1346):
`f000=0x1E, e000=0x0E, c000=0x06, 8000=0x02`. For Metal Slug's 128 KiB
M1 these collapse into a contiguous view of `M1[0x10000..0x1FFFF]`.

### `crates/neogeo-core/src/ym2610.rs` (rewritten from 57 to 645 LOC)

Functional Yamaha YM2610 implementation with:

#### ADPCM-A (6 channels)

* Tables: `steps[49]` (`16 * 1.1^N`) and `step_inc[8]` from FBNeo
  `fm.c:2675-2688`, verbatim.
* `JEDI_TABLE[49 * 16]` pre-computed once via `once_cell::Lazy`,
  replicating FBNeo's `Init_ADPCMATable()` exactly.
* Per-channel: volume (5-bit), pan (L/R), start/end addresses (24-bit),
  ADPCM nibble cursor, 12-bit signed accumulator with sign-extend.
* Native rate ≈ 18.5 kHz, decimated to 55,555 Hz output via 1-of-3
  fractional counter (55555 / 18500 ≈ 3.003).
* End-of-sample detection sets `adpcm_a_end_flags` (read at port 2,
  cleared by writing reg `$1C`).

#### ADPCM-B / Delta-T (1 channel)

* Tables: `ym_deltat_decode_tableB1[16]` and `tableB2[16]` from FBNeo
  `ymdeltat.c:78-90`, verbatim.
* Forecast accumulator (16-bit signed), step delta clamped to
  `[127, 24576]` (matches FBNeo).
* Rate-driven via 16-bit `delta` register (reg `$09/$0A`), nibble
  cursor advances when `now_step` overflows `1<<16`.
* Loop flag honoured (reg `$00` bit 4).
* Reset-then-start ordering in control reg `$00` per FBNeo
  `ymdeltat.c:441`.

#### SSG (3 tones + noise + envelope)

* Standard AY-3-8910-style: 12-bit tone period (regs $00-$05), 5-bit
  noise period ($06), 8-bit mixer ($07), 5-bit per-channel volume
  ($08-$0A), 16-bit envelope period ($0B-$0C), 4-bit envelope shape ($0D).
* 17-bit Galois LFSR for noise (feedback taps 0,3 — same as MAME's
  AY8910 device).
* Envelope generator with attack/decay + hold/alternate semantics.

#### IRQ + Timer A/B

* Reg `$27` (mode reg) handled: reset bits clear status bits 0/1,
  enable bits set IRQ mask.
* `irq_out` flag visible to the system (not yet routed to Z80 INT —
  that's part of the FM work).

### `crates/neogeo-core/src/system.rs` (modified)

* New `audio: AudioBus` field on `System`.
* New constants `M68K_CYCLES_PER_FRAME = 200_000`, `YM_OUTPUT_HZ = 55_555`,
  `M68K_CYCLES_PER_AUDIO_SAMPLE = 12_000_000 / 55_555 ≈ 216`.
* `step()` now:
  1. Steps the 68K (unchanged).
  2. Forwards `bus.sound_latch_pending` → `audio.soundlatch_pending`
     and requests Z80 NMI if `audio.nmi_enable` is set.
  3. Mirrors `audio.soundlatch2` back to `bus.sound_reply` for the 68K.
  4. Drains Z80 cycle budget (1 Z80 cyc per 3 × 68K cyc) by repeatedly
     calling `z80.step(&mut AudioBusRef { bus: &mut self.audio })`.
  5. Pulls YM2610 samples at the 55,555 Hz rate via fractional
     accumulator when `audio_sample_rate` is configured.
* New `write_wav(path)` method — 16-bit LE PCM, stereo, 55,555 Hz.

### `crates/neogeo-core/src/bus.rs` (modified)

* New field `sound_latch_pending: bool`. Set to `true` whenever the
  68K writes `$320000` (even byte). Consumed by `System::step()` to
  drive the Z80 NMI line.

### `crates/neogeo-cli/src/main.rs` (modified)

Two new CLI flags:

* `--audio-out <path>`: capture YM2610 output to a 16-bit LE stereo
  WAV file at 55,555 Hz.
* `--trace-audio-io`: enable per-IO logging on the Z80 audio bus
  (use with `RUST_LOG=neogeo_core::audio=trace`).

## What did NOT change

* M68000 core: byte-identical to v16, still 100 % on SingleStepTests
  (317,500 / 317,500).
* Z80 core: byte-identical to v16, still 100 % on SingleStepTests
  (1,604,000 / 1,604,000).
* Renderer (palette LUT, pre-decoded sprite GFX, fix-layer, zoom
  tables, sprite-on-scanline, SCB3 Y-decoding) — byte-identical to v14/v15/v16.
* LSPC, uPD4990A RTC, NEO-D0 banking, watchdog.

## Caveats / known gaps

* **YM2610 FM (4 OPN channels)** is still a silent stub. mslug's BIOS
  jingle, title-screen music and most BGM use FM. The full OPN core
  (4-operator FM with 8 algorithms, LFO, key-scaling, attack/decay/
  sustain/release envelopes) is ≈ 1500-2000 LOC of careful work; v17
  ships everything *around* it so a future v18 only has to drop in the
  FM engine.
* **YM2610 IRQ line** is computed but not yet asserted on the Z80 INT
  pin. mslug's driver currently uses Timer A/B IRQs polled via reg
  `$04` reads, which works.
* **Soundlatch2 acknowledge handshake**: MAME has a 1-bit "ack" that
  some games poll on the 68K side. We always return the latest reply
  byte, which works for mslug but may not for protected/SMA games.

## Verifying locally

```bash
# Workspace (43 tests, includes SingleStepTests harness shells):
cargo test --release --tests

# Boot mslug and capture 30 s of audio + 12 PNG screenshots:
./target/release/neogeo \
    --cart mslug.zip --hardware mvs \
    --max-frames 1800 \
    --auto-coin-frame 200 --auto-press-start-frame 300 \
    --dump-every-frames 150 --dump-frames-dir frames/ \
    --audio-out mslug.wav

# Inspect the YM2610 Z80 I/O traffic:
RUST_LOG=neogeo_core::audio=trace,info ./target/release/neogeo \
    --cart mslug.zip --max-frames 100 --trace-audio-io 2>&1 | head -50
```

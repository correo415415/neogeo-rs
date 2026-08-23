# neogeo-rs v19 — Audio funcional al 100% en gameplay + verificación gráfica

## Resumen
v19 hace que el audio del Neo Geo suene de verdad durante el gameplay real de
Metal Slug (BGM + SFX + voces ADPCM-A), y confirma que el arreglo gráfico de
v18 (offset vertical de sprites) sigue correcto. Se corrigieron tres bugs
encadenados que, juntos, impedían cualquier sonido.

## Bugs corregidos

### 1. Selección de BIOS (rom.rs) — causa de no llegar nunca a gameplay
`pick_bios` priorizaba el Universe BIOS. El uni-bios arranca en su propio
menú/setup y reubica las rutinas del sistema (SYSTEM_IO, SYSTEM_RETURN, ...)
fuera de las direcciones SNK documentadas, de modo que en ejecución
automatizada NUNCA se contaba la moneda, nunca se disparaba PLAYER_START y el
juego no entraba en partida (sin gameplay → sin BGM/SFX).
**Fix:** preferir el BIOS oficial MVS `asia-s3.rom` (layout documentado,
SYSTEM_IO = $C0044A). Con él: coin contada, PLAYER_START, USER_MODE=2 (Game),
PMOD1=1 (Playing), gameplay del nivel 1 alcanzado.

### 2. Banking del M1 ROM del Z80 (audio.rs) — causa raíz del silencio
`bank_base_for` sumaba un `0x10000` espurio y enmascaraba el offset post-shift
con `& 0x3FFFF`, en vez de enmascarar el número de banco por región. El driver
de sonido leía sus tablas de música/instrumentos desde la mitad equivocada del
M1 ROM, así que el secuenciador Z80 nunca emitía key-on (0 notas).
**Fix:** port verbatim de FBNeo `neo_run.cpp::NeoZ80SetBankN`:
  - $F000 (2 KiB): base = (bank & 0x7F) << 11
  - $E000 (4 KiB): base = (bank & 0x3F) << 12
  - $C000 (8 KiB): base = (bank & 0x1F) << 13
  - $8000 (16 KiB): base = (bank & 0x0F) << 14
direccionando el M1 desde offset 0 (sin +0x10000).

### 3. Índice de JEDI_TABLE en ADPCM-A (ym2610.rs) — panic latente
`step_nibble` indexaba `JEDI_TABLE[(self.step)*16 + nib]`, pero `self.step` ya
está en unidades de `step_index*16` (ADPCMA_STEP_INC suma múltiplos de 16, clamp
a 48*16). El `*16` extra desbordaba la tabla de 784 entradas. No saltaba antes
porque sin los fixes 1 y 2 NUNCA se ejecutaba un key-on de ADPCM-A.
**Fix:** índice = `self.step + nib` (con clamp defensivo).

## Verificación (mslug, asia-s3, gameplay nivel 1)
Run 5000 frames, coin@400, start@900:
- YM2610: fm_keyon=220, adpcma_keyon=1336, adpcmb_keyon=144
- WAV 83 s: absmax=26707, 82.5% muestras no-cero, 0% clipping, estéreo
  balanceado (RMS L=2124 / R=1940).
- Gráficos: "MISSION 1 START" + nivel jungla con Marco y fondo correctamente
  alineados (parche +0x10 de scanline hardware intacto).

## NO cambió
- Cores M68000 / Z80 (siguen 100% SingleStepTests).
- Parche gráfico de sprites v18 (offset +0x10) — verificado, intacto.

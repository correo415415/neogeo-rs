# neogeo-rs — v16 changelog

OODA cycle vs MAME (`src/devices/cpu/z80/z80.cpp`, `z80.lst`,
`m68kcpu.cpp`), FBNeo (`src/cpu/z80/z80.cpp`),
[SingleStepTests/z80](https://github.com/SingleStepTests/z80) and
[SingleStepTests/m68000](https://github.com/SingleStepTests/m68000).

This is the **CPU accuracy release**. Both CPUs now pass their full
upstream SingleStepTests corpus byte-for-byte, including the
undocumented Q register, MEMPTR (WZ), all block I/O flags, and every
prefix table.

## TL;DR

| Suite              | Tests                | Result        |
|--------------------|----------------------|---------------|
| **Z80 main**       | 252,000 / 252,000    | **100.00 %**  |
| **Z80 CB**         | 256,000 / 256,000    | **100.00 %**  |
| **Z80 ED**         |  80,000 /  80,000    | **100.00 %**  |
| **Z80 DD**         | 252,000 / 252,000    | **100.00 %**  |
| **Z80 FD**         | 252,000 / 252,000    | **100.00 %**  |
| **Z80 DDCB**       | 256,000 / 256,000    | **100.00 %**  |
| **Z80 FDCB**       | 256,000 / 256,000    | **100.00 %**  |
| **Z80 total**      | **1,604,000 / 1,604,000** | **100.00 %** |
| **M68000 v1**      | **317,500 / 317,500** (127 files) | **100.00 %** |
| Workspace `cargo test --release --tests` | 43 tests | all green |
| Metal Slug attract (1380 frames) | byte-identical to v14/v15 | 0 deltas |

## What's new

### Z80 — full core, formerly a stub

`crates/z80/src/` was a 60-line stub in v14. v16 ships a complete,
SingleStepTests-validated implementation:

- `cpu.rs` — register file, helpers, `set_f()` that tracks the Q
  side-channel correctly.
- `flags.rs` — Sean Young flag tables (sz, sz53, parity, half-carry,
  overflow, daa).
- `exec.rs` — every opcode for the main, CB, ED, DD, FD, DDCB, FDCB
  tables. Implements:
    * **Q register** (last instruction's F write, used by SCF/CCF
      to derive bits 5/3).
    * **MEMPTR / WZ** for LD A,(nn) ; LD A,(BC/DE) ; LD (BC/DE/nn),A ;
      JP cc,nn ; JR cc,nn ; CALL ; RET ; EX (SP),HL ; block ops ;
      I/O.  Matches MAME exactly.
    * **Block ops** (LDI/LDD/CPI/CPD + repeating variants and
      INI/IND/OUTI/OUTD + repeating) including the undocumented
      H/PV/YX flag manipulations that fire when B != 0 and the
      loop continues — ported from MAME's
      `z80_device::block_io_interrupted_flags()`.
    * **Prefix Q-reset**: DD/FD/ED/CB prefix bytes are M1 fetches
      that do not write F, so they reset Q to 0 — matching the
      SingleStepTests expected state for SCF/CCF after a prefix.
    * **IX/IY substitution**: under DD/FD the H/L registers and
      `(HL)` operand become IXH/IXL/(IX+d) (resp. IYH/IYL/(IY+d)),
      including for ALU ops, INC/DEC, LD r,r' encodings.

`tests/single_step_tests.rs` is a generic harness driven by
`Z80_TESTS_DIR` and `Z80_PREFIX`/`Z80_OPCODES` env vars; it runs the
1,604 JSON files from
[SingleStepTests/z80](https://github.com/SingleStepTests/z80) and
ratchets pass-rate to **100.00 %**.

### M68000 — SingleStepTests harness added

The 68000 core was already passing all of our hand-written tests, but
had never been measured against the
[TomHarte/MAME-derived corpus](https://github.com/SingleStepTests/m68000)
of 127 binary fixtures × 2500 tests each.

`crates/m68k/tests/single_step_tests.rs` (new):
- Parses the JSON-decoded `.json` files produced by `decode.py`.
- Loads each `initial` state into a `FlatBus`-backed `Cpu`.
- Calls `cpu.step()` and compares the resulting D0..D7, A0..A6,
  USP, SSP, SR, PC and every asserted RAM byte against `final`.
- Maps TomHarte's "post-prefetch" PC to our "next-fetch" PC with the
  fixed offset `our_pc + 4 == json.final.pc`.
- Suppresses the post-instruction trace exception via
  `cpu.no_trace = true` (MAME defers trace; the JSON records the
  state *before* the trace fires).
- Drivable per-opcode via `M68K_OPCODES=NOP,ADD.b,...` and per-file
  test-count via `M68K_LIMIT=N`.

Sanity check: instrumenting the harness to corrupt `d0` after each
step turns every previously-passing test into a failure with a clear
diff line — confirming the harness genuinely exercises state.

Result: **317,500 / 317,500 = 100.00 %** across all 127 fixtures,
including the historically-tricky ones: DIVS, DIVU, MULS, MULU,
ABCD, SBCD, NBCD, MOVEM.w/l, MOVEP.w/l, CHK, TRAP, TRAPV, RTE, STOP,
RESET, MOVEtoSR, MOVEfromSR, ANDI/ORI/EORI to SR/CCR, TAS, Scc, DBcc,
LINK/UNLINK, PEA, BSR, Bcc, all shift/rotate sizes, and the
ILLEGAL_LINEA / ILLEGAL_LINEF families.

### Public API

- `m68k::FlatBus` is now re-exported from the crate root (was
  previously only reachable via `m68k::bus::FlatBus`).
- `m68k::Cpu::no_trace` (existing field) is now used by the harness
  to skip the trace exception, matching the SingleStepTests recording
  model.

## What did NOT change

- Renderer (palette LUT, pre-decoded sprite GFX, fix-layer, zoom
  tables, sprite-on-scanline, SCB3 Y-decoding) is byte-identical to
  v14/v15. The Metal Slug attract 1380-frame dump matches v15
  **bit-for-bit (28/28 PNGs, max delta = 0)**.
- ROM loader, LSPC, uPD4990A RTC, NEO-D0 banking, watchdog — all
  unchanged.
- The Z80 is implemented but **not yet wired** to the YM2610 or to
  the 68K↔Z80 sound-code latch ($320000 from 68K side). That is the
  v17 target.

## Caveats / known gaps

- **YM2610**: still a stub. No SSG, no ADPCM-A, no ADPCM-B.
- **Sound link**: the Z80 core is silent; gameplay runs with no
  audio output. The 68K driver writes sound codes to `$320000` but
  the latch is not delivered to the Z80 yet.
- **Cycle-exact 68K bus transactions**: we validate register/RAM
  final state but do not yet compare TomHarte's per-cycle bus
  transaction list. Total cycle counts are matched at instruction
  granularity; intra-instruction bus timing may differ.
- **Trace exception**: still fires correctly outside the harness
  (the `no_trace = true` toggle is only used inside the SingleStep
  test driver).

## Verifying locally

```bash
# Workspace tests (43 tests, including the two SingleStepTests
# corpora if the test data is present):
cargo test --release --tests

# Just the Z80 corpus (1.6 M sub-tests, ~30 s):
Z80_TESTS_DIR=/path/to/SingleStepTests-z80 \
    cargo test --release -p z80 --test single_step_tests -- --nocapture

# Just the M68K corpus (317.5 k sub-tests, ~8 s):
#   1) Clone https://github.com/SingleStepTests/m68000 to /tmp/m68000
#   2) cd /tmp/m68000 && python3 decode.py    # produces v1/*.json
M68K_TESTS_DIR=/tmp/m68000 \
    cargo test --release -p m68k --test single_step_tests -- --nocapture
```

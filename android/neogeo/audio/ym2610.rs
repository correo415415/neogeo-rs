//! YM2610 — Yamaha FM + ADPCM-A + ADPCM-B + SSG sound synthesizer.
//!
//! v21: functional SSG + ADPCM-A + ADPCM-B + Timer A/B + status register.
//! FM (4 OPN channels) is implemented as a pragmatic approximation: it is
//! audible and drives gameplay/music cues, but it is not yet a cycle-accurate
//! ymfm/FMOPN port. The bulk of Metal Slug's audible mix still comes from
//! ADPCM-A samples (drums, voices, SFX) and ADPCM-B streams; SSG is also
//! implemented because the BIOS jingle and many UI clicks use it.
//!
//! Decoding tables and channel-update logic are ported byte-identically
//! from FBNeo `src/burn/snd/fm.c` (jedi_table, step_inc, steps[49]) and
//! `src/burn/snd/ymdeltat.c` (decode_tableB1, decode_tableB2). These
//! are the same tables MAME's `ymfm` library uses.
//!
//! References:
//!   * Yamaha YM2610 OPNB datasheet (1990).
//!   * MAME `src/devices/sound/ymopn.cpp` (via ymfm).
//!   * FBNeo `src/burn/snd/fm.c` and `ymdeltat.c`.
//!   * [neogeodev wiki — YM2610](https://wiki.neogeodev.org/index.php/YM2610).
//!
//! Native output: stereo 16-bit @ ~55,555 Hz (master clock / 144).
//! For Neo Geo with master = 8 MHz YM2610 clock: 8_000_000 / 144 ≈ 55_555.
//!
//! The implementation runs in "fixed-rate" mode: every call to
//! `step_one_sample()` advances all channels by one output frame.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss,
         clippy::cast_possible_truncation, clippy::cast_lossless,
         clippy::cast_precision_loss, clippy::too_many_lines,
         clippy::similar_names, clippy::if_not_else)]

// ===========================================================================
//   ADPCM-A decoding tables (verbatim from FBNeo fm.c, lines 2675-2702)
// ===========================================================================

/// Yamaha's "1.1^N" step table — 49 entries (`16 * 1.1^N`, rounded).
const ADPCMA_STEPS: [i32; 49] = [
     16,  17,   19,   21,   23,   25,   28,
     31,  34,   37,   41,   45,   50,   55,
     60,  66,   73,   80,   88,   97,  107,
    118, 130,  143,  157,  173,  190,  209,
    230, 253,  279,  307,  337,  371,  408,
    449, 494,  544,  598,  658,  724,  796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

/// Step increment per nibble bottom-3-bits — 8 entries.
/// Note: 4 entries are -1*16 because nibbles 0..3 decrease step.
const ADPCMA_STEP_INC: [i32; 8] = [
    -1 * 16, -1 * 16, -1 * 16, -1 * 16,
     2 * 16,  5 * 16,  7 * 16,  9 * 16,
];

/// Pre-computed `jedi_table[step*16 + nib]` — replicated from
/// FBNeo `Init_ADPCMATable()`.
static JEDI_TABLE: once_cell::sync::Lazy<[i32; 49 * 16]> =
    once_cell::sync::Lazy::new(|| {
        let mut t = [0i32; 49 * 16];
        for step in 0..49usize {
            for nib in 0..16usize {
                let mag = (2 * (nib as i32 & 0x07) + 1) * ADPCMA_STEPS[step] / 8;
                t[step * 16 + nib] = if nib & 0x08 != 0 { -mag } else { mag };
            }
        }
        t
    });

// ===========================================================================
//   ADPCM-B (Delta-T) decoding tables (verbatim from FBNeo ymdeltat.c)
// ===========================================================================

/// Forecast-to-next-forecast multiplier table.
#[allow(dead_code)]
const DELTAT_TABLE_B1: [i32; 16] = [
     1,   3,   5,   7,   9,  11,  13,  15,
    -1,  -3,  -5,  -7,  -9, -11, -13, -15,
];

/// Delta-to-next-delta multiplier table (×64).
const DELTAT_TABLE_B2: [i32; 16] = [
     57,  57,  57,  57, 77, 102, 128, 153,
     57,  57,  57,  57, 77, 102, 128, 153,
];

// ===========================================================================
//   ADPCM-A channel
// ===========================================================================

#[derive(Debug, Clone)]
struct AdpcmAChan {
    /// True when the channel is currently keyed-on.
    on: bool,
    /// Instrument level (IL) — raw 5-bit register value already XORed with 0x1F.
    /// Range 0..=31, where 0 = loudest and 31 = silent.
    /// Combined with the chip master TL (also 0..=63, 0=loudest) per ymfm.
    il: u8,
    /// Pan mask straight from register: bit 7 = pan LEFT, bit 6 = pan RIGHT
    /// (matches ymfm `m_regs.ch_pan_left/right`).
    pan: u8,
    /// Start address in bytes (low 24 bits used).
    start: u32,
    /// End address (inclusive, bytes).
    end: u32,
    /// Current sample-byte address (bytes×2 = nibbles).
    addr_nib: u32,
    /// Buffered byte (so we don't refetch on every nibble).
    cur_byte: u8,
    /// Accumulator (12-bit signed).
    acc: i32,
    /// Step index (0..48), the stride into JEDI_TABLE.
    step: i32,
    /// Cached `adpcm_out` from FBNeo: precomputed `(acc * vol_mul) >> vol_shift
    /// & ~3` so that ticks *between* nibble events reproduce the exact same
    /// 12-bit value the real chip's track-and-hold output would hold.
    /// Updated every time a nibble is processed OR the volume registers
    /// change (TL master, IL per-channel). Matches FBNeo's `ch->adpcm_out`.
    adpcm_out: i32,
    /// Cached `(vol_mul, vol_shift)` for this channel — derived from
    /// `adpcmTL + IL` and refreshed whenever either changes (FBNeo
    /// `FM_ADPCMAWrite` cases 0x01 and 0x08–0x0D).
    vol_mul: i32,
    vol_shift: i32,
}

impl AdpcmAChan {
    const fn new() -> Self {
        Self {
            // Default IL=0 (loudest) and pan=both. Matches MAME/ymfm reset state
            // and means that a key-on issued before the driver writes $08..$0D
            // is still audible (some BIOS test routines rely on this).
            on: false, il: 0, pan: 0xC0,
            start: 0, end: 0,
            addr_nib: 0, cur_byte: 0, acc: 0, step: 0,
            adpcm_out: 0,
            // Default vol: IL=0+TL=0 -> mul=15, shift=1 (loudest non-silent).
            vol_mul: 15, vol_shift: 1,
        }
    }
    fn key_on(&mut self) {
        self.on = true;
        self.addr_nib = self.start * 2;
        self.acc = 0;
        self.step = 0;
        self.cur_byte = 0;
        self.adpcm_out = 0;
    }
    fn key_off(&mut self) {
        self.on = false;
        // ymfm `adpcm_a_channel::clock`: `if (m_playing == 0) { m_accumulator = 0; return false; }`.
        // Silencing the accumulator on keyoff prevents residual DC and
        // eliminates the click/hum tail that could otherwise leak through the
        // track-and-hold path (see `adpcm_out` refresh below).
        self.acc = 0;
        self.adpcm_out = 0;
    }

    /// Recompute `(vol_mul, vol_shift)` and `adpcm_out` from current
    /// `acc` + cached volume. Mirrors ymfm `adpcm_a_channel::output`
    /// (`ymfm_adpcm.cpp:223-246`) which computes
    ///   `mul   = 15 - (vol & 7)`
    ///   `shift = 4 + 1 + (vol >> 3)`
    ///   `value = ((int16_t(acc << 4) * mul) >> shift) & ~3`
    /// The `acc << 4` sign-extends the 12-bit accumulator into an int16
    /// centred at zero, so the previous `acc * vol_mul` (with a shift of
    /// only `1 + (vol >> 3)`) both lost the sign bit and was off by 16 in
    /// amplitude. This new form is bit-identical to ymfm.
    fn refresh_volume(&mut self, master_tl: u8) {
        let volume = (self.il as i32) + (master_tl as i32);
        if volume >= 63 {
            self.vol_mul = 0;
            self.vol_shift = 0;
        } else {
            self.vol_mul = 15 - (volume & 7);              // 0.75 dB per step
            self.vol_shift = 4 + 1 + (volume >> 3);        // -6 dB per step, +4 for `acc << 4`
        }
        // Recompute cached output sample (same formula as step_nibble below).
        //
        // **BUG FIX (v42)**: the previous form `((self.acc as i16 as i32) << 4)`
        // did NOT mask to 12 bits first, so it lost the sign of the 12-bit
        // accumulator whenever `self.acc` was stored in the 0..4095 unsigned
        // range. That path is executed every time the driver writes TL / IL /
        // pan mid-stream (which mslug2's intro does constantly), producing
        // samples up to 2x over-amplified compared to the track-and-hold path
        // in `step_nibble`. The exact ymfm formula is
        // `int16_t(m_accumulator << 4)` -- first shift the 12-bit UNSIGNED
        // value by 4 into the top nibble of an int16 slot, THEN reinterpret
        // that int16 with sign so bit-11 (which is now bit-15) sign-extends
        // into the full 32-bit multiplication. Without the initial mask the
        // sign extension is meaningless because bit-15 of the raw i32 is 0.
        self.adpcm_out = if !self.on || self.vol_mul == 0 {
            0
        } else {
            let ext = (((self.acc & 0xFFF) as u16) << 4) as i16 as i32;
            ((ext * self.vol_mul) >> self.vol_shift) & !3
        };
    }
    /// Advance one nibble. Returns the 12-bit accumulator sample.
    ///
    /// Verbatim port of ymfm `adpcm_a_channel::clock` (`ymfm_adpcm.cpp:151-214`):
    ///
    /// 1. If not playing → clear `acc` and return.
    /// 2. If we are about to read nibble 0 (byte-aligned), fetch the byte AND
    ///    check EOS by comparing `(cur_addr ^ end_shifted) & 0xfffff == 0`
    ///    (20 bits only — ymfm comment: "only low 20 bits are used for
    ///    comparison on the YM2610").
    /// 3. Decode the nibble via the jedi table, wrap `acc` to 12 bits with a
    ///    simple `& 0xfff` (no sign-extend — the accumulator IS 12-bit).
    /// 4. Update `step_index` clamped to `[0, 48]` (note: our `step` field
    ///    stores `step_index * 16`, so we cap at `48*16 = 768`).
    fn step_nibble(&mut self, rom: &[u8]) -> i32 {
        // ymfm: `if (m_playing == 0) { m_accumulator = 0; return false; }`
        if !self.on {
            self.acc = 0;
            return 0;
        }

        let data;
        if (self.addr_nib & 1) == 0 {
            // About to read nibble 0 → fetch the byte first AND check EOS.
            // ymfm end-comparison uses only the low 20 bits (`& 0xfffff`).
            //   uint32_t end = (m_regs.ch_end(m_choffs) + 1) << m_address_shift;
            //   if (((m_curaddress ^ end) & 0xfffff) == 0) { stop; }
            // In our layout `self.end` already includes the trailing `| 0xFF`
            // written by `write_adpcm_a` (i.e. `((hi<<8)|lo)<<8 | 0xFF`),
            // which equals `((end_reg + 1) << 8) - 1`. Convert to the
            // ymfm-canonical `(end_reg + 1) << 8 == self.end + 1`, then
            // multiply by 2 to move from bytes to nibbles.
            let end_nib = (self.end + 1) << 1;
            let cur_nib = self.addr_nib;
            // 20 bits of bytes → 21 bits of nibbles. Compare after masking.
            if ((cur_nib ^ end_nib) & ((1 << 21) - 1)) == 0 {
                self.on = false;
                self.acc = 0;
                self.adpcm_out = 0;
                return 0;
            }
            let byte_addr = (self.addr_nib >> 1) as usize;
            self.cur_byte = if byte_addr < rom.len() { rom[byte_addr] } else { 0 };
            data = ((self.cur_byte >> 4) & 0x0F) as i32;
        } else {
            // Low nibble of previously-fetched byte.
            data = (self.cur_byte & 0x0F) as i32;
        }
        self.addr_nib = self.addr_nib.wrapping_add(1);

        // Update accumulator: `self.step` is already in units of
        // `step_index * 16` (ADPCMA_STEP_INC is pre-multiplied) so
        // `JEDI_TABLE[step + data]` matches FBNeo's
        // `jedi_table[step_index*16 + nib]` layout.
        let jedi_idx = (self.step as usize) + (data as usize);
        let delta = JEDI_TABLE[jedi_idx.min(JEDI_TABLE.len() - 1)];
        // ymfm: `m_accumulator = (m_accumulator + delta) & 0xfff;` — the
        // 12-bit hardware accumulator wraps modulo-4096. We then reinterpret
        // it as a signed 12-bit value when computing the DAC output below.
        self.acc = (self.acc.wrapping_add(delta)) & 0xFFF;

        // Update step index (clamped 0..=48).
        self.step = self.step.wrapping_add(ADPCMA_STEP_INC[(data & 7) as usize]);
        if self.step > 48 * 16 { self.step = 48 * 16; }
        if self.step < 0       { self.step = 0; }

        // Compute cached track-and-hold output using the SAME formula as
        // ymfm `adpcm_a_channel::output` (`ymfm_adpcm.cpp:239`):
        //   value = ((int16_t(m_accumulator << 4) * mul) >> shift) & ~3
        // The `<< 4` sign-extends the 12-bit accumulator into an int16
        // centred at zero (bit 11 becomes bit 15). Between nibble events the
        // YM3016 DAC holds this value.
        self.adpcm_out = if self.vol_mul == 0 {
            0
        } else {
            // Sign-extend from 12-bit acc to i32 via `acc << 4` reinterpreted
            // as i16 then widened. This exactly mirrors ymfm's cast chain.
            let ext = (((self.acc & 0xFFF) as u16) << 4) as i16 as i32;
            ((ext * self.vol_mul) >> self.vol_shift) & !3
        };

        // Return the signed 12-bit accumulator (for callers that peek).
        (((self.acc & 0xFFF) as u16) << 4) as i16 as i32 >> 4
    }
}

// ===========================================================================
//   ADPCM-B (Delta-T) channel
// ===========================================================================

#[derive(Debug, Clone)]
struct DeltaTChan {
    on: bool,
    /// Output volume (0..255, raw register $1B).
    volume: u8,
    /// Pan mask: bit 7 = LEFT, bit 6 = RIGHT (ymfm convention).
    pan: u8,
    /// Delta (sample rate increment) — 16-bit.
    delta: u16,
    /// Start byte address.
    start: u32,
    /// End byte address (inclusive).
    end: u32,
    /// **Legacy toggle** (kept for A/B testing against older baselines).
    /// ymfm `adpcm_b_channel::output` ALWAYS linear-interpolates between
    /// the previous and current accumulator using the Q16 fractional
    /// `m_position`:
    ///   `(prev * ((pos ^ 0xFFFF) + 1) + cur * pos) >> 16`
    /// so the default MUST be `true` for chip-accurate output. Setting this
    /// to `false` reproduces our v33 raw-sample behaviour (subjective A/B
    /// only, do NOT use in production).
    interpolate: bool,
    /// Internal Q16 fractional sample counter used for Delta-T nibble timing.
    now_step: u32,
    /// Current byte-nibble address.
    addr_nib: u32,
    cur_byte: u8,
    /// Forecast/accumulator (16-bit signed) — the current decoded sample.
    acc: i32,
    /// Previous decoded sample, used as the lower endpoint for linear
    /// interpolation (ymfm `adpcm_b_channel::output`).
    prev_acc: i32,
    /// Forecast delta (16-bit, "step size"). ymfm calls this `m_adpcm_step`
    /// and clamps it to `[STEP_MIN, STEP_MAX] = [127, 24576]`.
    adpcmd: i32,
    /// Loop flag (REG 0x10 bit 4).
    looped: bool,
    /// Which nibble of the current byte we consume next: 0 = high nibble
    /// (freshly fetched), 1 = low nibble. Matches ymfm `m_curnibble` and is
    /// XORed each nibble step (`ymfm_adpcm.cpp:472`).
    curnibble: u8,
}

const DELTAT_ADDR_MASK: u32 = (1 << 25) - 1;

impl DeltaTChan {
    const fn new() -> Self {
        Self {
            // ymfm reset: regdata all zeroed -> volume=0 (mute), pan=0 (no out),
            // delta=0. Driver writes the real values before key-on.
            on: false, volume: 0, pan: 0,
            delta: 0,
            start: 0, end: 0,
            // Chip-accurate default: linear interpolation always ON, like ymfm.
            interpolate: true,
            now_step: 0, addr_nib: 0, cur_byte: 0,
            acc: 0, prev_acc: 0, adpcmd: 127,
            looped: false,
            curnibble: 0,
        }
    }
    fn key_on(&mut self) {
        self.on = true;
        // ymfm `load_start`: `m_curaddress = start << address_shift; m_curnibble = 0;`
        self.addr_nib = (self.start * 2) & DELTAT_ADDR_MASK;
        self.curnibble = 0;
        self.now_step = 0;
        self.acc = 0;
        self.prev_acc = 0;
        self.adpcmd = 127;
        self.cur_byte = 0;
    }
    fn key_off(&mut self) {
        self.on = false;
        // Match ymfm: clear the accumulator so the DAC settles to zero and
        // no residual sample leaks through the interpolator.
        self.acc = 0;
        self.prev_acc = 0;
    }
    /// Advance one output sample at the YM2610 native ~55.5 kHz rate.
    ///
    /// Verbatim port of ymfm `adpcm_b_channel::clock`+`::output`
    /// (`ymfm_adpcm.cpp:447-558`). Structural differences vs the previous
    /// version:
    ///
    /// * `curnibble` is now an explicit field (was inferred from address
    ///   parity, which is close but not identical when `addr_nib` wraps).
    /// * EOS check is done AFTER decoding the SECOND nibble of a byte
    ///   (i.e. when we just processed `curnibble==1`), matching ymfm exactly.
    /// * Linear interpolation is ALWAYS applied (default `interpolate=true`),
    ///   which is what the real chip does through the YM3016 DAC.
    fn step_one(&mut self, rom: &[u8]) -> i32 {
        if !self.on { return self.acc; }

        // Advance the Q16 fractional position by delta_n.
        let new_pos = (self.now_step as u32).wrapping_add(self.delta as u32);
        self.now_step = new_pos & 0xFFFF;

        // If we did not overflow, the DAC just holds the previous value.
        // Still emit the interpolated sample so callers get smooth audio.
        if new_pos < 0x1_0000 {
            return self.output_sample();
        }

        // At least one overflow: process nibble(s). In practice the delta_n
        // is much smaller than 0x10000 so a single overflow per call is the
        // norm, but we loop just in case (matches ymfm which also loops
        // implicitly via `clock` being called per FM sample).
        //
        // ymfm reads the byte ONLY on curnibble==0, then extracts:
        //   uint8_t data = uint8_t(m_curbyte << (4 * m_curnibble)) >> 4;
        //   m_curnibble ^= 1;
        if self.curnibble == 0 {
            // Fetch a fresh byte from V-ROM.
            let byte_addr = (self.addr_nib >> 1) as usize;
            self.cur_byte = if byte_addr < rom.len() { rom[byte_addr] } else { 0 };
        }
        let data: i32 = if self.curnibble == 0 {
            ((self.cur_byte >> 4) & 0x0F) as i32
        } else {
            (self.cur_byte & 0x0F) as i32
        };
        self.curnibble ^= 1;

        // After consuming the LOW nibble (curnibble is now 0 post-XOR), we
        // advance the byte cursor and check EOS/limit/repeat — same order
        // as ymfm `ymfm_adpcm.cpp:474-508`.
        if self.curnibble == 0 {
            // Compute inclusive-last-byte end: ymfm uses
            //   at_end := m_curaddress == ((end + 1) << address_shift) - 1
            // In our layout `self.end` already stores `((end_reg << 8) | 0xFF)`
            // written by write_reg_a $14/$15, which equals the ymfm-canonical
            // `((end_reg + 1) << 8) - 1`. So we can compare byte addresses
            // directly: current byte address == self.end.
            let cur_byte = self.addr_nib >> 1;
            if cur_byte == self.end {
                if self.looped {
                    // Loop back to start: reload cursor, keep acc smooth.
                    self.addr_nib = (self.start << 1) & DELTAT_ADDR_MASK;
                    // Do NOT reset acc/adpcmd on loop — ymfm `load_start`
                    // reloads the address but leaves the accumulator; the
                    // driver is expected to encode a smooth loop point.
                } else {
                    self.on = false;
                    self.acc = 0;
                    self.prev_acc = 0;
                    return 0;
                }
            } else {
                self.addr_nib = self.addr_nib.wrapping_add(2) & DELTAT_ADDR_MASK;
            }
        }

        // Snapshot previous accumulator for interpolation.
        self.prev_acc = self.acc;

        // Yamaha Delta-T decode (ymfm formulas, `ymfm_adpcm.cpp:521-531`):
        //   forecast: 1/8, 3/8, 5/8, ... (odd numerators)
        //   delta = (2 * mag + 1) * adpcmd / 8;  sign from bit 3
        let mag = (data & 7) as i32;
        let mut delta = (2 * mag + 1) * self.adpcmd / 8;
        if data & 8 != 0 { delta = -delta; }
        self.acc = (self.acc + delta).clamp(-32768, 32767);

        // Scale the ADPCM step: 0.9, 0.9, 0.9, 0.9, 1.2, 1.6, 2.0, 2.4.
        // (Table implicitly /64.)
        self.adpcmd = (self.adpcmd * DELTAT_TABLE_B2[data as usize] / 64)
            .clamp(127, 24576);

        self.output_sample()
    }

    /// Interpolated DAC output using the Q16 fractional position.
    /// Mirrors ymfm `adpcm_b_channel::output` (`ymfm_adpcm.cpp:548`):
    ///   `result = (prev * ((position ^ 0xFFFF) + 1) + accum * position) >> 16`
    #[inline]
    fn output_sample(&self) -> i32 {
        if self.interpolate {
            let pos = self.now_step as i64;
            let inv = ((pos ^ 0xFFFF) + 1) as i64;
            (((self.prev_acc as i64) * inv + (self.acc as i64) * pos) >> 16) as i32
        } else {
            self.acc
        }
    }
}

// ===========================================================================
//   SSG (AY-3-8910 derived: 3 square tones + 1 noise + envelope)
// ===========================================================================
//
// Ported from ymfm `ymfm_ssg.cpp` / MAME ay8910. Keeps amplitude curve and
// envelope semantics consistent with what Neo Geo titles expect.
//
//   - SSG external clock is YM2610_master / 4 (= 2 MHz on Neo Geo).
//   - Tone/noise/envelope step clock inside the AY core is 250 kHz.
//   - Tone/noise period units are clock/16 (state toggle at 50% duty).
//   - Envelope: 32-step state machine + 4-bit shape decode.
//   - Volume curve: 32-entry table calibrated by MAME (`s_amplitudes`).
//
// Internal clock vs host output:
//   - YM2610 host output is master / 144 (~55.555 kHz).
//   - SSG step clock is 250 kHz (= 4.5 internal ticks per host sample).
//   We use a 16.16 fixed-point fractional counter so the SSG core advances
//   exactly the right number of internal ticks per host sample.

/// 32-entry amplitude table from `ymfm_ssg.cpp` (`s_amplitudes`).
/// 0 = silent, 31 = loudest.
const SSG_AMPLITUDES: [i16; 32] = [
        0,    32,    78,   141,   178,   222,   262,   306,
      369,   441,   509,   585,   701,   836,   965,  1112,
     1334,  1595,  1853,  2146,  2576,  3081,  3576,  4135,
     5000,  6006,  7023,  8155,  9963, 11976, 14132, 16382,
];

/// SSG internal step clock = chip / 4.
///
/// Derivation (verbatim from FBNeo `fm.c` lines 1942 and 4072):
///   `OPNSetPres(OPN, 6*24, 6*24, 4*2)` → OPN prescaler 144, SSG prescaler 8.
///   `SSGClk(index, OPN->ST.clock * 2 / SSGpres)` → SSG clock = clock * 2/8 = clock/4.
///
/// For NeoGeo (`NEOGEO_YM2610_CLOCK = NEOGEO_MASTER_CLOCK/3 = 24_000_000/3 = 8_000_000`):
///   SSG external clock = 8_000_000 / 4 = 2_000_000 Hz
///
/// Inside the AY, tone/noise generators use a further /8 divider (see FBNeo
/// `ay8910.c` line 740 comment: "the step clock for the tone and noise
/// generators is the chip clock divided by 8"), so:
///   SSG step clock = 2_000_000 / 8 = 250_000 Hz
///
/// Tone frequency = step_clock / (2 * period) = 125_000 / period (canonical AY formula).
#[allow(dead_code)]
pub(crate) const SSG_STEP_CLOCK_HZ: u32 = 250_000;

/// Host audio sample rate = master / 144 = 8_000_000 / 144 ≈ 55_555.555 Hz.
#[allow(dead_code)]
pub(crate) const HOST_SAMPLE_RATE_HZ: u32 = 55_555;

/// SSG step-clock ticks per host audio sample, in 16.16 fixed-point.
///
/// Exact ratio: 250_000 / (8_000_000/144) = 250_000 * 144 / 8_000_000 = **4.5**
///   → `4.5 * 65536 = 294_912`.
///
/// **Bug fix (v34)**: the previous value `18 << 16` (= 18 ticks/sample) drove
/// the SSG at ~999_990 Hz, four times the real chip's 250 kHz step clock.
/// That made every BIOS coin beep, menu click and SSG-driven SFX play
/// **two octaves too high** (`500_000 / period` vs the correct
/// `125_000 / period`). User-visible symptom: insert-coin sound "more
/// trebly than it should be".
const SSG_TICKS_PER_SAMPLE_Q16: u32 = 294_912;

/// YM2610 busy duration after each data-port write, expressed in Z80 cycles.
///
/// MAME/ymfm marks YM2610 writes busy for `32 * clock_prescale()` chip clocks.
/// On Neo Geo the YM2610 runs at 8 MHz and the OPN prescale is 6, so the busy
/// window is `32 * 6 = 192` YM clocks = 24 µs. The Z80 runs at 4 MHz, hence
/// `24 µs * 4 MHz = 96` T-states.
const YM2610_BUSY_Z80_CYCLES: u32 = 96;

#[derive(Debug, Clone)]
struct Ssg {
    /// Channel tone period (12-bit) for A, B, C.
    tone_period: [u16; 3],
    /// Noise period (5-bit).
    noise_period: u8,
    /// Mixer register R7: bits 0..2 = tone disable per channel (active high),
    /// bits 3..5 = noise disable per channel.
    mixer: u8,
    /// Per-channel amplitude/env-enable (R8, R9, R10).
    /// Bit 4 = envelope enable; bits 0..3 = fixed amplitude (0..15).
    vol: [u8; 3],
    /// Envelope period (16-bit).
    env_period: u16,
    /// Envelope shape (R13, 4 bits).
    env_shape: u8,
    /// Tone counters.
    tone_count: [u32; 3],
    tone_state: [u8; 3],
    /// Noise counter + LFSR (17-bit, feedback from bits 0 and 3).
    noise_count: u32,
    noise_state: u32,
    /// Envelope counter + state (state grows from 0 upwards; ymfm semantics).
    env_count: u32,
    env_state: u32,
    /// Q16 accumulator for ticks-per-host-sample.
    tick_frac: u32,
    /// Running DC estimator (Q16) for the AC-coupling HPF applied to the SSG
    /// output — mirrors the series-capacitor between the YM3016 SSG pin and
    /// the amplifier stage on the Neo Geo audio board. One-pole HP ~10 Hz.
    dc_estimator_q16: i64,
}

impl Ssg {
    /// One-pole HP alpha in Q16 for ~10 Hz cutoff at 55555 Hz.
    /// `round((1 - exp(-2*pi*10/55555)) * 65536) = 74`.
    const HP_ALPHA_Q16: i64 = 74;

    const fn new() -> Self {
        Self {
            tone_period: [0; 3], noise_period: 0, mixer: 0xFF,
            vol: [0; 3], env_period: 0, env_shape: 0,
            tone_count: [0; 3], tone_state: [0; 3],
            noise_count: 0, noise_state: 1,
            env_count: 0, env_state: 0,
            tick_frac: 0,
            dc_estimator_q16: 0,
        }
    }

    /// Reset the SSG core to the ymfm/MAME power-on state.
    fn reset(&mut self) {
        self.tone_period = [0; 3];
        self.noise_period = 0;
        self.mixer = 0xFF;
        self.vol = [0; 3];
        self.env_period = 0;
        self.env_shape = 0;
        self.tone_count = [0; 3];
        self.tone_state = [0; 3];
        self.noise_count = 0;
        self.noise_state = 1;
        self.env_count = 0;
        self.env_state = 0;
        self.tick_frac = 0;
        self.dc_estimator_q16 = 0;
    }

    /// Write a register (0x00..0x0F).
    fn write_reg(&mut self, reg: u8, val: u8) {
        match reg {
            0x00 => self.tone_period[0] = (self.tone_period[0] & 0xFF00) | val as u16,
            0x01 => self.tone_period[0] = (self.tone_period[0] & 0x00FF) | ((val as u16 & 0x0F) << 8),
            0x02 => self.tone_period[1] = (self.tone_period[1] & 0xFF00) | val as u16,
            0x03 => self.tone_period[1] = (self.tone_period[1] & 0x00FF) | ((val as u16 & 0x0F) << 8),
            0x04 => self.tone_period[2] = (self.tone_period[2] & 0xFF00) | val as u16,
            0x05 => self.tone_period[2] = (self.tone_period[2] & 0x00FF) | ((val as u16 & 0x0F) << 8),
            0x06 => self.noise_period = val & 0x1F,
            0x07 => self.mixer = val,
            0x08 => self.vol[0] = val & 0x1F,
            0x09 => self.vol[1] = val & 0x1F,
            0x0A => self.vol[2] = val & 0x1F,
            0x0B => self.env_period = (self.env_period & 0xFF00) | val as u16,
            0x0C => self.env_period = (self.env_period & 0x00FF) | ((val as u16) << 8),
            0x0D => {
                // Shape write resets the envelope state machine (ymfm).
                self.env_shape = val & 0x0F;
                self.env_state = 0;
                self.env_count = 0;
            }
            _ => {}
        }
    }

    /// One internal SSG tick (master/8). Mirrors ymfm `ssg_engine::clock`.
    ///
    /// The AY-3-8910 datasheet states "a period of 0 shall produce an
    /// identical result to a period of 1"; ymfm relies on that implicitly.
    /// We enforce it explicitly with `.max(1)` on the three dividers so a
    /// power-on register still holding 0 never toggles every single tick.
    #[inline]
    fn clock_tick(&mut self) {
        for chan in 0..3 {
            self.tone_count[chan] = self.tone_count[chan].wrapping_add(1);
            let period = (self.tone_period[chan] as u32).max(1);
            if self.tone_count[chan] >= period {
                self.tone_state[chan] ^= 1;
                self.tone_count[chan] = 0;
            }
        }
        self.noise_count = self.noise_count.wrapping_add(1);
        let np = (self.noise_period as u32).max(1);
        if (self.noise_count >> 1) >= np && self.noise_count != 1 {
            let fb = ((self.noise_state & 1) ^ ((self.noise_state >> 3) & 1)) << 17;
            self.noise_state = (self.noise_state ^ fb) >> 1;
            self.noise_count = 0;
        }
        self.env_count = self.env_count.wrapping_add(1);
        let ep = (self.env_period as u32).max(1);
        if self.env_count >= ep {
            self.env_state = self.env_state.wrapping_add(1);
            self.env_count = 0;
        }
    }

    /// Compute current envelope volume (0..=31) using ymfm shape decode.
    #[inline]
    fn envelope_volume(&mut self) -> u32 {
        let hold = (self.env_shape & 0x01) != 0;
        let alternate = (self.env_shape & 0x02) != 0;
        let attack = (self.env_shape & 0x04) != 0;
        let cont = (self.env_shape & 0x08) != 0;
        if (hold || !cont) && self.env_state >= 32 {
            self.env_state = 32;
            if (attack ^ alternate) && cont { 31 } else { 0 }
        } else {
            let mut atk = attack;
            if alternate {
                atk ^= ((self.env_state >> 5) & 1) != 0;
            }
            let s = self.env_state & 31;
            if atk { s } else { s ^ 31 }
        }
    }

    /// Advance one host sample. Returns AC-coupled mono i32 (centred at 0).
    fn step_one(&mut self) -> i32 {
        self.tick_frac = self.tick_frac.wrapping_add(SSG_TICKS_PER_SAMPLE_Q16);
        let ticks = self.tick_frac >> 16;
        self.tick_frac &= 0xFFFF;
        for _ in 0..ticks {
            self.clock_tick();
        }

        let env_vol = self.envelope_volume();
        let noise_bit = self.noise_state & 1;
        let mixer = self.mixer;
        // Unipolar 3-channel sum — exactly like ymfm `ssg_engine::output`.
        let mut acc: i32 = 0;
        for chan in 0..3 {
            let noise_on = ((mixer >> (chan + 3)) & 1) as u32 | noise_bit;
            let tone_on  = ((mixer >> chan) & 1) as u32 | self.tone_state[chan] as u32;
            let on = noise_on & tone_on;
            if on == 0 { continue; }
            let volume: u32 = if (self.vol[chan] & 0x10) != 0 {
                env_vol
            } else {
                let v = ((self.vol[chan] & 0x0F) as u32) * 2;
                if v != 0 { v | 1 } else { 0 }
            };
            acc += SSG_AMPLITUDES[(volume & 31) as usize] as i32;
        }

        // AC-couple via one-pole HPF: y[n] = x[n] - dc, dc += alpha*(x - dc).
        // Mirrors the AC-coupling capacitor on the SSG analog output pin.
        let x_q16 = (acc as i64) << 16;
        let error = x_q16 - self.dc_estimator_q16;
        self.dc_estimator_q16 += (error * Self::HP_ALPHA_Q16) >> 16;
        ((x_q16 - self.dc_estimator_q16) >> 16) as i32
    }
}

// ===========================================================================
//   V-ROM classification (MAME/FBNeo naming convention)
// ===========================================================================

/// Which ADPCM address space a V-ROM belongs to based on its filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VRomKind {
    /// `*-v1*` (`v1`, `v11`, `v12`, ...) — ADPCM-A samples.
    AdpcmA,
    /// `*-v2*` (`v2`, `v21`, `v22`, ...) — ADPCM-B (Delta-T) samples.
    AdpcmB,
    /// Unlabelled or shared V-ROM — alias into both spaces.
    Shared,
}

/// Classify a V-ROM by its MAME/FBNeo filename.
///
/// **Rule (verified against `hash/neogeo.xml` in MAME upstream):**
///
/// * Extension of the form `vXY` — **two digits**, e.g. `021-v11.v11`,
///   `021-v21.v21`, `nam75-v11.v11`, `wh1-v22.v22`: the FIRST digit encodes
///   the region (`1` → ADPCM-A, `2` → ADPCM-B) and the second digit is the
///   index of that ROM within its region.
///
/// * Extension of the form `vX` — **one digit**, e.g. `201-v1.v1`,
///   `201-v2.v2`, `201-v3.v3`, `201-v4.v4` (Metal Slug and every other cart
///   that only uses ADPCM-A): the digit is just an index; ALL such files
///   belong to the ADPCM-A region.
///
/// * Anything else (single unlabelled `.rom` / `.bin` V blob): shared — alias
///   the same data into both address spaces.
///
/// This rule reproduces MAME's `<dataarea name="ymsnd:adpcma">` /
/// `<dataarea name="ymsnd:adpcmb">` grouping for every cart in the current
/// `neogeo.xml`, and matches the FBNeo `BRF_SND` type-5 (A) / type-6 (B)
/// tagging in `d_neogeo.cpp`.
fn classify_v_rom(filename: &str) -> VRomKind {
    let base = filename
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(filename)
        .to_ascii_lowercase();

    let dot = match base.rfind('.') {
        Some(d) => d,
        None => return VRomKind::Shared,
    };
    let ext = &base[dot + 1..];
    let ext_bytes = ext.as_bytes();

    // `vXY` (two digits): first digit selects the region.
    if ext_bytes.len() == 3
        && ext_bytes[0] == b'v'
        && ext_bytes[1].is_ascii_digit()
        && ext_bytes[2].is_ascii_digit()
    {
        return match ext_bytes[1] {
            b'1' => VRomKind::AdpcmA,
            b'2' => VRomKind::AdpcmB,
            // 3–9 would be exotic (SSG-only channel? there's no such thing
            // on YM2610). Play safe and share the blob rather than dropping
            // audio for a set we don't recognise.
            _ => VRomKind::Shared,
        };
    }

    // `vX` (one digit): index only — always ADPCM-A.
    if ext_bytes.len() == 2
        && ext_bytes[0] == b'v'
        && ext_bytes[1].is_ascii_digit()
    {
        return VRomKind::AdpcmA;
    }

    VRomKind::Shared
}

#[cfg(test)]
mod v_rom_classify_tests {
    use super::{classify_v_rom, VRomKind};

    // ---- Two-digit extensions (region-encoded) --------------------------

    #[test]
    fn joyjoy_two_digit_split() {
        // MAME `hash/neogeo.xml` for joyjoy has 021-v11.v11 in
        // <dataarea name="ymsnd:adpcma"> and 021-v21.v21 in
        // <dataarea name="ymsnd:adpcmb">.
        assert_eq!(classify_v_rom("021-v11.v11"), VRomKind::AdpcmA);
        assert_eq!(classify_v_rom("021-v21.v21"), VRomKind::AdpcmB);
    }

    // ---- One-digit extensions (index only — ALL ADPCM-A) ----------------

    #[test]
    fn mslug_all_v_roms_are_adpcm_a() {
        // MAME `hash/neogeo.xml` for mslug puts BOTH v1 and v2 inside the
        // `ymsnd:adpcma` dataarea at offsets 0x000000 and 0x400000. There is
        // NO `ymsnd:adpcmb` dataarea for Metal Slug.
        assert_eq!(classify_v_rom("201-v1.v1"), VRomKind::AdpcmA);
        assert_eq!(classify_v_rom("201-v2.v2"), VRomKind::AdpcmA);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(classify_v_rom("021-V11.V11"), VRomKind::AdpcmA);
        assert_eq!(classify_v_rom("021-V21.V21"), VRomKind::AdpcmB);
    }

    #[test]
    fn unlabelled_falls_back_to_shared() {
        assert_eq!(classify_v_rom("custom-adpcm.rom"), VRomKind::Shared);
        assert_eq!(classify_v_rom("foo.bin"), VRomKind::Shared);
    }
}

// ===========================================================================
//   YM2610 main struct
// ===========================================================================

pub struct Ym2610 {
    /// Register file part A (00..FF).
    regs_a: Box<[u8; 256]>,
    /// Register file part B (00..FF).
    regs_b: Box<[u8; 256]>,
    /// Last address latched on port-A pair (0x4 = address, 0x5 = data).
    addr_a: u8,
    /// Last address latched on port-B pair (0x6 = address, 0x7 = data).
    addr_b: u8,
    /// Status register: bit 0 = TimerA, bit 1 = TimerB, bit 7 = busy.
    /// Bits 0/1 also gated by REG 0x29 mask (FM IRQ enables).
    status_a: u8,
    /// Remaining busy time after the last YM2610 data write, in Z80 cycles.
    busy_z80_cycles: u32,

    /// 6 ADPCM-A channels.
    adpcm_a: [AdpcmAChan; 6],
    /// 1 ADPCM-B channel (Delta-T).
    adpcm_b: DeltaTChan,
    /// SSG (3 channels).
    ssg: Ssg,

    /// IRQ line state (driven by Timer A/B + ADPCM-A end flags).
    pub irq_out: bool,
    /// Enable mask for IRQ (REG 0x29 / 0x1C bits).
    irq_enable: u8,
    /// Timer A reload value (10-bit, written via $24 high + $25 low 2 bits).
    timer_a_period: u32,
    /// Timer B reload value (8 bits).
    timer_b_period: u32,
    /// Timer A enable (bit 0 of $27 mode register).
    timer_a_enabled: bool,
    /// Timer B enable (bit 1).
    timer_b_enabled: bool,
    /// Timer A counter, in YM2610 output samples (1 sample = 144 master cycles).
    timer_a_count: i32,
    /// Timer B counter, in YM2610 output samples.
    timer_b_count: i32,
    /// ADPCM-A "arrived end" flags (one bit per channel).
    adpcm_a_end_flags: u8,
    /// Global ADPCM-A TL (master volume, 0..63, 0=loudest).
    adpcma_tl: u8,
    /// FM (4-channel OPN) implementation.
    fm: FmOpn,

    // DEBUG diagnostics (counts of non-zero contributions per subsystem).
    pub dbg_fm_nz: u64,
    pub dbg_adpcma_nz: u64,
    pub dbg_adpcmb_nz: u64,
    pub dbg_ssg_nz: u64,
    pub dbg_adpcma_keyon: u64,
    pub dbg_adpcmb_keyon: u64,
    pub dbg_fm_keyon: u64,
    /// Per-channel key-on counters for ADPCM-A (6 channels) and FM (4 channels).
    pub dbg_adpcma_keyon_ch: [u64; 6],
    pub dbg_adpcma_nz_ch: [u64; 6],
    pub dbg_fm_keyon_ch: [u64; 4],
    pub dbg_fm_nz_ch: [u64; 4],
    pub dbg_timer_flag_set: u64,
    pub dbg_status_read_nonzero: u64,
    pub dbg_status_read_total: u64,

    /// Fractional sample counter (for ADPCM-A 18.5 kHz at ~55 kHz output).
    adpcm_a_frac: u32,

    /// Tick subdivider for SSG (master / 8 — we tick it once per output sample
    /// which is close enough for sound effects).

    /// V-ROM space 0 (ADPCM-A) and 1 (ADPCM-B). The bus owns these slices;
    /// we keep a pointer-equivalent via shared Vec slices set at boot.
    pub adpcm_a_rom: Vec<u8>,
    pub adpcm_b_rom: Vec<u8>,
}

impl std::fmt::Debug for Ym2610 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ym2610")
            .field("addr_a", &self.addr_a)
            .field("addr_b", &self.addr_b)
            .field("status_a", &self.status_a)
            .field("irq_out", &self.irq_out)
            .field("adpcm_a_rom_len", &self.adpcm_a_rom.len())
            .field("adpcm_b_rom_len", &self.adpcm_b_rom.len())
            .finish()
    }
}

impl Default for Ym2610 {
    fn default() -> Self { Self::new() }
}

impl Ym2610 {
    pub fn new() -> Self {
        Self {
            regs_a: Box::new([0; 256]),
            regs_b: Box::new([0; 256]),
            addr_a: 0, addr_b: 0, status_a: 0, busy_z80_cycles: 0,
            adpcm_a: [
                AdpcmAChan::new(), AdpcmAChan::new(), AdpcmAChan::new(),
                AdpcmAChan::new(), AdpcmAChan::new(), AdpcmAChan::new(),
            ],
            adpcm_b: DeltaTChan::new(),
            ssg: Ssg::new(),
            irq_out: false,
            irq_enable: 0,
            adpcm_a_end_flags: 0,
            adpcma_tl: 0,
            timer_a_period: 0,
            timer_b_period: 0,
            timer_a_enabled: false,
            timer_b_enabled: false,
            timer_a_count: 0,
            timer_b_count: 0,
            fm: FmOpn::new(),
            dbg_fm_nz: 0,
            dbg_adpcma_nz: 0,
            dbg_adpcmb_nz: 0,
            dbg_ssg_nz: 0,
            dbg_adpcma_keyon: 0,
            dbg_adpcmb_keyon: 0,
            dbg_fm_keyon: 0,
            dbg_adpcma_keyon_ch: [0; 6],
            dbg_adpcma_nz_ch: [0; 6],
            dbg_fm_keyon_ch: [0; 4],
            dbg_fm_nz_ch: [0; 4],
            dbg_timer_flag_set: 0,
            dbg_status_read_nonzero: 0,
            dbg_status_read_total: 0,
            adpcm_a_frac: 0,
            adpcm_a_rom: Vec::new(),
            adpcm_b_rom: Vec::new(),
        }
    }

    /// Hook V-ROM data into the chip, routing each ROM to the correct
    /// ADPCM address space based on its MAME/FBNeo filename.
    ///
    /// ## Naming convention (MAME hash/neogeo.xml + FBNeo `d_neogeo.cpp`)
    ///
    /// The Neo Geo YM2610 has **two independent 24-bit address spaces**:
    /// ADPCM-A (region "adpcma" in MAME, `BRF_SND` type-5 in FBNeo) and
    /// ADPCM-B (region "adpcmb" in MAME, `BRF_SND` type-6 in FBNeo).
    /// Cartridges label their V-ROMs so the loader can tell them apart:
    ///
    /// * `NNN-v1?.v1?` (e.g. `021-v11.v11`, `202-v11.v1`) → **ADPCM-A**
    /// * `NNN-v2?.v2?` (e.g. `021-v21.v21`, `202-v21.v2`) → **ADPCM-B**
    /// * `NNN-v.v` or a single unlabelled V-ROM              → shared blob
    ///   (a lot of early carts and Metal Slug's `201-v1..v4` fall into
    ///   this bucket — they only carry ADPCM-A data and no ADPCM-B channel)
    ///
    /// Getting the routing wrong is catastrophic: the ADPCM-B Delta-T decoder
    /// walks its accumulator based on the bytes it reads, and feeding it
    /// ADPCM-A data (which is a different encoding, not signed deltas) makes
    /// the accumulator saturate positive within a few samples — exactly the
    /// "pitido constante + DC offset" symptom we were tracking in JoyJoy Kid.
    ///
    /// MAME reference: `src/mame/neogeo/neogeo.cpp` (`m_ym->set_addrmap(0,
    /// adpcma_map); m_ym->set_addrmap(1, adpcmb_map)`).
    pub fn install_v_roms(&mut self, v_roms: &[(String, Vec<u8>)]) {
        if v_roms.is_empty() {
            return;
        }
        // Bucket the ROMs by their MAME/FBNeo naming convention.
        let mut a_bucket: Vec<&(String, Vec<u8>)> = Vec::new();
        let mut b_bucket: Vec<&(String, Vec<u8>)> = Vec::new();
        let mut unlabelled: Vec<&(String, Vec<u8>)> = Vec::new();
        for entry in v_roms {
            match classify_v_rom(&entry.0) {
                VRomKind::AdpcmA => a_bucket.push(entry),
                VRomKind::AdpcmB => b_bucket.push(entry),
                VRomKind::Shared => unlabelled.push(entry),
            }
        }

        let concat = |bucket: &[&(String, Vec<u8>)]| -> Vec<u8> {
            let mut out = Vec::with_capacity(bucket.iter().map(|(_, d)| d.len()).sum());
            for (_, d) in bucket { out.extend_from_slice(d); }
            out
        };

        if a_bucket.is_empty() && b_bucket.is_empty() {
            // No naming hints at all — behave like the previous loader and
            // alias the shared blob into both spaces.
            let blob = concat(&unlabelled);
            self.adpcm_a_rom = blob.clone();
            self.adpcm_b_rom = blob;
        } else {
            // Named A/B ROMs are authoritative. Any unlabelled leftovers
            // (rare, would indicate a malformed set) get appended to A —
            // matching what MAME does for combined regions.
            let mut a = concat(&a_bucket);
            a.extend_from_slice(&concat(&unlabelled));
            let b = concat(&b_bucket);

            // MAME `neogeo_base_state::init_audio()` (src/mame/snk/neogeo.cpp:1367-1369):
            //     if (ymdelta_size) install adpcm_b
            //     else if (ym_size)  install adpcm_a in the ADPCM-B space
            // — the ADPCM-B address space falls back to the ADPCM-A blob
            // when the cart doesn't ship a separate delta-T ROM. Without
            // this fallback a driver that key-ons ADPCM-B on a mslug-style
            // cart would read all-zeros and hang the Delta-T decoder.
            self.adpcm_a_rom = a.clone();
            self.adpcm_b_rom = if b.is_empty() { a } else { b };
        }

        log::info!(
            "YM2610: V-ROM installed — ADPCM-A {} bytes ({} files), ADPCM-B {} bytes ({} files, {})",
            self.adpcm_a_rom.len(), a_bucket.len() + unlabelled.len(),
            self.adpcm_b_rom.len(), b_bucket.len(),
            if b_bucket.is_empty() && !self.adpcm_b_rom.is_empty() { "aliased-from-A per MAME fallback" } else { "native" },
        );
    }

    /// Read from port 0..=3 (the I/O ports as the Z80 sees them).
    /// Port 0 = status register, ports 1/2/3 = misc/SSG status/ADPCM status.
    pub fn read_port(&mut self, port: u8) -> u8 {
        match port & 3 {
            0 => {
                self.dbg_status_read_total += 1;
                let status = self.status_with_busy();
                if status != 0 { self.dbg_status_read_nonzero += 1; }
                status
            }
            1 => {
                // SSG register data — returns last latched SSG reg value
                let reg = self.addr_a & 0x0F;
                if reg < 0x10 { self.regs_a[reg as usize] } else { 0 }
            }
            2 => self.adpcm_a_end_flags,
            3 => 0,
            _ => unreachable!(),
        }
    }

    /// Write to port 0..=3 — the Z80 view.
    /// Port 0 = address A, port 1 = data A, port 2 = address B, port 3 = data B.
    pub fn write_port(&mut self, port: u8, val: u8) {
        match port & 3 {
            0 => self.addr_a = val,
            1 => {
                self.write_reg_a(self.addr_a, val);
                self.busy_z80_cycles = YM2610_BUSY_Z80_CYCLES;
            }
            2 => self.addr_b = val,
            3 => {
                self.write_reg_b(self.addr_b, val);
                self.busy_z80_cycles = YM2610_BUSY_Z80_CYCLES;
            }
            _ => unreachable!(),
        }
    }

    #[inline]
    fn status_with_busy(&self) -> u8 {
        if self.busy_z80_cycles != 0 {
            self.status_a | 0x80
        } else {
            self.status_a
        }
    }

    /// Advance the YM2610 host-visible busy timer by Z80 cycles.
    pub fn elapse_z80_cycles(&mut self, cycles: u32) {
        self.busy_z80_cycles = self.busy_z80_cycles.saturating_sub(cycles);
    }

    // -----------------------------------------------------------------
    //  YM2610 register decoder — verbatim port of FBNeo `fm.c::YM2610Write`
    // -----------------------------------------------------------------
    //  Port A  ($04 addr / $05 data):
    //    $00..$0F  SSG
    //    $10..$1C  ADPCM-B (Delta-T)
    //      $10: control 1   $11: control 2  (pan/AD-DA mode)
    //      $12: start L     $13: start H
    //      $14: stop  L     $15: stop  H
    //      $19: delta L     $1A: delta H
    //      $1B: volume      $1C: flag-control / IRQ mask
    //    $20..$2F  OPN mode / timers / FM key
    //      $24/$25: timer-A hi/lo   $26: timer-B    $27: mode
    //      $28:    FM key on/off (slot mask in high nibble, channel in low)
    //      $2D/$2E/$2F: prescaler
    //    $30..$FF  FM ch.1/2 operator regs (DT/MUL, TL, KS/AR, AM/DR, SR, SL/RR, etc.)
    //  Port B  ($06 addr / $07 data):
    //    $00..$2F  ADPCM-A control:
    //      $00: control (b7=dump/off  b5..0=channel mask)
    //      $01: master TL (b5..0 inverted)
    //      $08..$0D: per-channel pan(7..6) + IL(4..0 inverted)
    //      $10..$15: start L    $18..$1D: start H
    //      $20..$25: end   L    $28..$2D: end   H
    //    $30..$FF  FM ch.3/4 operator regs
    //
    // Sources:
    //   FBNeo `src/burn/snd/fm.c::YM2610Write`,  `FM_ADPCMAWrite`,
    //   FBNeo `src/burn/snd/ymdeltat.c::YM_DELTAT_ADPCM_Write`.
    fn write_reg_a(&mut self, addr: u8, val: u8) {
        self.regs_a[addr as usize] = val;
        match addr {
            // SSG
            0x00..=0x0F => self.ssg.write_reg(addr, val),
            // ADPCM-B / Delta-T
            0x10 => {
                if val & 0x01 != 0 { self.adpcm_b.key_off(); }
                if val & 0x80 != 0 {
                    self.adpcm_b.looped = val & 0x10 != 0;
                    self.adpcm_b.key_on();
                    self.dbg_adpcmb_keyon += 1;
                }
            }
            0x11 => self.adpcm_b.pan = val & 0xC0,
            0x12 | 0x13 => {
                let lo = self.regs_a[0x12] as u32;
                let hi = self.regs_a[0x13] as u32;
                self.adpcm_b.start = ((hi << 8) | lo) << 8;
            }
            0x14 | 0x15 => {
                let lo = self.regs_a[0x14] as u32;
                let hi = self.regs_a[0x15] as u32;
                self.adpcm_b.end = (((hi << 8) | lo) << 8) | 0xFF;
            }
            0x19 | 0x1A => {
                self.adpcm_b.delta = (self.regs_a[0x19] as u16) | ((self.regs_a[0x1A] as u16) << 8);
            }
            0x1B => self.adpcm_b.volume = val,
            0x1C => {} // flag mask — ignore
            0x22 => self.fm.write_lfo(val),

            // Timer A: $24 = bits 9..2, $25 = bits 1..0
            0x24 | 0x25 => {
                self.timer_a_period = ((self.regs_a[0x24] as u32) << 2)
                                    | ((self.regs_a[0x25] as u32) & 0x03);
            }
            // Timer B (8 bits)
            0x26 => { self.timer_b_period = self.regs_a[0x26] as u32; }
            0x27 => {
                // bit 0 = Load TA, 1 = Load TB, 2 = Enable TA IRQ, 3 = Enable TB IRQ,
                // bit 4 = Reset TA flag, 5 = Reset TB flag.
                //
                // ymfm / FBNeo: the Load bit is edge-triggered — the counter
                // is (re)loaded only on the 0→1 transition, not while the
                // driver holds the bit high. Otherwise every subsequent write
                // to $27 (e.g. flag-reset writes) would restart the timer.
                // We compare against the previously-latched value in regs_a.
                // Note: `regs_a[0x27]` was just overwritten to `val` at the
                // top of `write_reg_a`, so we mirror the state via the
                // enabled flags (which reflect the last effective load).
                let prev_ta_enabled = self.timer_a_enabled;
                let prev_tb_enabled = self.timer_b_enabled;
                if val & 0x10 != 0 { self.status_a &= !0x01; }
                if val & 0x20 != 0 { self.status_a &= !0x02; }
                let load_ta = val & 0x01 != 0;
                let load_tb = val & 0x02 != 0;
                if load_ta && !prev_ta_enabled {
                    self.timer_a_count = (1024 - self.timer_a_period as i32).max(1);
                }
                self.timer_a_enabled = load_ta;
                if load_tb && !prev_tb_enabled {
                    self.timer_b_count = ((256 - self.timer_b_period as i32) * 16).max(1);
                }
                self.timer_b_enabled = load_tb;
                self.irq_enable = val & 0x0C;
                self.fm.write_mode(val);
            }
            // FM key on/off. Verbatim port of FBNeo `fm.c::OPNWriteMode` case 0x28:
            //   c = val & 0x03; if c==3 break; if (val & 0x04) c += 3;
            //   slot bits in val[4..7] = SLOT1,SLOT2,SLOT3,SLOT4.
            // The 6 logical channels in our FmOpn cover bank A (0..2) and
            // bank B (3..5); YM2610 routes physical FM1/FM2 to indices 0,1
            // and FM3/FM4 to indices 3,4 (indices 2 and 5 exist in the
            // register decoder but are not wired to the DAC).
            0x28 => {
                let mut c = (val & 0x03) as usize;
                if c == 3 {
                    // unused slot pair
                } else {
                    if (val & 0x04) != 0 { c += 3; }
                    let slot_mask = (val >> 4) & 0x0F;
                    // Update per-channel telemetry for the four wired channels.
                    if slot_mask != 0 {
                        self.dbg_fm_keyon += 1;
                        let routed = [1usize, 2, 4, 5];
                        for (i, &idx) in routed.iter().enumerate() {
                            if c == idx { self.dbg_fm_keyon_ch[i] += 1; }
                        }
                    }
                    self.fm.key(c, slot_mask);
                }
            }
            0x2D | 0x2E | 0x2F => {}
            // FM operator registers (channels 1 & 2 — bank 0)
            0x30..=0xFF => self.fm.write_reg(addr, val, false),
            _ => {}
        }
    }

    fn write_reg_b(&mut self, addr: u8, val: u8) {
        self.regs_b[addr as usize] = val;
        // ADPCM-A registers ($00..$2F)
        if addr < 0x30 {
            self.write_adpcm_a(addr, val);
            return;
        }
        // FM operator registers (channels 3 & 4 — bank 1)
        self.fm.write_reg(addr, val, true);
    }

    fn write_adpcm_a(&mut self, addr: u8, val: u8) {
        match addr {
            0x00 => {
                // YM2610 ADPCM-A control. bit 7 = dump (key off when set),
                // bits 5..0 = per-channel key-on mask.
                // Reference: ymfm `adpcm_a_engine::write` + Yamaha datasheet.
                if val & 0x80 != 0 {
                    for ch in 0..6 {
                        if val & (1 << ch) != 0 { self.adpcm_a[ch].key_off(); }
                    }
                } else {
                    for ch in 0..6 {
                        if val & (1 << ch) != 0 {
                            self.adpcm_a[ch].key_on();
                            self.dbg_adpcma_keyon += 1;
                            self.dbg_adpcma_keyon_ch[ch] += 1;
                        }
                    }
                }
            }
            0x01 => {
                // ADPCM-A master Total Level (TL). FBNeo applies `val & 0x3F)
                // ^ 0x3F` to invert the polarity (0 = loudest, 63 = silent).
                // We store the pre-inverted value so `volume = TL + IL` reads
                // identically to FBNeo's `volume = F2610->adpcmTL + IL`.
                self.adpcma_tl = (val & 0x3F) ^ 0x3F;
                // A master-TL change requires recomputing every channel's
                // cached `adpcm_out` so silent ticks don't keep emitting the
                // previous (louder) value (FBNeo `FM_ADPCMAWrite` case 0x01).
                let tl = self.adpcma_tl;
                for ch in 0..6 { self.adpcm_a[ch].refresh_volume(tl); }
            }
            0x08..=0x0D => {
                // Per-channel: IL (bits 0..=4) + pan-L (bit 7) + pan-R (bit 6).
                // FBNeo also XORs the IL with 0x1F (0 = loudest, 31 = silent),
                // and ymfm uses the pre-inverted value when summing with TL.
                let ch = (addr - 0x08) as usize;
                self.adpcm_a[ch].il = (val & 0x1F) ^ 0x1F;
                self.adpcm_a[ch].pan = val & 0xC0;
                let tl = self.adpcma_tl;
                self.adpcm_a[ch].refresh_volume(tl);
            }
            0x10..=0x15 => {
                let ch = (addr - 0x10) as usize;
                let lo = val as u32;
                let hi = self.regs_b[(0x18 + ch) as usize] as u32;
                self.adpcm_a[ch].start = ((hi << 8) | lo) << 8;
            }
            0x18..=0x1D => {
                let ch = (addr - 0x18) as usize;
                let lo = self.regs_b[(0x10 + ch) as usize] as u32;
                let hi = val as u32;
                self.adpcm_a[ch].start = ((hi << 8) | lo) << 8;
            }
            0x20..=0x25 => {
                let ch = (addr - 0x20) as usize;
                let lo = val as u32;
                let hi = self.regs_b[(0x28 + ch) as usize] as u32;
                self.adpcm_a[ch].end = (((hi << 8) | lo) << 8) | 0xFF;
            }
            0x28..=0x2D => {
                let ch = (addr - 0x28) as usize;
                let lo = self.regs_b[(0x20 + ch) as usize] as u32;
                let hi = val as u32;
                self.adpcm_a[ch].end = (((hi << 8) | lo) << 8) | 0xFF;
            }
            _ => {}
        }
    }

    /// Advance Timer A/B by one output sample.  Returns true if a new IRQ
    /// edge was generated this sample.
    fn tick_timers(&mut self) -> bool {
        let mut new_irq = false;
        if self.timer_a_enabled {
            self.timer_a_count -= 1;
            if self.timer_a_count <= 0 {
                self.timer_a_count += (1024 - self.timer_a_period as i32).max(1);
                if self.irq_enable & 0x04 != 0 && (self.status_a & 0x01) == 0 {
                    self.status_a |= 0x01;
                    new_irq = true;
                }
            }
        }
        if self.timer_b_enabled {
            self.timer_b_count -= 1;
            if self.timer_b_count <= 0 {
                self.timer_b_count += ((256 - self.timer_b_period as i32) * 16).max(1);
                if self.irq_enable & 0x08 != 0 && (self.status_a & 0x02) == 0 {
                    self.status_a |= 0x02;
                    new_irq = true;
                }
            }
        }
        if new_irq { self.irq_out = true; self.dbg_timer_flag_set += 1; }
        new_irq
    }

    /// Generate one stereo sample at ~55,555 Hz native rate.
    /// Returns `(left, right)` as i16.
    ///
    /// Mixing follows MAME's `neogeo_stereo` machine config
    /// (`src/mame/snk/neogeo.cpp`, route gains `add_route`):
    ///
    /// ```text
    /// m_ym->add_route(0, "speaker", 0.84, 0);   // SSG  -> L
    /// m_ym->add_route(0, "speaker", 0.84, 1);   // SSG  -> R
    /// m_ym->add_route(1, "speaker", 0.98, 0);   // FM/ADPCM L
    /// m_ym->add_route(2, "speaker", 0.98, 1);   // FM/ADPCM R
    /// ```
    ///
    /// We keep all stages in i32 and apply the gains as exact integer ratios
    /// (`*84/100` and `*98/100`) on the final stereo sum, then clamp to i16.
    pub fn step_one_sample(&mut self) -> (i16, i16) {
        let _ = self.tick_timers();

        // ADPCM-A clocking. Native rate is master/432 ≈ 18.518 kHz, host
        // mixes at master/144 ≈ 55.555 kHz, so the nibble advance ratio is
        // exactly 3.
        self.adpcm_a_frac = self.adpcm_a_frac.wrapping_add(1);
        let advance_a = self.adpcm_a_frac >= 3;
        if advance_a { self.adpcm_a_frac = 0; }

        // ADPCM-A mixing. FBNeo accuracy upgrade (v33):
        // We no longer recompute volume on every host tick. Instead each
        // channel holds a cached `adpcm_out = (acc * vol_mul) >> vol_shift
        // & ~3` that is refreshed *only* when a nibble is decoded or the
        // driver writes to TL/IL/PAN registers (see `refresh_volume`).
        // Between nibble events the chip's analog output stays at this
        // value (track-and-hold), exactly like FBNeo's `*ch->pan +=
        // ch->adpcm_out` in `ADPCMA_calc_chan`.
        let mut fm_left: i32  = 0;
        let mut fm_right: i32 = 0;

        for ch_idx in 0..6 {
            let was_on = self.adpcm_a[ch_idx].on;
            if advance_a {
                self.adpcm_a[ch_idx].step_nibble(&self.adpcm_a_rom);
            }
            let scaled = self.adpcm_a[ch_idx].adpcm_out;
            let pan = self.adpcm_a[ch_idx].pan;
            // bit 7 = pan left, bit 6 = pan right (ymfm convention).
            if pan & 0x80 != 0 { fm_left  += scaled; }
            if pan & 0x40 != 0 { fm_right += scaled; }
            if scaled != 0 {
                self.dbg_adpcma_nz += 1;
                self.dbg_adpcma_nz_ch[ch_idx] += 1;
            }
            if was_on && !self.adpcm_a[ch_idx].on {
                self.adpcm_a_end_flags |= 1 << ch_idx;
            }
        }

        // ADPCM-B (Delta-T). ymfm mixes with `(sample * volume) >> 9`
        // (`>> 8` for volume, `>> 1` for the YM2610's rshift=1).
        let b = self.adpcm_b.step_one(&self.adpcm_b_rom);
        let bv = self.adpcm_b.volume as i32;
        let b_scaled = (b * bv) >> 9;
        if self.adpcm_b.pan & 0x80 != 0 { fm_left  += b_scaled; }
        if self.adpcm_b.pan & 0x40 != 0 { fm_right += b_scaled; }
        if b_scaled != 0 { self.dbg_adpcmb_nz += 1; }

        // FM channels 1,2,4,5 with per-channel telemetry. `step_one_with_per_channel`
        // already applies the YM2610 `rshift=1` (`carrier >> 1`) inside the
        // per-channel loop and clips each accumulator per MAME's ymfm.
        let (fm_l, fm_r, per_ch) = self.fm.step_one_with_per_channel();
        fm_left  += fm_l;
        fm_right += fm_r;
        if fm_l != 0 || fm_r != 0 { self.dbg_fm_nz += 1; }
        for c in 0..4 {
            if per_ch[c] != 0 { self.dbg_fm_nz_ch[c] += 1; }
        }

        // SSG: mono (Neo Geo wires the SSG analog output to both speakers
        // through the YM3016 mixer). The raw SSG output is on roughly the
        // same scale as the FM mix; we keep it as-is and only apply route
        // gain 0.84 below.
        let ssg = self.ssg.step_one();
        if ssg != 0 { self.dbg_ssg_nz += 1; }

        // Apply MAME `add_route` gains (0.84 for SSG, 0.98 for FM/ADPCM)
        // as exact integer ratios to keep the arithmetic in i64 to avoid
        // overflow before the final clamp.
        let left_i64  = (fm_left  as i64) * 98 / 100 + (ssg as i64) * 84 / 100;
        let right_i64 = (fm_right as i64) * 98 / 100 + (ssg as i64) * 84 / 100;

        let l = left_i64.clamp(-32768, 32767) as i16;
        let r = right_i64.clamp(-32768, 32767) as i16;
        (l, r)
    }

    /// Read status register (busy + timer overflows + IRQ).
    pub fn read_status(&self) -> u8 {
        self.status_with_busy()
    }

    /// Render `n` stereo samples. Helper used when the host wants a
    /// fixed buffer rather than per-sample iteration.
    pub fn render(&mut self, out_l: &mut [i16], out_r: &mut [i16]) {
        let n = out_l.len().min(out_r.len());
        for i in 0..n {
            let (l, r) = self.step_one_sample();
            out_l[i] = l;
            out_r[i] = r;
        }
    }
}

// ===========================================================================
//   FM — 4-channel OPN approximation
// ===========================================================================
//
// Pragmatic FM synthesiser: 4 channels x 2 operators (modulator + carrier)
// per channel.  This is a heavily simplified take on the Yamaha OPN engine
// — enough to make Metal Slug's BGM/SFX audible without porting the full
// 1500-LOC `fm.c`.  Algorithm 0 (carrier modulated by feedback-modulator)
// is approximated for every channel.  Pan + key-on/off + total-level (TL)
// are honoured.  ADSR is reduced to a two-stage env (attack → sustain
// release).
//
// Native chip math:
//   f_phase_per_sec  =  fnum * 2^block * F_MASTER / 144  ~= fnum * 2^block * 8e6/144
//   sample_rate      =  master / 144 = ~55_555 Hz
//   phase_step_q16   =  (fnum << (block+5)) >> 1   (compromise calibrated by ear)
//
// References: FBNeo `fm.c` op_calc, MAME `ymopn.cpp`.

// =====================================================================
//  OPN DSP tables — verbatim port of FBNeo `fm.c::init_tables`.
//  References:
//    crates/../src/ref/FBNeo/src/burn/snd/fm.c  (Jarek Burczynski / MAME)
//  Constants:
//    ENV_BITS=10, ENV_LEN=1024, MAX_ATT=1023
//    SIN_BITS=10, SIN_LEN=1024
//    TL_RES_LEN=256 (8-bit), TL_TAB_LEN=13*2*256=6656
//    FREQ_SH=16  (Q16 phase), EG_SH=16, RATE_STEPS=8
//  States: 0=OFF, 1=REL, 2=SUS, 3=DEC, 4=ATT (matches FBNeo EG_OFF..EG_ATT).
// =====================================================================

const ENV_BITS: u32 = 10;
const ENV_LEN: i32  = 1 << ENV_BITS;             // 1024
const MAX_ATT_INDEX: i32 = ENV_LEN - 1;          // 1023
const MIN_ATT_INDEX: i32 = 0;
const SIN_LEN: usize = 1024;
const SIN_MASK: usize = SIN_LEN - 1;
const TL_RES_LEN: usize = 256;
const TL_TAB_LEN: usize = 13 * 2 * TL_RES_LEN;   // 6656
const FREQ_SH: u32 = 16;
const FREQ_MASK: u32 = (1u32 << FREQ_SH) - 1;
const RATE_STEPS: usize = 8;
const ENV_QUIET: u32 = (TL_TAB_LEN as u32) >> 3; // 832

// EG states (FBNeo numbering: 0=OFF, 1=REL, 2=SUS, 3=DEC, 4=ATT).
const EG_OFF: u8 = 0;
const EG_REL: u8 = 1;
const EG_SUS: u8 = 2;
const EG_DEC: u8 = 3;
const EG_ATT: u8 = 4;

/// FBNeo `tl_tab`: 13*2*256 entries. Signed amplitude (±8192 max).
/// Computed from `m = floor((1<<16)/2^((x+1)*ENV_STEP/4/8))` then folded into
/// the chip's 13-bit-signed amplitude format with positive/negative mirror.
static TL_TAB: once_cell::sync::Lazy<[i32; TL_TAB_LEN]> = once_cell::sync::Lazy::new(|| {
    // ENV_STEP = 128.0/1024 = 0.125 dB per unit -> ENV_STEP/4 = 0.03125
    let env_step: f64 = 128.0 / (ENV_LEN as f64);
    let mut tab = [0i32; TL_TAB_LEN];
    for x in 0..TL_RES_LEN {
        let m = (1u64 << 16) as f64 / 2f64.powf(((x as f64) + 1.0) * env_step / 4.0 / 8.0);
        let m = m.floor() as i64;
        let mut n = m as i32;
        n >>= 4;                  // 12 bits
        if (n & 1) != 0 { n = (n >> 1) + 1; } else { n >>= 1; }
        n <<= 2;                  // 13 bits
        tab[x * 2 + 0] = n;
        tab[x * 2 + 1] = -n;
        for i in 1..13 {
            tab[x * 2 + 0 + i * 2 * TL_RES_LEN] =  tab[x * 2] >> i;
            tab[x * 2 + 1 + i * 2 * TL_RES_LEN] = -tab[x * 2 + 0 + i * 2 * TL_RES_LEN];
        }
    }
    tab
});

/// FBNeo `sin_tab`: 1024 entries indexing into TL_TAB. Encodes the log-sin
/// (amplitude in 1/256 dB units) plus the sign bit. Indexed by phase«10.
static SIN_TAB: once_cell::sync::Lazy<[u32; SIN_LEN]> = once_cell::sync::Lazy::new(|| {
    let env_step: f64 = 128.0 / (ENV_LEN as f64);
    let mut tab = [0u32; SIN_LEN];
    for i in 0..SIN_LEN {
        // sin( ((i*2)+1) * pi / SIN_LEN ) avoids hitting zero.
        let m = ((((i * 2) + 1) as f64) * std::f64::consts::PI / (SIN_LEN as f64)).sin();
        let am = if m > 0.0 { 8.0 * (1.0 / m).ln() / 2f64.ln() }
                 else        { 8.0 * (-1.0 / m).ln() / 2f64.ln() };
        let o = am / (env_step / 4.0);
        let mut n = (2.0 * o) as i32;
        if (n & 1) != 0 { n = (n >> 1) + 1; } else { n >>= 1; }
        tab[i] = (n as u32) * 2 + if m >= 0.0 { 0 } else { 1 };
    }
    tab
});

/// `eg_inc[19*8]` — envelope rate increment table (FBNeo verbatim).
static EG_INC: [u8; 19 * RATE_STEPS] = [
    0,1, 0,1, 0,1, 0,1,   /* rates 00..11 0 (increment by 0 or 1) */
    0,1, 0,1, 1,1, 0,1,   /* rates 00..11 1 */
    0,1, 1,1, 0,1, 1,1,   /* rates 00..11 2 */
    0,1, 1,1, 1,1, 1,1,   /* rates 00..11 3 */
    1,1, 1,1, 1,1, 1,1,   /* rate 12 0 */
    1,1, 1,2, 1,1, 1,2,   /* rate 12 1 */
    1,2, 1,2, 1,2, 1,2,   /* rate 12 2 */
    1,2, 2,2, 1,2, 2,2,   /* rate 12 3 */
    2,2, 2,2, 2,2, 2,2,   /* rate 13 0 */
    2,2, 2,4, 2,2, 2,4,   /* rate 13 1 */
    2,4, 2,4, 2,4, 2,4,   /* rate 13 2 */
    2,4, 4,4, 2,4, 4,4,   /* rate 13 3 */
    4,4, 4,4, 4,4, 4,4,   /* rate 14 0 */
    4,4, 4,8, 4,4, 4,8,   /* rate 14 1 */
    4,8, 4,8, 4,8, 4,8,   /* rate 14 2 */
    4,8, 8,8, 4,8, 8,8,   /* rate 14 3 */
    8,8, 8,8, 8,8, 8,8,   /* rate 15 */
    16,16,16,16,16,16,16,16, /* attack 15 */
    0,0, 0,0, 0,0, 0,0,   /* infinity rates */
];

/// `eg_rate_select[32+64+32]=[128]` — FBNeo verbatim.
/// Stored as already-multiplied-by-RATE_STEPS for direct addressing.
/// Pre-multiplied by RATE_STEPS=8 here.
static EG_RATE_SELECT: [u32; 128] = [
    // 32 infinite time rates (= 18 * 8 = 144)
    144,144,144,144,144,144,144,144,
    144,144,144,144,144,144,144,144,
    144,144,144,144,144,144,144,144,
    144,144,144,144,144,144,144,144,
    // rates 00-11 (48 entries) -> 0,8,16,24
      0,  8, 16, 24,    0,  8, 16, 24,    0,  8, 16, 24,
      0,  8, 16, 24,    0,  8, 16, 24,    0,  8, 16, 24,
      0,  8, 16, 24,    0,  8, 16, 24,    0,  8, 16, 24,
      0,  8, 16, 24,    0,  8, 16, 24,    0,  8, 16, 24,
    // rate 12,13,14,15 (16 entries)
     32, 40, 48, 56,   64, 72, 80, 88,   96,104,112,120,
    128,128,128,128,
    // 32 dummy rates same as 15 3 = 128
    128,128,128,128,128,128,128,128,
    128,128,128,128,128,128,128,128,
    128,128,128,128,128,128,128,128,
    128,128,128,128,128,128,128,128,
];

/// `eg_rate_shift[32+64+32]=[128]` — FBNeo verbatim.
static EG_RATE_SHIFT: [u32; 128] = [
    // 32 infinite time rates
    0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,
    // rates 00-11 (48 entries: 12 × 4)
    11,11,11,11, 10,10,10,10, 9,9,9,9, 8,8,8,8,
     7, 7, 7, 7,  6, 6, 6, 6, 5,5,5,5, 4,4,4,4,
     3, 3, 3, 3,  2, 2, 2, 2, 1,1,1,1, 0,0,0,0,
    // rate 12, 13, 14, 15 (16 entries: 4 × 4)
     0,0,0,0,  0,0,0,0,  0,0,0,0,  0,0,0,0,
    // 32 dummy rates same as 15 3
     0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,
     0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,
];

/// `sl_table[16]` — sustain-level lookup, in env units (1/8 dB).
static SL_TABLE: once_cell::sync::Lazy<[u32; 16]> = once_cell::sync::Lazy::new(|| {
    // SC(db) = db * (4.0/ENV_STEP); ENV_STEP=128/1024=0.125 -> SC(db) = db*32.
    // Map: SL nibble 0..14 -> 0..14 ·3dB; SL nibble 15 -> 31 ·3dB.
    let env_step: f64 = 128.0 / (ENV_LEN as f64);
    let sc = |db: f64| -> u32 { (db * (4.0 / env_step)) as u32 };
    [ sc( 0.0), sc( 1.0), sc( 2.0), sc( 3.0),
      sc( 4.0), sc( 5.0), sc( 6.0), sc( 7.0),
      sc( 8.0), sc( 9.0), sc(10.0), sc(11.0),
      sc(12.0), sc(13.0), sc(14.0), sc(31.0) ]
});

/// FBNeo detune base table (4*32). Expanded below to signed phase-step offsets.
const DT_TAB_BASE: [u8; 4 * 32] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,1,1,1,1,1,1,1,1,2,2,2,2,2,3,3,3,4,4,4,5,5,6,6,7,8,8,8,8,
    1,1,1,1,2,2,2,2,2,3,3,3,4,4,4,5,5,6,6,7,8,8,9,10,11,12,13,14,16,16,16,16,
    2,2,2,2,2,3,3,3,4,4,4,5,5,6,6,7,8,8,9,10,11,12,13,14,16,17,19,20,22,22,22,22,
];

/// Approximate FBNeo `dt_tab` in this core's Q16 phase domain.
/// With output rate fixed to the YM2610 native rate, FBNeo's init scales the
/// raw table by roughly 64 before applying it to the phase step.
static DT_TAB: once_cell::sync::Lazy<[[i32; 32]; 8]> = once_cell::sync::Lazy::new(|| {
    let mut out = [[0i32; 32]; 8];
    for d in 0..4 {
        for i in 0..32 {
            let v = (DT_TAB_BASE[d * 32 + i] as i32) * 64;
            out[d][i] = v;
            out[d + 4][i] = -v;
        }
    }
    out
});


const FN_MAX: u32 = 0x10000000;
const LFO_SAMPLES_PER_STEP: [u32; 8] = [108, 77, 71, 67, 62, 44, 8, 5];
const LFO_AMS_DEPTH_SHIFT: [u8; 4] = [8, 3, 1, 0];
static LFO_PM_OUTPUT: [[u8; 8]; 56] = [
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  1,  1,  1,  1],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  1,  1,  1,  1],
    [ 0,  0,  1,  1,  2,  2,  2,  3],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  1],
    [ 0,  0,  0,  0,  1,  1,  1,  1],
    [ 0,  0,  1,  1,  2,  2,  2,  3],
    [ 0,  0,  2,  3,  4,  4,  5,  6],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  0,  0,  1,  1],
    [ 0,  0,  0,  0,  1,  1,  1,  1],
    [ 0,  0,  0,  1,  1,  1,  1,  2],
    [ 0,  0,  1,  1,  2,  2,  2,  3],
    [ 0,  0,  2,  3,  4,  4,  5,  6],
    [ 0,  0,  4,  6,  8,  8, 10, 12],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  1,  1,  1,  1],
    [ 0,  0,  0,  1,  1,  1,  2,  2],
    [ 0,  0,  1,  1,  2,  2,  3,  3],
    [ 0,  0,  1,  2,  2,  2,  3,  4],
    [ 0,  0,  2,  3,  4,  4,  5,  6],
    [ 0,  0,  4,  6,  8,  8, 10, 12],
    [ 0,  0,  8, 12, 16, 16, 20, 24],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  2,  2,  2,  2],
    [ 0,  0,  0,  2,  2,  2,  4,  4],
    [ 0,  0,  2,  2,  4,  4,  6,  6],
    [ 0,  0,  2,  4,  4,  4,  6,  8],
    [ 0,  0,  4,  6,  8,  8, 10, 12],
    [ 0,  0,  8, 12, 16, 16, 20, 24],
    [ 0,  0, 16, 24, 32, 32, 40, 48],
    [ 0,  0,  0,  0,  0,  0,  0,  0],
    [ 0,  0,  0,  0,  4,  4,  4,  4],
    [ 0,  0,  0,  4,  4,  4,  8,  8],
    [ 0,  0,  4,  4,  8,  8, 12, 12],
    [ 0,  0,  4,  8,  8,  8, 12, 16],
    [ 0,  0,  8, 12, 16, 16, 20, 24],
    [ 0,  0, 16, 24, 32, 32, 40, 48],
    [ 0,  0, 32, 48, 64, 64, 80, 96],
];

#[inline]
fn lfo_pm_offset(block_fnum: u16, pms: u8, lfo_pm: u8) -> i32 {
    if pms == 0 {
        return 0;
    }
    let fnum = (((block_fnum as u32) & 0x07f0) >> 4) as u8;
    let depth = (pms & 0x07) as usize;
    let phase = (lfo_pm & 31) as usize;
    let (step, sign) = match phase {
        0..=7 => (phase, 1),
        8..=15 => ((phase - 8) ^ 7, 1),
        16..=23 => (phase - 16, -1),
        _ => (((phase - 24) ^ 7), -1),
    };
    let mut value = 0i32;
    for bit in 0..7usize {
        if (fnum & (1 << bit)) != 0 {
            value += LFO_PM_OUTPUT[bit * 8 + depth][step] as i32;
        }
    }
    value * sign
}

#[inline]
fn lfo_am_attenuation(am_enabled: bool, ams_shift: u8, lfo_am: u32) -> u32 {
    if am_enabled { lfo_am >> ams_shift } else { 0 }
}

#[derive(Debug, Clone, Copy)]
struct FmOp {
    /// DT/MUL register $30..$3F  — bits 0..3 = MUL multiplier (0→15, 0 = 0.5).
    mul: u8,
    /// Detune table selector (0..7, where 4..7 are negative detune).
    dt: u8,
    /// Total Level $40..$4F, already left-shifted by `ENV_BITS-7` = <<3.
    tl: u32,
    /// AR rate (effective 0 or 32+(v<<1)).
    ar: u32,
    /// DR rate (effective 0 or 32+(v<<1)).
    dr: u32,
    /// SR rate (effective 0 or 32+(v<<1)).
    sr: u32,
    /// RR rate (34 + (v<<2)).
    rr: u32,
    /// Sustain level (env units).
    sl: u32,
    /// KSR shift amount (3 - (ar_reg >> 6)).
    ksr_shift: u32,
    /// KSR offset added to AR/DR/SR/RR when selecting eg rate.
    ksr: u32,
    /// Current phase counter (Q16).
    phase: u32,
    /// Volume (env current attenuation, 0..1023).
    volume: i32,
    /// EG state (EG_OFF/REL/SUS/DEC/ATT).
    state: u8,
    /// EG counter shift cached per stage.
    eg_sh_ar: u32,
    eg_sh_d1r: u32,
    eg_sh_d2r: u32,
    eg_sh_rr: u32,
    /// EG rate-select offset into EG_INC table.
    eg_sel_ar: u32,
    eg_sel_d1r: u32,
    eg_sel_d2r: u32,
    eg_sel_rr: u32,
    /// Amplitude-modulation enable (bit 7 of $60-$6F).
    am_enabled: bool,
    /// Latched key state for FM_KEYON / FM_KEYOFF detection.
    key: bool,
    /// SSG-EG register $90-$9F low nibble: bit3=enable, bit2=attack,
    /// bit1=alternate, bit0=hold (FBNeo `SLOT->ssg`).
    ssg: u8,
    /// SSG-EG negation latch (FBNeo `SLOT->ssgn`). Bit 1 set = output
    /// currently inverted; bit 0 = "swapped once" marker for hold shapes.
    ssgn: u8,
}

impl FmOp {
    const fn new() -> Self {
        Self {
            mul: 1, dt: 0, tl: 0, ar: 0, dr: 0, sr: 0, rr: 34, sl: 0,
            ksr_shift: 3, ksr: 0,
            phase: 0, volume: MAX_ATT_INDEX,
            state: EG_OFF,
            eg_sh_ar: 0, eg_sh_d1r: 0, eg_sh_d2r: 0, eg_sh_rr: 0,
            eg_sel_ar: 17 * (RATE_STEPS as u32),
            eg_sel_d1r: 17 * (RATE_STEPS as u32),
            eg_sel_d2r: 17 * (RATE_STEPS as u32),
            eg_sel_rr: 17 * (RATE_STEPS as u32),
            am_enabled: false,
            key: false,
            ssg: 0,
            ssgn: 0,
        }
    }

    /// Refresh cached `eg_sh_*` / `eg_sel_*` based on AR/DR/SR/RR + KSR.
    fn refresh_eg_rates(&mut self) {
        if (self.ar + self.ksr) < 32 + 62 {
            self.eg_sh_ar  = EG_RATE_SHIFT [(self.ar + self.ksr) as usize] as u32;
            self.eg_sel_ar = EG_RATE_SELECT[(self.ar + self.ksr) as usize] as u32;
        } else {
            self.eg_sh_ar  = 0;
            self.eg_sel_ar = 17 * (RATE_STEPS as u32);
        }
        self.eg_sh_d1r = EG_RATE_SHIFT [(self.dr + self.ksr) as usize];
        self.eg_sh_d2r = EG_RATE_SHIFT [(self.sr + self.ksr) as usize];
        self.eg_sh_rr  = EG_RATE_SHIFT [(self.rr + self.ksr) as usize];
        self.eg_sel_d1r= EG_RATE_SELECT[(self.dr + self.ksr) as usize];
        self.eg_sel_d2r= EG_RATE_SELECT[(self.sr + self.ksr) as usize];
        self.eg_sel_rr = EG_RATE_SELECT[(self.rr + self.ksr) as usize];
    }

    #[inline]
    fn detune(&self, kc: u32) -> i32 {
        DT_TAB[self.dt as usize][(kc & 31) as usize]
    }

    /// FBNeo `FM_KEYON`: restart phase & enter attack.
    fn key_on(&mut self) {
        if !self.key {
            self.key = true;
            self.phase = 0;
            // FBNeo fm.c L940: re-arm SSG-EG inversion latch from the attack
            // bit of the shape ($90 bit 2) on every key-on.
            self.ssgn = (self.ssg & 0x04) >> 1;
            self.state = EG_ATT;
        }
    }
    /// FBNeo `FM_KEYOFF`: unconditionally move to release when transitioning
    /// from key-on to key-off. ymfm `fm_operator::keyonoff` re-enters release
    /// regardless of the current EG state, so a keyoff issued *during* the
    /// attack phase also silences the operator (the previous `> EG_REL`
    /// guard blocked this transition when state was EG_SUS/EG_DEC/EG_ATT).
    fn key_off(&mut self) {
        if self.key {
            self.key = false;
            // Only skip release if we're already off (env at max attenuation).
            if self.state != EG_OFF { self.state = EG_REL; }
        }
    }

    /// FBNeo `advance_eg_channel` (per slot). `eg_cnt` is the chip global env
    /// counter (incremented every `eg_timer_overflow` chip samples).
    ///
    /// Includes the full SSG-EG state machine (fm.c L1240-1416, the
    /// non-YM2612/YM2608 branch used for the YM2610): when SSG-EG is enabled
    /// ($90 bit 3) decay and sustain run 4x faster, and when the envelope
    /// reaches ENV_QUIET during sustain the shape either holds (bit 0),
    /// alternates inversion (bit 1) and/or repeats by restarting the phase
    /// generator with volume=511 in attack state.
    fn advance_eg(&mut self, eg_cnt: u32) {
        let ssg_on = (self.ssg & 0x08) != 0;
        let mut swap_flag: u8 = 0;
        match self.state {
            EG_ATT => {
                let mask = (1u32 << self.eg_sh_ar) - 1;
                if (eg_cnt & mask) == 0 {
                    let step_idx = self.eg_sel_ar + ((eg_cnt >> self.eg_sh_ar) & 7);
                    let inc = EG_INC[step_idx as usize] as i32;
                    // FBNeo: volume += (~volume * eg_inc[...]) >> 4
                    self.volume += (!self.volume * inc) >> 4;
                    if self.volume <= MIN_ATT_INDEX {
                        self.volume = MIN_ATT_INDEX;
                        self.state = EG_DEC;
                    }
                }
            }
            EG_DEC => {
                let mask = (1u32 << self.eg_sh_d1r) - 1;
                if (eg_cnt & mask) == 0 {
                    let step_idx = self.eg_sel_d1r + ((eg_cnt >> self.eg_sh_d1r) & 7);
                    let inc = EG_INC[step_idx as usize] as i32;
                    // SSG-EG: decay runs 4x faster on YM2610 (fm.c L1284).
                    self.volume += if ssg_on { 4 * inc } else { inc };
                    if self.volume >= self.sl as i32 {
                        // FBNeo non-2612/2608 branch does NOT clamp volume to
                        // SL here, it only changes state (fm.c L1286/L1296).
                        self.state = EG_SUS;
                    }
                }
            }
            EG_SUS => {
                let mask = (1u32 << self.eg_sh_d2r) - 1;
                if ssg_on {
                    if (eg_cnt & mask) == 0 {
                        let step_idx = self.eg_sel_d2r + ((eg_cnt >> self.eg_sh_d2r) & 7);
                        // 4x faster on YM2610 (fm.c L1315).
                        self.volume += 4 * EG_INC[step_idx as usize] as i32;
                        if self.volume >= ENV_QUIET as i32 {
                            // Non-2612/2608: snap straight to max attenuation.
                            self.volume = MAX_ATT_INDEX;
                            if (self.ssg & 0x01) != 0 {
                                // Hold shape: swap once (recording it in bit
                                // 0 of ssgn), then keep the level forever.
                                if (self.ssgn & 1) == 0 {
                                    swap_flag = (self.ssg & 0x02) | 1;
                                }
                            } else {
                                // Repeat shape: same as KEY-ON — restart the
                                // phase generator and re-enter attack with
                                // volume=511 (fm.c L1355-1358).
                                self.phase = 0;
                                self.volume = 511;
                                self.state = EG_ATT;
                                swap_flag = self.ssg & 0x02;
                            }
                        }
                    }
                } else if (eg_cnt & mask) == 0 {
                    let step_idx = self.eg_sel_d2r + ((eg_cnt >> self.eg_sh_d2r) & 7);
                    self.volume += EG_INC[step_idx as usize] as i32;
                    if self.volume >= MAX_ATT_INDEX { self.volume = MAX_ATT_INDEX; }
                }
            }
            EG_REL => {
                let mask = (1u32 << self.eg_sh_rr) - 1;
                if (eg_cnt & mask) == 0 {
                    let step_idx = self.eg_sel_rr + ((eg_cnt >> self.eg_sh_rr) & 7);
                    self.volume += EG_INC[step_idx as usize] as i32;
                    if self.volume >= MAX_ATT_INDEX {
                        self.volume = MAX_ATT_INDEX;
                        self.state = EG_OFF;
                    }
                }
            }
            _ => {}
        }
        // Reverse the inversion latch after this step (fm.c L1418).
        self.ssgn ^= swap_flag;
    }

    /// FBNeo `volume_calc(SLOT)` simplified: env attenuation + TL, clipped.
    /// Returns combined attenuation 0..1023 (0 loudest).
    ///
    /// When SSG-EG is enabled and the negation latch (`ssgn` bit 1) is set
    /// while the envelope is active (state > EG_REL), the raw attenuation is
    /// bitwise-inverted (fm.c L1410-1411) which produces the characteristic
    /// inverted-sawtooth shapes.
    #[inline]
    fn vol_out(&self) -> u32 {
        let mut v = self.volume.max(MIN_ATT_INDEX).min(MAX_ATT_INDEX) as u32;
        if (self.ssg & 0x08) != 0 && (self.ssgn & 0x02) != 0 && self.state > EG_REL {
            v ^= MAX_ATT_INDEX as u32;
        }
        v + self.tl
    }

    /// FBNeo `op_calc(phase, env, pm)`: log-sin + tl_tab lookup.
    /// `pm` (phase modulator) is already 13-bit signed amplitude (±8192).
    #[inline]
    fn op_calc(phase: u32, env: u32, pm: i32) -> i32 {
        // p = (env<<3) + sin_tab[ ((phase & ~FREQ_MASK) + (pm<<15)) >> FREQ_SH & SIN_MASK ]
        let pm_off = (pm as i32).wrapping_shl(15);
        let idx = ((((phase & !FREQ_MASK) as i32).wrapping_add(pm_off) as u32) >> FREQ_SH) as usize & SIN_MASK;
        let p = (env << 3).wrapping_add(SIN_TAB[idx]);
        if (p as usize) >= TL_TAB_LEN { 0 } else { TL_TAB[p as usize] }
    }

    /// FBNeo `op_calc1`: like op_calc but uses `pm` without the <<15 shift
    /// (used only for OP1 feedback path).
    #[inline]
    fn op_calc1(phase: u32, env: u32, pm: i32) -> i32 {
        let idx = ((((phase & !FREQ_MASK) as i32).wrapping_add(pm) as u32) >> FREQ_SH) as usize & SIN_MASK;
        let p = (env << 3).wrapping_add(SIN_TAB[idx]);
        if (p as usize) >= TL_TAB_LEN { 0 } else { TL_TAB[p as usize] }
    }
}

#[derive(Debug, Clone, Copy)]
struct FmCh {
    op: [FmOp; 4],
    /// FNUM (11 bits) + BLOCK (3 bits): block in bits 13..11.
    fnum_block: u16,
    /// Algorithm (0..7).
    alg: u8,
    /// Feedback (3 bits).
    fb: u8,
    /// Left/Right output enable (bit 7 = L, bit 6 = R per OPN convention).
    /// On OPN they are bits 7 = L ("FL"), 6 = R ("FR") of the $B4 register.
    pan: u8,
    /// Channel AMS shift and PMS depth from $B4-$B6.
    ams: u8,
    pms: u8,
    /// Previous op0 outputs (for feedback).
    op0_prev: [i32; 2],
    /// Delayed MEM value used by algorithms 0..3/5.
    mem_value: i32,
    /// Latched 3F\xn block/fnum hi for the next fnum write.
    fnum_latch_hi: u8,
    /// Active (any operator has env<1023).
    active: bool,
}

impl FmCh {
    const fn new() -> Self {
        Self {
            op: [FmOp::new(); 4],
            fnum_block: 0,
            alg: 0,
            fb: 0,
            pan: 0xC0,
            ams: 8,
            pms: 0,
            op0_prev: [0; 2],
            mem_value: 0,
            fnum_latch_hi: 0,
            active: false,
        }
    }
    /// 5-bit keycode used for KSR / detune. Implements the MAME canonical
    /// formula (`ymfm_opn.cpp::opn_registers_base::cache_operator_data`):
    ///
    /// ```text
    /// keycode  = bitfield(block_freq, 10, 4) << 1;            // top4 << 1
    /// keycode |= bitfield(0xfe80, bitfield(block_freq, 7, 4)); // LSB from magic table
    /// ```
    ///
    /// `block_freq` here is the 14-bit `(block << 11) | fnum` register pair.
    fn kcode(&self) -> u32 {
        Self::kcode_from_block_freq(self.fnum_block)
    }

    fn kcode_from_block_freq(block_freq: u16) -> u32 {
        let block_freq = block_freq as u32;
        // Top 4 bits (bits 10..13 of block_freq) shifted left by 1.
        let kc_hi = ((block_freq >> 10) & 0x0F) << 1;
        // LSB lookup from the 16-bit magic word 0xfe80 indexed by bits 7..10.
        let idx = ((block_freq >> 7) & 0x0F) as u32;
        let kc_lo = (0xfe80u32 >> idx) & 1;
        kc_hi | kc_lo
    }
}

pub struct FmOpn {
    /// Six logical FM channel slots, matching the OPN-B/2610 register decode
    /// space (`val[0..1]` + `val[2]` bank bit). On real YM2610 only channels
    /// 1 and 2 of each bank are wired to the YM3016 DAC, but the chip's
    /// register decoder still services all six indices (ymfm `opn_registers_base<true>::operator_map`).
    ch: [FmCh; 6],
    /// Channel-3 special mode (register $27 bits 6..7 != 0): operators 2/3/4 of
    /// logical channel 2 get their own block+fnum pairs from $A8/$AC, $A9/$AD,
    /// $AA/$AE respectively. Operator 1 keeps the regular $A0/$A4 pair.
    ch2_multi_freq: bool,
    ch2_multi_block_freq: [u16; 3],
    /// Global envelope counter (FBNeo `OPN->eg_cnt`). Incremented every chip
    /// sample; `eg_timer` accumulator/overflow are folded into the host
    /// `step_one_with_per_channel` cadence which already runs at the chip
    /// sample rate (1 host sample = 1 chip sample for the OPN, 3 host samples
    /// = 1 EG counter tick on FBNeo's wiring).
    eg_cnt: u32,
    eg_timer: u32,
    lfo_enabled: bool,
    lfo_rate: u8,
    lfo_delay: u32,
    lfo_pos: u8,
    lfo_am: u32,
    lfo_pm: u8,
}

impl FmOpn {
    pub fn new() -> Self {
        Self {
            ch: [FmCh::new(); 6],
            ch2_multi_freq: false,
            ch2_multi_block_freq: [0; 3],
            eg_cnt: 0,
            eg_timer: 0,
            lfo_enabled: false,
            lfo_rate: 0,
            lfo_delay: 0,
            lfo_pos: 0,
            lfo_am: 0,
            lfo_pm: 0,
        }
    }

    pub fn write_lfo(&mut self, val: u8) {
        self.lfo_enabled = (val & 0x08) != 0;
        self.lfo_rate = val & 0x07;
        if !self.lfo_enabled {
            self.lfo_delay = 0;
            self.lfo_pos = 0;
            self.lfo_am = 0;
            self.lfo_pm = 0;
        }
    }

    fn clock_lfo(&mut self) {
        if self.lfo_enabled {
            let period = LFO_SAMPLES_PER_STEP[self.lfo_rate as usize];
            self.lfo_delay += 1;
            if self.lfo_delay >= period {
                self.lfo_delay = 0;
                self.lfo_pos = (self.lfo_pos + 1) & 0x7f;
            }
            let pos = self.lfo_pos as u32;
            self.lfo_am = if pos < 64 { (pos & 63) * 2 } else { 126 - ((pos & 63) * 2) };
            self.lfo_pm = (self.lfo_pos >> 2) & 31;
        } else {
            self.lfo_am = 0;
            self.lfo_pm = 0;
        }
    }

    fn update_channel_ksr(&mut self, c: usize) {
        let default_kc = self.ch[c].kcode();
        for op in 0..4 {
            let kc = if self.ch2_multi_freq && c == 2 && op != 0 {
                FmCh::kcode_from_block_freq(self.ch2_multi_block_freq[op - 1])
            } else {
                default_kc
            };
            let new_ksr = kc >> self.ch[c].op[op].ksr_shift;
            if new_ksr != self.ch[c].op[op].ksr {
                self.ch[c].op[op].ksr = new_ksr;
            }
            self.ch[c].op[op].refresh_eg_rates();
        }
    }

    #[cfg(test)]
    #[inline]
    fn block_freq_for_op(&self, c: usize, op: usize) -> u16 {
        if self.ch2_multi_freq && c == 2 && op != 0 {
            self.ch2_multi_block_freq[op - 1]
        } else {
            self.ch[c].fnum_block
        }
    }

    pub fn write_mode(&mut self, val: u8) {
        self.ch2_multi_freq = (val & 0xC0) != 0;
        self.update_channel_ksr(2);
    }

    pub fn key(&mut self, fm_ch: usize, slot_mask: u8) {
        let c = if fm_ch < self.ch.len() { fm_ch } else { return; };
        for op in 0..4 {
            if slot_mask & (1 << op) != 0 {
                self.ch[c].op[op].key_on();
            } else {
                self.ch[c].op[op].key_off();
            }
        }
        self.ch[c].active = slot_mask != 0;
        // Refresh KSR/eg rates for every slot of this channel (block/fnum may
        // have been written in between key events).
        self.update_channel_ksr(c);
    }

    /// `addr` is the **port** register address. `bank=false` selects FM1/FM2,
    /// `bank=true` selects FM3/FM4.  Within each bank, register $X0/$X4/$X8/$XC
    /// addresses op-1/2/3/4 of ch-0/1.
    pub fn write_reg(&mut self, addr: u8, val: u8, bank: bool) {
        // bank=false targets channels 0..=2 (port-A side: $30..$FF on $04/$05).
        // bank=true  targets channels 3..=5 (port-B side: $30..$FF on $06/$07).
        let bank_base = if bank { 3 } else { 0 };
        // Channel within bank: register low 2 bits. 3 is unused.
        let ch = (addr & 0x03) as usize;
        if ch >= 3 { return; }
        // OPN register slots are addressed in Yamaha's hardware order
        // x0,x4,x8,xC = SLOT1,SLOT3,SLOT2,SLOT4, while the synthesis path in
        // this file uses the natural operator order OP1,OP2,OP3,OP4.
        // Remap the raw register slot to the natural operator index.
        let raw_slot = ((addr >> 2) & 0x03) as usize;
        let op = match raw_slot {
            0 => 0, // SLOT1 -> OP1
            1 => 2, // SLOT3 -> OP3
            2 => 1, // SLOT2 -> OP2
            _ => 3, // SLOT4 -> OP4
        };
        let fm_ch = bank_base + ch;
        if fm_ch >= self.ch.len() { return; }
        let chan = &mut self.ch[fm_ch];
        let group = addr & 0xF0;
        match group {
            0x30 => {
                chan.op[op].mul = val & 0x0F;
                chan.op[op].dt = (val >> 4) & 0x07;
            }
            0x40 => {
                // FBNeo set_tl: SLOT->tl = (v&0x7f)<<(ENV_BITS-7)  -> <<3.
                chan.op[op].tl = ((val & 0x7F) as u32) << (ENV_BITS - 7);
            }
            0x50 => {
                // FBNeo set_ar_ksr: ar = (v&0x1f) ? 32 + ((v&0x1f)<<1) : 0
                //                  KSR = 3 - (v>>6)
                let v = val & 0x1F;
                chan.op[op].ar = if v != 0 { 32 + ((v as u32) << 1) } else { 0 };
                chan.op[op].ksr_shift = 3 - ((val >> 6) as u32);
                // Refresh KSR if changed and the EG rates.
                let kc = chan.kcode();
                chan.op[op].ksr = kc >> chan.op[op].ksr_shift;
                chan.op[op].refresh_eg_rates();
            }
            0x60 => {
                let v = val & 0x1F;
                chan.op[op].dr = if v != 0 { 32 + ((v as u32) << 1) } else { 0 };
                chan.op[op].am_enabled = (val & 0x80) != 0;
                chan.op[op].refresh_eg_rates();
            }
            0x70 => {
                let v = val & 0x1F;
                chan.op[op].sr = if v != 0 { 32 + ((v as u32) << 1) } else { 0 };
                chan.op[op].refresh_eg_rates();
            }
            0x80 => {
                // SL nibble high, RR nibble low.
                let sl_nib = (val >> 4) as usize;
                chan.op[op].sl = SL_TABLE[sl_nib];
                chan.op[op].rr = 34 + (((val & 0x0F) as u32) << 2);
                chan.op[op].refresh_eg_rates();
            }
            0x90 => {
                // SSG-EG shape (FBNeo fm.c L2107-2109):
                //   bit 3 = enable, bit 2 = attack, bit 1 = alternate,
                //   bit 0 = hold. ssgn bit 1 primed from the attack bit.
                chan.op[op].ssg = val & 0x0F;
                chan.op[op].ssgn = (val & 0x04) >> 1;
            }
            0xA0 => match addr & 0x0C {
                0x00 => { // F-num low (write must follow $A4 high latch)
                    let lo = val as u16;
                    let hi = chan.fnum_latch_hi as u16;
                    chan.fnum_block = (hi << 8) | lo;
                    self.update_channel_ksr(fm_ch);
                }
                0x04 => chan.fnum_latch_hi = val & 0x3F,
                0x08 if !bank => {
                    self.ch2_multi_block_freq[ch] =
                        (self.ch2_multi_block_freq[ch] & 0x3F00) | val as u16;
                    self.update_channel_ksr(2);
                }
                0x0C if !bank => {
                    self.ch2_multi_block_freq[ch] =
                        ((val as u16 & 0x3F) << 8) | (self.ch2_multi_block_freq[ch] & 0x00FF);
                    self.update_channel_ksr(2);
                }
                _ => {}
            },
            0xB0 => match addr & 0x0C {
                0x00 => {
                    let feedback = (val >> 3) & 0x07;
                    chan.fb = if feedback != 0 { feedback + 6 } else { 0 };
                    chan.alg = val & 0x07;
                }
                0x04 => {
                    chan.pan = val & 0xC0;
                    chan.pms = val & 0x07;
                    chan.ams = LFO_AMS_DEPTH_SHIFT[((val >> 4) & 0x03) as usize];
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Advance every FM channel by one host sample.
    ///
    /// Implements the eight OPN algorithms with all four operators per channel
    /// (matches the topology described in `ymfm_opn.h` and FBNeo `fm.c`).
    /// Returns separately scaled per-channel contributions so the caller can
    /// also track non-zero activity per channel.
    pub fn step_one_with_per_channel(&mut self) -> (i32, i32, [i32; 4]) {
        let mut l: i32 = 0;
        let mut r: i32 = 0;
        let mut per_ch = [0i32; 4];
        // YM2610 mixes only channels 1,2,4,5 to the DAC; channels 0 and 3 are
        // not routed on this variant (FBNeo YM2610UpdateOne).
        let routed = [1usize, 2, 4, 5];

        self.clock_lfo();
        let lfo_enabled = self.lfo_enabled;
        let lfo_am = self.lfo_am;
        let lfo_pm = self.lfo_pm;

        self.eg_timer += 1;
        while self.eg_timer >= 3 {
            self.eg_timer -= 3;
            self.eg_cnt = self.eg_cnt.wrapping_add(1);
            for c in 0..6 {
                for o in 0..4 {
                    self.ch[c].op[o].advance_eg(self.eg_cnt);
                }
            }
        }

        let ch2_multi_freq = self.ch2_multi_freq;
        let ch2_multi_block_freq = self.ch2_multi_block_freq;
        for c in 0..6 {
            let chan = &mut self.ch[c];
            let alg = chan.alg;
            let env1 = chan.op[0].vol_out().saturating_add(lfo_am_attenuation(chan.op[0].am_enabled, chan.ams, lfo_am));
            let env2 = chan.op[1].vol_out().saturating_add(lfo_am_attenuation(chan.op[1].am_enabled, chan.ams, lfo_am));
            let env3 = chan.op[2].vol_out().saturating_add(lfo_am_attenuation(chan.op[2].am_enabled, chan.ams, lfo_am));
            let env4 = chan.op[3].vol_out().saturating_add(lfo_am_attenuation(chan.op[3].am_enabled, chan.ams, lfo_am));

            // FBNeo chan_calc node graph.
            let mut carrier = 0i32;
            let mut m2 = 0i32;
            let mut c1 = 0i32;
            let mut c2 = 0i32;
            let mut mem = 0i32;

            // Restore delayed MEM sample to m2 or c2 depending on algorithm.
            match alg {
                0 | 1 | 2 | 5 => m2 = chan.mem_value,
                3 => c2 = chan.mem_value,
                _ => {}
            }

            // Previous OP1 output participates in the current sample; the newly
            // computed OP1 output is stored for the next sample, just like FBNeo.
            let mut fb_sum = chan.op0_prev[0] + chan.op0_prev[1];
            chan.op0_prev[0] = chan.op0_prev[1];
            let old_op1 = chan.op0_prev[0];

            if alg == 5 {
                mem = old_op1;
                c1 = old_op1;
                c2 = old_op1;
            } else {
                match alg {
                    0 | 3 | 4 | 6 => c1 += old_op1,
                    1 => mem += old_op1,
                    2 => c2 += old_op1,
                    7 => carrier += old_op1,
                    _ => {}
                }
            }

            chan.op0_prev[1] = 0;
            if env1 < ENV_QUIET {
                if chan.fb == 0 { fb_sum = 0; }
                chan.op0_prev[1] = FmOp::op_calc1(chan.op[0].phase, env1, fb_sum << chan.fb);
            }

            // MAME `ymfm_fm.ipp` clamps each algorithm-additive operator's
            // contribution to the channel result with `clamp(result, -clipmax-1, clipmax)`
            // using `clipmax = 32767`. This avoids per-channel wrap-around that
            // would otherwise leak into the global mix. We apply the same clamp
            // every time we sum into the carrier bus.
            const CLIPMAX: i32 = 32767;
            const CLIPMIN: i32 = -32768;

            if env3 < ENV_QUIET {
                let op3 = FmOp::op_calc(chan.op[2].phase, env3, m2);
                match alg {
                    5 | 6 | 7 => carrier = (carrier + op3).clamp(CLIPMIN, CLIPMAX),
                    _ => c2 += op3,
                }
            }

            if env2 < ENV_QUIET {
                let op2 = FmOp::op_calc(chan.op[1].phase, env2, c1);
                match alg {
                    0 | 1 | 2 | 3 => mem += op2,
                    _ => carrier = (carrier + op2).clamp(CLIPMIN, CLIPMAX),
                }
            }

            if env4 < ENV_QUIET {
                carrier = (carrier + FmOp::op_calc(chan.op[3].phase, env4, c2))
                    .clamp(CLIPMIN, CLIPMAX);
            }

            chan.mem_value = mem;

            // Update phase counters AFTER output calculation.
            //
            // Following MAME `compute_phase_step` (ymfm_opn.cpp):
            //   fnum_eff   = fnum + pm_offset           // PM applied on fnum, not on the already-block-shifted step
            //   phase_step = fnum_eff << (block + 5)    // our Q.16 convention
            //   step      += detune                     // detune is post-shift
            //   inc        = (step * multiple) >> 1     // multiple is x.1 (mul*2 or 1 if mul==0)
            for o in 0..4 {
                let block_freq = if ch2_multi_freq && c == 2 && o != 0 {
                    ch2_multi_block_freq[o - 1]
                } else {
                    chan.fnum_block
                };
                let kc = FmCh::kcode_from_block_freq(block_freq);
                let fnum  = (block_freq & 0x07FF) as i32;
                let block = ((block_freq >> 11) & 0x07) as u32;
                let pm_off = if lfo_enabled && chan.pms != 0 {
                    lfo_pm_offset(block_freq, chan.pms, lfo_pm)
                } else {
                    0
                };
                // Apply PM to fnum, keep it inside the 12-bit signed envelope (MAME masks to 0xFFF).
                let fnum_pm = ((fnum + pm_off) & 0x0FFF) as u32;
                let step    = fnum_pm << (block + 5);
                let mul = if chan.op[o].mul == 0 { 1 } else { (chan.op[o].mul as u32) * 2 };
                let mut base = (step as i64) + (chan.op[o].detune(kc) as i64);
                if base < 0 {
                    base += FN_MAX as i64;
                } else if base >= FN_MAX as i64 {
                    base -= FN_MAX as i64;
                }
                let inc = (base as u32).wrapping_mul(mul) >> 1;
                chan.op[o].phase = chan.op[o].phase.wrapping_add(inc);
            }

            let scaled = carrier >> 1;
            for (i, &idx) in routed.iter().enumerate() {
                if c == idx { per_ch[i] = scaled; }
            }
            if routed.contains(&c) {
                if chan.pan & 0x80 != 0 { l += scaled; }
                if chan.pan & 0x40 != 0 { r += scaled; }
            }
        }
        (l, r, per_ch)
    }

    /// Back-compat wrapper used by tests and the legacy mixer path.
    pub fn step_one(&mut self) -> (i32, i32) {
        let (l, r, _) = self.step_one_with_per_channel();
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adpcma_register_pairs_map_start_and_end_per_channel() {
        let mut ym = Ym2610::new();

        // Channel 4 start = 0x123400, end = 0x5678FF.
        ym.write_reg_b(0x14, 0x34);
        ym.write_reg_b(0x1C, 0x12);
        ym.write_reg_b(0x24, 0x78);
        ym.write_reg_b(0x2C, 0x56);

        assert_eq!(ym.adpcm_a[4].start, 0x123400);
        assert_eq!(ym.adpcm_a[4].end, 0x5678FF);
    }

    #[test]
    fn adpcma_keyon_after_typical_setup_produces_audio() {
        // Real Neo Geo drivers do: write IL/pan ($08..$0D), then key-on.
        // Verify the full pipeline produces non-zero stereo output.
        let mut ym = Ym2610::new();
        ym.adpcm_a_rom = vec![0x4Cu8; 0x1000];
        ym.write_reg_b(0x10, 0x00); ym.write_reg_b(0x18, 0x00);
        ym.write_reg_b(0x20, 0x0F); ym.write_reg_b(0x28, 0x00);
        // Loudest: $01 master TL register field = $3F (XORed gives 0).
        ym.write_reg_b(0x01, 0x3F);
        // $08 per-channel: bits 4..0 = IL ($1F = loudest after XOR), pan = both.
        ym.write_reg_b(0x08, 0xDF);
        ym.write_reg_b(0x00, 0x01);
        assert!(ym.adpcm_a[0].on);
        let mut total_nz = 0;
        let mut peak = 0i32;
        let mut vol_dump = (0,0,0);
        for i in 0..512 {
            let (l, r) = ym.step_one_sample();
            if l != 0 || r != 0 { total_nz += 1; }
            let pk = (l as i32).abs().max((r as i32).abs());
            if pk > peak { peak = pk; }
            if i == 100 {
                let il = ym.adpcm_a[0].il as i32;
                let tl = ym.adpcma_tl as i32;
                vol_dump = (il, tl, (il ^ 0x1F) + (tl ^ 0x3F));
            }
        }
        assert!(total_nz > 0,
            "ADPCM-A key-on must produce audio (got 0 nz, peak={peak}, il/tl/vol={:?})", vol_dump);
    }

    #[test]
    fn adpcma_pan_bit7_routes_to_left_channel() {
        let mut ym = Ym2610::new();
        ym.adpcm_a_rom = vec![0x4Cu8; 0x1000];
        ym.write_reg_b(0x10, 0x00); ym.write_reg_b(0x18, 0x00);
        ym.write_reg_b(0x20, 0x0F); ym.write_reg_b(0x28, 0x00);
        ym.write_reg_b(0x01, 0x3F);  // master TL loudest
        ym.write_reg_b(0x08, 0x9F);  // pan=LEFT only ($80) + IL=$1F (loudest)
        ym.write_reg_b(0x00, 0x01);
        let mut l_nz = 0; let mut r_nz = 0;
        for _ in 0..512 {
            let (l, r) = ym.step_one_sample();
            if l != 0 { l_nz += 1; }
            if r != 0 { r_nz += 1; }
        }
        assert!(l_nz > 0, "pan=$80 should produce LEFT audio (got {l_nz})");
        assert_eq!(r_nz, 0, "pan=$80 must be silent on RIGHT (got {r_nz})");
    }

    #[test]
    fn adpcma_pan_bit6_routes_to_right_channel() {
        let mut ym = Ym2610::new();
        ym.adpcm_a_rom = vec![0x4Cu8; 0x1000];
        ym.write_reg_b(0x10, 0x00); ym.write_reg_b(0x18, 0x00);
        ym.write_reg_b(0x20, 0x0F); ym.write_reg_b(0x28, 0x00);
        ym.write_reg_b(0x01, 0x3F);
        ym.write_reg_b(0x08, 0x5F);  // pan=RIGHT only ($40) + IL=$1F (loudest)
        ym.write_reg_b(0x00, 0x01);
        let mut l_nz = 0; let mut r_nz = 0;
        for _ in 0..512 {
            let (l, r) = ym.step_one_sample();
            if l != 0 { l_nz += 1; }
            if r != 0 { r_nz += 1; }
        }
        assert!(r_nz > 0, "pan=$40 should produce RIGHT audio (got {r_nz})");
        assert_eq!(l_nz, 0, "pan=$40 must be silent on LEFT (got {l_nz})");
    }

    #[test]
    fn fm_register_slot_order_matches_opn_layout() {
        let mut fm = FmOpn::new();
        // x4 must configure OP3, not OP2.
        fm.write_reg(0x34, 0x07, false); // MUL for raw slot x4
        assert_eq!(fm.ch[0].op[2].mul, 0x07, "x4 must map to OP3");
        assert_eq!(fm.ch[0].op[1].mul, 0x01, "x4 must not clobber OP2");
        // x8 must configure OP2, not OP3.
        fm.write_reg(0x38, 0x05, false);
        assert_eq!(fm.ch[0].op[1].mul, 0x05, "x8 must map to OP2");
        assert_eq!(fm.ch[0].op[2].mul, 0x07, "x8 must not overwrite OP3");
    }

    #[test]
    fn fm_smoke_generates_nonzero_output() {
        let mut fm = FmOpn::new();
        // Configure ALL four operators with audible AR so envelopes progress.
        // Yamaha hardware register layout:
        //   $X0 = SLOT1 (OP1) | $X4 = SLOT3 (OP3)
        //   $X8 = SLOT2 (OP2) | $XC = SLOT4 (OP4)
        for off in [0x31u8, 0x35, 0x39, 0x3D] { fm.write_reg(off, 0x01, false); }
        for off in [0x41u8, 0x45, 0x49, 0x4D] { fm.write_reg(off, 0x00, false); }
        fm.write_reg(0x41, 24, false);
        for off in [0x51u8, 0x55, 0x59, 0x5D] { fm.write_reg(off, 31, false); }
        for off in [0x61u8, 0x65, 0x69, 0x6D] { fm.write_reg(off, 0,  false); }
        for off in [0x71u8, 0x75, 0x79, 0x7D] { fm.write_reg(off, 0,  false); }
        for off in [0x81u8, 0x85, 0x89, 0x8D] { fm.write_reg(off, 0,  false); }
        fm.write_reg(0xA5, 0x22, false);
        fm.write_reg(0xA1, 0x69, false);
        fm.write_reg(0xB1, (4 << 3) | 4, false);
        fm.write_reg(0xB5, 0xC0, false);
        fm.key(1, 0b1111);

        let mut nonzero = 0usize;
        for _ in 0..4096 {
            let (l, r) = fm.step_one();
            if l != 0 || r != 0 {
                nonzero += 1;
            }
        }
        assert!(nonzero > 0, "FM approximation stayed silent");
    }

    // -- Audio precision audit (v33): tablas y comportamiento vs FBNeo --

    #[test]
    fn jedi_table_matches_fbneo_formula() {
        // FBNeo init: value = (2*(nib & 7) + 1) * steps[step] / 8;
        //             jedi[step*16 + nib] = (nib & 8) ? -value : value;
        let steps_ref: [i32; 49] = [
             16,  17,   19,   21,   23,   25,   28,
             31,  34,   37,   41,   45,   50,   55,
             60,  66,   73,   80,   88,   97,  107,
            118, 130,  143,  157,  173,  190,  209,
            230, 253,  279,  307,  337,  371,  408,
            449, 494,  544,  598,  658,  724,  796,
            876, 963, 1060, 1166, 1282, 1411, 1552,
        ];
        assert_eq!(ADPCMA_STEPS, steps_ref, "ADPCMA_STEPS divergent");
        for step in 0..49usize {
            for nib in 0..16usize {
                let mag = (2 * (nib as i32 & 0x7) + 1) * steps_ref[step] / 8;
                let expected = if nib & 0x08 != 0 { -mag } else { mag };
                assert_eq!(JEDI_TABLE[step * 16 + nib], expected,
                    "JEDI mismatch step={step} nib={nib}");
            }
        }
    }

    #[test]
    fn adpcma_step_inc_matches_fbneo() {
        // FBNeo: const INT8 step_inc[8] = {-1,-1,-1,-1,2,5,7,9}; nosotros
        // pre-multiplicamos por 16 para que el index sea `step + nib` sin
        // multiplicar en cada nibble.
        let expected: [i32; 8] = [-16, -16, -16, -16, 32, 80, 112, 144];
        assert_eq!(ADPCMA_STEP_INC, expected);
    }

    #[test]
    fn adpcma_volume_table_matches_fbneo_formula() {
        // ymfm: vol = adpcmTL + IL; if vol>=63 silent; else
        //       mul = 15 - (vol & 7), shift = 4 + 1 + (vol >> 3),
        //       out = ((int16_t(acc << 4) * mul) >> shift) & ~3.
        let mut ch = AdpcmAChan::new();
        ch.on = true;
        ch.acc = 0x100;
        for tl in 0u8..64 {
            for il in 0u8..32 {
                ch.il = il;
                ch.refresh_volume(tl);
                let vol = il as i32 + tl as i32;
                let (exp_mul, exp_shift) = if vol >= 63 {
                    (0, 0)
                } else {
                    (15 - (vol & 7), 5 + (vol >> 3))
                };
                assert_eq!(ch.vol_mul, exp_mul,
                    "vol_mul mismatch TL={tl} IL={il}");
                assert_eq!(ch.vol_shift, exp_shift,
                    "vol_shift mismatch TL={tl} IL={il}");
                let exp_out = if exp_mul == 0 { 0 } else {
                    let ext = ((ch.acc as i16 as i32) << 4) as i32;
                    ((ext * exp_mul) >> exp_shift) & !3
                };
                assert_eq!(ch.adpcm_out, exp_out,
                    "adpcm_out cache mismatch TL={tl} IL={il}");
            }
        }
    }

    #[test]
    fn adpcma_tl_write_refreshes_all_channels() {
        // Si el driver baja el master TL en medio de una nota, los seis
        // canales activos deben atenuar inmediatamente (FBNeo recorre los
        // 6 canales en FM_ADPCMAWrite case 0x01).
        let mut ym = Ym2610::new();
        ym.adpcm_a_rom = vec![0x4Cu8; 0x1000];
        ym.write_reg_b(0x10, 0x00); ym.write_reg_b(0x18, 0x00);
        ym.write_reg_b(0x20, 0x0F); ym.write_reg_b(0x28, 0x00);
        ym.write_reg_b(0x01, 0x3F);   // master TL loudest -> almacenado 0
        ym.write_reg_b(0x08, 0xDF);   // ch0 IL loudest, pan=both
        ym.write_reg_b(0x00, 0x01);   // key-on ch0
        for _ in 0..32 { ym.step_one_sample(); }
        // Después del key-on debemos tener vol_mul != 0 en ch0.
        let mul_loud = ym.adpcm_a[0].vol_mul;
        assert!(mul_loud > 0, "ch0 vol_mul should be non-zero after loud key-on");
        // Subimos el TL a máximo silencio (val=0x00 -> almacenado 0x3F).
        ym.write_reg_b(0x01, 0x00);
        // Master TL=0x3F + IL=0x1F=31 -> vol=94 >=63 -> mul=0, shift=0.
        assert_eq!(ym.adpcm_a[0].vol_mul, 0,
            "TL write must zero vol_mul (silent)");
        assert_eq!(ym.adpcm_a[0].vol_shift, 0,
            "TL write must zero vol_shift (silent)");
        assert_eq!(ym.adpcm_a[0].adpcm_out, 0,
            "cached adpcm_out must be 0 after silence write");
    }

    #[test]
    fn adpcmb_defaults_to_raw_non_interpolated_output() {
        let mut dt = DeltaTChan::new();
        dt.on = true;
        dt.interpolate = false;
        dt.acc = 1234;
        dt.prev_acc = -4321;
        dt.now_step = 0x8000;
        dt.delta = 0;
        assert_eq!(dt.step_one(&[]), 1234,
            "chip-accurate Delta-T mode must return raw current sample");
    }

    #[test]
    fn adpcmb_optional_interpolation_matches_fbneo_formula() {
        let mut dt = DeltaTChan::new();
        dt.on = true;
        dt.interpolate = true;
        dt.acc = 1000;
        dt.prev_acc = -1000;
        dt.now_step = 0x8000;
        dt.delta = 0;
        assert_eq!(dt.step_one(&[]), 0,
            "midpoint interpolation must average prev/current samples");
    }

    #[test]
    fn adpcmb_nibble_cursor_wraps_to_25_bits() {
        let mut dt = DeltaTChan::new();
        dt.on = true;
        dt.interpolate = false;
        dt.delta = 1;
        dt.now_step = 0xFFFF;
        dt.end = 0xFF_FFFE;      // ensure EOS does not stop playback first
        dt.addr_nib = DELTAT_ADDR_MASK - 1;
        dt.curnibble = 1;        // consume low nibble so the byte cursor advances
        let _ = dt.step_one(&[]);
        assert_eq!(dt.addr_nib, 0,
            "Delta-T nibble cursor must wrap to 25 bits like FBNeo/MAME");
    }

    #[test]
    fn fm_pan_left_only_produces_silence_on_right() {
        let mut fm = FmOpn::new();
        for off in [0x31u8, 0x35, 0x39, 0x3D] { fm.write_reg(off, 0x01, false); }
        for off in [0x41u8, 0x45, 0x49, 0x4D] { fm.write_reg(off, 0x00, false); }
        for off in [0x51u8, 0x55, 0x59, 0x5D] { fm.write_reg(off, 31, false); }
        for off in [0x81u8, 0x85, 0x89, 0x8D] { fm.write_reg(off, 0,  false); }
        fm.write_reg(0xA5, 0x22, false);
        fm.write_reg(0xA1, 0x69, false);
        fm.write_reg(0xB1, 4, false);
        fm.write_reg(0xB5, 0x80, false);
        fm.key(1, 0b1111);
        let mut r_nz = 0;
        for _ in 0..2048 {
            let (_l, r) = fm.step_one();
            if r != 0 { r_nz += 1; }
        }
        assert_eq!(r_nz, 0, "FM pan=$80 (L only) must be silent on right (got {r_nz})");
    }

    // -- SSG clock accuracy audit (v34): el bug del coin beep agudo --

    #[test]
    fn ssg_step_clock_matches_fbneo_prescaler_chain() {
        // FBNeo fm.c line 4072:
        //   OPNSetPres(OPN, 6*24, 6*24, 4*2);  /* OPN 1/144, SSG 1/8 */
        // FBNeo fm.c line 1942:
        //   SSGClk(index, OPN->ST.clock * 2 / SSGpres);  => clock * 2 / 8 = clock / 4
        // NeoGeo YM2610 clock = master/3 = 24_000_000/3 = 8_000_000.
        // External SSG clock = 8_000_000 * 2 / 8 = 2_000_000.
        // Internal step clock (FBNeo ay8910.c L740 comment: "chip clock /8"):
        //   2_000_000 / 8 = 250_000 Hz.
        const NEOGEO_YM2610_CLOCK: u32 = 24_000_000 / 3;
        const SSG_PRES: u32 = 4 * 2;
        let ssg_external = NEOGEO_YM2610_CLOCK * 2 / SSG_PRES;
        let ssg_internal = ssg_external / 8;
        assert_eq!(ssg_external, 2_000_000, "SSG external clock must be 2 MHz");
        assert_eq!(ssg_internal, SSG_STEP_CLOCK_HZ,
            "SSG internal step clock must be 250 kHz");
    }

    #[test]
    fn ssg_ticks_per_sample_yields_canonical_tone_frequency() {
        // The AY tone freq formula is `clock_step / (2 * period)`. The host
        // mixer runs at `master/144 = ~55_555 Hz` and on every host sample
        // we advance the SSG by `SSG_TICKS_PER_SAMPLE_Q16 >> 16` ticks (plus
        // a Q16 fractional accumulator). Equivalent ticks/sec = host_rate *
        // (Q16 / 65536). We require ticks/sec == SSG_STEP_CLOCK_HZ (= 250000).
        let ticks_per_sample_f = SSG_TICKS_PER_SAMPLE_Q16 as f64 / 65536.0;
        let host_rate = HOST_SAMPLE_RATE_HZ as f64;
        let effective_ssg_hz = host_rate * ticks_per_sample_f;
        // 55_555 * 4.5 = 249_997.5 ≈ 250_000 (chip-accurate within 0.01 %).
        let err = (effective_ssg_hz - SSG_STEP_CLOCK_HZ as f64).abs();
        assert!(err < 50.0,
            "effective SSG clock {} Hz differs from chip's {} by {} Hz \
             (must be <50 Hz)", effective_ssg_hz, SSG_STEP_CLOCK_HZ, err);
    }

    // -- FM DSP tables: bit-exact verification vs FBNeo `fm.c` --

    #[test]
    fn eg_rate_shift_matches_fbneo_table() {
        // Verbatim FBNeo `fm.c` `eg_rate_shift[32+64+32]`. 32 infinite zeros,
        // then 12 rates × 4 = 48 entries from 11..0, then 16 zeros (rates
        // 12..15), then 32 dummy zeros.
        let mut ref_tbl = [0u32; 128];
        for r in 0..12u32 {
            for k in 0..4 {
                ref_tbl[32 + (r as usize) * 4 + k] = 11 - r;
            }
        }
        // rates 12..15 + 32 dummies are already 0.
        assert_eq!(EG_RATE_SHIFT[..], ref_tbl[..],
            "EG_RATE_SHIFT must match FBNeo eg_rate_shift verbatim");
    }

    #[test]
    fn eg_rate_select_matches_fbneo_table() {
        // FBNeo `eg_rate_select`: 32 entries of O(18), then rates 00..11 cycle
        // O(0)..O(3), then rate 12 = O(4..7), rate 13 = O(8..11),
        // rate 14 = O(12..15), rate 15 = O(16)×4, then 32 dummies O(16).
        // O(a) = a * RATE_STEPS = a * 8.
        let mut ref_tbl = [0u32; 128];
        for i in 0..32 { ref_tbl[i] = 18 * 8; }
        for r in 0..12 {
            for k in 0..4 {
                ref_tbl[32 + r * 4 + k] = (k as u32) * 8;
            }
        }
        for k in 0..4 { ref_tbl[32 + 48 + k]  = (4 + k as u32) * 8; }  // rate 12
        for k in 0..4 { ref_tbl[32 + 52 + k]  = (8 + k as u32) * 8; }  // rate 13
        for k in 0..4 { ref_tbl[32 + 56 + k]  = (12 + k as u32) * 8; } // rate 14
        for k in 0..4 { ref_tbl[32 + 60 + k]  = 16 * 8; }              // rate 15
        for i in 96..128 { ref_tbl[i] = 16 * 8; }                       // dummies
        assert_eq!(EG_RATE_SELECT[..], ref_tbl[..],
            "EG_RATE_SELECT must match FBNeo eg_rate_select verbatim");
    }

    #[test]
    fn eg_inc_table_matches_fbneo_verbatim() {
        // Spot-check the canonical 19×8 increments (key rows from FBNeo fm.c L219).
        let row = |idx: usize| -> [u8; 8] {
            let base = idx * 8;
            [EG_INC[base+0], EG_INC[base+1], EG_INC[base+2], EG_INC[base+3],
             EG_INC[base+4], EG_INC[base+5], EG_INC[base+6], EG_INC[base+7]]
        };
        assert_eq!(row( 0), [0,1, 0,1, 0,1, 0,1]);
        assert_eq!(row( 4), [1,1, 1,1, 1,1, 1,1], "rate 12 step 0 must be all ones");
        assert_eq!(row( 8), [2,2, 2,2, 2,2, 2,2], "rate 13 step 0 must be all twos");
        assert_eq!(row(12), [4,4, 4,4, 4,4, 4,4], "rate 14 step 0 must be all fours");
        assert_eq!(row(16), [8,8, 8,8, 8,8, 8,8], "rate 15 must be all eights");
        assert_eq!(row(17), [16,16,16,16,16,16,16,16],
            "attack 15 special row must be all sixteens");
        assert_eq!(row(18), [0,0, 0,0, 0,0, 0,0], "infinity rates must be all zeros");
    }

    #[test]
    fn sl_table_matches_fbneo_sc_formula() {
        // FBNeo SC(db) = db * (4.0/ENV_STEP); ENV_STEP = 128/1024 -> SC(db) = db*32.
        // sl_table = [SC(0..=14), SC(31)].
        let expected: [u32; 16] = [
            0, 32, 64, 96, 128, 160, 192, 224,
            256, 288, 320, 352, 384, 416, 448, 992,
        ];
        for i in 0..16 {
            assert_eq!(SL_TABLE[i], expected[i],
                "SL_TABLE[{i}] must match FBNeo sc({}) formula", if i==15 {31} else {i});
        }
    }

    #[test]
    fn dt_tab_base_matches_fbneo_verbatim() {
        // FBNeo fm.c L382: dt_tab[128] (4 detune values × 32 keycodes).
        // FD=0 -> all zeros.
        for i in 0..32 {
            assert_eq!(DT_TAB_BASE[i], 0, "DT_TAB FD=0 row must be all zeros");
        }
        // FD=1 spot-check: ..1,1,1,1,1,1,1,1,2,2,2,2,2,3,3,3,4,4,4,5,5,6,6,7,8,8,8,8
        assert_eq!(&DT_TAB_BASE[32..64],
            &[0,0,0,0, 1,1,1,1, 1,1,1,1, 2,2,2,2,
              2,3,3,3, 4,4,4,5, 5,6,6,7, 8,8,8,8][..]);
        // FD=2 spot-check: starts with 1,1,1,1 (FBNeo line 405)
        assert_eq!(&DT_TAB_BASE[64..68], &[1,1,1,1][..]);
        // FD=3 ends with 22,22,22,22 (saturated max detune)
        assert_eq!(&DT_TAB_BASE[124..128], &[22,22,22,22][..]);
    }

    #[test]
    fn dt_tab_mirrors_negative_for_fd_4_to_7() {
        // FBNeo wires DT=4..7 as the negative twin of DT=0..3.
        for d in 0..4 {
            for i in 0..32 {
                assert_eq!(DT_TAB[d + 4][i], -DT_TAB[d][i],
                    "DT_TAB[d+4] must mirror DT_TAB[d] (d={d}, i={i})");
            }
        }
    }

    #[test]
    fn lfo_samples_per_step_matches_fbneo() {
        assert_eq!(LFO_SAMPLES_PER_STEP, [108, 77, 71, 67, 62, 44, 8, 5],
            "LFO_SAMPLES_PER_STEP must match FBNeo lfo_samples_per_step");
    }

    #[test]
    fn lfo_ams_depth_shift_matches_fbneo() {
        assert_eq!(LFO_AMS_DEPTH_SHIFT, [8, 3, 1, 0],
            "LFO_AMS_DEPTH_SHIFT must match FBNeo lfo_ams_depth_shift");
    }

    #[test]
    fn lfo_pm_output_extremes_match_fbneo() {
        // FNUM BIT 4 depth 7 row: {0,0,0,0,1,1,1,1}
        assert_eq!(LFO_PM_OUTPUT[0*8 + 7], [0,0,0,0,1,1,1,1]);
        // FNUM BIT 10 depth 7 row: {0,0,32,48,64,64,80,96} (max PM)
        assert_eq!(LFO_PM_OUTPUT[6*8 + 7], [0,0,32,48,64,64,80,96],
            "deepest LFO PM bit10 row must match FBNeo");
        // FNUM BIT 9 depth 7: {0,0,16,24,32,32,40,48}
        assert_eq!(LFO_PM_OUTPUT[5*8 + 7], [0,0,16,24,32,32,40,48]);
        // Depth 0 always zero across all FNUM bits.
        for bit in 0..7 {
            assert_eq!(LFO_PM_OUTPUT[bit*8 + 0], [0,0,0,0,0,0,0,0],
                "LFO PM depth=0 must be silent (bit={bit})");
        }
    }

    #[test]
    fn tl_tab_size_and_amplitude_match_fbneo() {
        // FBNeo `tl_tab` total length = 13 * 2 * TL_RES_LEN = 6656.
        assert_eq!(TL_TAB.len(), 6656, "TL_TAB must be 13*2*256 entries");
        // First positive entry should be close to ±8138/8139 (chip max ~8192).
        let v0 = TL_TAB[0];
        let vn = TL_TAB[1];
        assert!(v0.abs() >= 8000 && v0.abs() <= 8192,
            "TL_TAB[0] amplitude out of expected range (got {v0})");
        assert_eq!(v0, -vn, "adjacent TL_TAB entries must mirror sign");
        // Each octave shift halves amplitude (within ±1 LSB).
        for i in 1..13 {
            let halved = (v0 >> i).abs();
            let actual = TL_TAB[i * 2 * 256].abs();
            assert!((halved - actual).abs() <= 1,
                "TL_TAB octave shift mismatch at i={i} (halved={halved}, actual={actual})");
        }
    }

    #[test]
    fn sin_tab_size_and_phase_extremes_match_log_sin() {
        // FBNeo `sin_tab` length is SIN_LEN = 1024.
        assert_eq!(SIN_TAB.len(), 1024, "SIN_TAB must be 1024 entries");
        // Phase 0 (sin smallest) should encode the loudest attenuation.
        // Phase 256 (sin = 1, peak) should encode the smallest attenuation.
        let s_min_phase = SIN_TAB[0];
        let s_peak_phase = SIN_TAB[256];
        assert!(s_min_phase > s_peak_phase,
            "SIN_TAB[0] attenuation must exceed SIN_TAB[256] (got {s_min_phase} vs {s_peak_phase})");
        // Sign bit: SIN_TAB[0..512] should be positive (LSB=0), SIN_TAB[512..1024] negative.
        assert_eq!(SIN_TAB[0] & 1, 0, "first half must be positive");
        assert_eq!(SIN_TAB[513] & 1, 1, "second half must encode negative sign in LSB");
    }

    #[test]
    fn env_constants_match_fbneo_definitions() {
        assert_eq!(ENV_BITS, 10);
        assert_eq!(ENV_LEN, 1024);
        assert_eq!(MAX_ATT_INDEX, 1023);
        assert_eq!(MIN_ATT_INDEX, 0);
        assert_eq!(TL_TAB_LEN, 6656);
        assert_eq!(FREQ_SH, 16);
        assert_eq!(FREQ_MASK, 0xFFFF);
        assert_eq!(RATE_STEPS, 8);
        assert_eq!(ENV_QUIET, 832, "ENV_QUIET should be TL_TAB_LEN >> 3");
        assert_eq!(FN_MAX, 0x10000000);
    }

    #[test]
    fn op_calc_returns_zero_for_quiet_envelope() {
        // When the combined env exceeds TL_TAB_LEN the operator must mute.
        let result = FmOp::op_calc(0, (TL_TAB_LEN as u32) >> 3, 0);
        // env<<3 = (TL_TAB_LEN >> 3) << 3 = TL_TAB_LEN. Adding sin_tab will
        // exceed the table -> 0.
        let _ = result; // value depends on sin_tab; key check below.
        // Force a known-quiet env that definitely exceeds TL_TAB after <<3.
        assert_eq!(FmOp::op_calc(0, 2048, 0), 0,
            "op_calc must clamp to 0 when env<<3 exceeds TL_TAB_LEN");
    }

    // -- Mixer & integration regression --

    // -- Timer A/B (mode register $27) --

    #[test]
    fn timer_a_period_matches_fbneo_formula() {
        // FBNeo fm.c L831: TAC = (1024 - TA).
        let mut ym = Ym2610::new();
        // TA = $024<<2 | $025[1..0]; pick TA = 1.
        ym.write_reg_a(0x24, 0);
        ym.write_reg_a(0x25, 1);
        // Load TA via $27 bit 0.
        ym.write_reg_a(0x27, 0x01);
        assert_eq!(ym.timer_a_count, 1024 - 1,
            "timer A count must equal 1024 - TA after load");
    }

    #[test]
    fn timer_b_period_matches_fbneo_formula() {
        // FBNeo fm.c L813: TBC = (256 - TB) << 4.
        let mut ym = Ym2610::new();
        ym.write_reg_a(0x26, 5);
        ym.write_reg_a(0x27, 0x02);   // Load TB via $27 bit 1.
        assert_eq!(ym.timer_b_count, (256 - 5) * 16,
            "timer B count must equal (256 - TB) << 4 after load");
    }

    #[test]
    fn timer_irq_flag_resets_on_mode_bit4_bit5() {
        let mut ym = Ym2610::new();
        ym.status_a = 0x03;            // simulate Timer A + B overflow latched.
        ym.write_reg_a(0x27, 0x10);    // reset TA flag
        assert_eq!(ym.status_a & 0x01, 0, "bit 4 of $27 must clear Timer A flag");
        assert_eq!(ym.status_a & 0x02, 0x02, "Timer B flag must remain set");
        ym.write_reg_a(0x27, 0x20);    // reset TB flag
        assert_eq!(ym.status_a & 0x02, 0, "bit 5 of $27 must clear Timer B flag");
    }

    #[test]
    fn step_one_sample_returns_silence_on_reset_chip() {
        // Fresh chip with no key-ons should produce dead silence on both lanes
        // for the first second of audio (=55_555 samples).
        let mut ym = Ym2610::new();
        let mut left_nz = 0u32;
        let mut right_nz = 0u32;
        for _ in 0..HOST_SAMPLE_RATE_HZ {
            let (l, r) = ym.step_one_sample();
            if l != 0 { left_nz += 1; }
            if r != 0 { right_nz += 1; }
        }
        assert_eq!(left_nz, 0, "reset chip must be silent on left channel");
        assert_eq!(right_nz, 0, "reset chip must be silent on right channel");
    }

    #[test]
    fn ssg_concert_a_period_produces_440hz_range() {
        // Reality check: programming a tone to play A4 (440 Hz) should give
        // a period close to canonical AY value.
        //   period_canonical = 125_000 / 440 ≈ 284 (0x11C)
        // After our v34 fix the SSG sample loop should emit ≈440 Hz when
        // we program period=284. Without the fix it would be ~1.76 kHz.
        let mut ssg = Ssg::new();
        ssg.write_reg(0x07, 0b00111110);   // mixer: tone A on, others off
        ssg.write_reg(0x08, 0x0F);         // ch A vol = max fixed
        ssg.write_reg(0x00, 0x1C);         // tone A period low byte
        ssg.write_reg(0x01, 0x01);         // tone A period high (0x011C = 284)
        // Count rising edges of channel A over exactly 1 second of host
        // samples (HOST_SAMPLE_RATE_HZ samples).
        let mut last = 0u8;
        let mut edges = 0u32;
        for _ in 0..HOST_SAMPLE_RATE_HZ {
            let _ = ssg.step_one();
            let cur = ssg.tone_state[0];
            if cur == 1 && last == 0 { edges += 1; }
            last = cur;
        }
        // Expected 440 ± 5 Hz (tolerance for the 0.01% Q16 rounding).
        assert!(edges >= 430 && edges <= 450,
            "SSG A4 (period=284) should emit ~440 rising edges/sec, got {edges} \
             — if value is ~1760 the prescaler/clock fix regressed");
    }

    #[test]
    fn busy_flag_stays_high_for_96_z80_cycles_after_data_write() {
        let mut ym = Ym2610::new();
        ym.write_port(0, 0x22);
        ym.write_port(1, 0x08);
        assert_eq!(ym.read_status() & 0x80, 0x80, "busy must raise immediately after data write");
        ym.elapse_z80_cycles(YM2610_BUSY_Z80_CYCLES - 1);
        assert_eq!(ym.read_status() & 0x80, 0x80, "busy must remain asserted before the final cycle");
        ym.elapse_z80_cycles(1);
        assert_eq!(ym.read_status() & 0x80, 0x00, "busy must clear after 96 Z80 cycles");
    }

    #[test]
    fn fm_mode_3slot_uses_separate_block_fnum_for_channel_3_ops_2_to_4() {
        let mut ym = Ym2610::new();

        // Base frequency for logical channel 2 (operator 1 keeps using this).
        ym.fm.ch[2].fnum_block = 0x1234;

        // 3-slot special frequencies: A8/AC, A9/AD, AA/AE.
        ym.write_reg_a(0xA8, 0x78);
        ym.write_reg_a(0xAC, 0x09);
        ym.write_reg_a(0xA9, 0x9A);
        ym.write_reg_a(0xAD, 0x0A);
        ym.write_reg_a(0xAA, 0xBC);
        ym.write_reg_a(0xAE, 0x0B);

        ym.write_reg_a(0x27, 0x40); // enable channel-3 multi-frequency mode

        assert_eq!(ym.fm.block_freq_for_op(2, 0), 0x1234);
        assert_eq!(ym.fm.block_freq_for_op(2, 1), 0x0978);
        assert_eq!(ym.fm.block_freq_for_op(2, 2), 0x0A9A);
        assert_eq!(ym.fm.block_freq_for_op(2, 3), 0x0BBC);
    }
}

#[cfg(test)]
mod adpcm_a_volume_tests {
    use super::AdpcmAChan;

    /// Regression test for the mslug2 intro bug: `refresh_volume` must
    /// sign-extend the 12-bit accumulator identically to ymfm's
    /// `int16_t(m_accumulator << 4)` sequence in
    /// `src/devices/sound/ymfm/src/ymfm_adpcm.cpp:239`.
    ///
    /// Before the v42 fix an accumulator value with bit-11 set (i.e. a negative
    /// 12-bit two's-complement number) came out of `refresh_volume` as a large
    /// POSITIVE sample, doubling the peak and desynchronising the track-and-hold
    /// path used by `step_nibble`. The result was the "voz sampleada 2x mas
    /// fuerte que en MAME" symptom in the Metal Slug 2 intro.
    #[test]
    fn refresh_volume_sign_extends_12bit_accumulator() {
        let mut ch = AdpcmAChan::new();
        ch.on = true;
        ch.il = 0;
        ch.refresh_volume(0);

        // Case A: acc = 0x100 -> positive sample stays positive.
        ch.acc = 0x100;
        ch.refresh_volume(0);
        let pos = ch.adpcm_out;
        assert!(pos > 0, "positive 12-bit sample should stay positive, got {pos}");

        // Case B: acc = 0x800 -> bit-11 set, must sign-extend to negative.
        ch.acc = 0x800;
        ch.refresh_volume(0);
        let neg = ch.adpcm_out;
        assert!(neg < 0, "negative 12-bit sample must sign-extend, got {neg}");

        // Case C: magnitude of -2048 must exceed magnitude of +256 by ~8x.
        assert!(
            neg.unsigned_abs() >= pos.unsigned_abs() * 4,
            "expected |neg|>>|pos| but neg={neg} pos={pos}",
        );
    }

    /// `refresh_volume` and `step_nibble`'s track-and-hold cache MUST match
    /// bit-for-bit for the same (acc, vol). Otherwise a mid-stream register
    /// write introduces an audible discontinuity in the DAC output.
    #[test]
    fn refresh_volume_matches_track_and_hold() {
        let mut ch = AdpcmAChan::new();
        ch.on = true; ch.il = 0;
        ch.refresh_volume(0);
        for acc in [0i32, 0x001, 0x100, 0x7FF, 0x800, 0xC00, 0xFFF] {
            ch.acc = acc;
            let expected = {
                let ext = (((acc & 0xFFF) as u16) << 4) as i16 as i32;
                ((ext * ch.vol_mul) >> ch.vol_shift) & !3
            };
            ch.refresh_volume(0);
            assert_eq!(
                ch.adpcm_out, expected,
                "acc=0x{acc:03X}: refresh_volume={} vs manual={}",
                ch.adpcm_out, expected,
            );
        }
    }
}

#[cfg(test)]
mod ssg_eg_tests {
    use super::*;

    /// Build an FmOpn with SSG-EG programmed on ch1/op1 via register writes.
    fn opn_with_ssg(shape: u8) -> FmOpn {
        let mut fm = FmOpn::new();
        // Register slot x1 = channel 1, raw slot 0 = OP1.
        fm.write_reg(0x91, shape, false);
        fm
    }

    #[test]
    fn reg_0x90_sets_ssg_and_ssgn() {
        let fm = opn_with_ssg(0x0F);
        assert_eq!(fm.ch[1].op[0].ssg, 0x0F);
        // ssgn primed from attack bit: (0x04)>>1 = 2.
        assert_eq!(fm.ch[1].op[0].ssgn, 0x02);

        let fm2 = opn_with_ssg(0x08);
        assert_eq!(fm2.ch[1].op[0].ssg, 0x08);
        assert_eq!(fm2.ch[1].op[0].ssgn, 0x00);
    }

    #[test]
    fn key_on_rearms_ssgn_from_attack_bit() {
        let mut op = FmOp::new();
        op.ssg = 0x0C; // enable + attack
        op.ssgn = 0;   // scrambled by previous playback
        op.key_on();
        assert_eq!(op.ssgn, 0x02, "key-on must re-arm ssgn = (ssg&4)>>1");
        assert_eq!(op.state, EG_ATT);
        assert_eq!(op.phase, 0);
    }

    #[test]
    fn vol_out_inverts_when_ssgn_negated() {
        let mut op = FmOp::new();
        op.volume = 100;
        op.tl = 0;
        op.state = EG_SUS; // active (> EG_REL)
        op.ssg = 0x08;
        op.ssgn = 0x02;
        assert_eq!(op.vol_out(), 100 ^ MAX_ATT_INDEX as u32);
        // Without the enable bit there is no inversion.
        op.ssg = 0;
        assert_eq!(op.vol_out(), 100);
        // In release/off the inversion is dropped too.
        op.ssg = 0x08;
        op.state = EG_REL;
        assert_eq!(op.vol_out(), 100);
    }

    #[test]
    fn ssg_repeat_shape_restarts_attack_at_env_quiet() {
        // Shape 0x08 (\\\\): repeat, no hold, no alternate.
        let mut op = FmOp::new();
        op.ssg = 0x08;
        op.ssgn = 0;
        op.state = EG_SUS;
        op.volume = ENV_QUIET as i32; // already at the SSG "quiet" threshold
        op.phase = 0xDEAD;
        op.sr = 32 + (10 << 1);
        op.refresh_eg_rates();
        // Force an EG step where the counter aligns (mask hit at eg_cnt=0).
        op.advance_eg(0);
        assert_eq!(op.state, EG_ATT, "repeat shape must re-enter attack");
        assert_eq!(op.volume, 511, "repeat shape restarts with volume=511");
        assert_eq!(op.phase, 0, "phase generator must restart");
        assert_eq!(op.ssgn, 0, "no alternate bit -> no inversion toggle");
    }

    #[test]
    fn ssg_hold_shape_swaps_once_then_holds() {
        // Shape 0x0B (\¯¯¯): enable+alternate+hold.
        let mut op = FmOp::new();
        op.ssg = 0x0B;
        op.ssgn = 0;
        op.state = EG_SUS;
        op.volume = ENV_QUIET as i32;
        op.sr = 32 + (10 << 1);
        op.refresh_eg_rates();
        op.advance_eg(0);
        assert_eq!(op.state, EG_SUS, "hold shape stays in sustain");
        assert_eq!(op.volume, MAX_ATT_INDEX);
        // swap_flag = (ssg&2)|1 = 3 -> ssgn 0^3 = 3 (inverted + swapped-once).
        assert_eq!(op.ssgn, 0x03);
        // Second pass: swapped-once marker set, so nothing changes any more.
        op.advance_eg(0);
        assert_eq!(op.ssgn, 0x03, "hold shape must not swap twice");
        assert_eq!(op.volume, MAX_ATT_INDEX);
    }

    #[test]
    fn ssg_decay_runs_4x_faster() {
        // Same DR / same eg counter: SSG-EG decay increment must be 4x.
        let mk = |ssg: u8| {
            let mut op = FmOp::new();
            op.ssg = ssg;
            op.state = EG_DEC;
            op.volume = 0;
            op.sl = MAX_ATT_INDEX as u32; // don't hit SL during the test
            op.dr = 32 + (16 << 1);
            op.refresh_eg_rates();
            op
        };
        let mut plain = mk(0x00);
        let mut ssg = mk(0x08);
        // Find an eg_cnt where the rate mask hits and the increment is nonzero.
        let mut checked = false;
        for cnt in 0u32..64 {
            let v0p = plain.volume;
            let v0s = ssg.volume;
            plain.advance_eg(cnt);
            ssg.advance_eg(cnt);
            let dp = plain.volume - v0p;
            let ds = ssg.volume - v0s;
            if dp > 0 {
                assert_eq!(ds, 4 * dp, "SSG-EG decay must be 4x plain decay");
                checked = true;
            }
        }
        assert!(checked, "test never hit a decay step; adjust DR");
    }
}

// ============================================================================
// Savestates
// ============================================================================
//
// `adpcm_a_rom` / `adpcm_b_rom` son V-ROM (datos de cartucho) y quedan fuera
// del savestate. Los contadores dbg_* tampoco se persisten: son diagnósticos
// y no afectan a la emulación.

crate::state::state_fields!(AdpcmAChan {
    on, il, pan, start, end, addr_nib, cur_byte, acc, step, adpcm_out,
    vol_mul, vol_shift,
});

crate::state::state_fields!(DeltaTChan {
    on, volume, pan, delta, start, end, interpolate, now_step, addr_nib,
    cur_byte, acc, prev_acc, adpcmd, looped, curnibble,
});

crate::state::state_fields!(Ssg {
    tone_period, noise_period, mixer, vol, env_period, env_shape,
    tone_count, tone_state, noise_count, noise_state, env_count, env_state,
    tick_frac, dc_estimator_q16,
});

crate::state::state_fields!(FmOp {
    mul, dt, tl, ar, dr, sr, rr, sl, ksr_shift, ksr, phase, volume, state,
    eg_sh_ar, eg_sh_d1r, eg_sh_d2r, eg_sh_rr,
    eg_sel_ar, eg_sel_d1r, eg_sel_d2r, eg_sel_rr,
    am_enabled, key, ssg, ssgn,
});

crate::state::state_fields!(FmCh {
    op, fnum_block, alg, fb, pan, ams, pms, op0_prev, mem_value,
    fnum_latch_hi, active,
});

crate::state::state_fields!(FmOpn {
    ch, ch2_multi_freq, ch2_multi_block_freq, eg_cnt, eg_timer,
    lfo_enabled, lfo_rate, lfo_delay, lfo_pos, lfo_am, lfo_pm,
});

crate::state::state_fields!(Ym2610 {
    regs_a, regs_b, addr_a, addr_b, status_a, busy_z80_cycles,
    adpcm_a, adpcm_b, ssg, irq_out, irq_enable,
    timer_a_period, timer_b_period, timer_a_enabled, timer_b_enabled,
    timer_a_count, timer_b_count, adpcm_a_end_flags, adpcma_tl, fm,
    adpcm_a_frac,
});

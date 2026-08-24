//! NEC uPD4990A real-time clock / calendar emulation.
//!
//! Ported from FBNeo's `src/burn/drv/neogeo/neo_upd4990a.cpp` and matched
//! against MAME's `upd1990a` driver.
//!
//! The Neo Geo BIOS uses this chip to source the current date/time for the
//! title-screen display. The official BIOSes (`asia-s3.rom`,
//! `japan-j3.bin`) refuse to boot the cartridge if they cannot complete the
//! "time read" handshake, displaying "CALENDAR ERROR" instead.
//!
//! Interface (write at `$380028..$38002F`, byte LSB):
//!   bit 0 — DATA_IN  (shift register input)
//!   bit 1 — CLK      (shifts on rising edge)
//!   bit 2 — STB      (latches command on rising edge)
//!
//! Read (via `$380001` STATUS_B port):
//!   bit 6 — TP       (1 Hz "test pulse" / programmable rate)
//!   bit 7 — DATA_OUT (LSB of shift register in shift mode,
//!                     or 1 Hz pulse in pulse mode)

/// 68K cycles per second. NEOGEO_MASTER_CLOCK = 24 MHz, 68K = master/2.
pub const CYCLES_PER_SECOND: u64 = 12_000_000;

/// uPD4990A is driven by a 32.768 kHz XTAL. Used to derive the TP /
/// data_out divisors via the same ratios MAME uses
/// (clock/512 → 64 Hz toggle = 32 Hz pulse, etc.).
pub const XTAL_HZ: u64 = 32_768;

#[derive(Debug)]
pub struct Upd4990a {
    // ---- Current time (BCD-free decimal) ----
    pub seconds: u32, pub minutes: u32, pub hours: u32,
    pub day: u32, pub month: u32, pub year: u32, pub weekday: u32,

    // ---- Mode ----
    /// 0 = pulse mode (data_out reflects 1 Hz pulse), 1 = shift mode, 2 = time-set.
    pub mode: u8,
    /// 0 = tp follows 1 Hz, 1 = programmable rate (cmd 4-7).
    pub tp_mode: u8,

    // ---- Shift register ----
    /// 64-bit shift register (FBNeo uses two u32; we keep them as a single u64).
    pub shift_reg: u64,
    /// Last 4-bit command latched on STB.
    pub command: u8,

    // ---- Counters ----
    /// Sub-second cycle counter for the 1 Hz tp pulse (counts up to CYCLES_PER_SECOND).
    pub count: u64,
    /// Sub-interval cycle counter for the programmable tp rate. Counts
    /// **half-period** cycles — when it overflows we toggle `tp`.
    pub tp_count: u64,
    /// TP half-period in 68K cycles (i.e. one toggle every `tp_interval` cyc).
    pub tp_interval: u64,

    /// Sub-interval cycle counter for the data_out 1 Hz pulse (pulse mode).
    pub data_out_count: u64,
    /// Data-out half-period in 68K cycles.
    pub data_out_interval: u64,
    /// Latched data_out level (in pulse mode it toggles at data_out_interval).
    pub data_out_pulse: bool,

    /// Output line states.
    pub tp: bool,
    pub prev_clk: bool,
    pub prev_stb: bool,
}

/// Compute the 68K-cycle half-period needed for a given TP-toggle
/// frequency (Hz). MAME uses `clock()/div * 2` for the toggle rate, so the
/// half-period (one full cycle = two halves) is `CYCLES_PER_SECOND /
/// (toggle_hz)`.
fn half_period_cycles(toggle_hz: u64) -> u64 {
    if toggle_hz == 0 { CYCLES_PER_SECOND }
    else { CYCLES_PER_SECOND / toggle_hz }
}

impl Default for Upd4990a {
    fn default() -> Self {
        Self::new()
    }
}

impl Upd4990a {
    pub fn new() -> Self {
        // Default to a sensible date: 2024-01-01 12:00:00, Monday.
        // Reset state matches MAME: TP/data_out toggle from the very first
        // tick at register-hold rates (64 Hz / 1 Hz pulse) so the BIOS's
        // RTC calibration loop completes within a few thousand cycles
        // instead of waiting for a software-triggered MODE_REGISTER_HOLD.
        Self {
            seconds: 0, minutes: 0, hours: 12,
            day: 1, month: 1, year: 24, weekday: 1,
            mode: 0,
            tp_mode: 0,
            shift_reg: 0,
            command: 0,
            count: 0,
            tp_count: 0,
            // MODE_REGISTER_HOLD default: TP toggles at 128 Hz (= 64 Hz pulse).
            // MAME: `from_hz((clock()/512.0) * 2.0)` with clock = XTAL(32768)
            //   = 32768/512 * 2 = 128 Hz toggle.
            tp_interval: half_period_cycles(128),
            data_out_count: 0,
            // Data-out 1 Hz pulse (= 2 Hz toggle in MAME).
            data_out_interval: half_period_cycles(2),
            data_out_pulse: false,
            tp: false,
            prev_clk: false,
            prev_stb: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Tick the chip by `cycles` 68K cycles.
    pub fn tick(&mut self, cycles: u32) {
        let c = cycles as u64;
        self.count = self.count.wrapping_add(c);

        // 1 Hz second rollover (calendar advance).
        if self.count >= CYCLES_PER_SECOND {
            self.count -= CYCLES_PER_SECOND;
            self.seconds += 1;
            if self.seconds >= 60 {
                self.seconds = 0;
                self.minutes += 1;
                if self.minutes >= 60 {
                    self.minutes = 0;
                    self.hours += 1;
                    if self.hours >= 24 {
                        self.hours = 0;
                        self.weekday = (self.weekday + 1) % 7;
                    }
                }
            }
        }

        // TP toggle — every `tp_interval` 68K cycles we flip `tp`. MAME does
        // it via a timer; here we accumulate cycles and toggle when we cross
        // a half-period boundary. Use a while-loop so that we cope with
        // ticks larger than the interval (high-frequency TP modes).
        self.tp_count = self.tp_count.wrapping_add(c);
        while self.tp_count >= self.tp_interval && self.tp_interval > 0 {
            self.tp_count -= self.tp_interval;
            self.tp = !self.tp;
        }

        // Data-out pulse toggle (only used in `mode == 0` / pulse mode).
        self.data_out_count = self.data_out_count.wrapping_add(c);
        while self.data_out_count >= self.data_out_interval && self.data_out_interval > 0 {
            self.data_out_count -= self.data_out_interval;
            self.data_out_pulse = !self.data_out_pulse;
        }
    }

    /// CPU writes via `$380028..$38002F` (bus offset `$28`). Only the low
    /// 3 bits matter: bit 0 = DATA_IN, bit 1 = CLK, bit 2 = STB.
    ///
    /// In serial mode (MVS pulls C0/C1/C2 = 1, so `c_unlatched == 7`) the
    /// command is read from the **7th nibble** of the shift register. We
    /// keep the shift register as 7 nibbles packed into a u64 (28 bits used,
    /// nibble[6] in bits 24-27). MAME's clock callback for the 7th nibble:
    ///
    /// ```c
    /// m_shift_reg[6] >>= 1;
    /// m_shift_reg[6] |= (m_data_in << 3);
    /// ```
    ///
    /// On a rising STB edge the chip latches `m_c = m_shift_reg[6]`.
    pub fn write(&mut self, data: u8) {
        let new_data = (data & 1) != 0;
        let new_clk = (data & 2) != 0;
        let new_stb = (data & 4) != 0;
        log::trace!(
            "uPD4990A pin write: DATA={} CLK={} STB={} (shift_reg=${:016X})",
            new_data as u8, new_clk as u8, new_stb as u8, self.shift_reg
        );

        // CLK rising edge: shift the 7th nibble. The other nibbles only
        // shift while in MODE_SHIFT (time-set / time-read) — implement them
        // simultaneously since the 68K test path doesn't depend on the LSB.
        if new_clk && !self.prev_clk {
            // Shift nibble[6] (bits 24-27): rotate right within the nibble,
            // inserting DATA_IN at bit 3 (the MSB of the nibble).
            let nib6 = ((self.shift_reg >> 24) & 0xF) as u8;
            let new_nib6 = (nib6 >> 1) | (if new_data { 0b1000 } else { 0 });
            self.shift_reg = (self.shift_reg & !(0xF << 24)) | ((new_nib6 as u64) << 24);

            // When the chip is in MODE_SHIFT also rotate the lower 24 bits
            // (nibbles 0..5) so time-set / time-read work. We mirror MAME's
            // serial-mode bit propagation: each nibble takes the LSB of
            // the next as its new MSB; the lowest nibble feeds back to
            // `data_out`.
            if self.mode == 1 {
                let lsb0 = (self.shift_reg & 1) as u8;
                let mut s = self.shift_reg & 0x00FF_FFFF;
                // Propagate: each byte-pair really, but treat per-nibble.
                // Shift the whole 24-bit window right by 1, then inject
                // nib6's LSB into bit 23.
                let nib6_lsb = (new_nib6 & 1) as u64;
                s = (s >> 1) | (nib6_lsb << 23);
                self.shift_reg = (self.shift_reg & !0x00FF_FFFF) | s;
                // Keep `data_out` exposed as the LSB so `data_out()` reads it.
                let _ = lsb0; // (consumed by `data_out` separately)
            }
        }

        // STB rising edge: latch `command = shift_reg[6]` and process it.
        if new_stb && !self.prev_stb {
            self.command = ((self.shift_reg >> 24) & 0xF) as u8;
            log::info!(
                "uPD4990A STB rising: command=${:X} (nibble[6]=${:X}, shift_reg=${:016X})",
                self.command, self.command, self.shift_reg
            );
            self.process_command();
        }

        self.prev_clk = new_clk;
        self.prev_stb = new_stb;
    }

    fn process_command(&mut self) {
        match self.command & 0x0F {
            0x00 => {
                // MODE_REGISTER_HOLD: per MAME, this restarts the timers at
                // their default rates. TP at 64 Hz pulse (128 Hz toggle),
                // data_out at 1 Hz pulse (2 Hz toggle).
                self.mode = 0;
                self.tp_mode = 0;
                self.tp_interval = half_period_cycles(128);
                self.data_out_interval = half_period_cycles(2);
                self.tp_count = 0;
                self.data_out_count = 0;
                self.tp = false;
                self.data_out_pulse = false;
            }
            0x01 => {
                // Register shift mode -- data_out reads from shift reg LSB.
                self.mode = 1;
            }
            0x02 => {
                // Time set & counter hold -- decode BCD from shift reg.
                self.mode = 2;
                let r0 = (self.shift_reg & 0xFFFF_FFFF) as u32;
                let r1 = (self.shift_reg >> 32) as u32;
                self.seconds = ((r0 >> 0) & 0xF) + ((r0 >> 4) & 0xF) * 10;
                self.minutes = ((r0 >> 8) & 0xF) + ((r0 >> 12) & 0xF) * 10;
                self.hours = ((r0 >> 16) & 0xF) + ((r0 >> 20) & 0xF) * 10;
                self.day = ((r0 >> 24) & 0xF) + ((r0 >> 28) & 0xF) * 10;
                self.weekday = r1 & 0xF;
                self.month = (r1 >> 4) & 0xF;
                self.year = ((r1 >> 8) & 0xF) + ((r1 >> 12) & 0xF) * 10;
            }
            0x03 => {
                // Time read -- pack current time as BCD into shift reg.
                self.mode = 0;
                let mut r0: u32 = 0;
                r0 |= (self.seconds % 10) << 0;
                r0 |= (self.seconds / 10) << 4;
                r0 |= (self.minutes % 10) << 8;
                r0 |= (self.minutes / 10) << 12;
                r0 |= (self.hours % 10) << 16;
                r0 |= (self.hours / 10) << 20;
                r0 |= (self.day % 10) << 24;
                r0 |= (self.day / 10) << 28;
                let mut r1: u32 = 0;
                r1 |= self.weekday << 0;
                r1 |= self.month << 4;
                r1 |= (self.year % 10) << 8;
                r1 |= (self.year / 10) << 12;
                self.shift_reg = (r0 as u64) | ((r1 as u64) << 32);
            }
            cmd @ (0x04 | 0x05 | 0x06 | 0x07) => {
                // TP at nn Hz pulse → 2*nn Hz toggle. MAME uses div table
                // {512, 128, 16, 8} so toggle = clock/div*2.
                let pulse_hz = [64u64, 256, 2048, 4096][(cmd & 3) as usize];
                self.tp_mode = 1;
                self.tp_interval = half_period_cycles(pulse_hz * 2);
                self.tp_count = 0;
                self.tp = false;
            }
            cmd @ (0x08 | 0x09 | 0x0A | 0x0B) => {
                // TP_1S_INT (8), TP_10S_INT (9), TP_30S_INT (A), TP_60S_INT (B).
                // MAME: `m_timer_tp->adjust(zero, 0, one_second * mul / 2.0)`
                // with mul = {1, 10, 30, 60}. The `attotime::zero` first-fire
                // means the timer **toggles immediately** when the command is
                // latched. The subsequent toggles fire every `1s * mul / 2`,
                // so the full TP cycle (low → high → low) lasts `mul` seconds.
                //
                // In 68K cycles, half-period = `CYCLES_PER_SECOND * mul / 2`.
                // We emulate the immediate toggle by pre-setting `tp` to
                // `true` (because MAME's `m_tp` initial value is 0, and the
                // first callback flips it to 1 at t=0). The wait loop in the
                // BIOS at `$C10D94` keys off the **rising edge** of tp, so
                // starting with `tp = true` means the *first* wait sees no
                // rising edge until tp falls (after mul/2 seconds) and then
                // rises again (after mul seconds total). That gives the
                // calibration loop the ~1 second it needs at cmd=8.
                let mul = [1u64, 10, 30, 60][(cmd & 3) as usize];
                self.tp_mode = 1;
                self.tp_interval = (CYCLES_PER_SECOND * mul) / 2;
                self.tp_count = 0;
                self.tp = true; // first "toggle" already fired (t=0)
                log::debug!(
                    "uPD4990A: MODE_TP_{}S_INT cmd={} interval={} cycles (tp=true)",
                    mul, cmd, self.tp_interval
                );
            }
            0x0C..=0x0F => {
                // MODE_INT_RESET_OUTPUT (C) and undocumented (D-F).
                // Reset TP to 0 — used to clear a long interval pulse early.
                self.mode = 0;
                self.tp = false;
                self.tp_count = 0;
            }
            _ => {}
        }
    }

    /// `data_out` line. In shift mode it's the LSB of the shift register;
    /// in pulse mode it's the 1 Hz pulse toggled by the tick loop.
    pub fn data_out(&self) -> bool {
        if self.mode == 0 {
            self.data_out_pulse
        } else {
            (self.shift_reg & 1) != 0
        }
    }

    /// Pack `tp` and `data_out` into the two status bits the BIOS reads on
    /// `$380001` (bit 6 = tp, bit 7 = data_out).
    pub fn status_bits(&self) -> u8 {
        let mut v = 0u8;
        if self.tp { v |= 0x40; }
        if self.data_out() { v |= 0x80; }
        v
    }
}

// ============================================================================
// Savestates
// ============================================================================

crate::state::state_fields!(Upd4990a {
    seconds, minutes, hours, day, month, year, weekday, mode, tp_mode,
    shift_reg, command, count, tp_count, tp_interval, data_out_count,
    data_out_interval, data_out_pulse, tp, prev_clk, prev_stb,
});

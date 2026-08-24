//! Neo Geo audio subsystem — Z80 bus + soundlatch + bank windows.
//!
//! Memory map of the Z80 (as the audio CPU sees it):
//!
//! ```text
//!   $0000-$7FFF  bank_main  — SM1 audio BIOS *or* M1 cartridge ROM,
//!                              selected by `use_cart_audio` (system reg).
//!   $8000-$BFFF  bank_8000  — 16 KiB window into M1 (region 3, step 16 KiB).
//!   $C000-$DFFF  bank_C000  —  8 KiB window into M1 (region 2, step  8 KiB).
//!   $E000-$EFFF  bank_E000  —  4 KiB window into M1 (region 1, step  4 KiB).
//!   $F000-$F7FF  bank_F000  —  4 KiB window into M1 (region 0, step  2 KiB).
//!   $F800-$FFFF  ram        —  2 KiB on-chip Z80 RAM.
//! ```
//!
//! I/O map (port low byte; high byte often carries the bank index for $08-$0B):
//!
//! ```text
//!   $00  R: soundlatch read + ACK (clears NMI pending)
//!        W: clear soundlatch (also valid via the same port)
//!   $04  R: YM2610 status register (port 0)
//!        W: YM2610 address A (port 0)
//!   $05  W: YM2610 data A    (port 1)
//!   $06  R: YM2610 ADPCM-A end flags (port 2)
//!        W: YM2610 address B (port 2)
//!   $07  W: YM2610 data B    (port 3)
//!   $08  W: enable Z80 NMI from soundlatch
//!        R: select bank for $F000-$F7FF window (bank# = port_hi)
//!   $09  R: select bank for $E000-$EFFF window
//!   $0A  R: select bank for $C000-$DFFF window
//!   $0B  R: select bank for $8000-$BFFF window
//!   $0C  W: write soundlatch2 (Z80 → 68K reply)
//!   $18  W: disable Z80 NMI from soundlatch
//! ```
//!
//! Verbatim references: MAME `src/mame/snk/neogeo.cpp` lines 880-925,
//! 1090-1130, 1305-1360, 1780-1810.

use crate::audio::ym2610::Ym2610;
use crate::cpu::z80::Z80Bus;

/// 2 KiB on-chip Z80 RAM at $F800-$FFFF.
pub const Z80_RAM_SIZE: usize = 0x800;

/// Audio CPU bus. Owns M1 ROM, SM1 audio-BIOS, Z80 RAM and the YM2610.
pub struct AudioBus {
    /// M1 ROM (cartridge audio program). Length is `0x10000 + 4 * 0x10000` or
    /// less for non-banked games; mslug is exactly `0x20000` (128 KiB).
    pub m1_rom: Vec<u8>,
    /// SM1 audio-BIOS (only used if the user supplied an `sm1.sm1` file).
    /// When empty, the main bank always reads from M1.
    pub sm1_rom: Vec<u8>,
    /// 2 KiB on-chip Z80 RAM.
    pub ram: Box<[u8; Z80_RAM_SIZE]>,
    /// YM2610.
    pub ym: Ym2610,

    /// Selected bank for each window (0..0xFF), index = MAME region.
    /// 0 = $F000 window, 1 = $E000, 2 = $C000, 3 = $8000.
    pub bank_sel: [u8; 4],

    /// `use_cart_audio` flag from the 68K system latch (bit 1). When 0,
    /// the main bank reads from `sm1_rom`; when 1, from `m1_rom`.
    /// MAME default for cartridge games is 1.
    pub use_cart_audio: bool,

    /// Soundlatch byte written by the 68K to $320000 — read by Z80 at IN($00).
    pub soundlatch: u8,
    /// Pending bit — true when 68K has written and Z80 hasn't ACKed yet.
    pub soundlatch_pending: bool,
    /// Reply latch — written by Z80 at OUT($0C), read by 68K at $320000.
    pub soundlatch2: u8,

    /// NMI enable line driven by Z80 (OUT $08 = enable, OUT $18 = disable).
    /// The actual NMI fires when both this AND `soundlatch_pending` are true.
    pub nmi_enable: bool,
    /// Latched edge state for the Z80 NMI line. Real hardware asserts NMI when
    /// a sound command becomes pending while NMI is enabled, and keeps the line
    /// asserted until the Z80 acknowledges the latch (IN/OUT $00) or disables
    /// NMI (OUT $18). Without this gate, re-requesting NMI every 68K step traps
    /// the Z80 forever at vector $0066 and the sound driver never reads port $00.
    pub nmi_asserted: bool,

    /// Trace I/O for debugging (`RUST_LOG=neogeo_core::audio=trace`).
    pub trace: bool,
}

impl Default for AudioBus {
    fn default() -> Self { Self::new() }
}

impl AudioBus {
    pub fn new() -> Self {
        Self {
            m1_rom: Vec::new(),
            sm1_rom: Vec::new(),
            ram: Box::new([0; Z80_RAM_SIZE]),
            ym: Ym2610::new(),
            // Initial banks per MAME hack — see neogeo.cpp lines 1346-1349.
            bank_sel: [0x1E, 0x0E, 0x06, 0x02],
            use_cart_audio: true,
            soundlatch: 0,
            soundlatch_pending: false,
            soundlatch2: 0,
            nmi_enable: false,
            nmi_asserted: false,
            trace: false,
        }
    }

    /// Install M1 ROM (cartridge audio program) and optionally SM1 (audio BIOS).
    pub fn install_m1(&mut self, m1: Vec<u8>) {
        log::info!("AudioBus: M1 ROM installed, {} bytes", m1.len());
        self.m1_rom = m1;
    }
    pub fn install_sm1(&mut self, sm1: Vec<u8>) {
        log::info!("AudioBus: SM1 ROM installed, {} bytes", sm1.len());
        self.sm1_rom = sm1;
    }

    /// Compute the byte offset into M1 ROM for `region` (0..3) and the
    /// configured bank index.
    ///
    /// Verbatim port of FBNeo `neo_run.cpp::NeoZ80SetBankN`. Each window
    /// addresses the **whole** M1 ROM from offset 0 (NOT `0x10000 + ...`):
    ///   region 0 ($F000, 2 KiB): base = (bank & 0x7F) << 11
    ///   region 1 ($E000, 4 KiB): base = (bank & 0x3F) << 12
    ///   region 2 ($C000, 8 KiB): base = (bank & 0x1F) << 13
    ///   region 3 ($8000,16 KiB): base = (bank & 0x0F) << 14
    /// The bank number (port high byte) is masked **before** the shift, per
    /// region. The previous implementation added a spurious `0x10000` and
    /// masked the post-shift offset, which sent the sound driver's music /
    /// instrument tables to the wrong half of the M1 ROM, so the Z80
    /// sequencer never emitted any FM/ADPCM key-on (silent output).
    fn bank_base_for(&self, region: usize) -> usize {
        if self.m1_rom.is_empty() {
            return 0;
        }
        let bank = self.bank_sel[region] as u32;
        let (bank_mask, shift): (u32, u32) = match region {
            0 => (0x7F, 11), // $F000 window — 2 KiB
            1 => (0x3F, 12), // $E000 window — 4 KiB
            2 => (0x1F, 13), // $C000 window — 8 KiB
            _ => (0x0F, 14), // $8000 window — 16 KiB
        };
        let off = ((bank & bank_mask) << shift) as usize;
        // Wrap into the ROM by its (power-of-two) size for safety.
        off & (self.m1_rom.len() - 1)
    }

    /// Read a byte from the Z80 memory space.
    fn mem_read(&self, addr: u16) -> u8 {
        let a = addr as usize;
        match a {
            0x0000..=0x7FFF => {
                // Main bank — SM1 audio BIOS *or* M1 cart, with bank-0 picking
                // SM1 when `use_cart_audio == false`.
                let src = if !self.use_cart_audio && !self.sm1_rom.is_empty() {
                    &self.sm1_rom[..]
                } else {
                    &self.m1_rom[..]
                };
                if a < src.len() { src[a] } else { 0xFF }
            }
            0x8000..=0xBFFF => {
                let base = self.bank_base_for(3);
                let off = base + (a - 0x8000);
                if off < self.m1_rom.len() { self.m1_rom[off] } else { 0xFF }
            }
            0xC000..=0xDFFF => {
                let base = self.bank_base_for(2);
                let off = base + (a - 0xC000);
                if off < self.m1_rom.len() { self.m1_rom[off] } else { 0xFF }
            }
            0xE000..=0xEFFF => {
                let base = self.bank_base_for(1);
                let off = base + (a - 0xE000);
                if off < self.m1_rom.len() { self.m1_rom[off] } else { 0xFF }
            }
            0xF000..=0xF7FF => {
                let base = self.bank_base_for(0);
                let off = base + (a - 0xF000);
                if off < self.m1_rom.len() { self.m1_rom[off] } else { 0xFF }
            }
            0xF800..=0xFFFF => self.ram[(a - 0xF800) & (Z80_RAM_SIZE - 1)],
            _ => 0xFF,
        }
    }

    fn mem_write(&mut self, addr: u16, val: u8) {
        let a = addr as usize;
        if (0xF800..=0xFFFF).contains(&a) {
            self.ram[(a - 0xF800) & (Z80_RAM_SIZE - 1)] = val;
        }
        // Writes to ROM regions are silently ignored.
    }

    fn io_read(&mut self, port: u16) -> u8 {
        let lo = (port & 0xFF) as u8;
        let hi = ((port >> 8) & 0xFF) as u8;
        let v = match lo {
            0x00 => {
                // Read soundlatch + acknowledge (clears pending → drops NMI line).
                let v = self.soundlatch;
                log::trace!("Z80 IN  $0000 = ${:02X} (soundlatch ack)", v);
                self.soundlatch_pending = false;
                self.nmi_asserted = false;
                v
            }
            0x04 => self.ym.read_port(0),
            0x05 => self.ym.read_port(1),
            0x06 => self.ym.read_port(2),
            0x07 => self.ym.read_port(3),

            // 0x08..0x0B reads select banks; offset (lo & 3) is region,
            // hi byte is bank number. MAME `audio_cpu_bank_select_r`.
            0x08 | 0x09 | 0x0A | 0x0B => {
                let region = (lo & 3) as usize;
                self.bank_sel[region] = hi;
                if self.trace {
                    log::trace!("Z80 BANK region={region} -> bank=${hi:02X}");
                }
                0  // returns 0 per MAME
            }
            _ => 0xFF,
        };
        if self.trace { log::trace!("Z80 IN  ${port:04X} = ${v:02X}"); }
        v
    }

    fn io_write(&mut self, port: u16, val: u8) {
        let lo = (port & 0xFF) as u8;
        if self.trace { log::trace!("Z80 OUT ${port:04X} <- ${val:02X}"); }
        match lo {
            0x00 => {
                // Clearing the latch (writes are valid too).
                self.soundlatch_pending = false;
                self.nmi_asserted = false;
            }
            0x04 => self.ym.write_port(0, val),
            0x05 => self.ym.write_port(1, val),
            0x06 => self.ym.write_port(2, val),
            0x07 => self.ym.write_port(3, val),

            // 0x08 enables NMI, 0x18 disables it. MAME differentiates by bit 4.
            0x08 => {
                self.nmi_enable = true;
                if self.trace {
                    log::trace!("Z80 NMI enable (pending={} asserted={})", self.soundlatch_pending, self.nmi_asserted);
                }
            }
            0x18 => {
                self.nmi_enable = false;
                self.nmi_asserted = false;
                if self.trace {
                    log::trace!("Z80 NMI disable");
                }
            },

            // 0x0C: Z80 reply to 68K (read at $320000 via soundlatch2).
            0x0C => self.soundlatch2 = val,

            _ => {}
        }
    }
}

/// Adapter implementing `Z80Bus` over an `&mut AudioBus`. Used by the Z80
/// core's `step()` method.
pub struct AudioBusRef<'a> {
    pub bus: &'a mut AudioBus,
}

impl Z80Bus for AudioBusRef<'_> {
    fn read(&mut self, addr: u16) -> u8 { self.bus.mem_read(addr) }
    fn write(&mut self, addr: u16, value: u8) { self.bus.mem_write(addr, value); }
    fn io_read(&mut self, port: u16) -> u8 { self.bus.io_read(port) }
    fn io_write(&mut self, port: u16, value: u8) { self.bus.io_write(port, value); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_audio_bank_entries_match_mame_bootstrap() {
        let bus = AudioBus::new();
        assert_eq!(bus.bank_sel, [0x1E, 0x0E, 0x06, 0x02]);
    }

    #[test]
    fn bank_base_for_matches_fbneo_masks_and_shifts() {
        let mut bus = AudioBus::new();
        bus.m1_rom = vec![0; 0x20000];
        bus.bank_sel = [0x7F, 0x3F, 0x1F, 0x0F];

        assert_eq!(bus.bank_base_for(0), 0x3F800 & (bus.m1_rom.len() - 1));
        assert_eq!(bus.bank_base_for(1), 0x3F000 & (bus.m1_rom.len() - 1));
        assert_eq!(bus.bank_base_for(2), 0x3E000 & (bus.m1_rom.len() - 1));
        assert_eq!(bus.bank_base_for(3), 0x3C000 & (bus.m1_rom.len() - 1));
    }
}

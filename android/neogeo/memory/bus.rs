//! Neo Geo memory bus implementing the official 68k memory map.
//!
//! Source: NeoGeo Development Wiki — *68k memory map* — and Copetti's
//! *Neo Geo architecture* article.
//!
//! ```text
//! $000000-$0FFFFF  P-ROM bank 0 (vector table + program)   [1 MiB]
//! $100000-$10FFFF  Work RAM (64 KiB) + mirror up to $1FFFFF
//! $200000-$2FFFFF  P-ROM bank 1 (bank-switched)            [up to 1 MiB]
//! $300000-$3FFFFF  I/O area (NEO-C1, NEO-F0, LSPC regs)
//! $400000-$401FFF  Palette RAM (8 KiB) + mirror
//! $800000-$BFFFFF  Memory card                             [MVS]
//! $C00000-$C1FFFF  System ROM (BIOS)                       [128 KiB]
//! $D00000-$D0FFFF  Backup RAM (MVS only)
//! ```

use crate::cpu::m68k::bus::Bus;

use crate::graphics::lspc::Lspc;
use crate::memory::prot::{CartProt, SmaGame};
use crate::memory::upd4990a::Upd4990a;

const WORK_RAM_SIZE: usize = 0x10000; // 64 KiB
/// Palette RAM: 2 banks of 0x1000 u16 entries each = 0x2000 u16 = **16 KiB**.
/// MAME: `std::vector<uint16_t> m_paletteram(0x1000 * 2)`, with
/// `m_palette_bank` (0 or 0x1000) added to the u16 offset on every R/W.
/// We store the same layout as a flat byte array, so byte_offset =
/// `(bank_u16_offset << 1) | (addr & 1)`.
const PALETTE_RAM_SIZE: usize = 0x4000; // 16 KiB (2 banks of 0x2000 bytes)
const BACKUP_RAM_SIZE: usize = 0x10000; // 64 KiB (MVS only)
/// Memory card SRAM size. The original SNK card and what MAME's
/// `ng_memcard_device` models is 2 KiB. The BIOS R/W self-test only
/// touches the first 2 KiB anyway (`MOVE.W #$07FF, D7`).
const MEMCARD_RAM_SIZE: usize = 0x800;

#[derive(Debug)]
pub struct NeoGeoBus {
    /// BIOS / System ROM at $C00000..$C20000 (128 KiB max).
    pub system_rom: Vec<u8>,
    /// Cartridge P ROM (program for 68000), banked.
    pub p_rom: Vec<u8>,
    /// 68000 work RAM at $100000..$110000.
    pub work_ram: Box<[u8; WORK_RAM_SIZE]>,
    /// Banked palette RAM at $400000..$402000.
    pub palette_ram: Box<[u8; PALETTE_RAM_SIZE]>,
    /// Backup RAM (MVS only) at $D00000..$D10000.
    pub backup_ram: Box<[u8; BACKUP_RAM_SIZE]>,
    /// 2 KiB Memory Card SRAM mapped at $800000..$BFFFFF.
    ///
    /// Connected to D0..D7 of the 68K data bus (the LOW byte / odd address)
    /// but its chip-select is /UDS, so it is only enabled on word or
    /// upper-byte accesses — exactly what the BIOS R/W test uses.
    /// MAME's `ng_memcard_device::read` returns `0xff00 | data[off]` and
    /// `write` stores `data & 0x00ff`, gated by `!lock1 && unlock2`.
    pub memcard_ram: Box<[u8; MEMCARD_RAM_SIZE]>,
    /// True when the memory card is considered present (CD1=CD2=0 in
    /// REG_STATUS_B). We always model an inserted card so the BIOS R/W
    /// self-test can succeed.
    pub memcard_present: bool,
    /// LSPC graphics processor (VRAM + sprite control).
    pub lspc: Lspc,
    /// uPD4990A RTC/calendar chip (`$380028..$38002F` write, status bits
    /// 6 & 7 of `$380001` read). The official BIOS refuses to boot until
    /// it sees a 1 Hz tp pulse.
    pub upd4990a: Upd4990a,
    /// Active P-ROM bank for the $200000-$2FFFFF window.
    /// Byte offset into the cart P-ROM that the bus $200000-$2FFFFF window
    /// currently maps to. Set by writes to $2FFFF0-$2FFFFF (MAME
    /// `write_banksel`). Default at reset: $100000 (the upper MiB) for
    /// carts > 1 MiB, 0 for carts ≤ 1 MiB (MAME `init_cpu`).
    pub p_rom_bank_offset: usize,
    /// Whether the SYSTEM ROM is mapped at $000000 (true at reset, on MVS the
    /// BIOS-mapped reset is replaced by the cartridge once REG_SWPBIOS is
    /// written). For now we always boot directly to cart vectors at $0.
    pub bios_at_zero: bool,
    /// HC259 system-latch bits (8 outputs).
    /// Bit 0 = screen_shadow, 1 = use_cart_vectors, 2 = memcard_lock1,
    /// 3 = memcard_unlock2, 4 = memcard_regsel, 5 = use_cart_audio (also
    /// selects S-ROM source: 0 = sfix.sfix / 1 = cart s1.bin),
    /// 6 = save_ram_unlock, 7 = palette_bank.
    pub systemlatch: u8,
    /// REG_DIPSW value (MVS hardware DIP switches; mostly $FF on AES).
    pub dipsw: u8,
    /// REG_SYSTYPE (MVS slot ID etc).
    pub systype: u8,
    /// Last value written by 68000 to REG_SOUND ($320000) — read by Z80.
    pub sound_latch: u8,
    /// Set to true when the 68K writes a new soundlatch and the Z80
    /// has not yet acknowledged it. Used by `System` to assert the Z80
    /// NMI line (gated by `AudioBus::nmi_enable`).
    pub sound_latch_pending: bool,
    /// Last value written by Z80 to its reply port — read by 68000 from REG_STATUS_B.
    pub sound_reply: u8,
    /// Player 1 controller bits (active low) — read from REG_P1CNT.
    pub p1_input: u8,
    /// Player 2 controller bits (active low).
    pub p2_input: u8,
    /// Coin / start / service bits — read from REG_STATUS_B low bits.
    pub start_select: u8,
    /// REG_STATUS_A coin / service inputs (active LOW).
    /// Bit 0 = Coin-in 1, Bit 1 = Coin-in 2, Bit 2 = Service,
    /// Bit 3 = Coin-in 3, Bit 4 = Coin-in 4. Bits 5..7 are reserved for
    /// hardware status / RTC and are composed at read time. Default $FF
    /// means "nothing pressed" — the BIOS waits indefinitely until a
    /// coin or service bit drops to 0. See:
    /// https://wiki.neogeodev.org/index.php/Memory_mapped_registers#REG_STATUS_A
    pub coin_inputs: u8,
    /// Watchdog countdown in 68K cycles. Resets to `WATCHDOG_TIMEOUT_68K`
    /// on any write to `$300001`; ticked down by `tick_watchdog` each step;
    /// when it reaches zero the system requests a hard reset.
    pub watchdog_cycles: i64,
    /// Set to true when the watchdog has expired — the system must reset
    /// the CPU and re-arm the watchdog.
    pub watchdog_expired: bool,
    /// Active cartridge protection device (MAME `set_slot_idx` cart-type
    /// switch). `CartProt::None` for unprotected carts.
    pub prot: CartProt,
}

/// MAME `WATCHDOG_TIMER(...).set_time(attotime::from_ticks(3244030, NEOGEO_MASTER_CLOCK))`.
/// Master clock = 24 MHz, 68000 runs at master/2 = 12 MHz, so:
///   `watchdog_68k_cycles = 3_244_030 / 2 ≈ 1_622_015` cycles ≈ 0.135 s.
pub const WATCHDOG_TIMEOUT_68K: i64 = 1_622_015;

impl NeoGeoBus {
    pub fn new() -> Self {
        Self {
            system_rom: vec![0xFF; 0x20000],
            p_rom: vec![0xFF; 0x100000],
            work_ram: Box::new([0; WORK_RAM_SIZE]),
            palette_ram: Box::new([0; PALETTE_RAM_SIZE]),
            backup_ram: Box::new([0; BACKUP_RAM_SIZE]),
            memcard_ram: Box::new([0xFF; MEMCARD_RAM_SIZE]),
            memcard_present: true,
            lspc: Lspc::new(),
            upd4990a: Upd4990a::new(),
            p_rom_bank_offset: 0x100000,
            bios_at_zero: false,
            systemlatch: 0,
            dipsw: 0xFF,
            systype: 0x00, // AES-like default
            sound_latch: 0,
            sound_latch_pending: false,
            // Default sound_reply = 1: the BIOS Z80 handshake at $C10FAE
            // expects `sound_reply == 1` to skip the wait loop. Without a
            // running Z80 we mimic an already-acknowledged reply so the
            // BIOS can advance past the audio init phase. The Z80, once
            // emulated, will overwrite this via its own reply port.
            sound_reply: 1,
            p1_input: 0xFF,
            p2_input: 0xFF,
            start_select: 0xFF,
            coin_inputs: 0xFF,
            watchdog_cycles: WATCHDOG_TIMEOUT_68K,
            watchdog_expired: false,
            prot: CartProt::None,
        }
    }

    /// Protection-device reads shadowing the $200000-$2FFFFF window.
    /// `a` is the even (word-aligned) bus address. Returns `Some(word)`
    /// when a device claims the address, `None` to fall through to the
    /// P-ROM bank window. Mirrors MAME `set_slot_idx` read handlers.
    fn prot_read16(&mut self, a: u32) -> Option<u16> {
        // Take the device out to sidestep borrow conflicts when a device
        // needs to read back through the bus (Metal Slug X).
        let mut prot = std::mem::take(&mut self.prot);
        let result = match &mut prot {
            CartProt::None => None,
            // fatfury2 / ssideki: whole window.
            CartProt::FatFury2(p) => Some(p.protection_r((a - 0x200000) >> 1)),
            // kof98's read overlay lives at $100 (handled in read_phys8).
            CartProt::Kof98(_) => None,
            // mslugx: $2FFFE0-$2FFFEF.
            CartProt::MslugX(p) => {
                if (0x2FFFE0..=0x2FFFEF).contains(&a) {
                    let select_word = u16::from_be_bytes([
                        self.work_ram[0xF00A],
                        self.work_ram[0xF00B],
                    ]);
                    let p_rom = &self.p_rom;
                    let r = p.protection_r(
                        |addr| *p_rom.get(addr as usize).unwrap_or(&0),
                        select_word,
                    );
                    log::trace!(
                        "mslugx prot_r ${a:06X} cmd={:04X} sel={select_word:04X} -> {r:04X}",
                        p.command
                    );
                    Some(r)
                } else {
                    None
                }
            }
            CartProt::Sma(p) => {
                // $2FE446 handshake is common to every SMA game.
                if a == 0x2FE446 {
                    Some(p.prot_9a37_r())
                } else {
                    let rng_addrs: &[u32] = match p.game {
                        SmaGame::Kof99 => &[0x2FFFF8, 0x2FFFFA],
                        SmaGame::Garou | SmaGame::GarouH => &[0x2FFFCC, 0x2FFFF0],
                        SmaGame::Mslug3 | SmaGame::Mslug3a => &[],
                        SmaGame::Kof2000 => &[0x2FFFD8, 0x2FFFDA],
                    };
                    if rng_addrs.contains(&a) {
                        Some(p.random_r())
                    } else {
                        None
                    }
                }
            }
            // PVC: 8KiB cart RAM at $2FE000-$2FFFFF.
            CartProt::Pvc(p) => {
                if (0x2FE000..=0x2FFFFF).contains(&a) {
                    Some(p.protection_r(((a - 0x2FE000) >> 1) as usize))
                } else {
                    None
                }
            }
        };
        self.prot = prot;
        result
    }

    /// Protection-device writes over $200000-$2FFFFF, 16-bit with byte-lane
    /// mask (MAME `COMBINE_DATA` semantics). `word_addr` must be even.
    /// Returns `true` when a device consumed the access. Mirrors MAME
    /// `set_slot_idx` write handlers + `write_bankprot`.
    ///
    /// A word write passes `mem_mask = 0xFFFF`. A byte write passes the
    /// value replicated in its lane with mask `0xFF00` (even address) or
    /// `0x00FF` (odd). This is critical for PVC: kof2003's bank routine at
    /// $14520 does `move.b d0,$2FFFF0.l` — a lone BYTE write to the high
    /// lane of the bank register. A latch scheme that waits for the odd
    /// byte drops it and the game bankswitches to the wrong base.
    fn prot_write16(&mut self, word_addr: u32, data: u16, mem_mask: u16) -> bool {
        // Which word addresses does the current device claim?
        let claimed = match &self.prot {
            CartProt::None => false,
            CartProt::FatFury2(_) => true, // whole window
            CartProt::Kof98(_) => word_addr == 0x20AAAA,
            CartProt::MslugX(_) => (0x2FFFE0..=0x2FFFEF).contains(&word_addr),
            CartProt::Sma(p) => {
                let bank_reg: u32 = match p.game {
                    SmaGame::Kof99 => 0x2FFFF0,
                    SmaGame::Garou | SmaGame::GarouH => 0x2FFFC0,
                    SmaGame::Mslug3 | SmaGame::Mslug3a => 0x2FFFE4,
                    SmaGame::Kof2000 => 0x2FFFEC,
                };
                word_addr == bank_reg
            }
            CartProt::Pvc(_) => (0x2FE000..=0x2FFFFF).contains(&word_addr),
        };
        if !claimed {
            return false;
        }
        match &mut self.prot {
            CartProt::FatFury2(p) => p.protection_w((word_addr - 0x200000) >> 1, data),
            CartProt::Kof98(p) => p.protection_w(data),
            CartProt::MslugX(p) => p.protection_w((word_addr - 0x2FFFE0) >> 1, data),
            CartProt::Sma(p) => {
                // MAME `write_bankprot`: scrambled banksel.
                let base = p.bank_base(data);
                self.p_rom_bank_offset = base;
                log::trace!(
                    "SMA bankswitch: sel={data:04X} -> P offset ${base:08X}"
                );
            }
            CartProt::Pvc(p) => {
                let offset = ((word_addr - 0x2FE000) >> 1) as usize;
                if let Some(base) = p.protection_w(offset, data, mem_mask) {
                    self.p_rom_bank_offset = base;
                    log::trace!("PVC bankswitch -> P offset ${base:08X}");
                }
            }
            CartProt::None => unreachable!(),
        }
        true
    }

    /// Advance the watchdog by `cycles` 68K cycles. Returns `true` if the
    /// watchdog has just expired (caller should reset the CPU).
    pub fn tick_watchdog(&mut self, cycles: u32) -> bool {
        if self.watchdog_expired {
            return false;
        }
        self.watchdog_cycles = self.watchdog_cycles.saturating_sub(cycles as i64);
        if self.watchdog_cycles <= 0 {
            self.watchdog_expired = true;
            log::info!("WATCHDOG expired — issuing hard reset");
            return true;
        }
        false
    }

    /// Reload the watchdog. Called whenever the CPU writes to `$300001`.
    pub fn kick_watchdog(&mut self) {
        self.watchdog_cycles = WATCHDOG_TIMEOUT_68K;
    }

    /// Replace the contents of the System ROM (BIOS).
    ///
    /// Neo Geo BIOS dumps are stored byte-swapped within each 16-bit word
    /// (MAME loads them with `ROM_GROUPWORD | ROM_REVERSE`). We undo the
    /// swap here so subsequent big-endian reads return the correct opcode
    /// bytes.
    pub fn load_system_rom(&mut self, data: Vec<u8>) {
        let mut buf = vec![0xFF; 0x20000];
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        for chunk in buf.chunks_exact_mut(2) {
            chunk.swap(0, 1);
        }
        self.system_rom = buf;
        // Hardware behaviour: BIOS is mapped at $000000–$00007F at reset
        // until the BIOS itself writes to $3A0011 (REG_SWPBIOS).
        self.bios_at_zero = true;
    }

    /// Replace the cartridge P-ROM contents.
    ///
    /// Like the BIOS, cartridge P-ROM dumps are byte-swapped within each
    /// 16-bit word (MAME loads them with `ROM_LOAD16_WORD_SWAP`).
    pub fn load_p_rom(&mut self, mut data: Vec<u8>) {
        for chunk in data.chunks_exact_mut(2) {
            chunk.swap(0, 1);
        }
        self.p_rom = data;
        // Re-initialise the $200000 bank window per MAME `init_cpu`:
        // carts > 1 MiB point the window at the second MiB; smaller carts
        // leave it at 0 (the window mirrors the base ROM).
        self.p_rom_bank_offset = if self.p_rom.len() > 0x100000 { 0x100000 } else { 0 };
    }

    /// Read one byte by following the Neo Geo memory map. This is the
    /// authoritative routine — read16/read32 default impls in the `Bus`
    /// trait fall back to two/four read8 calls.
    fn read_phys8(&mut self, addr: u32) -> u8 {
        let a = addr & 0x00FF_FFFF;
        match a {
            0x000000..=0x00007F => {
                // Banked vector table: only the first 128 bytes are remapped.
                // When `bios_at_zero` is true (the default at reset) the BIOS
                // provides the reset SSP/PC and the exception vectors; the
                // BIOS itself flips the bit via $3A0011 (REG_SWPBIOS) to
                // expose the cartridge vectors before transferring control.
                if self.bios_at_zero && !self.system_rom.is_empty() {
                    self.system_rom[a as usize]
                } else if !self.p_rom.is_empty() {
                    *self.p_rom.get(a as usize).unwrap_or(&0xFF)
                } else {
                    0xFF
                }
            }
            0x000080..=0x0FFFFF => {
                // KOF98 boot overlay at $100-$103 (MAME installs
                // protection_r over 0x00100-0x00103).
                if (0x000100..=0x000103).contains(&a) {
                    if let CartProt::Kof98(p) = &self.prot {
                        let w = p.protection_r((a >> 1) & 1);
                        return if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 };
                    }
                }
                // Cartridge P-ROM in normal operation, independent of the
                // vector swap.
                if !self.p_rom.is_empty() {
                    let off = a as usize;
                    *self.p_rom.get(off).unwrap_or(&0xFF)
                } else {
                    0xFF
                }
            }
            0x100000..=0x1FFFFF => self.work_ram[(a as usize) & 0xFFFF],
            // $800000..$BFFFFF: Memory card. The JEIDA 8-bit card is wired
            // to D0..D7 of the bus, but chip-select is /UDS — so even-byte
            // accesses return open-bus ($FF) and odd-byte accesses return
            // the SRAM byte at `(addr>>1) & 0x7FF`.
            // Equivalently (per MAME): word reads return `$FF00 | data`.
            0x800000..=0xBFFFFF => {
                if !self.memcard_present {
                    return 0xFF;
                }
                if a & 1 == 0 {
                    // Even byte ($800000, $800002…): open-bus / /UDS side.
                    0xFF
                } else {
                    let off = ((a as usize) >> 1) & 0x7FF;
                    self.memcard_ram[off]
                }
            }
            0x200000..=0x2FFFFF => {
                // Protection devices shadow parts of this window (MAME
                // `set_slot_idx` install_read_handler calls). They are
                // 16-bit devices; route through `prot_read16` and pick
                // the byte lane.
                if let Some(w) = self.prot_read16(a & !1) {
                    return if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 };
                }
                // Bank window: bus $200000-$2FFFFF maps 1 MiB of the cart
                // P-ROM selected by the banksel register. For carts ≤ 1 MiB
                // the offset is 0 so the window mirrors the base ROM (MAME
                // `init_cpu` behaviour).
                let cart_off = self.p_rom_bank_offset + ((a as usize) & 0xFFFFF);
                *self.p_rom.get(cart_off).unwrap_or(&0xFF)
            }
            0x300000..=0x3FFFFF => self.io_read8(a),
            0x400000..=0x7FFFFF => {
                // Palette RAM has two banks of $1000 u16 entries each
                // ($2000 bytes per bank). Bit 7 of the system latch
                // selects which bank is visible to the CPU at any time.
                // The CPU address is byte-aligned, so we keep the low 13
                // bits (= $1FFF, one bank's byte range) and OR-in the
                // bank offset ($2000) when bank 1 is selected.
                let bank = if (self.systemlatch & 0x80) != 0 { 0x2000 } else { 0 };
                let off = bank | ((a as usize) & 0x1FFF);
                self.palette_ram[off]
            }
            0xC00000..=0xCFFFFF => {
                let off = (a as usize) & 0x1FFFF;
                self.system_rom.get(off).copied().unwrap_or(0xFF)
            }
            0xD00000..=0xDFFFFF => self.backup_ram[(a as usize) & 0xFFFF],
            _ => {
                log::trace!("bus read8 from unmapped ${a:06X}");
                0xFF
            }
        }
    }

    fn write_phys8(&mut self, addr: u32, value: u8) {
        let a = addr & 0x00FF_FFFF;
        match a {
            0x000000..=0x0FFFFF => {
                log::trace!("write to P-ROM ${a:06X} = ${value:02X} (ignored)");
            }
            0x100000..=0x1FFFFF => {
                let off = (a as usize) & 0xFFFF;
                if off == 0xFEE4 || off == 0xFEE3 || off == 0xFE8C {
                    log::debug!("WORK_RAM write ${a:06X} = ${value:02X}");
                }
                // Track when we cross 57 (start of calendar-error pass window).
                if off == 0xFEE4 && (value == 57 || value == 63 || value == 64) {
                    log::info!("BIOS_INT1_FRAME_COUNTER hit ${value:02X} ({value})");
                }
                self.work_ram[off] = value;
            }
            // $800000..$BFFFFF: Memory card writes. The card is gated by
            // `lock1 == 0 && unlock2 == 1` (74HC259 outputs 2 and 3). Per
            // MAME's ng_memcard, writes only happen on /UDS-asserted cycles
            // — for our split byte-at-a-time bus that means the ODD byte of
            // each word write reaches SRAM (high byte is open-bus).
            0x800000..=0xBFFFFF => {
                if !self.memcard_present {
                    return;
                }
                let lock1 = (self.systemlatch >> 2) & 1;
                let unlock2 = (self.systemlatch >> 3) & 1;
                let write_enabled = lock1 == 0 && unlock2 == 1;
                if !write_enabled {
                    log::trace!(
                        "memcard write blocked: ${a:06X} = ${value:02X} (lock1={lock1} unlock2={unlock2})"
                    );
                    return;
                }
                if a & 1 == 1 {
                    // ODD byte of the word = D0..D7 of the bus = SRAM data byte.
                    let off = ((a as usize) >> 1) & 0x7FF;
                    self.memcard_ram[off] = value;
                }
                // EVEN byte ($800000, $800002…) writes only affect open-bus
                // (D8..D15) — the card is wired only to D0..D7 — so we drop
                // them silently.
            }
            0x200000..=0x2FFFFF => {
                // Protection devices first (MAME installs their write
                // handlers over this window per cart type). These are
                // 16-bit devices; a byte access hits one lane, so pass
                // COMBINE_DATA-style (data, mem_mask). Word writes come
                // in through `write16` below and never reach this path.
                let (data, mem_mask) = if a & 1 == 0 {
                    (u16::from(value) << 8, 0xFF00)
                } else {
                    (u16::from(value), 0x00FF)
                };
                if self.prot_write16(a & !1, data, mem_mask) {
                    return;
                }
                // Standard cart bankswitch register. MAME maps the banksel
                // write handler ONLY at $2FFFF0-$2FFFFF (`set_slot_idx`:
                // `install_write_handler(0x2ffff0, 0x2fffff, write_banksel)`);
                // the rest of the window is read-only ROM. Restricting the
                // decode is important for protected carts, whose protection
                // RAM/registers also live inside this window ($2FE000+ for
                // PVC, $2FFFF0+ for SMA) and must NOT double as banksel.
                if a >= 0x2FFFF0 {
                    // MAME `write_banksel`:
                    //   bank = data & 0x07;
                    //   if ((bank + 1) * 0x100000 >= len) bank = 0;
                    //   bank_base = (bank + 1) * 0x100000;
                    let mut bank = (value as usize) & 7;
                    if (bank + 1) * 0x100000 >= self.p_rom.len() {
                        bank = 0;
                    }
                    let bank_off = (bank + 1) * 0x100000;
                    if bank_off != self.p_rom_bank_offset {
                        self.p_rom_bank_offset = bank_off;
                        log::trace!(
                            "P-ROM bank: req={} -> ROM offset ${:08X}",
                            value, bank_off
                        );
                    }
                } else {
                    log::trace!("write to P-ROM bank window ${a:06X} = ${value:02X} (ignored)");
                }
            }
            0x300000..=0x3FFFFF => self.io_write8(a, value),
            0x400000..=0x7FFFFF => {
                let bank = if (self.systemlatch & 0x80) != 0 { 0x2000 } else { 0 };
                let off = bank | ((a as usize) & 0x1FFF);
                self.palette_ram[off] = value;
            }
            0xC00000..=0xCFFFFF => {
                log::trace!("write to SYSROM ${a:06X} ignored");
            }
            0xD00000..=0xDFFFFF => {
                // Backup RAM write requires `save_ram_unlock` (systemlatch
                // bit 6) to be set; otherwise writes are silently dropped.
                // The BIOS unlocks the latch (`$3A001D`) right before any
                // backup-RAM access and re-locks it (`$3A000D`) afterwards.
                if (self.systemlatch & 0x40) != 0 {
                    self.backup_ram[(a as usize) & 0xFFFF] = value;
                } else {
                    log::trace!(
                        "backup RAM write blocked: ${a:06X} = ${value:02X} (locked)"
                    );
                }
            }
            _ => log::trace!("bus write8 to unmapped ${a:06X} = ${value:02X}"),
        }
    }

    fn io_read8(&mut self, a: u32) -> u8 {
        // Identify the region. The Neo Geo I/O area $300000-$3FFFFF is
        // decoded by several chips simultaneously and the *function* used
        // depends on the top byte of the address.
        let high = a & 0xFF_FF00;
        match high {
            // REG_P1CNT and REG_DIPSW share the page based on A0.
            0x300000 => {
                if a & 1 == 0 {
                    self.p1_input
                } else {
                    self.dipsw
                }
            }
            // $320000-1 = AUDIO_COIN port (MAME `portr("AUDIO_COIN")`).
            // High byte ($320000): get_audio_result (Z80 -> 68K latch).
            // Low byte  ($320001):
            //   bits 0..1 = coin1/coin2 (active low)
            //   bit  2    = service1
            //   bits 3..4 = unused / extra coins on MVS
            //   bit  5    = sense 4-slot
            //   bit  6    = uPD4990A `tp` (1 Hz test pulse)
            //   bit  7    = uPD4990A `data_out`
            0x320000 => {
                if a & 1 == 0 {
                    // High byte = Z80 sound reply.
                    self.sound_reply
                } else {
                    // $320001 = REG_STATUS_A — switch inputs are active LOW.
                    //   bit 0 : Coin-in 1
                    //   bit 1 : Coin-in 2
                    //   bit 2 : Service button
                    //   bit 3 : Coin-in 3
                    //   bit 4 : Coin-in 4
                    //   bit 5 : 0=4-slot, 1=6-slot (we model 4-slot → 0)
                    //   bit 6 : RTC tp (uPD4990A 1 Hz pulse)
                    //   bit 7 : RTC data_out
                    // Source: NeoGeo Dev Wiki, Memory_mapped_registers#REG_STATUS_A.
                    let mut v: u8 = 0xFF;
                    // Apply coin / service bits (active-low). The caller
                    // pulls a bit to 0 to simulate a press.
                    v &= self.coin_inputs | 0xE0;
                    // bit 5: 4-slot MVS → 0. Clear it.
                    v &= !0x20;
                    // RTC bits — composed from uPD4990A.
                    v &= !0xC0;
                    if self.upd4990a.tp { v |= 0x40; }
                    if self.upd4990a.data_out() { v |= 0x80; }
                    if self.coin_inputs != 0xFF {
                        log::trace!("REG_STATUS_A read: coin_inputs=${:02X} -> ${:02X}", self.coin_inputs, v);
                    }
                    v
                }
            }
            0x340000 => {
                if a & 1 == 0 {
                    self.p2_input
                } else {
                    0xFF
                }
            }
            0x380000 => {
                // REG_STATUS_B (NeoGeo wiki — single 8-bit register at
                // $380000, MAME exposes it on the high byte of a 16-bit
                // PORT_START("SYSTEM") with bits 0..7 = unused (=$FF)).
                //
                //   bit 7    : hardware type — 0=AES, 1=MVS
                //   bit 6    : memory card write-protected
                //   bit 5..4 : memory card status (00 = inserted)
                //   bit 3    : P2 SELECT  (active LOW)
                //   bit 2    : P2 START   (active LOW)
                //   bit 1    : P1 SELECT  (active LOW)
                //   bit 0    : P1 START   (active LOW)
                //
                // A0=0 (the $380000 byte address) is REG_STATUS_B itself.
                // A0=1 ($380001) is the "low byte" of the MAME-mapped
                // 16-bit port and is always $FF (unused, active-low).
                if a & 1 == 0 {
                    // Compose the STATUS_B byte from inputs + hardware bits.
                    let mut v: u8 = 0xFF;
                    // Apply start/select bits 0..3 from `start_select`
                    // (active LOW; default 0xFF means "nothing pressed").
                    v &= self.start_select | 0xF0;
                    // Memory card: bits 5..4 = 00 ("inserted OK"). Real
                    // hardware leaves them low when a card is present and
                    // high when absent; we model "always inserted" so the
                    // BIOS doesn't lock waiting for one.
                    v &= !0x30;
                    // Hardware type: bit 7 high = MVS, low = AES.
                    v = (v & 0x7F) | (self.systype & 0x80);
                    if self.start_select != 0xFF {
                        log::trace!("REG_STATUS_B read: start_select=${:02X} -> ${:02X}", self.start_select, v);
                    }
                    v
                } else {
                    // MAME's "SYSTEM" port low byte = unused / active-low.
                    0xFF
                }
            }
            0x3C0000..=0x3C00FF => self.lspc.read_register_byte(a as u16),
            _ => 0xFF,
        }
    }

    fn io_write8(&mut self, a: u32, value: u8) {
        let high = a & 0xFF_FF00;
        match high {
            // $300000: REG_DIPSW (read), watchdog-reset (write to $300001).
            // MAME `map(0x300001).w("watchdog", reset_w)`.
            0x300000 => {
                if a & 1 != 0 {
                    self.kick_watchdog();
                    log::trace!("WATCHDOG kicked <- ${value:02X}");
                }
            }
            0x320000 => {
                if a & 1 == 0 {
                    // REG_SOUND: 68000 → Z80 command latch.
                    // Raises NMI on the Z80 (subject to NMI enable).
                    // MAME `neogeo.cpp:1005`: `m_soundlatch->write(data)`
                    // → `data_pending_callback` → audionmi in_w<0>(1).
                    self.sound_latch = value;
                    self.sound_latch_pending = true;
                    log::trace!("REG_SOUND <- ${value:02X}");
                }
            }
            0x380000 => {
                // The whole $380000..$3800FF page is decoded per MAME's
                // `io_control_w(offset, data).umask16(0x00ff)` so writes only
                // hit on odd byte addresses (LSB). MAME passes `offset` as
                // the **word index** ((addr - $380000) >> 1) to the handler,
                // and the dispatch is `offset & 0x78`:
                //   0x00, 0x08  -> controller select / card bank
                //   0x28        -> uPD4990A (data_in, clk, stb)
                //   0x30, 0x70  -> coin lockout / counter
                if a & 1 == 1 {
                    let word_offset = ((a as u8) >> 1) & 0x7F;
                    match word_offset & 0x78 {
                        0x28 => {
                            self.upd4990a.write(value);
                        }
                        _ => log::trace!(
                            "$380000 page write ${a:06X} = ${value:02X} (offs=${word_offset:02X})"
                        ),
                    }
                }
            }
            0x3A0000 => {
                // NEO-D0 system-control register block, implemented via a
                // 74HC259 addressable latch. Per MAME (`neogeo.cpp`):
                //   map(0x3a0000, 0x3a001f).mirror(0x01ffe0)
                //     .w(m_systemlatch, FUNC(hc259_device::write_a3))
                //     .umask16(0x00ff);
                // The HC259 decodes A1..A4 as { bit_index[2:0], value }:
                //   bit_index = (offset & 7)
                //   value     = (offset & 8) >> 3
                // where `offset = (addr - 0x3a0000) >> 1` (umask16 collapses
                // the LSB). Then the corresponding output Q[bit_index] is
                // latched to `value`.
                let offset = (a >> 1) & 0xF;
                let bit_idx = (offset & 7) as u8;
                let bit_val = ((offset >> 3) & 1) as u8;
                let mask = 1u8 << bit_idx;
                if bit_val != 0 {
                    self.systemlatch |= mask;
                } else {
                    self.systemlatch &= !mask;
                }
                match bit_idx {
                    0 => log::debug!("systemlatch: screen_shadow = {}", bit_val),
                    1 => {
                        // bit 1 = use_cart_vectors. 0 = BIOS at $0, 1 = cart.
                        let new_bios_at_zero = bit_val == 0;
                        if new_bios_at_zero != self.bios_at_zero {
                            log::debug!(
                                "systemlatch: use_cart_vectors = {} (bios_at_zero={})",
                                bit_val, new_bios_at_zero
                            );
                            self.bios_at_zero = new_bios_at_zero;
                        }
                    }
                    2 => log::trace!("systemlatch: memcard_lock1 = {}", bit_val),
                    3 => log::trace!("systemlatch: memcard_unlock2 = {}", bit_val),
                    4 => log::trace!("systemlatch: memcard_regsel = {}", bit_val),
                    5 => log::debug!(
                        "systemlatch: use_cart_audio/fix_source = {} (0=sfix,1=cart)",
                        bit_val
                    ),
                    6 => log::debug!("systemlatch: save_ram_unlock = {}", bit_val),
                    7 => log::debug!("systemlatch: palette_bank = {}", bit_val),
                    _ => {}
                }
            }
            0x3C0000..=0x3C00FF => self.lspc.write_register_byte(a as u16, value),
            _ => {}
        }
    }
}

impl Default for NeoGeoBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for NeoGeoBus {
    fn read8(&mut self, addr: u32) -> u8 {
        self.read_phys8(addr)
    }
    fn write8(&mut self, addr: u32, value: u8) {
        self.write_phys8(addr, value);
    }

    // Specialize 16/32 for LSPC ports — they're declared as 16-bit registers.
    fn read16(&mut self, addr: u32) -> u16 {
        let a = addr & 0x00FF_FFFF;
        if (0x3C0000..=0x3C00FE).contains(&a) {
            self.lspc.read_register_word(a as u16)
        } else if (0x200000..=0x2FFFFF).contains(&a) {
            // Protection devices are 16-bit and often *stateful* per
            // access (mslugx bitstream counter, SMA LFSR). A word read
            // must hit the device exactly ONCE — splitting it into two
            // byte reads would advance the state twice and hand the
            // game a mangled bit stream (mslugx then boots into the
            // "WARNING: THIS ROM CARTRIDGE..." screen forever).
            if let Some(w) = self.prot_read16(a & !1) {
                w
            } else {
                let hi = u16::from(self.read_phys8(addr));
                let lo = u16::from(self.read_phys8(addr.wrapping_add(1)));
                (hi << 8) | lo
            }
        } else {
            let hi = u16::from(self.read_phys8(addr));
            let lo = u16::from(self.read_phys8(addr.wrapping_add(1)));
            (hi << 8) | lo
        }
    }

    fn write16(&mut self, addr: u32, value: u16) {
        let a = addr & 0x00FF_FFFF;
        if (0x3C0000..=0x3C00FE).contains(&a) {
            self.lspc.write_register_word(a as u16, value);
        } else if (0x200000..=0x2FFFFF).contains(&a) {
            // Protection devices are 16-bit and stateful: a word write
            // must hit the device exactly ONCE (PVC stamps its bank
            // marker on every access at >= $FF8 — splitting into two
            // byte writes would double-trigger it).
            if !self.prot_write16(a & !1, value, 0xFFFF) {
                self.write_phys8(addr, (value >> 8) as u8);
                self.write_phys8(addr.wrapping_add(1), value as u8);
            }
        } else {
            self.write_phys8(addr, (value >> 8) as u8);
            self.write_phys8(addr.wrapping_add(1), value as u8);
        }
    }
}

// ============================================================================
// Savestates
// ============================================================================
//
// `system_rom` y `p_rom` son ROM (se reponen desde el set cargado); todo lo
// demás es estado mutable y viaja en el savestate.

crate::state::state_fields!(NeoGeoBus {
    work_ram, palette_ram, backup_ram, memcard_ram, memcard_present,
    lspc, upd4990a, p_rom_bank_offset, bios_at_zero, systemlatch, dipsw,
    systype, sound_latch, sound_latch_pending, sound_reply, p1_input,
    p2_input, start_select, coin_inputs, watchdog_cycles, watchdog_expired,
    prot,
});

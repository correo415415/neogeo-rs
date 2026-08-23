//! Cartridge protection devices.
//!
//! Rust ports of MAME `src/devices/bus/neogeo/prot_*.cpp` and
//! `src/devices/machine/alpha_8921.cpp` (BSD-3-Clause, S. Smith /
//! David Haywood / Fabio Priuli / Razoola / Mr.K / iq_132).
//!
//! Covered by this module (PR3 scope):
//!   * **Fatal Fury 2 / Super Sidekicks** — ALPHA-8921 (PRO-CT0) serial
//!     shift-register chip mapped over the whole $200000-$2FFFFF window.
//!   * **KOF98** — early P-ROM scramble + boot-time ROM overlay at $100.
//!   * **Metal Slug X** — ALTERA command/counter bitstream reader at
//!     $2FFFE0-$2FFFEF.
//!   * **SMA** (kof99 / garou / garouh / mslug3 / mslug3a / kof2000) —
//!     encrypted P-ROM (data+address line swaps), scrambled bankswitch
//!     register, RNG readback and the $2FE446 `0x9a37` handshake.
//!
//! The 68000-visible wiring (which bus addresses hit which handler) mirrors
//! MAME `neogeo_base_state::set_slot_idx`:
//!
//! | game      | reads                                  | writes                     |
//! |-----------|----------------------------------------|----------------------------|
//! | fatfury2  | $200000-$2FFFFF → protection_r         | $200000-$2FFFFF → protection_w |
//! | kof98     | $000100-$000103 → protection_r         | $20AAAA-$20AAAB → protection_w |
//! | mslugx    | $2FFFE0-$2FFFEF → protection_r         | $2FFFE0-$2FFFEF → protection_w |
//! | kof99     | $2FE446 → 9a37, $2FFFF8/$2FFFFA → rng  | $2FFFF0 → scrambled banksel |
//! | garou(h)  | $2FE446 → 9a37, $2FFFCC/$2FFFF0 → rng  | $2FFFC0 → scrambled banksel |
//! | mslug3(a) | $2FE446 → 9a37                         | $2FFFE4 → scrambled banksel |
//! | kof2000   | $2FE446 → 9a37, $2FFFD8/$2FFFDA → rng  | $2FFFEC → scrambled banksel |

/// MAME `BIT(x, n, w)`: extract `w` bits starting at bit `n`.
#[inline]
fn bits(x: u32, n: u32, w: u32) -> u32 {
    (x >> n) & ((1 << w) - 1)
}

/// MAME `bitswap<N>(val, bN-1, .., b0)`: first listed source bit becomes the
/// result MSB. Generic over the number of bits via the slice length.
#[inline]
fn bitswap(val: u32, srcs: &[u32]) -> u32 {
    let n = srcs.len();
    let mut r = 0u32;
    for (i, &b) in srcs.iter().enumerate() {
        r |= ((val >> b) & 1) << (n - 1 - i);
    }
    r
}

#[inline]
fn bitswap16(v: u16, srcs: &[u32; 16]) -> u16 {
    bitswap(v as u32, srcs) as u16
}

/// Read a 16-bit big-endian word from ROM storage (our P-ROM keeps 68000
/// byte order after `load_p_rom`'s un-byteswap).
#[inline]
fn rd16(rom: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([rom[off], rom[off + 1]])
}

#[inline]
fn wr16(rom: &mut [u8], off: usize, v: u16) {
    rom[off..off + 2].copy_from_slice(&v.to_be_bytes());
}

// ============================================================================
// ALPHA-8921 (PRO-CT0 / SNK-9201) — MAME src/devices/machine/alpha_8921.cpp
// ============================================================================

/// Serial shift-register / sprite serializer chip. Fatal Fury 2 (and Super
/// Sidekicks) use it as a protection handshake: the game loads a 32-bit
/// value, clocks it, and reads back nibble permutations.
#[derive(Debug, Default, Clone)]
pub struct Alpha8921 {
    pub clk: bool,
    pub load: bool,
    pub even: bool,
    pub h: bool,
    pub c: u32,
    pub sr: u32,
    gad: u8,
    gbd: u8,
}

impl Alpha8921 {
    pub fn new() -> Self {
        Self::default()
    }

    /// CLK pin. On the falling edge: LOAD latches C into SR, otherwise SR
    /// shifts one step in 6-bit groups, direction chosen by H.
    pub fn clk_w(&mut self, state: bool) {
        if self.clk != state {
            self.clk = state;
            if !self.clk {
                // falling edge
                if self.load {
                    self.sr = self.c;
                } else if self.h {
                    self.sr = (bits(self.sr, 24, 6) << 26)
                        | (bits(self.sr, 16, 6) << 18)
                        | (bits(self.sr, 8, 6) << 10)
                        | (bits(self.sr, 0, 6) << 2);
                } else {
                    self.sr = (bits(self.sr, 26, 6) << 24)
                        | (bits(self.sr, 18, 6) << 16)
                        | (bits(self.sr, 10, 6) << 8)
                        | bits(self.sr, 2, 6);
                }
            }
        }
    }

    pub fn load_w(&mut self, state: bool) {
        self.load = state;
    }
    pub fn even_w(&mut self, state: bool) {
        self.even = state;
    }
    pub fn h_w(&mut self, state: bool) {
        self.h = state;
    }
    pub fn c_w(&mut self, data: u32) {
        self.c = data;
    }

    fn update_output(&mut self) {
        if self.h {
            self.gbd = bitswap(self.sr, &[30, 22, 14, 6]) as u8;
            self.gad = bitswap(self.sr, &[31, 23, 15, 7]) as u8;
        } else {
            self.gbd = bitswap(self.sr, &[25, 17, 9, 1]) as u8;
            self.gad = bitswap(self.sr, &[24, 16, 8, 0]) as u8;
        }
        if self.even {
            core::mem::swap(&mut self.gad, &mut self.gbd);
        }
    }

    pub fn gad_r(&mut self) -> u8 {
        self.update_output();
        self.gad & 0xF
    }

    pub fn gbd_r(&mut self) -> u8 {
        self.update_output();
        self.gbd & 0xF
    }
}

// ============================================================================
// Fatal Fury 2 protection — MAME prot_fatfury2.cpp
// ============================================================================

#[derive(Debug, Default, Clone)]
pub struct FatFury2Prot {
    pub alpha: Alpha8921,
}

impl FatFury2Prot {
    pub fn new() -> Self {
        Self::default()
    }

    /// `offset` = 16-bit word offset from $200000 (`(addr - 0x200000) >> 1`).
    pub fn protection_r(&mut self, offset: u32) -> u16 {
        self.alpha.even_w(bits(offset, 1, 1) != 0);
        self.alpha.h_w(bits(offset, 2, 1) != 0);
        let gad = u32::from(self.alpha.gad_r());
        let gbd = u32::from(self.alpha.gbd_r());
        // Data pins from PRO-CT0:
        //   D0..D7 = GAD2 GAD3 GAD0 GAD1 GBD2 GBD3 GBD0 GBD1
        ((bits(gbd, 0, 2) << 6)
            | (bits(gbd, 2, 2) << 4)
            | (bits(gad, 0, 2) << 2)
            | bits(gad, 2, 2)) as u16
    }

    /// `offset` = word offset from $200000, `data` = the 16-bit value.
    pub fn protection_w(&mut self, offset: u32, data: u16) {
        // /PORTOEL drives the PRO-CT0 CLK pin.
        self.alpha.clk_w(true);
        self.alpha.load_w(bits(offset, 0, 1) != 0); // A1
        self.alpha.even_w(bits(offset, 1, 1) != 0); // A2
        self.alpha.h_w(bits(offset, 2, 1) != 0); // A3
        // C16-31 = A4-A19 (word-offset bits 3..18), C0-C15 = D0-D15, both
        // through the same nibble-interleave bitswap (prot_fatfury2.cpp).
        const SWAP: [u32; 16] = [15, 11, 14, 10, 13, 9, 12, 8, 7, 3, 6, 2, 5, 1, 4, 0];
        let addr_part = bitswap16(bits(offset, 3, 16) as u16, &SWAP);
        let data_part = bitswap16(data, &SWAP);
        self.alpha
            .c_w((u32::from(addr_part) << 16) | u32::from(data_part));
        // release /PORTOEL
        self.alpha.clk_w(false);
    }
}

// ============================================================================
// KOF98 protection — MAME prot_kof98.cpp
// ============================================================================

#[derive(Debug, Default, Clone)]
pub struct Kof98Prot {
    /// 0 = pass-through, 1 = overlay $00C2/$00FD, 2 = overlay $4E45/$4F2D.
    pub prot_state: u8,
    /// The words originally at $100/$102 (captured after decrypt).
    pub default_rom: [u16; 2],
}

impl Kof98Prot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Unscramble the 242-P1 program ROM in place (region >= 0x600000,
    /// stored big-endian). The scramble only moves 2-byte pairs around, so
    /// it is byte-order agnostic as long as pair offsets stay even.
    pub fn decrypt_68k(&mut self, rom: &mut [u8]) {
        assert!(rom.len() >= 0x600000, "kof98 P region must be >= 0x600000");
        let dst: Vec<u8> = rom[..0x200000].to_vec();
        const SEC: [usize; 8] = [
            0x000000, 0x100000, 0x000004, 0x100004, 0x10000A, 0x00000A, 0x10000E, 0x00000E,
        ];
        const POS: [usize; 4] = [0x000, 0x004, 0x00A, 0x00E];

        let mut i = 0x800;
        while i < 0x100000 {
            let mut j = 0;
            while j < 0x100 {
                let mut k = 0;
                while k < 16 {
                    let s = SEC[k / 2];
                    rom[i + j + k..i + j + k + 2]
                        .copy_from_slice(&dst[i + j + s + 0x100..i + j + s + 0x102]);
                    rom[i + j + k + 0x100..i + j + k + 0x102]
                        .copy_from_slice(&dst[i + j + s..i + j + s + 2]);
                    k += 2;
                }
                if (0x080000..0x0C0000).contains(&i) {
                    for &p in &POS {
                        rom[i + j + p..i + j + p + 2]
                            .copy_from_slice(&dst[i + j + p..i + j + p + 2]);
                        rom[i + j + p + 0x100..i + j + p + 0x102]
                            .copy_from_slice(&dst[i + j + p + 0x100..i + j + p + 0x102]);
                    }
                } else if i >= 0x0C0000 {
                    for &p in &POS {
                        rom[i + j + p..i + j + p + 2]
                            .copy_from_slice(&dst[i + j + p + 0x100..i + j + p + 0x102]);
                        rom[i + j + p + 0x100..i + j + p + 0x102]
                            .copy_from_slice(&dst[i + j + p..i + j + p + 2]);
                    }
                }
                j += 0x10;
            }
            rom[i..i + 2].copy_from_slice(&dst[i..i + 2]);
            rom[i + 2..i + 4].copy_from_slice(&dst[i + 0x100000..i + 0x100002]);
            rom[i + 0x100..i + 0x102].copy_from_slice(&dst[i + 0x100..i + 0x102]);
            rom[i + 0x102..i + 0x104].copy_from_slice(&dst[i + 0x100100..i + 0x100102]);
            i += 0x200;
        }
        // MAME: memmove(&src[0x100000], &src[0x200000], 0x400000)
        rom.copy_within(0x200000..0x600000, 0x100000);

        self.default_rom[0] = rd16(rom, 0x100);
        self.default_rom[1] = rd16(rom, 0x102);
    }

    /// ROM overlay read at $100-$103 (`offset` 0 → $100, 1 → $102).
    pub fn protection_r(&self, offset: u32) -> u16 {
        match (self.prot_state, offset) {
            (1, 0) => 0x00C2,
            (1, _) => 0x00FD,
            (2, 0) => 0x4E45,
            (2, _) => 0x4F2D,
            (_, 0) => self.default_rom[0],
            (_, _) => self.default_rom[1],
        }
    }

    /// Write to $20AAAA (values worked out on real hardware by Razoola).
    pub fn protection_w(&mut self, data: u16) {
        match data {
            0x0090 => self.prot_state = 1,
            0x00F0 => self.prot_state = 2,
            _ => log::trace!("kof98: unknown protection write {data:04X}"),
        }
    }
}

// ============================================================================
// Metal Slug X protection — MAME prot_mslugx.cpp
// ============================================================================

#[derive(Debug, Default, Clone)]
pub struct MslugXProt {
    pub command: u16,
    pub counter: i32,
}

impl MslugXProt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write to $2FFFE0-$2FFFEF; `offset` = word offset from $2FFFE0.
    pub fn protection_w(&mut self, offset: u32, data: u16) {
        match offset {
            0x0 => self.command = 0,           // start new read
            0x1 | 0x2 => self.command |= data, // command bits, pulsed
            0x3 => {}                          // finished
            0x5 => {
                // init
                self.counter = 0;
                self.command = 0;
            }
            _ => log::trace!("mslugx: unknown protection write off={offset} data={data:04X}"),
        }
    }

    /// Protection read. Needs two bus values supplied by the caller because
    /// the device reads back through the 68000 address space:
    ///   * `read_byte(addr)`: P-ROM bitstream bytes around $DEDD2
    ///   * `select_word`: the word at work-RAM $10F00A (command $0FFF path)
    ///
    /// Returns a single bit like MAME's `protection_r`.
    pub fn protection_r(
        &mut self,
        mut read_byte: impl FnMut(u32) -> u8,
        select_word: u16,
    ) -> u16 {
        match self.command {
            0x0001 => {
                let c = self.counter as u32;
                let res =
                    (read_byte(0xDEDD2 + ((c >> 3) & 0xFFF)) >> (!c & 0x07)) & 1;
                self.counter += 1;
                res as u16
            }
            0x0FFF => {
                let select = i32::from(select_word) - 1;
                let s = select as u32;
                ((read_byte(0xDEDD2 + ((s >> 3) & 0x0FFF)) >> (!s & 0x07)) & 1) as u16
            }
            _ => {
                log::trace!("mslugx: unknown protection read, cmd={:04X}", self.command);
                0
            }
        }
    }
}

// ============================================================================
// SMA protection — MAME prot_sma.cpp
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmaGame {
    Kof99,
    Garou,
    GarouH,
    Mslug3,
    Mslug3a,
    Kof2000,
}

#[derive(Debug, Clone)]
pub struct SmaProt {
    pub game: SmaGame,
    pub rng: u16,
}

impl SmaProt {
    pub fn new(game: SmaGame) -> Self {
        Self { game, rng: 0x2345 }
    }

    pub fn reset(&mut self) {
        self.rng = 0x2345;
    }

    /// $2FE446 handshake.
    pub fn prot_9a37_r(&self) -> u16 {
        0x9A37
    }

    /// SMA pseudo-random generator (verified for KOF99 by Razoola; MAME
    /// uses the same taps for every SMA game).
    pub fn random_r(&mut self) -> u16 {
        let old = self.rng;
        let newbit = ((self.rng >> 2)
            ^ (self.rng >> 3)
            ^ (self.rng >> 5)
            ^ (self.rng >> 6)
            ^ (self.rng >> 7)
            ^ (self.rng >> 11)
            ^ (self.rng >> 12)
            ^ (self.rng >> 15))
            & 1;
        self.rng = (self.rng << 1) | newbit;
        old
    }

    /// Scrambled bankswitch: raw register value → absolute byte offset into
    /// the decrypted P region (already includes the +$100000 banked base,
    /// so it drops straight into `NeoGeoBus::p_rom_bank_offset`).
    pub fn bank_base(&self, sel: u16) -> usize {
        match self.game {
            SmaGame::Kof99 => {
                const OFF: [usize; 33] = [
                    0x000000, 0x100000, 0x200000, 0x300000, 0x3CC000, 0x4CC000, 0x3F2000,
                    0x4F2000, 0x407800, 0x507800, 0x40D000, 0x50D000, 0x417800, 0x517800,
                    0x420800, 0x520800, 0x424800, 0x524800, 0x429000, 0x529000, 0x42E800,
                    0x52E800, 0x431800, 0x531800, 0x54D000, 0x551000, 0x567000, 0x592800,
                    0x588800, 0x581800, 0x599800, 0x594800, 0x598000,
                ];
                let data = bitswap(sel as u32, &[5, 12, 10, 8, 6, 14]) as usize;
                0x100000 + OFF.get(data).copied().unwrap_or(0)
            }
            SmaGame::Garou => {
                const OFF: [usize; 55] = [
                    0x000000, 0x100000, 0x200000, 0x300000, 0x280000, 0x380000, 0x2D0000,
                    0x3D0000, 0x2F0000, 0x3F0000, 0x400000, 0x500000, 0x420000, 0x520000,
                    0x440000, 0x540000, 0x498000, 0x598000, 0x4A0000, 0x5A0000, 0x4A8000,
                    0x5A8000, 0x4B0000, 0x5B0000, 0x4B8000, 0x5B8000, 0x4C0000, 0x5C0000,
                    0x4C8000, 0x5C8000, 0x4D0000, 0x5D0000, 0x458000, 0x558000, 0x460000,
                    0x560000, 0x468000, 0x568000, 0x470000, 0x570000, 0x478000, 0x578000,
                    0x480000, 0x580000, 0x488000, 0x588000, 0x490000, 0x590000, 0x5D0000,
                    0x5D8000, 0x5E0000, 0x5E8000, 0x5F0000, 0x5F8000, 0x600000,
                ];
                let data = bitswap(sel as u32, &[12, 14, 6, 7, 9, 5]) as usize;
                0x100000 + OFF.get(data).copied().unwrap_or(0)
            }
            SmaGame::GarouH => {
                const OFF: [usize; 64] = [
                    0x000000, 0x100000, 0x200000, 0x300000, 0x280000, 0x380000, 0x2D0000,
                    0x3D0000, 0x2C8000, 0x3C8000, 0x400000, 0x500000, 0x420000, 0x520000,
                    0x440000, 0x540000, 0x598000, 0x698000, 0x5A0000, 0x6A0000, 0x5A8000,
                    0x6A8000, 0x5B0000, 0x6B0000, 0x5B8000, 0x6B8000, 0x5C0000, 0x6C0000,
                    0x5C8000, 0x6C8000, 0x5D0000, 0x6D0000, 0x458000, 0x558000, 0x460000,
                    0x560000, 0x468000, 0x568000, 0x470000, 0x570000, 0x478000, 0x578000,
                    0x480000, 0x580000, 0x488000, 0x588000, 0x490000, 0x590000, 0x5D8000,
                    0x6D8000, 0x5E0000, 0x6E0000, 0x5E8000, 0x6E8000, 0x6E8000, 0x000000,
                    0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000,
                    0x000000,
                ];
                let data = bitswap(sel as u32, &[13, 11, 2, 14, 8, 4]) as usize;
                0x100000 + OFF[data & 63]
            }
            SmaGame::Mslug3 => {
                const OFF: [usize; 49] = [
                    0x000000, 0x020000, 0x040000, 0x060000, 0x070000, 0x090000, 0x0B0000,
                    0x0D0000, 0x0E0000, 0x0F0000, 0x120000, 0x130000, 0x140000, 0x150000,
                    0x180000, 0x190000, 0x1A0000, 0x1B0000, 0x1E0000, 0x1F0000, 0x200000,
                    0x210000, 0x240000, 0x250000, 0x260000, 0x270000, 0x2A0000, 0x2B0000,
                    0x2C0000, 0x2D0000, 0x300000, 0x310000, 0x320000, 0x330000, 0x360000,
                    0x370000, 0x380000, 0x390000, 0x3C0000, 0x3D0000, 0x400000, 0x410000,
                    0x440000, 0x450000, 0x460000, 0x470000, 0x4A0000, 0x4B0000, 0x4C0000,
                ];
                let data = bitswap(sel as u32, &[9, 3, 6, 15, 12, 14]) as usize;
                0x100000 + OFF.get(data).copied().unwrap_or(0)
            }
            SmaGame::Mslug3a => {
                const OFF: [usize; 48] = [
                    0x000000, 0x030000, 0x040000, 0x070000, 0x080000, 0x0A0000, 0x0C0000,
                    0x0E0000, 0x0F0000, 0x100000, 0x130000, 0x140000, 0x150000, 0x160000,
                    0x190000, 0x1A0000, 0x1B0000, 0x1C0000, 0x1F0000, 0x200000, 0x210000,
                    0x220000, 0x250000, 0x260000, 0x270000, 0x280000, 0x2B0000, 0x2C0000,
                    0x2D0000, 0x2E0000, 0x310000, 0x320000, 0x330000, 0x340000, 0x370000,
                    0x380000, 0x390000, 0x3A0000, 0x3D0000, 0x3E0000, 0x400000, 0x410000,
                    0x440000, 0x450000, 0x460000, 0x470000, 0x4A0000, 0x4B0000,
                ];
                let data = bitswap(sel as u32, &[11, 12, 6, 1, 3, 15]) as usize;
                0x100000 + OFF.get(data).copied().unwrap_or(0)
            }
            SmaGame::Kof2000 => {
                const OFF: [usize; 36] = [
                    0x000000, 0x100000, 0x200000, 0x300000, 0x3F7800, 0x4F7800, 0x3FF800,
                    0x4FF800, 0x407800, 0x507800, 0x40F800, 0x50F800, 0x416800, 0x516800,
                    0x41D800, 0x51D800, 0x424000, 0x524000, 0x523800, 0x623800, 0x526000,
                    0x626000, 0x528000, 0x628000, 0x52A000, 0x62A000, 0x52B800, 0x62B800,
                    0x52D000, 0x62D000, 0x52E800, 0x62E800, 0x618000, 0x619000, 0x61A000,
                    0x61A800,
                ];
                let data = bitswap(sel as u32, &[5, 10, 3, 7, 14, 15]) as usize;
                0x100000 + OFF.get(data).copied().unwrap_or(0)
            }
        }
    }
}

/// Decrypt an SMA P region in place. `rom` must be the full $900000-byte
/// region: fixed part at $0 (produced here), SMA ROM at $C0000, banked
/// (encrypted) P data at $100000. Storage is big-endian (68000 order).
///
/// Step order matches each game's routine in MAME `prot_sma.cpp`:
///   * kof99 / kof2000:              data → banked-addr → fixed-relocate
///   * garou / garouh / mslug3(-a):  data → fixed-relocate → banked-addr
pub fn sma_decrypt(game: SmaGame, rom: &mut [u8]) {
    assert!(rom.len() >= 0x900000, "SMA P region must be 0x900000 bytes");
    match game {
        SmaGame::Kof99 | SmaGame::Kof2000 => {
            sma_step_data(game, rom);
            sma_step_banked_addr(game, rom);
            sma_step_fixed(game, rom);
        }
        SmaGame::Garou | SmaGame::GarouH | SmaGame::Mslug3 | SmaGame::Mslug3a => {
            sma_step_data(game, rom);
            sma_step_fixed(game, rom);
            sma_step_banked_addr(game, rom);
        }
    }
}

/// Step 1: swap data lines over the whole banked area ($100000..$900000).
fn sma_step_data(game: SmaGame, rom: &mut [u8]) {
    let data_swap: [u32; 16] = match game {
        SmaGame::Kof99 => [13, 7, 3, 0, 9, 4, 5, 6, 1, 12, 8, 14, 10, 11, 2, 15],
        SmaGame::Garou => [13, 12, 14, 10, 8, 2, 3, 1, 5, 9, 11, 4, 15, 0, 6, 7],
        SmaGame::GarouH => [14, 5, 1, 11, 7, 4, 10, 15, 3, 12, 8, 13, 0, 2, 9, 6],
        SmaGame::Mslug3 => [4, 11, 14, 3, 1, 13, 0, 7, 2, 8, 12, 15, 10, 9, 5, 6],
        SmaGame::Mslug3a => [2, 11, 12, 14, 9, 3, 1, 4, 13, 7, 6, 8, 10, 15, 0, 5],
        SmaGame::Kof2000 => [12, 8, 11, 3, 15, 14, 7, 0, 10, 13, 6, 5, 9, 2, 1, 4],
    };
    for i in 0..(0x800000 / 2) {
        let off = 0x100000 + i * 2;
        let w = rd16(rom, off);
        wr16(rom, off, bitswap16(w, &data_swap));
    }
}

/// Step 2: relocate + address-unscramble the fixed part ($0-$C0000).
/// Source offsets are relative to the region base like MAME's
/// `rom[i] = rom[fixed_src/2 + bitswap<19>(i, ...)]`.
fn sma_step_fixed(game: SmaGame, rom: &mut [u8]) {
    let (fixed_src, fixed_swap): (usize, &[u32; 19]) = match game {
        SmaGame::Kof99 => (
            0x700000,
            &[18, 11, 6, 14, 17, 16, 5, 8, 10, 12, 0, 4, 3, 2, 7, 9, 15, 13, 1],
        ),
        SmaGame::Garou => (
            0x710000,
            &[18, 4, 5, 16, 14, 7, 9, 6, 13, 17, 15, 3, 1, 2, 12, 11, 8, 10, 0],
        ),
        SmaGame::GarouH => (
            0x7F8000,
            &[18, 5, 16, 11, 2, 6, 7, 17, 3, 12, 8, 14, 4, 0, 9, 1, 10, 15, 13],
        ),
        SmaGame::Mslug3 => (
            0x5D0000,
            &[18, 15, 2, 1, 13, 3, 0, 9, 6, 16, 4, 11, 5, 7, 12, 17, 14, 10, 8],
        ),
        SmaGame::Mslug3a => (
            0x5D0000,
            &[18, 1, 16, 14, 7, 17, 5, 8, 4, 15, 6, 3, 2, 0, 13, 10, 12, 9, 11],
        ),
        SmaGame::Kof2000 => (
            0x73A000,
            &[18, 8, 4, 15, 13, 3, 14, 16, 2, 6, 17, 7, 12, 10, 0, 5, 11, 1, 9],
        ),
    };
    for i in 0..(0xC0000 / 2) {
        let si = fixed_src + (bitswap(i as u32, fixed_swap) as usize) * 2;
        let w = rd16(rom, si);
        wr16(rom, i * 2, w);
    }
}

/// Step 3: swap address lines inside each block of the banked part.
fn sma_step_banked_addr(game: SmaGame, rom: &mut [u8]) {
    let (blk, addr_swap, extent): (usize, &[u32], usize) = match game {
        SmaGame::Kof99 => (0x800, &[6, 2, 4, 9, 8, 3, 1, 7, 0, 5][..], 0x600000),
        SmaGame::Garou => (
            0x8000,
            &[9, 4, 8, 3, 13, 6, 2, 7, 0, 12, 1, 11, 10, 5][..],
            0x800000,
        ),
        SmaGame::GarouH => (
            0x8000,
            &[12, 8, 1, 7, 11, 3, 13, 10, 6, 9, 5, 4, 0, 2][..],
            0x800000,
        ),
        SmaGame::Mslug3 => (
            0x10000,
            &[2, 11, 0, 14, 6, 4, 13, 8, 9, 3, 10, 7, 5, 12, 1][..],
            0x800000,
        ),
        SmaGame::Mslug3a => (
            0x10000,
            &[12, 0, 11, 3, 4, 13, 6, 8, 14, 7, 5, 2, 10, 9, 1][..],
            0x800000,
        ),
        SmaGame::Kof2000 => (0x800, &[4, 1, 3, 8, 6, 2, 7, 0, 9, 5][..], 0x63A000),
    };
    let words_per_blk = blk / 2;
    let mut base = 0usize;
    while base < extent {
        let src: Vec<u8> = rom[0x100000 + base..0x100000 + base + blk].to_vec();
        for j in 0..words_per_blk {
            let sj = bitswap(j as u32, addr_swap) as usize;
            let w = rd16(&src, sj * 2);
            wr16(rom, 0x100000 + base + j * 2, w);
        }
        base += blk;
    }
}

// ============================================================================
// Top-level cartridge protection state + per-game detection
// ============================================================================

#[derive(Debug, Default, Clone)]
pub enum CartProt {
    #[default]
    None,
    FatFury2(FatFury2Prot),
    Kof98(Kof98Prot),
    MslugX(MslugXProt),
    Sma(SmaProt),
}

/// Map a MAME set name to its protection device — the software counterpart
/// of `neogeo_base_state::set_slot_idx`'s cart-type switch.
pub fn detect_protection(name: &str) -> CartProt {
    let n = name.to_ascii_lowercase();
    match n.as_str() {
        // ALPHA-8921 (PRO-CT0). MAME type NEOGEO_FATFURY2 covers both
        // Fatal Fury 2 and Super Sidekicks.
        "fatfury2" | "ssideki" => CartProt::FatFury2(FatFury2Prot::new()),
        "kof98" | "kof98a" | "kof98k" | "kof98ka" => CartProt::Kof98(Kof98Prot::new()),
        "mslugx" => CartProt::MslugX(MslugXProt::new()),
        "kof99" | "kof99h" | "kof99e" | "kof99k" | "kof99ka" => {
            CartProt::Sma(SmaProt::new(SmaGame::Kof99))
        }
        "garou" => CartProt::Sma(SmaProt::new(SmaGame::Garou)),
        "garouh" => CartProt::Sma(SmaProt::new(SmaGame::GarouH)),
        "mslug3" => CartProt::Sma(SmaProt::new(SmaGame::Mslug3)),
        "mslug3a" => CartProt::Sma(SmaProt::new(SmaGame::Mslug3a)),
        "kof2000" => CartProt::Sma(SmaProt::new(SmaGame::Kof2000)),
        _ => CartProt::None,
    }
}

#[cfg(test)]
mod prot_tests {
    use super::*;

    #[test]
    fn bitswap_matches_mame_msb_first_convention() {
        // bitswap<4>(x, 3,2,1,0) is the identity.
        assert_eq!(bitswap(0b1010, &[3, 2, 1, 0]), 0b1010);
        // Reversing the bit list reverses the bits.
        assert_eq!(bitswap(0b1000, &[0, 1, 2, 3]), 0b0001);
    }

    #[test]
    fn alpha8921_load_and_shift() {
        let mut a = Alpha8921::new();
        a.load_w(true);
        a.c_w(0xDEAD_BEEF);
        a.clk_w(true);
        a.clk_w(false); // falling edge latches C
        assert_eq!(a.sr, 0xDEAD_BEEF);

        // LOAD low + H high: 6-bit groups shift up by 2 within 8-bit lanes.
        a.load_w(false);
        a.h_w(true);
        let before = a.sr;
        a.clk_w(true);
        a.clk_w(false);
        let expect = (bits(before, 24, 6) << 26)
            | (bits(before, 16, 6) << 18)
            | (bits(before, 8, 6) << 10)
            | (bits(before, 0, 6) << 2);
        assert_eq!(a.sr, expect);
    }

    #[test]
    fn fatfury2_write_then_read_roundtrip_is_deterministic() {
        let mut p1 = FatFury2Prot::new();
        let mut p2 = FatFury2Prot::new();
        for (off, data) in [(1u32, 0xFFFFu16), (0, 0x1234), (5, 0xA5A5)] {
            p1.protection_w(off, data);
            p2.protection_w(off, data);
        }
        for off in [0u32, 2, 4, 6] {
            assert_eq!(p1.protection_r(off), p2.protection_r(off));
        }
        // Loading all-ones data must produce set taps in the low half.
        let mut p = FatFury2Prot::new();
        p.protection_w(0x1, 0xFFFF); // offset bit0 = LOAD, all data bits on
        let r = p.protection_r(0x0);
        assert_eq!(r & 0xFF, 0xFF, "all-ones load must read back set nibbles");
    }

    #[test]
    fn kof98_state_machine_values_from_razoola() {
        let mut p = Kof98Prot::new();
        p.default_rom = [0x1234, 0x5678];
        assert_eq!(p.protection_r(0), 0x1234);
        assert_eq!(p.protection_r(1), 0x5678);
        p.protection_w(0x0090);
        assert_eq!(p.protection_r(0), 0x00C2);
        assert_eq!(p.protection_r(1), 0x00FD);
        p.protection_w(0x00F0);
        assert_eq!(p.protection_r(0), 0x4E45);
        assert_eq!(p.protection_r(1), 0x4F2D);
    }

    #[test]
    fn mslugx_bitstream_reader_walks_bits_msb_first() {
        let mut p = MslugXProt::new();
        // Program command 0x0001 through the write interface.
        p.protection_w(0x0, 0);
        p.protection_w(0x1, 0x0001);
        // Fake ROM: byte at $DEDD2 = 0b1010_0001.
        let rb = |addr: u32| -> u8 {
            if addr == 0xDEDD2 { 0xA1 } else { 0 }
        };
        // counter=0 → bit (~0 & 7) = 7 (MSB) of $DEDD2 → 1
        assert_eq!(p.protection_r(rb, 0), 1);
        // counter=1 → bit 6 → 0
        assert_eq!(p.protection_r(rb, 0), 0);
        // counter=2 → bit 5 → 1
        assert_eq!(p.protection_r(rb, 0), 1);
        assert_eq!(p.counter, 3);
    }

    #[test]
    fn sma_rng_first_read_returns_seed_and_advances() {
        let mut s = SmaProt::new(SmaGame::Kof99);
        assert_eq!(s.random_r(), 0x2345);
        let second = s.random_r();
        // Recompute the expected LFSR step manually.
        let seed: u16 = 0x2345;
        let nb = ((seed >> 2) ^ (seed >> 3) ^ (seed >> 5) ^ (seed >> 6)
            ^ (seed >> 7) ^ (seed >> 11) ^ (seed >> 12) ^ (seed >> 15))
            & 1;
        assert_eq!(second, (seed << 1) | nb);
        s.reset();
        assert_eq!(s.random_r(), 0x2345);
    }

    #[test]
    fn sma_bank_base_sel_zero_maps_to_bank0() {
        for game in [
            SmaGame::Kof99,
            SmaGame::Garou,
            SmaGame::GarouH,
            SmaGame::Mslug3,
            SmaGame::Mslug3a,
            SmaGame::Kof2000,
        ] {
            let s = SmaProt::new(game);
            assert_eq!(s.bank_base(0), 0x100000, "{game:?} sel=0 must map bank 0");
        }
    }

    #[test]
    fn sma_kof99_bank_unscramble_known_pattern() {
        let s = SmaProt::new(SmaGame::Kof99);
        // bitswap<6>(sel, 5,12,10,8,6,14): sel bit 14 → data bit 0 →
        // bankoffset[1] = 0x100000 → base 0x200000.
        assert_eq!(s.bank_base(1 << 14), 0x100000 + 0x100000);
        // sel bit 5 is the MSB → data = 32 → bankoffset[32] = 0x598000.
        assert_eq!(s.bank_base(1 << 5), 0x100000 + 0x598000);
    }

    #[test]
    fn detection_table_maps_known_sets() {
        assert!(matches!(detect_protection("fatfury2"), CartProt::FatFury2(_)));
        assert!(matches!(detect_protection("ssideki"), CartProt::FatFury2(_)));
        assert!(matches!(detect_protection("kof98"), CartProt::Kof98(_)));
        assert!(matches!(detect_protection("KOF98"), CartProt::Kof98(_)));
        assert!(matches!(detect_protection("mslugx"), CartProt::MslugX(_)));
        assert!(matches!(
            detect_protection("garou"),
            CartProt::Sma(SmaProt { game: SmaGame::Garou, .. })
        ));
        assert!(matches!(
            detect_protection("kof2000"),
            CartProt::Sma(SmaProt { game: SmaGame::Kof2000, .. })
        ));
        assert!(matches!(detect_protection("mslug"), CartProt::None));
        assert!(matches!(detect_protection("kof97"), CartProt::None));
    }

    #[test]
    fn kof98_decrypt_smoke_and_default_rom_capture() {
        // Synthetic 0x600000 region with a recognizable byte pattern.
        let mut rom = vec![0u8; 0x600000];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut p = Kof98Prot::new();
        p.decrypt_68k(&mut rom);
        assert_eq!(p.default_rom[0], rd16(&rom, 0x100));
        assert_eq!(p.default_rom[1], rd16(&rom, 0x102));
    }

    #[test]
    fn sma_decrypt_smoke_all_games_and_data_swap_is_permutation() {
        for game in [
            SmaGame::Kof99,
            SmaGame::Garou,
            SmaGame::GarouH,
            SmaGame::Mslug3,
            SmaGame::Mslug3a,
            SmaGame::Kof2000,
        ] {
            let mut rom = vec![0u8; 0x900000];
            for (i, b) in rom.iter_mut().enumerate() {
                *b = (i % 253) as u8;
            }
            sma_decrypt(game, &mut rom);
            // The fixed part must have been populated with banked data.
            assert!(
                rom[..0xC0000].iter().any(|&b| b != 0),
                "{game:?}: fixed part came out empty"
            );
        }
        // The 16 data-swap lists must each be a permutation of 0..16 —
        // guards against typos in the tables.
        for swap in [
            [13u32, 7, 3, 0, 9, 4, 5, 6, 1, 12, 8, 14, 10, 11, 2, 15],
            [13, 12, 14, 10, 8, 2, 3, 1, 5, 9, 11, 4, 15, 0, 6, 7],
            [14, 5, 1, 11, 7, 4, 10, 15, 3, 12, 8, 13, 0, 2, 9, 6],
            [4, 11, 14, 3, 1, 13, 0, 7, 2, 8, 12, 15, 10, 9, 5, 6],
            [2, 11, 12, 14, 9, 3, 1, 4, 13, 7, 6, 8, 10, 15, 0, 5],
            [12, 8, 11, 3, 15, 14, 7, 0, 10, 13, 6, 5, 9, 2, 1, 4],
        ] {
            let mut seen = [false; 16];
            for &b in &swap {
                assert!(!seen[b as usize], "duplicate bit {b} in data swap");
                seen[b as usize] = true;
            }
        }
    }
}

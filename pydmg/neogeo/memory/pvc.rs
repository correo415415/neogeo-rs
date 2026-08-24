//! PVC protection (mslug5, svcchaos, kof2003) — port of MAME
//! `src/devices/bus/neogeo/prot_pvc.cpp` (BSD-3-Clause,
//! S. Smith / David Haywood / Fabio Priuli).
//!
//! Two halves:
//! 1. A 0x1000-word cart RAM mapped at $2FE000-$2FFFFF with magic registers:
//!    - write to $FF0 unpacks a packed colour into $FF1/$FF2,
//!    - writes to $FF4/$FF5 pack a colour into $FF6,
//!    - a write at >= $FF8 latches a P-ROM bank switch (bank base is
//!      `((ram[0xff8]>>8)|(ram[0xff9]<<8)) + 0x100000`, with marker bytes
//!      written back so the game sees the handshake succeeded).
//! 2. One-shot P-ROM descrambles (`*_decrypt_68k`) run at load time.
//!
//! Byte-order note: MAME's decrypts index the raw little-endian region with
//! `BYTE_XOR_LE(i)` (= `i ^ 1` on LE hosts). Our `bus.p_rom` stores
//! big-endian 68000 byte order after `load_p_rom`'s swap, i.e.
//! `p_rom[j] == mame_rom[j ^ 1]`. Substituting `j = i ^ 1` everywhere makes
//! the XOR passes index-direct (`p_rom[j] ^= xor[j % 0x20]`), makes the word
//! bitswap read `p_rom[i+1] | p_rom[i+2] << 8` directly, and leaves all the
//! even-sized block moves untouched.

use super::prot::bitswap;

// ---------------------------------------------------------------------------
// Cart RAM device
// ---------------------------------------------------------------------------

/// Which PVC cartridge is inserted (selects the P-ROM descramble).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PvcGame {
    Mslug5,
    Svc,
    Kof2003,
    Kof2003h,
}

/// PVC protection chip state: 0x1000 u16 of cart RAM at $2FE000-$2FFFFF.
#[derive(Clone)]
pub struct PvcProt {
    pub game: PvcGame,
    cart_ram: [u16; 0x1000],
}

impl std::fmt::Debug for PvcProt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PvcProt").field("game", &self.game).finish()
    }
}

impl PvcProt {
    pub fn new(game: PvcGame) -> Self {
        Self {
            game,
            cart_ram: [0; 0x1000],
        }
    }

    /// 16-bit read at word `offset` (0..0x1000) inside the PVC RAM window.
    pub fn protection_r(&self, offset: usize) -> u16 {
        self.cart_ram[offset & 0xfff]
    }

    /// 16-bit write. Returns `Some(bank_base)` when the write triggers a
    /// P-ROM bankswitch (offset >= 0xff8), which the bus must apply to its
    /// banked $200000-$2FFFFF window.
    pub fn protection_w(&mut self, offset: usize, data: u16) -> Option<usize> {
        let offset = offset & 0xfff;
        self.cart_ram[offset] = data;
        if offset == 0xff0 {
            self.write_unpack_color();
        } else if (0xff4..=0xff5).contains(&offset) {
            self.write_pack_color();
        }
        if offset >= 0xff8 {
            let bankaddress =
                ((self.cart_ram[0xff8] >> 8) as u32) | ((self.cart_ram[0xff9] as u32) << 8);
            self.cart_ram[0xff8] = (self.cart_ram[0xff8] & 0xfe00) | 0x00a0;
            self.cart_ram[0xff9] &= 0x7fff;
            return Some(bankaddress as usize + 0x100000);
        }
        None
    }

    fn write_unpack_color(&mut self) {
        let pen = self.cart_ram[0xff0];
        let b = ((pen & 0x000f) << 1) | ((pen & 0x1000) >> 12);
        let g = ((pen & 0x00f0) >> 3) | ((pen & 0x2000) >> 13);
        let r = ((pen & 0x0f00) >> 7) | ((pen & 0x4000) >> 14);
        let s = (pen & 0x8000) >> 15;
        self.cart_ram[0xff1] = (g << 8) | b;
        self.cart_ram[0xff2] = (s << 8) | r;
    }

    fn write_pack_color(&mut self) {
        let gb = self.cart_ram[0xff4];
        let sr = self.cart_ram[0xff5];
        self.cart_ram[0xff6] = ((gb & 0x001e) >> 1)
            | ((gb & 0x1e00) >> 5)
            | ((sr & 0x001e) << 7)
            | ((gb & 0x0001) << 12)
            | ((gb & 0x0100) << 5)
            | ((sr & 0x0001) << 14)
            | ((sr & 0x0100) << 7);
    }
}

// ---------------------------------------------------------------------------
// P-ROM descrambles
// ---------------------------------------------------------------------------

const MSLUG5_XOR1: [u8; 0x20] = [
    0xc2, 0x4b, 0x74, 0xfd, 0x0b, 0x34, 0xeb, 0xd7, 0x10, 0x6d, 0xf9, 0xce, 0x5d, 0xd5, 0x61,
    0x29, 0xf5, 0xbe, 0x0d, 0x82, 0x72, 0x45, 0x0f, 0x24, 0xb3, 0x34, 0x1b, 0x99, 0xea, 0x09,
    0xf3, 0x03,
];
const MSLUG5_XOR2: [u8; 0x20] = [
    0x36, 0x09, 0xb0, 0x64, 0x95, 0x0f, 0x90, 0x42, 0x6e, 0x0f, 0x30, 0xf6, 0xe5, 0x08, 0x30,
    0x64, 0x08, 0x04, 0x00, 0x2f, 0x72, 0x09, 0xa0, 0x13, 0xc9, 0x0b, 0xa0, 0x3e, 0xc2, 0x00,
    0x40, 0x2b,
];
const SVC_XOR1: [u8; 0x20] = [
    0x3b, 0x6a, 0xf7, 0xb7, 0xe8, 0xa9, 0x20, 0x99, 0x9f, 0x39, 0x34, 0x0c, 0xc3, 0x9a, 0xa5,
    0xc8, 0xb8, 0x18, 0xce, 0x56, 0x94, 0x44, 0xe3, 0x7a, 0xf7, 0xdd, 0x42, 0xf0, 0x18, 0x60,
    0x92, 0x9f,
];
const SVC_XOR2: [u8; 0x20] = [
    0x69, 0x0b, 0x60, 0xd6, 0x4f, 0x01, 0x40, 0x1a, 0x9f, 0x0b, 0xf0, 0x75, 0x58, 0x0e, 0x60,
    0xb4, 0x14, 0x04, 0x20, 0xe4, 0xb9, 0x0d, 0x10, 0x89, 0xeb, 0x07, 0x30, 0x90, 0x50, 0x0e,
    0x20, 0x26,
];
const KOF2003_XOR2: [u8; 0x20] = [
    0x2f, 0x02, 0x60, 0xbb, 0x77, 0x01, 0x30, 0x08, 0xd8, 0x01, 0xa0, 0xdf, 0x37, 0x0a, 0xf0,
    0x65, 0x28, 0x03, 0xd0, 0x23, 0xd3, 0x03, 0x70, 0x42, 0xbb, 0x06, 0xf0, 0x28, 0xba, 0x0f,
    0xf0, 0x7a,
];
const KOF2003H_XOR2: [u8; 0x20] = [
    0x2b, 0x09, 0xd0, 0x7f, 0x51, 0x0b, 0x10, 0x4c, 0x5b, 0x07, 0x70, 0x9d, 0x3e, 0x0b, 0xb0,
    0xb6, 0x54, 0x09, 0xe0, 0xcc, 0x3d, 0x0d, 0x80, 0x99, 0x87, 0x03, 0x90, 0x82, 0xfe, 0x04,
    0x20, 0x18,
];

/// Common 0x800000-byte descramble used by mslug5 and svc (in-place shuffle,
/// then final relocation). `rom` must be at least 0x800000 bytes, already in
/// our big-endian byte order (see module doc).
fn descramble_800000(
    rom: &mut [u8],
    xor1: &[u8; 0x20],
    xor2: &[u8; 0x20],
    word_swap: &[u32; 16],
    blk64_swap: &[u32; 8],
    blk256_xor: usize,
    blk256_swap: &[u32; 8],
) {
    const ROM_SIZE: usize = 0x800000;
    assert!(rom.len() >= ROM_SIZE, "PVC P region must be 8MiB");

    // XOR passes (index-direct thanks to the j = i^1 substitution).
    for j in 0..0x100000 {
        rom[j] ^= xor1[j % 0x20];
    }
    for j in 0x100000..ROM_SIZE {
        rom[j] ^= xor2[j % 0x20];
    }

    // 16-bit bitswap on the middle bytes of each 4-byte group.
    for i in (0x100000..ROM_SIZE).step_by(4) {
        let w = (rom[i + 1] as u16) | ((rom[i + 2] as u16) << 8);
        let w = bitswap(w as u32, word_swap) as u16;
        rom[i + 1] = (w & 0xff) as u8;
        rom[i + 2] = (w >> 8) as u8;
    }

    let mut buf = rom[..ROM_SIZE].to_vec();

    // 64KiB-block shuffle over the first 1MiB.
    for i in 0..(0x100000 / 0x10000) {
        let ofst = (i & 0xf0) + bitswap((i & 0x0f) as u32, blk64_swap) as usize;
        rom[i * 0x10000..(i + 1) * 0x10000]
            .copy_from_slice(&buf[ofst * 0x10000..(ofst + 1) * 0x10000]);
    }

    // 256-byte-block shuffle over the banked area.
    for i in (0x100000..ROM_SIZE).step_by(0x100) {
        let ofst = (i & 0xf000ff)
            + ((i & 0x000f00) ^ blk256_xor)
            + ((bitswap(((i & 0x0ff000) >> 12) as u32, blk256_swap) as usize) << 12);
        rom[i..i + 0x100].copy_from_slice(&buf[ofst..ofst + 0x100]);
    }

    // Final relocation: rom[1M..2M] = rom'[7M..8M]; rom[2M..8M] = rom'[1M..7M].
    buf.copy_from_slice(&rom[..ROM_SIZE]);
    rom[0x100000..0x200000].copy_from_slice(&buf[0x700000..0x800000]);
    rom[0x200000..0x800000].copy_from_slice(&buf[0x100000..0x700000]);
}

/// Common 0x900000-byte descramble used by kof2003/kof2003h. The first
/// 1MiB of the extra bank at 0x800000 is pre-XORed from the scrambled data.
fn descramble_900000(
    rom: &mut [u8],
    xor1: &[u8; 0x20],
    xor2: &[u8; 0x20],
    word_swap: &[u32; 16],
    blk64_swap: &[u32; 8],
    blk256_xor: usize,
    blk256_swap: &[u32; 8],
) {
    const ROM_SIZE: usize = 0x900000;
    assert!(rom.len() >= ROM_SIZE, "kof2003 P region must be 9MiB");

    // Pre-step: derive the extra bank (index-direct after j = i^1, both
    // offsets shift bit 0 identically).
    for i in 0..0x100000 {
        rom[0x800000 + i] ^= rom[0x100002 | i];
    }

    for j in 0..0x100000 {
        rom[j] ^= xor1[j % 0x20];
    }
    for j in 0x100000..0x800000 {
        rom[j] ^= xor2[j % 0x20];
    }

    for i in (0x100000..0x800000).step_by(4) {
        let w = (rom[i + 1] as u16) | ((rom[i + 2] as u16) << 8);
        let w = bitswap(w as u32, word_swap) as u16;
        rom[i + 1] = (w & 0xff) as u8;
        rom[i + 2] = (w >> 8) as u8;
    }

    let mut buf = vec![0u8; ROM_SIZE];

    for i in 0..(0x100000 / 0x10000) {
        let ofst = (i & 0xf0) + bitswap((i & 0x0f) as u32, blk64_swap) as usize;
        buf[i * 0x10000..(i + 1) * 0x10000]
            .copy_from_slice(&rom[ofst * 0x10000..(ofst + 1) * 0x10000]);
    }

    for i in (0x100000..ROM_SIZE).step_by(0x100) {
        let ofst = (i & 0xf000ff)
            + ((i & 0x000f00) ^ blk256_xor)
            + ((bitswap(((i & 0x0ff000) >> 12) as u32, blk256_swap) as usize) << 12);
        buf[i..i + 0x100].copy_from_slice(&rom[ofst..ofst + 0x100]);
    }

    // Final relocation via buf.
    rom[0x000000..0x100000].copy_from_slice(&buf[0x000000..0x100000]);
    rom[0x100000..0x200000].copy_from_slice(&buf[0x800000..0x900000]);
    rom[0x200000..0x900000].copy_from_slice(&buf[0x100000..0x800000]);
}

pub fn mslug5_decrypt_68k(rom: &mut [u8]) {
    descramble_800000(
        rom,
        &MSLUG5_XOR1,
        &MSLUG5_XOR2,
        &[15, 14, 13, 12, 10, 11, 8, 9, 6, 7, 4, 5, 3, 2, 1, 0],
        &[7, 6, 5, 4, 1, 0, 3, 2],
        0x00700,
        &[5, 4, 7, 6, 1, 0, 3, 2],
    );
}

pub fn svc_px_decrypt(rom: &mut [u8]) {
    descramble_800000(
        rom,
        &SVC_XOR1,
        &SVC_XOR2,
        &[15, 14, 13, 12, 10, 11, 8, 9, 6, 7, 4, 5, 3, 2, 1, 0],
        &[7, 6, 5, 4, 2, 3, 0, 1],
        0x00a00,
        &[4, 5, 6, 7, 1, 0, 3, 2],
    );
}

pub fn kof2003_decrypt_68k(rom: &mut [u8]) {
    descramble_900000(
        rom,
        &SVC_XOR1,
        &KOF2003_XOR2,
        &[15, 14, 13, 12, 5, 4, 7, 6, 9, 8, 11, 10, 3, 2, 1, 0],
        &[7, 6, 5, 4, 0, 1, 2, 3],
        0x00800,
        &[4, 5, 6, 7, 1, 0, 3, 2],
    );
}

pub fn kof2003h_decrypt_68k(rom: &mut [u8]) {
    descramble_900000(
        rom,
        &MSLUG5_XOR1,
        &KOF2003H_XOR2,
        &[15, 14, 13, 12, 10, 11, 8, 9, 6, 7, 4, 5, 3, 2, 1, 0],
        &[7, 6, 5, 4, 1, 0, 3, 2],
        0x00400,
        &[6, 7, 4, 5, 0, 1, 2, 3],
    );
}

/// Dispatch the correct P-ROM descramble for `game`.
pub fn pvc_decrypt_68k(game: PvcGame, rom: &mut [u8]) {
    match game {
        PvcGame::Mslug5 => mslug5_decrypt_68k(rom),
        PvcGame::Svc => svc_px_decrypt(rom),
        PvcGame::Kof2003 => kof2003_decrypt_68k(rom),
        PvcGame::Kof2003h => kof2003h_decrypt_68k(rom),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Unpack then pack must round-trip the colour fields.
    #[test]
    fn pvc_color_pack_unpack_roundtrip() {
        let mut pvc = PvcProt::new(PvcGame::Mslug5);
        for &pen in &[0x0000u16, 0xffff, 0x1234, 0x8ace, 0x7531] {
            pvc.protection_w(0xff0, pen);
            let gb = pvc.protection_r(0xff1);
            let sr = pvc.protection_r(0xff2);
            // Re-pack what the unpack produced.
            pvc.protection_w(0xff4, gb);
            pvc.protection_w(0xff5, sr);
            assert_eq!(
                pvc.protection_r(0xff6),
                pen,
                "pack(unpack(0x{pen:04x})) mismatch"
            );
        }
    }

    /// Bankswitch write returns base + 0x100000 and stamps the marker.
    #[test]
    fn pvc_bankswitch_marker() {
        let mut pvc = PvcProt::new(PvcGame::Mslug5);
        // MAME's write_bankprot_pvc triggers get_bank_base() on EVERY write
        // at >= $FF8, so the $FF8 write itself already stamps the marker:
        // 0x2300 -> (0x2300 & 0xfe00) | 0xa0 = 0x22a0, bank base
        // (0x2300 >> 8) + 0x100000. The following $FF9 write then sees the
        // stamped low register: bankaddress = (0x22a0 >> 8) | (0x0045 << 8).
        let first = pvc.protection_w(0xff8, 0x2300);
        assert_eq!(first, Some(0x0023 + 0x100000));
        let base = pvc.protection_w(0xff9, 0x0045);
        assert_eq!(base, Some(0x4522 + 0x100000));
        assert_eq!(pvc.protection_r(0xff8), 0x22a0);
        assert_eq!(pvc.protection_r(0xff9) & 0x8000, 0);
    }

    /// The descramble must be deterministic and actually permute/alter data.
    #[test]
    fn mslug5_decrypt_is_deterministic_and_nontrivial() {
        let mut rom: Vec<u8> = (0..0x800000usize)
            .map(|i| ((i as u32).wrapping_mul(2654435761).wrapping_add(12345) >> 7) as u8)
            .collect();
        let orig = rom.clone();
        let mut rom2 = rom.clone();
        mslug5_decrypt_68k(&mut rom);
        mslug5_decrypt_68k(&mut rom2);
        assert_eq!(rom, rom2, "descramble must be deterministic");
        assert_ne!(rom, orig, "descramble must change the data");
        // Byte histogram must be preserved by the block moves + XOR + swap
        // only in total length, sanity-check length unchanged.
        assert_eq!(rom.len(), 0x800000);
    }

    /// All four descrambles run without panicking on a max-size region.
    #[test]
    fn all_pvc_decrypts_run() {
        let mut rom = vec![0xa5u8; 0x900000];
        kof2003_decrypt_68k(&mut rom);
        let mut rom = vec![0x5au8; 0x900000];
        kof2003h_decrypt_68k(&mut rom);
        let mut rom = vec![0x3cu8; 0x800000];
        svc_px_decrypt(&mut rom);
    }
}

//! NEO-CMC42 / NEO-CMC50 graphics (and M1) encryption.
//!
//! Port of MAME's `src/devices/bus/neogeo/prot_cmc.cpp` (BSD-3-Clause,
//! S. Smith / David Haywood / Fabio Priuli).
//!
//! The CMC chips sit between the cart's C-ROMs and the LSPC: sprite data
//! is stored scrambled (a 32-bit data XOR applied in byte couples plus a
//! 24-bit address scramble), and on encrypted carts the S (fix) tiles are
//! carved out of the *end* of the C data instead of shipping as an `s1`
//! file. The CMC50 revision additionally encrypts the Z80 M1 program with
//! an address scramble keyed on a checksum of the M1's first 64 KiB.
//!
//! Everything here operates on the *interleaved* sprite region — the
//! same byte layout MAME builds with `ROM_LOAD16_BYTE` (c1 at even
//! addresses, c2 at odd, then c3/c4, ...). Helpers to interleave /
//! de-interleave our per-file `c_roms` vectors live at the bottom.

use super::cmc_tables as t;

/// Which CMC revision a cart carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmcVariant {
    /// CMC42: C/S encryption only (kof99, garou, mslug3, ...).
    Cmc42,
    /// CMC50: C/S encryption + M1 encryption (kof2000, mslug4/5, svc, ...).
    Cmc50,
}

/// Per-game CMC parameters.
#[derive(Debug, Clone, Copy)]
pub struct CmcGame {
    pub variant: CmcVariant,
    /// Per-game address-XOR seed (MAME's `*_GFX_KEY`).
    pub extra_xor: u32,
    /// Size of the fix-tile region extracted from the end of the C data
    /// (0x20000 for the standard char set, 0x80000 for the large one).
    pub sfix_bytes: usize,
}

/// Look up CMC parameters by MAME set name. Returns `None` for carts
/// without CMC graphics encryption.
#[must_use]
pub fn detect_cmc(name: &str) -> Option<CmcGame> {
    use CmcVariant::{Cmc42, Cmc50};
    let (variant, extra_xor, sfix_bytes) = match name {
        // ---- CMC42 (keys from prot_cmc.h) ----
        "kof99" | "kof99h" | "kof99e" | "kof99k" | "kof99ka" | "kof99n" => (Cmc42, 0x00, 0x20000),
        "garou" | "garouh" => (Cmc42, 0x06, 0x80000),
        "mslug3" | "mslug3h" | "mslug3a" => (Cmc42, 0xad, 0x80000),
        "zupapa" => (Cmc42, 0xbd, 0x20000),
        "ganryu" => (Cmc42, 0x07, 0x20000),
        "s1945p" => (Cmc42, 0x05, 0x20000),
        "preisle2" => (Cmc42, 0x9f, 0x20000),
        "bangbead" => (Cmc42, 0xf8, 0x20000),
        "nitd" => (Cmc42, 0xff, 0x20000),
        "sengoku3" | "sengoku3a" => (Cmc42, 0xfe, 0x20000),
        // ---- CMC50 ----
        "kof2000" | "kof2000n" => (Cmc50, 0x00, 0x80000),
        "kof2001" | "kof2001h" => (Cmc50, 0x1e, 0x20000),
        "mslug4" | "mslug4h" => (Cmc50, 0x31, 0x80000),
        "rotd" | "rotdh" => (Cmc50, 0x3f, 0x20000),
        "pnyaa" | "pnyaaa" => (Cmc50, 0x2e, 0x20000),
        "kof2002" | "kof2002h" => (Cmc50, 0xec, 0x20000),
        "matrim" => (Cmc50, 0x6a, 0x80000),
        "samsho5" | "samsho5h" => (Cmc50, 0x0f, 0x20000),
        "samsho5sp" | "samsh5sp" | "samsh5sph" | "samsh5spho" => (Cmc50, 0x0d, 0x20000),
        "mslug5" | "mslug5h" => (Cmc50, 0x19, 0x20000),
        "svc" => (Cmc50, 0x57, 0x80000),
        "kof2003" | "kof2003h" => (Cmc50, 0x9d, 0x80000),
        "jockeygp" | "jockeygpa" => (Cmc50, 0xac, 0x20000),
        _ => return None,
    };
    Some(CmcGame { variant, extra_xor, sfix_bytes })
}

/// The 9 table pointers `gfx_decrypt` works from — CMC42 uses the kof99
/// set, CMC50 the kof2000 set.
struct Tables {
    type0_t03: &'static [u8; 256],
    type0_t12: &'static [u8; 256],
    type1_t03: &'static [u8; 256],
    type1_t12: &'static [u8; 256],
    address_8_15_xor1: &'static [u8; 256],
    address_8_15_xor2: &'static [u8; 256],
    address_16_23_xor1: &'static [u8; 256],
    address_16_23_xor2: &'static [u8; 256],
    address_0_7_xor: &'static [u8; 256],
}

impl Tables {
    fn for_variant(v: CmcVariant) -> Self {
        match v {
            CmcVariant::Cmc42 => Tables {
                type0_t03: &t::KOF99_TYPE0_T03,
                type0_t12: &t::KOF99_TYPE0_T12,
                type1_t03: &t::KOF99_TYPE1_T03,
                type1_t12: &t::KOF99_TYPE1_T12,
                address_8_15_xor1: &t::KOF99_ADDRESS_8_15_XOR1,
                address_8_15_xor2: &t::KOF99_ADDRESS_8_15_XOR2,
                address_16_23_xor1: &t::KOF99_ADDRESS_16_23_XOR1,
                address_16_23_xor2: &t::KOF99_ADDRESS_16_23_XOR2,
                address_0_7_xor: &t::KOF99_ADDRESS_0_7_XOR,
            },
            CmcVariant::Cmc50 => Tables {
                type0_t03: &t::KOF2000_TYPE0_T03,
                type0_t12: &t::KOF2000_TYPE0_T12,
                type1_t03: &t::KOF2000_TYPE1_T03,
                type1_t12: &t::KOF2000_TYPE1_T12,
                address_8_15_xor1: &t::KOF2000_ADDRESS_8_15_XOR1,
                address_8_15_xor2: &t::KOF2000_ADDRESS_8_15_XOR2,
                address_16_23_xor1: &t::KOF2000_ADDRESS_16_23_XOR1,
                address_16_23_xor2: &t::KOF2000_ADDRESS_16_23_XOR2,
                address_0_7_xor: &t::KOF2000_ADDRESS_0_7_XOR,
            },
        }
    }

    /// MAME `cmc_prot_device::decrypt` — one byte-couple data XOR.
    #[inline]
    fn decrypt_couple(
        &self,
        c0: u8,
        c1: u8,
        table0hi: &[u8; 256],
        table0lo: &[u8; 256],
        table1: &[u8; 256],
        base: usize,
        invert: bool,
    ) -> (u8, u8) {
        let hi = (base >> 8) & 0xff;
        let tmp = table1[(base & 0xff) ^ self.address_0_7_xor[hi] as usize];
        let xor0 = (table0hi[hi] & 0xfe) | (tmp & 0x01);
        let xor1 = (tmp & 0xfe) | (table0lo[hi] & 0x01);
        if invert {
            (c1 ^ xor0, c0 ^ xor1)
        } else {
            (c0 ^ xor0, c1 ^ xor1)
        }
    }
}

/// MAME `cmc_prot_device::gfx_decrypt` — decrypt the interleaved sprite
/// region in place (data XOR pass then address-scramble pass).
fn gfx_decrypt(tables: &Tables, rom: &mut [u8], extra_xor: u32) {
    let rom_size = rom.len();
    let words = rom_size / 4;
    let mut buf = vec![0u8; rom_size];

    // Data xor.
    for rpos in 0..words {
        let b = 4 * rpos;
        let (r0, r3) = tables.decrypt_couple(
            rom[b],
            rom[b + 3],
            tables.type0_t03,
            tables.type0_t12,
            tables.type1_t03,
            rpos,
            (rpos >> 8) & 1 != 0,
        );
        buf[b] = r0;
        buf[b + 3] = r3;
        let inv2 =
            ((rpos >> 16) ^ tables.address_16_23_xor2[(rpos >> 8) & 0xff] as usize) & 1 != 0;
        let (r1, r2) = tables.decrypt_couple(
            rom[b + 1],
            rom[b + 2],
            tables.type0_t12,
            tables.type0_t03,
            tables.type1_t12,
            rpos,
            inv2,
        );
        buf[b + 1] = r1;
        buf[b + 2] = r2;
    }

    // Address xor.
    for rpos in 0..words {
        let mut baser = rpos ^ extra_xor as usize;
        baser ^= (tables.address_8_15_xor1[(baser >> 16) & 0xff] as usize) << 8;
        baser ^= (tables.address_8_15_xor2[baser & 0xff] as usize) << 8;
        baser ^= (tables.address_16_23_xor1[baser & 0xff] as usize) << 16;
        baser ^= (tables.address_16_23_xor2[(baser >> 8) & 0xff] as usize) << 16;
        baser ^= tables.address_0_7_xor[(baser >> 8) & 0xff] as usize;

        if rom_size == 0x300_0000 {
            // special handling for preisle2
            if rpos < 0x200_0000 / 4 {
                baser &= (0x200_0000 / 4) - 1;
            } else {
                baser = 0x200_0000 / 4 + (baser & ((0x100_0000 / 4) - 1));
            }
        } else if rom_size == 0x600_0000 {
            // special handling for kf2k3pcb
            if rpos < 0x400_0000 / 4 {
                baser &= (0x400_0000 / 4) - 1;
            } else {
                baser = 0x400_0000 / 4 + (baser & ((0x100_0000 / 4) - 1));
            }
        } else {
            // Clamp to the real rom size (region sizes are powers of two).
            baser &= words - 1;
        }

        rom[4 * rpos..4 * rpos + 4].copy_from_slice(&buf[4 * baser..4 * baser + 4]);
    }
}

impl CmcGame {
    /// Decrypt the interleaved sprite region in place (CMC42 or CMC50).
    pub fn gfx_decrypt(&self, sprite_region: &mut [u8]) {
        let tables = Tables::for_variant(self.variant);
        gfx_decrypt(&tables, sprite_region, self.extra_xor);
    }

    /// MAME `sfix_decrypt`: on encrypted carts the S (fix) data comes
    /// from the end of the (already decrypted) C data, with an in-block
    /// byte shuffle. Returns the extracted S-ROM.
    #[must_use]
    pub fn sfix_extract(&self, sprite_region: &[u8]) -> Vec<u8> {
        let tx = self.sfix_bytes.min(sprite_region.len());
        let src = &sprite_region[sprite_region.len() - tx..];
        (0..tx)
            .map(|i| src[(i & !0x1f) + ((i & 7) << 2) + ((!i & 8) >> 2) + ((i & 0x10) >> 4)])
            .collect()
    }
}

// ===================== CMC50 M1 decryption =====================

/// MAME `bitswap<16>` — `order[0]` names the source bit for output
/// bit 15, `order[15]` for output bit 0 (same argument order as C++).
#[inline]
fn bitswap16(v: u16, order: [u8; 16]) -> u16 {
    let mut out = 0u16;
    for (n, &b) in order.iter().enumerate() {
        out |= ((v >> b) & 1) << (15 - n);
    }
    out
}

/// MAME `cmc_prot_device::m1_address_scramble`.
fn m1_address_scramble(address: usize, key: u16) -> usize {
    const P1: [[u8; 16]; 8] = [
        [15, 14, 10, 7, 1, 2, 3, 8, 0, 12, 11, 13, 6, 9, 5, 4],
        [7, 1, 8, 11, 15, 9, 2, 3, 5, 13, 4, 14, 10, 0, 6, 12],
        [8, 6, 14, 3, 10, 7, 15, 1, 4, 0, 2, 5, 13, 11, 12, 9],
        [2, 8, 15, 9, 3, 4, 11, 7, 13, 6, 0, 10, 1, 12, 14, 5],
        [1, 13, 6, 15, 14, 3, 8, 10, 9, 4, 7, 12, 5, 2, 0, 11],
        [11, 15, 3, 4, 7, 0, 9, 2, 6, 14, 12, 1, 8, 5, 10, 13],
        [10, 5, 13, 8, 6, 15, 1, 14, 11, 9, 3, 0, 12, 7, 4, 2],
        [9, 3, 7, 0, 2, 12, 4, 11, 14, 10, 5, 8, 15, 13, 1, 6],
    ];

    let block = (address >> 16) & 7;
    let mut aux = (address & 0xffff) as u16;

    aux ^= bitswap16(key, [12, 0, 2, 4, 8, 15, 7, 13, 10, 1, 3, 6, 11, 9, 14, 5]);
    let p = &P1[block];
    aux = bitswap16(
        aux,
        [
            p[15], p[14], p[13], p[12], p[11], p[10], p[9], p[8], p[7], p[6], p[5], p[4], p[3],
            p[2], p[1], p[0],
        ],
    );
    aux ^= t::M1_ADDRESS_0_7_XOR[((aux >> 8) & 0xff) as usize] as u16;
    aux ^= (t::M1_ADDRESS_8_15_XOR[(aux & 0xff) as usize] as u16) << 8;
    aux = bitswap16(aux, [7, 15, 14, 6, 5, 13, 12, 4, 11, 3, 10, 2, 9, 1, 8, 0]);

    (block << 16) | aux as usize
}

/// MAME `cmc50_m1_decrypt`: the CMC50 checksums the first 64 KiB of the
/// encrypted M1 and uses that as the key for an address descramble over
/// the full 512 KiB. Returns the decrypted 512 KiB Z80 program.
#[must_use]
pub fn cmc50_m1_decrypt(m1_encrypted: &[u8]) -> Vec<u8> {
    const ROM_SIZE: usize = 0x80000;
    // Zero-pad to 512 KiB (some carts ship 256 KiB M1s in a 512 KiB region).
    let mut rom = vec![0u8; ROM_SIZE];
    let n = m1_encrypted.len().min(ROM_SIZE);
    rom[..n].copy_from_slice(&m1_encrypted[..n]);

    // util::sum16: byte-wise wrapping sum into a u16.
    let key = rom[..0x10000]
        .iter()
        .fold(0u16, |acc, &b| acc.wrapping_add(u16::from(b)));
    log::info!("cmc50 m1 decrypt: key={key:#06x}");

    let mut out = vec![0u8; ROM_SIZE];
    for (i, o) in out.iter_mut().enumerate() {
        *o = rom[m1_address_scramble(i, key)];
    }
    out
}

// ===================== C-ROM (de)interleaving =====================

/// Build the interleaved sprite region MAME's decrypts operate on:
/// per pair, c(2n) supplies even bytes and c(2n+1) odd bytes
/// (`ROM_LOAD16_BYTE` layout).
#[must_use]
pub fn interleave_c_roms(c_roms: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = c_roms
        .chunks(2)
        .filter(|p| p.len() == 2)
        .map(|p| p[0].len().min(p[1].len()) * 2)
        .sum();
    let mut out = Vec::with_capacity(total);
    for pair in c_roms.chunks(2) {
        if pair.len() < 2 {
            break;
        }
        let n = pair[0].len().min(pair[1].len());
        for i in 0..n {
            out.push(pair[0][i]);
            out.push(pair[1][i]);
        }
    }
    out
}

/// Split an interleaved sprite region back into one (even, odd) pair —
/// the format `decode_sprite_gfx` consumes.
#[must_use]
pub fn deinterleave_region(region: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let half = region.len() / 2;
    let mut even = Vec::with_capacity(half);
    let mut odd = Vec::with_capacity(half);
    for pair in region.chunks_exact(2) {
        even.push(pair[0]);
        odd.push(pair[1]);
    }
    (even, odd)
}

// ===================== tests =====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_known_games() {
        let g = detect_cmc("mslug3").unwrap();
        assert_eq!(g.variant, CmcVariant::Cmc42);
        assert_eq!(g.extra_xor, 0xad);
        assert_eq!(g.sfix_bytes, 0x80000);

        let g = detect_cmc("mslug5").unwrap();
        assert_eq!(g.variant, CmcVariant::Cmc50);
        assert_eq!(g.extra_xor, 0x19);
        assert_eq!(g.sfix_bytes, 0x20000);

        assert!(detect_cmc("mslugx").is_none());
        assert!(detect_cmc("puzzledp").is_none());
    }

    #[test]
    fn sfix_shuffle_is_a_permutation() {
        // Within each 32-byte block the source index formula must visit
        // each offset exactly once.
        let mut seen = [false; 32];
        for i in 0..32usize {
            let s = (i & !0x1f) + ((i & 7) << 2) + ((!i & 8) >> 2) + ((i & 0x10) >> 4);
            assert!(s < 32);
            assert!(!seen[s], "duplicate source {s}");
            seen[s] = true;
        }
    }

    #[test]
    fn m1_scramble_is_bijective() {
        // The address scramble must be a bijection over the 512 KiB space
        // for any key, or the descramble would drop program bytes.
        let key = 0x1234u16;
        let mut seen = vec![false; 0x80000];
        for a in 0..0x80000usize {
            let s = m1_address_scramble(a, key);
            assert!(s < 0x80000, "out of range: {s:#x}");
            assert!(!seen[s], "collision at {s:#x}");
            seen[s] = true;
        }
    }

    #[test]
    fn interleave_roundtrip() {
        let c1 = vec![1u8, 3, 5, 7];
        let c2 = vec![2u8, 4, 6, 8];
        let region = interleave_c_roms(&[c1.clone(), c2.clone()]);
        assert_eq!(region, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let (even, odd) = deinterleave_region(&region);
        assert_eq!(even, c1);
        assert_eq!(odd, c2);
    }

    #[test]
    fn gfx_decrypt_runs_on_small_region() {
        // Smoke test: 1 MiB power-of-two region, both variants.
        let g42 = CmcGame { variant: CmcVariant::Cmc42, extra_xor: 0xad, sfix_bytes: 0x100 };
        let g50 = CmcGame { variant: CmcVariant::Cmc50, extra_xor: 0x19, sfix_bytes: 0x100 };
        let mut r: Vec<u8> = (0..0x100000usize).map(|i| (i * 7 + 3) as u8).collect();
        let orig = r.clone();
        g42.gfx_decrypt(&mut r);
        assert_ne!(r, orig, "decrypt must change the data");
        let s = g42.sfix_extract(&r);
        assert_eq!(s.len(), 0x100);
        g50.gfx_decrypt(&mut r);
        let s = g50.sfix_extract(&r);
        assert_eq!(s.len(), 0x100);
    }
}

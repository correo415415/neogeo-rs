//! NEO-PCM2 'V' (ADPCM) ROM encryption — port of MAME
//! `src/devices/bus/neogeo/prot_pcm2.cpp` (BSD-3-Clause,
//! S. Smith / David Haywood / Fabio Priuli; NEO-PCM2 info thanks to Elsemi).
//!
//! Two schemes:
//! - `decrypt(rom, value)`: early PCM2 games (mslug4/ms4plus value=8,
//!   rotd value=16, pnyaa value=4) — address-line swap inside value-byte
//!   blocks over the whole V region.
//! - `swap(rom, value)`: later PVC-era games with additional scrambling over
//!   a fixed 16MiB region (mslug5=2, svc=3, kof2003=5, kof2002=0, matrim=1,
//!   samsho5=4, samsho5sp=6).

use super::prot::bitswap;

/// Per-game (offset, addr_xor) pairs for `swap`.
const ADDRS: [(u32, u32); 7] = [
    (0x000000, 0xa5000), // 0: kof2002
    (0xffce20, 0x01000), // 1: matrim
    (0xfe2cf6, 0x4e001), // 2: mslug5
    (0xffac28, 0xc2000), // 3: svc
    (0xfeb2c0, 0x0a000), // 4: samsho5
    (0xff14ea, 0xa7001), // 5: kof2003
    (0xffb440, 0x02000), // 6: samsho5sp
];

/// Per-game 8-byte XOR streams for `swap`.
const XORDATA: [[u8; 8]; 7] = [
    [0xf9, 0xe0, 0x5d, 0xf3, 0xea, 0x92, 0xbe, 0xef],
    [0xc4, 0x83, 0xa8, 0x5f, 0x21, 0x27, 0x64, 0xaf],
    [0xc3, 0xfd, 0x81, 0xac, 0x6d, 0xe7, 0xbf, 0x9e],
    [0xc3, 0xfd, 0x81, 0xac, 0x6d, 0xe7, 0xbf, 0x9e],
    [0xcb, 0x29, 0x7d, 0x43, 0xd2, 0x3a, 0xc2, 0xb4],
    [0x4b, 0xa4, 0x63, 0x46, 0xf0, 0x91, 0xea, 0x62],
    [0x4b, 0xa4, 0x63, 0x46, 0xf0, 0x91, 0xea, 0x62],
];

/// Early NEO-PCM2 scheme: swap address lines within `value`-byte blocks.
/// `value` is the block size in bytes (mslug4/ms4plus: 8, rotd: 16, pnyaa: 4)
/// and must be a power of two >= 4. Operates on 16-bit little-endian words
/// exactly like MAME's `uint16_t*` view of the raw byte region.
pub fn pcm2_decrypt(ymrom: &mut [u8], value: usize) {
    let words = value / 2; // block size in u16 words
    let xor = value / 4; // word-index XOR inside a block
    let mut buffer = vec![0u16; words];
    let size_words = ymrom.len() / 2;
    let mut i = 0usize;
    while i + words <= size_words {
        for (j, w) in buffer.iter_mut().enumerate() {
            let o = (i + j) * 2;
            *w = (ymrom[o] as u16) | ((ymrom[o + 1] as u16) << 8);
        }
        for j in 0..words {
            let w = buffer[j ^ xor];
            let o = (i + j) * 2;
            ymrom[o] = (w & 0xff) as u8;
            ymrom[o + 1] = (w >> 8) as u8;
        }
        i += words;
    }
}

/// Later NEO-PCM2 scheme (PVC-era additional scrambling) over a fixed
/// 16MiB region. `value` selects the game row in `ADDRS`/`XORDATA`:
/// kof2002=0, matrim=1, **mslug5=2**, svc=3, samsho5=4, kof2003=5,
/// samsho5sp=6. `ymrom` must be at least 0x1000000 bytes (pad with 0 if the
/// cart V data is smaller).
pub fn pcm2_swap(ymrom: &mut [u8], value: usize) {
    assert!(ymrom.len() >= 0x1000000, "PCM2 swap needs a 16MiB V region");
    let (addr_off, addr_xor) = ADDRS[value];
    let xordata = &XORDATA[value];
    let buf = ymrom[..0x1000000].to_vec();
    // bitswap<24>(i, 23..17, 0, 15..1, 16): swap bit16 <-> bit0.
    const SWAP_BITS: [u32; 24] = [
        23, 22, 21, 20, 19, 18, 17, 0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 16,
    ];
    for i in 0..0x1000000usize {
        let j = (bitswap(i as u32, &SWAP_BITS) ^ addr_xor) as usize;
        let d = ((i as u32).wrapping_add(addr_off) & 0xffffff) as usize;
        ymrom[j] = buf[d] ^ xordata[j & 0x7];
    }
}

/// Which PCM2 scheme (and per-game parameter) a cart set uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pcm2Mode {
    /// Early scheme: `pcm2_decrypt` block size in bytes.
    Decrypt(usize),
    /// Later scheme: `pcm2_swap` game row index.
    Swap(usize),
}

/// Map a MAME set name to its NEO-PCM2 V-ROM scheme (None = V is plain).
pub fn detect_pcm2(name: &str) -> Option<Pcm2Mode> {
    let n = name.to_ascii_lowercase();
    match n.as_str() {
        // Early NEO-PCM2 (address-line swap): value = block size.
        "mslug4" | "ms4plus" => Some(Pcm2Mode::Decrypt(8)),
        "rotd" => Some(Pcm2Mode::Decrypt(16)),
        "pnyaa" => Some(Pcm2Mode::Decrypt(4)),
        // Later NEO-PCM2 (16MiB scramble): value = table row.
        "kof2002" => Some(Pcm2Mode::Swap(0)),
        "matrim" => Some(Pcm2Mode::Swap(1)),
        "mslug5" | "mslug5h" => Some(Pcm2Mode::Swap(2)),
        "svc" => Some(Pcm2Mode::Swap(3)),
        "samsho5" | "samsho5h" => Some(Pcm2Mode::Swap(4)),
        "kof2003" | "kof2003h" => Some(Pcm2Mode::Swap(5)),
        "samsh5sp" | "samsh5sph" | "samsh5spho" => Some(Pcm2Mode::Swap(6)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// decrypt() is an involution for power-of-two block XOR swaps.
    #[test]
    fn pcm2_decrypt_is_involution() {
        let orig: Vec<u8> = (0..0x1000usize)
            .map(|i| ((i as u32).wrapping_mul(2654435761) >> 5) as u8)
            .collect();
        for &value in &[4usize, 8, 16] {
            let mut rom = orig.clone();
            pcm2_decrypt(&mut rom, value);
            assert_ne!(rom, orig, "value={value} must change data");
            pcm2_decrypt(&mut rom, value);
            assert_eq!(rom, orig, "value={value} must be an involution");
        }
    }

    /// swap() must be a bijection on the 16MiB region: every source byte is
    /// used exactly once (the address map i->j is a permutation).
    #[test]
    fn pcm2_swap_address_map_is_permutation() {
        // Verify the pure address transform without allocating 16MiB twice:
        // j = bitswap(i) ^ addr_xor must be a bijection of 0..2^24 —
        // bitswap of distinct bits is bijective, XOR is bijective. Spot-check
        // determinism + range on a sample.
        const SWAP_BITS: [u32; 24] = [
            23, 22, 21, 20, 19, 18, 17, 0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 16,
        ];
        let mut seen = std::collections::HashSet::new();
        for i in (0..0x1000000u32).step_by(0x1111) {
            let j = bitswap(i, &SWAP_BITS) ^ ADDRS[2].1;
            assert!(j < 0x1000000);
            assert!(seen.insert(j), "collision at i={i:#x}");
        }
    }

    /// swap() runs on a full-size region and is deterministic.
    #[test]
    fn pcm2_swap_deterministic() {
        let mut rom: Vec<u8> = (0..0x1000000usize).map(|i| (i >> 3) as u8).collect();
        let mut rom2 = rom.clone();
        pcm2_swap(&mut rom, 2);
        pcm2_swap(&mut rom2, 2);
        assert_eq!(rom, rom2);
    }
}

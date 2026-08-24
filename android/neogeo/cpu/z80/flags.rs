//! Z80 flag register (F) layout and helpers.
//!
//! ```text
//!   bit 7  S  sign (bit 7 of result)
//!   bit 6  Z  zero
//!   bit 5  Y  (undocumented) bit 5 of result
//!   bit 4  H  half-carry (carry from bit 3 to bit 4)
//!   bit 3  X  (undocumented) bit 3 of result
//!   bit 2  P/V parity (logic) or overflow (arithmetic)
//!   bit 1  N  add/subtract (1 = subtract)
//!   bit 0  C  carry
//! ```

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Flag {
    C = 0x01,
    N = 0x02,
    P = 0x04,
    X = 0x08,
    H = 0x10,
    Y = 0x20,
    Z = 0x40,
    S = 0x80,
}

impl Flag {
    #[must_use]
    pub const fn mask(self) -> u8 { self as u8 }
}

/// Parity LUT: `PARITY[x] == 0x04` iff `x` has even bit-population, else 0.
#[must_use]
pub const fn parity_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut n = i as u8;
        let mut c: u8 = 0;
        while n != 0 {
            c ^= n & 1;
            n >>= 1;
        }
        t[i] = if c == 0 { Flag::P as u8 } else { 0 };
        i += 1;
    }
    t
}

pub static PARITY: [u8; 256] = parity_table();

/// Compute Sign/Zero/YX for byte result `r`.
#[inline]
#[must_use]
pub const fn sz53(r: u8) -> u8 {
    let mut f = r & (Flag::S as u8 | Flag::Y as u8 | Flag::X as u8);
    if r == 0 {
        f |= Flag::Z as u8;
    }
    f
}

/// Like `sz53` but additionally OR in the parity of `r`.
#[inline]
#[must_use]
pub fn sz53p(r: u8) -> u8 {
    sz53(r) | PARITY[r as usize]
}

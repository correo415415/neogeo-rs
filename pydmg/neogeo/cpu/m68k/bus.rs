//! Trait every memory bus must implement so the CPU can read/write.
//!
//! All addresses are 24-bit physical (the 68000 has 24 address pins). The
//! upper byte of a 32-bit value passed in must already be masked to zero by
//! the caller; the CPU does this for you.

/// A read/write target for the M68K CPU.
///
/// Implementations decide how 16-bit aligned word/long accesses are handled
/// when they cross device boundaries (e.g. ROM → RAM). The CPU never issues
/// odd-aligned word/long accesses; if you see one in software, raise an
/// `AddressError` exception at the call site.
pub trait Bus {
    /// Read a single byte from the 24-bit address space.
    fn read8(&mut self, addr: u32) -> u8;
    /// Read a 16-bit word (big-endian, address must be even).
    fn read16(&mut self, addr: u32) -> u16 {
        let hi = self.read8(addr) as u16;
        let lo = self.read8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }
    /// Read a 32-bit long (big-endian, address must be even).
    fn read32(&mut self, addr: u32) -> u32 {
        let hi = self.read16(addr) as u32;
        let lo = self.read16(addr.wrapping_add(2)) as u32;
        (hi << 16) | lo
    }

    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16) {
        self.write8(addr, (value >> 8) as u8);
        self.write8(addr.wrapping_add(1), value as u8);
    }
    fn write32(&mut self, addr: u32, value: u32) {
        self.write16(addr, (value >> 16) as u16);
        self.write16(addr.wrapping_add(2), value as u16);
    }
}

/// A simple flat-memory bus useful for unit tests and the CPU test suite.
///
/// 16 MiB linear array, no banking. NOT for actual emulation — use
/// `crate::memory::bus::NeoGeoBus` for the real Neo Geo memory map.
#[derive(Debug)]
pub struct FlatBus {
    pub mem: Vec<u8>,
}

impl Default for FlatBus {
    fn default() -> Self {
        Self::new()
    }
}

impl FlatBus {
    pub fn new() -> Self {
        Self {
            mem: vec![0; 0x100_0000],
        }
    }

    pub fn load(&mut self, addr: u32, bytes: &[u8]) {
        let a = addr as usize;
        self.mem[a..a + bytes.len()].copy_from_slice(bytes);
    }
}

impl Bus for FlatBus {
    fn read8(&mut self, addr: u32) -> u8 {
        self.mem[(addr & 0x00FF_FFFF) as usize]
    }
    fn write8(&mut self, addr: u32, value: u8) {
        self.mem[(addr & 0x00FF_FFFF) as usize] = value;
    }
}

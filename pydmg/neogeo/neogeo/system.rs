//! High-level system that wires CPU + bus + LSPC + Z80 + YM2610 together.

use anyhow::Result;
use crate::cpu::m68k::{Bus, Cpu as M68k};
use crate::cpu::z80::Cpu as Z80;

use crate::audio::audio_bus::{AudioBus, AudioBusRef};
use crate::memory::bus::NeoGeoBus;
use crate::memory::rom::RomSet;

/// Hardware variant the user wants to emulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hardware {
    Aes,
    Mvs,
}

#[derive(Debug, Clone)]
pub struct SystemConfig {
    pub hardware: Hardware,
    /// If true, write a trace line for every instruction executed.
    pub trace_cpu: bool,
    /// If true, trace Z80 I/O ports (very verbose).
    pub trace_audio_io: bool,
    /// If `Some(rate)`, capture stereo audio samples at `rate` Hz and
    /// store them in `audio_buffer` for later WAV export.
    pub audio_sample_rate: Option<u32>,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            hardware: Hardware::Mvs,
            trace_cpu: false,
            trace_audio_io: false,
            audio_sample_rate: None,
        }
    }
}

// ---------------------------------------------------------------------------
//   Clock constants — derived from MAME `src/mame/snk/neogeo.h:33-35`.
// ---------------------------------------------------------------------------
//   NEOGEO_MASTER_CLOCK = 24_000_000 Hz
//   M68000 = MASTER / 2 = 12 MHz
//   Z80    = MASTER / 6 =  4 MHz
//   YM2610 = MASTER / 3 =  8 MHz
//
// Ratio Z80 / 68K = (MASTER/6) / (MASTER/2) = 1/3. So for every 68K cycle,
// the Z80 advances by 1/3 cycle. We use a fractional accumulator.
//
// YM2610 native output rate: master / 144 = 24M / 144 = 166_666 Hz.
// (That's the OPN base, ymfm internal rate.) For the chip output that
// reaches the speaker, ymfm uses master/144 = 55_555 Hz on 8 MHz YM2610,
// which is what we target with `step_one_sample`.

const M68K_CYCLES_PER_FRAME: u64 = 200_000; // ≈ 12 MHz / 60 Hz
const Z80_CYCLES_NUM: u64 = 1; // Z80 cycles per 3 × 68K cycles
const Z80_CYCLES_DEN: u64 = 3;
const YM_OUTPUT_HZ: u64 = 55_555;
/// Approximate 68K cycles per audio sample. 12e6 / 55555 ≈ 216.
const M68K_CYCLES_PER_AUDIO_SAMPLE: u64 = 12_000_000 / YM_OUTPUT_HZ;

/// Identify NEO-CMC fix-layer banking from the cart short name.
///
/// Mirrors the manual associations in MAME `neogeo.cpp` between cart
/// machine configs and `cartslot_fixed("sma_garou")` /
/// `cartslot_fixed("cmc50_kof2000n")` etc.
fn detect_fix_bank_type(name: &str) -> crate::graphics::video::FixBankType {
    let n = name.to_ascii_lowercase();
    // FIX_BANKTYPE_GAROU: Garou + Metal Slug 3/4 family.
    const GAROU_FAMILY: &[&str] = &[
        "garou", "garouh", "garoubl",
        "mslug3", "mslug3a", "mslug3h", "mslug3b6",
        "mslug4", "mslug4h", "ms4plus",
    ];
    if GAROU_FAMILY.iter().any(|g| n == *g) {
        return crate::graphics::video::FixBankType::Garou;
    }
    // FIX_BANKTYPE_KOF2000: KOF2000 and its bootlegs.
    const KOF2K_FAMILY: &[&str] = &[
        "kof2000", "kof2000n",
    ];
    if KOF2K_FAMILY.iter().any(|g| n == *g) {
        return crate::graphics::video::FixBankType::Kof2000;
    }
    crate::graphics::video::FixBankType::Std
}

pub struct System {
    pub m68k: M68k,
    pub z80: Z80,
    pub bus: NeoGeoBus,
    pub audio: AudioBus,
    pub config: SystemConfig,
    /// Master cycle counter, 68000 cycles since reset.
    pub master_cycles: u64,
    /// Last instruction count — bounded so the CLI can do "run N instructions".
    pub instructions: u64,
    /// Fix-layer tile ROM (`.s1`) — used by the video renderer when
    /// HC259 Q5 ("use_cart_audio" / fix-source mux) selects the cart path.
    pub s_rom: Vec<u8>,
    /// BIOS fix-layer S-ROM (`sfix.sfix`) — used when HC259 Q5=0
    /// (BIOS path active). Empty for AES-only sets without an SFIX.
    pub bios_sfix: Vec<u8>,
    /// NEO-CMC fix-layer banking mode. Defaults to `Std`; set to `Garou`
    /// or `Kof2000` by the cart auto-detect (or the user via CLI flag).
    pub fix_bank_type: crate::graphics::video::FixBankType,
    /// Sprite tile C-ROM banks (`.c1`, `.c2`, …) — used by the video renderer.
    pub c_roms: Vec<Vec<u8>>,
    /// Pre-decoded sprite graphics. One byte per pixel.
    pub sprite_gfx_decoded: Vec<u8>,
    /// Y-zoom lookup ROM (`000-lo.lo`, 64 KiB).
    pub lo_rom: Vec<u8>,
    /// DEBUG: frame counter.
    pub dbg_frame: u32,
    /// DEBUG: number of times BIOS SYSTEM_IO ($C0044A) was entered.
    pub dbg_sysio_hits: u64,
    /// DEBUG: number of level-1 (VBlank) IRQs actually taken by the 68K.
    pub dbg_irq1_taken: u64,

    /// Z80 cycle budget accumulator. Carries fractional cycles across `step()`s.
    z80_cycles_owed: i64,
    /// Audio sample budget — when it exceeds M68K_CYCLES_PER_AUDIO_SAMPLE we
    /// pull one stereo sample from the YM2610.
    audio_cycles_owed: u64,
    /// Captured stereo audio (interleaved L,R,L,R…) at YM_OUTPUT_HZ.
    pub audio_buffer: Vec<i16>,
}

impl System {
    pub fn new(config: SystemConfig) -> Self {
        let audio_trace = config.trace_audio_io;
        let mut audio = AudioBus::new();
        audio.trace = audio_trace;
        Self {
            m68k: M68k::new(),
            z80: Z80::new(),
            bus: NeoGeoBus::new(),
            audio,
            config,
            master_cycles: 0,
            instructions: 0,
            s_rom: Vec::new(),
            bios_sfix: Vec::new(),
            fix_bank_type: crate::graphics::video::FixBankType::Std,
            c_roms: Vec::new(),
            sprite_gfx_decoded: Vec::new(),
            lo_rom: Vec::new(),
            dbg_frame: 0,
            dbg_sysio_hits: 0,
            dbg_irq1_taken: 0,
            z80_cycles_owed: 0,
            audio_cycles_owed: 0,
            audio_buffer: Vec::new(),
        }
    }

    /// Load ROMs into the bus and reset both CPUs.
    pub fn load(&mut self, romset: RomSet) -> Result<()> {
        if !romset.bios.is_empty() {
            self.bus.load_system_rom(romset.bios);
        }
        if !romset.cart.p_rom.is_empty() {
            self.bus.load_p_rom(romset.cart.p_rom);
        }
        // Stash fix-tile S-ROM and sprite C-ROMs for the video renderer.
        self.s_rom = romset.cart.s_rom;
        self.bios_sfix = romset.bios_sfix;
        self.c_roms = romset.cart.c_roms;
        // Auto-detect NEO-CMC fix-layer banking by cart name. This is the
        // same set of carts MAME associates with FIX_BANKTYPE_GAROU /
        // FIX_BANKTYPE_KOF2000 via `get_fixed_bank_type()`.
        self.fix_bank_type = detect_fix_bank_type(&romset.cart.name);
        log::info!(
            "video sources installed: cart='{}' s_rom={} bytes, bios_sfix={} bytes, fix_bank={:?}",
            romset.cart.name,
            self.s_rom.len(), self.bios_sfix.len(), self.fix_bank_type,
        );
        self.sprite_gfx_decoded = crate::graphics::video::decode_sprite_gfx(&self.c_roms);
        log::info!(
            "sprite gfx pre-decoded: {:.1} MiB ({} tiles)",
            self.sprite_gfx_decoded.len() as f64 / (1024.0 * 1024.0),
            self.sprite_gfx_decoded.len() / 256,
        );
        self.lo_rom = romset.lo_rom;

        // ===== Audio subsystem hook-up =====
        if !romset.cart.m_rom.is_empty() {
            self.audio.install_m1(romset.cart.m_rom);
        }
        if !romset.cart.v_roms.is_empty() {
            self.audio.ym.install_v_roms(&romset.cart.v_roms);
        }
        Ok(())
    }

    /// Render the current LSPC state into a fresh 320×224 RGBA framebuffer.
    ///
    /// Honours the full HC259 system latch:
    ///   * Q0 → `screen_shadow` (palette LUT cols 2/3 — KOF combo hits,
    ///     pause menus, fades).
    ///   * Q5 → `use_cart_audio` (also drives fix-source mux: 0 = BIOS
    ///     SFIX, 1 = cart S-ROM).
    ///   * Q7 → `palette_bank`.
    #[must_use]
    pub fn render_frame_pixels(&self) -> crate::graphics::video::Frame {
        let palette_bank = (self.bus.systemlatch >> 7) & 1;
        let screen_shadow = (self.bus.systemlatch & 0x01) != 0;
        let use_cart_fix = (self.bus.systemlatch & 0x20) != 0; // Q5
        // When Q5=0 (BIOS fix active) and we actually have an SFIX, pass
        // it as the override. Otherwise the cart S-ROM is the source.
        let bios_sfix: Option<&[u8]> = if !use_cart_fix && !self.bios_sfix.is_empty() {
            Some(self.bios_sfix.as_slice())
        } else {
            None
        };
        crate::graphics::video::render_frame_full(
            &self.bus.lspc,
            self.bus.palette_ram.as_ref(),
            &self.s_rom,
            &self.c_roms,
            &self.sprite_gfx_decoded,
            &self.lo_rom,
            palette_bank,
            screen_shadow,
            bios_sfix,
            self.fix_bank_type,
        )
    }

    /// Reset both CPUs (re-reads the vector table at $0).
    pub fn reset(&mut self) {
        self.m68k.reset(&mut self.bus);
        self.z80.reset();
        self.master_cycles = 0;
        self.instructions = 0;
        self.bus.lspc.irq3_pending = true;
        self.bus.systype = match self.config.hardware {
            Hardware::Aes => 0x00,
            Hardware::Mvs => 0x80,
        };
        // Default for cartridge games: SM1 OFF (cart M1 selected).
        self.audio.use_cart_audio = true;
        self.z80_cycles_owed = 0;
        self.audio_cycles_owed = 0;
    }

    /// Run one 68000 instruction. Returns the cycles consumed.
    pub fn step(&mut self) -> u32 {
        let pre_pc = self.m68k.pc;

        if pre_pc == 0x00000122 {
            let req = self.bus.read8(0x10FDAE);
            log::info!("USER_ENTRY @$0122 BIOS_USER_REQUEST=${:02X} inst={} sf=${:02X}",
                req, self.instructions, self.bus.read8(0x10FDB4));
        }
        if pre_pc == 0x00000128 {
            let sf = self.bus.read8(0x10FDB4);
            log::info!("PLAYER_START @$0128 BIOS_START_FLAG=${:02X} inst={}",
                sf, self.instructions);
        }
        if pre_pc == 0x0000012E {
            log::info!("DEMO_END @$012E inst={}", self.instructions);
        }
        // DEBUG: track BIOS SYSTEM_IO entry (coin/start poll). MAME doc:
        // SYSTEM_IO lives at $C0044A and reads coin+start inputs each VBlank.
        if pre_pc == 0x00C0044A {
            self.dbg_sysio_hits = self.dbg_sysio_hits.wrapping_add(1);
            if self.dbg_sysio_hits <= 5 || self.dbg_sysio_hits % 200 == 0 {
                log::info!("SYSTEM_IO @$C0044A hit #{} inst={} coin_inputs=${:02X} start_select=${:02X}",
                    self.dbg_sysio_hits, self.instructions,
                    self.bus.coin_inputs, self.bus.start_select);
            }
        }

        let cycles = self.m68k.step(&mut self.bus);
        self.instructions += 1;
        self.master_cycles = self.master_cycles.wrapping_add(u64::from(cycles));

        if self.config.trace_cpu {
            log::trace!(
                "[{:>10}] ipc=${:08X} ir=${:04X} -> PC=${:08X} SR=${:04X} D0=${:08X} A6=${:08X} A7=${:08X} cyc={}",
                self.instructions, pre_pc, self.m68k.ir, self.m68k.pc,
                self.m68k.sr.0, self.m68k.d[0], self.m68k.a[6], self.m68k.a[7], cycles,
            );
        }

        self.bus.upd4990a.tick(cycles);

        // ============ Audio CPU (Z80) lockstep =============
        // Z80 runs at 1/3 of 68K speed. Accumulate budget and step the Z80
        // whenever we have at least 4 cycles owed (Z80 instructions are
        // 4-23 cycles each — keep slack to avoid runaway).
        self.z80_cycles_owed += cycles as i64 * Z80_CYCLES_NUM as i64;
        let mut z80_budget = self.z80_cycles_owed / Z80_CYCLES_DEN as i64;
        self.z80_cycles_owed -= z80_budget * Z80_CYCLES_DEN as i64;

        // Sync soundlatch pending → Z80 NMI.
        if self.bus.sound_latch_pending {
            self.audio.soundlatch = self.bus.sound_latch;
            self.audio.soundlatch_pending = true;
            log::trace!(
                "forward sound cmd ${:02X} to Z80 (nmi_enable={} asserted={})",
                self.audio.soundlatch, self.audio.nmi_enable, self.audio.nmi_asserted
            );
            self.bus.sound_latch_pending = false;
            if self.audio.nmi_enable && !self.audio.nmi_asserted {
                self.z80.request_nmi();
                self.audio.nmi_asserted = true;
            }
        }
        // If the 68K wrote a command while NMI was disabled, the Z80 should get
        // exactly one NMI edge once the driver re-enables NMI.
        if self.audio.soundlatch_pending && self.audio.nmi_enable && !self.audio.nmi_asserted {
            self.z80.request_nmi();
            self.audio.nmi_asserted = true;
        }

        // Sync soundlatch2 (Z80 → 68K reply).
        self.bus.sound_reply = self.audio.soundlatch2;

        while z80_budget > 0 {
            let mut bus_ref = AudioBusRef { bus: &mut self.audio };
            let used = self.z80.step(&mut bus_ref) as i64;
            self.audio.ym.elapse_z80_cycles(used.max(1) as u32);
            z80_budget -= used.max(1);
            if used == 0 { break; }
        }
        // Roll any unused Z80 budget back into the accumulator (we owe the
        // Z80 negative — i.e., it ran ahead — and will subtract it next call).
        self.z80_cycles_owed += z80_budget * Z80_CYCLES_DEN as i64;

        // ============ Audio sample capture & timer dispatch =============
        // Even when audio capture is disabled we must tick the YM2610 at the
        // chip's native rate so Timer A/B fire and drive the Z80 sound IRQ.
        self.audio_cycles_owed += cycles as u64;
        while self.audio_cycles_owed >= M68K_CYCLES_PER_AUDIO_SAMPLE {
            self.audio_cycles_owed -= M68K_CYCLES_PER_AUDIO_SAMPLE;
            let (l, r) = self.audio.ym.step_one_sample();
            if self.config.audio_sample_rate.is_some() {
                self.audio_buffer.push(l);
                self.audio_buffer.push(r);
            }
            // Forward YM2610 IRQ → Z80 INT (IM1 → RST $38).
            if self.audio.ym.irq_out {
                // Edge-triggered: latch the line until the Z80 acknowledges.
                self.z80.request_irq(0xFF);
                // Real chip keeps IRQ asserted until status bits cleared via $27.
                self.audio.ym.irq_out = false;
            }
        }

        // ============ LSPC + IRQ dispatch =============
        let _ = self.bus.lspc.tick(cycles);
        let cur_mask = self.m68k.sr.interrupt_mask();
        let req_level = if self.bus.lspc.irq3_pending {
            3
        } else if self.bus.lspc.display_position_pending {
            2
        } else if self.bus.lspc.vblank_pending {
            1
        } else {
            0
        };
        if req_level > 0 && req_level > cur_mask {
            self.m68k.pending_irq = req_level;
            if req_level == 1 { self.dbg_irq1_taken = self.dbg_irq1_taken.wrapping_add(1); }
        }

        if self.bus.tick_watchdog(cycles) {
            log::info!("WATCHDOG expired at inst={} cyc={} — hard reset",
                self.instructions, self.master_cycles);
            self.m68k.reset(&mut self.bus);
            self.bus.watchdog_expired = false;
            self.bus.kick_watchdog();
        }
        cycles
    }

    /// Run for approximately one video frame (≈ 200 000 68k cycles).
    pub fn run_frame(&mut self) {
        let mut budget: u64 = M68K_CYCLES_PER_FRAME;
        while budget > 0 {
            let used = u64::from(self.step());
            budget = budget.saturating_sub(used);
        }
        self.dbg_frame = self.dbg_frame.wrapping_add(1);
        let f = self.dbg_frame;
        if f % 100 == 0 {
            let statcurnt = self.bus.read8(0x10FDAC);
            let statchange = self.bus.read8(0x10FDAD);
            let user_req = self.bus.read8(0x10FDAE);
            let user_mode = self.bus.read8(0x10FDAF);
            let start_flag = self.bus.read8(0x10FDB4);
            let player_mod1 = self.bus.read8(0x10FDB6);
            log::info!("BIOS_STATE f={} STATCURNT=${:02X} STATCHANGE=${:02X} USER_REQ=${:02X} USER_MODE=${:02X} START_FLAG=${:02X} PMOD1=${:02X} start_select=${:02X} sysio_hits={} irq1={}",
                f, statcurnt, statchange, user_req, user_mode, start_flag, player_mod1, self.bus.start_select,
                self.dbg_sysio_hits, self.dbg_irq1_taken);
        }
    }

    /// Write the captured audio buffer to a 16-bit little-endian PCM WAV.
    pub fn write_wav(&self, path: &std::path::Path) -> Result<()> {
        if self.audio_buffer.is_empty() {
            anyhow::bail!("audio_buffer is empty — was audio_sample_rate configured?");
        }
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        let num_samples = self.audio_buffer.len() as u32;       // total samples (L+R)
        let bytes = num_samples * 2;                            // 16-bit
        let chunk_size = 36u32 + bytes;
        let byte_rate = YM_OUTPUT_HZ as u32 * 2 * 2;            // SR * channels * bytes_per_sample
        // RIFF header
        f.write_all(b"RIFF")?;
        f.write_all(&chunk_size.to_le_bytes())?;
        f.write_all(b"WAVE")?;
        // fmt chunk
        f.write_all(b"fmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?;                      // PCM
        f.write_all(&2u16.to_le_bytes())?;                      // stereo
        f.write_all(&(YM_OUTPUT_HZ as u32).to_le_bytes())?;
        f.write_all(&byte_rate.to_le_bytes())?;
        f.write_all(&4u16.to_le_bytes())?;                      // block align
        f.write_all(&16u16.to_le_bytes())?;                     // bits per sample
        // data chunk
        f.write_all(b"data")?;
        f.write_all(&bytes.to_le_bytes())?;
        // Stream samples as little-endian bytes (avoids unsafe).
        let mut buf = Vec::with_capacity(self.audio_buffer.len() * 2);
        for s in &self.audio_buffer { buf.extend_from_slice(&s.to_le_bytes()); }
        f.write_all(&buf)?;
        log::info!("WAV written: {} ({} samples = {:.1}s @ {} Hz stereo)",
            path.display(),
            self.audio_buffer.len() / 2,
            (self.audio_buffer.len() / 2) as f64 / YM_OUTPUT_HZ as f64,
            YM_OUTPUT_HZ,
        );
        Ok(())
    }
}

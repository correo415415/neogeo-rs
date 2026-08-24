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

/// Exact 68K cycles per video frame.
///
/// MAME timing: pixel clock = MASTER/4 = 6 MHz, HTOTAL = 384, VTOTAL = 264.
/// Frame = 384*264 px / 6 MHz = 16.896 ms → 12 MHz × 0.016896 = **202 752**
/// 68K cycles (59.1856 Hz — the real cabinet rate, not 60 Hz).
const M68K_CYCLES_PER_FRAME: u64 = 202_752;
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
    // FIX_BANKTYPE_GAROU (`get_fixed_bank_type() == 1`): Garou +
    // Metal Slug 3/4 family + mslug5 (MAME `neogeo_pvc_mslug5_cart_device`).
    const GAROU_FAMILY: &[&str] = &[
        "garou", "garouh", "garoubl",
        "mslug3", "mslug3a", "mslug3h", "mslug3b6",
        "mslug4", "mslug4h", "ms4plus",
        "mslug5", "mslug5h",
    ];
    if GAROU_FAMILY.iter().any(|g| n == *g) {
        return crate::graphics::video::FixBankType::Garou;
    }
    // FIX_BANKTYPE_KOF2000 (`get_fixed_bank_type() == 2`): KOF2000 plus the
    // PVC carts svc/kof2003/kof2003h (see MAME `bus/neogeo/pvc.h`).
    const KOF2K_FAMILY: &[&str] = &[
        "kof2000", "kof2000n",
        "svc", "kof2003", "kof2003h",
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
    /// Nombre del set MAME cargado (p.ej. "mslugx"). Identidad usada por la
    /// guardia de savestates: un estado solo puede cargarse sobre el mismo
    /// juego. Vacío hasta que `load()` instala un cartucho.
    pub game_name: String,
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

    /// Raster (per-scanline) accumulated frame buffer. Each visible line is
    /// rendered incrementally in `step()` when the LSPC crosses it, so
    /// mid-frame VRAM rewrites (IRQ2 raster effects like the VAPOROUS demo
    /// water ripple) appear on exactly the lines they affect.
    raster_frame: Vec<u32>,
    /// Next output row (0..=223) not yet rendered into `raster_frame`
    /// for the current frame. Rows are rendered lazily, in batches, when
    /// an event is about to change video state (IRQ2 raise) or the frame
    /// ends (VBLANK start) — MAME's `update_partial` model.
    raster_next_row: u16,
    /// LSPC scanline observed after the previous `lspc.tick` — used to
    /// detect the VBLANK-start (line 224) crossing.
    raster_prev_scanline: u16,
    /// Total visible lines rendered into `raster_frame` since power-on.
    /// While < SCREEN_H the buffer is still partially cold and
    /// `render_frame_pixels` falls back to the one-shot full render.
    raster_lines_rendered: u64,
    /// Snapshot of `raster_frame` taken at each VBLANK start (scanline 224
    /// crossing). This is what `render_frame_pixels` returns: a *complete*,
    /// tear-free frame. Returning the live `raster_frame` directly caused
    /// visible tearing near the bottom because `run_frame()`'s cycle budget
    /// expires mid-visible-area, mixing lines from two frames.
    raster_presented: Vec<u32>,
    /// Number of VBLANK snapshots taken. 0 = no complete frame yet →
    /// fall back to the one-shot full render.
    raster_snapshots: u64,
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
            game_name: String::new(),
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
            raster_frame: vec![
                0u32;
                crate::graphics::video::SCREEN_W * crate::graphics::video::SCREEN_H
            ],
            raster_next_row: 0,
            raster_prev_scanline: 0,
            raster_lines_rendered: 0,
            raster_presented: vec![
                0u32;
                crate::graphics::video::SCREEN_W * crate::graphics::video::SCREEN_H
            ],
            raster_snapshots: 0,
        }
    }

    /// Load ROMs into the bus and reset both CPUs.
    pub fn load(&mut self, mut romset: RomSet) -> Result<()> {
        self.game_name = romset.cart.name.clone();
        if !romset.bios.is_empty() {
            self.bus.load_system_rom(romset.bios);
        }
        // ==== Cartridge protection (MAME set_slot_idx counterpart) ====
        // Detect by set name, assemble the P region layout the decrypts
        // expect, load, then run the 68K-side decrypts on the swapped data.
        let mut prot = crate::memory::prot::detect_protection(&romset.cart.name);
        let mut p_data = romset.cart.p_rom;
        if let crate::memory::prot::CartProt::Sma(_) = &prot {
            // SMA carts use MAME's region layout:
            //   $000000-$0BFFFF  fixed part (produced by the decrypt)
            //   $0C0000-$0FFFFF  SMA chip ROM (`*-sma` file, 0x40000)
            //   $100000-$8FFFFF  encrypted banked P data (p1/p2)
            // Our loader concatenates plain P files, so rebuild the region
            // here from the raw (still byte-swapped) parts; `load_p_rom`
            // swaps the whole region uniformly afterwards.
            let mut region = vec![0u8; 0x900000];
            let sma = &romset.cart.sma_rom;
            if !sma.is_empty() {
                let n = sma.len().min(0x40000);
                region[0xC0000..0xC0000 + n].copy_from_slice(&sma[..n]);
            } else {
                log::warn!(
                    "SMA cart '{}' without SMA ROM — handshake code will be missing",
                    romset.cart.name
                );
            }
            let n = p_data.len().min(0x800000);
            region[0x100000..0x100000 + n].copy_from_slice(&p_data[..n]);
            p_data = region;
        }
        if let crate::memory::prot::CartProt::Pvc(p) = &prot {
            // PVC carts ship two 4MiB P-ROMs loaded with
            // ROM_LOAD32_WORD_SWAP: p1 words at region offset 4k, p2 words
            // at 4k+2. Our loader concatenated them (sorted by name), so
            // rebuild the interleaved layout from the two halves while the
            // data is still in on-disk byte order — `load_p_rom`'s uniform
            // per-word swap then matches MAME's logical BE region.
            // kof2003(h) additionally has a plain 1MiB p3 at $800000
            // (ROM_LOAD16_WORD_SWAP), which stays appended as-is.
            let region_size = match p.game {
                crate::memory::pvc::PvcGame::Mslug5 | crate::memory::pvc::PvcGame::Svc => {
                    0x800000
                }
                _ => 0x900000,
            };
            if p_data.len() >= 0x800000 {
                let mut region = vec![0u8; region_size];
                for k in 0..0x200000 {
                    region[4 * k..4 * k + 2]
                        .copy_from_slice(&p_data[2 * k..2 * k + 2]);
                    region[4 * k + 2..4 * k + 4]
                        .copy_from_slice(&p_data[0x400000 + 2 * k..0x400000 + 2 * k + 2]);
                }
                if region_size == 0x900000 && p_data.len() >= 0x900000 {
                    region[0x800000..0x900000]
                        .copy_from_slice(&p_data[0x800000..0x900000]);
                }
                log::info!(
                    "PVC cart '{}': assembled {:#x} P region (32-bit p1/p2 interleave)",
                    romset.cart.name, region_size
                );
                p_data = region;
            } else {
                log::warn!(
                    "PVC cart '{}' P data too small ({:#x}) — expected two 4MiB ROMs; skipping interleave",
                    romset.cart.name, p_data.len()
                );
            }
        }
        if !p_data.is_empty() {
            self.bus.load_p_rom(p_data);
        }
        match &mut prot {
            crate::memory::prot::CartProt::Kof98(p) => {
                if self.bus.p_rom.len() >= 0x600000 {
                    log::info!("kof98: unscrambling 242-P1 program ROM");
                    p.decrypt_68k(&mut self.bus.p_rom);
                } else {
                    log::warn!(
                        "kof98 P region too small ({:#X}) — skipping decrypt",
                        self.bus.p_rom.len()
                    );
                }
            }
            crate::memory::prot::CartProt::Sma(p) => {
                log::info!("SMA cart '{}': decrypting 68K ROM", romset.cart.name);
                crate::memory::prot::sma_decrypt(p.game, &mut self.bus.p_rom);
            }
            crate::memory::prot::CartProt::Pvc(p) => {
                let need = match p.game {
                    crate::memory::pvc::PvcGame::Mslug5
                    | crate::memory::pvc::PvcGame::Svc => 0x800000,
                    _ => 0x900000,
                };
                if self.bus.p_rom.len() >= need {
                    log::info!(
                        "PVC cart '{}': descrambling 68K ROM ({:?})",
                        romset.cart.name, p.game
                    );
                    crate::memory::pvc::pvc_decrypt_68k(p.game, &mut self.bus.p_rom);
                } else {
                    log::warn!(
                        "PVC P region too small ({:#X} < {need:#X}) — skipping decrypt",
                        self.bus.p_rom.len()
                    );
                }
            }
            _ => {}
        }
        if !matches!(prot, crate::memory::prot::CartProt::None) {
            log::info!("protection device active: {:?}", std::mem::discriminant(&prot));
        }
        self.bus.prot = prot;
        // Debug aid: dump the fully decrypted P region for offline
        // disassembly when NEOGEO_DUMP_PROM is set to a path.
        if let Ok(path) = std::env::var("NEOGEO_DUMP_PROM") {
            if !path.is_empty() {
                // Undo the CPU-side pairwise swap so the file matches
                // MAME's raw region byte order (big-endian words).
                let mut out = self.bus.p_rom.clone();
                for chunk in out.chunks_exact_mut(2) {
                    chunk.swap(0, 1);
                }
                match std::fs::write(&path, &out) {
                    Ok(()) => log::info!("dumped decrypted P region ({:#x} bytes) to {path}", out.len()),
                    Err(e) => log::warn!("P region dump to {path} failed: {e}"),
                }
            }
        }
        // Stash fix-tile S-ROM and sprite C-ROMs for the video renderer.
        self.s_rom = romset.cart.s_rom;
        self.bios_sfix = romset.bios_sfix;
        self.c_roms = romset.cart.c_roms;
        // ==== NEO-CMC42/CMC50 graphics (and M1) decryption ====
        // Encrypted carts store the sprite data scrambled and carve the
        // S (fix) tiles out of the end of the C data; CMC50 carts also
        // encrypt the Z80 M1 program. All of this must happen *before*
        // decode_sprite_gfx / install_m1 consume the data.
        if let Some(cmc) = crate::memory::cmc::detect_cmc(&romset.cart.name) {
            log::info!(
                "NEO-CMC cart '{}': {:?} extra_xor={:#04x} sfix={:#x}",
                romset.cart.name, cmc.variant, cmc.extra_xor, cmc.sfix_bytes,
            );
            // Build the interleaved sprite region MAME's tables expect
            // (c1 even bytes / c2 odd bytes, then c3/c4, ...).
            let mut region = crate::memory::cmc::interleave_c_roms(&self.c_roms);
            if !region.is_empty() {
                if !(region.len() / 4).is_power_of_two()
                    && region.len() != 0x300_0000
                    && region.len() != 0x600_0000
                {
                    log::warn!(
                        "CMC sprite region size {:#x} is not a power of two — address descramble may clamp incorrectly",
                        region.len()
                    );
                }
                cmc.gfx_decrypt(&mut region);
                // S data lives at the end of the decrypted C data.
                self.s_rom = cmc.sfix_extract(&region);
                log::info!("CMC: extracted {:#x} bytes of fix tiles from C data", self.s_rom.len());
                let (even, odd) = crate::memory::cmc::deinterleave_region(&region);
                self.c_roms = vec![even, odd];
            } else {
                log::warn!("CMC cart without paired C-ROM data — skipping gfx decrypt");
            }
            if cmc.variant == crate::memory::cmc::CmcVariant::Cmc50
                && !romset.cart.m_rom.is_empty()
            {
                romset.cart.m_rom =
                    crate::memory::cmc::cmc50_m1_decrypt(&romset.cart.m_rom);
            }
        }
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
        // SM1 (BIOS Z80 program) is a *separate* bank-mux entry, selected
        // at runtime by HC259 Q5 = 0 — install it alongside the cart M1.
        if !romset.sm1.is_empty() {
            self.audio.install_sm1(romset.sm1);
        }
        // ==== NEO-PCM2 V-ROM decryption ====
        // Must run on the concatenated ADPCM region before the YM2610
        // splits it into A/B blobs (MAME runs it on the whole ymsnd region).
        if let Some(mode) = crate::memory::pcm2::detect_pcm2(&romset.cart.name) {
            let mut blob: Vec<u8> = Vec::with_capacity(
                romset.cart.v_roms.iter().map(|(_, d)| d.len()).sum(),
            );
            for (_, d) in &romset.cart.v_roms {
                blob.extend_from_slice(d);
            }
            log::info!(
                "NEO-PCM2 cart '{}': {:?} on {:#x}-byte V region",
                romset.cart.name, mode, blob.len()
            );
            match mode {
                crate::memory::pcm2::Pcm2Mode::Decrypt(value) => {
                    crate::memory::pcm2::pcm2_decrypt(&mut blob, value);
                }
                crate::memory::pcm2::Pcm2Mode::Swap(value) => {
                    if blob.len() < 0x1000000 {
                        blob.resize(0x1000000, 0);
                    }
                    crate::memory::pcm2::pcm2_swap(&mut blob, value);
                }
            }
            // Single shared blob: the YM2610 aliases it into both the
            // ADPCM-A and ADPCM-B address spaces, matching MAME's fallback.
            romset.cart.v_roms = vec![("pcm2.v".to_string(), blob)];
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
        // Prefer the VBLANK-latched raster snapshot once at least one
        // complete frame has been accumulated. It carries the per-scanline
        // VRAM state (IRQ2 raster effects survive) AND is coherent: it was
        // captured when all 224 lines belonged to the same frame, unlike
        // the live `raster_frame` which is mid-update whenever the frame
        // budget expires inside the visible area (caused bottom tearing).
        if self.raster_snapshots > 0 {
            return self.raster_presented.clone();
        }
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

    /// Lazily render all not-yet-rendered rows of the current frame up to
    /// and including `up_to_row` (clamped to the visible area) with the
    /// CURRENT VRAM/palette/latch state.
    ///
    /// This is MAME's `update_partial` model: instead of rendering each
    /// row eagerly at a fixed line-boundary instant, rows accumulate and
    /// are flushed right before a video-state-changing event:
    ///
    ///   * an IRQ2 (display position) raise — the raster handler is about
    ///     to rewrite VRAM, so everything the beam has passed must be
    ///     drawn with the *pre-handler* state first;
    ///   * VBLANK start — the frame is complete; flush the tail and latch
    ///     the presentation snapshot.
    ///
    /// Why not eager per-line rendering? It raced with IRQ2 handlers that
    /// span a line boundary. VAPOROUS' cube scene fires an
    /// AUTOLOAD_REPEAT IRQ2 on every line 10..=121; on most lines the
    /// handler is taken mid-line and RTEs within the same line, but when
    /// the take happened late (line 121: taken at cycle 688/768, RTE 392
    /// cycles into the next line) the eager render of the next row caught
    /// the 68k mid-handler, before its VRAM writes completed -> one black
    /// line at the split point (row 122), absent on hardware. Deferring
    /// each row by one line just moved the artifact one row up. With lazy
    /// flushing the race is impossible by construction: rows are only
    /// sampled at raise instants (pre-handler) or at VBLANK, and a
    /// handler has until the *next* raise — a full line — to finish.
    ///
    /// In the LSPC timing domain (lspc.rs), scanlines 0..=223 are the
    /// visible area and vblank starts at 224; output row == timing line.
    /// The +0x10 sprite-Y bias belongs to the sprite COORDINATE domain
    /// and is applied inside render_sprite_scanline, NOT here (see PR#11).
    fn raster_catch_up(&mut self, up_to_row: u16) {
        let end = up_to_row.min(223);
        if self.raster_next_row > end {
            return;
        }
        let palette_bank = (self.bus.systemlatch >> 7) & 1;
        let screen_shadow = (self.bus.systemlatch & 0x01) != 0;
        let use_cart_fix = (self.bus.systemlatch & 0x20) != 0; // Q5
        let bios_sfix: Option<&[u8]> = if !use_cart_fix && !self.bios_sfix.is_empty() {
            Some(self.bios_sfix.as_slice())
        } else {
            None
        };
        for row in self.raster_next_row..=end {
            crate::graphics::video::render_scanline(
                &self.bus.lspc,
                self.bus.palette_ram.as_ref(),
                &self.s_rom,
                &self.c_roms,
                &self.sprite_gfx_decoded,
                &self.lo_rom,
                &mut self.raster_frame,
                row as usize,
                palette_bank,
                screen_shadow,
                bios_sfix,
                self.fix_bank_type,
            );
            self.raster_lines_rendered = self.raster_lines_rendered.wrapping_add(1);
        }
        self.raster_next_row = end + 1;
    }

    /// Raster bookkeeping after an `lspc.tick`: flush passed rows before a
    /// freshly raised IRQ2 handler can mutate video state, and finish +
    /// latch the frame at the VBLANK-start (line 224) crossing.
    fn raster_after_tick(&mut self, dp_was_pending: bool) {
        // IRQ2 raised by this tick: snapshot every row the beam has
        // passed with the current (pre-handler) state.
        if !dp_was_pending
            && self.bus.lspc.display_position_pending
            && self.bus.lspc.scanline < 224
        {
            self.raster_catch_up(self.bus.lspc.scanline);
        }
        let cur = self.bus.lspc.scanline;
        let prev = self.raster_prev_scanline;
        if cur == prev {
            return;
        }
        self.raster_prev_scanline = cur;
        // Did this tick cross the VBLANK start line (224)? (wraps at 264)
        let delta = (u32::from(cur) + 264 - u32::from(prev)) % 264;
        let off_224 = (224 + 264 - u32::from(prev)) % 264;
        if off_224 >= 1 && off_224 <= delta {
            // Frame complete: flush the tail and latch a coherent copy
            // for presentation (VBLANK-start snapshot, see PR#11).
            self.raster_catch_up(223);
            if self.raster_lines_rendered >= crate::graphics::video::SCREEN_H as u64 {
                self.raster_presented.copy_from_slice(&self.raster_frame);
                self.raster_snapshots = self.raster_snapshots.wrapping_add(1);
            }
            // Rows of the NEXT frame start accumulating from 0.
            self.raster_next_row = 0;
        }
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
        // DEBUG(kof2003): scanline-table builder at $14634 zero-fills with counts
        // derived from these work-RAM vars; a negative count makes the dbra loop
        // wipe 64K words across the stack. Dump the inputs at the fill loops.
        if pre_pc == 0x000149A4 || pre_pc == 0x000149C0 || pre_pc == 0x000149E0 {
            log::debug!(
                "KOF2K3TBL @${:06X} inst={} $107E12=${:04X} $107E16=${:04X} $107E18=${:08X} $107E1C=${:08X} $107E20=${:08X} $1087DE=${:04X} $1087E0=${:04X} $1087E2=${:04X} A2=${:08X} A7=${:08X}",
                pre_pc, self.instructions,
                self.bus.read16(0x107E12), self.bus.read16(0x107E16),
                self.bus.read32(0x107E18), self.bus.read32(0x107E1C),
                self.bus.read32(0x107E20),
                self.bus.read16(0x1087DE), self.bus.read16(0x1087E0),
                self.bus.read16(0x1087E2),
                self.m68k.a[2], self.m68k.a[7],
            );
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

        // Sync HC259 Q5 (use_cart_audio) → Z80 main-bank mux. MAME:
        // `set_use_cart_audio(state)` → `m_bank_audio_main->set_entry(state)`.
        // Only meaningful when an SM1 is resident; otherwise the AudioBus
        // falls back to M1 regardless.
        let q5 = (self.bus.systemlatch & 0x20) != 0;
        if q5 != self.audio.use_cart_audio {
            log::debug!("use_cart_audio {} -> {} (HC259 Q5)", self.audio.use_cart_audio, q5);
            self.audio.use_cart_audio = q5;
        }

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
        let dp_was_pending = self.bus.lspc.display_position_pending;
        let _ = self.bus.lspc.tick(cycles);
        self.raster_after_tick(dp_was_pending);
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

// ============================================================================
// Savestates
// ============================================================================
//
// Payload de `System` (tras la cabecera NGSS): CPUs + buses + contadores de
// planificación. Quedan fuera:
//   * ROMs (`s_rom`, `bios_sfix`, `c_roms`, `sprite_gfx_decoded`, `lo_rom`,
//     y dentro de bus/audio: `system_rom`, `p_rom`, `m1_rom`, `sm1_rom`,
//     V-ROMs) — se reponen de la instancia viva.
//   * Contadores dbg_* y `audio_buffer` — diagnósticos / stream transitorio.
//   * Buffers raster — se invalidan al cargar; el renderer usa el fallback
//     de render completo hasta el siguiente snapshot de VBLANK.

impl System {
    /// Serializa el estado completo de emulación a un buffer binario
    /// autocontenido (cabecera NGSS + versión + juego + payload).
    #[must_use]
    pub fn save_state(&self) -> Vec<u8> {
        use crate::state::{STATE_MAGIC, STATE_VERSION};
        let mut out = Vec::with_capacity(0x60000);
        out.extend_from_slice(&STATE_MAGIC);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        let name = self.game_name.as_bytes();
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name);
        self.save_payload(&mut out);
        out
    }

    /// Restaura un estado creado por [`Self::save_state`].
    ///
    /// Valida magic, versión y que el juego coincida con el cargado.
    /// La carga es transaccional: si el payload está corrupto o truncado,
    /// el estado previo se restaura y se devuelve el error.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), crate::state::StateError> {
        use crate::state::{StateError, StateReader, STATE_MAGIC, STATE_VERSION};
        let mut r = StateReader::new(data);
        if r.take(4)? != STATE_MAGIC {
            return Err(StateError::BadMagic);
        }
        let version = r.u16()?;
        if version != STATE_VERSION {
            return Err(StateError::BadVersion(version));
        }
        let name_len = r.u32()? as usize;
        if name_len > 256 {
            return Err(StateError::Corrupt("nombre de juego demasiado largo"));
        }
        let found = String::from_utf8_lossy(r.take(name_len)?).into_owned();
        if found != self.game_name {
            return Err(StateError::GameMismatch {
                expected: self.game_name.clone(),
                found,
            });
        }
        // Transaccional: rescate del estado actual por si el payload falla
        // a medio camino (el volcado es barato: ~220 KiB).
        let mut rescue = Vec::with_capacity(0x60000);
        self.save_payload(&mut rescue);
        match self.load_payload(&mut r) {
            Ok(()) => {
                self.post_load_fixup();
                Ok(())
            }
            Err(e) => {
                let mut rr = StateReader::new(&rescue);
                let _ = self.load_payload(&mut rr);
                self.post_load_fixup();
                Err(e)
            }
        }
    }

    fn save_payload(&self, out: &mut Vec<u8>) {
        use crate::state::StateSer;
        self.m68k.save(out);
        self.z80.save(out);
        self.bus.save(out);
        self.audio.save(out);
        self.master_cycles.save(out);
        self.instructions.save(out);
        self.z80_cycles_owed.save(out);
        self.audio_cycles_owed.save(out);
    }

    fn load_payload(
        &mut self,
        r: &mut crate::state::StateReader<'_>,
    ) -> Result<(), crate::state::StateError> {
        use crate::state::StateSer;
        self.m68k.load(r)?;
        self.z80.load(r)?;
        self.bus.load(r)?;
        self.audio.load(r)?;
        self.master_cycles.load(r)?;
        self.instructions.load(r)?;
        self.z80_cycles_owed.load(r)?;
        self.audio_cycles_owed.load(r)?;
        Ok(())
    }

    /// Invalida estado derivado tras cargar un savestate.
    fn post_load_fixup(&mut self) {
        // El pipeline raster incremental queda desincronizado del nuevo
        // scanline del LSPC: invalidarlo fuerza el fallback de render
        // completo hasta el siguiente snapshot de VBLANK.
        self.raster_next_row = 0;
        self.raster_prev_scanline = self.bus.lspc.scanline;
        self.raster_lines_rendered = 0;
        self.raster_snapshots = 0;
        // Descarta muestras de audio del estado anterior.
        self.audio_buffer.clear();
    }
}

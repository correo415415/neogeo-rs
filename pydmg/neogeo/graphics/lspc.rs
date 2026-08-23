//! LSPC-A2 / NEO-B1 — Line Sprite Processor.
//!
//! This is the Neo Geo's custom Video Display Controller. It owns:
//!   * 64 KiB of VRAM (sprite control SCB1–SCB4, fix-layer tiles)
//!   * 8 KiB of palette RAM (shared with the 68000 — but the 68000 sees it
//!     through the `$400000` aperture; here we only model the VRAM port)
//!   * A vertical-blank/timer interrupt generator
//!
//! For the moment we model the **register interface** so the BIOS can poke
//! the VRAM via `REG_VRAMADDR` / `REG_VRAMRW` / `REG_VRAMMOD`, and we
//! implement a basic VBlank counter so the CPU can take its level-1
//! interrupt. No actual pixel output yet.

// Neo Geo VRAM is 68 KiB (0x8800 words). The top 0x800 words (0x8000..0x87FF)
// host the sprite control blocks (SCB2/3/4 + draw lists). MAME's reference
// allocation is `make_unique<u16[]>(0x8000 + 0x800)`.
const VRAM_WORDS: usize = 0x8800;

#[derive(Debug)]
pub struct Lspc {
    pub vram: Box<[u16; VRAM_WORDS]>,
    pub vram_addr: u16,
    /// Buffered read — MAME's `m_vram_read_buffer`. Updated when the
    /// address is set or after a write completes its auto-increment.
    pub vram_read_buffer: u16,
    pub vram_mod: u16,
    pub lspc_mode: u16,
    pub timer_reload: u32,
    /// Cycle accumulator for the per-scanline timer.
    pub line_cycle_accum: u32,
    /// Internal scanline counter; the Neo Geo renders 264 lines total at 60Hz.
    pub scanline: u16,
    /// True for the time the CPU should read STATUS as "in VBlank".
    pub in_vblank: bool,
    /// Whether a level-1 (VBlank) interrupt is pending. **In MVS, VBL = IRQ1**.
    pub vblank_pending: bool,
    /// Whether a level-2 (display position / scanline) interrupt is pending.
    pub display_position_pending: bool,
    /// Whether a level-3 (cold boot / forced reset) interrupt is pending.
    pub irq3_pending: bool,
    /// IRQ2 / display-position control register, bits 4-7 of REG_LSPCMODE:
    ///   bit 4 = ENABLE          (IRQ2CTRL_ENABLE)
    ///   bit 5 = LOAD_RELATIVE   (reload timer at LSB write)
    ///   bit 6 = AUTOLOAD_VBLANK (reload timer at VBLANK)
    ///   bit 7 = AUTOLOAD_REPEAT (reload timer when it fires)
    pub display_position_interrupt_control: u8,
    /// Pixel-clock countdown counter (in LSPC pixel clocks; one scanline ≈ 384 px).
    pub display_counter: u32,
    /// Auto-animation counter. The LSPC ticks this every 4/8/16 frames
    /// depending on the speed bits in REG_LSPCMODE; sprites with attr bit
    /// 2 or 3 set use this counter to cycle their tile number.
    pub auto_animation_counter: u32,
    /// Internal subdivisor for the animation counter (frames since last bump).
    pub auto_animation_frame_counter: u32,
    /// When true, the animation counter is frozen (REG_LSPCMODE bit 3).
    pub auto_animation_disabled: bool,
}

impl Lspc {
    pub fn new() -> Self {
        Self {
            vram: Box::new([0; VRAM_WORDS]),
            vram_addr: 0,
            vram_read_buffer: 0,
            vram_mod: 0,
            lspc_mode: 0,
            timer_reload: 0,
            line_cycle_accum: 0,
            scanline: 0,
            in_vblank: false,
            vblank_pending: false,
            display_position_pending: false,
            irq3_pending: false,
            display_position_interrupt_control: 0,
            display_counter: 0,
            auto_animation_counter: 0,
            auto_animation_frame_counter: 0,
            auto_animation_disabled: false,
        }
    }

    /// Advance the LSPC by `cycles_68k` 68000 cycles. Generates VBL and
    /// display-position interrupts. Returns true if any IRQ became pending.
    pub fn tick(&mut self, cycles_68k: u32) -> bool {
        // 12 MHz / 60 Hz / 264 lines ≈ 758 cycles per scanline (NTSC).
        const CYC_PER_LINE: u32 = 758;
        // The display position counter ticks at the LSPC pixel clock
        // (~6 MHz, 384 px per line), i.e. roughly half the 68k clock.
        // We approximate by ticking it once per 68k cycle / 2.
        let prev_pos = self.display_counter;
        let dec = cycles_68k / 2;
        if dec > 0 {
            if prev_pos != 0 {
                if prev_pos > dec {
                    self.display_counter -= dec;
                } else {
                    // Timer expired.
                    self.display_counter = 0;
                    if (self.display_position_interrupt_control & (1 << 4)) != 0 {
                        self.display_position_pending = true;
                    }
                    if (self.display_position_interrupt_control & (1 << 7)) != 0 {
                        // AUTOLOAD_REPEAT — reload from timer_reload.
                        self.display_counter = self.timer_reload;
                    }
                }
            }
        }
        self.line_cycle_accum = self.line_cycle_accum.wrapping_add(cycles_68k);
        let mut new_irq = false;
        while self.line_cycle_accum >= CYC_PER_LINE {
            self.line_cycle_accum -= CYC_PER_LINE;
            self.scanline = self.scanline.wrapping_add(1);
            if self.scanline == 224 {
                self.in_vblank = true;
                self.vblank_pending = true;
                new_irq = true;
                log::debug!("LSPC: VBLANK start, vblank_pending=true");
                // AUTOLOAD_VBLANK — reload display counter at VBLANK start.
                if (self.display_position_interrupt_control & (1 << 6)) != 0 {
                    self.display_counter = self.timer_reload;
                }
                // Bump the auto-animation frame counter. Speed selected by
                // REG_LSPCMODE bits 8-15: anim_speed = ((lspc_mode >> 8) & 0xFF) + 1.
                // (Per MAME's `m_auto_animation_speed` handling.)
                if !self.auto_animation_disabled {
                    let speed = ((self.lspc_mode >> 8) & 0xFF) as u32 + 1;
                    self.auto_animation_frame_counter =
                        self.auto_animation_frame_counter.wrapping_add(1);
                    if self.auto_animation_frame_counter >= speed {
                        self.auto_animation_frame_counter = 0;
                        self.auto_animation_counter =
                            self.auto_animation_counter.wrapping_add(1);
                    }
                }
            }
            if self.scanline >= 264 {
                self.scanline = 0;
                self.in_vblank = false;
            }
        }
        new_irq
    }

    /// STATUS_A register read.
    ///
    /// Bit 7 — VBlank (1 = currently in VBlank).
    /// Bit 6 — 4H clock (alternates with horizontal pixel timing).
    /// Bit 5 — 16K signal.
    /// Bit 4 — Auto-animate quarter signal.
    /// Bit 3..0 — Open bus (read as 1).
    ///
    /// The BIOS POST polls bits 6 and 7 to verify the LSPC timing chain is
    /// alive. We synthesise a plausible value from `line_cycle_accum`.
    pub fn status_a(&self) -> u8 {
        let mut v: u8 = 0x0F; // low nibble open bus = 1.
        if self.in_vblank { v |= 0x80; }
        // 4H — toggles every 4 horizontal clocks; we approximate by taking
        // bit 2 of the per-line cycle accumulator (≈ every 4 cycles).
        if (self.line_cycle_accum & 0x04) != 0 { v |= 0x40; }
        // 16K — toggles every 16 horizontal clocks.
        if (self.line_cycle_accum & 0x10) != 0 { v |= 0x20; }
        // Auto-animation tick — derive from scanline.
        if (self.scanline & 0x01) != 0 { v |= 0x10; }
        v
    }

    /// MAME wraps VRAM offsets so that the top page (0x8000..) keeps its
    /// MSB and only wraps the low 11 bits (covering the SCB region);
    /// everything else wraps in the bottom 15 bits.
    fn map_vram_addr(addr: u16) -> usize {
        if addr & 0x8000 != 0 {
            (addr & 0x87FF) as usize
        } else {
            (addr & 0x7FFF) as usize
        }
    }

    /// Advance the VRAM access pointer using modulo, replicating MAME's
    /// `set_videoram_offset` ( top bit preserved, low 15 bits wrap ).
    fn advance_vram_addr(&mut self) {
        let new_low = self.vram_addr.wrapping_add(self.vram_mod) & 0x7FFF;
        self.vram_addr = (self.vram_addr & 0x8000) | new_low;
    }

    pub fn read_register_word(&mut self, port: u16) -> u16 {
        match port & 0xFE {
            // REG_VRAMADDR (read) returns the buffered word; no auto-increment.
            // REG_VRAMRW returns the same buffer.
            0x00 | 0x02 => self.vram_read_buffer,
            0x04 => self.vram_mod,
            // $3C0006 read = video_control:
            //   AAAA AAAA A??? BCCC
            //     A = raster line counter (vpos + 0x100, wrap 0x200)
            //     B = PAL/NTSC flag (LSPC2 only) -> 0 for NTSC
            //     CCC = animation counter low 3 bits
            0x06 => {
                let v_counter = (self.scanline as u32 + 0x100) as u32;
                let v_counter = if v_counter >= 0x200 { v_counter - 264 } else { v_counter };
                ((v_counter as u16) << 7) | (self.auto_animation_counter as u16 & 0x0007)
            }
            _ => 0xFFFF,
        }
    }

    pub fn write_register_word(&mut self, port: u16, value: u16) {
        match port & 0xFE {
            0x00 => {
                // Updating the address pre-fetches the new word into the buffer.
                self.vram_addr = value;
                self.vram_read_buffer = self.vram[Self::map_vram_addr(self.vram_addr)];
            }
            0x02 => {
                self.vram[Self::map_vram_addr(self.vram_addr)] = value;
                // Auto-increment, then refresh the read buffer at the new address.
                self.advance_vram_addr();
                self.vram_read_buffer = self.vram[Self::map_vram_addr(self.vram_addr)];
            }
            0x04 => self.vram_mod = value,
            0x06 => {
                // $3C0006 write = set_video_control:
                //   bits 8-15 = auto_animation_speed
                //   bit  3    = auto_animation_disabled
                //   bits 4-7  = display_position_interrupt_control
                self.lspc_mode = value;
                self.auto_animation_disabled = (value & 0x0008) != 0;
                self.display_position_interrupt_control = ((value >> 4) & 0x0F) as u8;
            }
            0x08 => {
                self.timer_reload = (self.timer_reload & 0x0000_FFFF) | (u32::from(value) << 16);
            }
            0x0A => {
                self.timer_reload = (self.timer_reload & 0xFFFF_0000) | u32::from(value);
                // LOAD_RELATIVE — at LSB write, schedule the timer.
                if (self.display_position_interrupt_control & (1 << 5)) != 0 {
                    self.display_counter = self.timer_reload;
                }
            }
            0x0C => {
                // IRQ ack — write 1 bits to clear pending levels.
                // MAME bit layout: bit 0 = IRQ3 (cold boot), bit 1 = IRQ2 (display position), bit 2 = IRQ1 (VBL).
                if value & 0x01 != 0 { self.irq3_pending = false; }
                if value & 0x02 != 0 { self.display_position_pending = false; }
                if value & 0x04 != 0 { self.vblank_pending = false; }
            }
            _ => {}
        }
    }

    pub fn read_register_byte(&mut self, port: u16) -> u8 {
        let w = self.read_register_word(port & !1);
        if port & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
    }
    pub fn write_register_byte(&mut self, port: u16, value: u8) {
        // Byte writes to LSPC registers are typically rejected by real HW,
        // but we model them as zero-extended word writes for permissiveness.
        let cur = self.read_register_word(port & !1);
        let w = if port & 1 == 0 {
            (u16::from(value) << 8) | (cur & 0x00FF)
        } else {
            (cur & 0xFF00) | u16::from(value)
        };
        self.write_register_word(port & !1, w);
    }
}

impl Default for Lspc {
    fn default() -> Self {
        Self::new()
    }
}

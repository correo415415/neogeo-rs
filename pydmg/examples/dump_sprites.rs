//! Sprite-state debugger for neogeo-rs.
//!
//! Runs the emulator to a target frame, then dumps the resolved sprite
//! anchors (SCB1/2/3/4 + sticky chain). Optional sub-commands list a
//! full per-sprite tilemap, the palette colours used, the Y-zoom table
//! row, and the raw pixel grid of any C-ROM tile.
//!
//! Example for the "white band on the sun" bug:
//!   dump_sprites --cart compare/mslug.zip --frame 1360 \
//!                --ymin 80 --ymax 160 \
//!                --show-tiles-for 361 \
//!                --show-zoomy 95 \
//!                --show-palette 0xAC0 --show-palette 0xAC1 \
//!                --show-tile-gfx 0x3FF

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use pydmg_neogeo::{
    rom::RomSet,
    system::{Hardware, SystemConfig},
    System,
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    bios: Option<PathBuf>,
    #[arg(long)]
    cart: PathBuf,
    #[arg(long, default_value_t = 1360)]
    frame: u32,
    #[arg(long, default_value_t = 80)]
    ymin: i32,
    #[arg(long, default_value_t = 160)]
    ymax: i32,
    #[arg(long, default_value = "mvs")]
    hardware: String,
    #[arg(long)]
    show_tiles_for: Option<u16>,
    #[arg(long, value_parser = parse_u16_hex)]
    show_palette: Vec<u16>,
    #[arg(long)]
    show_zoomy: Option<u8>,
    #[arg(long, value_parser = parse_u32_hex)]
    show_tile_gfx: Option<u32>,
}

fn parse_u16_hex(s: &str) -> Result<u16, String> {
    let t = s.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(t, 16).map_err(|e| e.to_string())
}

fn parse_u32_hex(s: &str) -> Result<u32, String> {
    let t = s.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).map_err(|e| e.to_string())
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();

    let mut romset = RomSet::default();
    if let Some(bios) = &args.bios {
        romset.load_bios(bios)?;
    }
    romset
        .load_cart_any(&args.cart)
        .with_context(|| format!("loading cart {}", args.cart.display()))?;

    let hw = match args.hardware.as_str() {
        "aes" => Hardware::Aes,
        _ => Hardware::Mvs,
    };
    let cfg = SystemConfig { hardware: hw, trace_cpu: false, trace_audio_io: false, audio_sample_rate: None };
    let mut sys = System::new(cfg);
    sys.load(romset)?;
    sys.reset();

    eprintln!("Emulating up to frame {}…", args.frame);
    for _ in 0..args.frame {
        sys.run_frame();
    }
    eprintln!("Done. Dumping sprite state.\n");

    // ---- Anchor resolution pass (mirrors the renderer's pass 1) ----
    let vram = &sys.bus.lspc.vram[..];
    let mut x: i32 = 0;
    let mut y: i32 = 0;
    let mut rows: i32 = 0;
    let mut zoom_y: u8 = 0;
    let mut zoom_x: i32 = 0;

    println!(
        "{:>4} {:>4} {:>4} {:>4} {:>4} {:>3} stk  tile0_lo attr0   pal hf vf aa  notes",
        "#", "x", "y", "rows", "zy", "zx",
    );

    for sprite in 0..381u16 {
        let scb2 = vram[0x8000 + sprite as usize];
        let scb3 = vram[0x8200 + sprite as usize];
        let scb4 = vram[0x8400 + sprite as usize];
        let sticky = (scb3 & 0x40) != 0;
        if sticky {
            x = (x + zoom_x + 1) & 0x1FF;
            zoom_x = ((scb2 >> 8) & 0x0F) as i32;
        } else {
            y = 0x200 - ((scb3 >> 7) as i32);
            x = (scb4 >> 7) as i32;
            zoom_y = (scb2 & 0xFF) as u8;
            zoom_x = ((scb2 >> 8) & 0x0F) as i32;
            rows = (scb3 & 0x3F) as i32;
        }
        if rows == 0 {
            continue;
        }
        let height = if rows < 0x21 { rows * 0x10 } else { 0x200 };
        let y_bot = y + height;
        if !(y < args.ymax && y_bot > args.ymin) {
            continue;
        }
        let scb1_base = (sprite as usize) * 0x40;
        let tile_lo = vram[scb1_base];
        let attr = vram[scb1_base + 1];
        let pal = (attr >> 8) & 0xFF;
        let hflip = attr & 1;
        let vflip = (attr >> 1) & 1;
        let aa = (attr >> 2) & 0x03;
        let mut notes = String::new();
        if pal == 0 { notes.push_str(" pal=0"); }
        if x >= 0x140 && x <= 0x1F0 { notes.push_str(" OFFSCREEN_X"); }
        let stk = if sticky { '>' } else { ' ' };

        println!(
            "{:>4} {:>4} {:>4} {:>4} {:>4} {:>3}  {}   {:04X}     {:04X}   {:02X}  {:>1}  {:>1}  {:>1} {}",
            sprite, x, y, rows, zoom_y, zoom_x, stk,
            tile_lo, attr, pal, hflip, vflip, aa, notes,
        );
    }

    if let Some(want) = args.show_tiles_for {
        println!("\n--- All 32 SCB1 tiles of sprite {} ---", want);
        println!("{:>4}   {:>6}  {:>6}  pal h v aa  code(20b)", "tile", "lo", "attr");
        let scb1_base = (want as usize) * 0x40;
        for t in 0..32usize {
            let lo = vram[scb1_base + t * 2];
            let at = vram[scb1_base + t * 2 + 1];
            let pal = (at >> 8) & 0xFF;
            let hflip = at & 1;
            let vflip = (at >> 1) & 1;
            let aa = (at >> 2) & 0x3;
            let code = (lo as u32) | (((at as u32) << 12) & 0xF_0000);
            println!(
                "  {:>2}    {:04X}    {:04X}    {:02X}  {:1} {:1} {:>2}   ${:05X}",
                t, lo, at, pal, hflip, vflip, aa, code,
            );
        }
    }

    if let Some(tile_code) = args.show_tile_gfx {
        use pydmg_neogeo::graphics::video::sprite_tile_pixel;
        println!("\n--- Tile $${:05X} pixel grid (4-bit colour indices) ---", tile_code);
        print!("       ");
        for x in 0..16 { print!("{:>2} ", x); }
        println!();
        for y in 0..16u8 {
            print!("  y={:>2}: ", y);
            for x in 0..16u8 {
                let c = sprite_tile_pixel(&sys.c_roms, tile_code, x, y);
                if c == 0 {
                    print!(" . ");
                } else {
                    print!("{:>2} ", c);
                }
            }
            println!();
        }
    }

    if let Some(zy) = args.show_zoomy {
        println!("\n--- L0 Y-zoom row for zoom_y={} ($${:02X}) ---", zy, zy);
        println!("     line:  tile sub_y  (byte)");
        let lo_rom: &[u8] = &sys.lo_rom;
        if lo_rom.len() < 0x10000 {
            println!("  (lo_rom not loaded, len={})", lo_rom.len());
        } else {
            for line in 0..64usize {
                let b = lo_rom[((zy as usize) << 8) | line];
                println!("     {:>3}:   {:>2}    {:>2}    (${:02X})", line, b >> 4, b & 0xF, b);
            }
        }
    }

    for idx in args.show_palette.iter().copied() {
        let bank = (sys.bus.systemlatch >> 7) & 1;
        let bank_off = (bank as usize & 1) * 0x2000;
        let off = bank_off + ((idx as usize) & 0xFFF) * 2;
        let pram = sys.bus.palette_ram.as_ref();
        let word = if off + 1 < pram.len() {
            ((pram[off] as u16) << 8) | (pram[off + 1] as u16)
        } else { 0 };
        let r = (((word & 0x0F00) >> 4) | ((word >> 11) & 8) | ((word >> 13) & 4)) as u8;
        let g = (((word & 0x00F0)     ) | ((word >> 10) & 8) | ((word >> 13) & 4)) as u8;
        let b = (((word & 0x000F) << 4) | ((word >>  9) & 8) | ((word >> 13) & 4)) as u8;
        println!("palette[${:03X}] (bank {}) = $${:04X}  =>  RGB({:3},{:3},{:3})",
                 idx, bank, word, r, g, b);
    }

    Ok(())
}

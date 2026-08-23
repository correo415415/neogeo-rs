//! CLI runner + SDL2 frontend for neogeo-rs.
//!
//! This binary launches the SDL2 GUI by default in a resizable window.
//! Pass `--headless` for the non-windowed runner, `--fullscreen` to
//! start fullscreen, and `--crop` to trim the 8-px overscan-safe column
//! on each side (304×224 view). `--ui` is still accepted as an explicit
//! no-op for backwards compatibility.
//!
//! At runtime the SDL2 frontend supports:
//!   * `F11`     -> toggle fullscreen / windowed in-flight.
//!   * `ESC`     -> quit.
//!   * Title bar -> shows the current view size (320×224 or 304×224, etc.).
//!
//! SDL2 is a *direct, non-optional* dependency — the same arrangement
//! that neogeo-rs v30/v31 used. `cargo build --release` alone is enough
//! to produce a fully-functional GUI binary; no `--features` flag needed.

mod ui;

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use pydmg_neogeo::{
    rom::RomSet,
    system::{Hardware, SystemConfig},
    System,
};
use ui::{run_ui, UiOptions};

#[derive(Parser, Debug)]
#[command(name = "neogeo", version, about = "Neo Geo emulator (WIP)")]
struct Args {
    /// Path to the Neo Geo BIOS file (raw .bin / .rom). Optional if the
    /// cartridge zip bundles a BIOS image, or if --bios-zip is given.
    #[arg(long)]
    bios: Option<PathBuf>,

    /// Path to a MAME-style **parent BIOS zip** (e.g. `neogeo.zip`).
    /// Loaded *before* the cart so its BIOS / `000-lo.lo` / `sfix.sfix` /
    /// `sm1.sm1` are available when the cart is a MAME split set that
    /// does not bundle them (e.g. `mslug2.zip`).
    #[arg(long)]
    bios_zip: Option<PathBuf>,

    /// Specific BIOS filename inside `--bios-zip` (or the cart zip) to
    /// load instead of the auto-picked one. Example:
    /// `--bios-name uni-bios_4_0.rom`.
    #[arg(long)]
    bios_name: Option<String>,

    /// Path to a cartridge: either a directory with split ROM files, a
    /// MAME/FBNeo `.zip` romset, or a single `.bin`/`.rom` program file.
    #[arg(long)]
    cart: Option<PathBuf>,

    /// Run for this many video frames before exiting.
    /// In UI mode: 0 = run indefinitely. If omitted, UI mode also runs indefinitely.
    /// In headless mode: default remains 1 frame.
    #[arg(long, default_value_t = 1)]
    max_frames: u32,

    /// Run for this many CPU instructions before exiting (headless only).
    #[arg(long)]
    max_instructions: Option<u64>,

    /// Print one log line per CPU instruction.
    #[arg(long, default_value_t = false)]
    trace_cpu: bool,

    /// Directory where PNG dumps of the framebuffer will be written.
    /// Created if missing. Implies frame-based loop.
    #[arg(long)]
    dump_frames_dir: Option<PathBuf>,

    /// Dump every Nth frame to PNG (default 1 = every frame).
    #[arg(long, default_value_t = 1)]
    dump_every_frames: u32,

    /// Hardware variant: "mvs" (arcade, default for asia-s3 BIOS) or "aes".
    #[arg(long, default_value = "mvs")]
    hardware: String,

    /// Simulate a P1 START press starting at the given frame.
    #[arg(long, default_value_t = 0)]
    auto_press_start_frame: u32,

    /// How many frames to hold each simulated START press.
    #[arg(long, default_value_t = 4)]
    press_hold_frames: u32,

    /// Period (in frames) between auto-press START events.
    #[arg(long, default_value_t = 40)]
    press_period_frames: u32,

    /// Simulate inserting a coin (Coin-in 1) starting at the given frame.
    #[arg(long, default_value_t = 0)]
    auto_coin_frame: u32,

    /// How many frames to hold the simulated coin press.
    #[arg(long, default_value_t = 8)]
    coin_hold_frames: u32,

    /// Launch SDL2 UI explicitly. Kept for backwards compatibility; the
    /// GUI is the default presentation.
    #[arg(long, default_value_t = false)]
    ui: bool,

    /// Force headless mode (no SDL2 window). Useful for dumping PNG
    /// frames, capturing WAV audio, or running on a headless server.
    #[arg(long, default_value_t = false)]
    headless: bool,

    /// Integer scale factor for the SDL2 window. Mainly useful together
    /// with `--windowed`; fullscreen mode is the default UI presentation.
    #[arg(long, default_value_t = 3)]
    ui_scale: u32,

    /// Start the SDL2 frontend in fullscreen instead of the default
    /// resizable window. Press F11 at runtime to toggle on the fly.
    #[arg(long, default_value_t = false)]
    fullscreen: bool,

    /// (Legacy alias for `--windowed` semantics; default is already
    /// windowed since v32. Kept so old scripts keep working.)
    #[arg(long, default_value_t = false)]
    windowed: bool,

    /// Request present-vsync when supported by the SDL renderer.
    #[arg(long, default_value_t = false)]
    ui_vsync: bool,

    /// Disable the 60 Hz frame cap (run as fast as the CPU allows).
    #[arg(long, default_value_t = false)]
    no_fps_cap: bool,

    /// Backwards-compatible alias for the default view (no crop).
    ///
    /// Older builds defaulted to the 304×224 cropped view; the current UI
    /// defaults to the full 320×224 raster and lets the user opt into the
    /// 304×224 bezel-safe view with `--crop`.
    #[arg(long, default_value_t = false)]
    show_full_raster: bool,

    /// Crop the 8-pixel overscan-safe column on each side of the picture.
    ///
    /// Off (default) -> full 320×224 raster (matches MAME's raw output).
    /// On            -> 304×224 view (matches MAME's `Screen 0 Cropped`
    ///                  and FBNeo's 304-px presentation, the safe-zone
    ///                  used by Metal Slug, KOF, Garou, Samurai Shodown,
    ///                  etc., who paint backdrop pillarboxes on cols 0/39).
    #[arg(long, default_value_t = false)]
    crop: bool,

    /// Output path for a 16-bit little-endian stereo PCM WAV of the
    /// captured YM2610 audio. Recorded at the chip's native ~55,555 Hz.
    /// When set, the system records every sample during execution.
    #[arg(long)]
    audio_out: Option<PathBuf>,

    /// Trace Z80 I/O (very verbose; use `RUST_LOG=neogeo_core::audio=trace`
    /// to actually display the lines).
    #[arg(long, default_value_t = false)]
    trace_audio_io: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let mut sys = load_system(&args)?;
    let launch_ui = !args.headless;

    if launch_ui {
        let ui_max_frames = if args.max_frames == 1 { 0 } else { args.max_frames };
        // `--show-full-raster` (legacy alias) forces crop=off. `--crop`
        // alone toggles the 304×224 view; with no flag we stay on the
        // full 320×224 raster, matching MAME's raw output.
        let crop_on = args.crop && !args.show_full_raster;
        run_ui(
            &mut sys,
            UiOptions {
                scale: args.ui_scale,
                vsync: args.ui_vsync,
                uncapped: args.no_fps_cap,
                max_frames: ui_max_frames,
                auto_coin_frame: args.auto_coin_frame,
                coin_hold_frames: args.coin_hold_frames,
                auto_press_start_frame: args.auto_press_start_frame,
                press_hold_frames: args.press_hold_frames,
                press_period_frames: args.press_period_frames,
                // Default is windowed (since v32). `--fullscreen` opts
                // into fullscreen; `--windowed` is kept as a no-op alias
                // for backwards compat. F11 still toggles in-flight.
                fullscreen: args.fullscreen && !args.windowed,
                crop: crop_on,
            },
        )?;
    } else {
        run_headless(&args, &mut sys)?;
        log_diagnostics(&sys);
    }

    Ok(())
}

fn load_system(args: &Args) -> Result<System> {
    let hw = match args.hardware.to_ascii_lowercase().as_str() {
        "aes" => Hardware::Aes,
        _ => Hardware::Mvs,
    };

    let cfg = SystemConfig {
        hardware: hw,
        trace_cpu: args.trace_cpu,
        trace_audio_io: args.trace_audio_io,
        audio_sample_rate: if args.audio_out.is_some() { Some(55_555) } else { None },
    };
    let mut sys = System::new(cfg);

    let mut romset = RomSet::default();
    // Explicit BIOS file beats everything.
    if let Some(p) = &args.bios {
        romset.load_bios(p)?;
    }
    // Parent BIOS zip provides the auto-pick BIOS, the Y-zoom ROM and the
    // BIOS-side sfix/sm1 fallbacks. Loaded BEFORE the cart so the cart can
    // still override slot ROMs it ships itself.
    if let Some(p) = &args.bios_zip {
        romset.load_parent_bios_zip(p)?;
    }
    // Explicit BIOS-by-name selection inside the parent zip.
    if let (Some(p), Some(want)) = (&args.bios_zip, &args.bios_name) {
        romset.pick_bios_from_zip(p, want)?;
    }
    if let Some(d) = &args.cart {
        romset.load_cart_any(d)?;
    }
    // If user gave --bios-name but only a cart zip, look there too.
    if let (Some(want), None, Some(cart_path)) =
        (&args.bios_name, &args.bios_zip, &args.cart)
    {
        let is_zip = cart_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("zip"))
            .unwrap_or(false);
        if is_zip {
            romset.pick_bios_from_zip(cart_path, want)?;
        }
    }
    sys.load(romset)?;
    sys.reset();

    log::info!(
        "Reset complete. PC=${:08X} SSP=${:08X}",
        sys.m68k.pc,
        sys.m68k.a[7]
    );

    Ok(sys)
}

fn run_headless(args: &Args, sys: &mut System) -> Result<()> {
    if let Some(dir) = &args.dump_frames_dir {
        fs::create_dir_all(dir)
            .with_context(|| format!("cannot create dump dir {}", dir.display()))?;
    }

    if let Some(n) = args.max_instructions {
        for _ in 0..n {
            sys.step();
        }
        log::info!(
            "Exit. Total instructions: {}, cycles: {}",
            sys.instructions,
            sys.master_cycles
        );
        return Ok(());
    }

    let every = args.dump_every_frames.max(1);
    let total_frames = if args.max_frames == 0 { 1 } else { args.max_frames };
    for f in 0..total_frames {
        apply_auto_inputs(sys, args, f);
        sys.run_frame();
        log::info!(
            "Frame {f} done — total inst={} cycles={} PC=${:08X}",
            sys.instructions,
            sys.master_cycles,
            sys.m68k.pc
        );

        if let Some(dir) = &args.dump_frames_dir {
            if f % every == 0 {
                let frame = sys.render_frame_pixels();
                let png = pydmg_neogeo::graphics::video::frame_to_png(
                    &frame,
                    pydmg_neogeo::graphics::video::SCREEN_W,
                    pydmg_neogeo::graphics::video::SCREEN_H,
                );
                let out = dir.join(format!("frame-{f:05}.png"));
                fs::write(&out, &png)
                    .with_context(|| format!("writing {}", out.display()))?;
                log::info!("  → wrote {}", out.display());
            }
        }
    }

    log::info!(
        "Exit. Total instructions: {}, cycles: {}",
        sys.instructions,
        sys.master_cycles
    );

    if let Some(path) = &args.audio_out {
        sys.write_wav(path)?;
    }

    let ym = &sys.audio.ym;
    log::info!(
        "YM2610: fm_keyon={} adpcma_keyon={} adpcmb_keyon={} | nonzero_samples fm={} adpcma={} adpcmb={} ssg={} (audio active when > 0)",
        ym.dbg_fm_keyon,
        ym.dbg_adpcma_keyon,
        ym.dbg_adpcmb_keyon,
        ym.dbg_fm_nz,
        ym.dbg_adpcma_nz,
        ym.dbg_adpcmb_nz,
        ym.dbg_ssg_nz,
    );
    log::info!(
        "YM2610 per-channel: ADPCM-A keyon=[{},{},{},{},{},{}] nz=[{},{},{},{},{},{}]",
        ym.dbg_adpcma_keyon_ch[0], ym.dbg_adpcma_keyon_ch[1], ym.dbg_adpcma_keyon_ch[2],
        ym.dbg_adpcma_keyon_ch[3], ym.dbg_adpcma_keyon_ch[4], ym.dbg_adpcma_keyon_ch[5],
        ym.dbg_adpcma_nz_ch[0], ym.dbg_adpcma_nz_ch[1], ym.dbg_adpcma_nz_ch[2],
        ym.dbg_adpcma_nz_ch[3], ym.dbg_adpcma_nz_ch[4], ym.dbg_adpcma_nz_ch[5],
    );
    log::info!(
        "YM2610 per-channel: FM keyon=[{},{},{},{}] nz=[{},{},{},{}]",
        ym.dbg_fm_keyon_ch[0], ym.dbg_fm_keyon_ch[1], ym.dbg_fm_keyon_ch[2], ym.dbg_fm_keyon_ch[3],
        ym.dbg_fm_nz_ch[0], ym.dbg_fm_nz_ch[1], ym.dbg_fm_nz_ch[2], ym.dbg_fm_nz_ch[3],
    );

    Ok(())
}

fn apply_auto_inputs(sys: &mut System, args: &Args, frame: u32) {
    if args.auto_coin_frame > 0 {
        let coin_start = args.auto_coin_frame;
        let coin_end = coin_start.saturating_add(args.coin_hold_frames);
        if frame == coin_start {
            sys.bus.coin_inputs &= !0x01;
            log::info!("AUTO INPUT: COIN1 inserted at frame {frame}");
        } else if frame == coin_end {
            sys.bus.coin_inputs |= 0x01;
            log::info!("AUTO INPUT: COIN1 released at frame {frame}");
        }
    }

    if args.auto_press_start_frame > 0 && frame >= args.auto_press_start_frame {
        let period = args.press_period_frames.max(args.press_hold_frames + 1);
        let phase = (frame - args.auto_press_start_frame) % period;
        let was_pressed = (sys.bus.start_select & 0x01) == 0;
        let press = phase < args.press_hold_frames;
        if press {
            sys.bus.start_select &= !0x01;
            if !was_pressed {
                log::info!("AUTO INPUT: P1 START pressed at frame {frame}");
            }
        } else {
            sys.bus.start_select |= 0x01;
            if was_pressed {
                log::info!("AUTO INPUT: P1 START released at frame {frame}");
            }
        }
    }
}

fn log_diagnostics(sys: &System) {
    let mut fix_nonzero = 0usize;
    for col in 0..40 {
        for row in 0..32 {
            let v = sys.bus.lspc.vram[0x7000 + col * 32 + row];
            if v != 0 {
                fix_nonzero += 1;
            }
        }
    }

    let mut pal_nonzero = 0usize;
    for w in sys.bus.palette_ram.chunks_exact(2) {
        if w != [0, 0] {
            pal_nonzero += 1;
        }
    }

    let mut spr_nonzero = 0usize;
    for i in 0..381 {
        if sys.bus.lspc.vram[(0x8200 + i) & 0x7FFF] != 0 {
            spr_nonzero += 1;
        }
    }

    log::info!(
        "VRAM diagnostics: fix-cells set={fix_nonzero}/1280, palette entries set={pal_nonzero}/4096, sprite SCB3 entries set={spr_nonzero}/381"
    );

    let mut pal_words = String::new();
    for i in 0..16 {
        let hi = sys.bus.palette_ram[i * 2] as u16;
        let lo = sys.bus.palette_ram[i * 2 + 1] as u16;
        let w = (hi << 8) | lo;
        pal_words.push_str(&format!("${w:04X} "));
    }
    log::info!("  Palette[0..16] = {pal_words}");
}

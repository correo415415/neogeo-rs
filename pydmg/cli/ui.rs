use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sdl2::{
    audio::{AudioCallback, AudioDevice, AudioSpecDesired},
    event::Event,
    keyboard::Keycode,
    pixels::PixelFormatEnum,
    rect::Rect,
    render::{Canvas, Texture},
    video::{FullscreenType, Window},
};

use pydmg_neogeo::{
    video::{Frame, SCREEN_H, SCREEN_W},
    System,
};

const P1_UP: u8 = 0x01;
const P1_DOWN: u8 = 0x02;
const P1_LEFT: u8 = 0x04;
const P1_RIGHT: u8 = 0x08;
const P1_A: u8 = 0x10;
const P1_B: u8 = 0x20;
const P1_C: u8 = 0x40;
const P1_D: u8 = 0x80;

const START_P1: u8 = 0x01;
const SELECT_P1: u8 = 0x02;
const START_P2: u8 = 0x04;
const SELECT_P2: u8 = 0x08;

const COIN_1: u8 = 0x01;
const COIN_2: u8 = 0x02;
const SERVICE_1: u8 = 0x04;

#[derive(Debug, Clone, Copy)]
pub struct UiOptions {
    pub scale: u32,
    pub vsync: bool,
    /// Si es `true` deshabilita el cap a 60 Hz (útil para benchmark).
    pub uncapped: bool,
    pub max_frames: u32,
    pub auto_coin_frame: u32,
    pub coin_hold_frames: u32,
    pub auto_press_start_frame: u32,
    pub press_hold_frames: u32,
    pub press_period_frames: u32,
    /// Presentación inicial. `true` = fullscreen desktop (default).
    /// El usuario puede alternar fullscreen <-> windowed con F11 en caliente.
    pub fullscreen: bool,
    /// Si es `true`, recorta 1 columna de 8 px a cada lado del framebuffer
    /// 320×224 emitido por la LSPC, devolviendo la vista canónica de
    /// 304×224 que usan MAME (`Screen 0 Cropped`) y FBNeo para juegos
    /// 304-based (Metal Slug, KOF, Garou, Samurai Shodown, …).
    pub crop: bool,
}

/// Shared ring-buffer fed by the emulator and drained by SDL's audio
/// callback. Each emulated frame produces ~925 stereo samples at the
/// YM2610's native rate (~55,555 Hz); SDL's callback pulls samples in
/// chunks at whatever rate the audio device requests.
///
/// We keep an oversize buffer (16 384 stereo samples ≈ 0.3 s of audio)
/// so brief stalls in either producer or consumer don't underflow.
struct AudioRing {
    buf: std::collections::VecDeque<i16>,
    max: usize,
}

impl AudioRing {
    fn new(max: usize) -> Self {
        Self { buf: std::collections::VecDeque::with_capacity(max), max }
    }
    fn push_stereo(&mut self, samples: &[i16]) {
        // Drop oldest when full to keep latency bounded.
        for &s in samples {
            if self.buf.len() >= self.max { self.buf.pop_front(); }
            self.buf.push_back(s);
        }
    }
    fn pull(&mut self, out: &mut [i16]) -> usize {
        let mut n = 0;
        while n < out.len() {
            match self.buf.pop_front() {
                Some(v) => { out[n] = v; n += 1; }
                None => break,
            }
        }
        n
    }
}

struct YmCallback {
    ring: Arc<Mutex<AudioRing>>,
}

impl AudioCallback for YmCallback {
    type Channel = i16;
    fn callback(&mut self, out: &mut [i16]) {
        let mut ring = self.ring.lock().unwrap();
        let n = ring.pull(out);
        // Underrun: fill rest with silence.
        for s in &mut out[n..] { *s = 0; }
    }
}

/// Geometry derived from `crop`: per-side trim in pixels plus the
/// visible (view_w × view_h) and the source rect inside the 320×224
/// framebuffer that the LSPC always emits.
///
/// Real hardware paints **40 tile columns** of 8 px each (320 px total).
/// SNK reserved **columns 0 and 39** as an overscan-safe pillarbox: the
/// BIOS clears them and 304-based games (Metal Slug, KOF, Garou, etc.)
/// never draw inside them, so they look like backdrop bars on a flat
/// LCD viewer. `--crop` trims exactly those two columns, giving the
/// canonical 304×224 view used by MAME (`Screen 0 Cropped`) and FBNeo.
///
/// There is no other sane crop value: trimming fewer pixels leaves the
/// pillarbox visible, trimming more starts eating game graphics. So we
/// expose a plain boolean rather than a numeric tunable.
#[derive(Debug, Clone, Copy)]
struct ViewGeom {
    crop_per_side_px: u32,
    view_w: u32,
    view_h: u32,
    src_rect: Rect,
}

impl ViewGeom {
    /// Visible columns trimmed per side when `--crop` is enabled.
    /// One 8-px tile column = exactly half the 16-px overscan-safe band.
    const CROP_PER_SIDE_PX: u32 = 8;

    fn new(crop: bool) -> Self {
        let crop_per_side_px = if crop { Self::CROP_PER_SIDE_PX } else { 0 };
        let view_w: u32 = SCREEN_W as u32 - crop_per_side_px * 2;
        let view_h: u32 = SCREEN_H as u32;
        Self {
            crop_per_side_px,
            view_w,
            view_h,
            src_rect: Rect::new(crop_per_side_px as i32, 0, view_w, view_h),
        }
    }
}

pub fn run_ui(sys: &mut System, opts: UiOptions) -> Result<()> {
    let scale = opts.scale.max(1);

    // Compute initial view geometry from the requested crop. The
    // *texture* is always full-frame (320×224); cropping is a zero-copy
    // change of the source rectangle passed to SDL's blit.
    let geom = ViewGeom::new(opts.crop);

    let sdl = sdl2::init().map_err(anyhow::Error::msg)?;
    let video = sdl.video().map_err(anyhow::Error::msg)?;
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "nearest");

    // ------- Audio device (NEW in v20) -------
    // Enable in-core audio capture so step() pulls YM2610 samples into
    // sys.audio_buffer. We then drain it into our SDL ring each frame.
    sys.config.audio_sample_rate = Some(55_555);
    let audio = sdl.audio().map_err(anyhow::Error::msg)?;
    let ring = Arc::new(Mutex::new(AudioRing::new(16_384)));
    let want = AudioSpecDesired {
        freq: Some(55_555),
        channels: Some(2),
        samples: Some(1024),
    };
    let device: AudioDevice<YmCallback> =
        audio.open_playback(None, &want, |_spec| YmCallback { ring: ring.clone() })
            .map_err(anyhow::Error::msg)?;
    device.resume();
    log::info!("SDL audio device opened: 55555 Hz stereo i16, buf=1024 samples");
    log::info!(
        "SDL UI view: {}x{} (crop={} => {} px por lado) | fullscreen={}",
        geom.view_w,
        geom.view_h,
        opts.crop,
        geom.crop_per_side_px,
        opts.fullscreen,
    );

    let mut window = video
        .window(
            "neogeo-rs | iniciando…",
            geom.view_w * scale,
            geom.view_h * scale,
        )
        .position_centered()
        .resizable()
        .allow_highdpi()
        .build()
        .map_err(anyhow::Error::msg)?;

    if opts.fullscreen {
        window
            .set_fullscreen(FullscreenType::Desktop)
            .map_err(anyhow::Error::msg)?;
    }

    let mut canvas = build_canvas(window, opts.vsync)?;
    // `set_logical_size` makes SDL letterbox to preserve the aspect ratio
    // when the window is resized (and when toggling fullscreen).
    canvas
        .set_logical_size(geom.view_w, geom.view_h)
        .map_err(anyhow::Error::msg)?;

    let texture_creator = canvas.texture_creator();
    // The streaming texture is always the full 320×224 framebuffer; the
    // crop happens at blit time via the source-rect argument to `copy()`.
    // Keeping the texture full-width avoids re-uploading partial rows and
    // makes future view changes (F11, --crop) zero-copy.
    //
    // ABGR8888 en little-endian deja los bytes en memoria como
    // R, G, B, A — exacto al formato `0xRRGGBBAA` que produce el
    // renderer del Neo Geo core. SDL2 con `RGBA8888` los reinterpreta
    // según la endianness y rompe el orden de canales en x86, dando
    // una imagen toda rojiza.
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::ABGR8888, SCREEN_W as u32, SCREEN_H as u32)
        .map_err(anyhow::Error::msg)?;

    let src_rect = geom.src_rect;

    let mut events = sdl.event_pump().map_err(anyhow::Error::msg)?;
    let mut running = true;
    let mut frame_index: u32 = 0;
    let mut fps_frames: u32 = 0;
    let mut fps_timer = Instant::now();
    let mut last_fps = 0.0_f64;
    // Track current fullscreen state so F11 can toggle it.
    let mut is_fullscreen = opts.fullscreen;

    // Neo Geo corre a 59.185606 Hz. Limitamos el bucle a ese ritmo,
    // salvo que el usuario active vsync (en cuyo caso el cap lo pone
    // el compositor) o pase --no-fps-cap (via opts.uncapped).
    let target_frame_time = Duration::from_secs_f64(1.0 / 59.185_606);
    let frame_cap_enabled = !opts.uncapped;
    let mut next_frame_deadline = Instant::now() + target_frame_time;

    update_title(&mut canvas, last_fps, geom.view_w, geom.view_h, is_fullscreen)?;

    while running {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => running = false,
                Event::KeyDown {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => {
                    if key == Keycode::Escape {
                        running = false;
                    } else if key == Keycode::F11 {
                        // Toggle fullscreen <-> windowed in-flight. Logical
                        // size + nearest scaling keep the cropped view
                        // pixel-perfect under both presentations.
                        is_fullscreen = !is_fullscreen;
                        let target = if is_fullscreen {
                            FullscreenType::Desktop
                        } else {
                            FullscreenType::Off
                        };
                        if let Err(e) = canvas.window_mut().set_fullscreen(target) {
                            log::warn!("F11 toggle failed: {e}");
                            is_fullscreen = !is_fullscreen;
                        } else {
                            log::info!(
                                "F11: fullscreen={} (view {}x{})",
                                is_fullscreen, geom.view_w, geom.view_h
                            );
                            update_title(
                                &mut canvas,
                                last_fps,
                                geom.view_w,
                                geom.view_h,
                                is_fullscreen,
                            )?;
                        }
                    } else {
                        apply_key(sys, key, true);
                    }
                }
                Event::KeyUp {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => apply_key(sys, key, false),
                _ => {}
            }
        }

        apply_auto_inputs(sys, frame_index, opts);

        sys.run_frame();
        // Drain freshly produced YM2610 samples into the SDL audio ring.
        if !sys.audio_buffer.is_empty() {
            let mut ring = ring.lock().unwrap();
            ring.push_stereo(&sys.audio_buffer);
            sys.audio_buffer.clear();
        }
        let frame = sys.render_frame_pixels();
        upload_frame(&mut texture, &frame)?;

        canvas.clear();
        // Source rect = the selected cropped region (always inside the
        // 320×224 framebuffer). dest = None lets SDL stretch into the
        // entire logical viewport, preserving aspect ratio on resize.
        canvas
            .copy(&texture, Some(src_rect), None)
            .map_err(anyhow::Error::msg)?;
        canvas.present();

        frame_index = frame_index.wrapping_add(1);
        fps_frames = fps_frames.wrapping_add(1);

        let elapsed = fps_timer.elapsed();
        if elapsed >= Duration::from_secs(1) {
            last_fps = fps_frames as f64 / elapsed.as_secs_f64();
            update_title(&mut canvas, last_fps, geom.view_w, geom.view_h, is_fullscreen)?;
            fps_frames = 0;
            fps_timer = Instant::now();
        }

        if opts.max_frames > 0 && frame_index >= opts.max_frames {
            break;
        }

        // Cap a 60 Hz. Dormimos lo justo para alinear con la deadline
        // del siguiente frame; si nos retrasamos resincronizamos a now.
        if frame_cap_enabled {
            let now = Instant::now();
            if now < next_frame_deadline {
                std::thread::sleep(next_frame_deadline - now);
                next_frame_deadline += target_frame_time;
            } else {
                // Nos hemos pasado: resync.
                next_frame_deadline = now + target_frame_time;
            }
        }
    }

    Ok(())
}

fn build_canvas(window: Window, vsync: bool) -> Result<Canvas<Window>> {
    if vsync {
        log::info!("SDL UI: solicitando vsync si el backend lo soporta");
    }
    window
        .into_canvas()
        .software()
        .build()
        .map_err(anyhow::Error::msg)
        .context("no se pudo crear canvas SDL")
}

fn upload_frame(texture: &mut Texture<'_>, frame: &Frame) -> Result<()> {
    texture
        .with_lock(None, |buf: &mut [u8], pitch: usize| {
            for y in 0..SCREEN_H {
                for x in 0..SCREEN_W {
                    let px = frame[y * SCREEN_W + x];
                    let off = y * pitch + x * 4;
                    buf[off] = (px >> 24) as u8;
                    buf[off + 1] = (px >> 16) as u8;
                    buf[off + 2] = (px >> 8) as u8;
                    buf[off + 3] = px as u8;
                }
            }
        })
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

fn update_title(
    canvas: &mut Canvas<Window>,
    fps: f64,
    view_w: u32,
    view_h: u32,
    fullscreen: bool,
) -> Result<()> {
    let mode = if fullscreen { "FS" } else { "WIN" };
    let title = format!(
        "neogeo-rs | {view_w}×{view_h} [{mode}] | {:.1} FPS | F11 fullscreen | ESC salir | 5 moneda | Enter START | Flechas mover | Z/X/C/V = A/B/C/D",
        fps
    );
    canvas.window_mut().set_title(&title).map_err(anyhow::Error::msg)?;
    Ok(())
}

fn apply_auto_inputs(sys: &mut System, frame: u32, opts: UiOptions) {
    if opts.auto_coin_frame > 0 {
        let coin_start = opts.auto_coin_frame;
        let coin_end = coin_start.saturating_add(opts.coin_hold_frames);
        if frame == coin_start {
            sys.bus.coin_inputs &= !COIN_1;
        } else if frame == coin_end {
            sys.bus.coin_inputs |= COIN_1;
        }
    }

    if opts.auto_press_start_frame > 0 && frame >= opts.auto_press_start_frame {
        let period = opts.press_period_frames.max(opts.press_hold_frames + 1);
        let phase = (frame - opts.auto_press_start_frame) % period;
        let press = phase < opts.press_hold_frames;
        set_active_low(&mut sys.bus.start_select, START_P1, press);
    }
}

fn apply_key(sys: &mut System, key: Keycode, pressed: bool) {
    match key {
        Keycode::Up => set_active_low(&mut sys.bus.p1_input, P1_UP, pressed),
        Keycode::Down => set_active_low(&mut sys.bus.p1_input, P1_DOWN, pressed),
        Keycode::Left => set_active_low(&mut sys.bus.p1_input, P1_LEFT, pressed),
        Keycode::Right => set_active_low(&mut sys.bus.p1_input, P1_RIGHT, pressed),
        Keycode::Z => set_active_low(&mut sys.bus.p1_input, P1_A, pressed),
        Keycode::X => set_active_low(&mut sys.bus.p1_input, P1_B, pressed),
        Keycode::C => set_active_low(&mut sys.bus.p1_input, P1_C, pressed),
        Keycode::V => set_active_low(&mut sys.bus.p1_input, P1_D, pressed),
        Keycode::Return => set_active_low(&mut sys.bus.start_select, START_P1, pressed),
        Keycode::RShift => set_active_low(&mut sys.bus.start_select, SELECT_P1, pressed),
        Keycode::Num5 => set_active_low(&mut sys.bus.coin_inputs, COIN_1, pressed),
        Keycode::Num6 => set_active_low(&mut sys.bus.coin_inputs, COIN_2, pressed),
        Keycode::F2 => set_active_low(&mut sys.bus.coin_inputs, SERVICE_1, pressed),
        Keycode::Num1 => set_active_low(&mut sys.bus.start_select, START_P1, pressed),
        Keycode::Num2 => set_active_low(&mut sys.bus.start_select, START_P2, pressed),
        Keycode::Num3 => set_active_low(&mut sys.bus.start_select, SELECT_P1, pressed),
        Keycode::Num4 => set_active_low(&mut sys.bus.start_select, SELECT_P2, pressed),
        _ => {}
    }
}

#[inline]
fn set_active_low(reg: &mut u8, mask: u8, pressed: bool) {
    if pressed {
        *reg &= !mask;
    } else {
        *reg |= mask;
    }
}

// -----------------------------------------------------------------------
// Tests: validan la geometría derivada de --crop-columns. Como los tests
// están dentro del binario, se compilan con `cargo test --features ui`.
// -----------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_off_yields_full_320() {
        let g = ViewGeom::new(false);
        assert_eq!(g.crop_per_side_px, 0);
        assert_eq!(g.view_w, 320);
        assert_eq!(g.view_h, 224);
        assert_eq!(g.src_rect.x(), 0);
        assert_eq!(g.src_rect.width(), 320);
    }

    #[test]
    fn crop_on_yields_304_centered() {
        let g = ViewGeom::new(true);
        assert_eq!(g.crop_per_side_px, 8);
        assert_eq!(g.view_w, 304);
        assert_eq!(g.view_h, 224);
        assert_eq!(g.src_rect.x(), 8);
        assert_eq!(g.src_rect.width(), 304);
    }

    #[test]
    fn crop_value_matches_neo_geo_overscan_safe_band() {
        // The Neo Geo's overscan-safe band is exactly 16 px wide (one 8-px
        // tile column on each edge). Trimming half on each side gives the
        // canonical 304-px active area used by MAME and FBNeo.
        assert_eq!(ViewGeom::CROP_PER_SIDE_PX, 8);
        assert_eq!(SCREEN_W as u32 - 2 * ViewGeom::CROP_PER_SIDE_PX, 304);
    }
}

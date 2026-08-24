//! JNI bridge between the `pydmg-neogeo` Rust core and the Android
//! Kotlin frontend.
//!
//! Design:
//!   * One global `Mutex<Option<EmulatorState>>` holds the live `System`
//!     plus a scratch RGBA framebuffer. Kotlin side is single-threaded
//!     for emu calls (one dedicated emulator thread), so the mutex is
//!     uncontended on the hot path; it exists only to guarantee
//!     `Send + Sync` for `static`.
//!   * Method naming is `Java_com_pydmg_neogeo_NativeBridge_<name>`.
//!     Matches the package + class declared in Kotlin
//!     (`com.pydmg.neogeo.NativeBridge`).
//!   * All functions take `JNIEnv` + `JClass` first (standard JNI),
//!     return primitive `jint`/`jboolean` or `void` (logged errors).
//!   * Logs go through `android_logger` -> `logcat` tag `pydmg-neogeo`.

#![allow(non_snake_case)]

mod aaudio_stream;

use std::sync::Mutex;

use jni::objects::{JByteArray, JClass, JIntArray, JShortArray, JString};
use jni::sys::{jboolean, jint, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use once_cell::sync::Lazy;

use pydmg_neogeo::{
    rom::RomSet,
    system::{Hardware, SystemConfig},
    video::{SCREEN_H, SCREEN_W},
    System,
};

// ---------------------------------------------------------------------------
//   Global state — single-instance emulator with scratch framebuffer.
// ---------------------------------------------------------------------------

struct EmulatorState {
    system: System,
    /// Reusable RGBA scratch (320 * 224 = 71 680 px). Kotlin copies
    /// from here to its IntArray each frame, avoiding per-frame
    /// allocations.
    frame_scratch: Vec<u32>,
}

/// Host audio sample rate delivered to AAudio.
///
/// **v4-audio decision (revised after user testing):** we no longer
/// resample inside the emulator thread. Instead we ask AAudio to open
/// its stream at the **YM2610's native 55_555 Hz** and let AAudio's
/// HAL do the SRC in its own real-time thread (SCHED_FIFO), which is
/// what modern Android devices do very efficiently now.
///
/// Why this changes vs the first v4-audio release:
///
///   * Kaiser 65-tap resampling inside the emu thread costs ~130 MACs
///     per output sample = 6.24 MFLOPS. On a modern flagship this is
///     nothing (~0.05 % of a core), but the emu thread ALSO runs the
///     M68K + Z80 + LSPC + YM2610 (which the v42 rebase made a bit
///     heavier due to ymfm-accurate ADPCM math), and in debug Kotlin
///     builds / mid-range devices the total budget can spill over the
///     16.6 ms/frame boundary. When that happens the emu drops below
///     60 fps AND the AAudio ring runs dry → audible "petardeo" AND
///     missed ADPCM-A key-ons ("Heavy Machine Gun" voice never
///     triggers because the Z80 hasn't caught up in time to load the
///     sample start address).
///
///   * By opening AAudio at 55_555 Hz directly the emu thread does
///     nothing beyond `emu.step()` and a raw i16 push into the ring.
///     The HAL-side resampler is production-tuned by the vendor and
///     runs alongside the emu, not on top of it.
///
///   * If the device HAL refuses 55_555 Hz for LOW_LATENCY exclusive
///     mode (some vendors clamp to 48000 / 44100), our SHARED fallback
///     inside `open_stream` still works because AAudio auto-inserts a
///     resampler in SHARED mode.
///
///   * The core keeps `audio_sample_rate = Some(55_555)` so `audio_buffer`
///     is fed
///     raw YM samples, exactly the way v42 wanted them.
const HOST_AUDIO_HZ: u32 = 55_555;

impl EmulatorState {
    fn new(hw: Hardware) -> Self {
        let mut cfg = SystemConfig::default();
        cfg.hardware = hw;
        // Raw YM2610 samples at 55_555 Hz. No in-core resampling.
        cfg.audio_sample_rate = Some(HOST_AUDIO_HZ);
        let system = System::new(cfg);
        Self {
            system,
            frame_scratch: vec![0u32; SCREEN_W * SCREEN_H],
        }
    }
}

static STATE: Lazy<Mutex<Option<EmulatorState>>> = Lazy::new(|| Mutex::new(None));

fn with_state<R>(default: R, f: impl FnOnce(&mut EmulatorState) -> R) -> R {
    let mut guard = STATE.lock().expect("STATE mutex poisoned");
    match guard.as_mut() {
        Some(st) => f(st),
        None => {
            log::error!("emulator method called before nativeCreate()");
            default
        }
    }
}

// ---------------------------------------------------------------------------
//   Bitflags constants — must stay in sync with NativeBridge.kt.
//   These map directly to NeoGeoBus input ports (active-low).
// ---------------------------------------------------------------------------

// Per-player bits — bus.p1_input / bus.p2_input (active low)
const DIR_UP: u8    = 0x01;
const DIR_DOWN: u8  = 0x02;
const DIR_LEFT: u8  = 0x04;
const DIR_RIGHT: u8 = 0x08;
const BTN_A_HW: u8  = 0x10;
const BTN_B_HW: u8  = 0x20;
const BTN_C_HW: u8  = 0x40;
const BTN_D_HW: u8  = 0x80;

// start_select bits (active low)
const START_P1: u8  = 0x01;
const SELECT_P1: u8 = 0x02;
const START_P2: u8  = 0x04;
const SELECT_P2: u8 = 0x08;

// coin bits (active low)
const COIN_1: u8 = 0x01;
const COIN_2: u8 = 0x02;

// Public constants exposed to Kotlin via getters.
const BTN_UP: jint     = 1 << 0;
const BTN_DOWN: jint   = 1 << 1;
const BTN_LEFT: jint   = 1 << 2;
const BTN_RIGHT: jint  = 1 << 3;
const BTN_A: jint      = 1 << 4;
const BTN_B: jint      = 1 << 5;
const BTN_C: jint      = 1 << 6;
const BTN_D: jint      = 1 << 7;
const BTN_START: jint  = 1 << 8;
const BTN_SELECT: jint = 1 << 9;
const BTN_COIN: jint   = 1 << 10;

// ---------------------------------------------------------------------------
//   Logger init — called once from Java side at app start.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeInitLogger(
    _env: JNIEnv,
    _class: JClass,
) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("pydmg-neogeo"),
    );
    log::info!("pydmg-neogeo native logger initialised");
}

// ---------------------------------------------------------------------------
//   Lifecycle: create / destroy
// ---------------------------------------------------------------------------

/// Create the emulator instance. `hardware`: 0 = MVS (arcade), 1 = AES.
/// Idempotent: a second call destroys the previous instance first.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeCreate(
    _env: JNIEnv,
    _class: JClass,
    hardware: jint,
) -> jboolean {
    let hw = if hardware == 1 { Hardware::Aes } else { Hardware::Mvs };
    let mut guard = STATE.lock().expect("STATE mutex poisoned");
    *guard = Some(EmulatorState::new(hw));
    log::info!("nativeCreate: hardware={:?}", hw);
    JNI_TRUE
}

#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeDestroy(
    _env: JNIEnv,
    _class: JClass,
) {
    let mut guard = STATE.lock().expect("STATE mutex poisoned");
    *guard = None;
    log::info!("nativeDestroy");
}

// ---------------------------------------------------------------------------
//   ROM loading — bytes-only API (Android does NOT use std::fs)
// ---------------------------------------------------------------------------

/// Load the parent BIOS zip (e.g. `neogeo.zip`) from a byte array.
/// Returns true on success.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeLoadBiosZip<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    data: JByteArray<'a>,
) -> jboolean {
    let bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(e) => { log::error!("convert bios bytes failed: {e}"); return JNI_FALSE; }
    };
    with_state(JNI_FALSE, |st| {
        // Build a RomSet, load BIOS zip into it, then we'll combine with cart
        // in nativeLoadCartZip. We stash the partial RomSet inside the
        // System by re-creating it. To keep things simple: load BIOS into
        // a fresh RomSet kept side-table-free; we apply cart later via a
        // single combined `nativeLoadCartAndStart` flow.
        // For an iterative UI (load BIOS, then choose cart, then start),
        // we hold the partial RomSet on the side.
        match BIOS_STAGE.lock() {
            Ok(mut g) => {
                let mut rs = RomSet::default();
                if let Err(e) = rs.load_parent_bios_zip_from_bytes(&bytes) {
                    log::error!("load_parent_bios_zip_from_bytes: {e:#}");
                    *g = None;
                    return JNI_FALSE;
                }
                log::info!("BIOS staged: {} bytes, lo_rom={} bytes, sfix={} bytes",
                           rs.bios.len(), rs.lo_rom.len(), rs.bios_sfix.len());
                *g = Some(rs);
                let _ = st;  // silence unused warning
                JNI_TRUE
            }
            Err(e) => { log::error!("BIOS_STAGE poisoned: {e}"); JNI_FALSE }
        }
    })
}

/// Load a cart zip (e.g. `mslug.zip`) from a byte array AND start the
/// emulator. `cartName` is the romset short name used for protection
/// auto-detect (`mslug`, `mslug3`, `kof98`, ...). Returns true on
/// success.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeLoadCartZip<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass<'a>,
    cart_name: JString<'a>,
    data: JByteArray<'a>,
) -> jboolean {
    let bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(e) => { log::error!("convert cart bytes failed: {e}"); return JNI_FALSE; }
    };
    let cart_name: String = match env.get_string(&cart_name) {
        Ok(s) => s.into(),
        Err(e) => { log::error!("convert cart_name failed: {e}"); return JNI_FALSE; }
    };

    // Pull staged BIOS RomSet (or create empty if user only loads cart).
    let mut rs = match BIOS_STAGE.lock() {
        Ok(mut g) => g.take().unwrap_or_default(),
        Err(e) => { log::error!("BIOS_STAGE poisoned: {e}"); return JNI_FALSE; }
    };
    if let Err(e) = rs.load_cart_zip_from_bytes(&cart_name, &bytes) {
        log::error!("load_cart_zip_from_bytes: {e:#}");
        return JNI_FALSE;
    }
    log::info!("Cart '{}' loaded: P:{} S:{} M:{} V:{} C:{}",
               cart_name, rs.cart.p_rom.len(), rs.cart.s_rom.len(),
               rs.cart.m_rom.len(), rs.cart.v_roms.len(),
               rs.cart.c_roms.iter().map(|c| c.len()).sum::<usize>());

    with_state(JNI_FALSE, |st| {
        if let Err(e) = st.system.load(rs) {
            log::error!("system.load: {e:#}");
            return JNI_FALSE;
        }
        st.system.reset();
        log::info!("System loaded and reset. Ready to run frames.");
        JNI_TRUE
    })
}

/// Staging slot for a partially-loaded RomSet between
/// `nativeLoadBiosZip` and `nativeLoadCartZip`.
static BIOS_STAGE: Lazy<Mutex<Option<RomSet>>> = Lazy::new(|| Mutex::new(None));

// ---------------------------------------------------------------------------
//   Per-frame run + framebuffer fetch
// ---------------------------------------------------------------------------

/// Run one emulated frame (~200 000 68K cycles).
///
/// **Audio side effect** (v4-audio): if the native AAudio driver is
/// running, this function drains the freshly-produced samples from
/// `System::audio_buffer` straight into the wait-free SPSC ring the
/// driver reads from. Kotlin never sees the samples — no JNI copy,
/// no locks, no AudioTrack blocking write.
///
/// Returns the number of i16 samples still buffered on the Kotlin
/// side, which is:
///   * 0 when the AAudio driver is running (all samples went to the
///     ring so Kotlin's fallback `nativeDrainAudio` has nothing to do)
///   * `audio_buffer.len()` otherwise (fallback AudioTrack path).
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeRunFrame(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    with_state(0, |st| {
        st.system.run_frame();
        // Steal samples for the AAudio ring if it's live. Whatever
        // the ring can't accept (extremely rare — only if the CPU
        // stalled long enough for it to overflow) is dropped instead
        // of being played back late.
        if aaudio_stream::is_running() && !st.system.audio_buffer.is_empty() {
            let n = aaudio_stream::push(&st.system.audio_buffer);
            let consumed = n * 2;
            if consumed >= st.system.audio_buffer.len() {
                st.system.audio_buffer.clear();
            } else {
                st.system.audio_buffer.drain(..consumed);
            }
        }
        st.system.audio_buffer.len() as jint
    })
}

/// Copy the most recent framebuffer into `out` (must be exactly
/// `SCREEN_W * SCREEN_H` = 320*224 = 71680 ints). Each int is
/// `0xRRGGBBAA` (matches the renderer's output verbatim).
///
/// **Legacy path** — kept for backwards compatibility with older
/// Kotlin builds that repack the pixels themselves. New code should
/// call [`nativeGetFramebufferArgb`], which returns pixels already in
/// Android's native `ARGB_8888` layout so `Bitmap.setPixels` can
/// consume them with zero repacking cost on the Kotlin side (saves
/// ~2–8 ms/frame on mid-range devices, since the previous per-pixel
/// Kotlin loop was one of the two things keeping mslug below 60 fps).
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeGetFramebuffer<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    out: JIntArray<'a>,
) -> jboolean {
    with_state(JNI_FALSE, |st| {
        let frame = st.system.render_frame_pixels();
        st.frame_scratch.copy_from_slice(&frame);
        // The JIntArray is i32 from Kotlin's view; the u32 RGBA values
        // re-interpret as i32 verbatim (same bit pattern). We go through
        // a transmute-free cast via to_le_bytes -> from_le_bytes, but
        // JNI's set_int_array_region takes &[i32] so a bit-cast suffices.
        // SAFETY: we use a plain reinterpret via slice::from_raw_parts; it
        // is a same-size, same-alignment view of [u32] as [i32]. This is
        // OK on all targets Rust supports.
        let n = st.frame_scratch.len();
        let ptr = st.frame_scratch.as_ptr() as *const i32;
        let view: &[i32] = unsafe { std::slice::from_raw_parts(ptr, n) };
        match env.set_int_array_region(&out, 0, view) {
            Ok(()) => JNI_TRUE,
            Err(e) => { log::error!("set_int_array_region: {e}"); JNI_FALSE }
        }
    })
}

/// **Preferred (v4-audio)**: copy the most recent framebuffer into
/// `out` already re-packed to Android's `ARGB_8888` (`0xAARRGGBB`).
///
/// This variant exists because the previous Kotlin loop repacking
/// 71 680 pixels per frame was measurable on mid-range Android
/// devices (~2–8 ms depending on JIT state). Doing the same work in
/// release-mode Rust takes <100 µs and gets fully auto-vectorised.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeGetFramebufferArgb<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    out: JIntArray<'a>,
) -> jboolean {
    with_state(JNI_FALSE, |st| {
        let frame = st.system.render_frame_pixels();
        // Rust output: 0xRRGGBBAA. Android ARGB_8888 wants: 0xAARRGGBB.
        //   px_argb = ((px_rgba >> 8) & 0x00FFFFFF) | ((px_rgba & 0xFF) << 24)
        // The renderer never emits partial-alpha pixels (backdrop and
        // sprites are always fully opaque), so `alpha_byte` is 0xFF and
        // we could hard-code it, but keeping the byte round-trip means
        // this stays honest if the renderer ever grows real alpha.
        for (dst, &src) in st.frame_scratch.iter_mut().zip(frame.iter()) {
            let rgba = src;
            let alpha = rgba & 0xFF;
            let rgb   = rgba >> 8;
            *dst = (alpha << 24) | rgb;
        }
        let n = st.frame_scratch.len();
        let ptr = st.frame_scratch.as_ptr() as *const i32;
        let view: &[i32] = unsafe { std::slice::from_raw_parts(ptr, n) };
        match env.set_int_array_region(&out, 0, view) {
            Ok(()) => JNI_TRUE,
            Err(e) => { log::error!("set_int_array_region: {e}"); JNI_FALSE }
        }
    })
}

/// Drain freshly-produced YM2610 audio samples into `out`.
/// `out.length` must be even (stereo, interleaved L/R).
/// Returns the number of `i16` samples actually written (≤ out.length).
///
/// **Legacy path**: only used when the app falls back to the Kotlin
/// `AudioTrack` engine (API 24–25, or an AAudio failure). On API 26+
/// the AAudio driver in [`aaudio_stream`] pulls samples directly from
/// the emulator via [`nativeRunFrame`] → `push_audio_to_ring`; Kotlin
/// never calls this any more.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeDrainAudio<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    out: JShortArray<'a>,
) -> jint {
    let capacity = match env.get_array_length(&out) {
        Ok(n) => n as usize,
        Err(_) => return 0,
    };
    with_state(0, |st| {
        let want = capacity.min(st.system.audio_buffer.len());
        if want == 0 { return 0; }
        let chunk: Vec<i16> = st.system.audio_buffer.drain(..want).collect();
        match env.set_short_array_region(&out, 0, &chunk) {
            Ok(()) => want as jint,
            Err(e) => { log::error!("set_short_array_region: {e}"); 0 }
        }
    })
}

// ---------------------------------------------------------------------------
//   Native AAudio driver — the preferred audio path on API 26+.
//
//   Kotlin flow:
//     onResume  → nativeAudioStart(48000)  → returns true if AAudio came up
//     each frame→ the driver pulls from the ring; Kotlin does nothing
//     onPause   → nativeAudioStop()
//
//   When AAudio start() returns false (API 24–25, or vendor HAL error),
//   the Kotlin side keeps its old AudioTrack engine alive as a fallback.
// ---------------------------------------------------------------------------

/// Attempt to start the AAudio LOW_LATENCY output stream. Returns true
/// on success (Kotlin should then leave audio to us and stop pumping
/// AudioTrack); returns false on any failure (Kotlin should keep its
/// AudioTrack path running as a fallback).
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeAudioStart(
    _env: JNIEnv, _class: JClass, sample_rate: jint,
) -> jboolean {
    if aaudio_stream::start(sample_rate) { JNI_TRUE } else { JNI_FALSE }
}

/// Stop and release the AAudio stream. Safe to call more than once.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeAudioStop(
    _env: JNIEnv, _class: JClass,
) {
    aaudio_stream::stop();
}

/// True when the AAudio driver is actively pulling from the ring.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeAudioIsRunning(
    _env: JNIEnv, _class: JClass,
) -> jboolean {
    if aaudio_stream::is_running() { JNI_TRUE } else { JNI_FALSE }
}

/// Diagnostics: [underruns, xrun_count, actual_sr, frames_per_burst,
/// perf_mode] as a jint array. All zero if the driver isn't running.
/// Kotlin uses this to show the audio status in the Ajustes tab and to
/// grow the AAudio buffer if xruns keep climbing (LatencyTuner-lite).
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeAudioStats<'a>(
    env: JNIEnv<'a>, _class: JClass<'a>, out: JIntArray<'a>,
) -> jboolean {
    let (u, x, sr, fpb, perf) = aaudio_stream::stats();
    let arr: [i32; 5] = [u as i32, x, sr, fpb, perf];
    match env.set_int_array_region(&out, 0, &arr) {
        Ok(()) => JNI_TRUE,
        Err(e) => { log::error!("nativeAudioStats: {e}"); JNI_FALSE }
    }
}

// ---------------------------------------------------------------------------
//   Input handling
// ---------------------------------------------------------------------------

fn apply_player_mask(mask: u32) -> u8 {
    let mut p: u8 = 0xFF;
    if (mask & BTN_UP    as u32) != 0 { p &= !DIR_UP; }
    if (mask & BTN_DOWN  as u32) != 0 { p &= !DIR_DOWN; }
    if (mask & BTN_LEFT  as u32) != 0 { p &= !DIR_LEFT; }
    if (mask & BTN_RIGHT as u32) != 0 { p &= !DIR_RIGHT; }
    if (mask & BTN_A     as u32) != 0 { p &= !BTN_A_HW; }
    if (mask & BTN_B     as u32) != 0 { p &= !BTN_B_HW; }
    if (mask & BTN_C     as u32) != 0 { p &= !BTN_C_HW; }
    if (mask & BTN_D     as u32) != 0 { p &= !BTN_D_HW; }
    p
}

/// Set both P1 and P2 states from two independent bitmasks of `BTN_*`
/// flags. Real hardware uses ACTIVE-LOW lines: setting a flag means
/// "button pressed", which in turn clears the corresponding bit in the
/// bus port.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeSetPlayerInputs(
    _env: JNIEnv,
    _class: JClass,
    p1_mask: jint,
    p2_mask: jint,
) {
    with_state((), |st| {
        let p1m = p1_mask as u32;
        let p2m = p2_mask as u32;
        let mut start_select: u8 = 0xFF;
        let mut coin: u8 = 0xFF;

        if (p1m & BTN_START  as u32) != 0 { start_select &= !START_P1; }
        if (p1m & BTN_SELECT as u32) != 0 { start_select &= !SELECT_P1; }
        if (p1m & BTN_COIN   as u32) != 0 { coin &= !COIN_1; }

        if (p2m & BTN_START  as u32) != 0 { start_select &= !START_P2; }
        if (p2m & BTN_SELECT as u32) != 0 { start_select &= !SELECT_P2; }
        if (p2m & BTN_COIN   as u32) != 0 { coin &= !COIN_2; }

        st.system.bus.p1_input = apply_player_mask(p1m);
        st.system.bus.p2_input = apply_player_mask(p2m);
        st.system.bus.start_select = start_select;
        st.system.bus.coin_inputs = coin;
    });
}

/// Backwards-compatible single-player setter used by older builds of the
/// Kotlin frontend. Delegates to `nativeSetPlayerInputs(mask, 0)`.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeSetInputs(
    env: JNIEnv,
    class: JClass,
    mask: jint,
) {
    Java_com_pydmg_neogeo_NativeBridge_nativeSetPlayerInputs(env, class, mask, 0);
}

// ---------------------------------------------------------------------------
//   Netplay support: deterministic checksum + frame counter
// ---------------------------------------------------------------------------
//
// For LAN netplay we need two things from the core:
//
//   * A **frame counter** the two peers can synchronise on. `dbg_frame`
//     is already exposed and monotonically increments once per
//     `run_frame()`, which is exactly what we need.
//   * A **deterministic checksum** of the emulator state so both peers
//     can compare and detect a desync. We hash the 64 KiB of 68K work
//     RAM (`bus.work_ram`) — that's the region games actually mutate
//     each frame; it captures score, positions, RNG seeds, and every
//     other piece of authoritative game state. The palette RAM and
//     VRAM are downstream (they're derived from work RAM), so hashing
//     work RAM alone is sufficient for desync detection and avoids the
//     cost of hashing hundreds of KiB per second.
//
// The checksum is CRC-32 (IEEE 802.3 polynomial, 0xEDB88320 reflected).
// It's not a cryptographic hash, but it's fast (~50 MB/s per core
// with the naive byte-at-a-time loop below, plenty for 64 KiB per
// keyframe), and its 32-bit width makes accidental collisions
// astronomically unlikely for this use case.
// ---------------------------------------------------------------------------

/// Precomputed CRC-32 table (IEEE 802.3, reflected). Computed at
/// program start via a `Lazy`. Avoids a build-time script.
static CRC32_TABLE: Lazy<[u32; 256]> = Lazy::new(|| {
    let mut t = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        t[i as usize] = c;
    }
    t
});

fn crc32(bytes: &[u8]) -> u32 {
    let table = &*CRC32_TABLE;
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// Return the current emulated frame counter (increments by 1 per
/// `nativeRunFrame`). Wraps at u32::MAX after ~2 years of continuous
/// play, which we don't care about.
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeFrameCounter(
    _env: JNIEnv, _class: JClass,
) -> jint {
    with_state(0, |st| st.system.dbg_frame as jint)
}

/// Return a deterministic 32-bit checksum of the 68K work RAM.
/// Used by the netplay layer to periodically verify both peers are
/// still in sync. Any divergence in gameplay-relevant state (score,
/// object positions, RNG, timers, palette bank selection, …) writes
/// to work RAM in the next few frames → the CRC drifts → we detect
/// the desync within one keyframe interval (default 60 frames = 1 s).
#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeStateChecksum(
    _env: JNIEnv, _class: JClass,
) -> jint {
    with_state(0, |st| crc32(&*st.system.bus.work_ram) as jint)
}

// ---------------------------------------------------------------------------
//   Static metadata exposed to Kotlin
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeScreenWidth(
    _env: JNIEnv, _class: JClass,
) -> jint { SCREEN_W as jint }

#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeScreenHeight(
    _env: JNIEnv, _class: JClass,
) -> jint { SCREEN_H as jint }

#[no_mangle]
pub extern "system" fn Java_com_pydmg_neogeo_NativeBridge_nativeAudioSampleRate(
    _env: JNIEnv, _class: JClass,
) -> jint { HOST_AUDIO_HZ as jint }

//! Native AAudio playback stream for the pydmg-neogeo Android port.
//!
//! ## Why we own the audio device from Rust, not from Kotlin
//!
//! The v3.2 audio path was:
//!
//! ```text
//!   emu thread ─► nativeDrainAudio (JNI copy)
//!               ─► short[] scratch
//!               ─► AudioTrack.write(WRITE_BLOCKING)   ← blocks emu thread
//!               ─► AudioFlinger normal mixer          ← ~20 ms of buffer
//!               ─► DAC
//! ```
//!
//! Google's official low-latency guide
//! (<https://developer.android.com/games/sdk/oboe/low-latency-audio>) lists
//! **six** things that path gets wrong:
//!
//!   1. It doesn't use AAudio (via Oboe or directly). We were on the
//!      Java `AudioTrack` API which is 2–3× the round-trip latency of a
//!      properly-configured AAudio LOW_LATENCY + EXCLUSIVE stream.
//!   2. It didn't request LOW_LATENCY performance mode.
//!   3. It didn't request EXCLUSIVE sharing mode.
//!   4. It matched the device rate for sample-rate conversion (✅
//!      already correct — Kaiser resampler emits 48 kHz).
//!   5. It didn't declare its use case (`USAGE_GAME` was set — ✅ ok).
//!   6. **It used blocking writes from the emulator thread.** This is
//!      the killer. The official guide says explicitly:
//!
//!         > *"Avoid blocking in the callback. When you use a low
//!         >  latency stream, the time between callbacks can be very
//!         >  short, just a few milliseconds. Blocking in the callback
//!         >  will cause underruns."*
//!
//!      and
//!
//!         > *"The main advantage of a callback function is that it can
//!         >  be scheduled with special optimizations by the audio
//!         >  library to achieve fast and reliable performance."*
//!
//! ## What we do instead
//!
//! ```text
//!   AAudio callback thread (SCHED_FIFO, tiny buffer)
//!       │
//!       ▼
//!   `data_callback` pulls N stereo frames straight out of a
//!   ring buffer that the emulator thread fills every ~16 ms.
//!       │
//!       ▼
//!   Zero JNI hops. Zero Java locks. Zero allocations. Zero blocking.
//! ```
//!
//! The emulator thread's job becomes:
//!   * `run_frame()` → produces ~800 stereo pairs
//!   * `push_to_ring(...)`  ← wait-free SPSC push
//!
//! The AAudio callback drains what it needs; if the ring runs dry it
//! outputs the last known sample repeated (better than a click) and
//! bumps an `underrun_count` we expose to Kotlin for diagnostics.
//!
//! ## Latency math
//!
//! With `frames_per_burst` = 96 (typical Pixel/S23) and `capacity` = 4 ×
//! burst = 384 frames at 48 kHz stereo:
//!
//!   * Ring buffer holds ≤ 8 ms of audio.
//!   * AAudio HAL adds ≤ 5 ms on modern SoCs.
//!   * Total round-trip ≈ **10–15 ms**, down from ~40 ms on the
//!     `AudioTrack` WRITE_BLOCKING path.
//!
//! ## Portability
//!
//! AAudio is API 26+ (Android 8.0). We keep `minSdk = 24` in the
//! Gradle project, so on API 24–25 we fall back to the previous
//! `AudioTrack` path automatically (Kotlin side checks
//! `nativeAudioBackend()`). This is what Oboe does internally.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::cell::UnsafeCell;
use std::os::raw::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;

// -----------------------------------------------------------------------
//   Minimal AAudio bindings.
//
//   ndk-sys 0.5 already ships bindings but they are behind a feature
//   flag we don't want to depend on across NDK versions. Copying the 5
//   symbols and 4 enums we actually use keeps the module self-contained
//   and drops one build-time surprise vector.
// -----------------------------------------------------------------------

pub type aaudio_stream_state_t = i32;
pub type aaudio_result_t = i32;
pub type aaudio_data_callback_result_t = i32;
pub type aaudio_direction_t = i32;
pub type aaudio_format_t = i32;
pub type aaudio_performance_mode_t = i32;
pub type aaudio_sharing_mode_t = i32;
pub type aaudio_usage_t = i32;
pub type aaudio_content_type_t = i32;

// Result codes
pub const AAUDIO_OK: aaudio_result_t = 0;

// Format
pub const AAUDIO_FORMAT_PCM_I16: aaudio_format_t = 1;
pub const AAUDIO_FORMAT_PCM_FLOAT: aaudio_format_t = 2;

// Direction
pub const AAUDIO_DIRECTION_OUTPUT: aaudio_direction_t = 0;

// Performance
pub const AAUDIO_PERFORMANCE_MODE_NONE: aaudio_performance_mode_t = 10;
pub const AAUDIO_PERFORMANCE_MODE_POWER_SAVING: aaudio_performance_mode_t = 11;
pub const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: aaudio_performance_mode_t = 12;

// Sharing
pub const AAUDIO_SHARING_MODE_EXCLUSIVE: aaudio_sharing_mode_t = 0;
pub const AAUDIO_SHARING_MODE_SHARED: aaudio_sharing_mode_t = 1;

// Usage / content type — matches AudioAttributes
pub const AAUDIO_USAGE_GAME: aaudio_usage_t = 14;
pub const AAUDIO_CONTENT_TYPE_MUSIC: aaudio_content_type_t = 2;

// Data callback result
pub const AAUDIO_CALLBACK_RESULT_CONTINUE: aaudio_data_callback_result_t = 0;
pub const AAUDIO_CALLBACK_RESULT_STOP: aaudio_data_callback_result_t = 1;

// Opaque types
#[repr(C)] pub struct AAudioStreamBuilder { _priv: [u8; 0] }
#[repr(C)] pub struct AAudioStream        { _priv: [u8; 0] }

pub type AAudioStream_dataCallback = unsafe extern "C" fn(
    stream: *mut AAudioStream,
    user_data: *mut c_void,
    audio_data: *mut c_void,
    num_frames: i32,
) -> aaudio_data_callback_result_t;

// ------------------------------------------------------------------
//   Runtime-loaded AAudio bindings.
//
//   We deliberately do NOT `#[link(name = "aaudio")]` because that
//   requires the sysroot to have `libaaudio.so`, which only exists at
//   API level 26+ in the NDK sysroot layout. cargo-ndk defaults the
//   platform level to 21 (matches our Gradle `minSdk = 24`), so a
//   compile-time link would fail with
//     `ld.lld: error: unable to find library -laaudio`.
//
//   Oboe solves this the same way: it `dlopen`s libaaudio.so at first
//   use and resolves each function pointer with `dlsym`. If any symbol
//   is missing (device running Android ≤ 7.1) we set the whole table
//   to null and `start()` returns false, at which point Kotlin falls
//   back to the classic AudioTrack engine.
//
//   The result is:
//     * `libpydmg_neogeo_jni.so` compiles cleanly against any NDK
//       platform level, including 21 (cargo-ndk's default) and 24
//       (our Gradle minSdk).
//     * `libaaudio.so` is only touched on API 26+ devices, and its
//       absence is a graceful runtime fallback rather than a startup
//       crash.
// ------------------------------------------------------------------

type Fn_createStreamBuilder = unsafe extern "C" fn(*mut *mut AAudioStreamBuilder) -> aaudio_result_t;
type Fn_builder_delete       = unsafe extern "C" fn(*mut AAudioStreamBuilder) -> aaudio_result_t;
type Fn_builder_setI32       = unsafe extern "C" fn(*mut AAudioStreamBuilder, i32);
type Fn_builder_setCb        = unsafe extern "C" fn(*mut AAudioStreamBuilder, Option<AAudioStream_dataCallback>, *mut c_void);
type Fn_builder_openStream   = unsafe extern "C" fn(*mut AAudioStreamBuilder, *mut *mut AAudioStream) -> aaudio_result_t;
type Fn_stream_action        = unsafe extern "C" fn(*mut AAudioStream) -> aaudio_result_t;
type Fn_stream_getI32        = unsafe extern "C" fn(*mut AAudioStream) -> i32;
type Fn_stream_setBufferSize = unsafe extern "C" fn(*mut AAudioStream, i32) -> i32;

/// All the AAudio entry points we use, loaded via `dlsym`. `None` on
/// any device where AAudio isn't available (API ≤ 25, or the vendor
/// stripped libaaudio.so).
struct AAudioApi {
    createStreamBuilder:            Fn_createStreamBuilder,
    builder_delete:                 Fn_builder_delete,
    builder_setDirection:           Fn_builder_setI32,
    builder_setSampleRate:          Fn_builder_setI32,
    builder_setChannelCount:        Fn_builder_setI32,
    builder_setFormat:              Fn_builder_setI32,
    builder_setPerformanceMode:     Fn_builder_setI32,
    builder_setSharingMode:         Fn_builder_setI32,
    builder_setUsage:               Fn_builder_setI32,
    builder_setContentType:         Fn_builder_setI32,
    builder_setDataCallback:        Fn_builder_setCb,
    builder_openStream:             Fn_builder_openStream,
    stream_requestStart:            Fn_stream_action,
    stream_requestStop:             Fn_stream_action,
    stream_close:                   Fn_stream_action,
    stream_getFramesPerBurst:       Fn_stream_getI32,
    stream_setBufferSizeInFrames:   Fn_stream_setBufferSize,
    stream_getXRunCount:            Fn_stream_getI32,
    stream_getSampleRate:           Fn_stream_getI32,
    stream_getPerformanceMode:      Fn_stream_getI32,
    stream_getSharingMode:          Fn_stream_getI32,
}

// Minimal libdl signatures. `libdl.so` is guaranteed to be present on
// every Android version we care about (it's the loader itself).
#[cfg(target_os = "android")]
extern "C" {
    fn dlopen(filename: *const u8, flag: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}

#[cfg(target_os = "android")]
const RTLD_NOW: i32 = 2;

/// Try to `dlopen("libaaudio.so")` and resolve every symbol we need.
/// Returns `None` if libaaudio is unavailable OR any symbol is missing
/// (defensive: partial installs would break the whole audio path).
#[cfg(target_os = "android")]
unsafe fn load_aaudio_api() -> Option<AAudioApi> {
    // Null-terminated C strings.
    let handle = dlopen(b"libaaudio.so\0".as_ptr(), RTLD_NOW);
    if handle.is_null() { return None; }

    macro_rules! sym {
        ($h:expr, $name:literal, $ty:ty) => {{
            let mut buf = [0u8; 128];
            let bytes = $name.as_bytes();
            buf[..bytes.len()].copy_from_slice(bytes);
            // trailing 0 already there.
            let p = dlsym($h, buf.as_ptr());
            if p.is_null() { dlclose($h); return None; }
            std::mem::transmute::<*mut c_void, $ty>(p)
        }};
    }

    let api = AAudioApi {
        createStreamBuilder:          sym!(handle, "AAudio_createStreamBuilder", Fn_createStreamBuilder),
        builder_delete:               sym!(handle, "AAudioStreamBuilder_delete", Fn_builder_delete),
        builder_setDirection:         sym!(handle, "AAudioStreamBuilder_setDirection", Fn_builder_setI32),
        builder_setSampleRate:        sym!(handle, "AAudioStreamBuilder_setSampleRate", Fn_builder_setI32),
        builder_setChannelCount:      sym!(handle, "AAudioStreamBuilder_setChannelCount", Fn_builder_setI32),
        builder_setFormat:            sym!(handle, "AAudioStreamBuilder_setFormat", Fn_builder_setI32),
        builder_setPerformanceMode:   sym!(handle, "AAudioStreamBuilder_setPerformanceMode", Fn_builder_setI32),
        builder_setSharingMode:       sym!(handle, "AAudioStreamBuilder_setSharingMode", Fn_builder_setI32),
        builder_setUsage:             sym!(handle, "AAudioStreamBuilder_setUsage", Fn_builder_setI32),
        builder_setContentType:       sym!(handle, "AAudioStreamBuilder_setContentType", Fn_builder_setI32),
        builder_setDataCallback:      sym!(handle, "AAudioStreamBuilder_setDataCallback", Fn_builder_setCb),
        builder_openStream:           sym!(handle, "AAudioStreamBuilder_openStream", Fn_builder_openStream),
        stream_requestStart:          sym!(handle, "AAudioStream_requestStart", Fn_stream_action),
        stream_requestStop:           sym!(handle, "AAudioStream_requestStop", Fn_stream_action),
        stream_close:                 sym!(handle, "AAudioStream_close", Fn_stream_action),
        stream_getFramesPerBurst:     sym!(handle, "AAudioStream_getFramesPerBurst", Fn_stream_getI32),
        stream_setBufferSizeInFrames: sym!(handle, "AAudioStream_setBufferSizeInFrames", Fn_stream_setBufferSize),
        stream_getXRunCount:          sym!(handle, "AAudioStream_getXRunCount", Fn_stream_getI32),
        stream_getSampleRate:         sym!(handle, "AAudioStream_getSampleRate", Fn_stream_getI32),
        stream_getPerformanceMode:    sym!(handle, "AAudioStream_getPerformanceMode", Fn_stream_getI32),
        stream_getSharingMode:        sym!(handle, "AAudioStream_getSharingMode", Fn_stream_getI32),
    };
    // We intentionally leak `handle`. The AAudio lib stays loaded for
    // the lifetime of the process, which is what we want anyway.
    Some(api)
}

#[cfg(not(target_os = "android"))]
unsafe fn load_aaudio_api() -> Option<AAudioApi> { None }

/// Cached API pointers. `Lazy` means we only pay the dlopen/dlsym cost
/// once, on the first `start()`. After that all calls are pure indirect
/// function calls, same as if we had `#[link]`ed.
static AAUDIO: Lazy<Option<AAudioApi>> = Lazy::new(|| unsafe { load_aaudio_api() });

// -----------------------------------------------------------------------
//   Wait-free SPSC ring buffer for interleaved i16 stereo samples.
//
//   Producer: emu thread, calls `push_stereo`.
//   Consumer: AAudio callback thread, calls `pop_frames_into`.
//
//   Capacity is fixed at construction; a power-of-two size lets us
//   mask instead of modulo. We store one atomic head + one atomic tail
//   and rely on Release/Acquire pairs so the reader always sees a
//   consistent snapshot without any locks.
// -----------------------------------------------------------------------

/// Wait-free SPSC ring for interleaved stereo i16 samples.
///
/// Uses `UnsafeCell` so both `push_stereo` and `pop_frames_into` can
/// take `&self`, which is what we need to share a `&'static StereoRing`
/// between the emu thread and the AAudio callback without any lock.
///
/// Safety contract:
///   * Exactly **one** producer thread ever calls `push_stereo` at a
///     time. In this codebase that is always the emu thread (or the
///     JNI trampoline for `nativeRunFrame`, which is called only from
///     the emu thread).
///   * Exactly **one** consumer thread ever calls `pop_frames_into`.
///     Here that is always the AAudio HAL callback.
///   * The two threads may be different but they never interleave on
///     the same role. The atomic head/tail with Acquire/Release fences
///     is what gives SPSC memory-visibility guarantees.
pub struct StereoRing {
    /// Interleaved L,R,L,R,... i16 samples. Length = cap_frames * 2.
    /// UnsafeCell because SPSC access is safe by construction — the
    /// producer only touches slots `tail..head`, the consumer only
    /// touches `head..tail`, and they never overlap.
    buf: UnsafeCell<Box<[i16]>>,
    cap_frames: usize,
    mask: usize,
    head: AtomicU32,
    tail: AtomicU32,
}

// SPSC ring is safe to share by construction (see doc above).
unsafe impl Sync for StereoRing {}
unsafe impl Send for StereoRing {}

impl StereoRing {
    pub fn new(cap_frames: usize) -> Self {
        assert!(cap_frames.is_power_of_two(), "capacity must be power of two");
        Self {
            buf: UnsafeCell::new(vec![0i16; cap_frames * 2].into_boxed_slice()),
            cap_frames,
            mask: cap_frames - 1,
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
        }
    }

    /// Push as many stereo frames as fit into the ring without
    /// overwriting unread samples. Returns the number of frames stored.
    /// **Producer-side only.**
    pub fn push_stereo(&self, src: &[i16]) -> usize {
        debug_assert!(src.len() % 2 == 0);
        let head = self.head.load(Ordering::Relaxed) as usize;
        let tail = self.tail.load(Ordering::Acquire) as usize;
        let used = head.wrapping_sub(tail);
        let free = self.cap_frames - used;
        let want = src.len() / 2;
        let n = want.min(free);
        // SAFETY: producer-exclusive access to slots [head..head+n).
        let buf: &mut [i16] = unsafe { &mut *self.buf.get() };
        for i in 0..n {
            let pos = (head.wrapping_add(i)) & self.mask;
            buf[pos * 2]     = src[i * 2];
            buf[pos * 2 + 1] = src[i * 2 + 1];
        }
        self.head.store((head.wrapping_add(n)) as u32, Ordering::Release);
        n
    }

    /// Drain up to `frames` stereo frames into `dst` (must have space
    /// for `frames * 2` i16 elements). Underflow is zero-filled so
    /// the DAC gets silence instead of stale data / a click.
    /// **Consumer-side only.**
    pub fn pop_frames_into(&self, dst: &mut [i16], frames: usize) -> usize {
        let head = self.head.load(Ordering::Acquire) as usize;
        let tail = self.tail.load(Ordering::Relaxed) as usize;
        let avail = head.wrapping_sub(tail);
        let n = avail.min(frames);
        // SAFETY: consumer-exclusive access to slots [tail..tail+n).
        let buf: &[i16] = unsafe { &*self.buf.get() };
        for i in 0..n {
            let pos = (tail.wrapping_add(i)) & self.mask;
            dst[i * 2]     = buf[pos * 2];
            dst[i * 2 + 1] = buf[pos * 2 + 1];
        }
        for i in n..frames {
            dst[i * 2]     = 0;
            dst[i * 2 + 1] = 0;
        }
        self.tail.store((tail.wrapping_add(n)) as u32, Ordering::Release);
        n
    }

    pub fn used_frames(&self) -> usize {
        let head = self.head.load(Ordering::Acquire) as usize;
        let tail = self.tail.load(Ordering::Acquire) as usize;
        head.wrapping_sub(tail)
    }
}

// -----------------------------------------------------------------------
//   Global driver singleton.
//
//   Only one output stream at a time (single emulator instance). The
//   ring is Arc'd behind a Mutex so start/stop lifetime is safe, but
//   the hot path (push / pop) never touches the mutex — the emu and
//   callback threads share the same `&'static StereoRing` reference.
// -----------------------------------------------------------------------

pub struct AAudioDriver {
    stream: *mut AAudioStream,
    /// Backing store lives forever once created so pointers passed to
    /// the AAudio callback remain valid until process exit. We accept
    /// this small (~48 KiB) leak; the driver is created once per app
    /// launch.
    ring: &'static StereoRing,
    running: AtomicBool,
    sample_rate: i32,
    frames_per_burst: i32,
    perf_mode: aaudio_performance_mode_t,
    sharing_mode: aaudio_sharing_mode_t,
}

// AAudio streams are safe to hand between threads once opened; the
// callback runs on its own SCHED_FIFO thread and only reads shared
// state through the atomic ring. Mark explicitly.
unsafe impl Send for AAudioDriver {}
unsafe impl Sync for AAudioDriver {}

static DRIVER: Lazy<Mutex<Option<AAudioDriver>>> = Lazy::new(|| Mutex::new(None));

/// Global ring bound at first `start()`. Kept in a `OnceCell`-esque
/// pattern via `Lazy` so both threads see the same reference.
///
/// Capacity math (revised for v4-audio /55555 Hz path):
///   * At 55_555 Hz stereo, one emulated 60 Hz frame produces
///     55_555 / 60 ≈ 926 samples.
///   * The emu thread pushes those ~926 samples in one burst per
///     video frame (~16.6 ms cadence).
///   * The AAudio callback drains in ~2 ms bursts of ~110 samples.
///   * We want the ring to absorb one full emu-frame worth of samples
///     with headroom in case the emu thread runs late by up to one
///     frame. That's 2 × 926 ≈ 1852 frames. Rounded up to 2048.
///   * At 55_555 Hz, 2048 stereo frames = **~37 ms** of audio in
///     the pipeline. Combined with AAudio's HAL buffer (~10 ms on
///     modern devices) total round-trip stays under 50 ms while
///     still giving the emu thread ~one frame of slack.
static RING: Lazy<StereoRing> = Lazy::new(|| StereoRing::new(2048));

/// Total number of ring underruns since app start. Bumped by the
/// AAudio callback thread every time the ring didn't have all the
/// samples it asked for. Exposed to Kotlin via `nativeAudioStats`.
/// Global (not a driver field) so the callback can touch it without
/// any lock — pure Relaxed atomic RMW is as real-time-safe as it gets.
static UNDERRUNS: AtomicU32 = AtomicU32::new(0);

/// AAudio-side callback. Runs on the audio HAL thread with real-time
/// priority. **Do NOT allocate. Do NOT lock. Do NOT log.**
unsafe extern "C" fn data_cb(
    _stream: *mut AAudioStream,
    _user_data: *mut c_void,
    audio_data: *mut c_void,
    num_frames: i32,
) -> aaudio_data_callback_result_t {
    let frames = num_frames as usize;
    let out = std::slice::from_raw_parts_mut(audio_data as *mut i16, frames * 2);
    let got = RING.pop_frames_into(out, frames);
    if got < frames {
        // Underrun — the ring didn't have enough samples. Bump the
        // global counter (single atomic RMW, no lock, no allocation)
        // and return CONTINUE anyway; `pop_frames_into` already
        // zero-filled the tail so the DAC gets silence instead of a
        // click.
        UNDERRUNS.fetch_add(1, Ordering::Relaxed);
    }
    AAUDIO_CALLBACK_RESULT_CONTINUE
}

/// Build + configure + open an AAudio stream. Extracted so we can
/// call it twice (EXCLUSIVE first, SHARED as fallback) without
/// duplicating twenty lines of setter calls.
unsafe fn open_stream(
    api: &AAudioApi,
    sample_rate: i32,
    sharing: aaudio_sharing_mode_t,
) -> Option<*mut AAudioStream> {
    let mut builder: *mut AAudioStreamBuilder = ptr::null_mut();
    if (api.createStreamBuilder)(&mut builder) != AAUDIO_OK || builder.is_null() {
        return None;
    }
    (api.builder_setDirection)(builder, AAUDIO_DIRECTION_OUTPUT);
    (api.builder_setFormat)(builder, AAUDIO_FORMAT_PCM_I16);
    (api.builder_setChannelCount)(builder, 2);
    (api.builder_setSampleRate)(builder, sample_rate);
    (api.builder_setPerformanceMode)(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
    (api.builder_setSharingMode)(builder, sharing);
    (api.builder_setUsage)(builder, AAUDIO_USAGE_GAME);
    (api.builder_setContentType)(builder, AAUDIO_CONTENT_TYPE_MUSIC);
    // Callback-driven, not blocking-write.
    (api.builder_setDataCallback)(builder, Some(data_cb), ptr::null_mut());
    // Let the platform pick FramesPerDataCallback (== framesPerBurst).

    let mut stream: *mut AAudioStream = ptr::null_mut();
    let res = (api.builder_openStream)(builder, &mut stream);
    (api.builder_delete)(builder);
    if res != AAUDIO_OK || stream.is_null() {
        log::warn!("AAudio openStream (sharing={}) failed: {}", sharing, res);
        return None;
    }
    Some(stream)
}

impl AAudioDriver {
    /// Open an output stream at the requested sample rate. Returns
    /// `None` on any AAudio failure — callers should then fall back to
    /// the Kotlin `AudioTrack` path.
    pub fn start(sample_rate: i32) -> Option<Self> {
        // libaaudio.so present? (Missing on API ≤ 25.)
        let api = match AAUDIO.as_ref() {
            Some(a) => a,
            None => { log::warn!("libaaudio.so not available on this device"); return None; }
        };
        unsafe {
            // Try EXCLUSIVE first (lowest latency, may be refused).
            let mut stream = open_stream(api, sample_rate, AAUDIO_SHARING_MODE_EXCLUSIVE);
            if stream.is_none() {
                // EXCLUSIVE may not be supported → retry SHARED once.
                // Same fallback ladder Oboe walks internally.
                log::warn!("AAudio EXCLUSIVE refused — retrying SHARED");
                stream = open_stream(api, sample_rate, AAUDIO_SHARING_MODE_SHARED);
            }
            let stream = match stream {
                Some(s) => s,
                None => { log::warn!("AAudio openStream failed on both sharing modes"); return None; }
            };

            let fpb = (api.stream_getFramesPerBurst)(stream);
            let sr = (api.stream_getSampleRate)(stream);
            let perf = (api.stream_getPerformanceMode)(stream);
            let share = (api.stream_getSharingMode)(stream);

            // Set the initial buffer size to 3× the burst. Google's
            // `LatencyTuner` starts here for output streams too; 2× is
            // the theoretical minimum but leaves zero slack for a
            // scheduler burp on mid-range hardware, which was one of
            // the two things producing audible clicks in v4-audio r1.
            // 3× keeps latency well under 20 ms and gives us a
            // comfortable margin.
            let buf_target = fpb * 3;
            let actual = (api.stream_setBufferSizeInFrames)(stream, buf_target);

            if (api.stream_requestStart)(stream) != AAUDIO_OK {
                log::warn!("AAudio requestStart failed");
                (api.stream_close)(stream);
                return None;
            }

            log::info!(
                "AAudio stream started: {} Hz, framesPerBurst={}, buffer={}, perf={}, sharing={}",
                sr, fpb, actual, perf, share,
            );

            // Force RING lazy init before returning so both threads
            // see the same instance.
            let _ = &*RING;

            Some(Self {
                stream,
                ring: &*RING,
                running: AtomicBool::new(true),
                sample_rate: sr,
                frames_per_burst: fpb,
                perf_mode: perf,
                sharing_mode: share,
            })
        }
    }

    pub fn push_samples(&self, interleaved_i16: &[i16]) -> usize {
        self.ring.push_stereo(interleaved_i16)
    }

    pub fn stop(&mut self) {
        if !self.running.swap(false, Ordering::AcqRel) { return; }
        if self.stream.is_null() { return; }
        if let Some(api) = AAUDIO.as_ref() {
            unsafe {
                (api.stream_requestStop)(self.stream);
                (api.stream_close)(self.stream);
            }
        }
        self.stream = ptr::null_mut();
    }

    pub fn underruns(&self) -> u32 { UNDERRUNS.load(Ordering::Relaxed) }
    pub fn xruns(&self) -> i32 {
        if self.stream.is_null() { return 0; }
        match AAUDIO.as_ref() {
            Some(api) => unsafe { (api.stream_getXRunCount)(self.stream) },
            None => 0,
        }
    }
    pub fn sample_rate(&self) -> i32 { self.sample_rate }
    pub fn frames_per_burst(&self) -> i32 { self.frames_per_burst }
    pub fn perf_mode(&self) -> i32 { self.perf_mode }
    pub fn sharing_mode(&self) -> i32 { self.sharing_mode }
}

impl Drop for AAudioDriver {
    fn drop(&mut self) {
        self.stop();
    }
}

// -----------------------------------------------------------------------
//   Public front door used by lib.rs JNI wrappers.
// -----------------------------------------------------------------------

pub fn start(sample_rate: i32) -> bool {
    let mut g = DRIVER.lock().expect("DRIVER mutex poisoned");
    if g.is_some() { return true; }  // idempotent
    match AAudioDriver::start(sample_rate) {
        Some(d) => { *g = Some(d); true }
        None => false,
    }
}

pub fn stop() {
    if let Ok(mut g) = DRIVER.lock() {
        if let Some(mut d) = g.take() { d.stop(); }
    }
}

pub fn push(samples: &[i16]) -> usize {
    // Even if the driver isn't running (fallback path), we still fill
    // the ring so a later `start()` picks up from the current state.
    RING.push_stereo(samples)
}

pub fn stats() -> (u32, i32, i32, i32, i32) {
    if let Ok(g) = DRIVER.lock() {
        if let Some(d) = g.as_ref() {
            return (d.underruns(), d.xruns(), d.sample_rate(), d.frames_per_burst(), d.perf_mode());
        }
    }
    (0, 0, 0, 0, 0)
}

pub fn is_running() -> bool {
    DRIVER.lock().ok().and_then(|g| g.as_ref().map(|d| d.running.load(Ordering::Acquire))).unwrap_or(false)
}

// ------------------------------------------------------------------
//   Tests — host-side, cover the ring only (AAudio itself is only
//   reachable on Android so we don't exercise it in cargo test).
// ------------------------------------------------------------------

#[cfg(test)]
mod ring_tests {
    use super::*;

    #[test]
    fn round_trip_preserves_samples() {
        let r = StereoRing::new(16);
        // 4 stereo frames = 8 i16s.
        let src = [1i16, -1, 2, -2, 3, -3, 4, -4];
        assert_eq!(r.push_stereo(&src), 4);
        let mut dst = [0i16; 8];
        assert_eq!(r.pop_frames_into(&mut dst, 4), 4);
        assert_eq!(dst, src);
    }

    #[test]
    fn wraparound_is_correct() {
        let r = StereoRing::new(4); // 4-frame ring, 8 i16 slots
        // Fill it once, drain, fill again — exercises head/tail past cap.
        let a = [10i16, 11, 20, 21, 30, 31, 40, 41];
        assert_eq!(r.push_stereo(&a), 4);
        let mut d1 = [0i16; 8];
        r.pop_frames_into(&mut d1, 4);
        assert_eq!(d1, a);
        // Second lap wraps head+tail past cap.
        let b = [50i16, 51, 60, 61, 70, 71, 80, 81];
        assert_eq!(r.push_stereo(&b), 4);
        let mut d2 = [0i16; 8];
        r.pop_frames_into(&mut d2, 4);
        assert_eq!(d2, b);
    }

    #[test]
    fn push_stops_when_full() {
        let r = StereoRing::new(4);
        // Ring holds 4 frames; try to push 6 in a single call.
        let src = [1i16, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        assert_eq!(r.push_stereo(&src), 4);
        assert_eq!(r.used_frames(), 4);
    }

    #[test]
    fn pop_underflow_is_zero_padded() {
        let r = StereoRing::new(8);
        // Ring is empty; ask for 4 frames.
        let mut d = [999i16; 8];
        assert_eq!(r.pop_frames_into(&mut d, 4), 0);
        assert_eq!(d, [0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn spsc_across_threads_preserves_order() {
        use std::sync::Arc;
        // Ring holds 1024 frames; producer sends 4000 with 16-frame
        // chunks so we exercise back-pressure (chunks bigger than free
        // space) and wraparound.
        let r = Arc::new(StereoRing::new(1024));
        let r_prod = Arc::clone(&r);
        let prod = std::thread::spawn(move || {
            let mut sent = 0usize;
            let mut v: i16 = 0;
            while sent < 4000 {
                // Build the chunk against a "virtual" counter so a
                // partial push doesn't lose or duplicate frames.
                let start = v;
                let mut chunk = [0i16; 32];
                for i in 0..16 {
                    let x = start.wrapping_add((i as i16) * 2);
                    chunk[i * 2]     = x;
                    chunk[i * 2 + 1] = x.wrapping_add(1);
                }
                let n = r_prod.push_stereo(&chunk);
                if n == 0 { std::thread::yield_now(); continue; }
                sent += n;
                v = start.wrapping_add((n as i16) * 2);
                if n < 16 { std::thread::yield_now(); }
            }
        });
        // Consumer: pop until we've got 4000 frames.
        let mut recv = 0usize;
        let mut expect: i16 = 0;
        let mut fails = 0;
        while recv < 4000 && fails < 10_000_000 {
            let mut chunk = [0i16; 32];
            let n = r.pop_frames_into(&mut chunk, 16);
            if n == 0 { fails += 1; std::thread::yield_now(); continue; }
            for i in 0..n {
                assert_eq!(chunk[i * 2],     expect,
                           "L @ frame {}", recv + i);
                assert_eq!(chunk[i * 2 + 1], expect.wrapping_add(1),
                           "R @ frame {}", recv + i);
                expect = expect.wrapping_add(2);
            }
            recv += n;
        }
        prod.join().unwrap();
        assert_eq!(recv, 4000, "consumer starved before producer finished");
    }
}

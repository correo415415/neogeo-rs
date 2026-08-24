package com.pydmg.neogeo

/**
 * Thin JNI shim between Kotlin and the Rust `pydmg-neogeo` core.
 *
 * Every `external fun` here is implemented as a `#[no_mangle]
 * extern "system" fn Java_com_pydmg_neogeo_NativeBridge_<name>` symbol
 * in `android-jni/src/lib.rs`. The native library is the cdylib
 * produced by `cargo ndk -t <abi> build --profile android-release -p
 * pydmg-neogeo-jni`.
 */
object NativeBridge {
    init {
        System.loadLibrary("pydmg_neogeo_jni")
    }

    // Lifecycle
    external fun nativeInitLogger()
    external fun nativeCreate(hardware: Int): Boolean
    external fun nativeDestroy()

    // ROMs
    external fun nativeLoadBiosZip(zipBytes: ByteArray): Boolean
    external fun nativeLoadCartZip(cartName: String, zipBytes: ByteArray): Boolean

    // Per-frame
    external fun nativeRunFrame(): Int
    /** Legacy: returns pixels as 0xRRGGBBAA. Kept for compat. */
    external fun nativeGetFramebuffer(out: IntArray): Boolean
    /** Preferred (v4-audio): returns pixels already in Android's
     *  native ARGB_8888 layout (0xAARRGGBB). Bitmap.setPixels can
     *  ingest the array verbatim, no Kotlin repacking loop. */
    external fun nativeGetFramebufferArgb(out: IntArray): Boolean
    /**
     * Legacy audio drain — only used by [AudioEngine]'s AudioTrack
     * fallback path when the native AAudio driver could not be brought
     * up (API 24–25 or a vendor HAL rejection). On API 26+ the native
     * driver pulls samples directly out of the emulator inside
     * [nativeRunFrame], so this function returns 0 there.
     */
    external fun nativeDrainAudio(out: ShortArray): Int

    // Native AAudio driver (LOW_LATENCY + EXCLUSIVE, API 26+).
    /** Start the native AAudio output stream. Returns true if AAudio
     *  came up successfully; false if the caller should fall back to
     *  the Kotlin AudioTrack engine. Idempotent. */
    external fun nativeAudioStart(sampleRate: Int): Boolean
    /** Stop and release the native AAudio stream. Safe to call twice. */
    external fun nativeAudioStop()
    /** True when AAudio is actively pulling from the ring buffer. */
    external fun nativeAudioIsRunning(): Boolean
    /** Fill `out` with [underruns, xruns, actualSr, framesPerBurst, perfMode].
     *  `out.length` must be ≥ 5. */
    external fun nativeAudioStats(out: IntArray): Boolean

    // Inputs
    external fun nativeSetPlayerInputs(p1Mask: Int, p2Mask: Int)
    // Backwards-compatible single-player JNI entry remains in Rust, but the
    // Kotlin app always calls the 2-player variant above.

    // Constants
    external fun nativeScreenWidth(): Int
    external fun nativeScreenHeight(): Int
    external fun nativeAudioSampleRate(): Int

    // Savestates (NGSS v1: cabecera + identidad de juego + payload, ~220 KiB).
    /** Serializa el estado completo; null si no hay emulador cargado. */
    external fun nativeSaveState(): ByteArray?
    /** Restaura un estado de [nativeSaveState]. Valida juego/versión en el
     *  core (carga transaccional: si falla, el estado previo se conserva). */
    external fun nativeLoadState(data: ByteArray): Boolean

    // Netplay support (LAN multiplayer, lockstep-with-delay).
    /** Monotonic frame counter, increments once per `nativeRunFrame`. */
    external fun nativeFrameCounter(): Int
    /** CRC-32 of the 68K work RAM. Used to detect desyncs between peers. */
    external fun nativeStateChecksum(): Int

    const val HW_MVS = 0
    const val HW_AES = 1

    // Bit flags shared by P1 and P2 masks.
    const val BTN_UP     = 1 shl 0
    const val BTN_DOWN   = 1 shl 1
    const val BTN_LEFT   = 1 shl 2
    const val BTN_RIGHT  = 1 shl 3
    const val BTN_A      = 1 shl 4
    const val BTN_B      = 1 shl 5
    const val BTN_C      = 1 shl 6
    const val BTN_D      = 1 shl 7
    const val BTN_START  = 1 shl 8
    const val BTN_SELECT = 1 shl 9
    const val BTN_COIN   = 1 shl 10

    const val DIR_MASK = BTN_UP or BTN_DOWN or BTN_LEFT or BTN_RIGHT
}

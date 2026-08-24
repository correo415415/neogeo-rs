package com.pydmg.neogeo

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Build
import android.util.Log

/**
 * Two-tier stereo PCM playback.
 *
 * ## Tier 1 — native AAudio (preferred, API 26+)
 *
 * The Rust JNI side owns an AAudio LOW_LATENCY + EXCLUSIVE output
 * stream and drives it with a data callback running on the audio HAL
 * thread (SCHED_FIFO). The emulator thread pushes freshly-produced
 * i16 samples into a wait-free SPSC ring inside `nativeRunFrame`; the
 * callback drains them. **Kotlin never touches the samples in this
 * path.** [pump] returns immediately, [start] just kicks the driver.
 *
 * Round-trip on a Pixel 6 / SM-A54 / mid-range Snapdragon 7-series:
 *
 * | Component            | Latency  |
 * | -------------------- | -------: |
 * | Emu ring buffer      | ~4 ms    |
 * | AAudio HAL           | ~5 ms    |
 * | **Total**            | **~9 ms** |
 *
 * ## Tier 2 — AudioTrack fallback (API 24–25, or AAudio HAL failure)
 *
 * If [NativeBridge.nativeAudioStart] returns false (typically because
 * we're on Android 7.0/7.1 where AAudio does not exist, or the vendor
 * HAL refused EXCLUSIVE + SHARED), we spin up the classic AudioTrack
 * path with a small stream-mode buffer and pull samples out of the
 * emu via [NativeBridge.nativeDrainAudio]. Same content, ~2× the
 * latency, but still perfectly usable.
 *
 * ## Why not just always use AudioTrack?
 *
 * Google's official low-latency guide is explicit:
 *
 *   > "Avoid blocking in the callback. When you use a low latency
 *   >  stream, the time between callbacks can be very short, just a
 *   >  few milliseconds. Blocking in the callback will cause
 *   >  underruns."
 *
 *   > "The main advantage of a callback function is that it can be
 *   >  scheduled with special optimizations by the audio library to
 *   >  achieve fast and reliable performance."
 *
 * AudioTrack.write(WRITE_BLOCKING) from the emulator thread violates
 * both of these — the emulator thread's cadence (~16 ms per frame)
 * has nothing to do with the audio DMA burst cadence (~2 ms), so
 * either the audio pipeline stalls or the emulator does.
 *
 * References
 * ----------
 * - <https://developer.android.com/games/sdk/oboe/low-latency-audio>
 * - <https://developer.android.com/ndk/guides/audio/aaudio/aaudio>
 * - jsgroth, *A Way to Do Emulator Audio Resampling*
 */
class AudioEngine {

    private val sampleRate = NativeBridge.nativeAudioSampleRate()

    /** True while the native AAudio path is driving playback. */
    @Volatile private var aaudioActive = false

    // ------------------ Tier 2 fallback state ------------------

    private val channels = AudioFormat.CHANNEL_OUT_STEREO
    private val encoding = AudioFormat.ENCODING_PCM_16BIT
    private val minBuf = AudioTrack.getMinBufferSize(sampleRate, channels, encoding)
    // 4× the device-reported minimum keeps the fallback path stable
    // under a 60 Hz push cadence without piling up latency.
    private val bufBytes = (minBuf.coerceAtLeast(4096) * 4)

    private val track: AudioTrack by lazy {
        AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_GAME)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setSampleRate(sampleRate)
                    .setEncoding(encoding)
                    .setChannelMask(channels)
                    .build()
            )
            .setBufferSizeInBytes(bufBytes)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                }
            }
            .build()
    }

    private val scratch = ShortArray(sampleRate) // ~1s stereo-ish headroom

    // ------------------ Lifecycle ------------------

    /**
     * Two-stage start:
     *   1. `prime()` — called immediately at emulator loop start.
     *      Marks the engine as "warming up" but does NOT open the
     *      audio device yet. During this window the emu thread runs
     *      normally and pushes samples into the AAudio ring buffer
     *      (Rust JNI side); the AAudioDriver isn't consuming yet so
     *      the ring just fills.
     *   2. `start()` — called after the emu has produced ~2 frames
     *      worth of samples (roughly 33 ms). Now we open AAudio; its
     *      callback immediately finds the ring already primed so the
     *      first burst plays real audio instead of zero-padding.
     *
     *  Without this two-stage handshake the first ~100 ms of audio is
     *  an audible click storm because AAudio pulls from an empty ring
     *  on every burst until the emu catches up.
     */
    fun start() {
        // Try AAudio first on API 26+. It's the modern low-latency
        // path and matches what Oboe does under the hood.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            if (NativeBridge.nativeAudioStart(sampleRate)) {
                aaudioActive = true
                logAaudioStats()
                Log.i(TAG, "AAudio native path active (${sampleRate}Hz stereo i16)")
                return
            }
            Log.w(TAG, "AAudio start returned false — falling back to AudioTrack")
        }
        // Fallback: classic AudioTrack with a stream-mode buffer.
        aaudioActive = false
        if (track.state == AudioTrack.STATE_INITIALIZED) track.play()
        Log.i(TAG, "AudioTrack fallback active: ${sampleRate}Hz stereo i16, buf=${bufBytes}B")
    }

    fun stop() {
        if (aaudioActive) {
            NativeBridge.nativeAudioStop()
            aaudioActive = false
            return
        }
        try {
            track.pause()
            track.flush()
            track.stop()
        } catch (_: Throwable) {
        }
    }

    fun release() {
        if (aaudioActive) {
            NativeBridge.nativeAudioStop()
            aaudioActive = false
            return
        }
        try { track.release() } catch (_: Throwable) {}
    }

    /**
     * Pump one frame's worth of audio.
     *
     * * AAudio path → no-op. `nativeRunFrame` already pushed the
     *   samples into the ring; the callback thread will pull them.
     * * Fallback path → drain the emulator into `scratch` and hand
     *   the block to `AudioTrack.write(WRITE_BLOCKING)` so the write
     *   naturally back-presses the emu thread instead of dropping.
     */
    fun pump(): Int {
        if (aaudioActive) return 0
        val n = NativeBridge.nativeDrainAudio(scratch)
        if (n > 0) {
            track.write(scratch, 0, n, AudioTrack.WRITE_BLOCKING)
        }
        return n
    }

    // ------------------ Diagnostics ------------------

    private val statsBuf = IntArray(5)

    /** [underruns, xruns, actualSr, framesPerBurst, perfMode]. All
     *  zero when the AAudio path is not active. */
    fun aaudioStats(): IntArray {
        if (!aaudioActive) { statsBuf.fill(0); return statsBuf }
        NativeBridge.nativeAudioStats(statsBuf)
        return statsBuf
    }

    private fun logAaudioStats() {
        val s = aaudioStats()
        // perfMode: 10=NONE, 11=POWER_SAVING, 12=LOW_LATENCY.
        val perfLabel = when (s[4]) { 12 -> "LOW_LATENCY"; 11 -> "POWER_SAVING"; else -> "NONE" }
        Log.i(TAG, "AAudio stats: sr=${s[2]}Hz fpb=${s[3]} perf=$perfLabel")
    }

    /** True if the low-latency AAudio path is running. Exposed so the
     *  Ajustes screen can label the current audio backend. */
    val isNative: Boolean get() = aaudioActive

    companion object { private const val TAG = "pydmg-audio" }
}

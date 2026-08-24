package com.pydmg.neogeo

import android.annotation.SuppressLint
import android.os.Bundle
import android.util.Log
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.widget.Button
import androidx.appcompat.app.AppCompatActivity
import android.os.Process
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.locks.LockSupport

/**
 * Landscape, fullscreen game activity. All emulation runs here on a
 * dedicated thread; this activity is finished when the user taps
 * "Salir al menú" so LibraryActivity (portrait) becomes visible again.
 */
class EmulatorActivity : AppCompatActivity() {

    companion object {
        private const val TAG = "EmulatorActivity"
    }

    private lateinit var emulatorView: EmulatorView
    private lateinit var audio: AudioEngine

    private lateinit var p1LeftControls: View
    private lateinit var p1Dpad: View
    private lateinit var p1Joystick: JoystickView
    private lateinit var p1Abcd: View

    private lateinit var pauseOverlay: View

    private val running = AtomicBoolean(false)
    private var emuThread: Thread? = null
    private val paused = AtomicBoolean(false)

    private val p1Mask = AtomicInteger(0)
    private val p2Mask = AtomicInteger(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        // Sustained performance mode: asks the SoC governor for a thermal
        // envelope it can hold indefinitely instead of boost-then-throttle.
        // For a long emulation session this yields far steadier frame times
        // than short bursts of max clocks (supported devices only).
        if (android.os.Build.VERSION.SDK_INT >= 24) {
            try { window.setSustainedPerformanceMode(true) } catch (_: Throwable) {}
        }
        setContentView(R.layout.activity_emulator)

        bindViews()
        wireHud()
        wireControls()
        applyControlPreferences()

        audio = AudioEngine()
    }

    override fun onResume() {
        super.onResume()
        hideSystemUI()
        applyControlPreferences()
        paused.set(false)
        pauseOverlay.visibility = View.GONE
        startEmulatorLoop()
    }

    override fun onPause() {
        stopEmulatorLoop()
        super.onPause()
    }

    override fun onDestroy() {
        stopEmulatorLoop()
        try { audio.release() } catch (_: Throwable) {}
        // Close the LAN netplay session (if any) so sockets are
        // released and NsdManager stops advertising the host. The
        // volatile reference on PydmgApp is cleared so the next game
        // launch starts fresh.
        try { PydmgApp.app.netSession?.close() } catch (_: Throwable) {}
        PydmgApp.app.netSession = null
        super.onDestroy()
    }

    @SuppressLint("MissingSuperCall")
    @Suppress("DEPRECATION")  // pre-API 33 fallback
    @Deprecated("Manual back handling kept for simplicity.")
    override fun onBackPressed() {
        if (pauseOverlay.visibility == View.VISIBLE) {
            hidePauseOverlay()
        } else {
            // First back press shows the pause overlay; a second one
            // exits via btn_back_to_library. We deliberately do NOT call
            // super.onBackPressed() here: we want the user to always pass
            // through the pause overlay before leaving the emulator.
            showPauseOverlay()
        }
    }

    // ---------- Pause overlay (animated) ----------

    private fun showPauseOverlay() {
        paused.set(true)
        pauseOverlay.alpha = 0f
        pauseOverlay.visibility = View.VISIBLE
        pauseOverlay.animate().alpha(1f).setDuration(160L).start()
    }

    private fun hidePauseOverlay() {
        pauseOverlay.animate().alpha(0f).setDuration(120L)
            .withEndAction {
                pauseOverlay.visibility = View.GONE
                pauseOverlay.alpha = 1f
                paused.set(false)
            }.start()
    }

    // ---------- Savestates ----------
    //
    // El fichero vive en el almacenamiento interno de la app:
    //   files/savestates/<set>.ngss   (un slot por juego)
    // Las llamadas JNI son seguras con el overlay visible: el bucle de
    // emulación está pausado (paused=true) y el mutex del lado Rust
    // serializa cualquier carrera residual. En partida LAN se bloquea:
    // cargar un estado solo en un peer rompería el lockstep.

    private fun stateFile(): java.io.File {
        val name = PydmgApp.prefs.lastCartName.ifEmpty { "unknown" }
        val dir = java.io.File(filesDir, "savestates")
        dir.mkdirs()
        return java.io.File(dir, "$name.ngss")
    }

    private fun toast(resId: Int) {
        android.widget.Toast.makeText(this, resId, android.widget.Toast.LENGTH_SHORT).show()
    }

    private fun doSaveState() {
        if (PydmgApp.app.netSession != null) {
            toast(R.string.state_netplay_blocked)
            return
        }
        val data = NativeBridge.nativeSaveState()
        if (data == null || data.isEmpty()) {
            toast(R.string.state_save_failed)
            return
        }
        try {
            // Escritura atómica: tmp + rename para no corromper el estado
            // previo si la app muere a mitad de escritura.
            val f = stateFile()
            val tmp = java.io.File(f.parentFile, f.name + ".tmp")
            tmp.writeBytes(data)
            if (!tmp.renameTo(f)) {
                f.delete()
                if (!tmp.renameTo(f)) throw java.io.IOException("rename failed")
            }
            Log.i(TAG, "savestate guardado: ${f.name} (${data.size} bytes)")
            toast(R.string.state_saved)
        } catch (e: Exception) {
            Log.e(TAG, "savestate write failed", e)
            toast(R.string.state_save_failed)
        }
    }

    private fun doLoadState() {
        if (PydmgApp.app.netSession != null) {
            toast(R.string.state_netplay_blocked)
            return
        }
        val f = stateFile()
        if (!f.exists()) {
            toast(R.string.state_none)
            return
        }
        val ok = try {
            NativeBridge.nativeLoadState(f.readBytes())
        } catch (e: Exception) {
            Log.e(TAG, "savestate read failed", e)
            false
        }
        if (ok) {
            toast(R.string.state_loaded)
            hidePauseOverlay()
        } else {
            toast(R.string.state_load_failed)
        }
    }

    // ---------- Binding ----------

    private fun bindViews() {
        emulatorView = findViewById(R.id.emulator_view)
        p1LeftControls = findViewById(R.id.p1_left_controls)
        p1Dpad = findViewById(R.id.dpad_container)
        p1Joystick = findViewById(R.id.joystick_p1)
        p1Abcd = findViewById(R.id.abcd_pad)
        pauseOverlay = findViewById(R.id.pause_overlay)
    }

    private fun wireHud() {
        findViewById<Button>(R.id.btn_hud_menu).setOnClickListener {
            showPauseOverlay()
        }
        findViewById<Button>(R.id.btn_resume).setOnClickListener {
            hidePauseOverlay()
        }
        findViewById<Button>(R.id.btn_back_to_library).setOnClickListener {
            finish()
        }
        findViewById<Button>(R.id.btn_save_state).setOnClickListener { doSaveState() }
        findViewById<Button>(R.id.btn_load_state).setOnClickListener { doLoadState() }

        bindTouch(R.id.btn_coin_p1,   1, NativeBridge.BTN_COIN)
        bindTouch(R.id.btn_select_p1, 1, NativeBridge.BTN_SELECT)
        bindTouch(R.id.btn_start_p1,  1, NativeBridge.BTN_START)
    }

    private fun wireControls() {
        // P1 D-pad
        bindTouch(R.id.dpad_up,    1, NativeBridge.BTN_UP)
        bindTouch(R.id.dpad_down,  1, NativeBridge.BTN_DOWN)
        bindTouch(R.id.dpad_left,  1, NativeBridge.BTN_LEFT)
        bindTouch(R.id.dpad_right, 1, NativeBridge.BTN_RIGHT)
        // P1 face
        bindTouch(R.id.btn_a, 1, NativeBridge.BTN_A)
        bindTouch(R.id.btn_b, 1, NativeBridge.BTN_B)
        bindTouch(R.id.btn_c, 1, NativeBridge.BTN_C)
        bindTouch(R.id.btn_d, 1, NativeBridge.BTN_D)

        p1Joystick.onDirectionMaskChanged = { setDirectionalMask(1, it) }
    }

    // ---------- Preferences ----------

    private fun applyControlPreferences() {
        val useJoystick = PydmgApp.prefs.useJoystick
        val alpha = PydmgApp.prefs.controlOpacity
        val scale = PydmgApp.prefs.controlScale

        listOf(p1LeftControls, p1Abcd, findViewById(R.id.top_hud_bar)).forEach {
            it.alpha = alpha
        }
        listOf(p1LeftControls, p1Abcd).forEach {
            it.scaleX = scale; it.scaleY = scale
        }

        p1Dpad.visibility = if (useJoystick) View.GONE else View.VISIBLE
        p1Joystick.visibility = if (useJoystick) View.VISIBLE else View.GONE

        // Clear stale directional inputs whenever the control mode flips.
        setDirectionalMask(1, 0)
        p2Mask.set(0)

        // Bilinear filtering toggle goes here when EmulatorView supports it.
        emulatorView.smoothFilter = PydmgApp.prefs.smoothFilter
        emulatorView.cropScreen = PydmgApp.prefs.cropScreen
    }

    // ---------- Emulator loop ----------

    private fun startEmulatorLoop() {
        if (running.get()) return
        running.set(true)
        // Note: `audio.start()` is deferred by 2 emulated frames so
        // the AAudio ring has time to prime. See the comment inside
        // the emu loop where `audio.start()` is actually invoked.
        emuThread = Thread({
            // Ask the scheduler for real-time-ish treatment. THREAD_PRIORITY_DISPLAY
            // (-4) keeps us above default UI work without starving the
            // audio HAL thread; combined with MAX_PRIORITY on the Java side
            // this measurably reduces frame-time jitter on busy devices.
            try { Process.setThreadPriority(Process.THREAD_PRIORITY_DISPLAY) } catch (_: Throwable) {}

            // ---- Frame pacing -------------------------------------------------
            // Previous versions relied on SurfaceHolder.lockCanvas blocking at
            // the display refresh boundary as an implicit 60 Hz clock. That
            // breaks on 90/120/144 Hz panels (the game runs 1.5–2.4× too fast)
            // and on devices where the compositor doesn't throttle software
            // canvases. Instead we pace explicitly against a monotonic
            // deadline at the Neo Geo's real refresh (59.185606 Hz):
            //   · coarse wait: LockSupport.parkNanos down to ~2 ms before the
            //     deadline (cheap, lets the CPU race to idle);
            //   · fine wait: Thread.yield spin for the last stretch (precise,
            //     bounded to 2 ms so it can't melt the battery);
            //   · if we're LATE (emu frame took > period) we skip the wait
            //     entirely and resync the deadline so one slow frame never
            //     snowballs into a stutter train.
            val framePeriodNs = (1_000_000_000.0 / 59.185606).toLong()
            var nextDeadline = System.nanoTime() + framePeriodNs

            // Audio timing is decoupled: the AAudio callback thread
            // drains from the SPSC ring at its own real-time cadence.
            // If the emu overshoots one frame every now and then, the
            // ring absorbs it; if it undershoots, AAudio zero-pads (a
            // single sample glitch nobody hears).
            val net = PydmgApp.app.netSession
            var localFrameCounter = 0
            var audioStarted = false

            // ---- Adaptive presentation skip (slow-device safety net) ----
            // If the device can't emulate + blit inside one frame period, we
            // drop the PRESENTATION of at most 2 consecutive frames (the blit
            // is the expensive part on entry-level GPUs/software canvases)
            // while the emulation and audio keep running at full speed. The
            // result on weak hardware is 30-40 visual fps with perfect game
            // speed and unbroken audio, instead of a slow-motion crawl.
            var skippedInARow = 0
            val maxSkip = 2
            while (running.get()) {
                // Session death → bail back to the library.
                if (net != null && !net.alive.get()) {
                    runOnUiThread {
                        android.widget.Toast.makeText(
                            this@EmulatorActivity,
                            R.string.netplay_peer_left,
                            android.widget.Toast.LENGTH_SHORT
                        ).show()
                        finish()
                    }
                    break
                }
                val netPaused = net?.paused?.get() == true
                if (!paused.get() && !netPaused) {
                    if (net == null) {
                        // ------ Solo / local co-op path (v3 original). ------
                        NativeBridge.nativeSetPlayerInputs(p1Mask.get(), p2Mask.get())
                    } else {
                        // ---------------- LAN netplay path ----------------
                        // The local mask depends on our role: HOST is
                        // always P1, CLIENT is always P2. The `p1Mask`
                        // AtomicInteger in this activity is the mask
                        // filled from the on-screen controls, and it's
                        // "our" mask regardless of role (there is only
                        // one visible pad on each device).
                        val localMask = p1Mask.get()
                        net.publishLocalInputs(localMask)
                        val (p1, p2) = net.pollFrameInputs()
                        NativeBridge.nativeSetPlayerInputs(p1, p2)
                    }
                    NativeBridge.nativeRunFrame()

                    // Start the AAudio stream only AFTER we've run
                    // a couple of emu frames, so the SPSC ring is
                    // pre-filled with ~1800 samples (~33 ms at
                    // 55555 Hz). Without this two-stage warmup the
                    // first audio burst hits an empty ring and the
                    // callback zero-pads → audible click storm at
                    // t=0.
                    if (!audioStarted && localFrameCounter >= 2) {
                        audio.start()
                        audioStarted = true
                    }

                    audio.pump()

                    // Present unless we're already past the next deadline
                    // (i.e. this frame overran). Cap consecutive skips so
                    // the screen never freezes even under heavy load.
                    val behind = System.nanoTime() > nextDeadline
                    if (!behind || skippedInARow >= maxSkip) {
                        emulatorView.presentFrame()
                        skippedInARow = 0
                    } else {
                        skippedInARow++
                    }

                    // Frame counter must advance every emu tick,
                    // both solo and netplay, so the AAudio warmup
                    // check above fires reliably.
                    localFrameCounter++

                    // Netplay bookkeeping: advance the shared clock,
                    // then exchange a keyframe once a second so both
                    // peers can detect a desync fast.
                    if (net != null) {
                        net.advanceFrame()
                        if (localFrameCounter % 60 == 0) {
                            val f = NativeBridge.nativeFrameCounter()
                            val crc = NativeBridge.nativeStateChecksum()
                            if (net.role == com.pydmg.neogeo.net.NetplaySession.Role.HOST) {
                                net.sendKeyframe(f, crc)
                            } else {
                                net.compareOrQueueLocalKeyframe(f, crc)
                            }
                        }
                        // Un desync ahora se auto-recupera: el cliente
                        // pide un savestate al host y ambos siguen.
                        // Solo avisamos con un toast informativo.
                        net.desync.getAndSet(null)?.let { d ->
                            runOnUiThread {
                                android.widget.Toast.makeText(
                                    this@EmulatorActivity,
                                    getString(R.string.netplay_resync),
                                    android.widget.Toast.LENGTH_SHORT
                                ).show()
                            }
                            android.util.Log.w(TAG,
                                "desync @f=${d.frame} local=${d.localCrc} remote=${d.remoteCrc} — resyncing")
                        }
                    }
                }
                // ---- Resincronización por savestate (fuera del paso de
                // frame: puede ocurrir mientras la sesión está pausada) ----
                if (net != null) {
                    // HOST: el cliente pidió un snapshot → capturar el
                    // estado NGSS entre frames y enviarlo por TCP.
                    if (net.consumeSnapshotRequest()) {
                        val snap = NativeBridge.nativeSaveState()
                        if (snap != null && snap.isNotEmpty()) {
                            net.sendStateSnapshot(snap)
                        } else {
                            android.util.Log.w(TAG, "snapshot capture failed during resync")
                        }
                    }
                    // CLIENT: llegó el snapshot del host → cargarlo y
                    // adoptar su contador de frames.
                    net.consumePendingRemoteState()?.let { rs ->
                        val ok = NativeBridge.nativeLoadState(rs.state)
                        if (ok) {
                            net.completeResync(rs.frame)
                            nextDeadline = System.nanoTime() + framePeriodNs
                            runOnUiThread {
                                android.widget.Toast.makeText(
                                    this@EmulatorActivity,
                                    R.string.netplay_resync_done,
                                    android.widget.Toast.LENGTH_SHORT
                                ).show()
                            }
                        } else {
                            android.util.Log.w(TAG, "state load failed during resync")
                        }
                    }
                }
                if (paused.get() || (net?.paused?.get() == true)) {
                    // Paused: idle politely and resync the pacing deadline so
                    // resume doesn't fast-forward a burst of frames.
                    try { Thread.sleep(16) } catch (_: InterruptedException) { break }
                    nextDeadline = System.nanoTime() + framePeriodNs
                } else {
                    // ---- Precise pacing to 59.1856 Hz ----
                    var now = System.nanoTime()
                    if (now >= nextDeadline + framePeriodNs) {
                        // More than a full frame late → resync, don't chase.
                        nextDeadline = now + framePeriodNs
                    } else {
                        // Coarse: park until ~2 ms before the deadline.
                        var remaining = nextDeadline - now
                        while (remaining > 2_000_000L) {
                            LockSupport.parkNanos(remaining - 2_000_000L)
                            if (!running.get()) break
                            now = System.nanoTime()
                            remaining = nextDeadline - now
                        }
                        // Fine: yield-spin the last stretch (bounded ≤ 2 ms).
                        while (System.nanoTime() < nextDeadline && running.get()) {
                            Thread.yield()
                        }
                        nextDeadline += framePeriodNs
                    }
                }
            }
            audio.stop()
        }, "neogeo-emu").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
    }

    private fun stopEmulatorLoop() {
        if (!running.get()) return
        running.set(false)
        emuThread?.interrupt()
        try { emuThread?.join(1000) } catch (_: Throwable) {}
        emuThread = null
    }

    // ---------- Input plumbing ----------

    private fun bindTouch(viewId: Int, player: Int, bit: Int) {
        val v: View = findViewById(viewId) ?: return
        val haptics = PydmgApp.prefs.hapticFeedback
        v.setOnTouchListener { view, ev ->
            when (ev.actionMasked) {
                MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                    setBit(player, bit, true); view.isPressed = true
                    if (haptics) {
                        view.performHapticFeedback(
                            HapticFeedbackConstants.KEYBOARD_TAP,
                            HapticFeedbackConstants.FLAG_IGNORE_GLOBAL_SETTING
                        )
                    }
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP, MotionEvent.ACTION_CANCEL -> {
                    setBit(player, bit, false); view.isPressed = false
                }
            }
            true
        }
    }

    private fun setBit(player: Int, bit: Int, pressed: Boolean) {
        val atomic = if (player == 1) p1Mask else p2Mask
        atomic.updateAndGet { old -> if (pressed) old or bit else old and bit.inv() }
    }

    private fun setDirectionalMask(player: Int, dirMask: Int) {
        val atomic = if (player == 1) p1Mask else p2Mask
        atomic.updateAndGet { old ->
            (old and NativeBridge.DIR_MASK.inv()) or dirMask
        }
    }

    private fun hideSystemUI() {
        @Suppress("DEPRECATION")
        window.decorView.systemUiVisibility = (
            View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_FULLSCREEN
                or View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
            )
    }
}

package com.pydmg.neogeo.net

import android.util.Log
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

/**
 * LAN netplay session with lockstep-plus-input-delay scheduling.
 *
 * Concepts
 * --------
 *
 *  * Each peer emulates the **same** ROM starting from the same reset,
 *    consuming the same inputs on the same frames, so the pixel output
 *    is identical on both devices (modulo the "which side of the pad I
 *    render" question, which is a pure UI decision).
 *
 *  * Every frame T, both peers must feed the emulator the exact same
 *    (P1_mask, P2_mask). To achieve that without waiting for the peer
 *    on every frame (which would double the round-trip latency), we
 *    use **input delay N**: the button I press *now* takes effect
 *    inside the emulator N frames later, and I have those N frames to
 *    ship my input to the peer over UDP.
 *
 *  * With RTT < 5 ms on typical home WiFi (2.4/5 GHz), N = 2 frames
 *    (~33 ms one-way tolerance) is comfortable. If the LAN is very
 *    good you can drop to 1 or even 0; if it's flakey you can raise
 *    to 3-4 at the cost of feeling less responsive. Runtime tuning
 *    lives in [HelloAckPacket.inputDelay] and is chosen by the host.
 *
 * Concurrency model
 * -----------------
 * A [NetplaySession] owns:
 *
 *   * One TCP socket to the peer (control channel).
 *   * One UDP DatagramSocket bound locally, used to send our inputs to
 *     the peer and to receive theirs.
 *   * One background thread that pumps the TCP socket (opcodes:
 *     KEYFRAME, PAUSE, RESUME, BYE, etc.).
 *   * One background thread that receives UDP packets and drops the
 *     latest input for each future frame into [remoteInputs].
 *
 * The emulator thread ([com.pydmg.neogeo.EmulatorActivity]'s
 * `neogeo-emu` thread) queries the session synchronously each frame:
 *
 *   * [publishLocalInputs] — schedule the local player's mask to
 *     apply N frames from now, and immediately UDP-broadcast it.
 *   * [pollFrameInputs] — return `(p1Mask, p2Mask)` for the current
 *     frame T, blocking briefly if the remote input has not arrived
 *     yet (it usually has).
 *
 * That's it. Deterministic + minimal moving parts.
 */
class NetplaySession private constructor(
    val role: Role,
    val inputDelay: Int,
    private val tcp: Socket,
    private val udp: DatagramSocket,
    private val remoteEndpoint: InetSocketAddress,
) : AutoCloseable {

    enum class Role { HOST, CLIENT }

    /** Bit set to 1 while the session is live. Cleared on close or on
     *  fatal TCP error. Emulator polls this and drops back to menu. */
    val alive = AtomicBoolean(true)
    /** Bit set to 1 while the session is currently paused (menu open
     *  on either peer, or a desync popup). Emulator idles when set. */
    val paused = AtomicBoolean(false)
    /** Latest observed desync info, or null if in sync. */
    val desync = AtomicReference<DesyncReport?>(null)

    // ---- Resincronización por savestate ----
    /** HOST: el cliente ha pedido un snapshot (STATE_REQ recibido). */
    private val snapshotRequested = AtomicBoolean(false)
    /** CLIENT: snapshot del host pendiente de cargar en el emulador. */
    private val pendingRemoteState = AtomicReference<RemoteState?>(null)

    class RemoteState(val frame: Int, val state: ByteArray)

    /**
     * Per-future-frame remote input masks. Key = frame number the
     * mask applies to. Populated by the UDP RX thread as packets
     * arrive, drained by the emulator thread as it consumes frames.
     *
     * Concurrent skip-list would be strictly better but
     * ConcurrentHashMap is available on every Android API level and
     * the map is tiny (at most `inputDelay + jitter` entries).
     */
    private val remoteInputs = ConcurrentHashMap<Int, Int>()

    /**
     * Local input scheduled for future frames. Same shape as
     * [remoteInputs] but for our own player. The emulator writes
     * here from [publishLocalInputs]; nothing reads it back — it's
     * just so the current-frame combined lookup finds our own mask
     * when frame T finally arrives.
     */
    private val localInputs = ConcurrentHashMap<Int, Int>()

    private val currentFrame = AtomicInteger(0)

    private val udpRxThread: Thread
    private val tcpRxThread: Thread

    init {
        udpRxThread = Thread({ udpReceiveLoop() }, "netplay-udp-rx").apply {
            isDaemon = true; start()
        }
        tcpRxThread = Thread({ tcpReceiveLoop() }, "netplay-tcp-rx").apply {
            isDaemon = true; start()
        }
    }

    // ------------------------------------------------------------------
    //   Public API called by the emulator thread
    // ------------------------------------------------------------------

    /**
     * Called every frame *before* stepping the emulator. `localMask`
     * is what the local player pressed just now (bits from
     * [com.pydmg.neogeo.NativeBridge]'s `BTN_*` constants).
     *
     * Side effects:
     *   1. Schedule the mask to apply at frame `currentFrame + inputDelay`.
     *   2. UDP-send it to the peer immediately.
     */
    fun publishLocalInputs(localMask: Int) {
        val applyAt = currentFrame.get() + inputDelay
        localInputs[applyAt] = localMask

        // Historial redundante: cada datagrama repite los últimos
        // [InputPacket.MAX_MASKS] masks (frames applyAt, applyAt-1, …),
        // así la pérdida de hasta MAX_MASKS-1 datagramas consecutivos
        // no pierde ningún input — el siguiente que llegue los trae.
        maskHistory[applyAt % maskHistory.size] = localMask
        val n = minOf(InputPacket.MAX_MASKS, applyAt + 1)
        val masks = IntArray(n) { i -> maskHistory[(applyAt - i) % maskHistory.size] }

        val buf = ByteBuffer.allocate(InputPacket.WIRE_SIZE).order(ByteOrder.LITTLE_ENDIAN)
        InputPacket(applyAt, currentFrame.get(), masks).encode(buf)
        try {
            udp.send(DatagramPacket(buf.array(), buf.position(), remoteEndpoint))
        } catch (t: Throwable) {
            Log.w(TAG, "UDP send failed: ${t.message}")
        }
    }

    /**
     * Called every frame *after* [publishLocalInputs], right before
     * stepping the emulator. Returns the `(p1Mask, p2Mask)` to feed
     * `nativeSetPlayerInputs`.
     *
     * Waits up to `waitMillis` for the remote input mask if we don't
     * have it yet (LAN packets arrive within microseconds → almost
     * always the map already has it by now). If the wait expires,
     * we substitute the last known remote mask ("input hold"), which
     * is exactly what real arcade netcode implementations do under
     * jitter.
     */
    fun pollFrameInputs(waitMillis: Long = 10L): Pair<Int, Int> {
        val f = currentFrame.get()
        val localMask = localInputs[f] ?: 0
        var remoteMask = remoteInputs[f]

        if (remoteMask == null) {
            val deadline = System.nanoTime() + waitMillis * 1_000_000L
            while (remoteMask == null && System.nanoTime() < deadline && alive.get()) {
                try { Thread.sleep(0, 200_000) } catch (_: InterruptedException) { break }
                remoteMask = remoteInputs[f]
            }
        }

        // Fall back to the most recent remote mask we've ever seen if
        // this frame's mask never made it. This is an "input hold" —
        // real-world netplay never stops the game, it just replays the
        // last button state on packet loss.
        if (remoteMask == null) {
            remoteMask = lastRemoteMask
        } else {
            lastRemoteMask = remoteMask
        }

        // Cleanup consumed entries so the map doesn't grow unbounded.
        localInputs.remove(f)
        remoteInputs.remove(f)

        return if (role == Role.HOST) localMask to remoteMask
        else remoteMask to localMask
    }

    /** Increment the local frame counter. Emulator calls this after
     *  each successful `nativeRunFrame()`. */
    fun advanceFrame() {
        currentFrame.incrementAndGet()
    }

    /** Host-only: send a keyframe (frame + CRC) to the client so they
     *  can verify sync. Called by the emulator every N frames. */
    fun sendKeyframe(frame: Int, crc32: Int) {
        if (role != Role.HOST) return
        try {
            tcp.getOutputStream().write(KeyframePacket(frame, crc32).encode())
        } catch (t: Throwable) {
            Log.w(TAG, "keyframe send failed: ${t.message}")
        }
    }

    /** Client-only: record local CRC so the TCP handler can compare
     *  when the host's keyframe arrives. */
    fun recordLocalKeyframe(frame: Int, crc32: Int) {
        pendingLocalKeyframes[frame] = crc32
    }

    // ------------------------------------------------------------------
    //   Resync por savestate (llamado desde el hilo del emulador)
    // ------------------------------------------------------------------

    /** HOST: true una sola vez cuando el cliente ha pedido resync.
     *  El emulador debe responder llamando a [sendStateSnapshot] con
     *  el resultado de `nativeSaveState()`. */
    fun consumeSnapshotRequest(): Boolean =
        role == Role.HOST && snapshotRequested.compareAndSet(true, false)

    /** HOST: envía el snapshot al cliente y continúa la partida desde
     *  el frame de sesión actual. Limpia los inputs programados para
     *  que ambos lados partan de un plano limpio (el input-hold cubre
     *  el hueco de delay). */
    fun sendStateSnapshot(state: ByteArray) {
        if (role != Role.HOST) return
        val f = currentFrame.get()
        localInputs.clear(); remoteInputs.clear()
        try {
            synchronized(tcpWriteLock) {
                tcp.getOutputStream().write(StateDataPacket(f, state).encode())
                tcp.getOutputStream().flush()
            }
            desync.set(null)
            paused.set(false)
            Log.i(TAG, "state snapshot sent (${state.size} B) at frame $f")
        } catch (t: Throwable) {
            Log.w(TAG, "state snapshot send failed: ${t.message}")
        }
    }

    /** CLIENT: snapshot del host pendiente de cargar, o null. El
     *  emulador lo consume, llama a `nativeLoadState()` y después a
     *  [completeResync]. */
    fun consumePendingRemoteState(): RemoteState? =
        pendingRemoteState.getAndSet(null)

    /** CLIENT: el snapshot se cargó con éxito — adopta el contador de
     *  frames del host y reanuda. */
    fun completeResync(hostFrame: Int) {
        localInputs.clear(); remoteInputs.clear()
        currentFrame.set(hostFrame)
        desync.set(null)
        paused.set(false)
        Log.i(TAG, "resynced to host frame $hostFrame")
    }

    /** CLIENT: pide al host un snapshot para resincronizar. */
    private fun requestStateSnapshot() {
        if (role != Role.CLIENT) return
        try {
            val buf = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN)
            Protocol.writeHeader(buf, Protocol.OP_STATE_REQ)
            synchronized(tcpWriteLock) {
                tcp.getOutputStream().write(buf.array())
                tcp.getOutputStream().flush()
            }
            Log.i(TAG, "state snapshot requested from host")
        } catch (t: Throwable) {
            Log.w(TAG, "state request failed: ${t.message}")
        }
    }

    override fun close() {
        if (!alive.compareAndSet(true, false)) return
        try { tcp.close() } catch (_: Throwable) {}
        try { udp.close() } catch (_: Throwable) {}
        udpRxThread.interrupt(); tcpRxThread.interrupt()
    }

    // ------------------------------------------------------------------
    //   Internals
    // ------------------------------------------------------------------

    @Volatile private var lastRemoteMask: Int = 0
    private val pendingLocalKeyframes = ConcurrentHashMap<Int, Int>()
    /** Anillo con los últimos masks locales para el envío redundante. */
    private val maskHistory = IntArray(64)
    /** Serializa las escrituras TCP (keyframes vs snapshots vs acks
     *  salen de hilos distintos). */
    private val tcpWriteLock = Any()

    private fun udpReceiveLoop() {
        val scratch = ByteArray(64)
        while (alive.get()) {
            try {
                val p = DatagramPacket(scratch, scratch.size)
                udp.receive(p)
                val buf = ByteBuffer.wrap(scratch, 0, p.length)
                val pkt = InputPacket.decode(buf) ?: continue
                // Cada datagrama trae masks[i] para el frame
                // (frameNumber - i): rellenamos todos los que aún no
                // hayamos consumido, recuperando así los inputs de
                // datagramas perdidos.
                val cur = currentFrame.get()
                for (i in pkt.masks.indices) {
                    val f = pkt.frameNumber - i
                    if (f >= cur) remoteInputs.putIfAbsent(f, pkt.masks[i])
                }
            } catch (_: SocketTimeoutException) {
                // Timeout is fine, loop and re-check `alive`.
            } catch (t: Throwable) {
                if (alive.get()) Log.w(TAG, "UDP rx: ${t.message}")
            }
        }
    }

    private fun tcpReceiveLoop() {
        val ins = tcp.getInputStream()
        val hdr = ByteArray(4)
        while (alive.get()) {
            try {
                if (!readFully(ins, hdr)) break
                if (hdr[0] != Protocol.MAGIC_0 || hdr[1] != Protocol.MAGIC_1) {
                    Log.w(TAG, "bad magic on TCP"); break
                }
                if (hdr[2] != Protocol.VERSION) {
                    Log.w(TAG, "bad version on TCP"); break
                }
                when (hdr[3]) {
                    Protocol.OP_KEYFRAME -> handleKeyframe(ins)
                    Protocol.OP_KEYFRAME_ACK -> handleKeyframeAck(ins)
                    Protocol.OP_STATE_REQ -> {
                        // El cliente pide resincronizar: pausamos y
                        // avisamos al hilo del emulador para que capture
                        // el snapshot entre frames.
                        paused.set(true)
                        snapshotRequested.set(true)
                    }
                    Protocol.OP_STATE_DATA -> handleStateData(ins)
                    Protocol.OP_PAUSE -> { paused.set(true) }
                    Protocol.OP_RESUME -> { paused.set(false) }
                    Protocol.OP_BYE -> {
                        Log.i(TAG, "peer said BYE")
                        alive.set(false); break
                    }
                    else -> Log.w(TAG, "unknown TCP opcode ${hdr[3]}")
                }
            } catch (t: Throwable) {
                if (alive.get()) Log.w(TAG, "TCP rx: ${t.message}")
                break
            }
        }
        alive.set(false)
    }

    private fun handleKeyframe(ins: java.io.InputStream) {
        val payload = ByteArray(8)
        if (!readFully(ins, payload)) return
        val b = ByteBuffer.wrap(payload).order(ByteOrder.LITTLE_ENDIAN)
        val frame = b.int
        val hostCrc = b.int
        val ourCrc = pendingLocalKeyframes.remove(frame)
        if (ourCrc == null) {
            // Haven't reached that frame yet; store host's CRC and let
            // the emulator thread cross-check when it gets there.
            pendingRemoteKeyframes[frame] = hostCrc
            return
        }
        val ok = ourCrc == hostCrc
        try {
            synchronized(tcpWriteLock) {
                tcp.getOutputStream().write(KeyframeAckPacket(frame, ok).encode())
            }
        } catch (_: Throwable) {}
        if (!ok) {
            desync.set(DesyncReport(frame, ourCrc, hostCrc))
            paused.set(true)
            // Auto-recuperación: en vez de quedarnos pausados para
            // siempre, pedimos el estado del host y seguimos jugando.
            requestStateSnapshot()
        }
    }

    /** CLIENT: recibe el snapshot NGSS del host por TCP. */
    private fun handleStateData(ins: java.io.InputStream) {
        val head = ByteArray(8)
        if (!readFully(ins, head)) return
        val b = ByteBuffer.wrap(head).order(ByteOrder.LITTLE_ENDIAN)
        val frame = b.int
        val len = b.int
        if (len <= 0 || len > StateDataPacket.MAX_STATE_BYTES) {
            Log.w(TAG, "state data with bogus length $len — dropping session")
            alive.set(false)
            return
        }
        val state = ByteArray(len)
        if (!readFully(ins, state)) return
        pendingRemoteState.set(RemoteState(frame, state))
        Log.i(TAG, "state snapshot received ($len B) for frame $frame")
    }

    private fun handleKeyframeAck(ins: java.io.InputStream) {
        val payload = ByteArray(5)
        if (!readFully(ins, payload)) return
        val b = ByteBuffer.wrap(payload).order(ByteOrder.LITTLE_ENDIAN)
        val frame = b.int
        val ok = b.get() != 0.toByte()
        if (!ok) {
            desync.set(DesyncReport(frame, 0, 0))
            paused.set(true)
        }
    }

    /** Set by the TCP RX loop when the peer's keyframe arrives before
     *  we reach that frame locally. Emulator polls this each frame. */
    private val pendingRemoteKeyframes = ConcurrentHashMap<Int, Int>()

    /** Client: given local CRC at frame F, compare against any queued
     *  host CRC for F and, if present, ack. Called from the emulator
     *  thread after computing the CRC. */
    fun compareOrQueueLocalKeyframe(frame: Int, crc32: Int) {
        val hostCrc = pendingRemoteKeyframes.remove(frame)
        if (hostCrc == null) {
            recordLocalKeyframe(frame, crc32)
        } else {
            val ok = hostCrc == crc32
            try {
                synchronized(tcpWriteLock) {
                    tcp.getOutputStream().write(KeyframeAckPacket(frame, ok).encode())
                }
            } catch (_: Throwable) {}
            if (!ok) {
                desync.set(DesyncReport(frame, crc32, hostCrc))
                paused.set(true)
                requestStateSnapshot()
            }
        }
    }

    companion object {
        private const val TAG = "netplay"

        /** How many UDP-worth ms to leave on the TCP recv side before
         *  we notice the peer is gone. */
        private const val TCP_SO_TIMEOUT_MS = 5_000
        /** UDP receive timeout — must be short so the RX loop can
         *  observe `alive == false` on session close. */
        private const val UDP_SO_TIMEOUT_MS = 200

        /**
         * Bind the given TCP port, wait for a client, negotiate the
         * session, and return the live [NetplaySession]. Blocks the
         * calling thread until a peer connects.
         *
         * The caller (typically a coroutine on the IO dispatcher) is
         * responsible for setting a UI timeout and cancelling if the
         * user gives up.
         */
        /**
         * v2: la sala verifica que el cliente trae el MISMO juego
         * ([gameName]); si no, responde REJECT y sigue esperando a
         * otro cliente. Después mide el RTT real con PING/PONG por
         * TCP y elige el input-delay adaptativamente:
         *
         *   RTT mín        delay
         *   ------------   -----
         *   < 8 ms          1 frame  (WiFi 5 GHz / ethernet)
         *   < 25 ms         2 frames (WiFi 2.4 GHz típico)
         *   < 50 ms         3 frames (red congestionada)
         *   >= 50 ms        4 frames (peor caso tolerable en LAN)
         */
        fun acceptAsHost(
            gameName: String,
            tcpPort: Int = Protocol.DEFAULT_TCP_PORT,
            udpPort: Int = Protocol.DEFAULT_UDP_PORT,
        ): NetplaySession {
            val server = ServerSocket(tcpPort)
            try {
                while (true) {
                    val tcp = server.accept()
                    tcp.tcpNoDelay = true
                    tcp.soTimeout = TCP_SO_TIMEOUT_MS

                    // Read HELLO (v2: nick + game)
                    val ins = tcp.getInputStream()
                    val hdr = ByteArray(4)
                    if (!readFully(ins, hdr) ||
                        hdr[0] != Protocol.MAGIC_0 || hdr[1] != Protocol.MAGIC_1 ||
                        hdr[2] != Protocol.VERSION || hdr[3] != Protocol.OP_HELLO) {
                        try { tcp.close() } catch (_: Throwable) {}
                        // Cliente con versión/protocolo distinto — sigue
                        // esperando al siguiente en vez de tirar la sala.
                        continue
                    }
                    val nickLen = ins.read()
                    val nick = ByteArray(nickLen)
                    readFully(ins, nick)
                    val gameLen = ins.read()
                    val game = ByteArray(gameLen)
                    readFully(ins, game)
                    val clientGame = String(game, Charsets.UTF_8)
                    Log.i(TAG, "client nick='${String(nick, Charsets.UTF_8)}' game='$clientGame'")

                    if (clientGame != gameName) {
                        // Sala de otro juego: rechazo limpio y a esperar
                        // al siguiente cliente.
                        Log.w(TAG, "rejecting client: game '$clientGame' != '$gameName'")
                        try {
                            tcp.getOutputStream().write(
                                RejectPacket(RejectPacket.REASON_GAME_MISMATCH).encode())
                            tcp.close()
                        } catch (_: Throwable) {}
                        continue
                    }

                    // ---- RTT probe: 4 PINGs por TCP, nos quedamos el mín ----
                    val outs = tcp.getOutputStream()
                    var minRttNs = Long.MAX_VALUE
                    val pongHdr = ByteArray(4)
                    val pongPayload = ByteArray(12)
                    for (seq in 0 until 4) {
                        val t0 = System.nanoTime()
                        outs.write(PingPacket(seq, t0).encode()); outs.flush()
                        if (!readFully(ins, pongHdr) ||
                            pongHdr[3] != Protocol.OP_PONG ||
                            !readFully(ins, pongPayload)) {
                            minRttNs = -1; break
                        }
                        val rtt = System.nanoTime() - t0
                        if (rtt < minRttNs) minRttNs = rtt
                    }
                    val inputDelay = when {
                        minRttNs < 0 -> 2               // sonda falló: default histórico
                        minRttNs < 8_000_000L -> 1
                        minRttNs < 25_000_000L -> 2
                        minRttNs < 50_000_000L -> 3
                        else -> 4
                    }
                    Log.i(TAG, "RTT min=${if (minRttNs < 0) "?" else "${minRttNs / 1_000_000.0} ms"} → inputDelay=$inputDelay")

                    // Reply HELLO_ACK
                    val sessionId = (Math.random() * Int.MAX_VALUE).toInt()
                    outs.write(HelloAckPacket(sessionId, inputDelay).encode())
                    outs.flush()

                    val udp = DatagramSocket(udpPort)
                    udp.soTimeout = UDP_SO_TIMEOUT_MS
                    val remote = InetSocketAddress(tcp.inetAddress, udpPort)
                    Log.i(TAG, "host session established with ${tcp.inetAddress}, delay=$inputDelay")
                    return NetplaySession(Role.HOST, inputDelay, tcp, udp, remote)
                }
                @Suppress("UNREACHABLE_CODE")
                throw IllegalStateException("unreachable")
            } finally {
                try { server.close() } catch (_: Throwable) {}
            }
        }

        /**
         * Connect to a listening host and complete the handshake.
         * Blocks until connected + session is ready.
         */
        fun connectAsClient(
            hostAddress: String,
            gameName: String,
            nickname: String = "player2",
            tcpPort: Int = Protocol.DEFAULT_TCP_PORT,
            udpPort: Int = Protocol.DEFAULT_UDP_PORT,
        ): NetplaySession {
            val addr = InetAddress.getByName(hostAddress)
            val tcp = Socket()
            tcp.connect(InetSocketAddress(addr, tcpPort), 4_000)
            tcp.tcpNoDelay = true
            tcp.soTimeout = TCP_SO_TIMEOUT_MS

            tcp.getOutputStream().write(HelloPacket(nickname, gameName).encode())
            tcp.getOutputStream().flush()

            // El host puede: (a) rechazarnos (REJECT), (b) sondear RTT
            // (PING × N — respondemos PONG eco), (c) aceptarnos
            // (HELLO_ACK). Procesamos opcodes hasta ver el ACK.
            val ins = tcp.getInputStream()
            val hdr = ByteArray(4)
            while (true) {
                if (!readFully(ins, hdr) ||
                    hdr[0] != Protocol.MAGIC_0 || hdr[1] != Protocol.MAGIC_1 ||
                    hdr[2] != Protocol.VERSION) {
                    tcp.close()
                    throw RuntimeException("bad handshake from host")
                }
                when (hdr[3]) {
                    Protocol.OP_PING -> {
                        val p = ByteArray(12)
                        if (!readFully(ins, p)) { tcp.close(); throw RuntimeException("ping cut short") }
                        val b = ByteBuffer.wrap(p).order(ByteOrder.LITTLE_ENDIAN)
                        val seq = b.int; val nanos = b.long
                        tcp.getOutputStream().write(PingPacket(seq, nanos).encode(Protocol.OP_PONG))
                        tcp.getOutputStream().flush()
                    }
                    Protocol.OP_REJECT -> {
                        val r = ins.read()
                        tcp.close()
                        throw GameMismatchException(
                            if (r == RejectPacket.REASON_GAME_MISMATCH.toInt())
                                "la sala es de otro juego" else "sala rechazó la conexión ($r)")
                    }
                    Protocol.OP_HELLO_ACK -> break
                    else -> { tcp.close(); throw RuntimeException("unexpected opcode ${hdr[3]} in handshake") }
                }
            }
            val payload = ByteArray(8)
            readFully(ins, payload)
            val b = ByteBuffer.wrap(payload).order(ByteOrder.LITTLE_ENDIAN)
            val sessionId = b.int
            val inputDelay = b.int
            Log.i(TAG, "client joined session $sessionId with delay=$inputDelay")

            val udp = DatagramSocket(udpPort)
            udp.soTimeout = UDP_SO_TIMEOUT_MS
            val remote = InetSocketAddress(addr, udpPort)
            return NetplaySession(Role.CLIENT, inputDelay, tcp, udp, remote)
        }

        private fun readFully(ins: java.io.InputStream, dst: ByteArray): Boolean {
            var off = 0
            while (off < dst.size) {
                val n = ins.read(dst, off, dst.size - off)
                if (n < 0) return false
                off += n
            }
            return true
        }
    }
}

/** El host rechazó la conexión porque su sala es de otro juego. */
class GameMismatchException(message: String) : RuntimeException(message)

/** Report captured when the two peers' work-RAM CRCs diverge. */
data class DesyncReport(
    val frame: Int,
    val localCrc: Int,
    val remoteCrc: Int,
)

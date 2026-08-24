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

        val buf = ByteBuffer.allocate(12).order(ByteOrder.LITTLE_ENDIAN)
        InputPacket(applyAt, localMask).encode(buf)
        val bytes = buf.array()
        try {
            udp.send(DatagramPacket(bytes, bytes.size, remoteEndpoint))
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

    private fun udpReceiveLoop() {
        val scratch = ByteArray(64)
        while (alive.get()) {
            try {
                val p = DatagramPacket(scratch, scratch.size)
                udp.receive(p)
                val buf = ByteBuffer.wrap(scratch, 0, p.length)
                val pkt = InputPacket.decode(buf) ?: continue
                // Drop packets we've already passed (very late arrivals).
                if (pkt.frameNumber >= currentFrame.get()) {
                    remoteInputs[pkt.frameNumber] = pkt.inputMask
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
            tcp.getOutputStream().write(KeyframeAckPacket(frame, ok).encode())
        } catch (_: Throwable) {}
        if (!ok) {
            desync.set(DesyncReport(frame, ourCrc, hostCrc))
            paused.set(true)
        }
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
            try { tcp.getOutputStream().write(KeyframeAckPacket(frame, ok).encode()) }
            catch (_: Throwable) {}
            if (!ok) {
                desync.set(DesyncReport(frame, crc32, hostCrc))
                paused.set(true)
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
        fun acceptAsHost(
            tcpPort: Int = Protocol.DEFAULT_TCP_PORT,
            udpPort: Int = Protocol.DEFAULT_UDP_PORT,
            inputDelay: Int = 2,
        ): NetplaySession {
            val server = ServerSocket(tcpPort)
            try {
                val tcp = server.accept()
                tcp.tcpNoDelay = true
                tcp.soTimeout = TCP_SO_TIMEOUT_MS

                // Read HELLO
                val ins = tcp.getInputStream()
                val hdr = ByteArray(4)
                if (!readFully(ins, hdr) ||
                    hdr[0] != Protocol.MAGIC_0 || hdr[1] != Protocol.MAGIC_1 ||
                    hdr[2] != Protocol.VERSION || hdr[3] != Protocol.OP_HELLO) {
                    tcp.close()
                    throw RuntimeException("bad HELLO from client")
                }
                val nickLen = ins.read()
                val nick = ByteArray(nickLen)
                readFully(ins, nick)
                Log.i(TAG, "client nick='${String(nick, Charsets.UTF_8)}'")

                // Reply HELLO_ACK
                val sessionId = (Math.random() * Int.MAX_VALUE).toInt()
                tcp.getOutputStream().write(HelloAckPacket(sessionId, inputDelay).encode())

                val udp = DatagramSocket(udpPort)
                udp.soTimeout = UDP_SO_TIMEOUT_MS
                val remote = InetSocketAddress(tcp.inetAddress, udpPort)
                Log.i(TAG, "host session established with ${tcp.inetAddress}, delay=$inputDelay")
                return NetplaySession(Role.HOST, inputDelay, tcp, udp, remote)
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
            nickname: String = "player2",
            tcpPort: Int = Protocol.DEFAULT_TCP_PORT,
            udpPort: Int = Protocol.DEFAULT_UDP_PORT,
        ): NetplaySession {
            val addr = InetAddress.getByName(hostAddress)
            val tcp = Socket()
            tcp.connect(InetSocketAddress(addr, tcpPort), 4_000)
            tcp.tcpNoDelay = true
            tcp.soTimeout = TCP_SO_TIMEOUT_MS

            tcp.getOutputStream().write(HelloPacket(nickname).encode())

            val ins = tcp.getInputStream()
            val hdr = ByteArray(4)
            if (!readFully(ins, hdr) ||
                hdr[0] != Protocol.MAGIC_0 || hdr[1] != Protocol.MAGIC_1 ||
                hdr[2] != Protocol.VERSION || hdr[3] != Protocol.OP_HELLO_ACK) {
                tcp.close()
                throw RuntimeException("bad HELLO_ACK from host")
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

/** Report captured when the two peers' work-RAM CRCs diverge. */
data class DesyncReport(
    val frame: Int,
    val localCrc: Int,
    val remoteCrc: Int,
)

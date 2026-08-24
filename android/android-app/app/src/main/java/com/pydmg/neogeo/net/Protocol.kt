package com.pydmg.neogeo.net

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Wire protocol for pydmg-neogeo LAN netplay (v1).
 *
 * Design goals
 * ------------
 *  * **Simple**: hand-rolled binary framing, no external dependency.
 *  * **Deterministic**: fixed little-endian layout, no floats, no
 *    Kotlin serialization voodoo.
 *  * **Small**: an input packet is 12 bytes; we can send one per
 *    emulated frame (60 Hz) with negligible bandwidth (~700 B/s).
 *  * **Debuggable**: every packet starts with a magic + version byte
 *    so tcpdump / Wireshark decoding is trivial.
 *
 * Transports
 * ----------
 *  * **TCP** for the handshake, ROM identity check, initial reset
 *    sync and periodic keyframes (checksum + frame number).
 *    Reliability matters for these; latency is not critical.
 *  * **UDP** for the per-frame input packets in the running session.
 *    A lost input packet is harmless: the next one arriving already
 *    covers subsequent frames (see [InputPacket.frameNumber]).
 *
 * Roles
 * -----
 *  * **HOST**: authoritative for the emulated clock, drives the pace.
 *    Local player is P1.
 *  * **CLIENT**: follows the host's pace. Local player is P2.
 *
 * Framing
 * -------
 * Every packet:
 *
 *  ```
 *  offset  size  field
 *  ------  ----  -----
 *  0       2     magic  = 0xD6, 0x64  ("pydmg", first two chars XOR'd)
 *  2       1     version = 0x01
 *  3       1     opcode
 *  4       ...   payload (opcode-dependent)
 *  ```
 *
 * Opcodes
 * -------
 *
 *  ```
 *   0x10  HELLO         (TCP, host <- client): protocol handshake.
 *   0x11  HELLO_ACK     (TCP, host -> client): protocol ACK + session id.
 *   0x20  ROM_ID        (TCP, both ways): romset name + CRC32 for match.
 *   0x30  START         (TCP, host -> client): begin emulation at frame 0.
 *   0x40  INPUT         (UDP, both ways): one frame of local player inputs.
 *   0x50  KEYFRAME      (TCP, host -> client): checksum + frame number.
 *   0x51  KEYFRAME_ACK  (TCP, client -> host): confirms match / reports mismatch.
 *   0x60  PAUSE         (TCP, both ways): pause the session (menu / desync).
 *   0x61  RESUME        (TCP, both ways): resume from pause.
 *   0xFF  BYE           (TCP/UDP): the peer is closing the session.
 *  ```
 */
object Protocol {
    const val MAGIC_0: Byte = 0xD6.toByte()
    const val MAGIC_1: Byte = 0x64.toByte()

    /**
     * v2 (salas LAN):
     *   * HELLO lleva el nombre del juego — el host rechaza (REJECT) si
     *     no coincide con el suyo, para que "unirse por IP manual" no
     *     pueda colarse en una sala de otro juego.
     *   * PING/PONG por TCP en el handshake: el host mide el RTT real
     *     y elige el input-delay adaptativamente (1..4 frames).
     *   * INPUT redundante: cada datagrama lleva los últimos 8 masks
     *     (frames F, F-1, …, F-7) + el frame actual del emisor para
     *     control de deriva. Perder hasta 7 datagramas seguidos no
     *     pierde ningún input.
     *   * STATE_REQ/STATE_DATA: resincronización por savestate — al
     *     detectar un desync el cliente pide el estado, el host manda
     *     su snapshot NGSS por TCP, el cliente lo carga y la partida
     *     continúa (en vez de quedarse pausada para siempre).
     */
    const val VERSION: Byte = 0x02

    const val OP_HELLO: Byte        = 0x10
    const val OP_HELLO_ACK: Byte    = 0x11
    const val OP_REJECT: Byte       = 0x12
    const val OP_ROM_ID: Byte       = 0x20
    const val OP_START: Byte        = 0x30
    const val OP_INPUT: Byte        = 0x40
    const val OP_PING: Byte         = 0x42
    const val OP_PONG: Byte         = 0x43
    const val OP_KEYFRAME: Byte     = 0x50
    const val OP_KEYFRAME_ACK: Byte = 0x51
    const val OP_STATE_REQ: Byte    = 0x52
    const val OP_STATE_DATA: Byte   = 0x53
    const val OP_PAUSE: Byte        = 0x60
    const val OP_RESUME: Byte       = 0x61
    const val OP_BYE: Byte          = 0xFF.toByte()

    /** TCP port the host binds. Also announced via mDNS. */
    const val DEFAULT_TCP_PORT: Int = 27750
    /** UDP port the host binds (input packets). */
    const val DEFAULT_UDP_PORT: Int = 27751

    /** Fixed prefix appended to every packet. */
    fun writeHeader(buf: ByteBuffer, op: Byte) {
        buf.order(ByteOrder.LITTLE_ENDIAN)
        buf.put(MAGIC_0); buf.put(MAGIC_1)
        buf.put(VERSION); buf.put(op)
    }
}

/**
 * One frame's worth of local player inputs sent over UDP.
 *
 * The 16-bit [inputMask] uses the same bit layout as
 * [com.pydmg.neogeo.NativeBridge]'s `BTN_*` constants — but only the
 * lower 11 bits are meaningful (UP/DOWN/LEFT/RIGHT/A/B/C/D/START/SELECT/COIN).
 *
 * [frameNumber] is the frame this input **applies to** (already offset
 * by the netplay input-delay N; the sender computed `local_frame + N`).
 * A packet for a frame the peer has already consumed is silently
 * dropped by the receiver.
 */
class InputPacket(
    /** Frame al que aplica masks[0]; masks[i] aplica a frameNumber - i. */
    val frameNumber: Int,
    /** Frame actual del emulador del emisor — control de deriva. */
    val senderFrame: Int,
    /** Historial redundante: hasta [MAX_MASKS] masks recientes. */
    val masks: IntArray,
) {
    fun encode(dst: ByteBuffer) {
        Protocol.writeHeader(dst, Protocol.OP_INPUT)
        dst.putInt(frameNumber)
        dst.putInt(senderFrame)
        val n = masks.size.coerceAtMost(MAX_MASKS)
        dst.put(n.toByte())
        for (i in 0 until n) dst.putShort((masks[i] and 0xFFFF).toShort())
    }

    companion object {
        /** Cuántos masks históricos viajan en cada datagrama. Con 8, se
         *  toleran 7 datagramas perdidos seguidos sin perder un input. */
        const val MAX_MASKS = 8
        /** 4 header + 4 frame + 4 senderFrame + 1 count + 2*8 masks. */
        const val WIRE_SIZE = 4 + 4 + 4 + 1 + 2 * MAX_MASKS

        /** Parses a validated INPUT packet from `src`. Returns null if
         *  the header magic / version / opcode don't match. */
        fun decode(src: ByteBuffer): InputPacket? {
            src.order(ByteOrder.LITTLE_ENDIAN)
            if (src.remaining() < 14) return null
            if (src.get() != Protocol.MAGIC_0) return null
            if (src.get() != Protocol.MAGIC_1) return null
            if (src.get() != Protocol.VERSION) return null
            if (src.get() != Protocol.OP_INPUT) return null
            val fn = src.int
            val sf = src.int
            val n = src.get().toInt() and 0xFF
            if (n == 0 || n > MAX_MASKS || src.remaining() < n * 2) return null
            val masks = IntArray(n) { src.short.toInt() and 0xFFFF }
            return InputPacket(fn, sf, masks)
        }
    }
}

/** TCP handshake payload — sent client → host on connect.
 *  v2: lleva también el nombre del set del juego para que el host pueda
 *  rechazar a un cliente con otro juego (defensa para "IP manual"). */
data class HelloPacket(
    val nickname: String,   // free-form, purely cosmetic
    val gameName: String,   // romset name, must match the host's
) {
    fun encode(): ByteArray {
        val nick = nickname.take(24).toByteArray(Charsets.UTF_8)
        val game = gameName.take(24).toByteArray(Charsets.UTF_8)
        val buf = ByteBuffer.allocate(4 + 1 + nick.size + 1 + game.size)
            .order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_HELLO)
        buf.put(nick.size.toByte())
        buf.put(nick)
        buf.put(game.size.toByte())
        buf.put(game)
        return buf.array()
    }
}

/** Host → client: la sala rechaza la conexión. */
data class RejectPacket(val reason: Byte) {
    fun encode(): ByteArray {
        val buf = ByteBuffer.allocate(4 + 1).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_REJECT)
        buf.put(reason)
        return buf.array()
    }
    companion object {
        const val REASON_GAME_MISMATCH: Byte = 1
    }
}

/** Handshake RTT probe (TCP, host → client; el cliente responde PONG
 *  con el mismo payload). El host usa el mínimo de varias muestras
 *  para elegir el input-delay. */
data class PingPacket(val seq: Int, val nanos: Long) {
    fun encode(op: Byte = Protocol.OP_PING): ByteArray {
        val buf = ByteBuffer.allocate(4 + 4 + 8).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, op)
        buf.putInt(seq)
        buf.putLong(nanos)
        return buf.array()
    }
}

/** Host → client: snapshot NGSS completo para resincronizar tras un
 *  desync. `frame` es el frame de sesión del host en el momento del
 *  snapshot; el cliente adopta ese contador tras cargar el estado. */
class StateDataPacket(val frame: Int, val state: ByteArray) {
    fun encode(): ByteArray {
        val buf = ByteBuffer.allocate(4 + 4 + 4 + state.size).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_STATE_DATA)
        buf.putInt(frame)
        buf.putInt(state.size)
        buf.put(state)
        return buf.array()
    }
    companion object {
        /** Un snapshot NGSS ronda los 220 KiB; 8 MiB es un tope de
         *  cordura contra corrupción del stream. */
        const val MAX_STATE_BYTES = 8 * 1024 * 1024
    }
}

/**
 * The host confirms the client's HELLO and states the session policy:
 * input delay in frames and the seed for the RNG-like state at t=0.
 *
 * [inputDelay] is the number of frames each peer inserts between "I
 * pressed the button locally" and "the emulator sees the button as
 * pressed". A larger delay hides more RTT jitter but feels less
 * responsive. Default is 2 frames (33 ms) for LAN.
 */
data class HelloAckPacket(
    val sessionId: Int,
    val inputDelay: Int,
) {
    fun encode(): ByteArray {
        val buf = ByteBuffer.allocate(4 + 4 + 4).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_HELLO_ACK)
        buf.putInt(sessionId)
        buf.putInt(inputDelay)
        return buf.array()
    }
}

/**
 * ROM identity + parent BIOS CRC. Both peers exchange this over TCP
 * before START; if they don't match, the session aborts with a clear
 * error toast on both sides.
 */
data class RomIdPacket(
    val cartName: String,
    val cartCrc32: Int,
    val biosCrc32: Int,
) {
    fun encode(): ByteArray {
        val n = cartName.take(24).toByteArray(Charsets.UTF_8)
        val buf = ByteBuffer.allocate(4 + 1 + n.size + 4 + 4).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_ROM_ID)
        buf.put(n.size.toByte())
        buf.put(n)
        buf.putInt(cartCrc32)
        buf.putInt(biosCrc32)
        return buf.array()
    }
}

/** Host → client: begin emulating at frame 0 now. */
data class StartPacket(val startEpochMillis: Long) {
    fun encode(): ByteArray {
        val buf = ByteBuffer.allocate(4 + 8).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_START)
        buf.putLong(startEpochMillis)
        return buf.array()
    }
}

/**
 * Periodic sanity ping — host sends "at frame N, my state CRC was C".
 * The client compares against its own CRC at frame N and replies with
 * a KEYFRAME_ACK marking match / mismatch.
 */
data class KeyframePacket(val frame: Int, val crc32: Int) {
    fun encode(): ByteArray {
        val buf = ByteBuffer.allocate(4 + 4 + 4).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_KEYFRAME)
        buf.putInt(frame)
        buf.putInt(crc32)
        return buf.array()
    }
}

data class KeyframeAckPacket(val frame: Int, val ok: Boolean) {
    fun encode(): ByteArray {
        val buf = ByteBuffer.allocate(4 + 4 + 1).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_KEYFRAME_ACK)
        buf.putInt(frame)
        buf.put(if (ok) 1 else 0)
        return buf.array()
    }
}

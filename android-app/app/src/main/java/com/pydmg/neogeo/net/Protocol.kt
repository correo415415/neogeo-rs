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
    const val VERSION: Byte = 0x01

    const val OP_HELLO: Byte        = 0x10
    const val OP_HELLO_ACK: Byte    = 0x11
    const val OP_ROM_ID: Byte       = 0x20
    const val OP_START: Byte        = 0x30
    const val OP_INPUT: Byte        = 0x40
    const val OP_KEYFRAME: Byte     = 0x50
    const val OP_KEYFRAME_ACK: Byte = 0x51
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
data class InputPacket(
    val frameNumber: Int,
    val inputMask: Int,
) {
    fun encode(dst: ByteBuffer) {
        Protocol.writeHeader(dst, Protocol.OP_INPUT)
        dst.putInt(frameNumber)
        dst.putShort((inputMask and 0xFFFF).toShort())
        // Padding to a round 12-byte packet for alignment. Not strictly
        // required but keeps Wireshark output tidy.
        dst.putShort(0)
    }

    companion object {
        /** Parses a validated INPUT packet from `src`. Returns null if
         *  the header magic / version / opcode don't match. */
        fun decode(src: ByteBuffer): InputPacket? {
            src.order(ByteOrder.LITTLE_ENDIAN)
            if (src.remaining() < 12) return null
            if (src.get() != Protocol.MAGIC_0) return null
            if (src.get() != Protocol.MAGIC_1) return null
            if (src.get() != Protocol.VERSION) return null
            if (src.get() != Protocol.OP_INPUT) return null
            val fn = src.int
            val mask = src.short.toInt() and 0xFFFF
            src.short  // padding
            return InputPacket(fn, mask)
        }
    }
}

/** TCP handshake payload — sent client → host on connect. */
data class HelloPacket(
    val nickname: String,   // free-form, purely cosmetic
) {
    fun encode(): ByteArray {
        val nick = nickname.take(24).toByteArray(Charsets.UTF_8)
        val buf = ByteBuffer.allocate(4 + 1 + nick.size).order(ByteOrder.LITTLE_ENDIAN)
        Protocol.writeHeader(buf, Protocol.OP_HELLO)
        buf.put(nick.size.toByte())
        buf.put(nick)
        return buf.array()
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

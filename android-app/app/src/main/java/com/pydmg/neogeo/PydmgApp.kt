package com.pydmg.neogeo

import android.app.Application
import android.content.Context
import android.net.Uri
import java.io.ByteArrayOutputStream

/**
 * App-scoped singletons:
 *   * [Prefs] — persistent settings
 *   * [biosBytesCache] — most recently loaded BIOS bytes (so we can re-stage
 *     them into the Rust core when launching a new cart without going back
 *     to the SAF picker).
 *   * [library] — current ROM library snapshot (lives across activities).
 *
 * No Hilt / Dagger — the surface area is small enough that a couple of
 * top-level objects are cleaner than a DI graph.
 */
class PydmgApp : Application() {

    override fun onCreate() {
        super.onCreate()
        instance = this
        prefsImpl = Prefs(this)
    }

    /** Cached BIOS bytes for re-staging on each cart launch. */
    @Volatile var biosBytesCache: ByteArray? = null

    /** Currently scanned ROM library. */
    @Volatile var library: List<RomEntry> = emptyList()

    /**
     * Active LAN netplay session, or null if the user is playing solo.
     * Set by [NetplayActivity] just before it launches
     * [EmulatorActivity]; consumed and cleared by EmulatorActivity in
     * its onDestroy() so the socket doesn't leak past the game.
     */
    @Volatile var netSession: com.pydmg.neogeo.net.NetplaySession? = null

    companion object {
        private lateinit var instance: PydmgApp
        private lateinit var prefsImpl: Prefs
        val prefs: Prefs get() = prefsImpl
        val app: PydmgApp get() = instance
    }
}

/** A single discovered ROM entry. */
data class RomEntry(
    val name: String,
    val uri: Uri,
    val isBios: Boolean,
    val sizeBytes: Long,
)

/** Reads any content://-style URI fully into RAM. */
internal fun Context.readAllBytes(uri: Uri): ByteArray? = try {
    contentResolver.openInputStream(uri)?.use { ins ->
        val bos = ByteArrayOutputStream()
        val buf = ByteArray(64 * 1024)
        while (true) {
            val n = ins.read(buf)
            if (n <= 0) break
            bos.write(buf, 0, n)
        }
        bos.toByteArray()
    }
} catch (_: Throwable) {
    null
}

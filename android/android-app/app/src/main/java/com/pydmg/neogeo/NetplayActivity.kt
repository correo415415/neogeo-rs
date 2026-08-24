package com.pydmg.neogeo

import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.view.View
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import com.pydmg.neogeo.net.GameMismatchException
import com.pydmg.neogeo.net.LanDiscovery
import com.pydmg.neogeo.net.NetplaySession
import com.pydmg.neogeo.net.Protocol
import java.net.NetworkInterface

/**
 * LAN netplay setup screen.
 *
 * Two roles the user can pick:
 *
 *   * **Crear sala** (Host): the app binds the TCP port, advertises
 *     itself via NSD/mDNS *including the game name as a TXT record*,
 *     and waits for the peer to connect. Once it does, we finish()
 *     into [EmulatorActivity] with the session handle stored on
 *     [PydmgApp].
 *
 *   * **Unirse a sala** (Client): scans mDNS for rooms and shows
 *     ONLY the ones running the *same game* we just loaded (the TXT
 *     `game` attribute must match [Prefs.lastCartName]); a tap on
 *     one connects. There's also an "IP manual" button for networks
 *     where mDNS is blocked (some enterprise WiFi, guest networks
 *     with client isolation, etc.) — even then, the host re-checks
 *     the game name during the TCP handshake and rejects mismatches,
 *     so filtering isn't just cosmetic.
 *
 * This activity is launched *after* the user has picked a ROM from
 * LibraryActivity — [PydmgApp.pendingCart] holds the ROM already.
 * The netplay session is created; both peers then jump into
 * EmulatorActivity which knows to talk to `PydmgApp.netSession`
 * instead of reading pads directly for the remote player.
 */
class NetplayActivity : AppCompatActivity() {

    private lateinit var status: TextView
    private lateinit var progress: ProgressBar
    private lateinit var listPeers: ListView
    private lateinit var editIp: EditText
    private lateinit var btnConnectIp: Button
    private lateinit var btnHost: Button
    private lateinit var btnCancel: Button

    private var discovery: LanDiscovery? = null
    private val peerAdapter by lazy {
        ArrayAdapter<LanDiscovery.Peer>(this, android.R.layout.simple_list_item_1)
    }

    private var pendingThread: Thread? = null

    /** El juego que este dispositivo acaba de cargar — las salas se
     *  filtran/crean con este nombre de set. */
    private val myGame: String get() = PydmgApp.prefs.lastCartName

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Portrait layout is built programmatically — no XML needed, the
        // activity is only ~150dp tall of content and this saves us a
        // resource file.
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 96, 48, 48)
        }
        val title = TextView(this).apply {
            text = getString(R.string.netplay_title)
            textSize = 24f
        }
        status = TextView(this).apply {
            text = getString(R.string.netplay_pick_a_role_game, myGame)
            setPadding(0, 32, 0, 16)
        }
        progress = ProgressBar(this).apply { visibility = View.GONE }
        btnHost = Button(this).apply { text = getString(R.string.netplay_host) }
        // NOTE: renamed from `hint` to `hintLabel` on purpose — the
        // original v3.2 code shadowed the name with a local val holding
        // a TextView, which then collided with the EditText's `hint`
        // property setter a few lines below and produced
        //   "Val cannot be reassigned" + "inferred type is String but
        //    TextView was expected"
        // The old code compiled purely by accident on older Kotlin
        // versions; 1.9.x is stricter about this shadowing.
        val hintLabel = TextView(this).apply {
            text = getString(R.string.netplay_or)
            setPadding(0, 24, 0, 8)
        }
        listPeers = ListView(this).apply { adapter = peerAdapter }
        val manualRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        editIp = EditText(this).apply {
            // Explicit setHint() so we never shadow with a local val
            // called `hint` again. Cheaper than defensive coding — the
            // property assignment `hint = ...` also works, but the
            // setter form is bullet-proof against future refactors.
            setHint(getString(R.string.netplay_ip_hint))
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        btnConnectIp = Button(this).apply { text = getString(R.string.netplay_join_manual) }
        manualRow.addView(editIp); manualRow.addView(btnConnectIp)
        btnCancel = Button(this).apply { text = getString(R.string.netplay_cancel) }

        root.addView(title)
        root.addView(status)
        root.addView(progress)
        root.addView(btnHost)
        root.addView(hintLabel)
        root.addView(listPeers)
        root.addView(manualRow)
        root.addView(btnCancel)
        setContentView(root)

        btnHost.setOnClickListener { startHosting() }
        btnConnectIp.setOnClickListener {
            val ip = editIp.text.toString().trim()
            if (ip.isNotEmpty()) connectClient(ip)
        }
        listPeers.setOnItemClickListener { _, _, position, _ ->
            val peer = peerAdapter.getItem(position) ?: return@setOnItemClickListener
            connectClient(peer.host.hostAddress ?: return@setOnItemClickListener)
        }
        btnCancel.setOnClickListener { finish() }

        startDiscovery()
    }

    override fun onDestroy() {
        try { discovery?.tearDown() } catch (_: Throwable) {}
        pendingThread?.interrupt()
        super.onDestroy()
    }

    // ----- Discovery -----

    private fun startDiscovery() {
        val d = LanDiscovery(this)
        d.onPeerFound = { peer ->
            runOnUiThread {
                if (peer != null) {
                    // Solo salas del MISMO juego. Las salas de hosts con
                    // versiones antiguas (sin TXT `game`) tampoco se
                    // muestran: no podemos garantizar que coincidan y el
                    // handshake las rechazaría igualmente.
                    if (peer.gameName != myGame) return@runOnUiThread
                    // Avoid dupes when the mDNS TTL refreshes.
                    val present = (0 until peerAdapter.count).any {
                        peerAdapter.getItem(it)?.serviceName == peer.serviceName
                    }
                    if (!present) peerAdapter.add(peer)
                } else {
                    // Peer lost — a full re-scan on next refresh is fine.
                }
            }
        }
        d.startDiscovery()
        discovery = d
    }

    // ----- Host role -----

    private fun startHosting() {
        btnHost.isEnabled = false
        listPeers.visibility = View.GONE
        editIp.visibility = View.GONE
        btnConnectIp.visibility = View.GONE
        progress.visibility = View.VISIBLE

        val serviceName = "pydmg-${android.os.Build.MODEL.take(16).replace(' ', '_')}"
        val localIps = collectLocalIps().joinToString(" / ")
        status.text = getString(R.string.netplay_hosting_wait_game, myGame, localIps,
            Protocol.DEFAULT_TCP_PORT)

        // El nombre del juego viaja en el TXT record: los clientes solo
        // verán esta sala si cargaron el mismo set.
        discovery?.registerHost(serviceName, Protocol.DEFAULT_TCP_PORT, myGame)

        pendingThread = Thread {
            try {
                val session = NetplaySession.acceptAsHost(gameName = myGame)
                PydmgApp.app.netSession = session
                runOnUiThread {
                    Toast.makeText(this, R.string.netplay_ready, Toast.LENGTH_SHORT).show()
                    startActivity(Intent(this, EmulatorActivity::class.java))
                    finish()
                }
            } catch (t: Throwable) {
                Log.w(TAG, "host accept failed", t)
                runOnUiThread {
                    status.text = getString(R.string.netplay_error, t.message ?: "?")
                    progress.visibility = View.GONE
                    btnHost.isEnabled = true
                    listPeers.visibility = View.VISIBLE
                    editIp.visibility = View.VISIBLE
                    btnConnectIp.visibility = View.VISIBLE
                }
            }
        }.apply { isDaemon = true; start() }
    }

    // ----- Client role -----

    private fun connectClient(ipOrHost: String) {
        btnHost.isEnabled = false
        listPeers.isEnabled = false
        btnConnectIp.isEnabled = false
        progress.visibility = View.VISIBLE
        status.text = getString(R.string.netplay_connecting, ipOrHost)

        pendingThread = Thread {
            try {
                val session = NetplaySession.connectAsClient(ipOrHost, gameName = myGame)
                PydmgApp.app.netSession = session
                runOnUiThread {
                    Toast.makeText(this, R.string.netplay_ready, Toast.LENGTH_SHORT).show()
                    startActivity(Intent(this, EmulatorActivity::class.java))
                    finish()
                }
            } catch (t: Throwable) {
                Log.w(TAG, "client connect failed", t)
                runOnUiThread {
                    status.text = if (t is GameMismatchException)
                        getString(R.string.netplay_game_mismatch, myGame)
                    else
                        getString(R.string.netplay_error, t.message ?: "?")
                    progress.visibility = View.GONE
                    btnHost.isEnabled = true
                    listPeers.isEnabled = true
                    btnConnectIp.isEnabled = true
                }
            }
        }.apply { isDaemon = true; start() }
    }

    /** All non-loopback IPv4 addresses currently assigned to the
     *  device — shown so the user can tell the peer "join 192.168.1.42"
     *  when mDNS isn't reaching them. */
    private fun collectLocalIps(): List<String> {
        return try {
            NetworkInterface.getNetworkInterfaces().toList().flatMap { nif ->
                nif.inetAddresses.toList()
                    .filter { !it.isLoopbackAddress && it.hostAddress?.contains(':') == false }
                    .map { it.hostAddress ?: "?" }
            }
        } catch (_: Throwable) { emptyList() }
    }

    companion object { private const val TAG = "netplay-ui" }
}

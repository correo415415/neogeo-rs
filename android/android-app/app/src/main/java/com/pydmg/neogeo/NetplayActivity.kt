package com.pydmg.neogeo

import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import com.pydmg.neogeo.net.GameMismatchException
import com.pydmg.neogeo.net.LanDiscovery
import com.pydmg.neogeo.net.NetplaySession
import com.pydmg.neogeo.net.Protocol
import java.net.NetworkInterface

/**
 * LAN netplay screen. Two modes, chosen in the launch chooser
 * (see LibraryActivity.showLaunchChooser):
 *
 *   * **MODE_HOST** ("Crear sala"): binds the TCP port, advertises
 *     the room via NSD/mDNS *including the game name as a TXT
 *     record*, and shows a "room open, waiting for P2" card until
 *     the peer connects. Then jumps into [EmulatorActivity] with the
 *     session handle stored on [PydmgApp].
 *
 *   * **MODE_JOIN** ("Unirse a sala"): scans mDNS and lists ONLY
 *     rooms running the *same game* we just loaded (TXT `game`
 *     attribute must match [Prefs.lastCartName]) as tappable cards.
 *     Manual IP entry remains available for networks where mDNS is
 *     blocked — the host still re-checks the game name during the
 *     TCP handshake and rejects mismatches, so filtering isn't just
 *     cosmetic.
 */
class NetplayActivity : AppCompatActivity() {

    // Shared
    private lateinit var netGame: TextView
    private lateinit var netStatus: TextView
    private lateinit var btnCancel: Button

    // Host panel
    private lateinit var hostPanel: LinearLayout
    private lateinit var hostStatus: TextView
    private lateinit var hostIps: TextView

    // Join panel
    private lateinit var joinPanel: LinearLayout
    private lateinit var roomList: ListView
    private lateinit var joinEmpty: View
    private lateinit var editIp: EditText
    private lateinit var btnConnectIp: Button

    private var discovery: LanDiscovery? = null
    private var pendingThread: Thread? = null

    private val rooms = ArrayList<LanDiscovery.Peer>()
    private val roomAdapter by lazy { RoomAdapter() }

    /** El juego que este dispositivo acaba de cargar — las salas se
     *  filtran/crean con este nombre de set. */
    private val myGame: String get() = PydmgApp.prefs.lastCartName

    /** Modo con el que se abrió la pantalla (del selector de arranque). */
    private var mode: Int = MODE_JOIN

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        mode = intent.getIntExtra(EXTRA_MODE, MODE_JOIN)
        setContentView(R.layout.activity_netplay)

        netGame = findViewById(R.id.net_game)
        netStatus = findViewById(R.id.net_status)
        btnCancel = findViewById(R.id.btn_cancel)
        hostPanel = findViewById(R.id.host_panel)
        hostStatus = findViewById(R.id.host_status)
        hostIps = findViewById(R.id.host_ips)
        joinPanel = findViewById(R.id.join_panel)
        roomList = findViewById(R.id.room_list)
        joinEmpty = findViewById(R.id.join_empty)
        editIp = findViewById(R.id.edit_ip)
        btnConnectIp = findViewById(R.id.btn_connect_ip)

        netGame.text = NeoGeoGames.lookup(myGame)?.title ?: myGame

        roomList.adapter = roomAdapter
        roomList.setOnItemClickListener { _, _, position, _ ->
            val peer = rooms.getOrNull(position) ?: return@setOnItemClickListener
            connectClient(peer.host.hostAddress ?: return@setOnItemClickListener)
        }
        btnConnectIp.setOnClickListener {
            val ip = editIp.text.toString().trim()
            if (ip.isNotEmpty()) connectClient(ip)
        }
        btnCancel.setOnClickListener { finish() }

        when (mode) {
            MODE_HOST -> {
                hostPanel.visibility = View.VISIBLE
                startHosting()
            }
            else -> {
                joinPanel.visibility = View.VISIBLE
                startDiscovery()
            }
        }
    }

    override fun onDestroy() {
        try { discovery?.tearDown() } catch (_: Throwable) {}
        pendingThread?.interrupt()
        super.onDestroy()
    }

    // ----- Discovery (JOIN mode) -----

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
                    if (rooms.none { it.serviceName == peer.serviceName }) {
                        rooms.add(peer)
                        roomAdapter.notifyDataSetChanged()
                        joinEmpty.visibility = View.GONE
                    }
                }
            }
        }
        d.startDiscovery()
        discovery = d
    }

    // ----- Host role -----

    private fun startHosting() {
        val serviceName = "pydmg-${android.os.Build.MODEL.take(16).replace(' ', '_')}"
        val localIps = collectLocalIps().joinToString(" / ")
        hostIps.text = getString(R.string.netplay_host_ips, localIps, Protocol.DEFAULT_TCP_PORT)

        // El nombre del juego viaja en el TXT record: los clientes solo
        // verán esta sala si cargaron el mismo set.
        if (discovery == null) discovery = LanDiscovery(this)
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
                    showStatus(getString(R.string.netplay_error, t.message ?: "?"))
                }
            }
        }.apply { isDaemon = true; start() }
    }

    // ----- Client role -----

    private fun connectClient(ipOrHost: String) {
        roomList.isEnabled = false
        btnConnectIp.isEnabled = false
        showStatus(getString(R.string.netplay_connecting, ipOrHost))

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
                    showStatus(
                        if (t is GameMismatchException)
                            getString(R.string.netplay_game_mismatch, myGame)
                        else
                            getString(R.string.netplay_error, t.message ?: "?"))
                    roomList.isEnabled = true
                    btnConnectIp.isEnabled = true
                }
            }
        }.apply { isDaemon = true; start() }
    }

    private fun showStatus(text: String) {
        netStatus.text = text
        netStatus.visibility = View.VISIBLE
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

    // ----- Room list adapter (cards) -----

    private inner class RoomAdapter : BaseAdapter() {
        override fun getCount(): Int = rooms.size
        override fun getItem(position: Int): Any = rooms[position]
        override fun getItemId(position: Int): Long = position.toLong()

        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
            val v = convertView ?: LayoutInflater.from(this@NetplayActivity)
                .inflate(R.layout.item_room, parent, false)
            val peer = rooms[position]
            v.findViewById<TextView>(R.id.room_name).text =
                peer.serviceName.removePrefix("pydmg-").replace('_', ' ')
            v.findViewById<TextView>(R.id.room_detail).text =
                "${peer.host.hostAddress}:${peer.port}"
            return v
        }
    }

    companion object {
        private const val TAG = "netplay-ui"
        const val EXTRA_MODE = "netplay_mode"
        const val MODE_HOST = 1
        const val MODE_JOIN = 2
    }
}

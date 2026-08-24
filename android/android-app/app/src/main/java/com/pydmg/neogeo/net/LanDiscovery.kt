package com.pydmg.neogeo.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.util.Log
import java.net.InetAddress
import java.util.concurrent.ConcurrentHashMap

/**
 * LAN peer discovery over multicast DNS via Android's built-in
 * [NsdManager] (works on API 16+ so it covers our minSdk 24 easily).
 *
 * Service type: `_pydmg-neogeo._tcp.` (lowercase, as required by mDNS).
 *
 * Two roles:
 *
 *   * The **host** calls [registerHost] on the port it just bound
 *     ([Protocol.DEFAULT_TCP_PORT]). Its device shows up in every
 *     LAN scanner running the app.
 *
 *   * The **client** calls [startDiscovery]; as peers appear on the
 *     network, they're delivered to [onPeerFound] with their IP.
 *
 * When a session actually starts, both peers should call
 * [tearDown] so the mDNS entry disappears (avoids stale entries
 * lingering in the local resolver cache).
 */
class LanDiscovery(context: Context) {

    private val nsd: NsdManager =
        context.applicationContext.getSystemService(Context.NSD_SERVICE) as NsdManager

    private var regListener: NsdManager.RegistrationListener? = null
    private var discListener: NsdManager.DiscoveryListener? = null

    /** Callback invoked on the main thread when a peer is discovered.
     *  Called with `null` when a previously-found peer disappears. */
    var onPeerFound: ((Peer?) -> Unit)? = null

    /** Snapshot of currently visible peers keyed by service name. */
    private val visible = ConcurrentHashMap<String, Peer>()

    data class Peer(
        val serviceName: String,
        val host: InetAddress,
        val port: Int,
    ) {
        override fun toString(): String = "$serviceName · ${host.hostAddress}:$port"
    }

    /** Register the local device as a joinable host under
     *  [serviceName] (defaults to a stable ID from Build.MODEL). */
    fun registerHost(
        serviceName: String,
        tcpPort: Int = Protocol.DEFAULT_TCP_PORT,
    ) {
        val info = NsdServiceInfo().apply {
            this.serviceName = serviceName
            this.serviceType = SERVICE_TYPE
            this.port = tcpPort
        }
        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(nsi: NsdServiceInfo) {
                Log.i(TAG, "registered as '${nsi.serviceName}' on port ${nsi.port}")
            }
            override fun onServiceUnregistered(nsi: NsdServiceInfo) {
                Log.i(TAG, "unregistered '${nsi.serviceName}'")
            }
            override fun onRegistrationFailed(nsi: NsdServiceInfo, err: Int) {
                Log.w(TAG, "registration failed for '${nsi.serviceName}': err=$err")
            }
            override fun onUnregistrationFailed(nsi: NsdServiceInfo, err: Int) {
                Log.w(TAG, "unregistration failed: err=$err")
            }
        }
        regListener = listener
        nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
    }

    /** Begin scanning for hosts. Peers land in [onPeerFound]. */
    fun startDiscovery() {
        val listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(t: String) {
                Log.i(TAG, "discovery started ($t)")
            }
            override fun onDiscoveryStopped(t: String) {
                Log.i(TAG, "discovery stopped")
            }
            override fun onStartDiscoveryFailed(t: String, err: Int) {
                Log.w(TAG, "discovery start failed err=$err")
            }
            override fun onStopDiscoveryFailed(t: String, err: Int) {
                Log.w(TAG, "discovery stop failed err=$err")
            }
            override fun onServiceFound(nsi: NsdServiceInfo) {
                // Resolve to get IP + port. Must be async, NsdManager
                // callbacks are one-shot resolvers.
                nsd.resolveService(nsi, object : NsdManager.ResolveListener {
                    override fun onResolveFailed(nsi: NsdServiceInfo, err: Int) {
                        Log.w(TAG, "resolve failed for '${nsi.serviceName}': $err")
                    }
                    override fun onServiceResolved(nsi: NsdServiceInfo) {
                        val host = nsi.host ?: return
                        val peer = Peer(nsi.serviceName, host, nsi.port)
                        visible[nsi.serviceName] = peer
                        onPeerFound?.invoke(peer)
                    }
                })
            }
            override fun onServiceLost(nsi: NsdServiceInfo) {
                visible.remove(nsi.serviceName)?.let { onPeerFound?.invoke(null) }
            }
        }
        discListener = listener
        nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener)
    }

    fun tearDown() {
        try { regListener?.let { nsd.unregisterService(it) } } catch (_: Throwable) {}
        try { discListener?.let { nsd.stopServiceDiscovery(it) } } catch (_: Throwable) {}
        regListener = null; discListener = null; visible.clear()
    }

    companion object {
        private const val TAG = "netplay-nsd"
        const val SERVICE_TYPE = "_pydmg-neogeo._tcp."
    }
}

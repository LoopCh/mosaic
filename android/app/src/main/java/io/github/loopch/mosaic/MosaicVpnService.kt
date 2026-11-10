package io.github.loopch.mosaic

import android.app.Notification
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import kotlinx.coroutines.CompletableDeferred
import kotlin.concurrent.thread

/**
 * Keeps Mosaic running in the foreground while connected.
 *
 * VPN mode: creates the VPN interface and hands its descriptor to the Rust core,
 * which runs its own TCP/IP stack on it. Mosaic itself is excluded from the VPN
 * (addDisallowedApplication), so the engines and direct connections go straight
 * to the network and never loop back into the tunnel.
 *
 * Proxy mode: just the local SOCKS5/HTTP proxy, kept alive by the foreground service.
 */
class MosaicVpnService : VpnService() {

    companion object {
        const val CHANNEL = "vpn"
        private const val NOTIF_ID = 1
        private const val ACTION_START = "start"
        private const val ACTION_STOP = "stop"
        private const val EXTRA_VPN = "vpn"
        private const val EXTRA_PORT = "port"

        @Volatile private var pending: CompletableDeferred<String>? = null

        /** True while connected through this service (for the tile and boot). */
        @Volatile var running = false
            private set

        /** Android's own kill switch, read while the VPN runs (API 29+): always-on VPN and
         *  "Block connections without VPN". Null until known. */
        @Volatile var alwaysOn: Boolean? = null
            private set
        @Volatile var lockdown: Boolean? = null
            private set

        /** Start and wait for the result: `{"ok":null}` or `{"err":"…"}`. */
        suspend fun start(ctx: Context, vpn: Boolean, port: Int): String {
            val d = CompletableDeferred<String>()
            pending = d
            val i = Intent(ctx, MosaicVpnService::class.java).setAction(ACTION_START).putExtra(EXTRA_VPN, vpn).putExtra(EXTRA_PORT, port)
            ctx.startForegroundService(i)
            return d.await()
        }

        fun stop(ctx: Context) {
            ctx.startService(Intent(ctx, MosaicVpnService::class.java).setAction(ACTION_STOP))
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                thread { shutdown() }
                return START_NOT_STICKY
            }
            ACTION_START -> {
                val vpn = intent!!.getBooleanExtra(EXTRA_VPN, true)
                goForeground(vpn, intent.getIntExtra(EXTRA_PORT, 7890))
                thread {
                    val reply = if (vpn) startVpn() else Native.invoke("connect", "{}")
                    val failed = reply.contains("\"err\"")
                    setRunning(!failed)
                    pending?.complete(reply)
                    pending = null
                    if (failed) {
                        stopForeground(STOP_FOREGROUND_REMOVE)
                        stopSelf()
                    }
                }
            }
            else -> {
                // Restarted by the system ("always-on VPN" or after being killed): reconnect as VPN.
                goForeground(true, 0)
                thread {
                    val failed = startVpn().contains("\"err\"")
                    setRunning(!failed)
                    if (failed) {
                        stopForeground(STOP_FOREGROUND_REMOVE)
                        stopSelf()
                    }
                }
            }
        }
        return START_NOT_STICKY
    }

    private fun startVpn(): String {
        if (prepare(this) != null) return Native.err(getString(R.string.no_permission))
        val builder = Builder()
            .setSession("Mosaic")
            .setMtu(1500)
            .addAddress("172.19.0.1", 30)
            .addRoute("0.0.0.0", 0)
            .addAddress("fdfe:dcba:9876::1", 126)
            .addRoute("::", 0)
            .addDnsServer("172.19.0.2")
        applyApps(builder)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) builder.setMetered(false)
        val pfd = try {
            builder.establish()
        } catch (e: Exception) {
            return Native.err("VPN: ${e.message}")
        } ?: return Native.err(getString(R.string.no_permission))
        val error = Native.startVpn(pfd.detachFd())
        return if (error.isEmpty()) Native.ok() else Native.err(error)
    }

    /**
     * Which apps use the VPN (Settings → Apps). Mosaic itself must never be inside:
     * its engines and direct connections would loop back into the tunnel.
     */
    private fun applyApps(b: Builder) {
        val s = Connector.settings()
        val mode = s.optString("apps_mode", "all")
        val list = s.optJSONArray("apps")
        val apps = (0 until (list?.length() ?: 0)).map { list!!.getString(it) }.filter { it != packageName }
        if (mode == "only" && apps.isNotEmpty()) {
            var added = 0
            for (a in apps) {
                try {
                    b.addAllowedApplication(a)
                    added++
                } catch (_: Exception) {
                    // uninstalled since it was chosen
                }
            }
            if (added > 0) return
        }
        b.addDisallowedApplication(packageName)
        if (mode == "bypass") {
            for (a in apps) {
                try {
                    b.addDisallowedApplication(a)
                } catch (_: Exception) {
                }
            }
        }
    }

    private fun setRunning(on: Boolean) {
        running = on
        if (on && Build.VERSION.SDK_INT >= 29) {
            alwaysOn = isAlwaysOn
            lockdown = isLockdownEnabled
        }
        Connector.refreshTile(this)
    }

    private fun shutdown() {
        setRunning(false)
        Native.invoke("disconnect", "{}")
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onRevoke() {
        // Another VPN app took over, or the user turned Mosaic off in system settings.
        thread { shutdown() }
    }

    override fun onDestroy() {
        setRunning(false)
        thread { Native.invoke("disconnect", "{}") }
        super.onDestroy()
    }

    private fun goForeground(vpn: Boolean, port: Int) {
        val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        ServiceCompat.startForeground(this, NOTIF_ID, notification(vpn, port), type)
    }

    private fun notification(vpn: Boolean, port: Int): Notification {
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), flags)
        val stop = PendingIntent.getService(this, 1, Intent(this, MosaicVpnService::class.java).setAction(ACTION_STOP), flags)
        val title = if (vpn) getString(R.string.notif_vpn) else getString(R.string.notif_proxy, port)
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_notif)
            .setContentTitle(title)
            .setContentText(getString(R.string.notif_text))
            .setContentIntent(open)
            .setOngoing(true)
            .setShowWhen(false)
            .addAction(0, getString(R.string.disconnect), stop)
            .build()
    }
}

package io.github.loopch.mosaic

import android.net.ConnectivityManager
import android.os.Build
import android.os.Process
import android.system.OsConstants
import org.json.JSONObject
import java.net.InetAddress
import java.net.InetSocketAddress
import java.util.concurrent.ConcurrentHashMap

/** The Rust core (libmosaic.so, crates/mosaic-android). Calls block: never use them on the main thread. */
object Native {
    init {
        System.loadLibrary("mosaic")
    }

    /** Returns "" or an error message. */
    @JvmStatic external fun init(dataDir: String, libDir: String, assetsDir: String): String

    /** `{"ok": value}` or `{"err": "message"}` — the same commands as the desktop app. */
    @JvmStatic external fun invoke(cmd: String, args: String): String

    /** Connect in VPN mode on [fd] (ownership passes to the core). Returns "" or an error message. */
    @JvmStatic external fun startVpn(fd: Int): String

    private val uidNames = ConcurrentHashMap<Int, String>()

    /**
     * Called by the core (from its own threads) for each new flow in VPN mode: the package
     * that owns the connection, for the Connections list and rules by app.
     */
    @JvmStatic
    fun ownerOf(tcp: Boolean, srcIp: String, srcPort: Int, dstIp: String, dstPort: Int): String? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
        val ctx = MosaicApp.instance ?: return null
        return try {
            val cm = ctx.getSystemService(ConnectivityManager::class.java)
            val uid = cm.getConnectionOwnerUid(
                if (tcp) OsConstants.IPPROTO_TCP else OsConstants.IPPROTO_UDP,
                InetSocketAddress(InetAddress.getByName(srcIp), srcPort),
                InetSocketAddress(InetAddress.getByName(dstIp), dstPort),
            )
            if (uid == Process.INVALID_UID) null
            else uidNames.getOrPut(uid) { ctx.packageManager.getPackagesForUid(uid)?.firstOrNull() ?: "uid:$uid" }
        } catch (e: Exception) {
            null
        }
    }

    fun ok(value: Any? = null): String = JSONObject().put("ok", value ?: JSONObject.NULL).toString()
    fun err(message: String): String = JSONObject().put("err", message).toString()

    /** Current settings.mode from a snapshot ("system_proxy", "proxy" or "tun"). */
    fun mode(): String = try {
        JSONObject(invoke("snapshot", "{}")).getJSONObject("ok").getJSONObject("settings").getString("mode")
    } catch (e: Exception) {
        "tun"
    }

    fun listenPort(): Int = try {
        JSONObject(invoke("snapshot", "{}")).getJSONObject("ok").getJSONObject("settings").getInt("listen_port")
    } catch (e: Exception) {
        7890
    }
}

package io.github.loopch.mosaic

import android.content.ComponentName
import android.content.Context
import android.net.VpnService
import android.service.quicksettings.TileService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/** Connecting from anywhere: the window, the Quick Settings tile, boot. */
object Connector {

    /** settings from a fresh snapshot (empty object if the core is not ready). */
    fun settings(): JSONObject = try {
        JSONObject(Native.invoke("snapshot", "{}")).getJSONObject("ok").getJSONObject("settings")
    } catch (e: Exception) {
        JSONObject()
    }

    fun connected(): Boolean = try {
        JSONObject(Native.invoke("snapshot", "{}")).getJSONObject("ok").getJSONObject("status").getBoolean("connected")
    } catch (e: Exception) {
        false
    }

    /** VPN mode needs the user's one-time permission; proxy mode does not. */
    fun needsPermission(ctx: Context, settings: JSONObject = settings()): Boolean =
        settings.optString("mode", "tun") != "proxy" && VpnService.prepare(ctx) != null

    /** Start the foreground service and wait for the result (`{"ok":…}` / `{"err":…}`). Permission must be granted. */
    suspend fun start(ctx: Context): String {
        val s = withContext(Dispatchers.IO) { settings() }
        val vpn = s.optString("mode", "tun") != "proxy"
        return MosaicVpnService.start(ctx.applicationContext, vpn, s.optInt("listen_port", 7890))
    }

    fun stop(ctx: Context) = MosaicVpnService.stop(ctx.applicationContext)

    /** Ask the Quick Settings tile to redraw. */
    fun refreshTile(ctx: Context) {
        try {
            TileService.requestListeningState(ctx, ComponentName(ctx, MosaicTile::class.java))
        } catch (_: Exception) {
        }
    }
}

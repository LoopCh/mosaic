package io.github.loopch.mosaic

import org.json.JSONObject

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

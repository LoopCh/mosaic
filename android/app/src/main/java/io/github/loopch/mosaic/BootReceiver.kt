package io.github.loopch.mosaic

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

/** "Connect automatically": connect after the phone starts (or after Mosaic is updated). */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED && intent.action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        val pending = goAsync()
        CoroutineScope(Dispatchers.IO).launch {
            try {
                val s = Connector.settings()
                if (s.optBoolean("auto_connect") && !MosaicVpnService.running && !Connector.needsPermission(ctx, s)) {
                    Connector.start(ctx)
                }
            } catch (_: Exception) {
            } finally {
                pending.finish()
            }
        }
    }
}

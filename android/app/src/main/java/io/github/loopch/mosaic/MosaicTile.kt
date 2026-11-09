package io.github.loopch.mosaic

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Quick Settings tile: one tap connects or disconnects. */
class MosaicTile : TileService() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    override fun onStartListening() {
        draw(MosaicVpnService.running)
    }

    override fun onClick() {
        if (MosaicVpnService.running) {
            Connector.stop(this)
            draw(false)
            return
        }
        scope.launch {
            val needsPermission = withContext(Dispatchers.IO) { Connector.needsPermission(this@MosaicTile) }
            if (needsPermission) {
                openApp(connect = true)
                return@launch
            }
            draw(true, busy = true)
            val reply = try {
                Connector.start(this@MosaicTile)
            } catch (e: Exception) {
                // Android may refuse to start a foreground service from here: let the window do it.
                openApp(connect = true)
                return@launch
            }
            draw(!reply.contains("\"err\""))
        }
    }

    @SuppressLint("StartActivityAndCollapseDeprecated")
    private fun openApp(connect: Boolean) {
        val i = Intent(this, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            .putExtra(MainActivity.EXTRA_CONNECT, connect)
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(PendingIntent.getActivity(this, 2, i, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(i)
        }
    }

    private fun draw(on: Boolean, busy: Boolean = false) {
        val tile = qsTile ?: return
        tile.state = if (on) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        if (Build.VERSION.SDK_INT >= 29) tile.subtitle = if (busy) "…" else null
        tile.updateTile()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }
}

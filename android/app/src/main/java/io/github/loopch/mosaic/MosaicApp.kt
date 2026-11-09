package io.github.loopch.mosaic

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.util.Log
import java.io.File

class MosaicApp : Application() {
    companion object {
        @Volatile var instance: MosaicApp? = null
            private set
    }

    override fun onCreate() {
        super.onCreate()
        instance = this
        val geo = File(filesDir, "geo")
        copyGeoFiles(geo)
        val err = Native.init(File(filesDir, "mosaic").absolutePath, applicationInfo.nativeLibraryDir, geo.absolutePath)
        if (err.isNotEmpty()) Log.e("Mosaic", err)

        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(MosaicVpnService.CHANNEL, getString(R.string.channel_name), NotificationManager.IMPORTANCE_LOW)
        )
    }

    /** geoip.dat / geosite.dat ship as assets; refresh them when the app is updated. */
    private fun copyGeoFiles(dir: File) {
        dir.mkdirs()
        val stamp = File(dir, ".version")
        val version = packageManager.getPackageInfo(packageName, 0).lastUpdateTime.toString()
        if (stamp.exists() && stamp.readText() == version) return
        val names = assets.list("geo") ?: emptyArray()
        for (name in names) {
            assets.open("geo/$name").use { input -> File(dir, name).outputStream().use { input.copyTo(it) } }
        }
        stamp.writeText(version)
    }
}

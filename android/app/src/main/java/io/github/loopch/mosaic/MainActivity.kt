package io.github.loopch.mosaic

import android.Manifest
import android.annotation.SuppressLint
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.graphics.Bitmap
import android.graphics.Canvas
import android.content.pm.PackageManager
import android.net.Uri
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.lifecycleScope
import androidx.webkit.WebViewAssetLoader
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream

/**
 * The window: the desktop UI (app/ui) in a WebView. `invoke(cmd, args)` from JS goes
 * to the Rust core; connect/disconnect go through MosaicVpnService so the
 * connection survives when the window is closed.
 */
class MainActivity : ComponentActivity() {

    companion object {
        /** Connect right away (from the Quick Settings tile when it could not do it itself). */
        const val EXTRA_CONNECT = "connect"
    }

    private lateinit var web: WebView
    private var vpnPermission: CompletableDeferred<Boolean>? = null
    private var qrResult: CompletableDeferred<String?>? = null

    private val scanQr = registerForActivityResult(ScanContract()) { r -> qrResult?.complete(r.contents) }

    private val askVpn = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
        vpnPermission?.complete(r.resultCode == RESULT_OK)
    }
    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val loader = WebViewAssetLoader.Builder()
            .addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(this))
            // App icons for Settings → Apps: /appicon/<package>.png
            .addPathHandler("/appicon/") { path -> appIcon(path.removeSuffix(".png")) }
            .build()
        web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.allowFileAccess = false
            setBackgroundColor(0xFF0A0C12.toInt())
            webViewClient = object : WebViewClient() {
                override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
                    loader.shouldInterceptRequest(request.url)

                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    if (request.url.host == "appassets.androidplatform.net") return false
                    openUrl(request.url.toString())
                    return true
                }
            }
            addJavascriptInterface(Bridge(), "MosaicBridge")
            loadUrl("https://appassets.androidplatform.net/assets/ui/index.html")
        }
        setContentView(web)

        maybeAutoConnect(intent)

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                moveTaskToBack(true)
            }
        })
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        maybeAutoConnect(intent)
    }

    /** "Connect automatically" (when Mosaic is opened) or a request from the tile. */
    private fun maybeAutoConnect(intent: Intent?) {
        val asked = intent?.getBooleanExtra(EXTRA_CONNECT, false) == true
        intent?.removeExtra(EXTRA_CONNECT)
        lifecycleScope.launch {
            val auto = withContext(Dispatchers.IO) { Connector.settings().optBoolean("auto_connect") }
            if ((asked || auto) && !MosaicVpnService.running) {
                val reply = connect()
                if (reply.contains("\"err\"")) {
                    val msg = JSONObject(reply).optString("err")
                    web.evaluateJavascript("window.toast && toast(${JSONObject.quote(msg)}, 'error')", null)
                }
            }
        }
    }

    private fun openUrl(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: Exception) {
        }
    }

    /** Connect through the foreground service (asking for the VPN permission when needed). */
    private suspend fun connect(): String {
        val settings = withContext(Dispatchers.IO) { Connector.settings() }
        if (settings.optString("mode", "tun") != "proxy") {
            val intent = VpnService.prepare(this)
            if (intent != null) {
                val d = CompletableDeferred<Boolean>()
                vpnPermission = d
                withContext(Dispatchers.Main) { askVpn.launch(intent) }
                if (!d.await()) return Native.err(getString(R.string.no_permission))
            }
        }
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            withContext(Dispatchers.Main) { askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS) }
        }
        return Connector.start(this)
    }

    private suspend fun disconnect(): String {
        Connector.stop(this)
        return withContext(Dispatchers.IO) { Native.invoke("disconnect", "{}") }
    }

    private suspend fun handle(cmd: String, args: String): String = when (cmd) {
        "connect" -> connect()
        "disconnect" -> disconnect()
        "open_url" -> {
            val url = try { JSONObject(args).getString("url") } catch (e: Exception) { "" }
            if (url.startsWith("https://")) withContext(Dispatchers.Main) { openUrl(url) }
            Native.ok()
        }
        // The VPN interface belongs to the service: when a change needs a reconnect,
        // reconnect through it rather than inside the core.
        "set_settings" -> {
            val wasConnected = withContext(Dispatchers.IO) { Connector.connected() }
            if (wasConnected) disconnect()
            val reply = withContext(Dispatchers.IO) { Native.invoke(cmd, args) }
            if (wasConnected) {
                val r = connect()
                if (r.contains("\"err\"")) r else reply
            } else reply
        }
        "read_clipboard" -> {
            val cm = getSystemService(ClipboardManager::class.java)
            val text = cm.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(this)?.toString() ?: ""
            Native.ok(text)
        }
        "scan_qr" -> {
            val d = CompletableDeferred<String?>()
            qrResult = d
            scanQr.launch(
                ScanOptions()
                    .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                    .setPrompt("")
                    .setBeepEnabled(false)
                    .setOrientationLocked(false)
            )
            Native.ok(d.await() ?: "")
        }
        "list_apps" -> withContext(Dispatchers.IO) { Native.ok(listApps()) }
        else -> withContext(Dispatchers.IO) { Native.invoke(cmd, args) }
    }

    /** Launchable apps with a small icon (data: URL), for Settings → Apps. */
    private fun listApps(): JSONArray {
        val pm = packageManager
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        val seen = HashSet<String>()
        val out = JSONArray()
        @Suppress("DEPRECATION")
        for (ri in pm.queryIntentActivities(launcher, 0)) {
            val ai: ApplicationInfo = ri.activityInfo.applicationInfo
            if (ai.packageName == packageName || !seen.add(ai.packageName)) continue
            out.put(
                JSONObject()
                    .put("pkg", ai.packageName)
                    .put("name", ri.loadLabel(pm).toString())
                    .put("icon", "/appicon/${ai.packageName}.png")
            )
        }
        return out
    }

    /** A launcher icon as a small PNG, served to the WebView (loaded lazily by <img>). */
    private fun appIcon(pkg: String): WebResourceResponse? = try {
        val d = packageManager.getApplicationIcon(pkg)
        val size = 72
        val bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        d.setBounds(0, 0, size, size)
        d.draw(Canvas(bmp))
        val bytes = ByteArrayOutputStream().also { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
        WebResourceResponse("image/png", null, ByteArrayInputStream(bytes))
    } catch (e: Exception) {
        null
    }

    inner class Bridge {
        @JavascriptInterface
        fun invoke(id: Int, cmd: String, args: String) {
            lifecycleScope.launch {
                val reply = try {
                    handle(cmd, args)
                } catch (e: Exception) {
                    Native.err(e.message ?: e.toString())
                }
                web.evaluateJavascript("window.__mosaicReply($id, ${JSONObject.quote(reply)})", null)
            }
        }
    }
}

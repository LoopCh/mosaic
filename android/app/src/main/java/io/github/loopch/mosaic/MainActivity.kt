package io.github.loopch.mosaic

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
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
import org.json.JSONObject

/**
 * The window: the desktop UI (app/ui) in a WebView. `invoke(cmd, args)` from JS goes
 * to the Rust core; connect/disconnect go through MosaicVpnService so the
 * connection survives when the window is closed.
 */
class MainActivity : ComponentActivity() {

    private lateinit var web: WebView
    private var vpnPermission: CompletableDeferred<Boolean>? = null

    private val askVpn = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
        vpnPermission?.complete(r.resultCode == RESULT_OK)
    }
    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val loader = WebViewAssetLoader.Builder()
            .addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(this))
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

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                moveTaskToBack(true)
            }
        })
    }

    private fun openUrl(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: Exception) {
        }
    }

    private fun connected(): Boolean = try {
        JSONObject(Native.invoke("snapshot", "{}")).getJSONObject("ok").getJSONObject("status").getBoolean("connected")
    } catch (e: Exception) {
        false
    }

    /** Connect through the foreground service (asking for the VPN permission when needed). */
    private suspend fun connect(): String {
        val mode = withContext(Dispatchers.IO) { Native.mode() }
        val vpn = mode != "proxy"
        if (vpn) {
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
        val port = withContext(Dispatchers.IO) { Native.listenPort() }
        return MosaicVpnService.start(this, vpn, port)
    }

    private suspend fun disconnect(): String {
        MosaicVpnService.stop(this)
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
            val wasConnected = withContext(Dispatchers.IO) { connected() }
            if (wasConnected) disconnect()
            val reply = withContext(Dispatchers.IO) { Native.invoke(cmd, args) }
            if (wasConnected) {
                val r = connect()
                if (r.contains("\"err\"")) r else reply
            } else reply
        }
        else -> withContext(Dispatchers.IO) { Native.invoke(cmd, args) }
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

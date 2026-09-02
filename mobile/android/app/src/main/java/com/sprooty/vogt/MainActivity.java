package com.sprooty.vogt;

import android.os.Bundle;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.graphics.Typeface;
import android.util.Log;
import android.webkit.JavascriptInterface;
import android.webkit.WebView;
import android.widget.ScrollView;
import android.widget.TextView;

import androidx.appcompat.app.AlertDialog;
import androidx.core.content.ContextCompat;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowInsetsCompat;

import com.getcapacitor.BridgeActivity;

import java.io.File;

public class MainActivity extends BridgeActivity {
    private static final String TAG = "MainActivity";
    private static final String JS_CLIPBOARD_BRIDGE = "AndroidClipboard";
    private static final String JS_VOICE_BRIDGE = "AndroidVoice";
    /** DOM event the PWA listens for when the notification ended the call. */
    private static final String VOICE_ENDED_EVENT = "vogt:voice-service-ended";

    @Override
    public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Before anything else: if the last run crashed, VogtApplication left a
        // trace. Show it once so the operator can copy it back without adb, then
        // clear it. Deliberately ahead of the bridge check — a crash in bridge
        // setup itself must still surface.
        maybeShowCrashReport();

        final WebView webView = getBridge() != null ? getBridge().getWebView() : null;
        if (webView == null) {
            return;
        }

        // The bridges are visible to every frame and origin the WebView loads,
        // not just the PWA (#523), so each gates its sensitive methods on the
        // WebView's current top-level origin being the trusted app origin.
        final String allowedHost = getBridge() != null ? hostOf(getBridge().getServerUrl()) : null;
        webView.addJavascriptInterface(new ClipboardBridge(this, webView, allowedHost), JS_CLIPBOARD_BRIDGE);
        webView.addJavascriptInterface(new VoiceBridge(this, webView, allowedHost), JS_VOICE_BRIDGE);
        // When the notification's "End conversation" action stops the service,
        // tell the PWA so it closes the conversation on its side too.
        VoiceConversationService.setEndFromUiListener(() ->
            webView.post(() -> webView.evaluateJavascript(
                "window.dispatchEvent(new CustomEvent('" + VOICE_ENDED_EVENT + "'))", null)));
        ViewCompat.setOnApplyWindowInsetsListener(webView, (view, insets) -> {
            Insets bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() | WindowInsetsCompat.Type.displayCutout()
            );
            publishInsetCssVars(webView, bars);
            return insets;
        });
        ViewCompat.requestApplyInsets(webView);
    }

    /**
     * If {@link VogtApplication} recorded a crash on the previous run, show it in
     * a copyable dialog and delete the file (read-once, so a fixed launch does not
     * keep nagging). This is how the operator returns a native stack trace with no
     * tooling: reproduce → relaunch → Copy → paste back.
     */
    private void maybeShowCrashReport() {
        final File crashFile = new File(getFilesDir(), VogtApplication.CRASH_FILE);
        if (!crashFile.exists()) {
            return;
        }
        final String trace = VogtApplication.readCrash(crashFile);
        // Delete after reading, regardless of what we do with the contents.
        if (!crashFile.delete()) {
            Log.w(TAG, "could not delete " + VogtApplication.CRASH_FILE + " after reading");
        }
        if (trace == null || trace.trim().isEmpty()) {
            return;
        }

        String firstLine = trace.split("\n", 2)[0].trim();
        String title = firstLine.isEmpty() ? "Vogt crash report" : ("Crashed: " + firstLine);

        int pad = Math.round(16 * getResources().getDisplayMetrics().density);
        TextView body = new TextView(this);
        body.setText(trace);
        body.setTypeface(Typeface.MONOSPACE);
        body.setTextSize(11);
        body.setTextIsSelectable(true);
        body.setPadding(pad, pad, pad, pad);
        ScrollView scroll = new ScrollView(this);
        scroll.addView(body);

        new AlertDialog.Builder(this)
            .setTitle(title)
            .setView(scroll)
            .setPositiveButton("Copy", (dialog, which) -> {
                ClipboardManager clipboard =
                    (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
                if (clipboard != null) {
                    clipboard.setPrimaryClip(ClipData.newPlainText("Vogt crash", trace));
                }
            })
            .setNegativeButton("Dismiss", null)
            .show();
    }

    private void publishInsetCssVars(WebView webView, Insets bars) {
        float density = webView.getResources().getDisplayMetrics().density;
        String script = String.format(
            "(() => {" +
                "const d = document.documentElement.dataset;" +
                "d.nativeInsetTop='%d';" +
                "d.nativeInsetRight='%d';" +
                "d.nativeInsetBottom='%d';" +
                "d.nativeInsetLeft='%d';" +
                "window.dispatchEvent(new CustomEvent('mydevenv2:native-insets'));" +
            "})()",
            NativeInsets.toCssPixels(bars.top, density),
            NativeInsets.toCssPixels(bars.right, density),
            NativeInsets.toCssPixels(bars.bottom, density),
            NativeInsets.toCssPixels(bars.left, density)
        );
        webView.post(() -> webView.evaluateJavascript(script, null));
    }

    @Override
    public void onDestroy() {
        // Don't leak the Activity through the service's static listener.
        VoiceConversationService.setEndFromUiListener(null);
        super.onDestroy();
    }

    /**
     * Start/stop the voice foreground service (FR-M6). Held only while a voice
     * conversation is active in the PWA; the PWA is the authority on when that
     * is, so this bridge is a pure lever with no policy of its own.
     */
    static class VoiceBridge {
        private final Context context;
        private final WebView webView;
        private final String allowedHost;

        VoiceBridge(Context context, WebView webView, String allowedHost) {
            this.context = context.getApplicationContext();
            this.webView = webView;
            this.allowedHost = allowedHost;
        }

        @JavascriptInterface
        public void startConversation() {
            // A framed third party must not be able to spin up the mic
            // foreground service (#523).
            if (!topLevelHostTrusted(webView, allowedHost)) {
                return;
            }
            Intent intent = new Intent(context, VoiceConversationService.class)
                .setAction(VoiceConversationService.ACTION_START);
            ContextCompat.startForegroundService(context, intent);
        }

        @JavascriptInterface
        public void endConversation() {
            // The web ended it, so stop directly without signalling back.
            context.stopService(new Intent(context, VoiceConversationService.class));
        }
    }

    /** The host of a URL, or null. */
    private static String hostOf(String url) {
        if (url == null) {
            return null;
        }
        return android.net.Uri.parse(url).getHost();
    }

    /**
     * Whether the WebView's current <em>top-level</em> document is the trusted
     * app origin (#523). A {@code @JavascriptInterface} bridge added with
     * {@code addJavascriptInterface} is visible to every frame and origin the
     * WebView loads, so a bridge must refuse to act unless the top page is ours.
     *
     * <p>Residual limitation: this checks the top-level document, not the
     * calling frame, so it does not stop a trusted top-level page's own
     * embedded iframe (e.g. {@code gui_stream_url}) from calling in. Restricting
     * per calling frame means migrating the bridge to
     * {@code WebViewCompat.addWebMessageListener} with allowedOriginRules
     * (which Capacitor already builds from the server URL) or a Capacitor
     * plugin; that turns the JS contract async and is the recommended complete
     * fix. This gate is the low-risk first layer.
     *
     * <p>Runs off the WebView's private JavaBridge thread, so it reads the URL
     * via a short post to the UI thread — never a deadlock, because the UI
     * thread is not blocked waiting on the bridge.
     */
    static boolean topLevelHostTrusted(final WebView webView, final String allowedHost) {
        if (allowedHost == null || webView == null) {
            return false;
        }
        final String[] holder = new String[1];
        final java.util.concurrent.CountDownLatch latch = new java.util.concurrent.CountDownLatch(1);
        webView.post(() -> {
            holder[0] = webView.getUrl();
            latch.countDown();
        });
        try {
            if (!latch.await(1, java.util.concurrent.TimeUnit.SECONDS)) {
                return false;
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return false;
        }
        return allowedHost.equalsIgnoreCase(hostOf(holder[0]));
    }

    static class ClipboardBridge {
        private final Context context;
        private final WebView webView;
        private final String allowedHost;

        ClipboardBridge(Context context, WebView webView, String allowedHost) {
            this.context = context.getApplicationContext();
            this.webView = webView;
            this.allowedHost = allowedHost;
        }

        @JavascriptInterface
        public String readText() {
            // Refuse to hand the clipboard (plausibly holding tokens) to a
            // page that is not the trusted app origin (#523).
            if (!topLevelHostTrusted(webView, allowedHost)) {
                return "";
            }
            ClipboardManager clipboard = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null || !clipboard.hasPrimaryClip()) {
                return "";
            }
            ClipData clip = clipboard.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) {
                return "";
            }
            CharSequence text = clip.getItemAt(0).coerceToText(context);
            return text != null ? text.toString() : "";
        }

        @JavascriptInterface
        public void writeText(String value) {
            if (!topLevelHostTrusted(webView, allowedHost)) {
                return;
            }
            ClipboardManager clipboard = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null) {
                return;
            }
            String text = value != null ? value : "";
            clipboard.setPrimaryClip(ClipData.newPlainText("Vogt", text));
        }
    }
}

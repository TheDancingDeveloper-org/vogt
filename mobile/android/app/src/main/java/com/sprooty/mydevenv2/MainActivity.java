package com.sprooty.mydevenv2;

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

        webView.addJavascriptInterface(new ClipboardBridge(this), JS_CLIPBOARD_BRIDGE);
        webView.addJavascriptInterface(new VoiceBridge(this), JS_VOICE_BRIDGE);
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

        VoiceBridge(Context context) {
            this.context = context.getApplicationContext();
        }

        @JavascriptInterface
        public void startConversation() {
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

    static class ClipboardBridge {
        private final Context context;

        ClipboardBridge(Context context) {
            this.context = context.getApplicationContext();
        }

        @JavascriptInterface
        public String readText() {
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
            ClipboardManager clipboard = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null) {
                return;
            }
            String text = value != null ? value : "";
            clipboard.setPrimaryClip(ClipData.newPlainText("Vogt", text));
        }
    }
}

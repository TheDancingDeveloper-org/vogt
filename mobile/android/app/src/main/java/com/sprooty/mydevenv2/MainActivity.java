package com.sprooty.mydevenv2;

import android.os.Bundle;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.webkit.JavascriptInterface;
import android.webkit.WebView;

import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowInsetsCompat;

import com.getcapacitor.BridgeActivity;

public class MainActivity extends BridgeActivity {
    private static final String JS_CLIPBOARD_BRIDGE = "AndroidClipboard";

    @Override
    public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        final WebView webView = getBridge() != null ? getBridge().getWebView() : null;
        if (webView == null) {
            return;
        }

        webView.addJavascriptInterface(new ClipboardBridge(this), JS_CLIPBOARD_BRIDGE);
        ViewCompat.setOnApplyWindowInsetsListener(webView, (view, insets) -> {
            Insets bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() | WindowInsetsCompat.Type.displayCutout()
            );
            publishInsetCssVars(webView, bars);
            return insets;
        });
        ViewCompat.requestApplyInsets(webView);
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

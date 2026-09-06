package com.thedancingdeveloper.vogt;

import android.os.Bundle;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.graphics.Typeface;
import android.net.Uri;
import android.util.Log;
import android.webkit.WebView;
import android.widget.ScrollView;
import android.widget.TextView;

import androidx.appcompat.app.AlertDialog;
import androidx.core.content.ContextCompat;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.webkit.JavaScriptReplyProxy;
import androidx.webkit.WebMessageCompat;
import androidx.webkit.WebViewCompat;
import androidx.webkit.WebViewFeature;

import com.getcapacitor.BridgeActivity;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.util.Collections;
import java.util.Set;

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

        // A JavaScript-interface bridge is visible to every frame and origin the
        // WebView loads, so the old gate could only check the *top* document and
        // could not stop a trusted page's embedded (cross-origin) iframe — e.g.
        // gui_stream_url — from calling in (#523 residual, #624). WebMessageListener
        // binds each bridge to an explicit origin allowlist: the framework delivers
        // a message only from a frame whose origin matches, and reports whether the
        // sender is the main frame, so a same-origin subframe is refused too.
        //
        // Timing: the listener injects its JS object into every navigation *after*
        // this call, which is where the PWA's first load happens. The behavioural
        // proof (an untrusted iframe is refused; the trusted PWA still works) is the
        // emulator journey in #626; this change is unit-covered in MainActivityTest.
        final Set<String> originRules =
            allowedOriginRules(getBridge() != null ? getBridge().getServerUrl() : null);
        if (WebViewFeature.isFeatureSupported(WebViewFeature.WEB_MESSAGE_LISTENER)
                && !originRules.isEmpty()) {
            WebViewCompat.addWebMessageListener(
                webView, JS_CLIPBOARD_BRIDGE, originRules, new ClipboardBridge(this));
            WebViewCompat.addWebMessageListener(
                webView, JS_VOICE_BRIDGE, originRules, new VoiceBridge(this));
        } else {
            // No safe per-origin channel: leave the bridges unregistered rather
            // than fall back to the frame-blind interface. Clipboard and the voice
            // service degrade to unavailable, which the PWA already treats as "not
            // on this platform" (web/src/clipboard.ts, web/src/voiceService.ts).
            Log.w(TAG, "WebMessageListener unsupported or no trusted origin; "
                + "native clipboard/voice bridges disabled");
        }
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
     * The single origin the native bridges trust, as a WebMessageListener origin
     * rule ({@code scheme://host[:port]}) derived from the URL Capacitor loads.
     * Empty when the server URL is missing or unparseable, which leaves the
     * bridges unregistered — fail closed, never open (#624).
     */
    static Set<String> allowedOriginRules(String serverUrl) {
        if (serverUrl == null) {
            return Collections.emptySet();
        }
        // java.net.URI, not android.net.Uri: the origin rule is a pure string
        // derivation, and keeping it off android.jar keeps it unit-testable.
        try {
            java.net.URI uri = new java.net.URI(serverUrl);
            String scheme = uri.getScheme();
            String host = uri.getHost();
            if (scheme == null || host == null) {
                return Collections.emptySet();
            }
            String origin = scheme + "://" + host + (uri.getPort() >= 0 ? ":" + uri.getPort() : "");
            return Collections.singleton(origin);
        } catch (java.net.URISyntaxException e) {
            return Collections.emptySet();
        }
    }

    /** A bridge request: {@code {"op":..., "id":..., "value":...}}. */
    static final class Request {
        final String op;
        final String id;
        final String value;

        private Request(String op, String id, String value) {
            this.op = op;
            this.id = id;
            this.value = value;
        }

        /** Parse a message payload, or null if it is not a JSON object. */
        static Request parse(String data) {
            if (data == null) {
                return null;
            }
            try {
                JSONObject obj = new JSONObject(data);
                String op = obj.has("op") ? obj.optString("op", null) : null;
                String id = obj.has("id") ? obj.optString("id", "") : null;
                String value = obj.has("value") ? obj.optString("value", "") : null;
                return new Request(op, id, value);
            } catch (JSONException e) {
                return null;
            }
        }
    }

    // ----- Voice foreground service bridge (FR-M6, #523/#624) ----------------

    /** What a voice message asks for, once frame and payload are resolved. */
    enum VoiceAction { START, END, NONE }

    /**
     * Resolve a voice message to an action. {@code NONE} unless it comes from the
     * main frame and names a known op — origin is already gated by the listener's
     * allowlist, and the main-frame requirement denies even a same-origin iframe,
     * so no embedded frame can start or stop the mic service (#523/#624).
     */
    static VoiceAction voiceAction(String data, boolean isMainFrame) {
        if (!isMainFrame) {
            return VoiceAction.NONE;
        }
        Request req = Request.parse(data);
        if (req == null || req.op == null) {
            return VoiceAction.NONE;
        }
        switch (req.op) {
            case "start":
                return VoiceAction.START;
            case "end":
                return VoiceAction.END;
            default:
                return VoiceAction.NONE;
        }
    }

    /**
     * Start/stop the voice foreground service (FR-M6). Held only while a voice
     * conversation is active in the PWA; the PWA is the authority on when that is,
     * so this bridge is a pure lever with no policy of its own. Bound to the
     * trusted origin and the main frame by {@link #voiceAction}.
     */
    static final class VoiceBridge implements WebViewCompat.WebMessageListener {
        private final Context context;

        VoiceBridge(Context context) {
            this.context = context.getApplicationContext();
        }

        @Override
        public void onPostMessage(WebView view, WebMessageCompat message, Uri sourceOrigin,
                boolean isMainFrame, JavaScriptReplyProxy replyProxy) {
            switch (voiceAction(message.getData(), isMainFrame)) {
                case START:
                    Intent start = new Intent(context, VoiceConversationService.class)
                        .setAction(VoiceConversationService.ACTION_START);
                    ContextCompat.startForegroundService(context, start);
                    break;
                case END:
                    // The web ended it, so stop directly without signalling back.
                    context.stopService(new Intent(context, VoiceConversationService.class));
                    break;
                default:
                    break;
            }
        }
    }

    // ----- Clipboard bridge (#523/#624) --------------------------------------

    /** The privileged clipboard operations, isolated so the frame gate is testable. */
    interface Clipboard {
        String read();

        void write(String value);
    }

    /**
     * Handle a clipboard message. Returns the JSON reply for a {@code read}, or
     * null. Refuses everything but the trusted main frame: the listener's origin
     * allowlist already blocks foreign origins, and the main-frame requirement
     * additionally blocks a same-origin embedded iframe, so no subframe can read
     * or write the clipboard (#523/#624).
     */
    static String handleClipboard(String data, boolean isMainFrame, Clipboard clipboard) {
        if (!isMainFrame) {
            return null;
        }
        Request req = Request.parse(data);
        if (req == null || req.op == null) {
            return null;
        }
        switch (req.op) {
            case "read":
                return readReply(req.id, clipboard.read());
            case "write":
                if (req.value != null) {
                    clipboard.write(req.value);
                }
                return null;
            default:
                return null;
        }
    }

    /** The reply to a clipboard {@code read}: {@code {"id":..., "text":...}}. */
    static String readReply(String id, String text) {
        try {
            JSONObject obj = new JSONObject();
            obj.put("id", id == null ? JSONObject.NULL : id);
            obj.put("text", text == null ? "" : text);
            return obj.toString();
        } catch (JSONException e) {
            return null;
        }
    }

    /** The real Android clipboard, backing {@link Clipboard} at runtime. */
    static Clipboard androidClipboard(final Context appContext) {
        return new Clipboard() {
            @Override
            public String read() {
                ClipboardManager clipboard =
                    (ClipboardManager) appContext.getSystemService(Context.CLIPBOARD_SERVICE);
                if (clipboard == null || !clipboard.hasPrimaryClip()) {
                    return "";
                }
                ClipData clip = clipboard.getPrimaryClip();
                if (clip == null || clip.getItemCount() == 0) {
                    return "";
                }
                CharSequence text = clip.getItemAt(0).coerceToText(appContext);
                return text != null ? text.toString() : "";
            }

            @Override
            public void write(String value) {
                ClipboardManager clipboard =
                    (ClipboardManager) appContext.getSystemService(Context.CLIPBOARD_SERVICE);
                if (clipboard == null) {
                    return;
                }
                clipboard.setPrimaryClip(ClipData.newPlainText("Vogt", value != null ? value : ""));
            }
        };
    }

    /**
     * The clipboard the PWA reads and writes (plausibly holding tokens), bound to
     * the trusted origin and the main frame by {@link #handleClipboard}.
     */
    static final class ClipboardBridge implements WebViewCompat.WebMessageListener {
        private final Clipboard clipboard;

        ClipboardBridge(Context context) {
            this.clipboard = androidClipboard(context.getApplicationContext());
        }

        @Override
        public void onPostMessage(WebView view, WebMessageCompat message, Uri sourceOrigin,
                boolean isMainFrame, JavaScriptReplyProxy replyProxy) {
            String reply = handleClipboard(message.getData(), isMainFrame, clipboard);
            if (reply != null) {
                replyProxy.postMessage(reply);
            }
        }
    }
}

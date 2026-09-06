package com.thedancingdeveloper.vogt;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.json.JSONException;
import org.json.JSONObject;
import org.junit.Test;

import java.util.ArrayList;
import java.util.List;

/**
 * The frame/origin enforcement behind the native clipboard and voice bridges
 * (#624). These cover the pure decision layer: the origin allowlist the
 * WebMessageListener is bound to, and the rule that only the trusted main frame
 * may act. The behavioural proof — a real untrusted iframe is refused in a live
 * WebView while the trusted PWA still works — is the emulator journey in #626.
 */
public class MainActivityTest {

    /** A {@link MainActivity.Clipboard} that records what the bridge asked of it. */
    private static final class FakeClipboard implements MainActivity.Clipboard {
        String stored = "secret-token";
        int reads = 0;
        final List<String> writes = new ArrayList<>();

        @Override
        public String read() {
            reads++;
            return stored;
        }

        @Override
        public void write(String value) {
            writes.add(value);
        }
    }

    // ----- allowedOriginRules -------------------------------------------------

    @Test
    public void origin_rule_keeps_scheme_host_and_port() {
        assertEquals(
            java.util.Collections.singleton("http://127.0.0.1:8910"),
            MainActivity.allowedOriginRules("http://127.0.0.1:8910"));
    }

    @Test
    public void origin_rule_omits_a_default_port() {
        assertEquals(
            java.util.Collections.singleton("https://vogt.example.com"),
            MainActivity.allowedOriginRules("https://vogt.example.com/app"));
    }

    @Test
    public void origin_rule_is_empty_for_a_missing_or_unparseable_url() {
        // Fail closed: no rule means the bridge is never registered.
        assertTrue(MainActivity.allowedOriginRules(null).isEmpty());
        assertTrue(MainActivity.allowedOriginRules("not a url").isEmpty());
        assertTrue(MainActivity.allowedOriginRules("mailto:x@y.z").isEmpty());
    }

    // ----- voiceAction: an untrusted iframe cannot start/stop the mic ---------

    @Test
    public void voice_from_the_main_frame_starts_and_ends() {
        assertEquals(MainActivity.VoiceAction.START,
            MainActivity.voiceAction("{\"op\":\"start\"}", true));
        assertEquals(MainActivity.VoiceAction.END,
            MainActivity.voiceAction("{\"op\":\"end\"}", true));
    }

    @Test
    public void voice_from_a_subframe_is_refused() {
        // The core #624 guarantee for voice: a framed page cannot spin up the
        // foreground mic service even if it reaches the bridge object.
        assertEquals(MainActivity.VoiceAction.NONE,
            MainActivity.voiceAction("{\"op\":\"start\"}", false));
        assertEquals(MainActivity.VoiceAction.NONE,
            MainActivity.voiceAction("{\"op\":\"end\"}", false));
    }

    @Test
    public void voice_ignores_unknown_or_malformed_messages() {
        assertEquals(MainActivity.VoiceAction.NONE,
            MainActivity.voiceAction("{\"op\":\"nonsense\"}", true));
        assertEquals(MainActivity.VoiceAction.NONE,
            MainActivity.voiceAction("not json", true));
        assertEquals(MainActivity.VoiceAction.NONE,
            MainActivity.voiceAction("{}", true));
    }

    // ----- handleClipboard: an untrusted iframe cannot read/write -------------

    @Test
    public void clipboard_read_from_the_main_frame_returns_the_text() throws JSONException {
        FakeClipboard clip = new FakeClipboard();
        String reply = MainActivity.handleClipboard("{\"op\":\"read\",\"id\":\"7\"}", true, clip);
        assertEquals(1, clip.reads);
        JSONObject obj = new JSONObject(reply);
        assertEquals("7", obj.getString("id"));
        assertEquals("secret-token", obj.getString("text"));
    }

    @Test
    public void clipboard_read_from_a_subframe_is_refused_and_never_touches_the_clipboard() {
        FakeClipboard clip = new FakeClipboard();
        String reply = MainActivity.handleClipboard("{\"op\":\"read\",\"id\":\"7\"}", false, clip);
        // No reply, and — the point of #624 — the clipboard is never even read.
        assertNull(reply);
        assertEquals(0, clip.reads);
    }

    @Test
    public void clipboard_write_from_the_main_frame_stores_the_value() {
        FakeClipboard clip = new FakeClipboard();
        String reply = MainActivity.handleClipboard(
            "{\"op\":\"write\",\"value\":\"hello\"}", true, clip);
        assertNull(reply);
        assertEquals(1, clip.writes.size());
        assertEquals("hello", clip.writes.get(0));
    }

    @Test
    public void clipboard_write_from_a_subframe_is_refused() {
        FakeClipboard clip = new FakeClipboard();
        MainActivity.handleClipboard("{\"op\":\"write\",\"value\":\"hello\"}", false, clip);
        assertTrue(clip.writes.isEmpty());
    }

    @Test
    public void clipboard_ignores_unknown_ops_and_malformed_messages() {
        FakeClipboard clip = new FakeClipboard();
        assertNull(MainActivity.handleClipboard("{\"op\":\"wipe\"}", true, clip));
        assertNull(MainActivity.handleClipboard("not json", true, clip));
        assertEquals(0, clip.reads);
        assertTrue(clip.writes.isEmpty());
    }

    @Test
    public void read_reply_carries_id_and_text() throws JSONException {
        JSONObject obj = new JSONObject(MainActivity.readReply("3", "value"));
        assertEquals("3", obj.getString("id"));
        assertEquals("value", obj.getString("text"));
        // A read with no id still replies with an explicit null id.
        JSONObject noId = new JSONObject(MainActivity.readReply(null, ""));
        assertTrue(noId.isNull("id"));
        assertEquals("", noId.getString("text"));
    }

    @Test
    public void a_subframe_gate_is_independent_of_the_op() {
        // Belt and braces: every sensitive entry point honours isMainFrame.
        FakeClipboard clip = new FakeClipboard();
        assertNull(MainActivity.handleClipboard("{\"op\":\"read\",\"id\":\"1\"}", false, clip));
        assertEquals(MainActivity.VoiceAction.NONE,
            MainActivity.voiceAction("{\"op\":\"start\"}", false));
        assertFalse(clip.reads > 0);
    }
}

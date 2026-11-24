package com.thedancingdeveloper.vogt;

import static org.junit.Assert.*;
import org.junit.Test;

public class ServerAddressTest {
    @Test public void preservesTheChosenOrigin() {
        assertEquals("https://server.example", ServerAddress.normalize(" HTTPS://Server.Example/ "));
        assertEquals("http://127.0.0.1:8910", ServerAddress.normalize("http://127.0.0.1:8910"));
        assertEquals("http://[::1]:8910", ServerAddress.normalize("http://[::1]:8910/"));
    }
    @Test public void rejectsValuesThatCouldLoadADifferentResource() {
        for (String raw : new String[] {"", "example.com", "file:///etc/passwd", "javascript:alert(1)",
                "https://user:secret@example.com", "https://example.com/path", "https://example.com?q=x",
                "https://example.com#x", "https://example.com:99999", "https://example.com:0"}) {
            assertNull(raw, ServerAddress.normalize(raw));
        }
    }
}

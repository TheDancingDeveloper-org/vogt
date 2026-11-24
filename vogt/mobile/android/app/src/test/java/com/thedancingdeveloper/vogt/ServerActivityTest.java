package com.thedancingdeveloper.vogt;

import static org.junit.Assert.*;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import org.junit.Test;

/** WI-924: reopening the app keeps the signed-in session instead of asking again. */
public class ServerActivityTest {
    @Test public void aFreshLaunchWithASavedServerGoesStraightToIt() {
        assertTrue(ServerActivity.shouldForward("https://vogt.example", false));
    }

    @Test public void noSavedServerOrARestoredTaskAsksOrStays() {
        assertFalse(ServerActivity.shouldForward("", false));
        assertFalse(ServerActivity.shouldForward(null, false));
        assertFalse(ServerActivity.shouldForward("not a url", false));
        assertFalse("a restored task already has MainActivity above it",
            ServerActivity.shouldForward("https://vogt.example", true));
    }

    /**
     * The launcher activity must not be singleTask: a singleTask root
     * clears the activities above it on every launcher intent, which
     * destroyed the signed-in WebView each time the app was reopened.
     */
    @Test public void theLauncherActivityDoesNotClearTheTaskOnReopen() throws Exception {
        Path manifest = Paths.get("src", "main", "AndroidManifest.xml");
        String xml = new String(Files.readAllBytes(manifest), StandardCharsets.UTF_8)
            .replaceAll("(?s)<!--.*?-->", "");
        int start = xml.indexOf("android:name=\".ServerActivity\"");
        assertTrue("ServerActivity is declared", start >= 0);
        int open = xml.lastIndexOf("<activity", start);
        int close = xml.indexOf(">", start);
        String declaration = xml.substring(open, close);
        assertFalse(declaration, declaration.contains("singleTask"));
        assertFalse(declaration, declaration.contains("singleInstance"));
        assertFalse(declaration, declaration.contains("clearTaskOnLaunch"));
    }
}

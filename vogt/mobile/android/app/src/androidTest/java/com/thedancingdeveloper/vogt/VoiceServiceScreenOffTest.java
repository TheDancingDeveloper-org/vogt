package com.thedancingdeveloper.vogt;

import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.POST_NOTIFICATIONS;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.RECORD_AUDIO;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.action;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.grant;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.notificationPosted;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.stopServiceAndWait;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.waitUntil;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import android.os.SystemClock;

import androidx.lifecycle.Lifecycle;
import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Device coverage for screen-off survival: a started (not bound) foreground
 * service is independent of the Activity's visible lifecycle, so backgrounding
 * the UI — the deterministic proxy for the screen turning off — must not tear
 * the held service down. That is what keeps the assistant socket and FCM
 * delivery alive while the screen is off.
 *
 * <p>Scope boundary. This proves the service outlives the foreground Activity
 * and survives a background/foreground round-trip. It does NOT assert the
 * genuine screen-off, Doze-window, 30-minute battery + socket survival, or a
 * live FCM message landing while the device sleeps — those need a real device
 * and stay the manual/operator tier recorded in {@code android-instrumentation.yml},
 * the service class doc, and the issue. The service logs conversation start/end
 * to logcat precisely so that device measurement can bound the survival window.
 *
 * <p>Runs on an emulator/device via connectedAndroidTest.
 */
@RunWith(AndroidJUnit4.class)
public class VoiceServiceScreenOffTest {

    @After
    public void tearDown() {
        stopServiceAndWait();
    }

    @Test
    public void held_service_survives_the_activity_being_backgrounded() {
        grant(POST_NOTIFICATIONS);
        grant(RECORD_AUDIO);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            assertTrue("the held service posts its foreground notification",
                waitUntil(VoiceServiceTestSupport::notificationPosted, 8000));

            // Background the UI: the closest deterministic stand-in for the
            // screen turning off. A started foreground service is not bound to
            // the Activity, so it must remain up.
            scenario.moveToState(Lifecycle.State.CREATED);
            // Give any teardown that a bug would trigger time to happen, then
            // assert the service is still held.
            SystemClock.sleep(2000);
            assertTrue("the foreground service outlives the backgrounded Activity",
                notificationPosted());

            // Foreground again: the service is still the same held instance and
            // the Activity reconnects without being finished.
            scenario.moveToState(Lifecycle.State.RESUMED);
            assertTrue("the service is still held after returning to the foreground",
                notificationPosted());
            scenario.onActivity(activity ->
                assertFalse("MainActivity reconnects after the screen-off proxy",
                    activity.isFinishing()));
        }
    }
}

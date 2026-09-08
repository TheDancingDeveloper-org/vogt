package com.thedancingdeveloper.vogt;

import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.POST_NOTIFICATIONS;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.action;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.activeServiceNotifications;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.grant;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.notificationPosted;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.stopServiceAndWait;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.waitUntil;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Device coverage for process-reclaim recovery: after the held service is torn
 * down (an OS low-memory reclaim, or the user swiping the app away), a fresh
 * conversation re-registers the service cleanly and does not leave a stale one
 * behind.
 *
 * <p>Design note — why a re-issued START stands in for a real kill. The service
 * is {@code START_NOT_STICKY} deliberately: Android must NOT silently resurrect
 * a voice service the PWA no longer wants. Recovery is therefore driven by the
 * PWA re-issuing {@code ACTION_START} through MainActivity's bridge when the
 * conversation resumes — exactly the path this test drives. A genuine
 * low-memory process death (and its Doze/battery timing) is not deterministically
 * reproducible from instrumentation and stays on the manual/emulator tier
 * recorded in {@code android-instrumentation.yml} and the issue; here we prove
 * the native half — clean re-registration with no duplicate or orphaned
 * notification — which is what "reconnects and does not leave a stale service"
 * reduces to on this side.
 *
 * <p>Runs on an emulator/device via connectedAndroidTest.
 */
@RunWith(AndroidJUnit4.class)
public class VoiceServiceProcessReclaimTest {

    @After
    public void tearDown() {
        stopServiceAndWait();
    }

    @Test
    public void service_re_registers_after_a_simulated_reclaim() {
        grant(POST_NOTIFICATIONS);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            // A conversation is active: the held service posts its notification.
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            assertTrue("the service posts its foreground notification",
                waitUntil(VoiceServiceTestSupport::notificationPosted, 8000));

            // Simulate the reclaim: the service is torn down (onDestroy runs,
            // the wake lock releases, the notification clears) as it would be
            // when the OS reclaims the process or the task is swiped away.
            stopServiceAndWait();
            assertFalse("the reclaimed service leaves no notification behind",
                notificationPosted());

            // Recovery: the PWA resumes the conversation and re-drives the
            // bridge. The service must come back cleanly.
            scenario.moveToState(androidx.lifecycle.Lifecycle.State.RESUMED);
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            assertTrue("the service re-registers after the reclaim",
                waitUntil(VoiceServiceTestSupport::notificationPosted, 8000));

            // No stale service: exactly one held notification, not a duplicate
            // orphaned by the first instance.
            assertEquals("recovery leaves exactly one held service, no stale duplicate",
                1, activeServiceNotifications());
            scenario.onActivity(activity ->
                assertFalse("the app is alive after reclaim recovery", activity.isFinishing()));
        }
    }

    @Test
    public void stopping_leaves_no_stale_service() {
        grant(POST_NOTIFICATIONS);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            assertTrue(waitUntil(VoiceServiceTestSupport::notificationPosted, 8000));

            // Ending the conversation must fully retire the service — the
            // reclaim-recovery guarantee is only meaningful if a normal stop
            // never orphans one.
            stopServiceAndWait();
            assertEquals("a stopped conversation leaves no held notification",
                0, activeServiceNotifications());
        }
    }
}

package com.thedancingdeveloper.vogt;

import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.POST_NOTIFICATIONS;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.RECORD_AUDIO;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.action;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.grant;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.revoke;
import static com.thedancingdeveloper.vogt.VoiceServiceTestSupport.stopServiceAndWait;
import static org.junit.Assert.assertFalse;

import android.os.SystemClock;

import androidx.lifecycle.Lifecycle;
import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Device coverage for permission denial degrading gracefully rather than
 * crashing. The acceptance criterion is "microphone/notification permission
 * denial degrades without a crash".
 *
 * <p>The microphone-only denial case (a mic-typed foreground service started
 * without RECORD_AUDIO must not take the app down) is covered by
 * {@link VoiceServiceLifecycleTest#starting_without_record_audio_degrades_without_crashing()}.
 * This class adds the notification (FCM/POST_NOTIFICATIONS) denial and the
 * combined both-denied case, which the mic test does not exercise.
 *
 * <p>POST_NOTIFICATIONS is the same runtime grant FCM alerts require (see the
 * manifest comment), so denying it here also stands in for a user who refuses
 * the FCM notification prompt: the persistent service notification is suppressed
 * by the OS, but the foreground service — and therefore the app — must survive.
 *
 * <p>Runs on an emulator/device via connectedAndroidTest. Requires API 33+ for
 * POST_NOTIFICATIONS to be a runtime permission; on older levels the grant/revoke
 * is a no-op and the service simply starts, which the assertions still allow.
 */
@RunWith(AndroidJUnit4.class)
public class VoiceServicePermissionDenialTest {

    @After
    public void tearDown() {
        // Restore the permissions this test revoked so it does not leak state
        // into sibling tests, then clear the service.
        grant(POST_NOTIFICATIONS);
        grant(RECORD_AUDIO);
        stopServiceAndWait();
    }

    @Test
    public void starting_without_post_notifications_degrades_without_crashing() {
        // The user refused the notification prompt (the same grant FCM needs).
        revoke(POST_NOTIFICATIONS);
        grant(RECORD_AUDIO);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            // The persistent notification is suppressed by the OS with the
            // permission denied, so there is nothing to assert posted — the
            // point is that the process stays alive rather than crashing on the
            // missing grant.
            SystemClock.sleep(1500);
            scenario.moveToState(Lifecycle.State.RESUMED);
            scenario.onActivity(activity ->
                assertFalse("the app survives a notification-denied service start",
                    activity.isFinishing()));
        }
    }

    @Test
    public void starting_with_mic_and_notifications_denied_degrades_without_crashing() {
        // Both runtime grants refused: the foreground service falls back to
        // DATA_SYNC only, its notification is suppressed, and the app must still
        // stand rather than take a SecurityException or a missing-permission
        // crash to the ground.
        revoke(POST_NOTIFICATIONS);
        revoke(RECORD_AUDIO);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            SystemClock.sleep(1500);
            scenario.moveToState(Lifecycle.State.RESUMED);
            scenario.onActivity(activity ->
                assertFalse("the app survives a fully permission-denied service start",
                    activity.isFinishing()));
        }
    }
}

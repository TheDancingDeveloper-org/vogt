package com.thedancingdeveloper.vogt;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import android.app.NotificationManager;
import android.content.Context;
import android.content.Intent;
import android.os.SystemClock;
import android.service.notification.StatusBarNotification;

import androidx.lifecycle.Lifecycle;
import androidx.test.core.app.ActivityScenario;
import androidx.test.core.app.ApplicationProvider;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * Device coverage for the voice foreground-service lifecycle and the
 * notification "End" action (#626). Runs on an emulator/device via
 * connectedAndroidTest — the deterministic tier of #626; long-form screen-off
 * battery/socket survival and Play pre-launch stay a manual operator task (see
 * the service class doc and the issue).
 *
 * The service is started from a resumed MainActivity, which is how the PWA
 * drives it and what Android 14 (API 34) requires for a background-start-safe
 * foreground service.
 */
@RunWith(AndroidJUnit4.class)
public class VoiceServiceLifecycleTest {

    private static final int NOTIFICATION_ID = 4711;
    private static final String POST_NOTIFICATIONS = "android.permission.POST_NOTIFICATIONS";
    private static final String RECORD_AUDIO = "android.permission.RECORD_AUDIO";

    /** A pollable condition — avoids java.util.function (API 24) under minSdk 23. */
    private interface Condition {
        boolean met();
    }

    private Context context() {
        return ApplicationProvider.getApplicationContext();
    }

    private NotificationManager notifications() {
        return context().getSystemService(NotificationManager.class);
    }

    private void grant(String permission) {
        InstrumentationRegistry.getInstrumentation().getUiAutomation()
            .grantRuntimePermission(context().getPackageName(), permission);
    }

    private void revoke(String permission) {
        InstrumentationRegistry.getInstrumentation().getUiAutomation()
            .revokeRuntimePermission(context().getPackageName(), permission);
    }

    private boolean notificationPosted() {
        for (StatusBarNotification sbn : notifications().getActiveNotifications()) {
            if (sbn.getId() == NOTIFICATION_ID) {
                return true;
            }
        }
        return false;
    }

    /** Poll a condition up to {@code timeoutMs}; the service posts/clears async. */
    private boolean waitUntil(Condition condition, long timeoutMs) {
        long deadline = SystemClock.uptimeMillis() + timeoutMs;
        while (SystemClock.uptimeMillis() < deadline) {
            if (condition.met()) {
                return true;
            }
            SystemClock.sleep(100);
        }
        return condition.met();
    }

    private Intent action(String action) {
        return new Intent(context(), VoiceConversationService.class).setAction(action);
    }

    @After
    public void tearDown() {
        VoiceConversationService.setEndFromUiListener(null);
        context().stopService(new Intent(context(), VoiceConversationService.class));
        waitUntil(() -> !notificationPosted(), 3000);
    }

    @Test
    public void start_posts_the_foreground_notification_and_stop_removes_it() {
        grant(POST_NOTIFICATIONS);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            assertTrue("the held service posts its foreground notification",
                waitUntil(this::notificationPosted, 8000));

            context().startService(action(VoiceConversationService.ACTION_STOP_FROM_UI));
            assertTrue("stopping the service clears the notification",
                waitUntil(() -> !notificationPosted(), 8000));
        }
    }

    @Test
    public void notification_end_action_signals_the_ui_and_stops() throws InterruptedException {
        grant(POST_NOTIFICATIONS);
        CountDownLatch endedFromUi = new CountDownLatch(1);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            assertTrue(waitUntil(this::notificationPosted, 8000));

            // Install the observer AFTER launch: MainActivity.onCreate registers
            // its own end-from-UI listener (the DOM-event dispatch), so setting
            // it earlier would be overwritten.
            VoiceConversationService.setEndFromUiListener(endedFromUi::countDown);
            // The notification's "End conversation" action fires exactly this.
            context().startService(action(VoiceConversationService.ACTION_STOP_FROM_UI));

            assertTrue("End-from-notification signals the PWA",
                endedFromUi.await(8, TimeUnit.SECONDS));
            assertTrue("and the service stops",
                waitUntil(() -> !notificationPosted(), 8000));
        }
    }

    @Test
    public void starting_without_record_audio_degrades_without_crashing() {
        grant(POST_NOTIFICATIONS);
        revoke(RECORD_AUDIO);
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            // Before #523's fix, a microphone-typed FGS started without
            // RECORD_AUDIO threw SecurityException and took the app down. It must
            // now degrade (DATA_SYNC only, or stopSelf) with the app still alive.
            scenario.onActivity(activity ->
                activity.startForegroundService(action(VoiceConversationService.ACTION_START)));
            SystemClock.sleep(1500);
            scenario.moveToState(Lifecycle.State.RESUMED);
            scenario.onActivity(activity ->
                assertFalse("the app survives a mic-less service start", activity.isFinishing()));
        }
    }

    @Test
    public void background_then_foreground_keeps_the_activity_alive() {
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.moveToState(Lifecycle.State.CREATED); // backgrounded
            scenario.moveToState(Lifecycle.State.RESUMED); // foregrounded again
            scenario.onActivity(activity ->
                assertFalse("MainActivity reconnects after background/foreground",
                    activity.isFinishing()));
        }
    }
}

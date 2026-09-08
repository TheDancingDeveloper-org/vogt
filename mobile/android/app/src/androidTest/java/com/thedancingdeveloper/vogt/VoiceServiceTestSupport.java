package com.thedancingdeveloper.vogt;

import android.app.NotificationManager;
import android.content.Context;
import android.content.Intent;
import android.os.SystemClock;
import android.service.notification.StatusBarNotification;

import androidx.test.core.app.ApplicationProvider;
import androidx.test.platform.app.InstrumentationRegistry;

/**
 * Shared instrumentation helpers for the voice foreground-service device tests.
 *
 * <p>{@link VoiceServiceLifecycleTest} carries its own inlined copies of these
 * (it landed first, self-contained); the later tiers — permission denial,
 * process reclaim, screen-off survival — share this so the polling and
 * permission plumbing is written once. Everything here is deliberately free of
 * java.util.function (API 24) so it stays valid under the app's minSdk 23.
 */
final class VoiceServiceTestSupport {

    /** Must match {@code VoiceConversationService.NOTIFICATION_ID}. */
    static final int NOTIFICATION_ID = 4711;
    static final String POST_NOTIFICATIONS = "android.permission.POST_NOTIFICATIONS";
    static final String RECORD_AUDIO = "android.permission.RECORD_AUDIO";

    /** A pollable condition — avoids java.util.function (API 24) under minSdk 23. */
    interface Condition {
        boolean met();
    }

    private VoiceServiceTestSupport() {
    }

    static Context context() {
        return ApplicationProvider.getApplicationContext();
    }

    static NotificationManager notifications() {
        return context().getSystemService(NotificationManager.class);
    }

    static void grant(String permission) {
        InstrumentationRegistry.getInstrumentation().getUiAutomation()
            .grantRuntimePermission(context().getPackageName(), permission);
    }

    static void revoke(String permission) {
        InstrumentationRegistry.getInstrumentation().getUiAutomation()
            .revokeRuntimePermission(context().getPackageName(), permission);
    }

    /** How many active notifications carry the service's foreground id. */
    static int activeServiceNotifications() {
        int count = 0;
        for (StatusBarNotification sbn : notifications().getActiveNotifications()) {
            if (sbn.getId() == NOTIFICATION_ID) {
                count++;
            }
        }
        return count;
    }

    static boolean notificationPosted() {
        return activeServiceNotifications() > 0;
    }

    /** Poll a condition up to {@code timeoutMs}; the service posts/clears async. */
    static boolean waitUntil(Condition condition, long timeoutMs) {
        long deadline = SystemClock.uptimeMillis() + timeoutMs;
        while (SystemClock.uptimeMillis() < deadline) {
            if (condition.met()) {
                return true;
            }
            SystemClock.sleep(100);
        }
        return condition.met();
    }

    /** An intent naming the voice service with the given action. */
    static Intent action(String action) {
        return new Intent(context(), VoiceConversationService.class).setAction(action);
    }

    /** Stop the service and wait for its notification to clear. */
    static void stopServiceAndWait() {
        context().stopService(new Intent(context(), VoiceConversationService.class));
        waitUntil(new Condition() {
            @Override
            public boolean met() {
                return !notificationPosted();
            }
        }, 3000);
    }
}

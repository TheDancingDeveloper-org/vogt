package com.sprooty.mydevenv2;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.os.SystemClock;
import android.util.Log;

import androidx.core.app.NotificationCompat;
import androidx.core.content.ContextCompat;

/**
 * A foreground service held ONLY for the duration of an explicitly active voice
 * conversation (FR-M6). It exists to keep the process alive across screen-off —
 * so the assistant WebSocket, audio capture and TTS playback survive — and NOT
 * to listen: there is no microphone opened here, no wake word, no polling. The
 * PWA opens the mic through the WebView only while the push-to-talk button is
 * held; this service merely stops Android from freezing the process around it.
 *
 * <p>Lifecycle:
 * <ul>
 *   <li>{@code startConversation()} on the JS bridge starts it and it calls
 *       {@link #startForeground} with a persistent notification naming the app
 *       ("Vogt") and an "End conversation" action.</li>
 *   <li>{@code endConversation()} on the JS bridge (the conversation ended in
 *       the PWA) stops it silently — the web already knows.</li>
 *   <li>The notification's "End conversation" action routes back through
 *       {@link #ACTION_STOP_FROM_UI}, which signals the web to end the
 *       conversation before the service stops, so the two halves stay in
 *       agreement whichever side ended it.</li>
 * </ul>
 */
public class VoiceConversationService extends Service {
    private static final String TAG = "VoiceConversation";
    private static final String CHANNEL_ID = "vogt-voice-conversation";
    private static final int NOTIFICATION_ID = 4711;

    /** Start (or refresh) the held foreground service. */
    static final String ACTION_START = "com.sprooty.mydevenv2.voice.START";
    /** Stop requested from the notification's "End conversation" action. */
    static final String ACTION_STOP_FROM_UI = "com.sprooty.mydevenv2.voice.STOP_FROM_UI";

    /**
     * How the service tells the web that the conversation was ended from the
     * notification. MainActivity sets this on create and clears it on destroy;
     * the callback dispatches a DOM event the PWA listens for. A static
     * reference is deliberate over a broadcast: no exported receiver to reason
     * about across API levels, and the only listener is the single Activity.
     */
    interface EndFromUiListener {
        void onEndedFromUi();
    }

    private static volatile EndFromUiListener endFromUiListener;

    static void setEndFromUiListener(EndFromUiListener listener) {
        endFromUiListener = listener;
    }

    private PowerManager.WakeLock wakeLock;
    private long startedAtElapsed;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        final String action = intent != null ? intent.getAction() : null;
        if (ACTION_STOP_FROM_UI.equals(action)) {
            // The user pressed "End conversation" in the notification. Tell the
            // web so the PWA closes the conversation too, then stop.
            EndFromUiListener listener = endFromUiListener;
            if (listener != null) {
                listener.onEndedFromUi();
            }
            stopSelf();
            return START_NOT_STICKY;
        }

        startInForeground();
        return START_NOT_STICKY;
    }

    private void startInForeground() {
        createChannel();
        startedAtElapsed = SystemClock.elapsedRealtime();
        // Measurement hook (FR-M6): the device tester reads conversation
        // start/end out of logcat to bound the 30-minute battery + socket
        // survival check. The number itself is a device task, not this code's.
        Log.i(TAG, "voice conversation started (elapsedRealtime=" + startedAtElapsed + "ms)");

        Notification notification = buildNotification();
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                // This service does NOT itself open the mic (see the class doc):
                // the WebView does, while foregrounded. So request DATA_SYNC to
                // keep the process alive, and add the MICROPHONE type ONLY when
                // RECORD_AUDIO is actually granted — Android 14+ throws
                // SecurityException if a microphone-typed foreground service is
                // started without it, which previously crashed the whole app the
                // moment spoken replies were toggled on before any mic grant.
                int type = ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC;
                if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
                        == PackageManager.PERMISSION_GRANTED) {
                    type |= ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE;
                }
                startForeground(NOTIFICATION_ID, notification, type);
            } else {
                startForeground(NOTIFICATION_ID, notification);
            }
        } catch (Exception e) {
            // A foreground service that cannot start (a missing permission, a
            // background-start restriction) must never take the app down with
            // it: the conversation still works while the app is foregrounded, it
            // just will not survive screen-off (FR-M6, degraded).
            Log.w(TAG, "voice foreground service could not start; continuing without it", e);
            stopSelf();
            return;
        }
        acquireWakeLock();
    }

    private Notification buildNotification() {
        // Tapping the notification returns to the conversation.
        Intent openIntent = new Intent(this, MainActivity.class)
            .setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_NEW_TASK);
        PendingIntent openPending = PendingIntent.getActivity(
            this, 0, openIntent, pendingIntentFlags());

        // The "End conversation" action comes back to this service.
        Intent stopIntent = new Intent(this, VoiceConversationService.class)
            .setAction(ACTION_STOP_FROM_UI);
        PendingIntent stopPending = PendingIntent.getService(
            this, 1, stopIntent, pendingIntentFlags());

        return new NotificationCompat.Builder(this, CHANNEL_ID)
            // Names the app, per FR-M6's "persistent notification names the app".
            .setContentTitle(getString(R.string.app_name))
            .setContentText("Voice conversation active")
            .setSmallIcon(R.mipmap.ic_launcher)
            .setOngoing(true)
            .setContentIntent(openPending)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .addAction(0, "End conversation", stopPending)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .build();
    }

    private static int pendingIntentFlags() {
        int flags = PendingIntent.FLAG_UPDATE_CURRENT;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            flags |= PendingIntent.FLAG_IMMUTABLE;
        }
        return flags;
    }

    private void createChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationManager manager = (NotificationManager)
            getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager == null || manager.getNotificationChannel(CHANNEL_ID) != null) {
            return;
        }
        // Low importance: the ongoing notification is a status, not an alert —
        // it should not buzz. It is separate from the "vogt-alerts" channel the
        // push plugin uses for FCM interruptions.
        NotificationChannel channel = new NotificationChannel(
            CHANNEL_ID, "Voice conversation", NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Shown while a Vogt voice conversation is active");
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);
    }

    private void acquireWakeLock() {
        if (wakeLock != null) {
            return;
        }
        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        if (power == null) {
            return;
        }
        // A partial wake lock keeps the CPU (and therefore the assistant socket)
        // alive across screen-off for the conversation's duration only; it is
        // released in onDestroy. Battery cost of holding it is exactly what the
        // POC measurement in VOICE_POC §6 is there to read.
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "vogt:voice-conversation");
        wakeLock.setReferenceCounted(false);
        wakeLock.acquire();
    }

    private void releaseWakeLock() {
        if (wakeLock != null && wakeLock.isHeld()) {
            wakeLock.release();
        }
        wakeLock = null;
    }

    @Override
    public void onDestroy() {
        releaseWakeLock();
        if (startedAtElapsed > 0) {
            long durationMs = SystemClock.elapsedRealtime() - startedAtElapsed;
            Log.i(TAG, "voice conversation ended (held " + durationMs + "ms)");
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        // A started, not bound, service.
        return null;
    }
}

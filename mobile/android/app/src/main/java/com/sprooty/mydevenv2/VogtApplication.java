package com.sprooty.mydevenv2;

import android.app.Application;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.os.Build;
import android.util.Log;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;

/**
 * Application subclass whose only job is a last-resort crash recorder.
 *
 * <p>The assistant "unmute" path has crashed the app from native code the PWA's
 * JavaScript {@code try/catch} cannot see — a foreground-service
 * {@code SecurityException}, a WebView render-process kill, or an exception on a
 * background thread. With no adb from the operator, those traces were lost. This
 * installs a process-wide {@link Thread.UncaughtExceptionHandler} that writes the
 * stack trace — tagged with the app version so a stale reinstall is obvious on
 * sight — to {@code filesDir/last-crash.txt}, which {@link MainActivity} shows and
 * clears on the next launch. It then delegates to whatever handler was installed
 * before it (Android's, which terminates the process as usual), so nothing about
 * how the crash surfaces changes except that the trace now survives it.
 */
public class VogtApplication extends Application {
    private static final String TAG = "VogtApplication";
    /** Written here, read and cleared by {@link MainActivity} on the next launch. */
    static final String CRASH_FILE = "last-crash.txt";

    @Override
    public void onCreate() {
        super.onCreate();
        final Thread.UncaughtExceptionHandler previous =
            Thread.getDefaultUncaughtExceptionHandler();
        Thread.setDefaultUncaughtExceptionHandler((thread, throwable) -> {
            try {
                persistCrash(thread, throwable);
            } catch (Throwable recorderFailure) {
                // The recorder must never change how the crash itself surfaces.
                Log.e(TAG, "failed to persist crash trace", recorderFailure);
            }
            if (previous != null) {
                previous.uncaughtException(thread, throwable);
            }
        });
    }

    private void persistCrash(Thread thread, Throwable throwable) {
        StringWriter buffer = new StringWriter();
        PrintWriter writer = new PrintWriter(buffer);
        writer.println("Vogt " + versionLabel());
        writer.println("thread: " + (thread != null ? thread.getName() : "?"));
        writer.println("device: " + Build.MANUFACTURER + " " + Build.MODEL
            + " (Android " + Build.VERSION.RELEASE + ", API " + Build.VERSION.SDK_INT + ")");
        writer.println();
        throwable.printStackTrace(writer);
        writer.flush();

        File file = new File(getFilesDir(), CRASH_FILE);
        try (OutputStream out = new FileOutputStream(file)) {
            out.write(buffer.toString().getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            Log.e(TAG, "could not write " + CRASH_FILE, e);
        }
    }

    /**
     * versionName + versionCode read from {@link PackageManager}, NOT
     * {@code BuildConfig} — buildConfig generation can be off under AGP 8, and the
     * whole point of the label is to make a stale reinstall (the operator's "still
     * crashes" that may in fact have been the previous build) obvious on sight.
     */
    private String versionLabel() {
        try {
            PackageManager pm = getPackageManager();
            PackageInfo info = pm.getPackageInfo(getPackageName(), 0);
            long code = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P
                ? info.getLongVersionCode()
                : info.versionCode;
            return info.versionName + " (build " + code + ")";
        } catch (Exception e) {
            return "unknown version";
        }
    }

    /** Read the persisted crash trace, or {@code null} if it cannot be read. */
    static String readCrash(File file) {
        try (java.io.InputStream in = new java.io.FileInputStream(file)) {
            ByteArrayOutputStream buffer = new ByteArrayOutputStream();
            byte[] chunk = new byte[4096];
            int read;
            while ((read = in.read(chunk)) != -1) {
                buffer.write(chunk, 0, read);
            }
            return new String(buffer.toByteArray(), StandardCharsets.UTF_8);
        } catch (IOException e) {
            return null;
        }
    }
}

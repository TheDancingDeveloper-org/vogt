package com.getcapacitor.myapp;

import static org.junit.Assert.*;

import android.content.Context;
import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import com.sprooty.vogt.MainActivity;
import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Instrumented test, which will execute on an Android device.
 *
 * @see <a href="http://d.android.com/tools/testing">Testing documentation</a>
 */
@RunWith(AndroidJUnit4.class)
public class ExampleInstrumentedTest {

    @Test
    public void usesVogtAppContext() {
        // Context of the app under test.
        Context appContext = InstrumentationRegistry.getInstrumentation().getTargetContext();

        // The same instrumentation suite runs against both the production
        // package and the side-by-side dev package (#191). The Java namespace
        // is intentionally shared, so use the runtime application context
        // rather than a generated BuildConfig (which is not available to this
        // instrumentation source set in every Android Gradle configuration).
        assertTrue(
                "unexpected Vogt application id: " + appContext.getPackageName(),
                "com.sprooty.vogt".equals(appContext.getPackageName())
                        || "com.sprooty.vogt.dev".equals(appContext.getPackageName()));
    }

    @Test
    public void launchesMainActivity() {
        try (ActivityScenario<MainActivity> scenario = ActivityScenario.launch(MainActivity.class)) {
            scenario.onActivity(activity -> assertNotNull(activity.getBridge()));
        }
    }
}

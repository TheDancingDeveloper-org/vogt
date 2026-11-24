package com.thedancingdeveloper.vogt;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import android.view.ViewGroup;

import androidx.lifecycle.Lifecycle;
import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;

import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Regression guard for the launcher crash.
 *
 * <p>{@link ServerActivity} is the launcher and an {@code AppCompatActivity}, so
 * it must run under a {@code Theme.AppCompat} descendant. It was shipped on the
 * splash theme ({@code Theme.SplashScreen}, not AppCompat), and
 * {@code setContentView} then threw <em>"You need to use a Theme.AppCompat theme
 * (or descendant) with this activity"</em> in {@code onCreate} — the process died
 * on launch, before {@link MainActivity} (which surfaces the crash report) could
 * run, so it looked like the app simply would not open.
 *
 * <p>Reaching {@code RESUMED} means {@code onCreate}/{@code setContentView}
 * completed: under a non-AppCompat theme the launch throws there and this is
 * never reached. This fails on the old theme and passes on the fix. Deliberately
 * uses only {@code ActivityScenario} and a direct view-tree check — no Espresso,
 * whose input-injection reflection breaks on newer platform images.
 */
@RunWith(AndroidJUnit4.class)
public class ServerActivityLaunchTest {

    @Test
    public void launchesToTheChooserWithoutCrashing() {
        try (ActivityScenario<ServerActivity> scenario =
                 ActivityScenario.launch(ServerActivity.class)) {
            assertEquals(Lifecycle.State.RESUMED, scenario.getState());
            // The form inflated too: the vertical layout carries its title, the
            // URL field and the Connect button.
            scenario.onActivity(activity -> {
                ViewGroup content = activity.findViewById(android.R.id.content);
                ViewGroup form = (ViewGroup) content.getChildAt(0);
                assertTrue("ServerActivity content did not inflate",
                    form != null && form.getChildCount() >= 3);
            });
        }
    }
}

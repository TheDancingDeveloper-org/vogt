package com.thedancingdeveloper.vogt;

import android.content.Intent;
import android.os.Bundle;
import android.text.InputType;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import androidx.appcompat.app.AlertDialog;
import androidx.appcompat.app.AppCompatActivity;

/** Local entry point, also available with Back when a server is unreachable. */
public class ServerActivity extends AppCompatActivity {
    /**
     * Whether a fresh launch should go straight to the saved server rather
     * than ask for one again (WI-924). Only a first creation with a usable
     * saved origin forwards: a restored instance (the system rebuilding the
     * task) is already under the activity it forwarded to, and the user who
     * came Back here to change servers is not created again, only resumed.
     */
    static boolean shouldForward(String savedOrigin, boolean restored) {
        return !restored && ServerAddress.normalize(savedOrigin) != null;
    }

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        // Stay as the task's root, under MainActivity, so Android Back from
        // the app still returns here to change servers.
        if (shouldForward(getSharedPreferences("server", MODE_PRIVATE).getString("origin", ""),
                state != null)) {
            startActivity(new Intent(this, MainActivity.class));
        }
        LinearLayout form = new LinearLayout(this);
        form.setOrientation(LinearLayout.VERTICAL);
        int pad = Math.round(24 * getResources().getDisplayMetrics().density);
        form.setPadding(pad, pad * 3, pad, pad);
        TextView title = new TextView(this);
        title.setText("Connect to Vogt");
        title.setTextSize(28);
        form.addView(title);
        TextView explanation = new TextView(this);
        explanation.setText("Enter your Vogt server. Its interface and workspace will load together. Return here with Android Back to change servers.");
        form.addView(explanation);
        EditText server = new EditText(this);
        server.setSingleLine(true);
        server.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        server.setHint("https://vogt.example.com");
        server.setContentDescription("Vogt server URL");
        server.setText(getSharedPreferences("server", MODE_PRIVATE).getString("origin", ""));
        form.addView(server);
        Button connect = new Button(this);
        connect.setText("Connect");
        connect.setOnClickListener(view -> {
            String origin = ServerAddress.normalize(server.getText().toString());
            if (origin == null) {
                server.setError("Enter an HTTP or HTTPS origin without a path, credentials, query or fragment.");
                return;
            }
            Runnable open = () -> {
                getSharedPreferences("server", MODE_PRIVATE).edit().putString("origin", origin).commit();
                startActivity(new Intent(this, MainActivity.class));
            };
            if (origin.startsWith("http://")) {
                new AlertDialog.Builder(this).setTitle("Use an HTTP server?")
                    .setMessage("HTTP sends your token without TLS encryption. Use it only on a trusted private network, such as a VPN.")
                    .setNegativeButton("Cancel", null)
                    .setPositiveButton("Connect", (dialog, which) -> open.run()).show();
            } else open.run();
        });
        form.addView(connect);
        setContentView(form);
    }
}

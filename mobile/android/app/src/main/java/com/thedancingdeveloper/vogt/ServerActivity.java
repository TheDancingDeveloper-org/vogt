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
    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
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

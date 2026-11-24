package com.thedancingdeveloper.vogt;

import java.net.URI;
import java.net.URISyntaxException;
import java.util.Locale;

/** The user's one code/data origin, without credentials or routing suffixes. */
final class ServerAddress {
    static String normalize(String raw) {
        if (raw == null) return null;
        try {
            URI uri = new URI(raw.trim());
            String scheme = uri.getScheme();
            if (scheme == null || !(scheme.equalsIgnoreCase("https") || scheme.equalsIgnoreCase("http"))
                    || uri.getHost() == null || uri.getRawUserInfo() != null
                    || uri.getRawQuery() != null || uri.getRawFragment() != null
                    || !(uri.getRawPath().isEmpty() || uri.getRawPath().equals("/"))
                    || uri.getPort() > 65535 || uri.getPort() == 0) return null;
            return scheme.toLowerCase(Locale.ROOT) + "://" + uri.getHost().toLowerCase(Locale.ROOT)
                + (uri.getPort() == -1 ? "" : ":" + uri.getPort());
        } catch (URISyntaxException error) {
            return null;
        }
    }
}

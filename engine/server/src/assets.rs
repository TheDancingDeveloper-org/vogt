use axum::{
    body::Body,
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Embedded PWA bundle. Built by `web/` (pnpm build) prior to `cargo build`.
///
/// **`web/dist/` must exist before this crate compiles.** A missing directory
/// does not yield an empty asset set and a headless server: the derive fails to
/// expand and the crate dies with three errors about `WebAssets` having no
/// `get`, which reads like a version mismatch in `rust-embed` rather than a
/// missing build step. An empty `web/dist/` is enough, so `build.rs` writes a
/// placeholder `index.html` there when the real PWA bundle is absent — that is
/// what lets a bare `cargo build` succeed on a clean clone. The Docker build
/// still runs the real `pnpm build` first, so the shipped image embeds the
/// actual bundle rather than the placeholder.
#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
#[prefix = ""]
struct WebAssets;

fn serve(path: &str) -> Response {
    match WebAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut resp = Response::new(Body::from(file.data));
            if let Ok(v) = HeaderValue::from_str(mime.as_ref()) {
                resp.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            // HTML and the service worker are unversioned entry points and must
            // be revalidated so a browser can discover each deployment. Vite's
            // hashed JS/CSS assets can be cached immutably.
            let cache = if path == "index.html"
                || path == "/"
                || path.is_empty()
                || path == "sw.js"
                || path == "demo-manifest.json"
                || path == "demo-build.json"
            {
                "no-store, must-revalidate"
            } else {
                "public, max-age=31536000, immutable"
            };
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            resp
        }
        None => not_found(),
    }
}

fn not_found() -> Response {
    if let Some(idx) = WebAssets::get("index.html") {
        // SPA fallback: serve index.html for unknown routes so deep links work
        // when a future router upgrade moves us off HashRouter.
        let mut resp = Response::new(Body::from(idx.data));
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, no-store, must-revalidate"),
        );
        return resp;
    }
    (
        StatusCode::NOT_FOUND,
        "Vogt web bundle not present (build with: cd web && pnpm build).\n",
    )
        .into_response()
}

pub async fn root() -> Response {
    serve("index.html")
}

/// Catch-all asset handler for `/{*path}`.
pub async fn asset_wild(Path(path): Path<String>) -> Response {
    if path.is_empty() {
        return serve("index.html");
    }
    serve(&path)
}

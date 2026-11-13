//! The web app (`web/`), served from the binary when built with the `web`
//! feature: `GET /` is `index.html`, `GET /assets/…` the built files.
//! Neither needs the token (the files are not secret); the socket at `/v1`
//! still does. Without the feature, `GET /` says so in a line of text.

use std::net::SocketAddr;

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

/// Whether this build carries the web app.
pub const EMBEDDED: bool = cfg!(feature = "web");

/// The page's Content-Security-Policy: scripts, styles and images from the
/// page's own origin only, sockets anywhere (the user types the address),
/// no plugins, no framing, no forms, no `<base>`.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     img-src 'self'; font-src 'self'; connect-src ws: wss:; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

const NO_APP: &str = "this build of triad has no web app; build it with `--features web` \
     (after `pnpm -C web build`), or run the app from web/ with `pnpm dev`\n";

fn find(path: &str) -> Option<(&'static str, &'static [u8])> {
    assets::ASSETS
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, ty, body)| (*ty, *body))
}

fn file(ty: &'static str, body: &'static [u8], cache: &'static str) -> Response {
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ty));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

/// `GET /`: the app's page, never cached, under the CSP.
pub async fn index() -> Response {
    match find("/index.html") {
        Some((ty, body)) => {
            let mut r = file(ty, body, "no-cache");
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CSP),
            );
            h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
            r
        }
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            NO_APP,
        )
            .into_response(),
    }
}

/// `GET /assets/…`: the built files. Their names carry a content hash, so
/// they may be cached for good.
pub async fn asset(Path(rest): Path<String>) -> Response {
    match find(&format!("/assets/{rest}")) {
        Some((ty, body)) => file(ty, body, "public, max-age=31536000, immutable"),
        None => (StatusCode::NOT_FOUND, "no such file\n").into_response(),
    }
}

/// The origins a page served from `local` has, which may open the socket
/// without `--allow-origin`: the bound loopback address itself and
/// `localhost`, on the bound port. They are fixed at startup from the bound
/// address, never taken from a request's `Host` (which a DNS-rebinding page
/// controls). The IPv6 form is allowed only for an IPv6 bind: bound to
/// 127.0.0.1, the same port on ::1 is not ours, and another local process
/// could serve a page there. A non-loopback bind gets none; pass
/// `--allow-origin` for the name the page is reached by.
pub fn own_origins(local: SocketAddr) -> Vec<String> {
    if !EMBEDDED || !local.ip().is_loopback() {
        return Vec::new();
    }
    let port = local.port();
    let own = match local {
        SocketAddr::V4(a) => format!("http://{}:{port}", a.ip()),
        SocketAddr::V6(a) => format!("http://[{}]:{port}", a.ip()),
    };
    vec![own, format!("http://localhost:{port}")]
}

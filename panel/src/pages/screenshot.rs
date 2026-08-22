//! `/dev/screenshot` — what the TV is showing right now, proxied from the
//! daemon's HTTP bridge `GET /screenshot`.
//!
//! Split out of the Dev page in `docs/PANEL_IA.md` phase 4: it is the one
//! read-only surface on a page otherwise made of destructive buttons, and at
//! full width a 4K capture is finally legible.
//!
//! ## Three routes, one capability
//!
//! | Route | Purpose |
//! |---|---|
//! | `GET /dev/screenshot` | this page |
//! | `POST /dev/screenshot/capture` | confirm the bridge answers, read provenance |
//! | `GET /dev/screenshot/image` | the PNG proxy the `<img>` points at |
//!
//! The proxy used to *be* `GET /dev/screenshot`; it was renamed to free that
//! path for the page. All three sit in the `Gate::Screenshot` block.
//!
//! Degradation: the `<img>` is only ever emitted after a capture call already
//! succeeded, so a daemon-down or bridge-unconfigured state renders an honest
//! banner rather than a broken image.

use askama::Template;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};

use crate::capabilities::Chrome;
use crate::state::{AppState, SharedState};

#[derive(Template)]
#[template(path = "screenshot.html")]
struct ScreenshotTemplate {
    chrome: Chrome,
    /// `[panel].http_bridge_base` — the capability is the NODE's statement
    /// about itself and registers these routes on an MCP-only node too, where
    /// this panel has no HTTP bridge to call. The page says so up front rather
    /// than letting the operator find out by clicking.
    bridge_configured: bool,
}

/// `GET /dev/screenshot` — the viewer. No bridge call on load; the capture is
/// an explicit click.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(ScreenshotTemplate {
        chrome: Chrome::new(&state.caps, "dev.screenshot"),
        bridge_configured: state.cfg.http_bridge_base.is_some(),
    })
}

#[derive(Template)]
#[template(path = "screenshot_result.html")]
struct ScreenshotResultTemplate {
    ok: bool,
    message: String,
    sha: String,
    branch: String,
    version: String,
    captured_at: String,
    cache_bust: u128,
}

/// `POST /dev/screenshot/capture` — calls the bridge screenshot endpoint to
/// confirm reachability and read provenance, then (on success) renders an
/// `<img>` pointing at the [`image`] proxy route. The `<img>` tag is only ever
/// emitted when this call already succeeded, so a daemon-down or
/// bridge-unconfigured state degrades to a banner — never a broken image.
pub async fn capture(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_capture(&state).await)
}

pub async fn render_capture(state: &AppState) -> String {
    match state.bridge.screenshot().await {
        Ok(shot) => {
            let tmpl = ScreenshotResultTemplate {
                ok: true,
                message: String::new(),
                sha: shot.sha,
                branch: shot.branch,
                version: shot.version,
                captured_at: shot.captured_at,
                cache_bust: now_millis(),
            };
            tmpl.render().unwrap_or_else(|e| {
                format!("<p class=\"banner banner-error\">render error: {e}</p>")
            })
        }
        Err(e) => {
            let reason = if e.is_configured() {
                "unreachable"
            } else {
                "not configured"
            };
            let tmpl = ScreenshotResultTemplate {
                ok: false,
                message: format!("HTTP bridge {reason} — see the banner above. ({e})"),
                sha: String::new(),
                branch: String::new(),
                version: String::new(),
                captured_at: String::new(),
                cache_bust: 0,
            };
            tmpl.render().unwrap_or_else(|e| {
                format!("<p class=\"banner banner-error\">render error: {e}</p>")
            })
        }
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `GET /dev/screenshot/image` — proxies the daemon's `GET /screenshot` PNG
/// bytes (`Content-Type: image/png`). Only ever linked from the DOM after
/// [`capture`] has already confirmed the bridge is reachable, so a direct hit
/// here (bridge down between the two calls) degrades to a `503` text body
/// rather than corrupting an `<img>` tag's expected type.
pub async fn image(State(state): State<SharedState>) -> Response {
    match state.bridge.screenshot().await {
        Ok(shot) => {
            let mut resp = Response::new(Body::from(shot.png));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("image/png"),
            );
            resp
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("screenshot unavailable: {e}"),
        )
            .into_response(),
    }
}

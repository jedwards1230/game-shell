//! Global daemon-reachability dot in `base.html`'s topnav. Deliberately its
//! own tiny module rather than owned by any one page — `base.html`/nav is
//! M1 territory per the page-ownership contract, and this partial is nav
//! chrome, not a page.

use std::time::Duration;

use axum::extract::State;
use axum::response::{Html, IntoResponse};

use crate::state::{AppState, SharedState};

/// Timeout for the nav dot's `status` probe — deliberately much shorter
/// than [`crate::ipc::IpcTransport`]'s 3s default so a hung/unreachable daemon
/// can never make the ~10s polling loop pile up requests.
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// `GET /nav/daemon-status` — the topnav's polled partial (`hx-trigger="load,
/// every 10s"`, `hx-swap="innerHTML"` on the containing span).
pub async fn daemon_status_dot(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_dot(&state).await)
}

/// A fast, cheap `status` probe: green dot when the daemon answers within
/// [`PROBE_TIMEOUT`], red when it doesn't. Always succeeds — this must never
/// slow or break page navigation.
pub async fn render_dot(state: &AppState) -> String {
    let reachable = state
        .node
        .command_timeout("status", PROBE_TIMEOUT)
        .await
        .is_ok();
    let (dot_class, title) = if reachable {
        ("dot-ok", "daemon reachable")
    } else {
        ("dot-error", "daemon unreachable")
    };
    format!(r#"<span class="dot {dot_class}" title="{title}"></span>daemon"#)
}

/// [`render_dot`]'s content wrapped as an htmx out-of-band swap fragment,
/// targeting the topnav's `#nav-daemon-status` span directly (rather than
/// waiting up to the normal 10s poll interval) — used by
/// `pages::dev`'s post-action verification (#7) so a restart/build/deploy
/// response updates the nav dot immediately instead of leaving it stale
/// until its own next poll. An OOB swap replaces the WHOLE matched element
/// (outerHTML), so this fragment re-declares the same `hx-get`/`hx-trigger`/
/// `hx-swap` attributes `base.html` gives the span — otherwise the
/// replacement would carry the fresh dot but silently drop the polling loop
/// that keeps it current afterward.
pub async fn render_oob(state: &AppState) -> String {
    let inner = render_dot(state).await;
    format!(
        r#"<span class="nav-daemon-status" id="nav-daemon-status" hx-swap-oob="true" hx-get="/nav/daemon-status" hx-trigger="load, every 10s" hx-swap="innerHTML">{inner}</span>"#
    )
}

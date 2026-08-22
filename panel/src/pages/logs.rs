//! `/system/logs` — shell log (via the daemon HTTP bridge) and daemon log (via
//! `journalctl`, direct exec) side by side. Never 500s on a data-source
//! failure — each panel degrades inline.

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::config;
use crate::state::{AppState, SharedState};
use crate::text::strip_ansi;

const DEFAULT_LINES: usize = 200;
const MAX_LINES: usize = 1000;

#[derive(Template)]
#[template(path = "logs.html")]
struct LogsTemplate {
    chrome: Chrome,
}

/// `GET /logs` — the page shell.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(LogsTemplate {
        chrome: Chrome::new(&state.caps, "system.logs"),
    })
}

#[derive(Deserialize)]
pub struct LogsQuery {
    lines: Option<usize>,
    filter: Option<String>,
    /// Presence (any value, e.g. `on`) means "drop lines containing the
    /// known icon-load noise" — a preset server-side exclude filter,
    /// distinct from the free-text `filter` substring match.
    hide_icons: Option<String>,
}

/// `GET /logs/view` — the refreshable log partial.
pub async fn view(
    State(state): State<SharedState>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let lines = q.lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES);
    Html(render_view(&state, lines, q.filter.as_deref(), q.hide_icons.is_some()).await)
}

#[derive(Template)]
#[template(path = "logs_view.html")]
struct LogsViewTemplate {
    shell_available: bool,
    shell_log: String,
    shell_message: String,
    daemon_log: String,
}

/// Build the log-panels partial HTML for `lines`/`filter`/`hide_icons`. The
/// shell log comes from the daemon HTTP bridge (`bridge.dev_logs`); when the
/// bridge is unconfigured or unreachable, that panel shows an inline message
/// instead of erroring. The daemon log comes from `journalctl` via the exec
/// tier and always attempts to render (a read failure degrades to an inline
/// message too). Both panes are ANSI-stripped (source processes emit color
/// codes meant for a terminal, not a `<pre>` block) and, when `hide_icons` is
/// set, post-filtered to drop the known icon-load noise line.
pub async fn render_view(
    state: &AppState,
    lines: usize,
    filter: Option<&str>,
    hide_icons: bool,
) -> String {
    let (shell_available, shell_log, shell_message) =
        match state.bridge.dev_logs(lines, filter).await {
            Ok(body) => (true, clean_log(&body, hide_icons), String::new()),
            Err(e) => {
                let reason = if e.is_configured() {
                    "unreachable"
                } else {
                    "not configured"
                };
                (
                    false,
                    String::new(),
                    format!("HTTP bridge {reason} — see the Dev page. ({e})"),
                )
            }
        };

    let daemon_log = match state
        .recovery
        .journal_unit(&config::daemon_journal_unit(), lines, filter)
        .await
    {
        Ok(body) => clean_log(&body, hide_icons),
        Err(e) => format!("journal read failed: {e}"),
    };

    let tmpl = LogsViewTemplate {
        shell_available,
        shell_log,
        shell_message,
        daemon_log,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// Strip ANSI escapes, then optionally drop lines matching the "icon load
/// noise" preset filter.
fn clean_log(raw: &str, hide_icons: bool) -> String {
    let stripped = strip_ansi(raw);
    if hide_icons {
        drop_icon_noise(&stripped)
    } else {
        stripped
    }
}

/// The "Hide icon noise" preset: drop every line containing this substring.
const ICON_NOISE_MARKER: &str = "Could not load icon";

fn drop_icon_noise(s: &str) -> String {
    s.lines()
        .filter(|line| !line.contains(ICON_NOISE_MARKER))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_icon_noise_removes_matching_lines_only() {
        let out = "line one\nCould not load icon for foo\nline two";
        assert_eq!(drop_icon_noise(out), "line one\nline two");
    }

    #[test]
    fn clean_log_strips_ansi_and_applies_hide_icons() {
        let raw = "\x1b[32mok\x1b[0m\nCould not load icon for bar\nanother line";
        assert_eq!(clean_log(raw, true), "ok\nanother line");
        assert_eq!(
            clean_log(raw, false),
            "ok\nCould not load icon for bar\nanother line"
        );
    }
}

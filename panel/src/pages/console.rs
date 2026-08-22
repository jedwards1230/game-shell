//! `/dev/console` — the raw IPC line console: send any single line of the
//! daemon's command vocabulary and see the raw reply.
//!
//! Moved here from the Tools page in `docs/PANEL_IA.md` phase 4, and the move
//! is the point: this is an arbitrary-command escape hatch, so it belongs
//! beside the other break-glass surfaces rather than under a general-purpose
//! tab. After phase 4 every `allow_dangerous`-gated control in the panel is in
//! the Dev group **except** `POST /system/updates/apply`, which stays with the
//! pending-package table and the job poll it belongs to (see `docs/PANEL.md`
//! § Dangerous actions).
//!
//! ## Two gates, deliberately different
//!
//! The **page** is [`Gate::Node`] — it exists iff a node answered the
//! handshake. `POST /dev/console/raw` is in the `allow_dangerous` block, so
//! with `allow_dangerous = false` (the default, and what the reference node
//! htpc-1 runs) the route does not exist and the page renders an explanatory
//! banner with **no form at all** — never a button that 404s.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::pages::ipc_console::{error_result, esc, pretty_block, result_html};
use crate::state::{AppState, SharedState};

/// Commands that belong to another page's guarded flow (Shell ▸ Advanced,
/// Controllers) — still allowed through the raw console, but with a warning,
/// since sending them here bypasses that page's own validation/UI.
///
/// **Duplicated as a JS regex in `console.html`** (`var GUARDED = /^(…)\b/`)
/// to sharpen the confirm prompt client-side. The two halves must stay in
/// sync; `crate::tests::the_console_guard_list_matches_its_template_regex`
/// pins them together.
pub const WARN_COMMANDS: &[&str] = &["set-config", "set-binding", "grab", "release", "handoff"];

#[derive(Template)]
#[template(path = "console.html")]
struct ConsoleTemplate {
    chrome: Chrome,
    /// `[panel].allow_dangerous` (S5) — gates the raw console, which drives
    /// the whole IPC vocabulary and is therefore an arbitrary-command escape
    /// hatch. `POST /dev/console/raw` is not registered when this is false,
    /// so the form is not rendered either.
    allow_dangerous: bool,
}

/// `GET /dev/console` — no IPC calls on load.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(ConsoleTemplate {
        chrome: Chrome::new(&state.caps, "dev.console"),
        allow_dangerous: state.cfg.allow_dangerous,
    })
}

#[derive(Deserialize)]
pub struct RawForm {
    cmd: String,
}

/// `POST /dev/console/raw` — sends any single IPC line as-is and shows the raw
/// reply. Rejects an empty line or one containing a newline/control character
/// (a smuggled second command). Commands in [`WARN_COMMANDS`] are still sent,
/// with a warning banner on the result.
pub async fn raw(State(state): State<SharedState>, Form(form): Form<RawForm>) -> impl IntoResponse {
    Html(render_raw(&state, &form.cmd).await)
}

pub async fn render_raw(state: &AppState, cmd: &str) -> String {
    let line = cmd.trim();
    if line.is_empty() {
        return error_result("command must not be empty");
    }
    if line.chars().any(|c| c.is_control()) {
        return error_result("command must be a single line with no control characters");
    }
    let word = line.split_whitespace().next().unwrap_or("");
    let warning = if WARN_COMMANDS.contains(&word) {
        format!(
            "{word} belongs to another page's guarded flow (Shell ▸ Advanced/Controllers) — \
             sending it here bypasses that page's own validation/UI. Proceeding anyway."
        )
    } else {
        String::new()
    };
    match state.node.command(line).await {
        Ok(reply) => result_html(true, &warning, &pretty_block(&reply)),
        Err(e) => result_html(
            false,
            &warning,
            &format!("<pre>{}</pre>", esc(&e.to_string())),
        ),
    }
}

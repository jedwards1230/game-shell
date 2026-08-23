//! The shared IPC-console plumbing: send one daemon IPC line, render the
//! reply, and validate the user-supplied tokens that become part of a command
//! line.
//!
//! **Not a page.** The Tools page was dissolved in `docs/PANEL_IA.md` phase 4
//! — it was a grab-bag grouped by *IPC domain* rather than by any job an
//! operator does — and its domains moved to the pages that own their subject:
//!
//! | Page | Route prefix | Was |
//! |---|---|---|
//! | Devices ▸ Network | `/devices/network/…` | Tools ▸ Network + Bluetooth |
//! | Devices ▸ Display & Audio | `/devices/display-audio/power/…` | Tools ▸ Power |
//! | Remote ▸ Navigation | `/remote/navigation/…` | Tools ▸ Navigation |
//! | Remote ▸ Launcher | `/remote/launcher/…` | Tools ▸ Apps |
//! | Dev ▸ Console | `/dev/console/raw` | Tools ▸ Raw console |
//!
//! What is left here is what all five share, in the same way
//! [`crate::pages::settings`] holds what the five settings forms share: the
//! `#…-result` partial, the reply pretty-printer, and the argument
//! validators. Tools ▸ System is not in the table because it was deleted
//! rather than moved — `sys-status`/`sys-metrics`/`storage-status`/`build-info`
//! are already on the Overview tiles, and the two `controllerdb-*` buttons
//! were exact duplicates of the Controllers page's own.
//!
//! Degradation: a [`crate::transport::TransportError`] (including
//! daemon-unreachable) renders as a failed result, never a 500.

use askama::Template;

use crate::state::AppState;

#[derive(Template)]
#[template(path = "ipc_result.html")]
struct IpcResultTemplate {
    ok: bool,
    warning: String,
    body_html: String,
}

/// The result partial every one of these pages swaps into its own
/// `#…-result` div. `body_html` is inlined unescaped — callers pass either
/// [`pretty_block`] output or markup they built with [`esc`].
pub fn result_html(ok: bool, warning: &str, body_html: &str) -> String {
    let tmpl = IpcResultTemplate {
        ok,
        warning: warning.to_string(),
        body_html: body_html.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// A failed result carrying one escaped message.
pub fn error_result(msg: &str) -> String {
    result_html(false, "", &format!("<pre>{}</pre>", esc(msg)))
}

/// Send `line` over IPC and render the reply: pretty-printed JSON when the
/// reply parses as JSON, the bare text otherwise. A `TransportError`
/// (including daemon-unreachable) renders as a failed result, never a 500.
pub async fn run_line(state: &AppState, line: &str) -> String {
    match state.node.command(line).await {
        Ok(reply) => result_html(true, "", &pretty_block(&reply)),
        Err(e) => error_result(&e.to_string()),
    }
}

/// A `<pre>` block of the reply, pretty-printed when it is JSON.
pub fn pretty_block(reply: &str) -> String {
    let text = match serde_json::from_str::<serde_json::Value>(reply) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| reply.to_string()),
        Err(_) => reply.to_string(),
    };
    format!("<pre>{}</pre>", esc(&text))
}

/// HTML-escape text destined for markup these modules build by hand.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Reject empty, whitespace, or control-character tokens — every
/// user-supplied argument that becomes part of an IPC command line (intent
/// names, wm_class, MAC addresses, interface names, ping hosts) goes through
/// this. Returns the trimmed token on success.
pub fn validate_token(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("value must not be empty".to_string());
    }
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "value {t:?} must not contain whitespace or control characters"
        ));
    }
    Ok(t.to_string())
}

/// [`validate_token`] plus a path check: an interface name reaches a sysfs
/// path on the daemon side, so it may contain neither a separator nor `..`.
pub fn validate_iface(s: &str) -> Result<String, String> {
    let t = validate_token(s)?;
    if t.contains('/') || t.contains("..") {
        return Err(format!("invalid interface name {t:?}"));
    }
    Ok(t)
}

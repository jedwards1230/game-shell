//! `/system/processes` — purely read-only observation of what is running:
//! Hyprland windows via IPC (`hypr-active`/`hypr-clients`/`hypr-monitors`)
//! and a top-processes snapshot via `ps`.
//!
//! Unit control moved to [`super::services`] and the pacman section to
//! [`super::updates`] in `docs/PANEL_IA.md` phase 2 — **this page mutates
//! nothing** and renders no action control at all.
//!
//! Degradation: the process list is exec-based (always available regardless
//! of the daemon); the Hyprland section is IPC-based and shows its own
//! "unavailable" note (daemon down, or the Hyprland actor itself down)
//! without failing the rest of the page — `GET /system/processes` is always
//! 200, never a 500.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::state::{AppState, SharedState};
use crate::transport::TransportError;

/// `hypr-clients` reply shape (`docs/IPC_PROTOCOL.md` § `hypr-clients`).
#[derive(Deserialize)]
struct HyprClientJson {
    class: String,
    title: String,
    address: String,
    workspace: String,
}

struct HyprClientView {
    class: String,
    title: String,
    workspace: String,
    address: String,
}

/// One row of the `ps axo pid,pcpu,pmem,comm --sort=-pcpu` snapshot (#15 —
/// rendered as a styled table instead of raw `<pre>` text).
struct ProcRow {
    pid: String,
    pcpu: String,
    pmem: String,
    comm: String,
}

/// Parse `ps axo pid,pcpu,pmem,comm`'s whitespace-column output into rows,
/// skipping the header line. `comm` is whatever's left after the first three
/// whitespace-delimited fields (defensive — this is just a process name, not
/// an argv, so it shouldn't itself contain spaces, but joining the remainder
/// rather than taking a fixed 4th token is cheap insurance). A line that
/// doesn't even have 3 columns (never expected from real `ps` output) is
/// skipped rather than panicking or emitting a garbled row.
fn parse_top_processes(raw: &str) -> Vec<ProcRow> {
    raw.lines()
        .skip(1) // header: "PID %CPU %MEM COMMAND"
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.to_string();
            let pcpu = parts.next()?.to_string();
            let pmem = parts.next()?.to_string();
            let comm: String = parts.collect::<Vec<_>>().join(" ");
            if comm.is_empty() {
                return None;
            }
            Some(ProcRow {
                pid,
                pcpu,
                pmem,
                comm,
            })
        })
        .collect()
}

#[derive(Template)]
#[template(path = "processes.html")]
struct ProcessesTemplate {
    chrome: Chrome,
    hypr_available: bool,
    hypr_active: String,
    /// Whether [`Self::hypr_active`] is the daemon's "nothing focused" answer.
    ///
    /// Hyprland reports no active window as an empty JSON object, which
    /// `pretty_or_raw` faithfully renders as a literal `{}` — technically
    /// honest and useless to read. The Clients section right below already
    /// renders a sentence in the same situation ("No Hyprland clients
    /// reported"); this lets Active window match it instead of being the one
    /// place the panel shows the operator raw JSON punctuation.
    hypr_active_empty: bool,
    hypr_clients_rows: Vec<HyprClientView>,
    hypr_clients_error: String,
    hypr_monitors: String,
    top_rows: Vec<ProcRow>,
    top_error: String,
}

/// `GET /system/processes` — gathers both sections synchronously (mirrors
/// `pages::dashboard::render_tiles`'s degrade-per-section approach, just
/// folded into the one page render rather than a separate polled partial).
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let active_res = state.node.command("hypr-active").await;
    let clients_res = state.node.command("hypr-clients").await;
    let monitors_res = state.node.command("hypr-monitors").await;
    // Reachable if any one of the three succeeded — a single command
    // failing (e.g. a transient IPC hiccup) shouldn't blank the whole
    // section when the others came back fine.
    let hypr_available = active_res.is_ok() || clients_res.is_ok() || monitors_res.is_ok();

    let (hypr_clients_rows, hypr_clients_error) = match clients_res {
        Ok(s) => match serde_json::from_str::<Vec<HyprClientJson>>(&s) {
            Ok(list) => (
                list.into_iter()
                    .map(|c| HyprClientView {
                        class: c.class,
                        title: c.title,
                        workspace: c.workspace,
                        address: c.address,
                    })
                    .collect(),
                String::new(),
            ),
            Err(e) => (
                Vec::new(),
                format!("failed to parse hypr-clients reply: {e}"),
            ),
        },
        Err(e) => (Vec::new(), e.to_string()),
    };

    let (top_rows, top_error) = match state.recovery.top_processes().await {
        Ok(out) => (parse_top_processes(&out), String::new()),
        Err(e) => (Vec::new(), format!("ps failed: {e}")),
    };

    let hypr_active = pretty_or_raw(active_res);
    let hypr_active_empty = is_empty_json_object(&hypr_active);

    let tmpl = ProcessesTemplate {
        chrome: Chrome::new(&state.caps, "system.processes"),
        hypr_available,
        hypr_active_empty,
        hypr_active,
        hypr_clients_rows,
        hypr_clients_error,
        hypr_monitors: pretty_or_raw(monitors_res),
        top_rows,
        top_error,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// Whether `s` is an empty JSON object — Hyprland's "nothing is focused".
///
/// Parses rather than string-matching, so it holds regardless of how
/// `pretty_or_raw` spaced the braces, and so a genuine payload can never be
/// mistaken for empty. A non-object (an error string, say) is never "empty":
/// those must keep rendering verbatim, since the text IS the diagnostic.
fn is_empty_json_object(s: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.is_empty()))
        .unwrap_or(false)
}

fn pretty_or_raw(res: Result<String, TransportError>) -> String {
    match res {
        Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(s),
            Err(_) => s,
        },
        Err(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a genuinely empty object counts as "nothing focused". The two
    /// directions matter differently: a false positive hides a real active
    /// window, and a false negative is the `{}` this replaced. An error string
    /// must never read as empty — for those the text IS the diagnostic.
    #[test]
    fn only_an_empty_json_object_reads_as_no_active_window() {
        for empty in ["{}", "  {}  ", "{\n}\n"] {
            assert!(
                is_empty_json_object(empty),
                "{empty:?} should read as no active window"
            );
        }
        for present in [
            r#"{"class":"tv.plex.Plex"}"#,
            r#"{"class":""}"#,
            "[]",
            "null",
            "",
            "transport error: connection refused",
            "{ not json",
        ] {
            assert!(
                !is_empty_json_object(present),
                "{present:?} must NOT be swallowed as an empty active window"
            );
        }
    }

    #[test]
    fn parse_top_processes_skips_header_and_splits_columns() {
        let raw = "  PID  %CPU  %MEM COMMAND\n\
                      1234  12.3   4.5 firefox\n\
                       567   0.1   0.2 systemd";
        let rows = parse_top_processes(raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, "1234");
        assert_eq!(rows[0].pcpu, "12.3");
        assert_eq!(rows[0].pmem, "4.5");
        assert_eq!(rows[0].comm, "firefox");
        assert_eq!(rows[1].comm, "systemd");
    }

    #[test]
    fn parse_top_processes_joins_multi_word_comm() {
        // `comm` shouldn't realistically contain spaces, but the parser
        // shouldn't silently drop trailing tokens if it ever does.
        let raw = "PID %CPU %MEM COMMAND\n1 0.0 0.0 some odd name";
        let rows = parse_top_processes(raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].comm, "some odd name");
    }

    #[test]
    fn parse_top_processes_skips_malformed_lines() {
        let raw = "PID %CPU %MEM COMMAND\n1 2 3 ok\ntoo short\n";
        let rows = parse_top_processes(raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].comm, "ok");
    }

    #[test]
    fn parse_top_processes_empty_body_yields_no_rows() {
        assert!(parse_top_processes("PID %CPU %MEM COMMAND\n").is_empty());
        assert!(parse_top_processes("").is_empty());
    }
}

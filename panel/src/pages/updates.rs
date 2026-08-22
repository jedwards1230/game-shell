//! `/system/updates` — the pacman system-update page.
//!
//! **Not to be confused with the crate-level `crate::updates`**, which owns
//! the actual pacman state (the `checkupdates` cache, the reboot-needed probe
//! and the single-flighted apply job) and is daemon-independent. This module
//! is only the *page*: it renders that state and drives it from HTTP.
//!
//! Split out of the old Processes page (`docs/PANEL_IA.md` phase 2): it has
//! its own cache TTL, its own background job with its own self-terminating
//! poll, and the most dangerous button in the panel — none of which belong on
//! a page about observing processes.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};

use crate::capabilities::Chrome;
use crate::state::{AppState, SharedState};

#[derive(Template)]
#[template(path = "updates.html")]
struct UpdatesTemplate {
    chrome: Chrome,
    updates_check_html: String,
    update_job_html: String,
}

/// `GET /system/updates` — the check section plus the job-status partial,
/// both rendered inline on first load; each then refreshes itself over htmx.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let tmpl = UpdatesTemplate {
        chrome: Chrome::new(&state.caps, "system.updates"),
        updates_check_html: render_updates_check(state, false).await,
        update_job_html: render_update_job(state).await,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

// ---------------------------------------------------------------------------
// The `checkupdates` section (#1)
// ---------------------------------------------------------------------------

struct PendingUpdateView {
    name: String,
    old_version: String,
    new_version: String,
}

#[derive(Template)]
#[template(path = "updates_check.html")]
struct UpdatesCheckTemplate {
    pending: Vec<PendingUpdateView>,
    reboot_needed: bool,
    reboot_unknown: bool,
    error: String,
    checked_ago: String,
    /// `[panel].allow_dangerous` (S5) — gates the "Run full update" button.
    /// `POST /system/updates/apply` (which runs `sudo -n pacman -Syu
    /// --noconfirm` under a NOPASSWD rule) is not registered when false.
    allow_dangerous: bool,
}

async fn render_updates_check(state: &AppState, force: bool) -> String {
    let snap = crate::updates::snapshot(&state.updates, force).await;
    let tmpl = UpdatesCheckTemplate {
        pending: snap
            .pending
            .into_iter()
            .map(|p| PendingUpdateView {
                name: p.name,
                old_version: p.old_version,
                new_version: p.new_version,
            })
            .collect(),
        reboot_needed: matches!(snap.reboot, crate::updates::RebootStatus::Needed),
        reboot_unknown: matches!(snap.reboot, crate::updates::RebootStatus::Unknown),
        error: snap.error.unwrap_or_default(),
        checked_ago: format!("{}s ago", snap.checked_at_secs_ago),
        allow_dangerous: state.cfg.allow_dangerous,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /system/updates/refresh` — forces a fresh `checkupdates` +
/// reboot-needed probe (bypassing the 5-minute cache TTL) and re-renders the
/// whole `#updates-check` section.
pub async fn refresh(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_updates_check(&state, true).await)
}

// ---------------------------------------------------------------------------
// The background apply job
// ---------------------------------------------------------------------------

/// The last non-empty line of a finished, failed job's log tail — e.g.
/// `sudo: a password is required` when the panel's run user lacks the
/// NOPASSWD sudo rule `sudo -n pacman -Syu --noconfirm` needs (see
/// `docs/PANEL.md` § System updates). Shown inline in the failure banner
/// (#1) so the operator sees the actual cause immediately rather than a
/// bare "Update failed" with the real reason hidden behind a click into the
/// log-tail `<details>`. Empty for a successful/still-running/never-run job,
/// or a failed one with no captured output.
fn last_error_line(done: bool, success: bool, log_tail: &[String]) -> String {
    if !done || success {
        return String::new();
    }
    log_tail
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

#[derive(Template)]
#[template(path = "updates_job.html")]
struct UpdateJobTemplate {
    running: bool,
    done: bool,
    success: bool,
    elapsed: u64,
    log_tail_text: String,
    /// The last non-empty log line, shown inline on a failed run (e.g.
    /// `sudo: a password is required`) so the operator sees the actual
    /// cause immediately rather than a bare "Update failed" — never a
    /// generic failure with the real reason hidden behind a click. Empty
    /// when there's nothing useful to show (success, or no output captured).
    last_error_line: String,
    reboot_needed: bool,
}

async fn render_update_job(state: &AppState) -> String {
    let job = crate::updates::job_snapshot(&state.updates).await;
    let (running, done, success, elapsed, log_tail) = match job {
        crate::updates::JobSnapshot::Idle => (false, false, false, 0, Vec::new()),
        crate::updates::JobSnapshot::Running {
            elapsed_secs,
            log_tail,
        } => (true, false, false, elapsed_secs, log_tail),
        crate::updates::JobSnapshot::Done {
            success,
            elapsed_secs,
            log_tail,
        } => (false, true, success, elapsed_secs, log_tail),
    };
    // Only re-probe reboot-needed status right when a job just finished (the
    // job itself invalidates the cache on completion, so this reflects
    // post-update state) — not on every poll, since `data-running` stops
    // further polling once `done` is true (see updates_job.html).
    let reboot_needed = if done {
        let snap = crate::updates::snapshot(&state.updates, false).await;
        matches!(snap.reboot, crate::updates::RebootStatus::Needed)
    } else {
        false
    };
    let last_error_line = last_error_line(done, success, &log_tail);
    let tmpl = UpdateJobTemplate {
        running,
        done,
        success,
        elapsed,
        log_tail_text: log_tail.join("\n"),
        last_error_line,
        reboot_needed,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `GET /system/updates/job` — the self-polling job-status partial
/// (`hx-trigger="every 2s [this.dataset.running=='1']"` — polls only while
/// `Running`, per `updates_job.html`).
pub async fn job(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_update_job(&state).await)
}

/// `POST /system/updates/apply` — starts the background `sudo -n pacman
/// -Syu --noconfirm` job (single-flighted — a second click while one is
/// already running is a no-op, not an error) and immediately renders the
/// job-status partial, which starts polling itself every 2s.
pub async fn apply(State(state): State<SharedState>) -> impl IntoResponse {
    let _ = crate::updates::start_apply(&state).await;
    Html(render_update_job(&state).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_error_line_surfaces_the_real_sudo_failure() {
        let tail = vec!["sudo: a password is required".to_string(), "".to_string()];
        assert_eq!(
            last_error_line(true, false, &tail),
            "sudo: a password is required",
            "must surface the actual sudo error, not a generic failure"
        );
    }

    #[test]
    fn last_error_line_skips_trailing_blank_lines() {
        let tail = vec![
            "error: target not found: bogus-pkg".to_string(),
            "".to_string(),
            "   ".to_string(),
        ];
        assert_eq!(
            last_error_line(true, false, &tail),
            "error: target not found: bogus-pkg"
        );
    }

    #[test]
    fn last_error_line_empty_when_not_a_failed_done_job() {
        assert_eq!(last_error_line(false, false, &["boom".to_string()]), "");
        assert_eq!(last_error_line(true, true, &["ok".to_string()]), "");
        assert_eq!(last_error_line(true, false, &[]), "");
    }
}

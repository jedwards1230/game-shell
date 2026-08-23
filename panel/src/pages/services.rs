//! `/system/services` — the systemd unit table, its per-unit restart, and the
//! arbitrary-unit inspector.
//!
//! Split out of the old Processes page (`docs/PANEL_IA.md` phase 2): unit
//! *control* is a different job from process *observation*, and it is the one
//! surface on the page that mutates anything. Phase 5 (#409) added the
//! read/restart asymmetry this page exists for:
//!
//! * **Read: any unit**, system or user, including one typed into the inspect
//!   form. Reading unit status is inert, so this side is unrestricted — but
//!   the typed name still goes through [`config::UnitName::parse`] before it
//!   reaches a `systemctl` argv, and only ever reaches `systemctl show`.
//! * **Restart: an allowlist only** — the three built-in tv-shell units plus
//!   whatever `[panel].managed_units` names. The client sends a *key*; the
//!   unit name is resolved server-side out of [`config::AppConfig`]. An
//!   arbitrary client-supplied unit name never reaches a mutating `systemctl`
//!   on any path, and cannot: [`crate::exec::Recovery::restart`] takes a
//!   [`config::RestartTarget`], which only the server-side table constructs.
//!
//! Degradation: everything here is exec-based (`systemctl`), so it works with
//! the daemon down. That is the whole point — this is a recovery surface.
//! System-scope restarts additionally need a per-unit NOPASSWD sudoers line
//! (`docs/PANEL.md` § Deployment prerequisite); with none, the restart fails
//! **closed** with an explicit refusal naming the unit, never a silent no-op.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse};
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::config::{self, RestartTarget, UnitScope};
use crate::exec::ExecError;
use crate::state::{AppState, SharedState};

use super::units::{self, unit_dot, UnitStatus};

/// One row of the restartable-unit table.
struct UnitView {
    key: String,
    label: String,
    unit: String,
    scope: &'static str,
    /// A dedicated dot/word status pair (color always paired with explicit
    /// text — #6), from the shared `pages::units::unit_dot` that Overview's
    /// Units tile and the Dev page's chips also render.
    dot_class: &'static str,
    state_word: &'static str,
    /// The raw `ActiveState`, shown only when it differs from `state_word`.
    state: String,
    /// `enabled` / `disabled` / `static` / … or empty.
    enabled_state: String,
    active_since: String,
    failure_reason: String,
    /// Danger tier for this row's Restart button (`docs/PANEL.md` § Danger
    /// tiers), derived from scope rather than from which table the unit is
    /// in: a system-scope restart is elevated and can take a service the
    /// whole box depends on with it (`.danger-severe`); a `--user` restart is
    /// disruptive but recoverable and is itself the recovery path
    /// (`.warn-action`).
    danger_class: &'static str,
    /// Confirm-dialog text for this unit's Restart button. Three flavors, all
    /// naming the specific unit: the panel's own unit warns that the click
    /// drops the page being looked at, a remote-access-critical unit warns
    /// that a failed restart can end remote access entirely, and everything
    /// else gets the plain wording.
    confirm: String,
}

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesTemplate {
    chrome: Chrome,
    builtin: Vec<UnitView>,
    managed: Vec<UnitView>,
}

/// `GET /system/services`.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let mut builtin = Vec::new();
    for key in config::BUILT_IN_UNIT_KEYS {
        let target = config::builtin_target(key).expect("built-in unit key");
        let status = read_status(state, target.scope(), target.unit()).await;
        builtin.push(unit_view(&status, &target, builtin_label(key)));
    }

    let mut managed = Vec::new();
    for target in &state.cfg.managed_units {
        let label = target.key().to_string();
        let status = read_status(state, target.scope(), target.unit()).await;
        managed.push(unit_view(&status, target, label));
    }

    let tmpl = ServicesTemplate {
        chrome: Chrome::new(&state.caps, "system.services"),
        builtin,
        managed,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn builtin_label(key: &str) -> String {
    match key {
        "daemon" => "Daemon",
        "shell" => "Shell",
        "panel" => "Panel",
        other => other,
    }
    .to_string()
}

/// Build one restartable unit's row from an already-read status.
///
/// **Pure, and deliberately so.** This is the join between `units::parse_show`
/// (which has its own tests) and the rendered row — the part that decides what
/// an operator staring at a broken box actually reads. While it did its own
/// `read_status`, no test could reach it with real data: CI has no `systemctl`,
/// so every row rendered through the `Err(_) => UnitStatus::default()` branch
/// and came out "unknown". Deleting `failure_reason` or `state` from this
/// struct left the whole suite green.
///
/// Taking the status as an argument lets a plain test pin active / failed /
/// not-found rows without a systemd on the machine running it.
fn unit_view(status: &UnitStatus, target: &RestartTarget, label: String) -> UnitView {
    let (dot_class, state_word) = unit_dot(&status.active_state);
    UnitView {
        key: target.key().to_string(),
        label,
        unit: target.unit().to_string(),
        scope: target.scope().as_str(),
        dot_class,
        state_word,
        state: status.active_state.clone(),
        enabled_state: status.unit_file_state.clone(),
        active_since: status.active_since.clone(),
        failure_reason: status.failure_reason(),
        danger_class: match target.scope() {
            UnitScope::System => "danger-severe",
            UnitScope::User => "warn-action",
        },
        confirm: confirm_text(target),
    }
}

/// The confirm a Restart button carries. `.danger-severe` buttons all name the
/// specific unit; two cases say more.
fn confirm_text(target: &RestartTarget) -> String {
    let unit = target.unit();
    if target.key() == "panel" {
        return format!(
            "Restart {unit} now? This is the panel serving THIS page — it will disconnect \
             immediately. Reload the page after a few seconds to reconnect."
        );
    }
    if target.is_remote_access_critical() {
        return format!(
            "Restart {unit} now? This unit is how you reach this box remotely — if it does \
             not come back, a failed restart may end remote access entirely and the box will \
             need physical attention."
        );
    }
    format!("Restart {unit} now?")
}

/// `systemctl show` for one unit, degraded to an empty status on any exec
/// failure — a status probe must never take the page down.
async fn read_status(
    state: &AppState,
    scope: UnitScope,
    unit: &config::UnitName,
) -> units::UnitStatus {
    match state.recovery.show_unit(scope, unit).await {
        Ok(raw) => units::parse_show(&raw),
        Err(_) => UnitStatus::default(),
    }
}

// ── restart (allowlist only) ───────────────────────────────────────────────

#[derive(Template)]
#[template(path = "services_result.html")]
struct ServicesResultTemplate {
    ok: bool,
    message: String,
}

fn result_html(ok: bool, message: &str) -> String {
    let tmpl = ServicesResultTemplate {
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /system/services/restart/{key}` — restart one allowlisted unit.
///
/// `key` is resolved against the server-side table
/// ([`config::AppConfig::restart_target`]: the three built-ins, then
/// `[panel].managed_units`) and an unknown key is refused **before any exec**.
/// The resolved [`RestartTarget`] is the only thing
/// [`crate::exec::Recovery::restart`] accepts, so this route cannot be made to
/// restart a unit the operator did not put in the table —
/// `docs/PANEL_IA.md` § "Preserving the no-arbitrary-unit property".
pub async fn restart(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    Html(render_restart(&state, &key).await)
}

pub async fn render_restart(state: &AppState, key: &str) -> String {
    let Some(target) = state.cfg.restart_target(key) else {
        return result_html(
            false,
            &format!(
                "unknown unit key {key:?} — nothing was run. Only the built-in tv-shell units \
                 ({}) and the units named in [panel].managed_units can be restarted from here.",
                config::BUILT_IN_UNIT_KEYS.join(", ")
            ),
        );
    };
    let outcome = state.recovery.restart(&target).await;
    restart_result_html(&target, outcome)
}

/// Render a restart outcome. Pure, so the fail-closed wording is testable
/// without a `sudo` on the machine running the suite.
fn restart_result_html(target: &RestartTarget, outcome: Result<String, ExecError>) -> String {
    let unit = target.unit();
    match outcome {
        Ok(out) => result_html(true, &format!("restarted {unit}\n{out}")),
        // The one failure that is NOT "the restart was attempted and went
        // wrong": nothing ran at all, because this node does not grant the
        // panel permission to run it. Saying so — and saying what is missing —
        // is the difference between a five-minute fix and an hour in the
        // journal looking for a restart that never happened.
        Err(ExecError::NotPermitted(detail)) => result_html(
            false,
            &format!(
                "NOT PERMITTED on this node: {unit} was not restarted, and nothing was run.\n\n\
                 {unit} is a system-scope unit, so the panel restarts it through\n  \
                 sudo -n systemctl restart {unit}\nand sudo refused:\n  {detail}\n\n\
                 Missing prerequisite: a per-unit NOPASSWD sudoers line for this exact \
                 command, e.g.\n  \
                 <panel-user> ALL=(root) NOPASSWD: /usr/bin/systemctl restart {unit}\n\n\
                 Those lines are generated by the htpc_common role in \
                 jedwards1230/homelab-ansible from the same list that renders \
                 [panel].managed_units. That generation has NOT landed yet, so every \
                 system-scope restart fails closed here today. User-scope units \
                 (systemctl --user) need no rule and are unaffected."
            ),
        ),
        Err(e) => result_html(false, &format!("restart {unit} failed: {e}")),
    }
}

// ── inspect (any unit, read-only) ──────────────────────────────────────────

/// Query for `GET /system/services/inspect`. Both fields default, so a bare
/// request renders the empty prompt rather than a 400.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct InspectQuery {
    unit: String,
    scope: String,
}

#[derive(Template)]
#[template(path = "services_inspect.html")]
struct InspectTemplate {
    /// Set when the input was rejected or the read failed; the rest is empty.
    error: String,
    /// The name and scope parsed cleanly, so this fragment is about a real
    /// unit even if reading it failed.
    ///
    /// It separates two things that both set `error` but deserve different
    /// answers: a REJECTED input (not a unit name, not a scope) is about the
    /// typing and gets a bare banner, while a unit that parsed but could not
    /// be read still has an allowlist answer — `systemctl` being unreachable
    /// says nothing about whether `[panel].managed_units` names it. Dropping
    /// that made the fragment least informative exactly when the exec tier was
    /// the thing in trouble.
    resolved: bool,
    unit: String,
    scope: String,
    found: bool,
    id: String,
    description: String,
    load_state: String,
    dot_class: &'static str,
    state_word: &'static str,
    active_state: String,
    sub_state: String,
    enabled_state: String,
    active_since: String,
    failure_reason: String,
    /// Whether this unit is also in the restart table — and under which key,
    /// so the fragment can say "restart it above" instead of implying the
    /// operator can restart anything they can read.
    restart_key: String,
}

/// `GET /system/services/inspect?unit=<name>&scope=<system|user>` — read one
/// arbitrary unit.
///
/// A **read**, hence a GET: it runs `systemctl show` and nothing else. The
/// operator-supplied name is validated by [`config::UnitName::parse`] before
/// it exists as anything a `systemctl` argv could take, and the validated
/// value is the one that reaches the exec.
pub async fn inspect(
    State(state): State<SharedState>,
    Query(q): Query<InspectQuery>,
) -> impl IntoResponse {
    Html(render_inspect(&state, &q.unit, &q.scope).await)
}

pub async fn render_inspect(state: &AppState, unit: &str, scope: &str) -> String {
    let tmpl = build_inspect(state, unit, scope).await;
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

async fn build_inspect(state: &AppState, unit: &str, scope: &str) -> InspectTemplate {
    let mut tmpl = InspectTemplate {
        error: String::new(),
        resolved: false,
        unit: unit.trim().to_string(),
        scope: scope.trim().to_string(),
        found: false,
        id: String::new(),
        description: String::new(),
        load_state: String::new(),
        dot_class: "dot-neutral",
        state_word: "unknown",
        active_state: String::new(),
        sub_state: String::new(),
        enabled_state: String::new(),
        active_since: String::new(),
        failure_reason: String::new(),
        restart_key: String::new(),
    };

    if unit.trim().is_empty() {
        tmpl.error = "Enter a unit name, e.g. sshd.service.".to_string();
        return tmpl;
    }
    let name = match config::UnitName::parse(unit) {
        Ok(n) => n,
        Err(e) => {
            tmpl.error = format!("Not a unit name: {e}");
            return tmpl;
        }
    };
    let scope = match config::UnitScope::parse(scope) {
        Ok(s) => s,
        Err(e) => {
            tmpl.error = format!("Not a scope: {e}");
            return tmpl;
        }
    };
    tmpl.unit = name.to_string();
    tmpl.scope = scope.as_str().to_string();
    tmpl.resolved = true;
    // The heading falls back to what was asked for; a successful read replaces
    // it with systemd's canonical `Id` below.
    tmpl.id = name.to_string();
    // Resolved from config BEFORE the read: whether a unit is restartable is a
    // `[panel].managed_units` fact, not a systemd one, so it survives a read
    // that fails.
    tmpl.restart_key = state
        .cfg
        .restart_targets()
        .into_iter()
        .find(|t| t.unit() == &name && t.scope() == scope)
        .map(|t| t.key().to_string())
        .unwrap_or_default();

    let status = match state.recovery.show_unit(scope, &name).await {
        Ok(raw) => units::parse_show(&raw),
        Err(e) => {
            tmpl.error = format!("could not read {name}: {e}");
            return tmpl;
        }
    };

    let (dot_class, state_word) = unit_dot(&status.active_state);
    tmpl.found = status.found();
    tmpl.id = if status.id.is_empty() {
        name.to_string()
    } else {
        status.id.clone()
    };
    tmpl.description = status.description.clone();
    tmpl.load_state = status.load_state.clone();
    tmpl.dot_class = dot_class;
    tmpl.state_word = state_word;
    tmpl.active_state = status.active_state.clone();
    tmpl.sub_state = status.sub_state.clone();
    tmpl.enabled_state = status.unit_file_state.clone();
    tmpl.active_since = status.active_since.clone();
    tmpl.failure_reason = status.failure_reason();
    tmpl
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(key: &str, unit: &str, scope: &str) -> RestartTarget {
        let raw = [config::RawManagedUnit {
            key: key.to_string(),
            unit: unit.to_string(),
            scope: scope.to_string(),
        }];
        config::resolve_managed_units(&raw)
            .expect("well-formed test entry")
            .remove(0)
    }

    /// A row for a unit that is up.
    ///
    /// Before `unit_view` became pure, nothing in the suite could reach it with
    /// a real status: CI has no `systemctl`, so every row went through
    /// `read_status`'s `Err(_) => UnitStatus::default()` branch and rendered
    /// "unknown". `units::parse_show` was tested, the rendered row was tested,
    /// and the join between them — which is what an operator reads — was not.
    #[test]
    fn a_running_unit_renders_its_real_state_not_unknown() {
        let status = units::parse_show(
            "Id=sshd.service\n\
             LoadState=loaded\n\
             ActiveState=active\n\
             SubState=running\n\
             UnitFileState=enabled\n\
             ActiveEnterTimestamp=Fri 2026-08-22 18:03:56 EDT\n",
        );
        let view = unit_view(
            &status,
            &managed("sshd", "sshd.service", "system"),
            "sshd".into(),
        );

        assert_eq!(view.state, "active");
        assert_eq!(view.enabled_state, "enabled");
        assert_eq!(view.active_since, "Fri 2026-08-22 18:03:56 EDT");
        assert!(
            view.failure_reason.is_empty(),
            "a healthy unit must not claim a failure reason, got {:?}",
            view.failure_reason
        );
        assert_eq!(view.state_word, "active");
        assert_ne!(
            view.state_word, "unknown",
            "this is the assertion the old shape could not make"
        );
    }

    /// The row that matters most: a failed unit must carry WHY it failed.
    ///
    /// This is the field an operator is actually looking for on a broken box,
    /// and it was the easiest thing in the file to delete without any test
    /// noticing.
    #[test]
    fn a_failed_unit_carries_its_failure_reason_into_the_row() {
        let status = units::parse_show(
            "Id=tv-shell-panel.service\n\
             LoadState=loaded\n\
             ActiveState=failed\n\
             SubState=failed\n\
             UnitFileState=enabled\n\
             Result=exit-code\n\
             ExecMainStatus=255\n",
        );
        let view = unit_view(
            &status,
            &config::builtin_target("panel").expect("built-in"),
            "Panel".into(),
        );

        assert_eq!(view.state, "failed");
        assert!(
            !view.failure_reason.is_empty(),
            "a failed unit must explain itself"
        );
        assert!(
            view.failure_reason.contains("255"),
            "the exit status is the diagnostic; got {:?}",
            view.failure_reason
        );
    }

    /// A unit systemd does not know about must not read as healthy.
    #[test]
    fn a_not_found_unit_does_not_render_as_active() {
        let status = units::parse_show(
            "Id=nope.service\nLoadState=not-found\nActiveState=inactive\nSubState=dead\n",
        );
        let view = unit_view(
            &status,
            &managed("nope", "nope.service", "system"),
            "nope".into(),
        );

        assert_ne!(view.state_word, "active");
        assert_eq!(view.state, "inactive");
    }

    /// The danger tier and confirm are scope-derived, and a pure `unit_view`
    /// finally lets that be asserted on the row itself rather than inferred.
    #[test]
    fn scope_drives_the_rows_danger_tier() {
        let status = UnitStatus::default();

        let system = unit_view(
            &status,
            &managed("net", "NetworkManager.service", "system"),
            "net".into(),
        );
        assert_eq!(system.danger_class, "danger-severe");
        assert_eq!(system.scope, "system");

        let user = unit_view(
            &status,
            &managed("thing", "thing.service", "user"),
            "thing".into(),
        );
        assert_eq!(user.danger_class, "warn-action");
        assert_eq!(user.scope, "user");
    }

    /// The behaviour htpc-1 shows today. Not "failed", not "ok" — an explicit
    /// refusal that names the unit and the missing prerequisite.
    #[test]
    fn a_missing_sudoers_line_renders_an_explicit_refusal_naming_the_unit() {
        let target = managed("sshd", "sshd.service", "system");
        let html = restart_result_html(
            &target,
            Err(ExecError::NotPermitted(
                "sudo: a password is required".to_string(),
            )),
        );
        assert!(html.contains("result-error"), "it is a failure: {html}");
        assert!(html.contains("NOT PERMITTED"), "{html}");
        assert!(html.contains("sshd.service"), "name the unit: {html}");
        assert!(html.contains("nothing was run"), "{html}");
        assert!(
            html.contains("sudoers") && html.contains("NOPASSWD"),
            "say what is missing: {html}"
        );
        assert!(
            html.contains("a password is required"),
            "keep sudo's own words: {html}"
        );
    }

    #[test]
    fn a_genuine_restart_failure_is_not_dressed_up_as_a_permission_problem() {
        let target = managed("sshd", "sshd.service", "system");
        let html = restart_result_html(
            &target,
            Err(ExecError::NonZero(1, "Job for sshd.service failed".into())),
        );
        assert!(!html.contains("NOT PERMITTED"), "{html}");
        assert!(html.contains("restart sshd.service failed"), "{html}");
    }

    #[test]
    fn the_confirm_names_the_unit_and_escalates_for_remote_access() {
        let sshd = managed("sshd", "sshd.service", "system");
        let confirm = confirm_text(&sshd);
        assert!(confirm.contains("sshd.service"), "{confirm}");
        assert!(confirm.contains("end remote access entirely"), "{confirm}");

        let bt = managed("bluetooth", "bluetooth.service", "system");
        let confirm = confirm_text(&bt);
        assert!(confirm.contains("bluetooth.service"), "{confirm}");
        assert!(!confirm.contains("remote access"), "{confirm}");

        let panel = config::builtin_target("panel").unwrap();
        assert!(confirm_text(&panel).contains("THIS page"));
    }
}

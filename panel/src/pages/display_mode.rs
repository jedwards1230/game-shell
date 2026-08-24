//! `/devices/display-audio/mode` — the resolution, refresh-rate and VRR
//! controls on Devices ▸ Display & Audio.
//!
//! **Not a page.** It is the one section of [`super::display_audio`] that does
//! not live in `settings.json`: resolution, refresh rate and VRR are Hyprland
//! *compositor* state, so they have their own read/write path
//! (`hypr-display-state` / `hypr-set-mode` / `hypr-set-vrr` /
//! `hypr-display-confirm` / `hypr-display-revert`) instead of going through
//! [`super::settings`]'s `SettingField` pipeline. Adding them to `SCHEMA`
//! would render three controls that write keys nothing reads.
//!
//! ## Why every route re-renders the whole section
//!
//! The other IPC pages swap a small `#…-result` partial and leave the form
//! alone. That is wrong here: applying a mode changes what the controls should
//! show (the selected option, the pending banner, the countdown), and a stale
//! form beside a fresh result is how someone confirms a change they can no
//! longer see. So every handler returns the same rendered section, notice
//! included, and htmx swaps the lot.
//!
//! ## Gating
//!
//! [`Gate::Node`](crate::capabilities::Gate::Node), like the two power probes
//! on the same page — these are IPC commands that map to no declared
//! `Feature`. Deliberately **not** behind `allow_dangerous`: the shell host
//! does not set it, so a dangerous-gated control would be invisible on the one
//! device this exists for. The safety story is the confirm-or-revert timer in
//! the daemon, not a config flag that hides the button.
//!
//! Degradation: an unreachable daemon renders the section as an inline banner,
//! never a 500.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;

use crate::pages::ipc_console::validate_token;
use crate::state::{AppState, SharedState};

// ---------------------------------------------------------------------------
// The `hypr-display-state` reply (docs/IPC_PROTOCOL.md § hypr-display-state)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisplayStateJson {
    #[serde(default)]
    displays: Vec<DisplayJson>,
    #[serde(default)]
    pending: Option<PendingJson>,
    #[serde(default)]
    revert_seconds: u64,
    #[serde(default)]
    config_path: String,
    #[serde(default)]
    config_present: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisplayJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    current_label: Option<String>,
    #[serde(default)]
    current_format: String,
    #[serde(default)]
    hdr: bool,
    #[serde(default)]
    vrr_active: bool,
    #[serde(default)]
    configured_line: Option<String>,
    #[serde(default)]
    configured_vrr: Option<u8>,
    #[serde(default)]
    modes: Vec<ModeJson>,
}

#[derive(Deserialize)]
struct ModeJson {
    value: String,
    label: String,
    #[serde(default)]
    current: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingJson {
    #[serde(default)]
    monitor: String,
    #[serde(default)]
    applied: String,
    #[serde(default)]
    previous: String,
    #[serde(default)]
    seconds_remaining: u64,
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

struct ModeView {
    value: String,
    label: String,
    current: bool,
}

struct DisplayView {
    name: String,
    description: String,
    current_label: String,
    current_format: String,
    hdr: bool,
    /// What Hyprland reports as active right now. Not the same question as
    /// [`Self::vrr_selected`]: mode 2 (fullscreen only) reads back inactive
    /// whenever nothing is fullscreen, which is not a disagreement.
    vrr_active: bool,
    /// The radio button to pre-select: the configured `vrr` argument when the
    /// output has one, otherwise inferred from the live reading.
    vrr_selected: u8,
    /// Whether [`Self::vrr_selected`] came from the config rather than being
    /// inferred — the UI says which, because an inferred value is a guess.
    vrr_from_config: bool,
    configured_line: String,
    has_config_line: bool,
    modes: Vec<ModeView>,
    /// An output that reports no parseable modes: render the reason, not an
    /// empty `<select>` that looks broken.
    no_modes: bool,
}

#[derive(Template)]
#[template(path = "display_mode.html")]
struct DisplayModeTemplate {
    /// Whether the daemon answered at all.
    available: bool,
    /// Why not, when it did not.
    error: String,
    notice_shown: bool,
    notice_ok: bool,
    notice_text: String,
    pending: bool,
    pending_monitor: String,
    pending_applied: String,
    pending_previous: String,
    pending_seconds: u64,
    revert_seconds: u64,
    config_path: String,
    config_present: bool,
    displays: Vec<DisplayView>,
}

impl DisplayModeTemplate {
    fn unavailable(error: String, notice: Option<(bool, String)>) -> Self {
        let (notice_ok, notice_text) = notice.clone().unwrap_or((false, String::new()));
        Self {
            available: false,
            error,
            notice_shown: notice.is_some(),
            notice_ok,
            notice_text,
            pending: false,
            pending_monitor: String::new(),
            pending_applied: String::new(),
            pending_previous: String::new(),
            pending_seconds: 0,
            revert_seconds: 0,
            config_path: String::new(),
            config_present: false,
            displays: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /devices/display-audio/mode` — the section itself. The page includes
/// it with `hx-trigger="load"`, and the pending banner re-requests it once a
/// second so a daemon-side auto-revert shows up without anyone clicking.
pub async fn section(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_section(&state, None).await)
}

#[derive(Deserialize)]
pub struct ApplyForm {
    pub monitor: String,
    pub mode: String,
}

/// `POST /devices/display-audio/mode/apply` — `hypr-set-mode <name> <mode>`.
pub async fn apply(
    State(state): State<SharedState>,
    Form(form): Form<ApplyForm>,
) -> impl IntoResponse {
    Html(render_apply(&state, &form.monitor, &form.mode).await)
}

pub async fn render_apply(state: &AppState, monitor: &str, mode: &str) -> String {
    let notice = match (validate_token(monitor), validate_token(mode)) {
        (Ok(m), Ok(v)) => {
            command_notice(
                state,
                &format!("hypr-set-mode {m} {v}"),
                &format!("Applied {v} to {m}."),
            )
            .await
        }
        (Err(e), _) | (_, Err(e)) => (false, e),
    };
    render_section(state, Some(notice)).await
}

#[derive(Deserialize)]
pub struct VrrForm {
    pub monitor: String,
    pub vrr: String,
}

/// `POST /devices/display-audio/mode/vrr` — `hypr-set-vrr <name> <0|1|2>`.
pub async fn vrr(State(state): State<SharedState>, Form(form): Form<VrrForm>) -> impl IntoResponse {
    Html(render_vrr(&state, &form.monitor, &form.vrr).await)
}

pub async fn render_vrr(state: &AppState, monitor: &str, vrr: &str) -> String {
    // The closed set is re-checked here rather than trusted from the radio
    // group: a form post is not a UI.
    let notice = match (validate_token(monitor), vrr.trim().parse::<u8>()) {
        (Ok(m), Ok(v)) if v <= 2 => {
            command_notice(
                state,
                &format!("hypr-set-vrr {m} {v}"),
                &format!("Set VRR to {} on {m}.", vrr_label(v)),
            )
            .await
        }
        (Ok(_), _) => (
            false,
            format!("invalid VRR mode {vrr:?} — expected 0, 1 or 2"),
        ),
        (Err(e), _) => (false, e),
    };
    render_section(state, Some(notice)).await
}

/// `POST /devices/display-audio/mode/confirm` — `hypr-display-confirm`. This
/// is the only path that writes `hyprland-local.conf`.
pub async fn confirm(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_confirm(&state).await)
}

pub async fn render_confirm(state: &AppState) -> String {
    let notice = match state.node.command("hypr-display-confirm").await {
        Ok(reply) => {
            let v: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
            let persisted = v.get("persisted").and_then(serde_json::Value::as_bool);
            let path = v
                .get("configPath")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the local Hyprland config");
            match persisted {
                Some(true) => (true, format!("Kept, and written to {path}.")),
                // Confirming still kept the mode — it just did not reach disk,
                // so it is gone on the next compositor restart. Say exactly
                // that rather than reporting a flat success or a flat failure.
                _ => (
                    false,
                    format!(
                        "Kept for this session, but could NOT be written to {path}: {}. \
                         It will revert when Hyprland restarts.",
                        v.get("persistError")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown error")
                    ),
                ),
            }
        }
        Err(e) => (false, e.to_string()),
    };
    render_section(state, Some(notice)).await
}

/// `POST /devices/display-audio/mode/revert` — `hypr-display-revert`.
pub async fn revert(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_revert(&state).await)
}

pub async fn render_revert(state: &AppState) -> String {
    let notice = command_notice(
        state,
        "hypr-display-revert",
        "Reverted to the previous display settings.",
    )
    .await;
    render_section(state, Some(notice)).await
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// `0` / `1` / `2` as the words the UI uses, so the notice and the radio
/// labels cannot drift apart.
pub fn vrr_label(vrr: u8) -> &'static str {
    match vrr {
        0 => "off",
        1 => "on",
        _ => "fullscreen only",
    }
}

/// Send one command and turn the outcome into the section's notice line.
async fn command_notice(state: &AppState, line: &str, ok_text: &str) -> (bool, String) {
    match state.node.command(line).await {
        Ok(_) => (true, ok_text.to_string()),
        Err(e) => (false, e.to_string()),
    }
}

pub async fn render_section(state: &AppState, notice: Option<(bool, String)>) -> String {
    let state_json = match state.node.command("hypr-display-state").await {
        Ok(reply) => match serde_json::from_str::<DisplayStateJson>(&reply) {
            Ok(v) => v,
            Err(e) => {
                return render(DisplayModeTemplate::unavailable(
                    format!("could not parse the daemon's display state: {e}"),
                    notice,
                ))
            }
        },
        Err(e) => return render(DisplayModeTemplate::unavailable(e.to_string(), notice)),
    };

    let displays = state_json
        .displays
        .into_iter()
        .map(|d| {
            let configured_vrr = d.configured_vrr;
            DisplayView {
                name: d.name,
                description: d.description,
                current_label: d.current_label.unwrap_or_else(|| "unknown".to_string()),
                current_format: d.current_format,
                hdr: d.hdr,
                vrr_active: d.vrr_active,
                vrr_selected: configured_vrr.unwrap_or(u8::from(d.vrr_active)),
                vrr_from_config: configured_vrr.is_some(),
                has_config_line: d.configured_line.is_some(),
                configured_line: d.configured_line.unwrap_or_default(),
                no_modes: d.modes.is_empty(),
                modes: d
                    .modes
                    .into_iter()
                    .map(|m| ModeView {
                        value: m.value,
                        label: m.label,
                        current: m.current,
                    })
                    .collect(),
            }
        })
        .collect();

    let (notice_ok, notice_text) = notice.clone().unwrap_or((false, String::new()));
    let pending = state_json.pending;
    render(DisplayModeTemplate {
        available: true,
        error: String::new(),
        notice_shown: notice.is_some(),
        notice_ok,
        notice_text,
        pending: pending.is_some(),
        pending_monitor: pending
            .as_ref()
            .map(|p| p.monitor.clone())
            .unwrap_or_default(),
        pending_applied: pending
            .as_ref()
            .map(|p| p.applied.clone())
            .unwrap_or_default(),
        pending_previous: pending
            .as_ref()
            .map(|p| p.previous.clone())
            .unwrap_or_default(),
        pending_seconds: pending.as_ref().map(|p| p.seconds_remaining).unwrap_or(0),
        revert_seconds: state_json.revert_seconds,
        config_path: state_json.config_path,
        config_present: state_json.config_present,
        displays,
    })
}

fn render(tmpl: DisplayModeTemplate) -> String {
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

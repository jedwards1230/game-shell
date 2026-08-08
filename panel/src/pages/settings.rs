//! `/settings` — typed forms over `settings.json` via the daemon's
//! `get-config`/`set-config` IPC commands (shallow merge; the daemon remains
//! the sole writer of `settings.json`), plus a read-only view of the
//! daemon-owned binding keys, a read-only `config.toml` viewer, and a raw
//! JSON escape hatch for keys this page doesn't model as typed fields.
//!
//! Degradation: when the daemon is unreachable, `GET /settings` still
//! returns 200 with a clear "unreachable" banner and no forms (mirrors
//! `pages::dev`'s up/down banner) — never a 500.

use std::collections::HashMap;

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;
use serde_json::Value;

use crate::capabilities::{CapabilitySnapshot, Chrome};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

// ---------------------------------------------------------------------------
// Settings schema — mirrors the QML-owned keys SettingsStore.qml persists.
// ---------------------------------------------------------------------------
//
// KEEP IN SYNC with shell/components/SettingsStore.qml's `_schema` table.
//
// This table drives BOTH the typed-form rendering (`build_groups`) and the
// save-patch parser (`build_patch`), so a checkbox left unchecked always maps
// to an explicit `false` rather than being silently omitted.

/// How a schema field's value is typed and validated.
#[derive(Clone, Copy, Debug)]
pub enum FieldKind {
    Bool,
    /// A closed set of allowed string values (rendered as a `<select>`).
    Enum(&'static [&'static str]),
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float,
    Str,
    /// A JSON array of strings, rendered as a textarea with one entry per
    /// line. Saving parses trimmed non-empty lines back into the array, so an
    /// emptied textarea clears the list to `[]`.
    StrList,
    /// An object-valued key that doesn't fit a simple form field (e.g. a
    /// nested map). Never rendered as a typed input and never emitted in the
    /// typed save patch — editable only via the raw JSON escape hatch.
    Complex,
}

pub struct SettingField {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub kind: FieldKind,
    pub default: &'static str,
}

/// QML-owned settings keys (25 of them, matching `SettingsStore.qml`'s
/// `_schema`, minus the daemon-owned `keyBindings` and `webApps` mirrors it
/// also carries — see [`DAEMON_OWNED_KEYS`] for those and their siblings).
pub const SCHEMA: &[SettingField] = &[
    SettingField {
        key: "themeMode",
        label: "Theme mode",
        group: "Appearance",
        kind: FieldKind::Enum(&["auto", "light", "dark"]),
        default: "dark",
    },
    SettingField {
        key: "autoThemeDarkStart",
        label: "Auto-theme: dark start hour",
        group: "Appearance",
        kind: FieldKind::Int {
            min: Some(0),
            max: Some(23),
        },
        default: "20",
    },
    SettingField {
        key: "autoThemeLightStart",
        label: "Auto-theme: light start hour",
        group: "Appearance",
        kind: FieldKind::Int {
            min: Some(0),
            max: Some(23),
        },
        default: "7",
    },
    SettingField {
        key: "reduceMotion",
        label: "Reduce motion",
        group: "Appearance",
        kind: FieldKind::Bool,
        default: "false",
    },
    SettingField {
        key: "textScale",
        label: "Text scale",
        group: "Appearance",
        kind: FieldKind::Float,
        default: "1.0",
    },
    SettingField {
        key: "controllerDebug",
        label: "Controller debug overlay",
        group: "Input",
        kind: FieldKind::Bool,
        default: "false",
    },
    SettingField {
        key: "rumbleEnabled",
        label: "Rumble enabled",
        group: "Input",
        kind: FieldKind::Bool,
        default: "true",
    },
    SettingField {
        key: "widgets",
        label: "Widgets config",
        group: "Widgets",
        kind: FieldKind::Complex,
        default: "{}",
    },
    SettingField {
        key: "hdrEnabled",
        label: "HDR enabled",
        group: "Display",
        kind: FieldKind::Bool,
        default: "true",
    },
    SettingField {
        key: "overscan",
        label: "Overscan percent",
        group: "Display",
        kind: FieldKind::Int {
            min: Some(0),
            max: Some(10),
        },
        default: "0",
    },
    SettingField {
        key: "autoDimEnabled",
        label: "Auto-dim enabled",
        group: "Display",
        kind: FieldKind::Bool,
        default: "false",
    },
    SettingField {
        key: "autoDimDelayMinutes",
        label: "Auto-dim delay (minutes)",
        group: "Display",
        kind: FieldKind::Int {
            min: Some(0),
            max: None,
        },
        default: "2",
    },
    SettingField {
        key: "wallpaperPath",
        label: "Wallpaper image path (empty = none)",
        group: "Display",
        kind: FieldKind::Str,
        default: "",
    },
    SettingField {
        key: "nightLightEnabled",
        label: "Night light enabled",
        group: "Night Light",
        kind: FieldKind::Bool,
        default: "false",
    },
    SettingField {
        key: "nightLightTemp",
        label: "Night light temperature (K)",
        group: "Night Light",
        kind: FieldKind::Int {
            min: Some(1000),
            max: Some(10000),
        },
        default: "4500",
    },
    SettingField {
        key: "sleepTimerMinutes",
        label: "Sleep timer (minutes, 0 = off)",
        group: "Power",
        kind: FieldKind::Int {
            min: Some(0),
            max: None,
        },
        default: "0",
    },
    SettingField {
        key: "wakeOnController",
        label: "Wake on controller input",
        group: "Power",
        kind: FieldKind::Bool,
        default: "true",
    },
    SettingField {
        key: "defaultSink",
        label: "Default audio sink",
        group: "Audio",
        kind: FieldKind::Str,
        default: "",
    },
    SettingField {
        key: "audioCardProfile",
        label: "Audio card profile (\"card|profile\")",
        group: "Audio",
        kind: FieldKind::Str,
        default: "",
    },
    SettingField {
        key: "cecFocusOnStartup",
        label: "CEC: claim active source on startup",
        group: "CEC",
        kind: FieldKind::Bool,
        default: "false",
    },
    SettingField {
        key: "cecFocusOnWake",
        label: "CEC: claim active source on wake",
        group: "CEC",
        kind: FieldKind::Bool,
        default: "true",
    },
    SettingField {
        key: "cecAutoSwitchOnPowerOn",
        label: "CEC: auto-switch input on device power-on",
        group: "CEC",
        kind: FieldKind::Bool,
        default: "false",
    },
    SettingField {
        key: "cecDefaultInput",
        label: "CEC: default input logical address (-1 = unset)",
        group: "CEC",
        kind: FieldKind::Int {
            min: Some(-1),
            max: Some(15),
        },
        default: "-1",
    },
    SettingField {
        key: "cecDeviceNames",
        label: "CEC device name overrides",
        group: "CEC",
        kind: FieldKind::Complex,
        default: "{}",
    },
    SettingField {
        key: "prewarmApps",
        label: "Prewarm apps at login (StartupWMClass, one per line)",
        group: "Apps",
        kind: FieldKind::StrList,
        default: "",
    },
];

/// Daemon-owned keys: `keyBindings` is written solely by the daemon;
/// `perGameBindings`/`perPlayerBindings` are the per-game/per-player override
/// layers documented in `docs/IPC_PROTOCOL.md` (`daemon/src/config.rs`); and
/// `webApps` is the web-app registry (`docs/WEB_APPS.md`) — daemon-owned from
/// P0, with the registry IPC arriving in P1. Rendered read-only here — the
/// Controllers page owns resolved bindings, and web-app management follows the
/// registry IPC — and NEVER emitted in a typed or raw save patch this page
/// constructs itself (the raw JSON escape hatch can still touch them if an
/// operator explicitly types them in, same as any other key).
const DAEMON_OWNED_KEYS: &[&str] = &[
    "keyBindings",
    "perGameBindings",
    "perPlayerBindings",
    "webApps",
];

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

struct FieldView {
    key: &'static str,
    label: &'static str,
    input_html: String,
}

struct GroupView {
    name: &'static str,
    fields: Vec<FieldView>,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    chrome: Chrome,
    daemon_up: bool,
    groups: Vec<GroupView>,
    complex_notes_html: String,
    daemon_owned_json: String,
    config_toml: String,
    config_toml_path: String,
    raw_json: String,
}

#[derive(Template)]
#[template(path = "settings_result.html")]
struct SettingsResultTemplate {
    ok: bool,
    message: String,
}

fn result_html(ok: bool, message: &str) -> String {
    let tmpl = SettingsResultTemplate {
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

// ---------------------------------------------------------------------------
// GET /settings
// ---------------------------------------------------------------------------

/// `GET /settings` — fetches the current settings document via `get-config`
/// and renders grouped typed forms, or a degraded banner (still HTTP 200)
/// when the daemon is unreachable.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    match state.node.get_config().await {
        Ok(cfg) => render_ok(&state.caps, &cfg),
        Err(_e) => render_degraded(&state.caps),
    }
}

fn render_ok(caps: &CapabilitySnapshot, cfg: &Value) -> String {
    let chrome = Chrome::new(caps, "settings");
    let (config_toml, config_toml_path) = read_config_toml();
    let tmpl = SettingsTemplate {
        chrome,
        daemon_up: true,
        groups: build_groups(cfg),
        complex_notes_html: complex_notes_html(),
        daemon_owned_json: daemon_owned_json(cfg),
        config_toml,
        config_toml_path,
        // Pretty-printed for editing; `render_save_raw` accepts this
        // unchanged (`serde_json::from_str` tolerates whitespace) and
        // `NodeTransportExt::set_config` already compacts it back to a single line
        // before it ever reaches the daemon.
        raw_json: serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".to_string()),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn render_degraded(caps: &CapabilitySnapshot) -> String {
    let chrome = Chrome::new(caps, "settings");
    let (config_toml, config_toml_path) = read_config_toml();
    let tmpl = SettingsTemplate {
        chrome,
        daemon_up: false,
        groups: Vec::new(),
        complex_notes_html: String::new(),
        daemon_owned_json: String::new(),
        config_toml,
        config_toml_path,
        raw_json: String::new(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// Build the grouped typed-form view model from the current settings
/// document, in `SCHEMA` order (first appearance of a group name wins its
/// position). `Complex`-kind fields are skipped — they're surfaced only via
/// `complex_notes_html` and the raw JSON escape hatch.
fn build_groups(cfg: &Value) -> Vec<GroupView> {
    let mut groups: Vec<GroupView> = Vec::new();
    for f in SCHEMA {
        if matches!(f.kind, FieldKind::Complex) {
            continue;
        }
        let field = FieldView {
            key: f.key,
            label: f.label,
            input_html: render_input(f, cfg),
        };
        match groups.iter_mut().find(|g| g.name == f.group) {
            Some(g) => g.fields.push(field),
            None => groups.push(GroupView {
                name: f.group,
                fields: vec![field],
            }),
        }
    }
    groups
}

/// Render a single field's `<input>`/`<select>` element, pre-filled from
/// `cfg` (falling back to the schema default when the key is absent or the
/// wrong JSON type).
fn render_input(f: &SettingField, cfg: &Value) -> String {
    match f.kind {
        FieldKind::Bool => {
            let current = cfg
                .get(f.key)
                .and_then(Value::as_bool)
                .unwrap_or(f.default == "true");
            format!(
                r#"<input type="checkbox" id="{k}" name="{k}"{chk}>"#,
                k = f.key,
                chk = if current { " checked" } else { "" }
            )
        }
        FieldKind::Enum(allowed) => {
            let current = cfg.get(f.key).and_then(Value::as_str).unwrap_or(f.default);
            let mut opts = String::new();
            for opt in allowed {
                let sel = if *opt == current { " selected" } else { "" };
                opts.push_str(&format!(
                    r#"<option value="{o}"{sel}>{o}</option>"#,
                    o = escape_attr(opt),
                    sel = sel
                ));
            }
            format!(
                r#"<select id="{k}" name="{k}">{opts}</select>"#,
                k = f.key,
                opts = opts
            )
        }
        FieldKind::Int { min, max } => {
            let current = cfg
                .get(f.key)
                .and_then(Value::as_i64)
                .map(|n| n.to_string())
                .unwrap_or_else(|| f.default.to_string());
            let min_attr = min.map(|m| format!(r#" min="{m}""#)).unwrap_or_default();
            let max_attr = max.map(|m| format!(r#" max="{m}""#)).unwrap_or_default();
            format!(
                r#"<input type="number" id="{k}" name="{k}" value="{v}"{min_attr}{max_attr}>"#,
                k = f.key,
                v = escape_attr(&current)
            )
        }
        FieldKind::Float => {
            let current = cfg
                .get(f.key)
                .and_then(Value::as_f64)
                .map(|n| n.to_string())
                .unwrap_or_else(|| f.default.to_string());
            format!(
                r#"<input type="number" step="0.01" id="{k}" name="{k}" value="{v}">"#,
                k = f.key,
                v = escape_attr(&current)
            )
        }
        FieldKind::Str => {
            let current = cfg
                .get(f.key)
                .and_then(Value::as_str)
                .unwrap_or(f.default)
                .to_string();
            format!(
                r#"<input type="text" id="{k}" name="{k}" value="{v}">"#,
                k = f.key,
                v = escape_attr(&current)
            )
        }
        FieldKind::StrList => {
            let current = cfg
                .get(f.key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            format!(
                r#"<textarea id="{k}" name="{k}" rows="3">{v}</textarea>"#,
                k = f.key,
                v = escape_attr(&current)
            )
        }
        FieldKind::Complex => String::new(),
    }
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A pre-rendered (safe-to-inline) note listing the `Complex`-kind schema
/// keys, for the "edit these via raw JSON instead" callout.
fn complex_notes_html() -> String {
    let keys: Vec<&str> = SCHEMA
        .iter()
        .filter(|f| matches!(f.kind, FieldKind::Complex))
        .map(|f| f.key)
        .collect();
    keys.iter()
        .map(|k| format!("<code>{}</code>", escape_attr(k)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pretty-printed JSON of just the daemon-owned keys present in `cfg`, for
/// the read-only bindings viewer.
fn daemon_owned_json(cfg: &Value) -> String {
    let mut obj = serde_json::Map::new();
    for key in DAEMON_OWNED_KEYS {
        if let Some(v) = cfg.get(*key) {
            obj.insert((*key).to_string(), v.clone());
        }
    }
    serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default()
}

/// Read `config.toml` read-only for display. Missing/unreadable file yields
/// an honest placeholder rather than an error — this page never writes it
/// (the edit path is deferred; see `docs/PANEL.md`).
fn read_config_toml() -> (String, String) {
    let path = crate::config::config_toml_path();
    let path_str = path.display().to_string();
    match std::fs::read_to_string(&path) {
        Ok(content) => (content, path_str),
        Err(_) => (format!("config.toml not found at {path_str}"), path_str),
    }
}

// ---------------------------------------------------------------------------
// POST /settings/save — typed form
// ---------------------------------------------------------------------------

/// `POST /settings/save` — parses a submitted form against `SCHEMA` and
/// `set-config`s the resulting patch. Checkbox absence maps to explicit
/// `false`; an invalid enum/int/float value fails validation and returns an
/// error partial without writing anything.
pub async fn save(
    State(state): State<SharedState>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    Html(render_save(&state, &form).await)
}

pub async fn render_save(state: &AppState, form: &HashMap<String, String>) -> String {
    match build_patch(form) {
        Ok(patch) => match state.node.set_config(&patch).await {
            Ok(()) => result_html(true, "Settings saved."),
            Err(e) => result_html(false, &format!("Save failed: {e}")),
        },
        Err(msg) => result_html(false, &msg),
    }
}

/// Build a `set-config` patch containing ONLY the non-`Complex` `SCHEMA`
/// keys. Bool fields always get an entry (`form.contains_key` gates
/// true/false, so an unchecked box is sent as explicit `false`); other
/// fields are included only when present in `form` (a missing typed field
/// leaves that key untouched via the shallow merge rather than guessing a
/// value). Returns `Err(message)` on the first validation failure — no
/// partial patch is ever sent.
fn build_patch(form: &HashMap<String, String>) -> Result<Value, String> {
    let mut patch = serde_json::Map::new();
    for f in SCHEMA {
        match f.kind {
            FieldKind::Complex => continue,
            FieldKind::Bool => {
                patch.insert(f.key.to_string(), Value::Bool(form.contains_key(f.key)));
            }
            FieldKind::Enum(allowed) => {
                if let Some(v) = form.get(f.key) {
                    if !allowed.contains(&v.as_str()) {
                        return Err(format!(
                            "invalid value for {}: {:?} (allowed: {})",
                            f.key,
                            v,
                            allowed.join(", ")
                        ));
                    }
                    patch.insert(f.key.to_string(), Value::String(v.clone()));
                }
            }
            FieldKind::Int { min, max } => {
                if let Some(v) = form.get(f.key) {
                    let n: i64 = v
                        .trim()
                        .parse()
                        .map_err(|_| format!("invalid integer for {}: {:?}", f.key, v))?;
                    if let Some(min) = min {
                        if n < min {
                            return Err(format!("{} must be >= {min}", f.key));
                        }
                    }
                    if let Some(max) = max {
                        if n > max {
                            return Err(format!("{} must be <= {max}", f.key));
                        }
                    }
                    patch.insert(f.key.to_string(), Value::Number(n.into()));
                }
            }
            FieldKind::Float => {
                if let Some(v) = form.get(f.key) {
                    let n: f64 = v
                        .trim()
                        .parse()
                        .map_err(|_| format!("invalid number for {}: {:?}", f.key, v))?;
                    let num = serde_json::Number::from_f64(n)
                        .ok_or_else(|| format!("invalid number for {}", f.key))?;
                    patch.insert(f.key.to_string(), Value::Number(num));
                }
            }
            FieldKind::Str => {
                if let Some(v) = form.get(f.key) {
                    patch.insert(f.key.to_string(), Value::String(v.clone()));
                }
            }
            FieldKind::StrList => {
                if let Some(v) = form.get(f.key) {
                    let items: Vec<Value> = v
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(|l| Value::String(l.to_string()))
                        .collect();
                    patch.insert(f.key.to_string(), Value::Array(items));
                }
            }
        }
    }
    Ok(Value::Object(patch))
}

// ---------------------------------------------------------------------------
// POST /settings/raw — raw JSON escape hatch
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RawForm {
    raw_json: String,
}

/// `POST /settings/raw` — validates the submitted text is a JSON *object*
/// server-side before writing anything (client-side JS in `settings.html`
/// does the same check for immediate feedback, but this is the
/// authoritative gate). A parse failure or non-object body returns a 200
/// error partial; nothing is sent to the daemon in either case.
pub async fn save_raw(
    State(state): State<SharedState>,
    Form(form): Form<RawForm>,
) -> impl IntoResponse {
    Html(render_save_raw(&state, &form.raw_json).await)
}

pub async fn render_save_raw(state: &AppState, raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() => match state.node.set_config(&v).await {
            Ok(()) => result_html(true, "Raw JSON merged into settings.json."),
            Err(e) => result_html(false, &format!("Save failed: {e}")),
        },
        Ok(_) => result_html(
            false,
            "Invalid: raw JSON must be an object, e.g. {\"key\":value} — not an array or scalar.",
        ),
        Err(e) => result_html(false, &format!("Invalid JSON: {e}")),
    }
}

//! The `settings.json` schema shared by every page that owns a slice of it.
//!
//! **Not a page.** The Settings page was dissolved in `docs/PANEL_IA.md`
//! phase 3; what is left here is the typed schema (mirroring the QML-owned
//! keys `SettingsStore.qml` persists), the form renderer, and the scoped
//! `set-config` patch builder that the five surviving save routes share:
//!
//! | Route | Groups |
//! |---|---|
//! | `POST /shell/appearance/save` | `Appearance` |
//! | `POST /shell/apps/save` | `Apps` |
//! | `POST /devices/display-audio/save` | `Display`, `Night Light`, `Power`, `Audio` |
//! | `POST /devices/cec/config` | `CEC` |
//! | `POST /devices/controllers/settings/save` | `Input` |
//!
//! Splitting one form into five is what makes [`build_patch`]'s group scoping
//! load-bearing rather than cosmetic — see its doc comment.

use std::collections::HashMap;

use askama::Template;
use serde_json::Value;

use crate::state::AppState;
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
    /// typed save patch — editable only via the raw JSON escape hatch on
    /// Shell ▸ Advanced.
    Complex,
}

pub struct SettingField {
    pub key: &'static str,
    pub label: &'static str,
    /// The page-facing grouping, and the unit of [`build_patch`]'s scoping —
    /// a save only ever touches the groups its form declared.
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
/// P0, with the registry IPC arriving in P1. Rendered read-only on Shell ▸
/// Advanced — the Controllers page owns resolved bindings, and web-app
/// management follows the registry IPC — and NEVER emitted in a typed or raw
/// save patch the panel constructs itself (the raw JSON escape hatch can still
/// touch them if an operator explicitly types them in, same as any other key).
pub const DAEMON_OWNED_KEYS: &[&str] = &[
    "keyBindings",
    "perGameBindings",
    "perPlayerBindings",
    "webApps",
];

/// The hidden form field each split settings form emits — once per
/// [`SettingField::group`] it renders — so [`build_patch`] knows which slice
/// of the schema the submission actually covers.
pub const GROUP_FIELD: &str = "__group";

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

pub struct FieldView {
    pub key: &'static str,
    pub label: &'static str,
    pub input_html: String,
}

pub struct GroupView {
    pub name: &'static str,
    pub fields: Vec<FieldView>,
}

#[derive(Template)]
#[template(path = "settings_result.html")]
struct SettingsResultTemplate {
    ok: bool,
    message: String,
}

/// The shared save-result partial every settings form swaps in.
pub fn result_html(ok: bool, message: &str) -> String {
    let tmpl = SettingsResultTemplate {
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

// ---------------------------------------------------------------------------
// Form rendering
// ---------------------------------------------------------------------------

/// Build the grouped typed-form view model for `scope` from the current
/// settings document, in `SCHEMA` order (first appearance of a group name wins
/// its position). Groups outside `scope` are skipped entirely, and
/// `Complex`-kind fields are never rendered — they are surfaced only via
/// [`complex_notes_html`] and the raw JSON escape hatch on Shell ▸ Advanced.
pub fn build_groups(cfg: &Value, scope: &[&str]) -> Vec<GroupView> {
    let mut groups: Vec<GroupView> = Vec::new();
    for f in SCHEMA {
        if matches!(f.kind, FieldKind::Complex) || !scope.contains(&f.group) {
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
/// keys, for Shell ▸ Advanced's "edit these via raw JSON instead" callout.
pub fn complex_notes_html() -> String {
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
/// Shell ▸ Advanced's read-only bindings viewer.
pub fn daemon_owned_json(cfg: &Value) -> String {
    let mut obj = serde_json::Map::new();
    for key in DAEMON_OWNED_KEYS {
        if let Some(v) = cfg.get(*key) {
            obj.insert((*key).to_string(), v.clone());
        }
    }
    serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default()
}

/// Read `config.toml` read-only for display. Missing/unreadable file yields
/// an honest placeholder rather than an error — the panel never writes it
/// from Shell ▸ Advanced (the one targeted exception anywhere is the CEC
/// page's `[cec].osd_name` editor; see `docs/PANEL.md`).
pub fn read_config_toml() -> (String, String) {
    let path = crate::config::config_toml_path();
    let path_str = path.display().to_string();
    match std::fs::read_to_string(&path) {
        Ok(content) => (content, path_str),
        Err(_) => (format!("config.toml not found at {path_str}"), path_str),
    }
}

// ---------------------------------------------------------------------------
// The scoped save
// ---------------------------------------------------------------------------

/// Run one settings form's save: build the scoped patch, then `set-config` it.
///
/// `owned` is the route's OWN group list — a compile-time constant on the page
/// module, never anything the client can influence. The submitted `__group`
/// companions must be a non-empty subset of it.
pub async fn render_save(state: &AppState, owned: &[&str], pairs: &[(String, String)]) -> String {
    match build_patch(owned, pairs) {
        Ok(patch) => match state.node.set_config(&patch).await {
            Ok(()) => result_html(true, "Settings saved."),
            Err(e) => result_html(false, &format!("Save failed: {e}")),
        },
        Err(msg) => result_html(false, &msg),
    }
}

/// The groups a submission declared, validated against the schema and against
/// the route's own group list.
///
/// Fails closed on an empty set. That is the whole point: [`build_patch`]
/// writes every `Bool` in scope as an explicit `true`/`false`, so a patch that
/// defaulted to "all groups" when a form forgot its companions would silently
/// clear the 10 checkboxes belonging to the other pages.
fn submitted_groups<'a>(
    owned: &[&'a str],
    pairs: &[(String, String)],
) -> Result<Vec<&'a str>, String> {
    let mut out: Vec<&'a str> = Vec::new();
    for (k, v) in pairs {
        if k != GROUP_FIELD {
            continue;
        }
        let name = v.trim();
        if !SCHEMA.iter().any(|f| f.group == name) {
            return Err(format!(
                "unknown settings group {name:?} — not a group in the settings schema"
            ));
        }
        let owned_name = owned.iter().find(|g| **g == name).ok_or_else(|| {
            format!("settings group {name:?} is not owned by the form that was submitted")
        })?;
        if !out.contains(owned_name) {
            out.push(owned_name);
        }
    }
    if out.is_empty() {
        return Err(format!(
            "no {GROUP_FIELD} submitted — a settings form must declare the schema \
             groups it owns; refusing to patch every group"
        ));
    }
    Ok(out)
}

/// Build a `set-config` patch from a submitted form, as `(name, value)` pairs
/// so the repeated [`GROUP_FIELD`] companions survive (a `HashMap` extractor
/// would keep only the last one).
///
/// **Scoped to the submitted groups.** Each page carries only its own slice of
/// [`SCHEMA`], so the patch must too: a `SCHEMA` entry whose `group` was not
/// declared by the form is skipped entirely and left untouched by the daemon's
/// shallow merge.
///
/// Within a submitted group the old behaviour is unchanged and deliberate:
/// `Bool` fields ALWAYS get an entry (`contains_key` gates true/false, so an
/// unchecked box is written as explicit `false`), while other kinds are
/// included only when present in the form. Returns `Err(message)` on the first
/// validation failure — no partial patch is ever sent.
pub fn build_patch(owned: &[&str], pairs: &[(String, String)]) -> Result<Value, String> {
    let groups = submitted_groups(owned, pairs)?;
    let form: HashMap<&str, &str> = pairs
        .iter()
        .filter(|(k, _)| k != GROUP_FIELD)
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let mut patch = serde_json::Map::new();
    for f in SCHEMA {
        if !groups.contains(&f.group) {
            continue;
        }
        match f.kind {
            FieldKind::Complex => continue,
            FieldKind::Bool => {
                patch.insert(f.key.to_string(), Value::Bool(form.contains_key(f.key)));
            }
            FieldKind::Enum(allowed) => {
                if let Some(v) = form.get(f.key) {
                    if !allowed.contains(v) {
                        return Err(format!(
                            "invalid value for {}: {:?} (allowed: {})",
                            f.key,
                            v,
                            allowed.join(", ")
                        ));
                    }
                    patch.insert(f.key.to_string(), Value::String((*v).to_string()));
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
                    patch.insert(f.key.to_string(), Value::String((*v).to_string()));
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

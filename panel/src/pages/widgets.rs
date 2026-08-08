//! `/widgets` — per-widget `enabled`/`order`/`size`/`prefs` editors over the
//! `widgets.<id>` subtree of `settings.json`, via the daemon's `get-config`/
//! `set-config` IPC commands.
//!
//! CRITICAL: `set-config` performs a SHALLOW top-level merge — the daemon
//! replaces the whole `widgets` value wholesale rather than merging inside
//! it. Sending a partial `widgets` object would silently drop the other
//! widget ids. Every save on this page therefore rebuilds and sends the
//! COMPLETE `widgets` object covering all [`MANIFESTS`] ids, never a patch
//! for just the one edited. The page itself always renders all five cards
//! pre-filled with their current values, so the submitted form already
//! carries the full state plus the one field the operator changed.
//!
//! Prefs schema SSOT: `shell/widgets/lib/WidgetManifests.qml`. KEEP IN SYNC —
//! [`MANIFESTS`] below is a hand-maintained Rust mirror of that file's
//! `manifests` array (ids, names, default order/enabled, per-widget size
//! enum, pref fields + defaults). If one changes, change both.
//!
//! Degradation: mirrors `pages::settings` — `GET /widgets` always returns
//! 200, with a clear "unreachable" banner (no forms) when the daemon is
//! down; never a 500.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde_json::Value;

use crate::capabilities::{CapabilitySnapshot, Chrome};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

// ---------------------------------------------------------------------------
// Widget manifest mirror — KEEP IN SYNC with
// shell/widgets/lib/WidgetManifests.qml's `manifests` array.
// ---------------------------------------------------------------------------

/// A single boolean pref field stored under `widgets.<id>.prefs`. Every
/// current pref across all widgets happens to be boolean (`hideFromRecent`);
/// widening to other `FieldKind`-style types is deferred until the QML side
/// grows one.
pub struct PrefField {
    pub key: &'static str,
    pub label: &'static str,
    pub default: bool,
}

pub struct WidgetManifest {
    pub id: &'static str,
    pub name: &'static str,
    pub default_order: i64,
    pub default_enabled: bool,
    /// Allowed `size` values, in display order. DIFFERS per widget — see
    /// `WidgetManifests.qml`.
    pub sizes: &'static [&'static str],
    pub size_default: &'static str,
    pub prefs: &'static [PrefField],
}

/// Framework order (moonlight, nowplaying, plex, recent, steam) — matches
/// `WidgetManifests.qml`'s `manifests` array order.
pub const MANIFESTS: &[WidgetManifest] = &[
    WidgetManifest {
        id: "moonlight",
        name: "Moonlight",
        default_order: 0,
        default_enabled: true,
        sizes: &["small", "medium", "large"],
        size_default: "medium",
        prefs: &[],
    },
    WidgetManifest {
        id: "nowplaying",
        name: "Now Playing",
        default_order: 1,
        default_enabled: true,
        sizes: &["small", "medium"],
        size_default: "medium",
        prefs: &[PrefField {
            key: "hideFromRecent",
            label: "Hide from Recent",
            default: true,
        }],
    },
    WidgetManifest {
        id: "plex",
        name: "Plex",
        default_order: 2,
        default_enabled: true,
        sizes: &["small", "medium"],
        size_default: "medium",
        prefs: &[PrefField {
            key: "hideFromRecent",
            label: "Hide from Recent",
            default: true,
        }],
    },
    WidgetManifest {
        id: "recent",
        name: "Apps",
        default_order: 3,
        default_enabled: true,
        sizes: &["small", "medium"],
        size_default: "medium",
        prefs: &[],
    },
    WidgetManifest {
        id: "steam",
        name: "Steam",
        default_order: 4,
        default_enabled: false,
        sizes: &["medium", "large"],
        size_default: "medium",
        prefs: &[],
    },
];

// ---------------------------------------------------------------------------
// Current-state resolution — mirrors `widgetConfig.js`'s `defaultSubtree` +
// `_fillWidget`: read existing typed values where present, else fall back to
// the manifest default. A `size` value outside the widget's own enum is
// treated as absent (falls back to default) rather than rendered as an
// unselectable option.
// ---------------------------------------------------------------------------

struct CurrentWidget {
    enabled: bool,
    order: i64,
    size: String,
    /// (pref key, current value), in manifest order.
    prefs: Vec<(&'static str, bool)>,
}

/// Resolve every [`MANIFESTS`] entry's current state against `cfg`'s
/// `widgets` subtree, in [`MANIFESTS`] declaration order (NOT yet sorted by
/// `order` — callers that need display/reorder order call
/// [`sort_by_order`] on the result).
fn resolve_current(cfg: &Value) -> Vec<(&'static WidgetManifest, CurrentWidget)> {
    let widgets_obj = cfg.get("widgets").and_then(Value::as_object);
    MANIFESTS
        .iter()
        .map(|m| {
            let existing = widgets_obj.and_then(|o| o.get(m.id));
            (m, current_widget(m, existing))
        })
        .collect()
}

/// Sort a resolved widget list by its persisted `order` field. A stable sort
/// (Rust's `sort_by_key` is stable) so widgets that tie on `order` keep
/// their [`MANIFESTS`] declaration order rather than shuffling
/// nondeterministically.
fn sort_by_order(current: &mut [(&'static WidgetManifest, CurrentWidget)]) {
    current.sort_by_key(|(_, cur)| cur.order);
}

fn current_widget(m: &'static WidgetManifest, existing: Option<&Value>) -> CurrentWidget {
    let mut enabled = m.default_enabled;
    let mut order = m.default_order;
    let mut size = m.size_default.to_string();
    let mut prefs: Vec<(&'static str, bool)> = m.prefs.iter().map(|p| (p.key, p.default)).collect();

    if let Some(w) = existing {
        if let Some(b) = w.get("enabled").and_then(Value::as_bool) {
            enabled = b;
        }
        if let Some(n) = w.get("order").and_then(Value::as_i64) {
            order = n;
        }
        if let Some(s) = w.get("size").and_then(Value::as_str) {
            if m.sizes.contains(&s) {
                size = s.to_string();
            }
        }
        if let Some(p) = w.get("prefs").and_then(Value::as_object) {
            for (key, val) in prefs.iter_mut() {
                if let Some(b) = p.get(*key).and_then(Value::as_bool) {
                    *val = b;
                }
            }
        }
    }

    CurrentWidget {
        enabled,
        order,
        size,
        prefs,
    }
}

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

struct PrefView {
    key: &'static str,
    label: &'static str,
    checked: bool,
}

struct WidgetCardView {
    id: &'static str,
    name: &'static str,
    enabled: bool,
    order: i64,
    size_select_html: String,
    prefs: Vec<PrefView>,
}

#[derive(Template)]
#[template(path = "widgets.html")]
struct WidgetsTemplate {
    chrome: Chrome,
    daemon_up: bool,
    cards: Vec<WidgetCardView>,
}

#[derive(Template)]
#[template(path = "widgets_result.html")]
struct WidgetsResultTemplate {
    tier: &'static str,
    ok: bool,
    message: String,
}

fn result_html(ok: bool, message: &str) -> String {
    let tmpl = WidgetsResultTemplate {
        tier: "IPC",
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

// ---------------------------------------------------------------------------
// GET /widgets
// ---------------------------------------------------------------------------

/// `GET /widgets` — fetches the current settings document via `get-config`
/// and renders one card per manifest widget, default-filled from
/// [`MANIFESTS`], or a degraded banner (still HTTP 200) when the daemon is
/// unreachable.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    match state.node.get_config().await {
        Ok(cfg) => render_ok(&state.caps, &cfg),
        Err(_e) => render_degraded(&state.caps),
    }
}

/// Build a card's view model from its manifest + resolved current state.
/// Shared by the initial page render and the reorder partial (#9) so both
/// stay pixel-identical.
fn build_card_view(m: &'static WidgetManifest, cur: &CurrentWidget) -> WidgetCardView {
    WidgetCardView {
        id: m.id,
        name: m.name,
        enabled: cur.enabled,
        order: cur.order,
        size_select_html: render_size_select(m, &cur.size),
        prefs: cur
            .prefs
            .iter()
            .zip(m.prefs)
            .map(|((key, checked), pf)| PrefView {
                key,
                label: pf.label,
                checked: *checked,
            })
            .collect(),
    }
}

fn render_ok(caps: &CapabilitySnapshot, cfg: &Value) -> String {
    let chrome = Chrome::new(caps, "widgets");
    let mut current = resolve_current(cfg);
    sort_by_order(&mut current);
    let cards = current
        .iter()
        .map(|(m, cur)| build_card_view(m, cur))
        .collect();

    let tmpl = WidgetsTemplate {
        chrome,
        daemon_up: true,
        cards,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn render_degraded(caps: &CapabilitySnapshot) -> String {
    let chrome = Chrome::new(caps, "widgets");
    let tmpl = WidgetsTemplate {
        chrome,
        daemon_up: false,
        cards: Vec::new(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// Render a widget's `size` `<select>`, pre-selected at `current` (already
/// validated to be one of `m.sizes` by [`current_widget`]).
fn render_size_select(m: &WidgetManifest, current: &str) -> String {
    let mut opts = String::new();
    for s in m.sizes {
        let sel = if *s == current { " selected" } else { "" };
        opts.push_str(&format!(r#"<option value="{s}"{sel}>{s}</option>"#));
    }
    format!(
        r#"<select id="w_{id}_size" name="w_{id}_size">{opts}</select>"#,
        id = m.id,
        opts = opts
    )
}

// ---------------------------------------------------------------------------
// POST /widgets/reorder/:id/up|down (#9)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "widgets_grid.html")]
struct WidgetsGridTemplate {
    cards: Vec<WidgetCardView>,
}

fn render_grid(cards: Vec<WidgetCardView>) -> String {
    let tmpl = WidgetsGridTemplate { cards };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /widgets/reorder/:id/up` — swap `id` with its predecessor in the
/// current display order.
pub async fn reorder_up(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    Html(render_reorder(&state, &id, "up").await)
}

/// `POST /widgets/reorder/:id/down` — swap `id` with its successor in the
/// current display order.
pub async fn reorder_down(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    Html(render_reorder(&state, &id, "down").await)
}

/// Swap widget `id` with its `up`/`down` neighbor in the current order,
/// renumber ALL five widgets' `order` fields to a clean sequential 0..N
/// (self-healing any pre-existing gaps/ties), and `set-config` the COMPLETE
/// `widgets` object in one call — the same shallow-merge requirement
/// [`build_widgets_patch`] documents for the form-driven Save button.
/// Returns just the refreshed `#widget-grid` partial (`hx-swap="outerHTML"`
/// on the caller side), so this is an immediately-persisted, standalone
/// action: any unsaved edits to OTHER cards' enabled/size/prefs fields are
/// reset to their last-persisted value when the grid re-renders, rather than
/// deferred to the page's "Save widgets" button.
pub async fn render_reorder(state: &AppState, id: &str, dir: &str) -> String {
    let cfg = match state.node.get_config().await {
        Ok(cfg) => cfg,
        Err(e) => {
            return format!(
                "<p class=\"banner banner-error\">Reorder failed: {e}</p>{}",
                render_grid(Vec::new())
            )
        }
    };

    let mut current = resolve_current(&cfg);
    sort_by_order(&mut current);

    if let Some(pos) = current.iter().position(|(m, _)| m.id == id) {
        let swap_with = match dir {
            "up" if pos > 0 => Some(pos - 1),
            "down" if pos + 1 < current.len() => Some(pos + 1),
            // Already at the boundary (or an unrecognized `dir`, unreachable
            // via the two fixed routes) — no swap, but still fall through to
            // the renumber+persist below so a stale/duplicate order in
            // settings.json gets self-healed either way.
            _ => None,
        };
        if let Some(other) = swap_with {
            current.swap(pos, other);
        }
    }
    // An unknown `id` (stale card, race with a manifest change) leaves
    // `current` unswapped — still renumbered/re-rendered below rather than
    // erroring, since the grid must always reflect *some* valid state.

    for (i, (_, cur)) in current.iter_mut().enumerate() {
        cur.order = i as i64;
    }

    let patch = widgets_patch_from(&current);
    let cards: Vec<WidgetCardView> = current
        .iter()
        .map(|(m, cur)| build_card_view(m, cur))
        .collect();

    match state.node.set_config(&patch).await {
        Ok(()) => render_grid(cards),
        Err(e) => format!(
            "<p class=\"banner banner-error\">Reorder failed to save: {e}</p>{}",
            render_grid(cards)
        ),
    }
}

/// Build a complete `{"widgets": {...}}` `set-config` patch from an already
/// resolved+ordered widget list — the reorder-path counterpart of
/// [`build_widgets_patch`] (which builds the same shape from a raw HTML
/// form instead of typed [`CurrentWidget`]s).
fn widgets_patch_from(current: &[(&'static WidgetManifest, CurrentWidget)]) -> Value {
    let mut widgets = serde_json::Map::new();
    for (m, cur) in current {
        let mut prefs = serde_json::Map::new();
        for (key, val) in &cur.prefs {
            prefs.insert((*key).to_string(), Value::Bool(*val));
        }
        let mut widget = serde_json::Map::new();
        widget.insert("enabled".to_string(), Value::Bool(cur.enabled));
        widget.insert("order".to_string(), Value::Number(cur.order.into()));
        widget.insert("size".to_string(), Value::String(cur.size.clone()));
        widget.insert("prefs".to_string(), Value::Object(prefs));
        widgets.insert(m.id.to_string(), Value::Object(widget));
    }
    let mut patch = serde_json::Map::new();
    patch.insert("widgets".to_string(), Value::Object(widgets));
    Value::Object(patch)
}

// ---------------------------------------------------------------------------
// POST /widgets/save
// ---------------------------------------------------------------------------

/// `POST /widgets/save` — the page always submits one whole-page form
/// covering all widgets (each pre-filled with its current value), so this
/// rebuilds and `set-config`s the COMPLETE `widgets` object in one call. An
/// invalid size for a given widget (checked against that widget's own
/// [`WidgetManifest::sizes`]) or a non-integer order fails validation and
/// returns an error partial without writing anything.
pub async fn save(
    State(state): State<SharedState>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    Html(render_save(&state, &form).await)
}

pub async fn render_save(state: &AppState, form: &HashMap<String, String>) -> String {
    match build_widgets_patch(form) {
        Ok(patch) => match state.node.set_config(&patch).await {
            Ok(()) => result_html(true, "Widgets saved."),
            Err(e) => result_html(false, &format!("Save failed: {e}")),
        },
        Err(msg) => result_html(false, &msg),
    }
}

/// Build a `{"widgets": {...}}` `set-config` patch containing all
/// [`MANIFESTS`] ids. Checkbox absence maps to explicit `false` (mirrors
/// `pages::settings::build_patch`'s bool handling); a missing/blank `order`
/// or `size` field falls back to the widget's manifest default rather than
/// failing (so a hand-built form, e.g. in a test, doesn't need every field).
/// Returns `Err(message)` on the first validation failure — no partial
/// patch is ever sent.
fn build_widgets_patch(form: &HashMap<String, String>) -> Result<Value, String> {
    let mut widgets = serde_json::Map::new();

    for m in MANIFESTS {
        let enabled = form.contains_key(&format!("w_{}_enabled", m.id));

        let order_key = format!("w_{}_order", m.id);
        let order: i64 = match form.get(&order_key).map(|v| v.trim()) {
            Some(v) if !v.is_empty() => v
                .parse()
                .map_err(|_| format!("invalid integer order for {}: {:?}", m.id, v))?,
            _ => m.default_order,
        };

        let size_key = format!("w_{}_size", m.id);
        let size = form
            .get(&size_key)
            .map(String::as_str)
            .unwrap_or(m.size_default);
        if !m.sizes.contains(&size) {
            return Err(format!(
                "invalid size for {}: {:?} (allowed: {})",
                m.id,
                size,
                m.sizes.join(", ")
            ));
        }

        let mut prefs = serde_json::Map::new();
        for p in m.prefs {
            let pref_key = format!("w_{}_pref_{}", m.id, p.key);
            prefs.insert(p.key.to_string(), Value::Bool(form.contains_key(&pref_key)));
        }

        let mut widget = serde_json::Map::new();
        widget.insert("enabled".to_string(), Value::Bool(enabled));
        widget.insert("order".to_string(), Value::Number(order.into()));
        widget.insert("size".to_string(), Value::String(size.to_string()));
        widget.insert("prefs".to_string(), Value::Object(prefs));

        widgets.insert(m.id.to_string(), Value::Object(widget));
    }

    let mut patch = serde_json::Map::new();
    patch.insert("widgets".to_string(), Value::Object(widgets));
    Ok(Value::Object(patch))
}

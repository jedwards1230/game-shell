//! tv-shell-panel — a LAN-only, server-rendered (axum + askama + vendored
//! HTMX) web control panel for the tv-shell HTPC daemon.
//!
//! M1 scope: the crate scaffold, three data-tier clients (`ipc` — the
//! primary Unix-socket IPC tier, `bridge` — the daemon's opt-in HTTP dev-ops
//! bridge, `exec` — a direct-exec recovery tier for when both of the above
//! are down), the app shell with nav for all nine pages, and three fully
//! implemented pages (Dashboard, Logs, Dev). M2 added Settings and Widgets;
//! M3 added the Tools console, Processes page, and the Dev-page screenshot
//! viewer. Controllers and CEC still render an honest stub until M4 lands.
//!
//! Auth (S1): every route is gated by the [`auth`] middleware except four
//! exemptions (`GET /assets/htmx.min.js`, `GET /assets/style.css`, and both
//! methods of `/login`). A browser exchanges the `[panel].token_file` secret
//! for a session cookie at `/login`; scripts send `Authorization: Bearer`.
//! With `[panel].token_file` unset the panel runs unauthenticated — which is
//! only permitted on a loopback bind (see `config::AppConfig::validate`).

mod assets;
mod auth;
mod bridge;
mod capabilities;
mod config;
mod exec;
mod humanize;
#[cfg(unix)]
mod ipc;
mod pages;
mod state;
mod text;
mod transport;
mod updates;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;

use capabilities::Gate;
use state::{AppState, SharedState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Resolves the panel's own token eagerly and refuses to start on a
    // non-loopback bind with auth effectively disabled (S3) — deliberately
    // BEFORE the listener below is bound.
    let cfg = config::load()?;
    if !cfg.enabled {
        tracing::info!("tv-shell-panel: disabled ([panel].enabled = false) — exiting cleanly");
        return Ok(());
    }

    let panel_bind = cfg.panel_bind;
    tracing::info!(
        "tv-shell-panel: config resolved — [panel].bind={:?}, auth={}, allow_dangerous={}",
        cfg.panel_bind_raw,
        if cfg.auth_enabled() {
            "enabled ([panel].token_file)"
        } else {
            "DISABLED (no [panel].token_file)"
        },
        cfg.allow_dangerous
    );
    let sock = config::socket_path();
    let node: Arc<dyn transport::NodeTransport> = Arc::new(ipc::IpcTransport::new(sock));
    let bridge: Arc<dyn bridge::DevBridge> = Arc::new(bridge::BridgeClient::new(
        cfg.http_bridge_base.clone(),
        cfg.http_token.clone(),
    ));
    let recovery = exec::Recovery::new();
    let updates = updates::UpdatesState::default();

    // The capability handshake — bounded, before the router is built, because
    // registration is static. A failed handshake yields the empty set, which
    // registers the recovery tier and nothing else (see `capabilities`).
    let caps = capabilities::handshake(node.as_ref()).await;
    // The two failure modes get different journal text for the same reason the
    // banner does: "restart the panel once the daemon is back" is wrong advice
    // when the daemon is already back and merely too old to answer.
    let handshake = match &caps.handshake {
        capabilities::Handshake::Ok => "ok".to_string(),
        capabilities::Handshake::Unreachable => {
            "FAILED, node unreachable (recovery tier only — restart the panel once the \
             daemon is back)"
                .to_string()
        }
        capabilities::Handshake::Refused(why) => format!(
            "FAILED, node refused: {why} (recovery tier only — the daemon is up but does \
             not speak `capabilities`; it is probably older than this panel, so rebuild \
             and redeploy it rather than restarting the panel)"
        ),
    };
    tracing::info!(
        "tv-shell-panel: capabilities — handshake={}, node_id={:?}, features=[{}]",
        handshake,
        caps.node_id,
        caps.feature_list(),
    );

    let state: SharedState = Arc::new(AppState {
        cfg,
        caps,
        node,
        bridge,
        recovery,
        updates,
    });

    let app = build_router(state);

    tracing::info!("tv-shell-panel listening on {panel_bind}");
    let listener = tokio::net::TcpListener::bind(panel_bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the panel's `Router`.
///
/// Three security properties live here, each covered by `crate::tests`:
///
/// 1. **Every registered route is behind [`auth::require_auth`]**, attached
///    with `route_layer` so it runs for every MATCHED route while an
///    unregistered path stays a plain 404.
/// 2. **The root-equivalent routes are only registered when
///    `[panel].allow_dangerous = true`** (S5) — an ungated-off route is not
///    404-by-handler, it does not exist. The corresponding UI affordances are
///    hidden by the same flag, so the panel never renders a button that 404s.
/// 3. **Every other route is registered only when the node declared it can
///    serve it** ([`capabilities`]). Same shape, same consequence: a gated-off
///    route 404s because it was never registered, and the nav is built from the
///    same [`Gate`] values so a hidden page has no link either.
///
/// ## The registration blocks are the tiers, and they are parsed
///
/// `crate::tests` reads THIS FUNCTION textually to attribute every route
/// declaration to the block it sits in, then asserts that attribution against the
/// hand-maintained `route_table()`. It understands exactly the block form used
/// below — `let app = if <condition> {` where `<condition>` is
/// `allow_dangerous`, `caps.allows(Gate::<Variant>)`, or the two ANDed — and
/// **panics on anything else**, including a nested conditional or a rebound
/// `app`. That is deliberate: an unattributed route is an unchecked route. A
/// new registration form must be taught to the parser in the same change.
fn build_router(state: SharedState) -> Router {
    let allow_dangerous = state.cfg.allow_dangerous;
    let caps = &state.caps;

    // ── Recovery tier — always registered, gated on nothing ────────────────
    //
    // None of these need the daemon: they are the panel's own exec tier
    // (systemctl/journalctl/ps/pacman), its own filesystem work, or static.
    // This is the panel's reason to exist, so a failed capability handshake
    // must not remove any of them. Every mutating route here is additionally
    // pinned by `tests::RECOVERY_TIER_MUTATING`, which each entry must justify.
    //
    // Route registration is one line per page so later milestones can add
    // routes without touching neighboring lines.
    let app = Router::new()
        .route("/", get(pages::dashboard::page))
        .route("/dashboard", get(pages::dashboard::page))
        .route("/dashboard/tiles", get(pages::dashboard::tiles)) // htmx poll partial
        .route(
            "/dashboard/updates-tile",
            get(pages::dashboard::updates_tile),
        ) // htmx poll partial, own slower interval
        .route("/processes", get(pages::processes::page))
        .route("/processes/restart/{key}", post(pages::processes::restart))
        .route(
            "/processes/updates/refresh",
            post(pages::processes::updates_refresh),
        )
        .route("/processes/updates/job", get(pages::processes::updates_job))
        // Media: the page itself, plus the wallpaper routes the PANEL serves
        // out of its own filesystem. The upload route raises the body limit
        // past axum's 2 MB default; `MAX_UPLOAD_BYTES` is still enforced
        // per-file in the handler. Deliberately NOT gated on
        // `Feature::Wallpapers`: `daemon/src/ipc.rs::features()` never emits
        // that (wallpapers belong to QML and to this filesystem tier), so
        // gating on it would delete a working page from every live node.
        .route("/media", get(pages::media::page))
        .route(
            "/media/wallpaper/upload",
            post(pages::media::upload).layer(DefaultBodyLimit::max(pages::media::MAX_UPLOAD_BYTES)),
        )
        .route("/media/wallpaper/delete", post(pages::media::delete))
        .route("/media/wallpaper/file", get(pages::media::file))
        // The shell pane comes from the bridge and degrades inline; the daemon
        // pane is `journalctl` via direct exec. So this page reads logs with no
        // node at all and is NOT `Feature::Logs` (which describes the DAEMON's
        // own `GET /dev/logs` and is emitted only with a network bridge).
        .route("/logs", get(pages::logs::page))
        .route("/logs/view", get(pages::logs::view)) // htmx refresh partial
        .route("/dev", get(pages::dev::page))
        // Recovery, NOT part of the S5 set: these restart the same two systemd
        // units `POST /processes/restart/{key}` restarts, so gating them while
        // that stays open would buy nothing.
        .route("/dev/restart-daemon", post(pages::dev::restart_daemon))
        .route("/dev/restart-shell", post(pages::dev::restart_shell))
        .route("/nav/daemon-status", get(pages::nav::daemon_status_dot))
        // The four auth-exempt routes (`auth::PUBLIC_PATHS`): the two
        // compiled-in static assets, plus the login form and its submission.
        .route("/login", get(pages::login::page))
        .route("/login", post(pages::login::submit))
        .route("/assets/htmx.min.js", get(assets::htmx_js))
        .route("/assets/style.css", get(assets::style_css));

    // ── Node tier — registered iff the handshake succeeded ─────────────────
    //
    // The Tools console drives the IPC line protocol directly. These map to no
    // single `Feature` (the daemon does not declare "I answer commands"), so
    // the honest statement is exactly "these exist iff a node answered a
    // handshake". The two controller-DB commands that also live under
    // `/tools/sys/` are in the Controllers block instead — they are the
    // controllers surface reached from a second page.
    let app = if caps.allows(Gate::Node) {
        app.route("/tools", get(pages::tools::page))
            .route("/tools/intent", post(pages::tools::intent))
            .route("/tools/key", post(pages::tools::key))
            .route("/tools/apps/list", post(pages::tools::list_apps))
            .route("/tools/apps/launch", post(pages::tools::launch_app))
            .route("/tools/apps/recents", post(pages::tools::get_recents))
            .route(
                "/tools/bt/power-status",
                post(pages::tools::bt_power_status),
            )
            .route("/tools/bt/power-on", post(pages::tools::bt_power_on))
            .route("/tools/bt/power-off", post(pages::tools::bt_power_off))
            .route("/tools/bt/scan-on", post(pages::tools::bt_scan_on))
            .route("/tools/bt/scan-off", post(pages::tools::bt_scan_off))
            .route("/tools/bt/list", post(pages::tools::bt_list))
            .route("/tools/bt/action", post(pages::tools::bt_action))
            .route("/tools/net/status", post(pages::tools::net_status))
            .route("/tools/net/wifi-list", post(pages::tools::net_wifi_list))
            .route(
                "/tools/net/wifi-rescan",
                post(pages::tools::net_wifi_rescan),
            )
            .route("/tools/net/throughput", post(pages::tools::net_throughput))
            .route("/tools/net/ping", post(pages::tools::net_ping))
            .route(
                "/tools/power/can-suspend",
                post(pages::tools::power_can_suspend),
            )
            .route("/tools/power/battery", post(pages::tools::power_battery))
            .route("/tools/sys/status", post(pages::tools::sys_status))
            .route("/tools/sys/metrics", post(pages::tools::sys_metrics))
            .route("/tools/sys/storage", post(pages::tools::sys_storage))
            .route("/tools/sys/build-info", post(pages::tools::sys_build_info))
    } else {
        app
    };

    // ── Capability tier — one block per declared `Feature` ─────────────────
    //
    // `Feature::Controllers` is emitted on any Linux daemon build (the
    // evdev/uinput runtime). The two `/tools/sys/controllerdb-*` routes sit
    // here, not in the node block: they are the same controller-DB surface the
    // Controllers page owns, reached from the Tools console.
    let app = if caps.allows(Gate::Controllers) {
        app.route(
            "/tools/sys/controllerdb-status",
            post(pages::tools::controllerdb_status),
        )
        .route(
            "/tools/sys/controllerdb-refresh",
            post(pages::tools::controllerdb_refresh),
        )
        .route("/controllers", get(pages::controllers::page))
        .route("/controllers/grab", post(pages::controllers::grab))
        .route("/controllers/release", post(pages::controllers::release))
        .route("/controllers/handoff", post(pages::controllers::handoff))
        .route(
            "/controllers/pad/battery",
            post(pages::controllers::pad_battery),
        )
        .route(
            "/controllers/pad/rumble-status",
            post(pages::controllers::pad_rumble_status),
        )
        .route(
            "/controllers/pad/rumble",
            post(pages::controllers::pad_rumble),
        )
        .route(
            "/controllers/input-devices",
            post(pages::controllers::input_devices),
        )
        .route(
            "/controllers/bindings/set",
            post(pages::controllers::bindings_set),
        )
        .route(
            "/controllers/bindings/capture",
            post(pages::controllers::bindings_capture),
        )
        .route(
            "/controllers/bindings/capture-cancel",
            post(pages::controllers::bindings_capture_cancel),
        )
        .route(
            "/controllers/active-game/set",
            post(pages::controllers::active_game_set),
        )
        .route(
            "/controllers/active-game/clear",
            post(pages::controllers::active_game_clear),
        )
        .route(
            "/controllers/controllerdb/status",
            post(pages::controllers::controllerdb_status),
        )
        .route(
            "/controllers/controllerdb/refresh",
            post(pages::controllers::controllerdb_refresh),
        )
    } else {
        app
    };

    // `Feature::Cec` is a CARGO-FEATURE claim (`--features cec`), never adapter
    // health — a wedged adapter must not delete the page that recovers it.
    // `/cec/recover/restart-daemon` therefore lives here despite being a unit
    // restart: it is the CEC page's own recovery ladder rung, and the two
    // always-registered paths to that same unit (`/dev/restart-daemon`,
    // `/processes/restart/{key}`) are untouched, so nothing is lost when the
    // page is absent.
    let app = if caps.allows(Gate::Cec) {
        app.route("/cec", get(pages::cec::page))
            .route("/cec/scan", post(pages::cec::scan))
            .route("/cec/device", post(pages::cec::device))
            .route("/cec/active-source", post(pages::cec::active_source))
            .route("/cec/power-on", post(pages::cec::power_on))
            .route("/cec/power-off", post(pages::cec::power_off))
            .route("/cec/test", post(pages::cec::test))
            .route("/cec/osd-name", post(pages::cec::save_osd_name))
            .route(
                "/cec/recover/restart-daemon",
                post(pages::cec::recover_restart_daemon),
            )
    } else {
        app
    };

    // The per-widget `widgets.<id>` subtree, read and written over IPC.
    let app = if caps.allows(Gate::Widgets) {
        app.route("/widgets", get(pages::widgets::page))
            .route("/widgets/save", post(pages::widgets::save))
            .route("/widgets/reorder/{id}/up", post(pages::widgets::reorder_up))
            .route(
                "/widgets/reorder/{id}/down",
                post(pages::widgets::reorder_down),
            )
    } else {
        app
    };

    // `get-config` / `set-config`. `/media/wallpaper/select` is here rather
    // than with the other wallpaper routes because selecting is the one that
    // WRITES — it persists `wallpaperPath` through `set-config`.
    let app = if caps.allows(Gate::SettingsStore) {
        app.route("/settings", get(pages::settings::page))
            .route("/settings/save", post(pages::settings::save))
            .route("/settings/raw", post(pages::settings::save_raw))
            .route("/media/wallpaper/select", post(pages::media::select))
    } else {
        app
    };

    // The daemon-owned web-app registry (`webapp-add` / `webapp-remove`).
    let app = if caps.allows(Gate::WebApps) {
        app.route("/media/webapp/add", post(pages::media::webapp_add))
            .route("/media/webapp/remove", post(pages::media::webapp_remove))
    } else {
        app
    };

    // Both routes proxy the daemon's bridge `/screenshot`. The daemon emits
    // `Feature::Screenshot` on Linux with EITHER network bridge configured
    // (`[http]` or `[mcp]`), while this proxy speaks only to the HTTP one — so
    // an MCP-only node declares the capability and registers these routes while
    // the panel has no bridge to call. That is a handled degradation, not a
    // dangling route: `BridgeClient` with no base URL returns
    // `BridgeError::NotConfigured` and `pages::dev` renders the honest
    // "bridge not configured" message. Deliberately NOT additionally gated on
    // `http_bridge_base.is_some()`: the capability is the NODE's statement
    // about itself, and folding a local config check into it would make the
    // route set depend on two different kinds of fact. `dev.html` already
    // carries `bridge_configured` for the UI half.
    //
    // No htpc-1 impact either way — it sets `[http].bind`.
    let app = if caps.allows(Gate::Screenshot) {
        app.route("/dev/screenshot", get(pages::dev::screenshot_png))
            .route(
                "/dev/screenshot/capture",
                post(pages::dev::screenshot_capture),
            )
    } else {
        app
    };

    // ── Danger ∩ capability ───────────────────────────────────────────────
    //
    // Deploy/build are root-equivalent AND go through the daemon bridge's
    // `/dev/deploy` + `/dev/build`, so they need BOTH the operator opt-in and
    // the node's `Feature::DevDeploy`. Either one missing means the route
    // cannot honestly be offered.
    let app = if allow_dangerous && caps.allows(Gate::DevDeploy) {
        app.route("/dev/deploy", post(pages::dev::deploy))
            .route("/dev/build", post(pages::dev::build))
    } else {
        app
    };

    // ── Danger tier ───────────────────────────────────────────────────────
    //
    // S5 — the rest of the root-equivalent set. Registered ONLY under
    // `[panel].allow_dangerous = true`; otherwise these paths do not exist.
    // The line: **restarting a unit is recovery** (ungated — it is the reason
    // the panel exists); **changing what code runs, powering the box, or
    // running arbitrary commands is root-equivalent** (gated, here). So
    // `/dev/restart-daemon`, `/dev/restart-shell`,
    // `POST /processes/restart/{key}` and `POST /cec/recover/restart-daemon`
    // are all deliberately NOT here.
    //
    // Reboot/suspend and the pacman apply are the PANEL's own exec tier, so
    // they carry no capability gate.
    //
    // `/tools/raw` keeps none either, but the honest statement is narrower than
    // it looks: with the handshake failed, `/tools` — the only UI that drives
    // it — is gone, so what survives is a curl-only escape hatch. It is kept
    // ungated because `allow_dangerous` is already the operator's explicit
    // opt-in to an arbitrary-command surface and narrowing it would not remove
    // a capability lie (the route reports the node's own error when the node is
    // down). It is NOT kept because the UI still works, which it does not.
    let app = if allow_dangerous {
        app.route("/dev/reboot", post(pages::dev::reboot))
            .route("/dev/suspend", post(pages::dev::suspend))
            .route(
                "/processes/updates/apply",
                post(pages::processes::updates_apply),
            )
            .route("/tools/raw", post(pages::tools::raw))
    } else {
        app
    };

    app.route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ))
    .with_state(state)
}

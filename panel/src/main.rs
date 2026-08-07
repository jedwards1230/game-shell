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
mod config;
mod exec;
mod humanize;
mod ipc;
mod pages;
mod state;
mod text;
mod updates;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;

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
    let ipc = ipc::IpcClient::new(sock);
    let bridge = bridge::BridgeClient::new(cfg.http_bridge_base.clone(), cfg.http_token.clone());
    let recovery = exec::Recovery::new();
    let updates = updates::UpdatesState::default();

    let state: SharedState = Arc::new(AppState {
        cfg,
        ipc,
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
/// Two security properties live here and are covered by
/// `tests::route_table_matches_main_rs_declarations` + friends:
///
/// 1. **Every registered route is behind [`auth::require_auth`]**, attached
///    with `route_layer` so it runs for every MATCHED route while an
///    unregistered path stays a plain 404.
/// 2. **The root-equivalent routes are only registered when
///    `[panel].allow_dangerous = true`** (S5) — an ungated-off route is not
///    404-by-handler, it does not exist. The corresponding UI affordances are
///    hidden by the same flag, so the panel never renders a button that 404s.
fn build_router(state: SharedState) -> Router {
    let allow_dangerous = state.cfg.allow_dangerous;

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
        .route("/settings", get(pages::settings::page))
        .route("/settings/save", post(pages::settings::save))
        .route("/settings/raw", post(pages::settings::save_raw))
        .route("/widgets", get(pages::widgets::page))
        .route("/widgets/save", post(pages::widgets::save))
        .route("/widgets/reorder/{id}/up", post(pages::widgets::reorder_up))
        .route(
            "/widgets/reorder/{id}/down",
            post(pages::widgets::reorder_down),
        )
        .route("/tools", get(pages::tools::page))
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
        .route(
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
        .route("/cec", get(pages::cec::page))
        .route("/cec/scan", post(pages::cec::scan))
        .route("/cec/device", post(pages::cec::device))
        .route("/cec/active-source", post(pages::cec::active_source))
        .route("/cec/power-on", post(pages::cec::power_on))
        .route("/cec/power-off", post(pages::cec::power_off))
        // Media: wallpapers (file uploads) + web-app registry. The upload route
        // raises the body limit past axum's 2 MB default; `MAX_UPLOAD_BYTES` is
        // still enforced per-file in the handler.
        .route("/media", get(pages::media::page))
        .route(
            "/media/wallpaper/upload",
            post(pages::media::upload).layer(DefaultBodyLimit::max(pages::media::MAX_UPLOAD_BYTES)),
        )
        .route("/media/wallpaper/select", post(pages::media::select))
        .route("/media/wallpaper/delete", post(pages::media::delete))
        .route("/media/wallpaper/file", get(pages::media::file))
        .route("/media/webapp/add", post(pages::media::webapp_add))
        .route("/media/webapp/remove", post(pages::media::webapp_remove))
        .route("/cec/test", post(pages::cec::test))
        .route("/cec/osd-name", post(pages::cec::save_osd_name))
        .route(
            "/cec/recover/restart-daemon",
            post(pages::cec::recover_restart_daemon),
        )
        .route("/logs", get(pages::logs::page))
        .route("/logs/view", get(pages::logs::view)) // htmx refresh partial
        .route("/dev", get(pages::dev::page))
        .route("/dev/screenshot", get(pages::dev::screenshot_png))
        .route(
            "/dev/screenshot/capture",
            post(pages::dev::screenshot_capture),
        )
        .route("/nav/daemon-status", get(pages::nav::daemon_status_dot))
        // The four auth-exempt routes (`auth::PUBLIC_PATHS`): the two
        // compiled-in static assets, plus the login form and its submission.
        .route("/login", get(pages::login::page))
        .route("/login", post(pages::login::submit))
        .route("/assets/htmx.min.js", get(assets::htmx_js))
        .route("/assets/style.css", get(assets::style_css));

    // S5 — the root-equivalent set. Registered ONLY under
    // `[panel].allow_dangerous = true`; otherwise these paths do not exist.
    // `POST /processes/restart/{key}` and `POST /cec/recover/restart-daemon`
    // are deliberately NOT here: restarting a unit is recovery, which is the
    // reason the panel exists.
    let app = if allow_dangerous {
        app.route("/dev/deploy", post(pages::dev::deploy))
            .route("/dev/build", post(pages::dev::build))
            .route("/dev/restart-daemon", post(pages::dev::restart_daemon))
            .route("/dev/restart-shell", post(pages::dev::restart_shell))
            .route("/dev/reboot", post(pages::dev::reboot))
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

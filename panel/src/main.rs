//! tv-shell-panel — a LAN-only, server-rendered (axum + askama + vendored
//! HTMX) web control panel for the tv-shell HTPC daemon.
//!
//! Three data-tier clients (`ipc` — the primary Unix-socket IPC tier,
//! `bridge` — the daemon's opt-in HTTP dev-ops bridge, `exec` — a direct-exec
//! recovery tier for when both of the above are down) under a two-level
//! navigation shell: a drawer of six subject groups, each with a sub-nav of
//! its pages (`docs/PANEL_IA.md`). Every page is fully implemented; phase 4
//! dissolved the last two grab-bag pages (Media, Tools) into the pages that
//! own their subjects, so each module under `pages` is one page with one
//! subject.
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
mod http;
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
        .route("/overview", get(pages::dashboard::page))
        .route("/overview/tiles", get(pages::dashboard::tiles)) // htmx poll partial
        .route(
            "/overview/updates-tile",
            get(pages::dashboard::updates_tile),
        ) // htmx poll partial, own slower interval
        // htmx poll partial, own 30s interval: one `systemctl show` per
        // `[panel].managed_units` entry, and that list is unbounded — see
        // `pages::dashboard::services_tile`.
        .route(
            "/overview/services-tile",
            get(pages::dashboard::services_tile),
        )
        .route("/system/services", get(pages::services::page))
        // A READ: `systemctl show` for any unit, either scope. A GET because
        // it mutates nothing — the asymmetry with the restart route below is
        // the whole point of the page (`docs/PANEL_IA.md` § Services).
        .route("/system/services/inspect", get(pages::services::inspect))
        .route(
            "/system/services/restart/{key}",
            post(pages::services::restart),
        )
        .route("/system/processes", get(pages::processes::page))
        .route("/system/updates", get(pages::updates::page))
        .route("/system/updates/refresh", post(pages::updates::refresh))
        .route("/system/updates/job", get(pages::updates::job))
        // The shell pane comes from the bridge and degrades inline; the daemon
        // pane is `journalctl` via direct exec. So this page reads logs with no
        // node at all and is NOT `Feature::Logs` (which describes the DAEMON's
        // own `GET /dev/logs` and is emitted only with a network bridge).
        .route("/system/logs", get(pages::logs::page))
        .route("/system/logs/view", get(pages::logs::view)) // htmx refresh partial
        .route("/dev/recovery", get(pages::dev::page))
        // Recovery, NOT part of the S5 set: these restart the same two systemd
        // units `POST /system/services/restart/{key}` restarts, so gating them while
        // that stays open would buy nothing.
        .route("/dev/restart-daemon", post(pages::dev::restart_daemon))
        .route("/dev/restart-shell", post(pages::dev::restart_shell))
        // Forwarding addresses for the pre-IA paths. Each redirect sits in the
        // SAME block as its target, so it can never outlive the page it points
        // at — see `pages::redirects`.
        .route("/dashboard", get(pages::redirects::dashboard))
        .route("/processes", get(pages::redirects::processes))
        .route("/logs", get(pages::redirects::logs))
        .route("/dev", get(pages::redirects::dev))
        .route("/nav/daemon-status", get(pages::nav::daemon_status_dot))
        // The four auth-exempt routes (`auth::PUBLIC_PATHS`): the two
        // compiled-in static assets, plus the login form and its submission.
        .route("/login", get(pages::login::page))
        .route("/login", post(pages::login::submit))
        .route("/assets/htmx.min.js", get(assets::htmx_js))
        .route("/assets/style.css", get(assets::style_css));

    // ── Node tier — registered iff the handshake succeeded ─────────────────
    //
    // These drive the IPC line protocol directly. They map to no single
    // `Feature` (the daemon does not declare "I answer commands"), so the
    // honest statement is exactly "these exist iff a node answered a
    // handshake". Phase 4 dissolved the Tools console that used to own all of
    // them into the pages that own their subjects — Devices ▸ Network,
    // Remote ▸ Navigation/Launcher, Dev ▸ Console — plus the two power probes
    // on Devices ▸ Display & Audio, whose PAGE is in the `settings_store`
    // block (the same one-capability-per-block reason as the CEC/Input saves
    // below; that page renders the probe buttons only under `Gate::Node`).
    //
    // The four Tools ▸ System probes were DELETED rather than moved: their
    // content is already on the Overview tiles.
    //
    // `GET /dev/console` is node tier while `POST /dev/console/raw` is in the
    // danger block: the page exists to explain itself, and renders no form
    // when the route it would post to is unregistered.
    let app = if caps.allows(Gate::Node) {
        app.route("/devices/network", get(pages::network::page))
            .route("/devices/network/status", post(pages::network::status))
            .route(
                "/devices/network/wifi-list",
                post(pages::network::wifi_list),
            )
            .route(
                "/devices/network/wifi-rescan",
                post(pages::network::wifi_rescan),
            )
            .route(
                "/devices/network/throughput",
                post(pages::network::throughput),
            )
            .route("/devices/network/ping", post(pages::network::ping))
            .route(
                "/devices/network/bt/power-status",
                post(pages::network::bt_power_status),
            )
            .route(
                "/devices/network/bt/power-on",
                post(pages::network::bt_power_on),
            )
            .route(
                "/devices/network/bt/power-off",
                post(pages::network::bt_power_off),
            )
            .route(
                "/devices/network/bt/scan-on",
                post(pages::network::bt_scan_on),
            )
            .route(
                "/devices/network/bt/scan-off",
                post(pages::network::bt_scan_off),
            )
            .route("/devices/network/bt/list", post(pages::network::bt_list))
            .route(
                "/devices/network/bt/action",
                post(pages::network::bt_action),
            )
            .route(
                "/devices/display-audio/power/can-suspend",
                post(pages::display_audio::power_can_suspend),
            )
            .route(
                "/devices/display-audio/power/battery",
                post(pages::display_audio::power_battery),
            )
            .route("/remote/navigation", get(pages::navigation::page))
            .route("/remote/navigation/intent", post(pages::navigation::intent))
            .route("/remote/navigation/key", post(pages::navigation::key))
            .route("/remote/launcher", get(pages::launcher::page))
            .route("/remote/launcher/list", post(pages::launcher::list_apps))
            .route("/remote/launcher/launch", post(pages::launcher::launch_app))
            .route(
                "/remote/launcher/recents",
                post(pages::launcher::get_recents),
            )
            .route("/dev/console", get(pages::console::page))
            .route("/tools", get(pages::redirects::tools))
    } else {
        app
    };

    // ── Capability tier — one block per declared `Feature` ─────────────────
    //
    // `Feature::Controllers` is emitted on any Linux daemon build (the
    // evdev/uinput runtime). The Tools console's duplicate pair of
    // controller-DB buttons used to sit here too; phase 4 DELETED them rather
    // than moving them — they were the same two commands this page already
    // owns (`/devices/controllers/controllerdb/{status,refresh}`), reached
    // from a second page.
    let app = if caps.allows(Gate::Controllers) {
        app.route("/devices/controllers", get(pages::controllers::page))
            .route("/controllers", get(pages::redirects::controllers))
            .route("/devices/controllers/grab", post(pages::controllers::grab))
            .route(
                "/devices/controllers/release",
                post(pages::controllers::release),
            )
            .route(
                "/devices/controllers/handoff",
                post(pages::controllers::handoff),
            )
            .route(
                "/devices/controllers/pad/battery",
                post(pages::controllers::pad_battery),
            )
            .route(
                "/devices/controllers/pad/rumble-status",
                post(pages::controllers::pad_rumble_status),
            )
            .route(
                "/devices/controllers/pad/rumble",
                post(pages::controllers::pad_rumble),
            )
            .route(
                "/devices/controllers/input-devices",
                post(pages::controllers::input_devices),
            )
            .route(
                "/devices/controllers/bindings/set",
                post(pages::controllers::bindings_set),
            )
            .route(
                "/devices/controllers/bindings/capture",
                post(pages::controllers::bindings_capture),
            )
            .route(
                "/devices/controllers/bindings/capture-cancel",
                post(pages::controllers::bindings_capture_cancel),
            )
            .route(
                "/devices/controllers/active-game/set",
                post(pages::controllers::active_game_set),
            )
            .route(
                "/devices/controllers/active-game/clear",
                post(pages::controllers::active_game_clear),
            )
            .route(
                "/devices/controllers/controllerdb/status",
                post(pages::controllers::controllerdb_status),
            )
            .route(
                "/devices/controllers/controllerdb/refresh",
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
    // `/system/services/restart/{key}`) are untouched, so nothing is lost when the
    // page is absent.
    let app = if caps.allows(Gate::Cec) {
        app.route("/devices/cec", get(pages::cec::page))
            .route("/cec", get(pages::redirects::cec))
            .route("/devices/cec/scan", post(pages::cec::scan))
            .route("/devices/cec/device", post(pages::cec::device))
            .route(
                "/devices/cec/active-source",
                post(pages::cec::active_source),
            )
            .route("/devices/cec/power-on", post(pages::cec::power_on))
            .route("/devices/cec/power-off", post(pages::cec::power_off))
            .route("/devices/cec/test", post(pages::cec::test))
            .route("/devices/cec/osd-name", post(pages::cec::save_osd_name))
            .route(
                "/devices/cec/recover/restart-daemon",
                post(pages::cec::recover_restart_daemon),
            )
    } else {
        app
    };

    // The per-widget `widgets.<id>` subtree, read and written over IPC.
    let app = if caps.allows(Gate::Widgets) {
        app.route("/shell/widgets", get(pages::widgets::page))
            .route("/widgets", get(pages::redirects::widgets))
            .route("/shell/widgets/save", post(pages::widgets::save))
            .route(
                "/shell/widgets/reorder/{id}/up",
                post(pages::widgets::reorder_up),
            )
            .route(
                "/shell/widgets/reorder/{id}/down",
                post(pages::widgets::reorder_down),
            )
    } else {
        app
    };

    // `get-config` / `set-config`, plus the wallpaper surface Shell ▸
    // Appearance absorbed from the dissolved Media page.
    //
    // The wallpaper routes are served out of the PANEL's own filesystem and
    // need no node, so they were recovery tier — deliberately not gated on
    // `Feature::Wallpapers`, which `daemon/src/ipc.rs::features()` never emits
    // (gating on it would delete a working page from every live node).
    // `Gate::SettingsStore` is a different claim: the daemon DOES emit
    // `settings_store`, and picking a wallpaper already required it (select
    // writes `wallpaperPath` through `set-config`).
    // Gating the whole wallpaper surface together is what lets the Shell group
    // vanish cleanly in recovery mode instead of rendering a one-page shell,
    // and it removes two entries from `tests::RECOVERY_TIER_MUTATING`.
    //
    // The accepted consequence, documented in `docs/PANEL.md`: **wallpaper
    // upload is no longer available with the daemon down.**
    //
    // The upload route raises the body limit past axum's 2 MB default;
    // `MAX_UPLOAD_BYTES` is still enforced per-file in the handler.
    // Two routes here belong to pages registered in OTHER blocks: the CEC
    // settings group (`/devices/cec/config`, whose page is in the `Gate::Cec`
    // block) and the Input settings group
    // (`/devices/controllers/settings/save`, whose page is in the
    // `Gate::Controllers` block). A block condition may name exactly one
    // capability — there is no two-capability AND — and `set-config` is the
    // capability these two actually need, so they sit here. The consequence is
    // that each can exist with no page in front of it, which is harmless and
    // already precedented by `/dev/console/raw`; the inverse — a page rendering a
    // form that posts to an unregistered route — is NOT harmless, so both
    // pages render their settings form only under
    // `caps.allows(Gate::SettingsStore)`.
    let app = if caps.allows(Gate::SettingsStore) {
        app.route("/shell/appearance", get(pages::appearance::page))
            .route("/shell/appearance/save", post(pages::appearance::save))
            .route("/shell/apps", get(pages::apps::page))
            .route("/shell/apps/save", post(pages::apps::save))
            .route("/shell/advanced", get(pages::advanced::page))
            .route("/shell/advanced/raw", post(pages::advanced::save_raw))
            .route("/devices/display-audio", get(pages::display_audio::page))
            .route(
                "/devices/display-audio/save",
                post(pages::display_audio::save),
            )
            .route("/devices/cec/config", post(pages::cec::save_config))
            .route(
                "/devices/controllers/settings/save",
                post(pages::controllers::save_settings),
            )
            .route(
                "/shell/appearance/wallpaper/upload",
                post(pages::appearance::upload)
                    .layer(DefaultBodyLimit::max(pages::appearance::MAX_UPLOAD_BYTES)),
            )
            .route(
                "/shell/appearance/wallpaper/delete",
                post(pages::appearance::delete),
            )
            .route(
                "/shell/appearance/wallpaper/file",
                get(pages::appearance::file),
            )
            .route(
                "/shell/appearance/wallpaper/select",
                post(pages::appearance::select),
            )
            .route("/settings", get(pages::redirects::settings))
            .route("/media", get(pages::redirects::media))
    } else {
        app
    };

    // The daemon-owned web-app registry (`webapp-add` / `webapp-remove`). The
    // Shell ▸ Apps page that renders these two forms is registered in the
    // `settings_store` block above, so it renders them only under
    // `Gate::WebApps` — otherwise a node declaring one capability and not the
    // other would be shown a form posting to an unregistered route.
    let app = if caps.allows(Gate::WebApps) {
        app.route("/shell/apps/webapp/add", post(pages::apps::webapp_add))
            .route(
                "/shell/apps/webapp/remove",
                post(pages::apps::webapp_remove),
            )
    } else {
        app
    };

    // The two capture routes proxy the daemon's bridge `/screenshot`; the third
    // is the page in front of them. The daemon emits
    // `Feature::Screenshot` on Linux with EITHER network bridge configured
    // (`[http]` or `[mcp]`), while this proxy speaks only to the HTTP one — so
    // an MCP-only node declares the capability and registers these routes while
    // the panel has no bridge to call. That is a handled degradation, not a
    // dangling route: `BridgeClient` with no base URL returns
    // `BridgeError::NotConfigured` and `pages::screenshot` renders the honest
    // "bridge not configured" message. Deliberately NOT additionally gated on
    // `http_bridge_base.is_some()`: the capability is the NODE's statement
    // about itself, and folding a local config check into it would make the
    // route set depend on two different kinds of fact. `screenshot.html` carries
    // `bridge_configured` for the UI half, and says so before the click rather
    // than after it.
    //
    // No htpc-1 impact either way — it sets `[http].bind`.
    //
    // `GET /dev/screenshot` is the PAGE as of phase 4 (it was the PNG proxy,
    // which moved to `/dev/screenshot/image` to free the path).
    let app = if caps.allows(Gate::Screenshot) {
        app.route("/dev/screenshot", get(pages::screenshot::page))
            .route("/dev/screenshot/image", get(pages::screenshot::image))
            .route("/dev/screenshot/capture", post(pages::screenshot::capture))
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
    // `POST /system/services/restart/{key}` and `POST /cec/recover/restart-daemon`
    // are all deliberately NOT here.
    //
    // Reboot/suspend and the pacman apply are the PANEL's own exec tier, so
    // they carry no capability gate.
    //
    // `/dev/console/raw` keeps none either, but the honest statement is
    // narrower than it looks: with the handshake failed, `/dev/console` — the
    // only UI that drives it — is gone, so what survives is a curl-only escape
    // hatch. It is kept ungated because `allow_dangerous` is already the
    // operator's explicit opt-in to an arbitrary-command surface and narrowing
    // it would not remove a capability lie (the route reports the node's own
    // error when the node is down). It is NOT kept because the UI still works,
    // which it does not.
    //
    // As of phase 4 every route in this block is under `/dev/` except the
    // pacman apply, which belongs beside the pending-package table and the
    // job poll on System ▸ Updates — see `docs/PANEL.md` § Dangerous actions,
    // and `tests::the_dangerous_set_is_the_dev_group_plus_the_updates_apply`.
    let app = if allow_dangerous {
        app.route("/dev/reboot", post(pages::dev::reboot))
            .route("/dev/suspend", post(pages::dev::suspend))
            .route("/system/updates/apply", post(pages::updates::apply))
            .route("/dev/console/raw", post(pages::console::raw))
    } else {
        app
    };

    app.route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ))
    .with_state(state)
}

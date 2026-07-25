//! Unix-socket IPC server: accepts connections, parses one command per line,
//! and dispatches to the input runtime over the control channel. The
//! `subscribe` command upgrades the connection into an event stream fed by the
//! broadcast bus.
//!
//! Cross-platform (only `tokio`/`tokio-util`), so it compiles and can be tested
//! on non-Linux hosts even though the input runtime is Linux-only.

use crate::protocol::{self, Command, Event};
use crate::state::Control;
use crate::{
    apps, bridge_core, config, controllerdb, health, moonlight, netinfo, notifications, plex,
    recents, steam, system, webapps, wol,
};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use std::os::unix::fs::PermissionsExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::codec::{Framed, LinesCodec};

#[cfg(target_os = "linux")]
use crate::bluetooth::BtReq;
#[cfg(all(target_os = "linux", feature = "cec"))]
use crate::cec::CecReq;
#[cfg(target_os = "linux")]
use crate::hyprland::HyprReq;
#[cfg(target_os = "linux")]
use crate::network::NetReq;
#[cfg(target_os = "linux")]
use crate::power::PowerReq;

/// Shared mutable state for the controller DB (updated on refresh).
///
/// Wrapped in `Arc<tokio::sync::RwLock<_>>` so multiple IPC connections can
/// read the status concurrently; the refresh path holds a write lock for the
/// swap.
pub type SharedControllerDbState = std::sync::Arc<tokio::sync::RwLock<ControllerDbState>>;

/// Current controller DB state, carried by the IPC layer for `controllerdb-*`
/// commands.
#[derive(Debug, Clone)]
pub struct ControllerDbState {
    pub source: String,
    pub entry_count: usize,
    pub last_downloaded: u64,
    pub last_error: Option<String>,
}

impl ControllerDbState {
    pub fn initial() -> Self {
        // Populate the initial state from the merged DB at startup. This reads
        // only the bundled baseline, any already-present on-disk cache, and the
        // env override — it does NOT perform an upstream network fetch. A fresh
        // install therefore reports the bundled baseline until an explicit
        // `controllerdb-refresh` runs.
        let (db, source) = controllerdb::load_merged_db();
        let last_downloaded = controllerdb::read_last_downloaded();
        ControllerDbState {
            source,
            entry_count: db.len(),
            last_downloaded,
            last_error: None,
        }
    }
}

/// Senders to the Phase 3 D-Bus actors (Bluetooth, Network, Power).
///
/// These actors are Linux-only, so their request types only exist on Linux. To
/// keep [`serve`] callable with one signature on every platform, this struct
/// carries the `Option<mpsc::Sender<…>>` fields on Linux and is empty elsewhere.
/// `None` (or non-Linux) makes the corresponding commands reply
/// `error:unsupported on this platform`.
#[derive(Clone, Default)]
pub struct DbusSenders {
    #[cfg(target_os = "linux")]
    pub bt: Option<mpsc::Sender<BtReq>>,
    #[cfg(target_os = "linux")]
    pub net: Option<mpsc::Sender<NetReq>>,
    #[cfg(target_os = "linux")]
    pub power: Option<mpsc::Sender<PowerReq>>,
    #[cfg(target_os = "linux")]
    pub hypr: Option<mpsc::Sender<HyprReq>>,
    /// HDMI-CEC actor (#94). Gated on BOTH linux AND the `cec` feature: the
    /// field is absent from the default Linux build (no libcec-sys), so the
    /// `dispatch_dbus` CEC arms that read it are gated identically.
    #[cfg(all(target_os = "linux", feature = "cec"))]
    pub cec: Option<mpsc::Sender<CecReq>>,
}

/// Bind the socket (removing any stale file), chmod 0o600, and serve until the
/// process exits.
pub async fn serve(
    sock_path: String,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
    dbus: DbusSenders,
    db_state: SharedControllerDbState,
) -> Result<()> {
    let _ = std::fs::remove_file(&sock_path);
    // Create the socket private from the instant it exists. Binding then
    // chmod'ing leaves a TOCTOU window where the socket carries the (umask-
    // dependent, possibly world-accessible) default perms and another local
    // process could connect. Tightening umask to 0o177 means the kernel creates
    // the socket node 0o600 atomically at bind; we restore the prior umask right
    // after. The explicit set_permissions below is then a belt-and-suspenders
    // assertion (umask can only clear bits, never guarantee an exact mode).
    let prev_umask = unsafe { libc::umask(0o177) };
    let bind_result = UnixListener::bind(&sock_path);
    unsafe {
        libc::umask(prev_umask);
    }
    let listener = bind_result.with_context(|| format!("binding unix socket at {sock_path}"))?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0o600 on {sock_path}"))?;
    tracing::info!("Listening on {sock_path}");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let control_tx = control_tx.clone();
                let events_tx = events_tx.clone();
                let dbus = dbus.clone();
                let db_state = db_state.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_client(stream, control_tx, events_tx, dbus, db_state).await
                    {
                        tracing::debug!("client connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("accept error: {e}");
            }
        }
    }
}

/// Send a control request and await the runtime's response line.
async fn request<F>(control_tx: &mpsc::Sender<Control>, make: F) -> Option<String>
where
    F: FnOnce(oneshot::Sender<String>) -> Control,
{
    let (reply_tx, reply_rx) = oneshot::channel();
    control_tx.send(make(reply_tx)).await.ok()?;
    reply_rx.await.ok()
}

/// Send a request to one of the Phase 3 D-Bus actors and await its reply line.
///
/// `tx` is `None` when the actor isn't wired (non-Linux build, or the actor
/// failed to start) — in that case the command replies
/// `error:unsupported on this platform`. A closed channel / dropped reply also
/// degrades to the same unsupported reply so a missing actor never wedges a
/// client. `make` builds the request enum from a fresh `oneshot` sender.
#[cfg(target_os = "linux")]
async fn request_dbus<T, F>(tx: &Option<mpsc::Sender<T>>, make: F) -> String
where
    F: FnOnce(oneshot::Sender<String>) -> T,
{
    let Some(tx) = tx else {
        return protocol::resp_unsupported();
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    if tx.send(make(reply_tx)).await.is_err() {
        return protocol::resp_unsupported();
    }
    reply_rx
        .await
        .unwrap_or_else(|_| protocol::resp_unsupported())
}

async fn handle_client(
    stream: UnixStream,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
    dbus: DbusSenders,
    db_state: SharedControllerDbState,
) -> Result<()> {
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(4096));

    while let Some(line) = framed.next().await {
        let line = line.context("reading command line")?;
        match Command::parse(&line) {
            Command::Subscribe => {
                framed.send(protocol::resp_subscribed()).await?;
                return stream_events(framed, events_tx).await;
            }
            cmd => {
                let response = dispatch(&control_tx, &events_tx, &dbus, &db_state, cmd).await;
                framed.send(response).await?;
            }
        }
    }
    Ok(())
}

/// Handle the stateless Phase 2 commands (app discovery + config/recents I/O).
/// These never touch the input runtime, so they are served directly here.
/// Filesystem work runs on a blocking thread so the IPC reactor isn't stalled.
/// Returns `None` for commands that aren't stateless (caller falls through to
/// the input-runtime dispatch).
async fn dispatch_stateless(cmd: &Command, db_state: &SharedControllerDbState) -> Option<String> {
    match cmd {
        Command::ListApps => Some(spawn_blocking_string(apps::list_apps_json).await),
        Command::GetConfig => {
            Some(spawn_blocking_string(|| config::load_config_json(&config::settings_path())).await)
        }
        Command::GetRecents => {
            Some(spawn_blocking_string(|| recents::load_recents(&recents::recents_path())).await)
        }
        Command::GetNotifications => Some(
            spawn_blocking_string(|| {
                notifications::load_notifications(&notifications::notifications_path())
            })
            .await,
        ),
        Command::RecordNotification(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    let mut entry: notifications::Notification = match parse_body(&body) {
                        Ok(v) => v,
                        Err(resp) => return resp,
                    };
                    // Honor a client-supplied creation time; fall back to
                    // the daemon clock only when none was provided.
                    if entry.time == 0.0 {
                        entry.time = notifications::now_unix_secs();
                    }
                    match notifications::record_notification(
                        &notifications::notifications_path(),
                        entry,
                    ) {
                        Ok(()) => protocol::resp_ok(),
                        Err(e) => protocol::resp_error(&format!("record-notification failed: {e}")),
                    }
                })
                .await,
            )
        }
        Command::RecordNotificationUsage => Some(protocol::resp_record_notification_usage()),
        Command::SetNotifications(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    let entries: Vec<notifications::Notification> = match parse_body(&body) {
                        Ok(v) => v,
                        Err(resp) => return resp,
                    };
                    match notifications::set_notifications(
                        &notifications::notifications_path(),
                        entries,
                    ) {
                        Ok(()) => protocol::resp_ok(),
                        Err(e) => protocol::resp_error(&format!("set-notifications failed: {e}")),
                    }
                })
                .await,
            )
        }
        Command::SetNotificationsUsage => Some(protocol::resp_set_notifications_usage()),
        Command::SetConfig(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    let updates: serde_json::Value = match parse_body(&body) {
                        Ok(v) => v,
                        Err(resp) => return resp,
                    };
                    if !updates.is_object() {
                        return protocol::resp_error("set-config body must be a JSON object");
                    }
                    match config::set_config(&config::settings_path(), &updates) {
                        Ok(merged) => merged,
                        Err(e) => protocol::resp_error(&format!("set-config failed: {e}")),
                    }
                })
                .await,
            )
        }
        Command::SetConfigUsage => Some(protocol::resp_set_config_usage()),
        // Web-app registry (#187 P1/P3). All three are stateless + filesystem
        // bound, so they run on the blocking pool like set-config rather than
        // through an actor. The daemon is the sole writer of both the registry
        // and the generated .desktop files.
        Command::WebappList => Some(
            spawn_blocking_string(move || {
                let apps = webapps::list(&config::settings_path());
                serde_json::to_string(&webapps::registry_json(&apps))
                    .unwrap_or_else(|_| "[]".to_string())
            })
            .await,
        ),
        Command::WebappAdd(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    #[derive(serde::Deserialize)]
                    struct AddReq {
                        name: String,
                        url: String,
                    }
                    let req: AddReq = match parse_body(&body) {
                        Ok(v) => v,
                        Err(resp) => return resp,
                    };
                    match webapps::add(&config::settings_path(), &req.name, &req.url) {
                        Ok(app) => serde_json::to_string(&app)
                            .unwrap_or_else(|_| protocol::resp_error("serialize failed")),
                        Err(e) => protocol::resp_error(&format!("webapp-add failed: {e}")),
                    }
                })
                .await,
            )
        }
        Command::WebappAddUsage => Some(protocol::resp_webapp_add_usage()),
        Command::WebappRemove(id) => {
            let id = id.clone();
            Some(
                spawn_blocking_string(move || {
                    match webapps::remove(&config::settings_path(), id.trim()) {
                        Ok(()) => protocol::resp_ok(),
                        Err(e) => protocol::resp_error(&format!("webapp-remove failed: {e}")),
                    }
                })
                .await,
            )
        }
        Command::WebappRemoveUsage => Some(protocol::resp_webapp_remove_usage()),
        Command::RecordLaunch(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    let mut entry: recents::Recent = match parse_body(&body) {
                        Ok(v) => v,
                        Err(resp) => return resp,
                    };
                    entry.time = recents::now_unix_secs();
                    match recents::record_launch(&recents::recents_path(), entry) {
                        Ok(()) => protocol::resp_ok(),
                        Err(e) => protocol::resp_error(&format!("record-launch failed: {e}")),
                    }
                })
                .await,
            )
        }
        Command::RecordLaunchUsage => Some(protocol::resp_record_launch_usage()),
        // Phase 4 Sunshine session detection. Stateless and cross-platform
        // (`reqwest` runs everywhere), so it's served here like `list-apps`
        // rather than via a Linux-only actor. Missing/empty `<host> <port>` is a
        // usage error (`SunshineStatusUsage`); a reachable-but-failing fetch
        // (network/TLS/parse error) degrades to the offline JSON
        // (`{"online":false,...}`).
        Command::SunshineStatus { host, port } => {
            Some(health::handle_sunshine_status(host, port).await)
        }
        Command::SunshineStatusUsage => Some(protocol::resp_sunshine_status_usage()),
        // Wake-on-LAN: send a magic packet to a streaming host whose Steam row is
        // showing the "Wake host" card. Stateless + cross-platform (UDP
        // broadcast). Missing host routes to `WolUsage`; an unresolvable MAC
        // degrades to `{"status":"error","reason":"no-mac"}`.
        Command::Wol { host } => Some(wol::handle_wol(host).await),
        Command::WolUsage => Some(protocol::resp_wol_usage()),
        // Network reads for the QML shell (#M3): per-interface throughput
        // counters (sysfs) and a bounded reachability/latency ping. Stateless +
        // fail-soft like sunshine-status/wol — not routed through the NM actor.
        Command::NetThroughput { iface } => {
            Some(netinfo::handle_net_throughput(iface.clone()).await)
        }
        Command::NetThroughputUsage => Some(protocol::resp_net_throughput_usage()),
        Command::NetPing { host, count } => {
            Some(netinfo::handle_net_ping(host.clone(), *count).await)
        }
        Command::NetPingUsage => Some(protocol::resp_net_ping_usage()),
        // Plex hubs (On Deck + Recently Added) for the home-screen widget.
        // Stateless + cross-platform like `sunshine-status`; the server URL and
        // token come from the daemon env. Unconfigured/unreachable degrades to
        // `{"enabled":false,…}` / empty hubs.
        Command::PlexHubs => Some(plex::handle_plex_hubs().await),
        // Steam library + launch for the home-screen Steam widget. Stateless +
        // cross-platform like `plex-hubs`; the tv-shell-host base URL + token
        // come from the daemon env. Unconfigured ⇒ `{"status":"disabled",…}`.
        Command::SteamLibrary => Some(steam::handle_steam_library().await),
        Command::SteamLaunch(appid) => Some(steam::handle_steam_launch(*appid).await),
        Command::SteamLaunchUsage => Some(protocol::resp_steam_launch_usage()),
        // Steam Big Picture HOME — reset the host's BPM to its home screen (no
        // game). Bare command; same stateless/cross-platform category as
        // steam-launch. Unconfigured/unreachable degrades to a JSON error status.
        Command::SteamBigPicture => Some(steam::handle_steam_bigpicture().await),
        // Steam graceful quit — terminate the running game on the host (SIGTERM to
        // its process group). Same stateless/cross-platform category as
        // steam-launch. Unconfigured/unreachable degrades to a JSON error status.
        Command::SteamQuit(appid) => Some(steam::handle_steam_quit(*appid).await),
        Command::SteamQuitUsage => Some(protocol::resp_steam_quit_usage()),
        // Steam host suspend — put the gaming PC to sleep. Bare command, same
        // stateless/cross-platform category as steam-quit. The host may REFUSE
        // (running game / live stream); that reply carries the reason and
        // `refused: true` rather than being flattened into a generic error.
        Command::SteamSuspend => Some(steam::handle_steam_suspend().await),
        // Steam sidecar roster + active-host selection (the widget's server
        // picker). `steam-hosts` is a cheap config read; `steam-set-host`
        // persists to settings.json, so its blocking file I/O runs off the
        // reactor via spawn_blocking (mirroring set-config).
        Command::SteamHosts => Some(steam::handle_steam_hosts()),
        Command::SteamSetHost(name) => {
            let name = name.clone();
            Some(spawn_blocking_string(move || steam::handle_steam_set_host(&name)).await)
        }
        Command::SteamSetHostUsage => Some(protocol::resp_steam_set_host_usage()),
        // Moonlight local-config "forget" — creds-free client-side unpair.
        // Stateless and cross-platform (just edits Moonlight.conf). Missing host
        // routes to `MoonlightForgetUsage`. Runs the blocking file edit off the
        // reactor via spawn_blocking.
        Command::MoonlightForget(host) => {
            let host = host.clone();
            Some(spawn_blocking_string(move || moonlight::handle_forget(&host)).await)
        }
        Command::MoonlightForgetUsage => Some(protocol::resp_moonlight_forget_usage()),

        // --- #159: controllerdb-status / controllerdb-refresh ---
        Command::ControllerDbStatus => {
            let state = db_state.read().await;
            // Build the status directly from stored state; avoids constructing a
            // proxy ControllerDb whose len() is always 0.
            let status = controllerdb::DbStatus::from_state(&state);
            Some(serde_json::to_string(&status).expect("controllerdb status serialize"))
        }
        // ControllerDbRefresh is NOT stateless — it must send Control::ControllerDbRefreshed
        // to the input runtime after a successful fetch so the DB is hot-swapped.
        // Return None here so dispatch() handles it with access to control_tx.
        Command::ControllerDbRefresh => None,

        // --- #160: pad-battery / pad-rumble-status ---
        Command::PadBatteryUsage => Some(protocol::resp_pad_battery_usage()),
        Command::PadRumbleStatusUsage => Some(protocol::resp_pad_rumble_status_usage()),

        // --- #164: sys-status / storage-status ---
        Command::SysStatus => Some(spawn_blocking_string(system::sys_status_json).await),
        Command::StorageStatus => Some(spawn_blocking_string(system::storage_status_json).await),
        Command::SysMetrics => Some(spawn_blocking_string(system::sys_metrics_json).await),
        // Build identity: resolved live (async git), so served directly rather
        // than via the blocking pool.
        Command::BuildInfo => Some(bridge_core::build_info_json().await),

        _ => None,
    }
}

/// Parse a JSON request body into `T`, returning the parsed value or the
/// already-formatted `error:invalid JSON: <e>` reply string on failure.
///
/// Call sites use
/// `let x: T = match parse_body(&body) { Ok(v) => v, Err(resp) => return resp };`
/// inside a `spawn_blocking_string` closure, so the `Err` arm short-circuits the
/// closure with the exact wire text the shell expects.
fn parse_body<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str::<T>(body).map_err(|e| protocol::resp_error(&format!("invalid JSON: {e}")))
}

/// Run a blocking closure that returns the response string on tokio's blocking
/// pool, falling back to an error reply if the task is cancelled/panics.
async fn spawn_blocking_string<F>(f: F) -> String
where
    F: FnOnce() -> String + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| protocol::resp_error(&format!("internal task failed: {e}")))
}

/// Resolve a non-subscribe command to its response line.
async fn dispatch(
    control_tx: &mpsc::Sender<Control>,
    events_tx: &broadcast::Sender<Event>,
    dbus: &DbusSenders,
    db_state: &SharedControllerDbState,
    cmd: Command,
) -> String {
    if let Some(resp) = dispatch_stateless(&cmd, db_state).await {
        // A successful set-config mutated settings.json; the input runtime caches
        // some of those keys (rumbleEnabled, #108), so nudge it to refresh. Fire
        // and forget — set-config's reply is already resolved and must not block
        // on the input runtime. Only on success (the reply isn't an error line).
        if matches!(cmd, Command::SetConfig(_)) && !resp.starts_with("error:") {
            let _ = control_tx.send(Control::ConfigChanged).await;
        }
        // A successful steam-set-host changed which sidecar is active. Re-probe it
        // NOW and broadcast a `health:steam` event so subscribed widgets (the
        // home Steam view's ServiceMonitor) re-fetch `steam-library` immediately
        // — the newly-selected host's library + reported IP appear at once rather
        // than after the next 10s/30s poll. Fire-and-forget: the set reply is
        // already resolved and must not block on the probe.
        if matches!(cmd, Command::SteamSetHost(_)) && !resp.starts_with("error:") {
            // Proactive Wake-on-LAN for the newly-selected host, gated on
            // [steam].wake_active_host_on_start (default OFF). Spawned
            // fire-and-forget: the reply above is already resolved and a wake
            // must never delay or fail it. Runs BEFORE the probe is awaited so a
            // sleeping host has the packet in flight while we check it.
            tokio::spawn(crate::wol::wake_active_host_if_enabled("steam-set-host"));
            let status = crate::service_health::probe_steam().await;
            let _ = events_tx.send(Event::ServiceHealth(crate::service_health::health_json(
                "steam", status,
            )));
        }
        return resp;
    }
    if let Some(resp) = dispatch_dbus(dbus, &cmd).await {
        return resp;
    }
    // The input-runtime arms below resolve to `None` (via `request`) exactly when
    // the control channel send fails or the oneshot reply is dropped — both mean
    // the input runtime is gone (panic-exhausted supervisor, Concern 1). Surface
    // that as a distinct, actionable error instead of `unknown` so a dead backend
    // is not confused with a client typo. (The standalone `return
    // protocol::resp_unknown()` arms below are genuine unknown-command paths and
    // keep replying `unknown`.)
    let fallback = protocol::resp_error("input-runtime-down");
    match cmd {
        Command::Grab => request(control_tx, Control::Grab).await,
        Command::Release => request(control_tx, Control::Release).await,
        Command::Handoff => request(control_tx, Control::Handoff).await,
        Command::Status => request(control_tx, Control::Status).await,
        Command::GetBindings => request(control_tx, Control::GetBindings).await,
        Command::SetBinding { action, button } => {
            request(control_tx, move |reply| Control::SetBinding {
                action,
                button,
                reply,
            })
            .await
        }
        Command::CaptureNext => request(control_tx, Control::CaptureNext).await,
        Command::CaptureCancel => request(control_tx, Control::CaptureCancel).await,
        Command::GetPads => request(control_tx, Control::GetPads).await,
        Command::ListInputDevices => {
            request(control_tx, Control::ListInputDevices).await
        }
        Command::Intent(name) => {
            request(control_tx, move |reply| Control::Intent { name, reply }).await
        }
        Command::Rumble { id, ms } => {
            request(control_tx, move |reply| Control::Rumble { id, ms, reply }).await
        }
        Command::Key(name) => {
            request(control_tx, move |reply| Control::Key { name, reply }).await
        }
        Command::SetActiveGame(id) => {
            request(control_tx, move |reply| Control::SetActiveGame {
                id: Some(id),
                reply,
            })
            .await
        }
        Command::SetActiveGameClear => {
            request(control_tx, move |reply| Control::SetActiveGame {
                id: None,
                reply,
            })
            .await
        }
        Command::ShellFocus(on) => {
            request(control_tx, move |reply| Control::ShellFocus { on, reply }).await
        }
        Command::OverlayFocus(on) => {
            request(control_tx, move |reply| Control::OverlayFocus { on, reply }).await
        }
        // --- #159: controllerdb-refresh — needs control_tx to hot-swap the runtime's DB ---
        Command::ControllerDbRefresh => {
            let db_state = db_state.clone();
            match controllerdb::refresh().await {
                Ok(text) => {
                    let (new_db, new_source) = controllerdb::load_merged_db();
                    let new_ts = controllerdb::read_last_downloaded();
                    {
                        let mut state = db_state.write().await;
                        state.source = new_source.clone();
                        state.entry_count = new_db.len();
                        state.last_downloaded = new_ts;
                        state.last_error = None;
                    }
                    tracing::info!(
                        "controllerdb: refreshed {} entries from upstream ({})",
                        new_db.len(),
                        text.lines().count()
                    );
                    // Notify the input runtime to hot-swap the DB without a restart.
                    let _ = request(control_tx, |reply| Control::ControllerDbRefreshed { reply }).await;
                    let state = db_state.read().await;
                    let status = controllerdb::DbStatus::from_state(&state);
                    Some(serde_json::to_string(&status).expect("controllerdb status serialize"))
                }
                Err(e) => {
                    tracing::warn!("controllerdb refresh failed: {e}");
                    let mut state = db_state.write().await;
                    state.last_error = Some(e);
                    let status = controllerdb::DbStatus::from_state(&state);
                    Some(serde_json::to_string(&status).expect("controllerdb status serialize"))
                }
            }
        }

        // --- #160: per-pad battery + rumble status ---
        Command::PadBatteryQuery(id) => {
            request(control_tx, move |reply| Control::PadBatteryQuery { id, reply }).await
        }
        Command::PadRumbleStatus(id) => {
            request(control_tx, move |reply| Control::PadRumbleStatus { id, reply }).await
        }
        // Handled without a round-trip to the runtime:
        Command::IntentUsage => return protocol::resp_intent_usage(),
        Command::ShellFocusUsage => return protocol::resp_shell_focus_usage(),
        Command::OverlayFocusUsage => return protocol::resp_overlay_focus_usage(),
        Command::RumbleUsage => return protocol::resp_rumble_usage(),
        Command::KeyUsage => return protocol::resp_key_usage(),
        Command::SetBindingUsage => return protocol::resp_set_binding_usage(),
        Command::PadBatteryUsage => return protocol::resp_pad_battery_usage(),
        Command::PadRumbleStatusUsage => return protocol::resp_pad_rumble_status_usage(),
        Command::Unknown => return protocol::resp_unknown(),
        // Subscribe is handled by the caller before dispatch.
        Command::Subscribe => return protocol::resp_unknown(),
        // Stateless Phase 2 commands are consumed by `dispatch_stateless`
        // above, which returns early; they never reach this match.
        Command::ListApps
        | Command::GetConfig
        | Command::SetConfig(_)
        | Command::SetConfigUsage
        | Command::RecordLaunch(_)
        | Command::RecordLaunchUsage
        | Command::GetRecents
        // Web-app registry commands are stateless (consumed by `dispatch_stateless`).
        | Command::WebappList
        | Command::WebappAdd(_)
        | Command::WebappAddUsage
        | Command::WebappRemove(_)
        | Command::WebappRemoveUsage
        // Notification commands are stateless (consumed by `dispatch_stateless`).
        | Command::GetNotifications
        | Command::RecordNotification(_)
        | Command::RecordNotificationUsage
        | Command::SetNotifications(_)
        | Command::SetNotificationsUsage
        // Phase 4 Sunshine is stateless (consumed by `dispatch_stateless`).
        | Command::SunshineStatus { .. }
        | Command::SunshineStatusUsage
        // Wake-on-LAN is stateless (consumed by `dispatch_stateless`).
        | Command::Wol { .. }
        | Command::WolUsage
        // Network reads (throughput/ping) are stateless (consumed by `dispatch_stateless`).
        | Command::NetThroughput { .. }
        | Command::NetThroughputUsage
        | Command::NetPing { .. }
        | Command::NetPingUsage
        // Plex hubs is stateless (consumed by `dispatch_stateless`).
        | Command::PlexHubs
        // Steam library/launch are stateless (consumed by `dispatch_stateless`).
        | Command::SteamLibrary
        | Command::SteamLaunch(_)
        | Command::SteamLaunchUsage
        | Command::SteamBigPicture
        | Command::SteamQuit(_)
        | Command::SteamQuitUsage
        | Command::SteamSuspend
        | Command::SteamHosts
        | Command::SteamSetHost(_)
        | Command::SteamSetHostUsage
        // Moonlight forget is stateless (consumed by `dispatch_stateless`).
        | Command::MoonlightForget(_)
        | Command::MoonlightForgetUsage
        // Controller DB status is stateless (consumed by `dispatch_stateless`).
        // ControllerDbRefresh is handled above with control_tx (hot-swap).
        | Command::ControllerDbStatus
        // System/storage status commands are stateless (#164, #235).
        | Command::SysStatus
        | Command::StorageStatus
        | Command::SysMetrics
        | Command::BuildInfo => return protocol::resp_unknown(),
        // Phase 3 D-Bus commands are consumed by `dispatch_dbus` above (which
        // returns early); they never reach this match. The MAC-usage variant is
        // a stateless error reply handled there too.
        Command::BtPowerStatus
        | Command::BtPowerOn
        | Command::BtPowerOff
        | Command::BtScanOn
        | Command::BtScanOff
        | Command::BtList
        | Command::BtConnect(_)
        | Command::BtDisconnect(_)
        | Command::BtPair(_)
        | Command::BtTrust(_)
        | Command::BtMacUsage(_)
        | Command::NetStatus
        | Command::NetWifiList
        | Command::NetWifiRescan
        | Command::PowerCanSuspend
        | Command::PowerSuspend
        | Command::PowerBattery
        // Phase 4 Hyprland commands are consumed by `dispatch_dbus` above.
        | Command::HyprActive
        | Command::HyprClients
        | Command::HyprMonitors
        // CEC commands fall through to dispatch_dbus (unsupported response) — not reached.
        | Command::CecScan
        | Command::CecDevice(_)
        | Command::CecPowerOn(_)
        | Command::CecPowerOff(_)
        | Command::CecActiveSource
        | Command::CecHealth
        | Command::CecTest
        | Command::CecAddrUsage(_) => return protocol::resp_unknown(),
    }
    .unwrap_or(fallback)
}

/// Route the Phase 3 Bluetooth/Network/Power commands to their D-Bus actors.
///
/// Returns `Some(reply)` for any Phase 3 command (including the MAC-usage error,
/// which is stateless), or `None` for everything else so the caller falls
/// through to the input-runtime dispatch. The `BtMacUsage` arm is handled here
/// regardless of platform.
///
/// On non-Linux builds the D-Bus actors don't exist, so every routed command
/// (except the usage error) replies `error:unsupported on this platform`.
#[cfg(target_os = "linux")]
async fn dispatch_dbus(dbus: &DbusSenders, cmd: &Command) -> Option<String> {
    let resp = match cmd {
        Command::BtPowerStatus => request_dbus(&dbus.bt, BtReq::PowerStatus).await,
        Command::BtPowerOn => request_dbus(&dbus.bt, BtReq::PowerOn).await,
        Command::BtPowerOff => request_dbus(&dbus.bt, BtReq::PowerOff).await,
        Command::BtScanOn => request_dbus(&dbus.bt, BtReq::ScanOn).await,
        Command::BtScanOff => request_dbus(&dbus.bt, BtReq::ScanOff).await,
        Command::BtList => request_dbus(&dbus.bt, BtReq::List).await,
        Command::BtConnect(mac) => {
            let mac = mac.clone();
            request_dbus(&dbus.bt, move |reply| BtReq::Connect { mac, reply }).await
        }
        Command::BtDisconnect(mac) => {
            let mac = mac.clone();
            request_dbus(&dbus.bt, move |reply| BtReq::Disconnect { mac, reply }).await
        }
        Command::BtPair(mac) => {
            let mac = mac.clone();
            request_dbus(&dbus.bt, move |reply| BtReq::Pair { mac, reply }).await
        }
        Command::BtTrust(mac) => {
            let mac = mac.clone();
            request_dbus(&dbus.bt, move |reply| BtReq::Trust { mac, reply }).await
        }
        Command::BtMacUsage(which) => protocol::resp_bt_mac_usage(which),
        Command::NetStatus => request_dbus(&dbus.net, NetReq::Status).await,
        Command::NetWifiList => request_dbus(&dbus.net, NetReq::WifiList).await,
        Command::NetWifiRescan => request_dbus(&dbus.net, NetReq::WifiRescan).await,
        Command::PowerCanSuspend => request_dbus(&dbus.power, PowerReq::CanSuspend).await,
        Command::PowerSuspend => request_dbus(&dbus.power, PowerReq::Suspend).await,
        Command::PowerBattery => request_dbus(&dbus.power, PowerReq::Battery).await,
        Command::HyprActive => request_dbus(&dbus.hypr, HyprReq::Active).await,
        Command::HyprClients => request_dbus(&dbus.hypr, HyprReq::Clients).await,
        Command::HyprMonitors => request_dbus(&dbus.hypr, HyprReq::Monitors).await,
        // HDMI-CEC (#94): the `cec` field exists only under `feature = "cec"`,
        // so the live arms are feature-gated; the default Linux build keeps the
        // `resp_unsupported()` fallthrough (libcec isn't linked there).
        #[cfg(feature = "cec")]
        Command::CecScan => request_dbus(&dbus.cec, CecReq::Scan).await,
        #[cfg(feature = "cec")]
        Command::CecDevice(addr) => {
            let addr = addr.clone();
            request_dbus(&dbus.cec, move |reply| CecReq::Device { addr, reply }).await
        }
        #[cfg(feature = "cec")]
        Command::CecPowerOn(addr) => {
            let addr = addr.clone();
            request_dbus(&dbus.cec, move |reply| CecReq::PowerOn { addr, reply }).await
        }
        #[cfg(feature = "cec")]
        Command::CecPowerOff(addr) => {
            let addr = addr.clone();
            request_dbus(&dbus.cec, move |reply| CecReq::PowerOff { addr, reply }).await
        }
        #[cfg(feature = "cec")]
        Command::CecActiveSource => request_dbus(&dbus.cec, CecReq::ActiveSource).await,
        #[cfg(feature = "cec")]
        Command::CecHealth => request_dbus(&dbus.cec, CecReq::Health).await,
        #[cfg(feature = "cec")]
        Command::CecTest => request_dbus(&dbus.cec, CecReq::Test).await,
        // Without the `cec` feature, libcec isn't linked at all — health/test
        // report the structured `no_libcec` unavailable JSON (so the AV Control
        // page shows an accurate message) while the action/read commands keep the
        // bare `resp_unsupported()`.
        #[cfg(not(feature = "cec"))]
        Command::CecHealth | Command::CecTest => protocol::cec_unavailable_json("no_libcec", 0),
        #[cfg(not(feature = "cec"))]
        Command::CecScan
        | Command::CecDevice(_)
        | Command::CecPowerOn(_)
        | Command::CecPowerOff(_)
        | Command::CecActiveSource => protocol::resp_unsupported(),
        Command::CecAddrUsage(which) => protocol::resp_cec_addr_usage(which),
        _ => return None,
    };
    Some(resp)
}

/// Non-Linux stub: the D-Bus actors don't exist, so every Phase 3 command
/// (other than the stateless MAC-usage error) is unsupported. Keeps `dispatch`
/// and the protocol parsing/tests cross-platform.
#[cfg(not(target_os = "linux"))]
async fn dispatch_dbus(_dbus: &DbusSenders, cmd: &Command) -> Option<String> {
    let resp = match cmd {
        Command::BtMacUsage(which) => protocol::resp_bt_mac_usage(which),
        Command::CecAddrUsage(which) => protocol::resp_cec_addr_usage(which),
        // Non-Linux: libcec doesn't exist — health/test report the structured
        // `no_libcec` unavailable JSON (so the AV Control page shows an accurate
        // message) while the action/read commands keep the bare unsupported line.
        Command::CecHealth | Command::CecTest => protocol::cec_unavailable_json("no_libcec", 0),
        Command::BtPowerStatus
        | Command::BtPowerOn
        | Command::BtPowerOff
        | Command::BtScanOn
        | Command::BtScanOff
        | Command::BtList
        | Command::BtConnect(_)
        | Command::BtDisconnect(_)
        | Command::BtPair(_)
        | Command::BtTrust(_)
        | Command::NetStatus
        | Command::NetWifiList
        | Command::NetWifiRescan
        | Command::PowerCanSuspend
        | Command::PowerSuspend
        | Command::PowerBattery
        | Command::HyprActive
        | Command::HyprClients
        | Command::HyprMonitors
        | Command::CecScan
        | Command::CecDevice(_)
        | Command::CecPowerOn(_)
        | Command::CecPowerOff(_)
        | Command::CecActiveSource => protocol::resp_unsupported(),
        _ => return None,
    };
    Some(resp)
}

/// Stream broadcast events to a subscribed client until it disconnects.
async fn stream_events(
    framed: Framed<UnixStream, LinesCodec>,
    events_tx: broadcast::Sender<Event>,
) -> Result<()> {
    let mut rx = events_tx.subscribe();
    let (mut sink, mut input) = framed.split();
    loop {
        tokio::select! {
            // Detect client disconnect (EOF / further input). Python reads
            // until EOF then drops the subscriber.
            next = input.next() => {
                match next {
                    None => return Ok(()),          // EOF
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(_)) => { /* ignore extra input from a subscriber */ }
                }
            }
            evt = rx.recv() => {
                match evt {
                    Ok(event) => sink.send(event.to_string()).await?,
                    // Slow subscriber fell behind: skip, don't die.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("subscriber lagged, dropped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Control;

    /// A tiny stand-in runtime that answers control messages, so the IPC layer
    /// can be exercised end-to-end on any host (no evdev needed).
    async fn fake_runtime(mut rx: mpsc::Receiver<Control>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                Control::Grab(r)
                | Control::Release(r)
                | Control::Handoff(r)
                | Control::CaptureCancel(r) => {
                    let _ = r.send(protocol::resp_ok());
                }
                Control::Status(r) => {
                    let _ = r.send(protocol::resp_status(true, true));
                }
                Control::GetBindings(r) => {
                    let _ = r.send(protocol::resp_bindings(&[(
                        "select".into(),
                        "BTN_SOUTH".into(),
                    )]));
                }
                Control::SetBinding { reply, .. } => {
                    let _ = reply.send(protocol::resp_ok());
                }
                Control::CaptureNext(r) => {
                    let _ = r.send(protocol::resp_captured("BTN_SOUTH"));
                }
                Control::GetPads(r) => {
                    let _ = r.send(protocol::resp_pads(&[(
                        "uniq:test".into(),
                        0,
                        "Test Pad".into(),
                        true,
                    )]));
                }
                Control::ListInputDevices(r) => {
                    let _ = r.send(protocol::resp_input_devices(&[protocol::InputDeviceInfo {
                        name: "Test Pad".into(),
                        path: "/dev/input/event0".into(),
                        vendor: 0x045e,
                        product: 0x028e,
                        phys: "usb-test/input0".into(),
                        handlers: vec!["event0".into(), "js0".into()],
                        grabbed: true,
                    }]));
                }
                Control::Intent { name, reply } => {
                    // Mirror the runtime: accept the closed vocabulary, reject
                    // anything else. The fake doesn't actually broadcast.
                    let resp = if protocol::is_known_intent(&name) {
                        protocol::resp_ok()
                    } else {
                        protocol::resp_unknown_intent(&name)
                    };
                    let _ = reply.send(resp);
                }
                Control::Rumble { reply, .. } => {
                    // The real runtime no-ops when the pad/capability/setting is
                    // absent but still replies `ok`; the fake mirrors that.
                    let _ = reply.send(protocol::resp_ok());
                }
                Control::Key { name, reply } => {
                    // Mirror the runtime: a known key maps to a code (and would
                    // tap the virtual keyboard), an unknown one is rejected. The
                    // fake doesn't emit; it only checks the vocabulary.
                    let resp = if crate::config::key_for_action(&name).is_some() {
                        protocol::resp_ok()
                    } else {
                        protocol::resp_unknown_key(&name)
                    };
                    let _ = reply.send(resp);
                }
                Control::ConfigChanged => {}
                Control::SetActiveGame { reply, .. } => {
                    // In-memory only; fake just replies ok.
                    let _ = reply.send(protocol::resp_ok());
                }
                Control::ShellFocus { reply, .. } | Control::OverlayFocus { reply, .. } => {
                    // In-memory toggle in the real runtime; the fake just replies ok.
                    let _ = reply.send(protocol::resp_ok());
                }
                Control::PadBatteryQuery { id, reply } => {
                    // Fake: no pads in the test fleet -> pad not found.
                    let _ = reply.send(protocol::resp_pad_not_found(&id));
                }
                Control::PadRumbleStatus { id, reply } => {
                    // Fake: no pads in the test fleet -> pad not found.
                    let _ = reply.send(protocol::resp_pad_not_found(&id));
                }
                Control::ControllerDbRefreshed { reply } => {
                    let _ = reply.send(protocol::resp_ok());
                }
                // No reply, no device in the fake fleet — mirror the runtime no-op.
                Control::SetSessionActive(_) => {}
                Control::Shutdown => break,
            }
        }
    }

    async fn send_line(stream: &mut UnixStream, line: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        // Replies are newline-framed (LinesCodec). Read until the first '\n'
        // so large replies (e.g. `list-apps` on a host with many .desktop
        // files) aren't truncated mid-JSON by a fixed-size single read.
        let mut acc = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.unwrap();
            if n == 0 || byte[0] == b'\n' {
                break;
            }
            acc.push(byte[0]);
        }
        String::from_utf8_lossy(&acc).trim_end().to_string()
    }

    #[tokio::test]
    async fn end_to_end_commands_and_subscribe() {
        // Deliberately NOT `crate::testutil::scratch_path` here: this binds a
        // real Unix-domain socket, and `sockaddr_un::sun_path` caps the path
        // at ~104-108 bytes. A `crate::testutil` path lives under this
        // checkout's `target/debug/deps/`, which under a nested worktree can
        // already be close to that budget before the filename — so this one
        // site keeps the short, real system temp dir on purpose. (It's also a
        // documented *separate* sandboxed-hook flake: the sandbox denies the
        // `bind()` syscall itself regardless of path, so relocating it
        // wouldn't fix the hook anyway — Linux CI is authoritative for it.)
        let dir = std::env::temp_dir();
        let sock = dir
            .join(format!("gs-ipc-test-{}.sock", std::process::id()))
            .to_string_lossy()
            .to_string();
        let (control_tx, control_rx) = mpsc::channel(16);
        let (events_tx, _) = broadcast::channel(16);

        tokio::spawn(fake_runtime(control_rx));
        // No D-Bus actors are wired in the test (Default = all None on Linux,
        // empty on macOS), so Phase 3 query commands reply `unsupported` while
        // the stateless MAC-usage error still works.
        let db_state = std::sync::Arc::new(tokio::sync::RwLock::new(ControllerDbState::initial()));
        let server = tokio::spawn(serve(
            sock.clone(),
            control_tx,
            events_tx.clone(),
            DbusSenders::default(),
            db_state,
        ));

        // Wait for the socket to appear.
        for _ in 0..100 {
            if std::path::Path::new(&sock).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Command round-trips.
        let mut s = UnixStream::connect(&sock).await.unwrap();
        assert_eq!(send_line(&mut s, "grab").await, "ok");
        assert_eq!(send_line(&mut s, "status").await, "connected:grabbed");
        assert_eq!(
            send_line(&mut s, "get-bindings").await,
            r#"{"select":"BTN_SOUTH"}"#
        );
        assert_eq!(
            send_line(&mut s, "set-binding select").await,
            "error:usage: set-binding <action> <button_name>"
        );
        assert_eq!(send_line(&mut s, "frobnicate").await, "unknown");

        // Stateless Phase 2 commands are served without the input runtime.
        // They read real (possibly absent) files; assert only the response
        // *shape* so the test is independent of the host's HOME contents.
        let apps = send_line(&mut s, "list-apps").await;
        assert!(
            serde_json::from_str::<serde_json::Value>(&apps)
                .map(|v| v.is_array())
                .unwrap_or(false),
            "list-apps should be a JSON array, got: {apps}"
        );
        let cfg = send_line(&mut s, "get-config").await;
        assert!(
            serde_json::from_str::<serde_json::Value>(&cfg)
                .map(|v| v.is_object())
                .unwrap_or(false),
            "get-config should be a JSON object, got: {cfg}"
        );
        let recents = send_line(&mut s, "get-recents").await;
        assert!(
            serde_json::from_str::<serde_json::Value>(&recents)
                .map(|v| v.is_array())
                .unwrap_or(false),
            "get-recents should be a JSON array, got: {recents}"
        );
        // Notification commands: get-notifications starts empty (or array from any
        // prior host-state); assert shape only.
        let notifs = send_line(&mut s, "get-notifications").await;
        assert!(
            serde_json::from_str::<serde_json::Value>(&notifs)
                .map(|v| v.is_array())
                .unwrap_or(false),
            "get-notifications should be a JSON array, got: {notifs}"
        );
        // record-notification usage error.
        assert_eq!(
            send_line(&mut s, "record-notification").await,
            "error:usage: record-notification <json-object>"
        );
        // set-notifications usage error.
        assert_eq!(
            send_line(&mut s, "set-notifications").await,
            "error:usage: set-notifications <json-array>"
        );
        // Malformed bodies produce error:invalid JSON.
        assert!(send_line(&mut s, "record-notification not-json")
            .await
            .starts_with("error:invalid JSON"));
        assert!(send_line(&mut s, "set-notifications not-json")
            .await
            .starts_with("error:invalid JSON"));
        // Usage + malformed-body errors are stateless and HOME-independent.
        assert_eq!(
            send_line(&mut s, "set-config").await,
            "error:usage: set-config <json-object>"
        );
        assert_eq!(
            send_line(&mut s, "record-launch").await,
            "error:usage: record-launch <json-object>"
        );
        assert!(send_line(&mut s, "set-config not-json")
            .await
            .starts_with("error:invalid JSON"));
        assert_eq!(
            send_line(&mut s, "set-config [1,2,3]").await,
            "error:set-config body must be a JSON object"
        );

        // Phase 3 commands: with no D-Bus actor wired, query commands report
        // unsupported, but the stateless MAC-usage error is platform-independent.
        assert_eq!(
            send_line(&mut s, "bt-power-status").await,
            "error:unsupported on this platform"
        );
        assert_eq!(
            send_line(&mut s, "net-status").await,
            "error:unsupported on this platform"
        );
        assert_eq!(
            send_line(&mut s, "power-can-suspend").await,
            "error:unsupported on this platform"
        );
        assert_eq!(
            send_line(&mut s, "bt-connect").await,
            "error:usage: bt-connect <mac>"
        );

        // CEC health/test report the structured `no_libcec` unavailable JSON
        // when libcec ISN'T linked (feature/platform off, #19 follow-up), so the
        // AV Control page can distinguish causes — NOT the bare error line. The
        // other CEC commands keep the bare `unsupported` reply. Gated on
        // `not(feature = "cec")`: with the feature ON but the actor absent
        // (`cec: None` here), the structured arms aren't taken — health/test
        // route through `request_dbus`'s `None` path to `unsupported` (covered by
        // `cec_scan_unsupported_when_actor_absent`).
        #[cfg(not(feature = "cec"))]
        {
            assert_eq!(
                send_line(&mut s, "cec-health").await,
                r#"{"transmit":"unavailable","reason":"no_libcec","since":0,"lastError":null}"#
            );
            assert_eq!(
                send_line(&mut s, "cec-test").await,
                r#"{"transmit":"unavailable","reason":"no_libcec","since":0,"lastError":null}"#
            );
            assert_eq!(
                send_line(&mut s, "cec-scan").await,
                "error:unsupported on this platform"
            );
        }

        // build-info dispatches statelessly and returns the {version,sha,branch}
        // JSON object; version is compile-time, sha/branch depend on cwd git
        // state — assert shape + version, not the sha/branch values.
        let build_info = send_line(&mut s, "build-info").await;
        let bi: serde_json::Value = serde_json::from_str(&build_info)
            .unwrap_or_else(|_| panic!("build-info should be JSON, got: {build_info}"));
        assert_eq!(
            bi.get("version").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(bi.get("sha").map(|v| v.is_string()).unwrap_or(false));
        assert!(bi.get("branch").map(|v| v.is_string()).unwrap_or(false));

        // Intent control surface: a closed-vocabulary name is accepted; an
        // unknown name is rejected; a bare command is a usage error.
        assert_eq!(send_line(&mut s, "intent home-tap").await, "ok");
        assert_eq!(send_line(&mut s, "intent home").await, "ok");
        assert_eq!(
            send_line(&mut s, "intent frobnicate").await,
            "error:unknown intent 'frobnicate'"
        );
        assert_eq!(
            send_line(&mut s, "intent").await,
            "error:usage: intent <name>"
        );
        // Deep-link intents: valid namespaced targets are accepted.
        assert_eq!(send_line(&mut s, "intent settings:bluetooth").await, "ok");
        assert_eq!(send_line(&mut s, "intent overlay:volume").await, "ok");
        assert_eq!(send_line(&mut s, "intent overlay:network").await, "ok");
        assert_eq!(send_line(&mut s, "intent app:firefox").await, "ok");
        // Unknown overlay target -> rejected.
        assert_eq!(
            send_line(&mut s, "intent overlay:bogus").await,
            "error:unknown intent 'overlay:bogus'"
        );
        // Lock the rest of the coarse vocabulary that scripts/super-intent.sh
        // and the nav drawer ride on (bare Super -> menu, Super+Backspace ->
        // home-hold, Super+Right -> overlay:session). A rename here would break
        // the Hyprland binds silently.
        assert_eq!(send_line(&mut s, "intent menu").await, "ok");
        assert_eq!(send_line(&mut s, "intent home-hold").await, "ok");
        assert_eq!(send_line(&mut s, "intent power").await, "ok");
        assert_eq!(send_line(&mut s, "intent settings").await, "ok");
        assert_eq!(send_line(&mut s, "intent overlay:session").await, "ok");

        // Rumble control surface: a well-formed command round-trips the runtime
        // (the fake replies ok); a malformed body is a stateless usage error.
        assert_eq!(send_line(&mut s, "rumble uniq:test 200").await, "ok");
        assert_eq!(
            send_line(&mut s, "rumble uniq:test").await,
            "error:usage: rumble <id> <ms>"
        );

        // Key surface: a known token round-trips the runtime (ok); an unknown
        // token is rejected; a bare command is a stateless usage error.
        assert_eq!(send_line(&mut s, "key up").await, "ok");
        assert_eq!(send_line(&mut s, "key select").await, "ok");
        // The full D-pad/back nav vocabulary the QML KeyNavigation chains and
        // the screenshot-automation `key <name>` channel depend on (see
        // docs/qa-screenshot-views.md "two CLI channels").
        assert_eq!(send_line(&mut s, "key down").await, "ok");
        assert_eq!(send_line(&mut s, "key left").await, "ok");
        assert_eq!(send_line(&mut s, "key right").await, "ok");
        assert_eq!(send_line(&mut s, "key back").await, "ok");
        assert_eq!(
            send_line(&mut s, "key sideways").await,
            "error:unknown key 'sideways'"
        );
        assert_eq!(send_line(&mut s, "key").await, "error:usage: key <name>");

        // Overlay-focus control surface: `on`/`off` round-trip the runtime (the
        // fake replies ok); a missing/invalid arg is a stateless usage error.
        assert_eq!(send_line(&mut s, "overlay-focus on").await, "ok");
        assert_eq!(send_line(&mut s, "overlay-focus off").await, "ok");
        assert_eq!(
            send_line(&mut s, "overlay-focus").await,
            "error:usage: overlay-focus on|off"
        );
        assert_eq!(
            send_line(&mut s, "overlay-focus maybe").await,
            "error:usage: overlay-focus on|off"
        );

        // get-pads round-trips the runtime and returns the fleet JSON array.
        assert_eq!(
            send_line(&mut s, "get-pads").await,
            r#"[{"id":"uniq:test","index":0,"name":"Test Pad","grabbed":true}]"#
        );

        // list-input-devices round-trips the runtime and returns the diagnostics
        // enumerator array (one object per controller-like input device).
        assert_eq!(
            send_line(&mut s, "list-input-devices").await,
            r#"[{"name":"Test Pad","path":"/dev/input/event0","vendor":"045e","product":"028e","phys":"usb-test/input0","handlers":["event0","js0"],"grabbed":true}]"#
        );
        drop(s);

        // Subscribe receives broadcast events.
        let mut sub = UnixStream::connect(&sock).await.unwrap();
        assert_eq!(send_line(&mut sub, "subscribe").await, "subscribed");
        events_tx.send(Event::ControllerWake).unwrap();
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64];
        let n = sub.read(&mut buf).await.unwrap();
        assert_eq!(
            String::from_utf8_lossy(&buf[..n]).trim_end(),
            "controller-wake"
        );

        // A broadcast `intent:*` event reaches the subscriber on the wire.
        events_tx.send(Event::Intent("home-tap".into())).unwrap();
        let n = sub.read(&mut buf).await.unwrap();
        assert_eq!(
            String::from_utf8_lossy(&buf[..n]).trim_end(),
            "intent:home-tap"
        );

        server.abort();
    }

    /// Concern 1b (input-runtime down-detection): when the input runtime is gone,
    /// an input-runtime command must resolve to the distinct
    /// `error:input-runtime-down` line — NOT `unknown` — so a dead backend is
    /// distinguishable from a client typo on the wire. Dropping the control
    /// receiver makes `request`'s send fail → `None` → the dispatch fallback
    /// fires. A genuinely unknown command still replies `unknown` (unchanged).
    #[tokio::test]
    async fn input_runtime_down_maps_to_error() {
        let (control_tx, control_rx) = mpsc::channel::<Control>(1);
        drop(control_rx); // no runtime listening — every send fails
        let (events_tx, _events_rx) = broadcast::channel(16);
        let db_state = std::sync::Arc::new(tokio::sync::RwLock::new(ControllerDbState::initial()));

        let down = dispatch(
            &control_tx,
            &events_tx,
            &DbusSenders::default(),
            &db_state,
            Command::Grab,
        )
        .await;
        assert_eq!(down, "error:input-runtime-down");

        let unknown = dispatch(
            &control_tx,
            &events_tx,
            &DbusSenders::default(),
            &db_state,
            Command::Unknown,
        )
        .await;
        assert_eq!(unknown, "unknown");
    }

    /// Degradation: with the `cec` feature on but no CEC actor wired
    /// (`cec: None`), `cec-scan` still answers `error:unsupported on this
    /// platform` via `request_dbus`'s `None` path rather than panicking. Gated
    /// on `feature = "cec"` because the `cec` field only exists then, and on
    /// linux because `dispatch_dbus` is the Linux variant.
    #[cfg(all(target_os = "linux", feature = "cec"))]
    #[tokio::test]
    async fn cec_scan_unsupported_when_actor_absent() {
        let dbus = DbusSenders {
            cec: None,
            ..Default::default()
        };
        let resp = dispatch_dbus(&dbus, &Command::CecScan).await;
        assert_eq!(resp, Some("error:unsupported on this platform".to_string()));
    }
}

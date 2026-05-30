//! Unix-socket IPC server: accepts connections, parses one command per line,
//! and dispatches to the input runtime over the control channel. The
//! `subscribe` command upgrades the connection into an event stream fed by the
//! broadcast bus.
//!
//! Cross-platform (only `tokio`/`tokio-util`), so it compiles and can be tested
//! on non-Linux hosts even though the input runtime is Linux-only.

use crate::protocol::{self, Command, Event};
use crate::state::Control;
use crate::{apps, config, health, recents};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::codec::{Framed, LinesCodec};

#[cfg(target_os = "linux")]
use crate::bluetooth::BtReq;
#[cfg(target_os = "linux")]
use crate::hyprland::HyprReq;
#[cfg(target_os = "linux")]
use crate::network::NetReq;
#[cfg(target_os = "linux")]
use crate::power::PowerReq;

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
}

/// Bind the socket (removing any stale file), chmod 0o600, and serve until the
/// process exits.
pub async fn serve(
    sock_path: String,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
    dbus: DbusSenders,
) -> Result<()> {
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding unix socket at {sock_path}"))?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0o600 on {sock_path}"))?;
    tracing::info!("Listening on {sock_path}");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let control_tx = control_tx.clone();
                let events_tx = events_tx.clone();
                let dbus = dbus.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, control_tx, events_tx, dbus).await {
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
) -> Result<()> {
    let mut framed = Framed::new(stream, LinesCodec::new());

    while let Some(line) = framed.next().await {
        let line = line.context("reading command line")?;
        match Command::parse(&line) {
            Command::Subscribe => {
                framed.send(protocol::resp_subscribed()).await?;
                return stream_events(framed, events_tx).await;
            }
            cmd => {
                let response = dispatch(&control_tx, &dbus, cmd).await;
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
async fn dispatch_stateless(cmd: &Command) -> Option<String> {
    match cmd {
        Command::ListApps => Some(spawn_blocking_string(apps::list_apps_json).await),
        Command::GetConfig => {
            Some(spawn_blocking_string(|| config::load_config_json(&config::settings_path())).await)
        }
        Command::GetRecents => {
            Some(spawn_blocking_string(|| recents::load_recents(&recents::recents_path())).await)
        }
        Command::SetConfig(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    match serde_json::from_str::<serde_json::Value>(&body) {
                        Ok(updates) if updates.is_object() => {
                            match config::set_config(&config::settings_path(), &updates) {
                                Ok(merged) => merged,
                                Err(e) => protocol::resp_error(&format!("set-config failed: {e}")),
                            }
                        }
                        Ok(_) => protocol::resp_error("set-config body must be a JSON object"),
                        Err(e) => protocol::resp_error(&format!("invalid JSON: {e}")),
                    }
                })
                .await,
            )
        }
        Command::SetConfigUsage => Some(protocol::resp_set_config_usage()),
        Command::RecordLaunch(body) => {
            let body = body.clone();
            Some(
                spawn_blocking_string(move || {
                    match serde_json::from_str::<recents::Recent>(&body) {
                        Ok(mut entry) => {
                            entry.time = recents::now_unix_secs();
                            match recents::record_launch(&recents::recents_path(), entry) {
                                Ok(()) => protocol::resp_ok(),
                                Err(e) => {
                                    protocol::resp_error(&format!("record-launch failed: {e}"))
                                }
                            }
                        }
                        Err(e) => protocol::resp_error(&format!("invalid JSON: {e}")),
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
        _ => None,
    }
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
async fn dispatch(control_tx: &mpsc::Sender<Control>, dbus: &DbusSenders, cmd: Command) -> String {
    if let Some(resp) = dispatch_stateless(&cmd).await {
        return resp;
    }
    if let Some(resp) = dispatch_dbus(dbus, &cmd).await {
        return resp;
    }
    let fallback = protocol::resp_unknown();
    match cmd {
        Command::Grab => request(control_tx, Control::Grab).await,
        Command::Release => request(control_tx, Control::Release).await,
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
        Command::Intent(name) => {
            request(control_tx, move |reply| Control::Intent { name, reply }).await
        }
        // Handled without a round-trip to the runtime:
        Command::IntentUsage => return protocol::resp_intent_usage(),
        Command::SetBindingUsage => return protocol::resp_set_binding_usage(),
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
        // Phase 4 Sunshine is stateless (consumed by `dispatch_stateless`).
        | Command::SunshineStatus { .. }
        | Command::SunshineStatusUsage => return protocol::resp_unknown(),
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
        | Command::HyprClients => return protocol::resp_unknown(),
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
        | Command::HyprClients => protocol::resp_unsupported(),
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
                Control::Grab(r) | Control::Release(r) | Control::CaptureCancel(r) => {
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
        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).trim_end().to_string()
    }

    #[tokio::test]
    async fn end_to_end_commands_and_subscribe() {
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
        let server = tokio::spawn(serve(
            sock.clone(),
            control_tx,
            events_tx.clone(),
            DbusSenders::default(),
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

        // get-pads round-trips the runtime and returns the fleet JSON array.
        assert_eq!(
            send_line(&mut s, "get-pads").await,
            r#"[{"id":"uniq:test","index":0,"name":"Test Pad","grabbed":true}]"#
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
}

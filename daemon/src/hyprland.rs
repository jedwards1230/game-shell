//! Hyprland compositor subsystem (Phase 4): a long-lived async actor that owns a
//! direct connection to the Hyprland IPC sockets. It answers request/response
//! queries over an `mpsc` of [`HyprReq`] and pushes `hypr:*` [`Event`]s onto the
//! shared broadcast bus.
//!
//! Mostly READ ONLY: active-window class/title/address and the full client
//! list, plus active-window / fullscreen change events. User-triggered,
//! one-shot compositor *actions* (`hyprctl dispatch exec/closewindow`)
//! deliberately stay shell-outs in the QML. The one exception is kiosk
//! fullscreen enforcement, class-agnostic and CONTINUOUS: on `openwindow`
//! [`force_fullscreen`] fullscreens the address the event names (a window
//! may not have won focus yet at map time — see its doc comment); on
//! `closewindow`, `movewindowv2`, and `activewindowv2`
//! [`enforce_active_fullscreen`] re-checks whatever Hyprland now considers
//! the active window and fullscreens it if the tiler left it windowed. That
//! second half is what keeps the kiosk invariant "exactly one app fills the
//! screen" holding *after* a window disappears or the active window changes
//! for any other reason — not just at launch. This has to live here rather
//! than in QML because it must react to Hyprland's own event stream — an
//! event this actor already owns — and it's not a per-app decision QML
//! makes, it's a blanket compositor policy that also needs to fire even if
//! Quickshell is slow to start or has crashed.
//!
//! This REPLACES the `hyprctl clients -j` shell-out in
//! `components/HyprctlClients.qml` and feeds `AppLifecycleManager.qml`'s
//! window-event watching.
//!
//! We speak Hyprland's socket protocol directly (`.socket.sock` for
//! request/response, `.socket2.sock` for the event stream) rather than via the
//! `hyprland` crate. That crate (0.3.x) hardcodes the legacy `/tmp/hypr/<sig>`
//! socket directory, but Hyprland >= 0.40 moved its sockets to
//! `$XDG_RUNTIME_DIR/hypr/<sig>`, so the crate can never connect on a current
//! compositor (it loops on `No such file or directory` and panics in its
//! parser). The wire protocol is trivial — write a command and read the reply;
//! read newline-delimited `EVENT>>DATA` lines — so owning it is version-robust
//! and matches the daemon's own-the-IPC design. We resolve the modern path
//! first and fall back to the legacy one.
//!
//! Linux-only (Hyprland IPC socket); `main.rs` declares it under
//! `#[cfg(target_os = "linux")]`. Single-owner discipline mirrors the Phase 3
//! actors: the `run` loop owns the data getters and the event listener runs on
//! its own task, pushing onto the broadcast bus.

use crate::protocol::Event;
use crate::state::Reply;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, watch};

/// Requests from the IPC server to the Hyprland actor. Each carries a `oneshot`
/// reply with a fully-formatted wire string.
#[derive(Debug)]
pub enum HyprReq {
    /// `hypr-active` -> compact JSON object `{class,title,address}` (`{}` if no
    /// active window).
    Active(Reply),
    /// `hypr-clients` -> compact JSON array of `{class,title,address,workspace}`.
    Clients(Reply),
    /// `hypr-monitors` -> compact JSON array of monitor objects including
    /// currentFormat + derived hdr bool.
    Monitors(Reply),
}

/// Resolve the Hyprland IPC socket directory for the current instance.
///
/// Hyprland >= 0.40 uses `$XDG_RUNTIME_DIR/hypr/<sig>`; older versions used
/// `/tmp/hypr/<sig>`. Prefer whichever actually exists, defaulting to the modern
/// path when neither is present yet (Hyprland may start after the daemon — the
/// connect attempt then fails and is retried).
fn socket_dir() -> Result<PathBuf> {
    // Resolve the instance signature via session_env, which SCANS
    // $XDG_RUNTIME_DIR/hypr/ for the live socket dir first and only falls back to
    // an inherited HYPRLAND_INSTANCE_SIGNATURE when no live dir exists yet. That
    // scan-first ordering is what lets a reconnect self-heal onto a restarted
    // Hyprland: a long-lived daemon can inherit a signature pinned to a DEAD
    // instance (see resolve_hypr_signature's doc), and trusting it would keep
    // every query and the event stream pointed at a dead socket ("Connection
    // refused") forever. Resolving per call (rather than once at startup) means
    // both this actor's queries and the event watcher re-resolve on every retry.
    let sig = crate::session_env::resolve_hypr_signature().ok_or_else(|| {
        anyhow!("could not resolve Hyprland instance signature (env unset and no live socket dir in $XDG_RUNTIME_DIR/hypr)")
    })?;
    let legacy = PathBuf::from(format!("/tmp/hypr/{sig}"));
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        let xdg = PathBuf::from(rt).join("hypr").join(&sig);
        if xdg.exists() {
            return Ok(xdg);
        }
        if legacy.exists() {
            return Ok(legacy);
        }
        return Ok(xdg);
    }
    Ok(legacy)
}

/// Send one command to Hyprland's request socket (`.socket.sock`) and return the
/// full response. The `j/` prefix asks Hyprland for JSON; the server writes the
/// reply and closes the connection, so we read to EOF.
async fn request(cmd: &str) -> Result<String> {
    let sock = socket_dir()?.join(".socket.sock");
    let mut stream = UnixStream::connect(&sock).await?;
    stream.write_all(cmd.as_bytes()).await?;
    stream.flush().await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Log severity for a failed event-listener (re)connect attempt, chosen by the
/// consecutive-failure streak (see [`note_reconnect`]).
#[derive(Debug, PartialEq, Eq)]
enum ReconnectSeverity {
    /// Below the streak threshold — a routine per-retry warning.
    Warn,
    /// At/past the threshold — the daemon is persistently deaf to the
    /// compositor; escalate loudly on every retry until a clean reconnect
    /// resets the streak.
    Escalate,
}

/// Advance the consecutive-failure counter for one reconnect outcome and decide
/// the log severity. Extracted as a pure function so the escalation lifecycle is
/// unit-testable without an async socket.
///
/// - `ok == true` marks a clean (re)connect end (the socket closed cleanly, e.g.
///   Hyprland exited/replaced): the streak resets to 0 and `None` is returned
///   (nothing to log).
/// - `ok == false` marks a failed attempt: the streak increments and the result
///   is `Some(Escalate)` once it reaches `threshold` (and stays escalated on
///   every subsequent failure until a clean reconnect resets it), else
///   `Some(Warn)`.
fn note_reconnect(failures: &mut u32, ok: bool, threshold: u32) -> Option<ReconnectSeverity> {
    if ok {
        *failures = 0;
        return None;
    }
    *failures = failures.saturating_add(1);
    Some(if *failures >= threshold {
        ReconnectSeverity::Escalate
    } else {
        ReconnectSeverity::Warn
    })
}

/// Run the Hyprland actor until `rx` is closed.
///
/// Owns the request socket queries, services [`HyprReq`]s, and (via a spawned
/// task) streams `.socket2.sock` events onto `events_tx`. Never panics: a
/// missing/closed socket degrades queries to an empty document and the event
/// watcher retries with capped backoff so `hypr:*` events self-heal if Hyprland
/// starts after the daemon or restarts later.
///
/// `active_window_tx` is the sender half of a [`tokio::sync::watch`] channel
/// carrying the latest focused-window class: every `activewindow` event is
/// published there (latest-wins / coalescing) so the input runtime can make the
/// Game/Shell presenter follow compositor focus. Focus is *state*, not an event
/// stream, so a watch channel (which only ever retains the newest value) is the
/// right primitive — a burst of focus changes can never back up or drop.
pub async fn run(
    mut rx: mpsc::Receiver<HyprReq>,
    events_tx: broadcast::Sender<Event>,
    active_window_tx: watch::Sender<String>,
    shell_focus_rx: watch::Receiver<bool>,
) -> Result<()> {
    {
        let events_tx = events_tx.clone();
        let shell_focus_rx = shell_focus_rx.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            // Count consecutive failed (re)connect attempts so a *persistent*
            // inability to reach any live Hyprland escalates from a routine
            // per-retry warn to one loud, unmissable line. That is the deaf-daemon
            // signature (event socket unreachable — a killed/restarted or absent
            // compositor); it trapped two investigators today because nothing
            // surfaced it. Note it deliberately does NOT catch the render-wedge
            // (frozen frames while IPC still answers): that leaves the read loop
            // blocked with neither Err nor Ok, so it is not observable from the
            // IPC socket at all — detecting it needs a render-side heartbeat
            // (see docs/KIOSK_WINDOW_MODEL.md, Phase 2).
            let mut consecutive_failures: u32 = 0;
            const ESCALATE_AFTER: u32 = 5;
            loop {
                match watch_events(
                    events_tx.clone(),
                    active_window_tx.clone(),
                    shell_focus_rx.clone(),
                )
                .await
                {
                    Ok(()) => {
                        // Socket closed cleanly (Hyprland exited/replaced); the next
                        // attempt re-resolves the live instance (self-heal).
                        backoff = Duration::from_secs(1);
                        note_reconnect(&mut consecutive_failures, true, ESCALATE_AFTER);
                    }
                    Err(e) => {
                        // note_reconnect increments the streak and, once it reaches
                        // ESCALATE_AFTER, escalates on EVERY retry (not just the
                        // Nth) so a persistent outage stays visible in the journal.
                        // The live `consecutive_failures` is interpolated so the "in
                        // a row" count is always accurate (it resets on the next
                        // clean reconnect, so a recovered-then-failed streak
                        // re-escalates from scratch).
                        match note_reconnect(&mut consecutive_failures, false, ESCALATE_AFTER) {
                            // Pass the count + error as structured fields (not string
                            // interpolation) so the numbers render unambiguously in
                            // journald/JSON: `consecutive_failures=N error=…`.
                            Some(ReconnectSeverity::Escalate) => tracing::error!(
                                consecutive_failures,
                                error = %e,
                                "hyprland: event listener is DEAF to the compositor — repeated failed \
                                 (re)connects. Hyprland is likely down or was restarted under a new \
                                 instance signature; kiosk fullscreen follow-focus and the gamepad \
                                 presenter's follow-focus will not fire until this recovers."
                            ),
                            _ => {
                                tracing::warn!(error = %e, "hyprland: event listener stopped; retrying")
                            }
                        }
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
    }

    tracing::info!("hyprland actor started");

    while let Some(req) = rx.recv().await {
        match req {
            HyprReq::Active(reply) => {
                let _ = reply.send(active_window_json().await);
            }
            HyprReq::Clients(reply) => {
                let _ = reply.send(clients_json().await);
            }
            HyprReq::Monitors(reply) => {
                let _ = reply.send(monitors_json().await);
            }
        }
    }

    tracing::info!("hyprland actor stopped");
    Ok(())
}

/// Query the active Hyprland window directly without going through the actor
/// channel. Returns `Ok(json)` with the same `{class,title,address}` shape as
/// `hypr-active`, or `Ok("{}")` when no window is active or the socket is
/// unreachable. Useful for one-shot reads from `bridge_core` where injecting
/// the actor `mpsc::Sender` would require threading it through multiple layers.
///
/// Called by `bridge_core::get_ui_state` (gated `#[cfg(target_os = "linux")]`).
pub async fn query_active_window() -> String {
    active_window_json().await
}

/// Build the `hypr-active` compact-JSON object `{class,title,address}`, or `{}`
/// when there's no active window / on any IPC failure (so the QML page stays
/// usable when the Hyprland socket is absent).
async fn active_window_json() -> String {
    match request("j/activewindow").await {
        Ok(body) => parse_active(&body),
        Err(e) => {
            tracing::debug!("hyprland: activewindow query failed: {e}");
            "{}".to_string()
        }
    }
}

/// Reshape Hyprland's verbose `j/activewindow` object down to the
/// `{class,title,address,fullscreen}` wire contract. Empty body or no `class` -> `{}`.
fn parse_active(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) if v.get("class").is_some() => active_entry(&v),
        _ => "{}".to_string(),
    }
}

/// Interpret Hyprland's `fullscreen` field as a bool. Across Hyprland versions
/// this has been a bool *or* an integer fullscreen-mode (0 = none/windowed,
/// nonzero = a fullscreen mode such as 1 = fullscreen, 2 = maximized). Treat
/// `true` or any nonzero integer as fullscreen; absent/`false`/0 as not.
fn is_fullscreen(v: &Value) -> bool {
    match v.get("fullscreen") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().map(|i| i != 0).unwrap_or(false),
        _ => false,
    }
}

/// Serialize the `{class,title,address,fullscreen}` subset of one window object.
/// `fullscreen` lets QML read the active window's fullscreen state on the initial
/// `hypr-active` query, before any live `hypr:fullscreen` event arrives.
fn active_entry(v: &Value) -> String {
    json!({
        "class": v.get("class").and_then(Value::as_str).unwrap_or(""),
        "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
        "address": v.get("address").and_then(Value::as_str).unwrap_or(""),
        "fullscreen": is_fullscreen(v),
    })
    .to_string()
}

/// Build the `hypr-clients` compact-JSON array, mirroring `hyprctl clients -j`
/// (`class,title,address,workspace`). Degrades to `[]` on IPC failure.
async fn clients_json() -> String {
    match request("j/clients").await {
        Ok(body) => parse_clients(&body),
        Err(e) => {
            tracing::debug!("hyprland: clients query failed: {e}");
            "[]".to_string()
        }
    }
}

/// Reshape Hyprland's `j/clients` array to `[{class,title,address,workspace}]`,
/// where `workspace` is the workspace *name* (matching what the QML read from
/// the old `hyprctl clients -j` `workspace.name`). Non-array body -> `[]`.
fn parse_clients(body: &str) -> String {
    match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Array(items)) => {
            let list: Vec<Value> = items.iter().map(client_entry).collect();
            Value::Array(list).to_string()
        }
        _ => "[]".to_string(),
    }
}

/// Serialize one client as `{class,title,address,workspace}` (compact JSON).
fn client_entry(v: &Value) -> Value {
    json!({
        "class": v.get("class").and_then(Value::as_str).unwrap_or(""),
        "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
        "address": v.get("address").and_then(Value::as_str).unwrap_or(""),
        "workspace": v
            .get("workspace")
            .and_then(|w| w.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        // Hyprland's per-window focus order (0 = most recently focused). Lets the
        // shell sort running-window cards most-recently-used first. Absent -> a
        // large sentinel so unknown windows sort last.
        "focusHistoryId": v
            .get("focusHistoryID")
            .and_then(Value::as_i64)
            .unwrap_or(9999),
    })
}

/// Build the `hypr-monitors` compact-JSON array. Degrades to `[]` on IPC failure.
async fn monitors_json() -> String {
    match request("j/monitors").await {
        Ok(body) => parse_monitors(&body),
        Err(e) => {
            tracing::debug!("hyprland: monitors query failed: {e}");
            "[]".to_string()
        }
    }
}

/// Reshape Hyprland's `j/monitors` array into a compact monitor array with
/// exactly: name, description, width, height, refreshRate, scale, x, y,
/// activeWorkspace (from activeWorkspace.name), dpmsStatus, vrr,
/// availableModes (passthrough array), currentFormat, and a DERIVED `hdr` bool.
///
/// `hdr` is derived: true when `currentFormat` (uppercased) contains `"2101010"`
/// (the 10-bit packed formats XRGB2101010/ARGB2101010 used by Hyprland for the
/// HDR/wide-gamut path on this box). Hyprland exposes no explicit hdr flag in
/// `j/monitors`, so 10-bit currentFormat is the proxy. Non-array body -> `[]`.
fn parse_monitors(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body.trim()) {
        Ok(serde_json::Value::Array(items)) => {
            let list: Vec<serde_json::Value> = items.iter().map(monitor_entry).collect();
            serde_json::Value::Array(list).to_string()
        }
        _ => "[]".to_string(),
    }
}

/// Serialize one monitor as the full compact monitor object.
fn monitor_entry(v: &serde_json::Value) -> serde_json::Value {
    let current_format = v
        .get("currentFormat")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    // hdr is derived from a 10-bit format (2101010 suffix present in e.g.
    // XRGB2101010 / ARGB2101010) — Hyprland's indicator that HDR/wide-gamut
    // tone-mapping is active on this monitor.
    let hdr = current_format.to_uppercase().contains("2101010");
    let active_workspace = v
        .get("activeWorkspace")
        .and_then(|w| w.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    json!({
        "name": v.get("name").and_then(serde_json::Value::as_str).unwrap_or(""),
        "description": v.get("description").and_then(serde_json::Value::as_str).unwrap_or(""),
        "width": v.get("width").and_then(serde_json::Value::as_u64).unwrap_or(0),
        "height": v.get("height").and_then(serde_json::Value::as_u64).unwrap_or(0),
        "refreshRate": v.get("refreshRate").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        "scale": v.get("scale").and_then(serde_json::Value::as_f64).unwrap_or(1.0),
        "x": v.get("x").and_then(serde_json::Value::as_i64).unwrap_or(0),
        "y": v.get("y").and_then(serde_json::Value::as_i64).unwrap_or(0),
        "activeWorkspace": active_workspace,
        "dpmsStatus": v.get("dpmsStatus").and_then(serde_json::Value::as_bool).unwrap_or(true),
        "vrr": v.get("vrr").and_then(serde_json::Value::as_bool).unwrap_or(false),
        "availableModes": v.get("availableModes").cloned().unwrap_or(serde_json::Value::Array(vec![])),
        "currentFormat": current_format,
        "hdr": hdr,
    })
}

/// Watch Hyprland's event socket (`.socket2.sock`) and fan `hypr:*` events onto
/// the broadcast bus. Reads newline-delimited `EVENT>>DATA` lines. Returns when
/// the socket closes (the caller retries with backoff); errors propagate so the
/// caller logs and retries.
async fn watch_events(
    events_tx: broadcast::Sender<Event>,
    active_window_tx: watch::Sender<String>,
    shell_focus_rx: watch::Receiver<bool>,
) -> Result<()> {
    let dir = socket_dir()?;
    let sock = dir.join(".socket2.sock");
    let stream = UnixStream::connect(&sock).await?;
    // Log the instance we actually attached to. The deaf-daemon failure mode
    // (attached to a dead instance, "Connection refused" looping in the retry
    // handler) is otherwise invisible — everything else looks healthy — so
    // naming the live socket dir on each successful (re)connect makes a stale
    // attach diagnosable from the journal at a glance.
    tracing::info!("hyprland: event listener attached to {}", dir.display());
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        let Some((event, data)) = line.split_once(">>") else {
            continue;
        };
        match event {
            // `activewindow>>class,title` — class is everything before the first
            // comma (a title may contain commas). Empty when focus is lost
            // (`activewindow>>,`), matching the empty-class wire contract.
            //
            // Also published to the input runtime over the `active_window` watch
            // channel (latest-wins) so the gamepad presenter follows focus (see
            // the `run` doc comment). Coalescing is the whole point: focus is
            // STATE, so if the input loop is momentarily busy, only the newest
            // class matters — the watch channel retains it rather than dropping or
            // backing up (the old `try_send` on a full control channel could drop
            // a focus update and desync the presenter until the next change).
            // `watch::send` only errs when every receiver is gone (shutting down),
            // which is harmless to ignore. This path can no longer stall the event
            // reader (and thus kiosk fullscreen enforcement) on a full channel.
            "activewindow" => {
                let class = data
                    .split_once(',')
                    .map(|(c, _)| c)
                    .unwrap_or(data)
                    .to_string();
                let _ = events_tx.send(Event::HyprActiveWindow(class.clone()));
                let _ = active_window_tx.send(class);
            }
            // `fullscreen>>0|1`.
            "fullscreen" => {
                let _ = events_tx.send(Event::HyprFullscreen(data.trim() == "1"));
            }
            // `openwindow>>ADDRESS,WORKSPACENAME,CLASS,TITLE` — title is the
            // remainder and may contain commas. Build compact JSON so commas in
            // titles can't break QML parsing.
            "openwindow" => {
                if let Some(address) = openwindow_address(data) {
                    let address = address.to_string();
                    if kiosk_may_enforce(&shell_focus_rx, "openwindow") {
                        tokio::spawn(async move { force_fullscreen(&address).await });
                    }
                }
                let json = parse_openwindow(data);
                let _ = events_tx.send(Event::HyprOpenWindow(json));
            }
            // `closewindow>>ADDRESS` — just the window address. Whatever
            // window Hyprland promotes to active next (if any) needs
            // re-fullscreening: the tiler reclaims the layout on close and
            // splits the survivor(s) instead of leaving one fullscreen. This
            // is the case that used to slip through — fullscreen was only
            // ever enforced on open.
            "closewindow" => {
                if kiosk_may_enforce(&shell_focus_rx, "closewindow") {
                    tokio::spawn(enforce_active_fullscreen());
                }
                let _ = events_tx.send(Event::HyprCloseWindow(data.trim().to_string()));
            }
            // `movewindowv2>>ADDRESS,WORKSPACEID,WORKSPACENAME` — a window
            // changed workspace, which can leave it (or whatever it displaced)
            // tiled on either side. No data fields are needed: re-check
            // whichever window is active now.
            "movewindowv2" => {
                if kiosk_may_enforce(&shell_focus_rx, "movewindowv2") {
                    tokio::spawn(enforce_active_fullscreen());
                }
            }
            // `activewindowv2>>ADDRESS` — focus changed for any reason not
            // already covered above (e.g. a keybind focus-cycle). Re-assert
            // fullscreen on the newly-active window so the invariant holds
            // regardless of *why* focus moved.
            "activewindowv2" => {
                if kiosk_may_enforce(&shell_focus_rx, "activewindowv2") {
                    tokio::spawn(enforce_active_fullscreen());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Parse the `openwindow` event data string into a compact JSON object.
///
/// Hyprland emits `openwindow>>ADDRESS,WORKSPACENAME,CLASS,TITLE` where TITLE
/// is the remainder (may contain commas). Returns a compact JSON object
/// `{"address":"0x..","class":"..","title":"..","workspace":".."}`.
fn parse_openwindow(data: &str) -> String {
    // Split into at most 4 parts: address, workspace, class, title (remainder).
    let mut parts = data.splitn(4, ',');
    let address = parts.next().unwrap_or("").trim();
    let workspace = parts.next().unwrap_or("").trim();
    let class = parts.next().unwrap_or("").trim();
    let title = parts.next().unwrap_or("").trim();
    serde_json::json!({
        "address": address,
        "class": class,
        "title": title,
        "workspace": workspace,
    })
    .to_string()
}

/// Extract the window address from an `openwindow` event's raw data
/// (`ADDRESS,WORKSPACENAME,CLASS,TITLE`). `None` for an empty/missing address
/// so callers skip the fullscreen dispatch rather than target an empty
/// selector. Also requires the `0x` prefix Hyprland always uses for window
/// addresses — cheap defense-in-depth against a malformed/truncated event
/// line reaching `dispatch focuswindow address:<...>` with garbage.
fn openwindow_address(data: &str) -> Option<&str> {
    data.split(',')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.starts_with("0x"))
}

/// Kiosk enforcement: force a newly-mapped window to take over the screen,
/// independent of its class. This exists because the static
/// `windowrule = fullscreen` effect (`config/hyprland.conf`) only applies if
/// the new window also wins initial keyboard focus on the same map —
/// Hyprland gates the static effect on `!m_noInitialFocus` in its own
/// `onMap()` — and on this kiosk a second app can map while something else
/// (Quickshell's layer surface, or the previous app) still holds focus, so
/// the windowrule silently no-ops and the new window lands tiled with
/// whatever else is on the workspace.
///
/// Doing it imperatively here, after the window has already mapped,
/// sidesteps that race entirely: `focuswindow` resolves the window by
/// address regardless of who currently has focus, and `fullscreen 0 set`
/// (not the bare toggle form) is idempotent, so this is safe to fire on
/// every open even if a window somehow already fullscreened itself. Runs on
/// its own spawned task so a slow/failed dispatch can't stall the event
/// reader loop. Best-effort: a failed dispatch (Hyprland socket hiccup, or a
/// window that closed again before the dispatch reached it) just leaves the
/// window as Hyprland's own layout put it — never panics, but IS logged so a
/// pattern of failures (e.g. the request socket going away) is visible
/// rather than silently swallowed.
///
/// This is the open-time half of kiosk fullscreen enforcement; the
/// continuous half — re-asserting fullscreen after a window closes, moves,
/// or focus otherwise changes — is [`enforce_active_fullscreen`].
/// Whether the kiosk fullscreen backstop may act right now.
///
/// It may NOT while the shell owns the screen. Both entry points below resolve
/// their target from Hyprland's *active window* — but the shell is a
/// wlr-layer-shell surface that never appears there, so while the shell is on
/// screen `activewindow` still names whatever toplevel was focused last: a
/// backgrounded, already-fullscreen app. Acting on that is not a harmless no-op,
/// because [`force_fullscreen`] issues a `dispatch focuswindow` *before*
/// `fullscreen 0 set` — it would actively yank focus (and the screen) to that
/// stale window out from under the shell. That is the "launched Steam, Plex came
/// to the front instead" bug: Plex was prewarmed, held the active-window slot,
/// and the backstop kept re-fullscreening it.
///
/// The shell re-asserts `shell-focus` on a heartbeat, so this gate reopens on
/// its own once the shell genuinely hands the screen to an app.
fn kiosk_may_enforce(shell_focus_rx: &watch::Receiver<bool>, event: &str) -> bool {
    if *shell_focus_rx.borrow() {
        tracing::debug!(
            event,
            "hyprland: kiosk fullscreen backstop skipped — the shell owns the screen"
        );
        return false;
    }
    true
}

async fn force_fullscreen(address: &str) {
    if let Err(e) = request(&format!("dispatch focuswindow address:{address}")).await {
        tracing::warn!("hyprland: force_fullscreen: failed to focus {address}: {e}");
    }
    if let Err(e) = request("dispatch fullscreen 0 set").await {
        tracing::warn!("hyprland: force_fullscreen: failed to fullscreen {address}: {e}");
    }
}

/// Whether the active-window JSON from `j/activewindow` names a window this
/// kiosk should force-fullscreen: something is actually focused (`class`
/// non-empty — an empty document, or an object with no `class`, means
/// nothing is focused: e.g. the last window just closed and only
/// Quickshell's layer-shell surface remains, which never appears in
/// `j/activewindow` at all, so it's naturally exempt from this whole
/// mechanism) and it isn't already fullscreen. The latter check is what
/// keeps [`enforce_active_fullscreen`] a true no-op on the common case and
/// stops it feeding back into its own `fullscreen`/`activewindowv2` events.
fn needs_fullscreen(v: &Value) -> bool {
    let focused = v
        .get("class")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.is_empty());
    focused && !is_fullscreen(v)
}

/// Continuous kiosk enforcement: ask Hyprland who's active *right now* and
/// fullscreen it if the tiler left it windowed. Fired after `closewindow`,
/// `movewindowv2`, and `activewindowv2` — any event where the previously
/// enforced fullscreen window may have been reclaimed by the tiler (most
/// visibly: a window closes and Hyprland re-tiles the survivor(s) into a
/// split instead of leaving one fullscreen).
///
/// Unlike [`force_fullscreen`], this has no window address of its own to
/// act on and deliberately skips the explicit `focuswindow` dispatch — there
/// is no wrong-window-has-focus race here (the window is already active by
/// definition), only a wrong-layout one, so a bare `fullscreen 0 set`
/// suffices. See [`needs_fullscreen`] for the skip conditions.
async fn enforce_active_fullscreen() {
    let body = match request("j/activewindow").await {
        Ok(body) => body,
        Err(e) => {
            tracing::debug!("hyprland: enforce_active_fullscreen: activewindow query failed: {e}");
            return;
        }
    };
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return;
    }
    let v = match serde_json::from_str::<Value>(trimmed) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                "hyprland: enforce_active_fullscreen: activewindow reply not valid JSON: {e}"
            );
            return;
        }
    };
    if !needs_fullscreen(&v) {
        return;
    }
    if let Err(e) = request("dispatch fullscreen 0 set").await {
        tracing::warn!("hyprland: enforce_active_fullscreen: failed to fullscreen: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_counter_lifecycle() {
        const THRESHOLD: u32 = 5;
        let mut failures = 0u32;

        // Below the threshold: each failure warns and increments the streak.
        for expected in 1..THRESHOLD {
            assert_eq!(
                note_reconnect(&mut failures, false, THRESHOLD),
                Some(ReconnectSeverity::Warn)
            );
            assert_eq!(failures, expected);
        }

        // Reaching the threshold escalates...
        assert_eq!(
            note_reconnect(&mut failures, false, THRESHOLD),
            Some(ReconnectSeverity::Escalate)
        );
        assert_eq!(failures, THRESHOLD);

        // ...and STAYS escalated on every subsequent failure (not just the Nth),
        // with the streak still climbing so the logged count is truthful.
        assert_eq!(
            note_reconnect(&mut failures, false, THRESHOLD),
            Some(ReconnectSeverity::Escalate)
        );
        assert_eq!(
            note_reconnect(&mut failures, false, THRESHOLD),
            Some(ReconnectSeverity::Escalate)
        );
        assert_eq!(failures, THRESHOLD + 2);

        // A clean reconnect resets the streak and logs nothing.
        assert_eq!(note_reconnect(&mut failures, true, THRESHOLD), None);
        assert_eq!(failures, 0);

        // After recovery a fresh failure warns again — the streak re-escalates
        // from scratch rather than staying latched.
        assert_eq!(
            note_reconnect(&mut failures, false, THRESHOLD),
            Some(ReconnectSeverity::Warn)
        );
        assert_eq!(failures, 1);
    }

    #[test]
    fn active_reshapes_to_contract() {
        // Hyprland's j/activewindow is verbose; we keep only
        // class/title/address/fullscreen.
        let body = r#"{"address":"0x55","class":"steam","title":"Steam, Big Picture","pid":42,"workspace":{"id":1,"name":"1"}}"#;
        let out = parse_active(body);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("class").unwrap(), "steam");
        assert_eq!(v.get("title").unwrap(), "Steam, Big Picture");
        assert_eq!(v.get("address").unwrap(), "0x55");
        assert_eq!(v.get("fullscreen").unwrap(), false); // absent -> false
        assert!(v.get("pid").is_none()); // dropped
    }

    #[test]
    fn active_fullscreen_field_handles_bool_and_int() {
        // bool true
        let v: Value = serde_json::from_str(&parse_active(
            r#"{"class":"a","title":"","address":"0x1","fullscreen":true}"#,
        ))
        .unwrap();
        assert_eq!(v.get("fullscreen").unwrap(), true);

        // integer fullscreen-mode: nonzero -> true (e.g. 1 = fullscreen, 2 = maximized)
        let v: Value = serde_json::from_str(&parse_active(
            r#"{"class":"a","title":"","address":"0x1","fullscreen":2}"#,
        ))
        .unwrap();
        assert_eq!(v.get("fullscreen").unwrap(), true);

        // integer 0 -> false (windowed)
        let v: Value = serde_json::from_str(&parse_active(
            r#"{"class":"a","title":"","address":"0x1","fullscreen":0}"#,
        ))
        .unwrap();
        assert_eq!(v.get("fullscreen").unwrap(), false);
    }

    #[test]
    fn active_empty_and_malformed_become_empty_object() {
        assert_eq!(parse_active(""), "{}");
        assert_eq!(parse_active("{}"), "{}"); // no class
        assert_eq!(parse_active("not json"), "{}");
    }

    #[test]
    fn clients_reshapes_each_entry_with_workspace_name() {
        let body = r#"[{"address":"0x1","class":"foo","title":"Foo","focusHistoryID":0,"workspace":{"id":2,"name":"web"}},
                       {"address":"0x2","class":"bar","title":"Bar","workspace":{"id":3,"name":"games"}}]"#;
        let out = parse_clients(body);
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].get("workspace").unwrap(), "web");
        assert_eq!(arr[0].get("focusHistoryId").unwrap(), 0);
        assert_eq!(arr[1].get("class").unwrap(), "bar");
        assert_eq!(arr[1].get("focusHistoryId").unwrap(), 9999); // absent -> sentinel
    }

    #[test]
    fn clients_non_array_becomes_empty_array() {
        assert_eq!(parse_clients("{}"), "[]");
        assert_eq!(parse_clients(""), "[]");
    }

    #[test]
    fn monitors_hdr_derived_from_10bit_format() {
        // XRGB2101010 -> hdr = true
        let body = r#"[{"name":"DP-1","description":"LG OLED","width":3840,"height":2160,"refreshRate":120.0,"scale":1.0,"x":0,"y":0,"activeWorkspace":{"id":1,"name":"1"},"dpmsStatus":true,"vrr":true,"availableModes":["3840x2160@120.00000"],"currentFormat":"XRGB2101010"}]"#;
        let out = parse_monitors(body);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("hdr").unwrap(), true);
        assert_eq!(arr[0].get("currentFormat").unwrap(), "XRGB2101010");
        assert_eq!(arr[0].get("name").unwrap(), "DP-1");
        assert_eq!(arr[0].get("width").unwrap(), 3840);
        assert_eq!(arr[0].get("activeWorkspace").unwrap(), "1");
    }

    #[test]
    fn monitors_hdr_false_for_8bit_format() {
        // XRGB8888 -> hdr = false
        let body = r#"[{"name":"HDMI-A-1","description":"Test Monitor","width":1920,"height":1080,"refreshRate":60.0,"scale":1.0,"x":0,"y":0,"activeWorkspace":{"id":1,"name":"1"},"dpmsStatus":true,"vrr":false,"availableModes":["1920x1080@60.00000"],"currentFormat":"XRGB8888"}]"#;
        let out = parse_monitors(body);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0].get("hdr").unwrap(), false);
        assert_eq!(arr[0].get("currentFormat").unwrap(), "XRGB8888");
    }

    #[test]
    fn monitors_non_array_becomes_empty_array() {
        assert_eq!(parse_monitors("{}"), "[]");
        assert_eq!(parse_monitors(""), "[]");
        assert_eq!(parse_monitors("not json"), "[]");
    }

    #[test]
    fn monitors_missing_current_format_defaults_to_empty_and_hdr_false() {
        // Missing currentFormat -> hdr=false, currentFormat=""
        let body = r#"[{"name":"DP-2","width":2560,"height":1440,"refreshRate":144.0,"scale":1.0,"x":0,"y":0,"activeWorkspace":{"id":1,"name":"1"}}]"#;
        let out = parse_monitors(body);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0].get("hdr").unwrap(), false);
        assert_eq!(arr[0].get("currentFormat").unwrap(), "");
    }

    #[test]
    fn parse_openwindow_basic() {
        let data = "0x12345678,1,steam,Steam Big Picture";
        let out = parse_openwindow(data);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("address").unwrap(), "0x12345678");
        assert_eq!(v.get("workspace").unwrap(), "1");
        assert_eq!(v.get("class").unwrap(), "steam");
        assert_eq!(v.get("title").unwrap(), "Steam Big Picture");
    }

    #[test]
    fn parse_openwindow_title_with_commas() {
        // Title may contain commas — only split into 4 parts max.
        let data = "0xabcdef,games,firefox,Mozilla Firefox, Web Browser, v120";
        let out = parse_openwindow(data);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("address").unwrap(), "0xabcdef");
        assert_eq!(v.get("workspace").unwrap(), "games");
        assert_eq!(v.get("class").unwrap(), "firefox");
        // Full remainder including commas is preserved.
        assert_eq!(
            v.get("title").unwrap(),
            "Mozilla Firefox, Web Browser, v120"
        );
    }

    #[test]
    fn parse_openwindow_missing_fields_default_to_empty() {
        // Fewer than 4 comma-separated parts — missing fields become "".
        let out = parse_openwindow("");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("address").unwrap(), "");
        assert_eq!(v.get("class").unwrap(), "");
        assert_eq!(v.get("title").unwrap(), "");

        let out2 = parse_openwindow("0x1,workspace");
        let v2: serde_json::Value = serde_json::from_str(&out2).unwrap();
        assert_eq!(v2.get("address").unwrap(), "0x1");
        assert_eq!(v2.get("workspace").unwrap(), "workspace");
        assert_eq!(v2.get("class").unwrap(), "");
        assert_eq!(v2.get("title").unwrap(), "");
    }

    #[test]
    fn parse_openwindow_output_is_compact_json() {
        let out = parse_openwindow("0x1,1,cls,title");
        // No newlines, no `": "` pretty-print spacing.
        assert!(!out.contains('\n'));
        assert!(!out.contains(": "));
    }

    #[test]
    fn openwindow_address_extracts_first_field() {
        assert_eq!(
            openwindow_address("0x12345678,1,steam,Steam Big Picture"),
            Some("0x12345678")
        );
        // Works for any class — the kiosk fullscreen enforcement it feeds is
        // class-agnostic by design.
        assert_eq!(
            openwindow_address("0xabc,games,some.random.App,Title"),
            Some("0xabc")
        );
    }

    #[test]
    fn openwindow_address_none_for_missing_or_empty() {
        assert_eq!(openwindow_address(""), None);
        assert_eq!(openwindow_address(",1,steam,Title"), None);
        assert_eq!(openwindow_address("  ,1,steam,Title"), None);
        // Missing `0x` prefix must also be rejected (defense-in-depth).
        assert_eq!(openwindow_address("12345678,1,steam,Title"), None);
        assert_eq!(openwindow_address("abc,1,steam,Title"), None);
    }

    #[test]
    fn needs_fullscreen_true_when_focused_and_windowed() {
        let v: Value =
            serde_json::from_str(r#"{"class":"steam","address":"0x1","fullscreen":0}"#).unwrap();
        assert!(needs_fullscreen(&v));
    }

    #[test]
    fn needs_fullscreen_false_when_already_fullscreen() {
        // Both the bool and integer-mode fullscreen encodings must suppress
        // enforcement — this is the loop-prevention no-op.
        let v: Value =
            serde_json::from_str(r#"{"class":"steam","address":"0x1","fullscreen":true}"#).unwrap();
        assert!(!needs_fullscreen(&v));
        let v: Value =
            serde_json::from_str(r#"{"class":"steam","address":"0x1","fullscreen":2}"#).unwrap();
        assert!(!needs_fullscreen(&v));
    }

    #[test]
    fn needs_fullscreen_false_when_nothing_focused() {
        // Empty object (no active window at all) and an object with an empty
        // `class` (Hyprland's "nothing focused" shape) both must no-op —
        // e.g. right after the last window closes and only Quickshell's
        // layer-shell surface remains.
        let empty: Value = serde_json::from_str("{}").unwrap();
        assert!(!needs_fullscreen(&empty));
        let no_class: Value =
            serde_json::from_str(r#"{"class":"","address":"","fullscreen":0}"#).unwrap();
        assert!(!needs_fullscreen(&no_class));
    }
}

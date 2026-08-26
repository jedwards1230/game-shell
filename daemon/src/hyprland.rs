//! Hyprland compositor subsystem (Phase 4): a long-lived async actor that owns a
//! direct connection to the Hyprland IPC sockets. It answers request/response
//! queries over an `mpsc` of [`HyprReq`] and pushes `hypr:*` [`Event`]s onto the
//! shared broadcast bus.
//!
//! Mostly READ ONLY: active-window class/title/address and the full client
//! list, plus active-window / fullscreen change events. User-triggered,
//! one-shot compositor *actions* (`hyprctl dispatch exec/closewindow`)
//! deliberately stay shell-outs in the QML. The one exception is kiosk
//! workspace assignment: on `openwindow` this actor parks the new window on
//! its app's workspace (see [`crate::workspaces`]), which is what makes the
//! invariant "exactly one app fills the screen" STRUCTURAL — two apps on
//! different workspaces cannot share the screen, so there is nothing to
//! re-assert afterwards.
//!
//! This replaced a continuous fullscreen backstop that re-fullscreened
//! whatever Hyprland considered active on `openwindow`, `closewindow`,
//! `movewindowv2`, and `activewindowv2`. That backstop resolved its target
//! from the *active window*, which names a stale toplevel while the shell's
//! layer surface is on screen — hence the `shell-focus` gate it needed, and
//! the "launched Steam, Plex came to the front" bug the gate existed to
//! prevent. Assignment has no such ambiguity: it acts on the address the
//! event itself names.
//!
//! It has to live here rather than in QML because it must react to Hyprland's
//! own event stream — which this actor already owns — and it must fire even
//! if Quickshell is slow to start or has crashed, including for windows the
//! shell never launched (Steam spawns `streaming_client` on its own).
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
use crate::workspaces;
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
    /// `hypr-display-state` -> everything the Display & Audio page needs to
    /// render its mode/VRR controls in one round trip: the live monitors, the
    /// `monitor=` line each output is configured with, and any change still
    /// awaiting confirmation. See [`display_state_json`].
    DisplayState(Reply),
    /// `hypr-set-mode <NAME> <WxH@R>` -> apply the mode and arm the revert
    /// timer.
    SetMode {
        monitor: String,
        mode: String,
        reply: Reply,
    },
    /// `hypr-set-vrr <NAME> <0|1|2>` -> apply the per-output VRR mode and arm
    /// the revert timer.
    SetVrr {
        monitor: String,
        vrr: u8,
        reply: Reply,
    },
    /// `hypr-display-confirm` -> keep the pending change and persist it to
    /// `hyprland-local.conf`.
    DisplayConfirm(Reply),
    /// `hypr-display-revert` -> put the previous `monitor=` line back now,
    /// without waiting for the timer.
    DisplayRevert(Reply),
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

/// How long any single Hyprland request may take before it is abandoned.
///
/// The socket is local and Hyprland answers in single-digit milliseconds, so
/// this is not a latency budget — it is a liveness floor. `read_to_end` on a
/// connection Hyprland accepted but never writes to (or never closes) waits
/// forever.
///
/// The biggest win is not the park path but the ACTOR: `active_window_json`,
/// `clients_json`, `monitors_json` and `set_mode` are all awaited inline in the
/// actor's request loop, so a single hung read wedged the entire Hyprland actor
/// and left every pending IPC `oneshot` unanswered forever — the shell's window
/// reads, display queries and resume verification all dead at once, with nothing
/// logged. An await that can hang forever is a bug regardless of what triggers
/// it.
///
/// `/keyword` is deliberately exempt — see [`KEYWORD_TIMEOUT`].
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Send one command to Hyprland's request socket (`.socket.sock`) and return the
/// full response. The `j/` prefix asks Hyprland for JSON; the server writes the
/// reply and closes the connection, so we read to EOF.
///
/// Bounded by [`REQUEST_TIMEOUT`], and an expiry is logged rather than folded
/// into a generic error — a request that timed out and one that failed to
/// connect are different problems, and last time the journal could not tell them
/// apart because neither left a line.
async fn request(cmd: &str) -> Result<String> {
    request_within(cmd, REQUEST_TIMEOUT).await
}

/// [`request`] with an explicit budget, for the one command whose latency is
/// bounded by display hardware rather than by IPC.
async fn request_within(cmd: &str, budget: Duration) -> Result<String> {
    match tokio::time::timeout(budget, request_unbounded(cmd)).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                "hyprland: request {cmd:?} timed out after {}s; abandoning it",
                budget.as_secs()
            );
            Err(anyhow!("hyprland request {cmd:?} timed out"))
        }
    }
}

/// The unbounded body of [`request`]. Never call this directly — the timeout is
/// the point.
async fn request_unbounded(cmd: &str) -> Result<String> {
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
) -> Result<()> {
    {
        let events_tx = events_tx.clone();
        // Sticky class -> workspace map, owned OUTSIDE the reconnect loop so a
        // compositor restart (or a dropped event socket) doesn't reshuffle every
        // app's workspace under the user.
        let registry = Arc::new(Mutex::new(workspaces::Registry::new()));
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
                    Arc::clone(&registry),
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

    // The pending display change, owned by this actor (and shared with the
    // spawned revert timer). See `DisplayGuard`.
    let display = std::sync::Arc::new(DisplayGuard::default());

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
            HyprReq::DisplayState(reply) => {
                let _ = reply.send(display_state_json(&display).await);
            }
            HyprReq::SetMode {
                monitor,
                mode,
                reply,
            } => {
                let _ = reply.send(set_mode(&display, &monitor, &mode).await);
            }
            HyprReq::SetVrr {
                monitor,
                vrr,
                reply,
            } => {
                let _ = reply.send(set_vrr(&display, &monitor, vrr).await);
            }
            HyprReq::DisplayConfirm(reply) => {
                let _ = reply.send(confirm_display(&display));
            }
            HyprReq::DisplayRevert(reply) => {
                let _ = reply.send(revert_display(&display, RevertCause::Manual).await);
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

// ---------------------------------------------------------------------------
// Display mode: apply → confirm-or-revert → persist
// ---------------------------------------------------------------------------
//
// The pure half of this (parsing and rewriting a `monitor=` line, editing
// `hyprland-local.conf`) is `crate::display_mode`, which builds everywhere.
// What is here is the part that needs the compositor and a clock.
//
// ## Why every change is provisional
//
// The shell's display is a TV on a couch with no keyboard. A mode the panel
// (or the AVR in the chain) cannot lock leaves a black screen and no way to
// undo it short of SSH. So an applied change arms a timer: after
// `display_mode::REVERT_SECONDS` the previous `monitor=` line goes back on its
// own unless a `hypr-display-confirm` arrives first. Confirming is proof the
// picture survived — and it is the *only* path that writes the change to disk,
// so an unconfirmed mode can never come back after a reboot.
//
// The timer lives in the DAEMON, not in the panel's browser tab: the tab may
// be closed, backgrounded by a phone, or on the far side of a network the
// change itself broke. A revert that depends on the client being alive is not
// a safety net.
//
// One caveat, stated rather than papered over: if the daemon itself dies
// inside the window, the timer dies with it and the mode stays until the
// compositor restarts. Nothing was persisted, so a Hyprland restart still
// restores the configured line.

use crate::display_mode::{self, Mode};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A display change that has been applied but not yet confirmed.
#[derive(Debug, Clone)]
struct Pending {
    /// The output the change addresses.
    monitor: String,
    /// The `monitor=` value to restore on revert.
    previous: String,
    /// The `monitor=` value currently live.
    applied: String,
    /// Which arming this is. The revert timer carries the generation it was
    /// armed for and does nothing if it no longer matches, so a confirm (or a
    /// manual revert, or a later change) cannot be undone by a stale timer.
    generation: u64,
    /// When the timer fires.
    deadline: Instant,
}

/// The actor's pending-change slot, shared with the spawned revert timer.
#[derive(Default)]
struct DisplayGuard {
    inner: Mutex<GuardState>,
}

#[derive(Default)]
struct GuardState {
    generation: u64,
    pending: Option<Pending>,
}

impl DisplayGuard {
    fn lock(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record a freshly applied change and return its generation.
    fn arm(&self, monitor: &str, previous: &str, applied: &str) -> u64 {
        let mut g = self.lock();
        g.generation += 1;
        let generation = g.generation;
        g.pending = Some(Pending {
            monitor: monitor.to_string(),
            previous: previous.to_string(),
            applied: applied.to_string(),
            generation,
            deadline: Instant::now() + Duration::from_secs(display_mode::REVERT_SECONDS),
        });
        generation
    }

    fn peek(&self) -> Option<Pending> {
        self.lock().pending.clone()
    }

    /// Clear and return the pending change, unconditionally.
    fn take(&self) -> Option<Pending> {
        self.lock().pending.take()
    }

    /// Clear and return the pending change only if it is still `generation` —
    /// the check that makes a stale timer a no-op.
    fn take_generation(&self, generation: u64) -> Option<Pending> {
        let mut g = self.lock();
        match &g.pending {
            Some(p) if p.generation == generation => g.pending.take(),
            _ => None,
        }
    }
}

/// Why a revert happened, for the log line and the reply.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RevertCause {
    /// The operator asked for it.
    Manual,
    /// Nobody confirmed in time.
    Timeout,
}

impl RevertCause {
    fn as_str(self) -> &'static str {
        match self {
            RevertCause::Manual => "manual",
            RevertCause::Timeout => "timeout",
        }
    }
}

/// Issue one `hyprctl keyword`-equivalent over the request socket.
///
/// The leading `/` is the empty flag block Hyprland's request parser expects
/// (the same position `j/` occupies in the read queries above); `hyprctl`
/// itself sends exactly this form. Hyprland answers `ok` on success and an
/// error string otherwise — and it answers `ok` for a *parsed* keyword, so a
/// successful reply means "accepted", not "the display lit up". That gap is
/// precisely what the revert timer covers.
/// Budget for `/keyword`, which is NOT an IPC read.
///
/// A `keyword monitor …` triggers a real modeset — on this shell's target that
/// is a 4K120 HDR chain through an AV receiver, so the reply waits on an HDMI
/// re-handshake and can legitimately take seconds. Capping it at
/// [`REQUEST_TIMEOUT`] would be actively dangerous: `apply_change` returns early
/// on `Err` and therefore never arms the auto-revert timer, so a *slow but
/// successful* modeset would leave a live display change with nothing scheduled
/// to undo it — a black TV on a couch with no keyboard, which is the exact
/// outcome that timer exists to prevent. Generous enough that only a genuine
/// hang trips it.
const KEYWORD_TIMEOUT: Duration = Duration::from_secs(60);

async fn keyword(arg: &str) -> Result<()> {
    let reply = request_within(&format!("/keyword {arg}"), KEYWORD_TIMEOUT).await?;
    let trimmed = reply.trim();
    if trimmed.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(anyhow!("hyprland rejected `keyword {arg}`: {trimmed}"))
    }
}

/// Read `hyprland-local.conf`, or `""` when it does not exist yet (which is
/// the shipped state — `config/hyprland.conf` sources it optionally).
fn read_local_conf() -> String {
    std::fs::read_to_string(display_mode::local_conf_path()).unwrap_or_default()
}

/// The live `hypr-monitors` array, parsed.
async fn live_monitors() -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(&monitors_json().await).unwrap_or_default()
}

fn monitor_str<'a>(m: &'a Value, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The `monitor=` value a change should be built from: the configured line
/// when `hyprland-local.conf` declares one (it carries the bitdepth/HDR
/// arguments the live read cannot report), otherwise one synthesized from the
/// live monitor.
///
/// Returns the value plus whether it came from the config file — the panel
/// renders that difference, because a synthesized base means "this output has
/// no line of its own yet" and confirming will create one.
fn base_line(conf: &str, monitors: &[Value], name: &str) -> Result<(String, bool)> {
    if let Some(line) = display_mode::configured_line(conf, name) {
        return Ok((line, true));
    }
    let m = monitors
        .iter()
        .find(|m| monitor_str(m, "name") == name)
        .ok_or_else(|| anyhow!("no such output '{name}'"))?;
    let width = m.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = m.get("height").and_then(Value::as_u64).unwrap_or(0);
    let refresh = m.get("refreshRate").and_then(Value::as_f64).unwrap_or(0.0);
    let mode = Mode::parse(&format!("{width}x{height}@{refresh}"))
        .ok_or_else(|| anyhow!("output '{name}' reports no usable current mode"))?;
    Ok((
        display_mode::synthesize_line(
            name,
            mode,
            m.get("x").and_then(Value::as_i64).unwrap_or(0),
            m.get("y").and_then(Value::as_i64).unwrap_or(0),
            m.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
        ),
        false,
    ))
}

/// `hypr-display-state` — one document with everything the panel's controls
/// need, so a page render is a single round trip rather than three.
async fn display_state_json(guard: &DisplayGuard) -> String {
    let conf = read_local_conf();
    let conf_path = display_mode::local_conf_path();
    let monitors = live_monitors().await;

    // One merged entry per output: the live read, the configured line, and the
    // ready-to-render mode picker. Merging here rather than in the panel keeps
    // the panel a renderer — and keeps the picker it shows identical to the
    // list `set_mode` validates against.
    let displays: Vec<Value> = monitors
        .iter()
        .map(|m| {
            let name = monitor_str(m, "name");
            let current = Mode::parse(&format!(
                "{}x{}@{}",
                m.get("width").and_then(Value::as_u64).unwrap_or(0),
                m.get("height").and_then(Value::as_u64).unwrap_or(0),
                m.get("refreshRate").and_then(Value::as_f64).unwrap_or(0.0),
            ));
            let available: Vec<String> = m
                .get("availableModes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let options: Vec<Value> = display_mode::mode_options(&available, current)
                .into_iter()
                .map(|o| json!({"value": o.value, "label": o.label, "current": o.current}))
                .collect();
            let configured = display_mode::configured_line(&conf, name);
            let configured_vrr = configured.as_deref().and_then(display_mode::line_vrr);
            let vrr_active = m.get("vrr").and_then(Value::as_bool).unwrap_or(false);

            json!({
                "name": name,
                "description": monitor_str(m, "description"),
                "currentMode": current.map(Mode::to_keyword),
                "currentLabel": current.map(Mode::label),
                "currentFormat": monitor_str(m, "currentFormat"),
                "hdr": m.get("hdr").and_then(Value::as_bool).unwrap_or(false),
                "dpmsStatus": m.get("dpmsStatus").and_then(Value::as_bool).unwrap_or(true),
                // What Hyprland reports as ACTIVE right now (a bool), versus
                // what the config asks for (0/1/2). They differ legitimately:
                // mode 2 reads back as inactive outside a fullscreen window.
                "vrrActive": vrr_active,
                "configuredLine": configured,
                "configuredVrr": configured_vrr,
                "modes": options,
            })
        })
        .collect();

    let pending = guard.peek().map(|p| {
        json!({
            "monitor": p.monitor,
            "applied": p.applied,
            "previous": p.previous,
            "secondsRemaining": p.deadline.saturating_duration_since(Instant::now()).as_secs(),
        })
    });

    json!({
        "displays": displays,
        "pending": pending,
        "revertSeconds": display_mode::REVERT_SECONDS,
        "configPath": conf_path.display().to_string(),
        "configPresent": conf_path.exists(),
    })
    .to_string()
}

/// Apply `next` to `monitor` and arm the revert timer. Shared by
/// [`set_mode`] and [`set_vrr`], which differ only in how they derive `next`.
async fn apply_change(
    guard: &Arc<DisplayGuard>,
    monitor: &str,
    previous: &str,
    next: &str,
    from_conf: bool,
) -> String {
    if let Some(p) = guard.peek() {
        return crate::protocol::resp_error(&format!(
            "a display change on '{}' is already awaiting confirmation — confirm or revert it first",
            p.monitor
        ));
    }
    if next == previous {
        return json!({
            "ok": true,
            "monitor": monitor,
            "applied": next,
            "unchanged": true,
        })
        .to_string();
    }
    if let Err(e) = keyword(&format!("monitor {next}")).await {
        return crate::protocol::resp_error(&crate::protocol::sanitize_ipc(&e.to_string()));
    }

    let generation = guard.arm(monitor, previous, next);
    tracing::info!(
        monitor,
        applied = next,
        previous,
        revert_seconds = display_mode::REVERT_SECONDS,
        "display: applied a provisional monitor line; reverting unless confirmed"
    );

    {
        let guard = Arc::clone(guard);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(display_mode::REVERT_SECONDS)).await;
            // A confirm, a manual revert, or a newer change all clear or
            // supersede this generation, so the timer is a no-op unless the
            // same change is still waiting. Claiming the slot BEFORE awaiting
            // the compositor is what keeps a confirm landing mid-revert from
            // reporting success over a revert already in flight.
            if let Some(p) = guard.take_generation(generation) {
                let _ = restore(&p, RevertCause::Timeout).await;
            }
        });
    }

    json!({
        "ok": true,
        "monitor": monitor,
        "applied": next,
        "previous": previous,
        "fromConfig": from_conf,
        "revertSeconds": display_mode::REVERT_SECONDS,
    })
    .to_string()
}

/// `hypr-set-mode <NAME> <WxH@R>`.
async fn set_mode(guard: &Arc<DisplayGuard>, monitor: &str, mode: &str) -> String {
    let Some(mode) = Mode::parse(mode) else {
        return crate::protocol::resp_error(&format!(
            "unparseable mode '{mode}' — expected WIDTHxHEIGHT@REFRESH"
        ));
    };
    let conf = read_local_conf();
    let monitors = live_monitors().await;

    // Offer only what the output reports: a mode outside `availableModes` is
    // the class of input that blanks a TV, and the panel's <select> is not a
    // security boundary.
    let Some(m) = monitors.iter().find(|m| monitor_str(m, "name") == monitor) else {
        return crate::protocol::resp_error(&format!("no such output '{monitor}'"));
    };
    let supported = m
        .get("availableModes")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|s| s.as_str())
                .filter_map(Mode::parse)
                .any(|a| a.same(mode))
        })
        .unwrap_or(false);
    if !supported {
        return crate::protocol::resp_error(&format!(
            "output '{monitor}' does not report mode {} as available",
            mode.to_keyword()
        ));
    }

    let (previous, from_conf) = match base_line(&conf, &monitors, monitor) {
        Ok(v) => v,
        Err(e) => return crate::protocol::resp_error(&e.to_string()),
    };
    let Some(next) = display_mode::rewrite_mode(&previous, mode) else {
        return crate::protocol::resp_error(&format!(
            "configured line for '{monitor}' has no resolution field: {previous}"
        ));
    };
    apply_change(guard, monitor, &previous, &next, from_conf).await
}

/// `hypr-set-vrr <NAME> <0|1|2>`.
///
/// Written as the output's own `vrr` argument rather than `misc:vrr` — see
/// `crate::display_mode`'s module docs for why the global has no effect on a
/// configured output. The mode field is untouched, so toggling VRR cannot
/// move the display off 4K120.
async fn set_vrr(guard: &Arc<DisplayGuard>, monitor: &str, vrr: u8) -> String {
    if vrr > 2 {
        return crate::protocol::resp_error("vrr must be 0 (off), 1 (on) or 2 (fullscreen only)");
    }
    let conf = read_local_conf();
    let monitors = live_monitors().await;
    let (previous, from_conf) = match base_line(&conf, &monitors, monitor) {
        Ok(v) => v,
        Err(e) => return crate::protocol::resp_error(&e.to_string()),
    };
    let Some(next) = display_mode::rewrite_vrr(&previous, vrr) else {
        return crate::protocol::resp_error(&format!(
            "configured line for '{monitor}' is too short to carry a vrr argument: {previous}"
        ));
    };
    apply_change(guard, monitor, &previous, &next, from_conf).await
}

/// `hypr-display-confirm` — keep the live change and write it to
/// `hyprland-local.conf`.
///
/// **Confirmation is the only thing that persists.** A change nobody could see
/// to confirm never reaches disk, so the worst a bad pick costs is
/// [`display_mode::REVERT_SECONDS`] of black screen rather than a machine that
/// boots into one.
///
/// A failed write does not un-confirm: the mode is live and the operator asked
/// to keep it. The reply reports `persisted:false` with the error so the panel
/// can say "kept for this session, but not written".
fn confirm_display(guard: &DisplayGuard) -> String {
    let Some(p) = guard.take() else {
        return crate::protocol::resp_error("no display change is awaiting confirmation");
    };
    let path = display_mode::local_conf_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = display_mode::upsert_monitor_line(&text, &p.monitor, &p.applied);
    match crate::config::atomic_write(&path, updated) {
        Ok(()) => {
            tracing::info!(
                monitor = %p.monitor,
                line = %p.applied,
                path = %path.display(),
                "display: change confirmed and persisted"
            );
            json!({
                "ok": true,
                "monitor": p.monitor,
                "line": p.applied,
                "persisted": true,
                "configPath": path.display().to_string(),
            })
            .to_string()
        }
        Err(e) => {
            tracing::warn!(
                monitor = %p.monitor,
                path = %path.display(),
                error = %e,
                "display: change confirmed but could not be persisted"
            );
            json!({
                "ok": true,
                "monitor": p.monitor,
                "line": p.applied,
                "persisted": false,
                "persistError": e.to_string(),
                "configPath": path.display().to_string(),
            })
            .to_string()
        }
    }
}

/// `hypr-display-revert`, and the body of the timeout path.
async fn revert_display(guard: &DisplayGuard, cause: RevertCause) -> String {
    let Some(p) = guard.take() else {
        return crate::protocol::resp_error("no display change is awaiting confirmation");
    };
    restore(&p, cause).await
}

/// Put a pending change's previous line back. Shared by the manual and
/// timeout paths, both of which have already claimed the pending slot.
async fn restore(p: &Pending, cause: RevertCause) -> String {
    match keyword(&format!("monitor {}", p.previous)).await {
        Ok(()) => {
            tracing::info!(
                monitor = %p.monitor,
                restored = %p.previous,
                cause = cause.as_str(),
                "display: reverted to the previous monitor line"
            );
            json!({
                "ok": true,
                "monitor": p.monitor,
                "reverted": p.previous,
                "cause": cause.as_str(),
            })
            .to_string()
        }
        Err(e) => {
            tracing::error!(
                monitor = %p.monitor,
                restored = %p.previous,
                cause = cause.as_str(),
                error = %e,
                "display: REVERT FAILED — the display may be left on the provisional mode"
            );
            crate::protocol::resp_error(&crate::protocol::sanitize_ipc(&e.to_string()))
        }
    }
}

/// Watch Hyprland's event socket (`.socket2.sock`) and fan `hypr:*` events onto
/// the broadcast bus. Reads newline-delimited `EVENT>>DATA` lines. Returns when
/// the socket closes (the caller retries with backoff); errors propagate so the
/// caller logs and retries.
async fn watch_events(
    events_tx: broadcast::Sender<Event>,
    active_window_tx: watch::Sender<String>,
    registry: Arc<Mutex<workspaces::Registry>>,
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
    // Adopt windows that are ALREADY mapped. They produced their `openwindow`
    // before we were listening — on a daemon restart mid-session, or when the
    // daemon starts after the compositor — so without this they would keep
    // whatever workspace they happen to share and the kiosk invariant would only
    // become true again after a reboot. Runs on every (re)connect, because a
    // reconnect means we were deaf for a while and may have missed opens.
    reconcile_workspaces(&registry).await;
    // Window bookkeeping across output changes (see `MonitorWatch`). A
    // `watch_events` local on purpose: it dies with the connection, and a
    // reconnect runs `reconcile_workspaces` fresh above, so a snapshot stranded
    // by a mid-sequence reconnect is discarded rather than diffed against a
    // world that moved on.
    let mut monitor_watch = MonitorWatch::default();
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
                // Park the new window on its app's workspace. This is the ONLY
                // place the kiosk invariant is maintained now: one class per
                // workspace means two apps cannot share the screen, so there is
                // nothing left to re-assert afterwards.
                //
                // Note there is no shell-focus gate here, unlike the fullscreen
                // backstop this replaced. That gate existed because the backstop
                // resolved its target from Hyprland's *active window*, which names
                // a stale toplevel while the shell (a layer surface) is on screen —
                // so it could yank the screen to the wrong app. This resolves its
                // target from the event's own address and moves it SILENTLY, so it
                // can neither pick the wrong window nor change what is displayed.
                if let Some(address) = openwindow_address(data) {
                    let address = address.to_string();
                    let hint = openwindow_class(data).unwrap_or("").to_string();
                    let registry = Arc::clone(&registry);
                    spawn_park_window(registry, address, hint);
                } else {
                    // No spawn, and this used to be the quietest exit of all:
                    // `park_window` never ran, so not even its failure paths
                    // could log. A malformed or truncated `openwindow` line
                    // produced a window that was simply never parked, with the
                    // journal showing nothing at all.
                    tracing::warn!(
                        "hyprland: openwindow with no usable address; window left unparked (data {data:?})"
                    );
                }
                let json = parse_openwindow(data);
                let _ = events_tx.send(Event::HyprOpenWindow(json));
            }
            // `closewindow>>ADDRESS` — just the window address. Nothing to
            // repair: the closing window was alone on its workspace, so there is
            // no survivor for the tiler to re-split. (Under the stacked model this
            // arm had to re-fullscreen whatever Hyprland promoted next.)
            "closewindow" => {
                let _ = events_tx.send(Event::HyprCloseWindow(data.trim().to_string()));
            }
            // An output going away is not a cosmetic event on this box. The TV
            // sits behind an AV receiver, and a link drop (`drm: Connector
            // HDMI-A-1 disconnected`) DESTROYS the app windows on it while
            // their processes keep running — observed ~10x in one session, with
            // Quickshell falling back to a placeholder screen. The app is then
            // unreachable forever: there is no window left to switch to, and
            // until now the layout only repaired itself when the daemon was
            // restarted.
            //
            // Snapshot on the way out so the windows that do not come back can
            // be NAMED, rather than leaving a silent ghost that only shows up
            // later as audio from an app nobody can find.
            // Both forms are handled: Hyprland 0.56 emits `monitorremoved` and
            // `monitorremovedv2`, and which one arrives (or whether both do) is
            // not something to depend on. First-removal-wins makes the twin a
            // no-op, so handling both is free.
            "monitorremoved" | "monitorremovedv2" => {
                tracing::warn!(
                    "hyprland: monitor removed ({}); snapshotting windows to see which survive",
                    data.trim()
                );
                match request("j/clients").await {
                    Ok(body) => monitor_watch.on_removed(Instant::now(), client_index(&body)),
                    Err(e) => tracing::warn!(
                        "hyprland: could not snapshot windows before the output went away: {e}"
                    ),
                }
            }
            // Deliberately inline rather than spawned. This blocks the event
            // loop for the length of one reconcile, which is the right trade:
            // events queue in the socket buffer rather than being lost, and a
            // reconcile racing the events that follow it is how the layout ends
            // up half-repaired.
            "monitoradded" | "monitoraddedv2" => {
                tracing::info!("hyprland: monitor added ({})", data.trim());
                {
                    // An unreadable client list must not diff — see
                    // `MonitorWatch::on_added`. `Err` here is not rare noise: a
                    // monitor add lands mid-DRM-handshake, which is exactly when
                    // a request is most likely to time out.
                    let after = request("j/clients").await.ok().map(|b| client_index(&b));
                    for (address, class) in monitor_watch.on_added(Instant::now(), after.as_deref())
                    {
                        // `error`, not `warn`: the app is still running and still
                        // producing sound, but it can never be shown again. That
                        // is data loss waiting to happen (it is somebody's live
                        // game), and the process is deliberately NOT killed here
                        // — a wrong guess costs them their session.
                        tracing::error!(
                            "hyprland: window {class} ({address}) did not survive the output change; \
                             its process may still be running and unreachable"
                        );
                    }
                }
                // Debounced: Hyprland emits the v1 and v2 add together, and a
                // flapping connector arrives in bursts. Each reconcile is a
                // client read plus one dispatch per window, all awaited on this
                // reader — collapsing the burst is what keeps it from going deaf
                // while windows are still mapping.
                if monitor_watch.should_reconcile(Instant::now()) {
                    reconcile_workspaces(&registry).await;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Ceiling on one park attempt.
///
/// `park_window`'s own worst case is `resolve_window_class`'s retry loop —
/// 10 × (100ms sleep + a bounded request) — plus one dispatch, so this sits
/// above that. It is not a latency budget; it exists so "spawned and hung" is a
/// log line rather than a task that lives forever holding a window nobody can
/// reach.
const PARK_TIMEOUT: Duration = Duration::from_secs(60);

/// Spawn [`park_window`] so neither a panic nor a hang inside it can vanish.
///
/// A detached `tokio::spawn` discards its `JoinHandle`, and with it the task's
/// outcome. A panic is not literally silent — the default hook writes to stderr,
/// which is the journal under systemd — but it arrives with no attribution, so
/// nothing says WHICH window was left unparked. A hang is genuinely silent.
///
/// Both matter here because a window that fails to park is exactly the split
/// view this module exists to prevent, and the last time it happened the journal
/// could not distinguish "never spawned" from "spawned and stuck". This wrapper
/// makes both cases nameable. It does not claim to be the cause of that
/// incident — it removes the reason it was undiagnosable.
fn spawn_park_window(registry: Arc<Mutex<workspaces::Registry>>, address: String, hint: String) {
    tokio::spawn(async move {
        let logged = address.clone();
        let mut task = tokio::spawn(park_window(registry, address, hint));
        match tokio::time::timeout(PARK_TIMEOUT, &mut task).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("hyprland: park task for {logged} died: {e}"),
            Err(_) => {
                task.abort();
                tracing::error!(
                    "hyprland: park task for {logged} hung past {}s and was abandoned; \
                     the window may be sharing a workspace",
                    PARK_TIMEOUT.as_secs()
                );
            }
        }
    });
}

/// Park one freshly-mapped window on its app's workspace.
///
/// Spawned per `openwindow`. Everything here is best-effort, but it is
/// deliberately NOT fire-and-forget: this path has produced two invisible
/// failures already, so each step that can silently do nothing is checked.
///
/// **The class cannot be taken from the event.** Hyprland's `openwindow` carries
/// the class it knows at map time, and for XWayland windows that is frequently
/// EMPTY — X11 sets `WM_CLASS` on its own schedule, often just after the surface
/// maps. Observed in the field (2026-08-26): a Steam Remote Play window opened,
/// the event carried no class, this returned early, and the window stayed on
/// whatever workspace it happened to map onto — sharing a screen with Steam Big
/// Picture and making both switcher rows lead to the same place. The window's
/// `initialClass` read `streaming_client` moments later, which is why the race is
/// so easy to miss after the fact.
///
/// So the class is resolved from `j/clients` by address, retrying briefly, and
/// the event's value is only a fast path.
async fn park_window(registry: Arc<Mutex<workspaces::Registry>>, address: String, hint: String) {
    let class = match resolve_window_class(&address, &hint).await {
        Some(c) => c,
        None => {
            tracing::warn!(
                "hyprland: {address} never reported a window class; left unparked \
                 (it may share a workspace with another app)"
            );
            return;
        }
    };

    let assigned = registry
        .lock()
        .expect("workspace registry mutex poisoned")
        .assign(&class);
    let Some(ws) = assigned else {
        // Unreachable in practice (both resolve paths yield a non-empty class),
        // but it used to return in total silence. Every exit from this function
        // now says something — see the log-line note below.
        tracing::warn!("hyprland: {address} resolved to an empty class; left unparked");
        return;
    };

    if !dispatch_ok(&workspaces::move_command(ws, &address)).await {
        tracing::warn!("hyprland: failed to park {class} ({address}) on workspace {ws}");
        return;
    }
    // Deliberately `info`, not `debug`. A default deploy runs at info level, and
    // when this silently did nothing the journal held no record either way —
    // which turned a one-line bug into a live-debugging session.
    //
    // The ADDRESS is in the line for a reason learned the hard way. This used to
    // log only the class, so a park line could not be attributed to a window —
    // and when a split view turned up with `steam` and `streaming_client` sharing
    // a workspace, "there is no park line for that window" could not be
    // established from the journal at all. A line naming only the class is
    // indistinguishable from the same class being parked for a different window.
    tracing::info!("hyprland: parked {class} ({address}) on workspace {ws}");
}

/// The authoritative window class for `address`, or `None` if it never appears.
///
/// `hint` (the `openwindow` event's class field) short-circuits when non-empty.
/// Otherwise poll the client list — an XWayland window's `WM_CLASS` typically
/// lands within a frame or two of the map.
async fn resolve_window_class(address: &str, hint: &str) -> Option<String> {
    if !hint.is_empty() {
        return Some(hint.to_string());
    }
    const ATTEMPTS: u32 = 10;
    const INTERVAL: Duration = Duration::from_millis(100);
    for _ in 0..ATTEMPTS {
        tokio::time::sleep(INTERVAL).await;
        if let Some(class) = client_class(address).await {
            return Some(class);
        }
    }
    None
}

/// Look one window up in `j/clients` by address and return its non-empty class.
async fn client_class(address: &str) -> Option<String> {
    let body = request("j/clients").await.ok()?;
    let Value::Array(clients) = serde_json::from_str::<Value>(body.trim()).ok()? else {
        return None;
    };
    for c in clients {
        if c.get("address").and_then(Value::as_str) != Some(address) {
            continue;
        }
        let class = c.get("class").and_then(Value::as_str).unwrap_or("").trim();
        return (!class.is_empty()).then(|| class.to_string());
    }
    None
}

/// Whether a dispatch reply body means "accepted".
///
/// Hyprland answers a good dispatch with `ok`; a rejected one comes back as prose
/// ("Invalid workspace...", "Window not found..."). An empty body is treated as
/// success because some dispatchers reply with nothing at all — the goal is to
/// catch the REJECTION text, not to demand a specific acknowledgement.
fn dispatch_accepted(body: &str) -> bool {
    let reply = body.trim();
    reply.is_empty() || reply.eq_ignore_ascii_case("ok")
}

/// Send a dispatch and report whether Hyprland ACCEPTED it.
///
/// `request` only fails on a socket-level error; Hyprland reports a rejected
/// dispatch in the reply BODY and the transport still succeeds. Treating `Ok(_)`
/// as success is therefore the same class of mistake as trusting `hyprctl
/// dispatch`'s exit code, which is what made the original resume bug invisible.
async fn dispatch_ok(cmd: &str) -> bool {
    match request(cmd).await {
        Ok(body) => {
            if dispatch_accepted(&body) {
                true
            } else {
                tracing::warn!("hyprland: dispatch {cmd:?} rejected: {}", body.trim());
                false
            }
        }
        Err(e) => {
            tracing::warn!("hyprland: dispatch {cmd:?} failed: {e}");
            false
        }
    }
}

/// Park every already-mapped window on its class's workspace.
///
/// Idempotent by construction: a window already on the right workspace is moved
/// to the workspace it is already on, which Hyprland treats as a no-op.
/// Best-effort throughout — an unreadable client list or a failed dispatch
/// leaves the window where it is, which is exactly the pre-reconcile state.
///
/// Note this reads the CLIENT LIST and feeds the registry from it, rather than
/// the other way round. That ordering matters on a restart: the live windows are
/// the ground truth, and the registry is rebuilt to agree with them.
async fn reconcile_workspaces(registry: &Arc<Mutex<workspaces::Registry>>) {
    let body = match request("j/clients").await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("hyprland: workspace reconcile skipped, client list unreadable: {e}");
            return;
        }
    };
    let Ok(Value::Array(clients)) = serde_json::from_str::<Value>(body.trim()) else {
        return;
    };
    for c in clients {
        let class = c.get("class").and_then(Value::as_str).unwrap_or("").trim();
        let address = c.get("address").and_then(Value::as_str).unwrap_or("");
        if class.is_empty() || address.is_empty() {
            // An XWayland window that has not set WM_CLASS yet lands here. It is
            // a legitimate skip, but a SILENT one was indistinguishable from
            // "reconcile never saw this window" when a split view had to be
            // explained after the fact.
            tracing::debug!(
                "hyprland: reconcile skipped a client with no class/address (address {:?}, class {:?})",
                address,
                class
            );
            continue;
        }
        let assigned = registry
            .lock()
            .expect("workspace registry mutex poisoned")
            .assign(class);
        let Some(ws) = assigned else {
            continue;
        };
        // Address included for the same reason as in `park_window`: a line
        // naming only the class cannot be attributed to a window.
        if dispatch_ok(&workspaces::move_command(ws, address)).await {
            tracing::info!("hyprland: reconciled {class} ({address}) onto workspace {ws}");
        } else {
            tracing::warn!(
                "hyprland: reconcile failed to park {class} ({address}) on workspace {ws}"
            );
        }
    }
}

/// How long a pre-output-loss window snapshot stays meaningful.
///
/// A connector that is coming back comes back in seconds. Without a bound the
/// snapshot outlives the event that took it — TV off for the evening, the user
/// closes apps normally in the meantime, and the next `monitoradded` reports
/// every one of them as destroyed by an output change that happened hours ago.
const MONITOR_SNAPSHOT_TTL: Duration = Duration::from_secs(30);

/// Minimum gap between layout reconciles triggered by monitor events.
///
/// Hyprland emits the v1 and v2 forms of an add together, and a flapping
/// connector produces bursts (~10 disconnects in one observed session). Each
/// reconcile is a `j/clients` plus one dispatch per window, all awaited inline
/// on the event reader — so collapsing a burst into one pass is what keeps the
/// reader from going deaf while windows are mapping.
const MONITOR_RECONCILE_DEBOUNCE: Duration = Duration::from_secs(1);

/// Window bookkeeping across an output change.
///
/// Extracted from the event loop so the SEQUENCING is testable — which is where
/// the bugs live. The pure diff was never the risky part; "what happens when the
/// client list cannot be read", "what happens when two outputs drop before
/// either returns", and "what happens when the add never comes" are.
#[derive(Debug, Default)]
struct MonitorWatch {
    /// Windows seen just before an output went away, and when.
    snapshot: Option<(Instant, Vec<(String, String)>)>,
    /// When a monitor-triggered reconcile last ran.
    last_reconcile: Option<Instant>,
}

impl MonitorWatch {
    /// Record the pre-loss window set. **First removal wins**: on a two-output
    /// chain (both HDMI outs into one AV receiver) a link drop arrives as
    /// `remove(A), remove(B), add(A), add(B)`, and windows destroyed with A are
    /// already gone by the time B is removed. Overwriting would drop exactly the
    /// windows this exists to name.
    fn on_removed(&mut self, now: Instant, index: Vec<(String, String)>) {
        if self.snapshot.is_none() {
            self.snapshot = Some((now, index));
        }
    }

    /// Windows that did not survive, given the client list read after the add.
    ///
    /// `after` is `None` when the client list could not be read. That case must
    /// NOT diff: an unreadable list looks identical to "every window was
    /// destroyed", and reporting a live game as lost — in the one path whose
    /// whole purpose is a trustworthy journal — is worse than reporting nothing.
    /// The snapshot is kept so the next add can still try. An EMPTY list is
    /// treated the same way: a compositor that just gained an output and has no
    /// clients at all is a reading, not a state.
    fn on_added(
        &mut self,
        now: Instant,
        after: Option<&[(String, String)]>,
    ) -> Vec<(String, String)> {
        let Some((taken, before)) = self.snapshot.take() else {
            return Vec::new();
        };
        if now.duration_since(taken) > MONITOR_SNAPSHOT_TTL {
            return Vec::new();
        }
        let Some(after) = after.filter(|a| !a.is_empty()) else {
            // Put it back — this add told us nothing.
            self.snapshot = Some((taken, before));
            return Vec::new();
        };
        vanished_windows(&before, after)
    }

    /// Whether a reconcile should run now, or be collapsed into the last one.
    fn should_reconcile(&mut self, now: Instant) -> bool {
        let due = match self.last_reconcile {
            Some(last) => now.duration_since(last) >= MONITOR_RECONCILE_DEBOUNCE,
            None => true,
        };
        if due {
            self.last_reconcile = Some(now);
        }
        due
    }
}

/// Reduce a `j/clients` body to `(address, class)` pairs.
///
/// Split out from the reconcile/monitor paths so the window-set bookkeeping is
/// testable without a compositor. Entries missing an address are dropped — an
/// address is the only stable identity a window has — while an empty class is
/// KEPT, because an XWayland window that has not set `WM_CLASS` yet is exactly
/// the window most worth noticing when it disappears.
fn client_index(body: &str) -> Vec<(String, String)> {
    let Ok(Value::Array(clients)) = serde_json::from_str::<Value>(body.trim()) else {
        return Vec::new();
    };
    clients
        .iter()
        .filter_map(|c| {
            let address = c.get("address").and_then(Value::as_str)?.trim();
            if address.is_empty() {
                return None;
            }
            let class = c
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            Some((address.to_string(), class))
        })
        .collect()
}

/// Windows present in `before` and absent from `after`, matched by address.
///
/// Used across an output change to name the windows an HDMI link drop destroyed.
/// Matching on address rather than class is what makes it precise: two windows
/// of one class are distinct entries, so losing one of them is still reported.
fn vanished_windows(
    before: &[(String, String)],
    after: &[(String, String)],
) -> Vec<(String, String)> {
    before
        .iter()
        .filter(|(address, _)| !after.iter().any(|(seen, _)| seen == address))
        .cloned()
        .collect()
}

/// Extract the window CLASS from an `openwindow` event's raw data
/// (`ADDRESS,WORKSPACENAME,CLASS,TITLE`), or `None` when the field is missing
/// or empty.
///
/// The class is the app identity the workspace registry keys on, so an empty one
/// is rejected here rather than downstream: assigning a workspace to a classless
/// surface would move it somewhere the switcher can never reach.
fn openwindow_class(data: &str) -> Option<&str> {
    let class = data.split(',').nth(2)?.trim();
    (!class.is_empty()).then_some(class)
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
#[cfg(test)]
mod tests {
    use super::*;

    // The live shape, from `hyprctl -j clients` on the deploy box.
    fn clients_body() -> &'static str {
        r#"[{"address":"0x1","class":"tv.plex.Plex","workspace":{"id":2}},
            {"address":"0x2","class":"steam","workspace":{"id":4}},
            {"address":"0x3","class":"streaming_client","workspace":{"id":3}}]"#
    }

    #[test]
    fn client_index_pairs_address_with_class() {
        let idx = client_index(clients_body());
        assert_eq!(
            idx,
            vec![
                ("0x1".to_string(), "tv.plex.Plex".to_string()),
                ("0x2".to_string(), "steam".to_string()),
                ("0x3".to_string(), "streaming_client".to_string()),
            ]
        );
    }

    #[test]
    fn client_index_keeps_classless_windows_but_drops_addressless_ones() {
        // An XWayland window that has not set WM_CLASS yet is the window most
        // worth noticing when it vanishes, so it must survive indexing.
        let body = r#"[{"address":"0x9","class":""},{"class":"orphan"},{"address":"  "}]"#;
        assert_eq!(client_index(body), vec![("0x9".to_string(), String::new())]);
    }

    #[test]
    fn client_index_degrades_to_empty_on_junk() {
        assert!(client_index("").is_empty());
        assert!(client_index("not json").is_empty());
        assert!(client_index("{}").is_empty());
    }

    // The failure this exists for: an HDMI link drop destroys app windows while
    // their processes keep running, leaving an app that can never be shown again.
    #[test]
    fn vanished_windows_names_what_did_not_come_back() {
        let before = client_index(clients_body());
        let after = client_index(r#"[{"address":"0x1","class":"tv.plex.Plex"}]"#);
        assert_eq!(
            vanished_windows(&before, &after),
            vec![
                ("0x2".to_string(), "steam".to_string()),
                ("0x3".to_string(), "streaming_client".to_string()),
            ]
        );
    }

    #[test]
    fn vanished_windows_is_empty_when_everything_survives() {
        let before = client_index(clients_body());
        assert!(vanished_windows(&before, &before).is_empty());
        // A window ADDED across the change is not a loss.
        let mut after = before.clone();
        after.push(("0x4".to_string(), "newcomer".to_string()));
        assert!(vanished_windows(&before, &after).is_empty());
    }

    // Matched by address, not class: two windows of one class are distinct
    // destinations under the kiosk model, so losing one still has to report.
    #[test]
    fn vanished_windows_matches_on_address_not_class() {
        let before = vec![
            ("0xa".to_string(), "steam".to_string()),
            ("0xb".to_string(), "steam".to_string()),
        ];
        let after = vec![("0xa".to_string(), "steam".to_string())];
        assert_eq!(
            vanished_windows(&before, &after),
            vec![("0xb".to_string(), "steam".to_string())]
        );
    }

    // A window whose class only resolved AFTER the snapshot must not read as a
    // loss — it is the same window, and the address is what says so.
    #[test]
    fn vanished_windows_tolerates_a_class_resolving_late() {
        let before = vec![("0xc".to_string(), String::new())];
        let after = vec![("0xc".to_string(), "streaming_client".to_string())];
        assert!(vanished_windows(&before, &after).is_empty());
    }

    // A modeset on a 4K120 HDR chain behind an AV receiver can legitimately take
    // seconds. If `/keyword` shared the IPC budget, a slow-but-successful mode
    // change would return Err, `apply_change` would skip arming the auto-revert,
    // and a live change would have nothing scheduled to undo it — a black TV
    // with no keyboard. Pin the separation, not the numbers' exact values.
    #[test]
    fn keyword_gets_a_far_larger_budget_than_an_ipc_read() {
        assert!(KEYWORD_TIMEOUT >= REQUEST_TIMEOUT * 10);
    }

    // --- MonitorWatch: the sequencing, which is where the bugs are ---

    fn idx(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, c)| (a.to_string(), c.to_string()))
            .collect()
    }

    // THE false-alarm case. An unreadable client list is indistinguishable from
    // "every window was destroyed", and a monitor add lands mid-DRM-handshake —
    // exactly when a request is most likely to time out. Reporting a live game
    // as lost, in the one path whose purpose is a trustworthy journal, is worse
    // than reporting nothing.
    #[test]
    fn unreadable_client_list_reports_nothing_and_keeps_the_snapshot() {
        let mut w = MonitorWatch::default();
        let t0 = Instant::now();
        w.on_removed(t0, idx(&[("0x1", "steam"), ("0x2", "streaming_client")]));

        assert!(
            w.on_added(t0, None).is_empty(),
            "no diff on an unreadable list"
        );
        // An add that yields zero clients is a reading, not a state.
        assert!(
            w.on_added(t0, Some(&[])).is_empty(),
            "no diff on an empty list"
        );
        // The snapshot survived both, so a later good read still works.
        assert_eq!(
            w.on_added(t0, Some(&idx(&[("0x1", "steam")]))),
            idx(&[("0x2", "streaming_client")])
        );
    }

    // Both HDMI outs run into one AV receiver, so a link drop arrives as
    // remove(A), remove(B), add(A), add(B). Windows destroyed with A are already
    // gone when B is removed, so overwriting the snapshot would discard exactly
    // the windows this feature exists to name.
    #[test]
    fn first_removal_wins_so_a_second_output_cannot_erase_the_evidence() {
        let mut w = MonitorWatch::default();
        let t0 = Instant::now();
        w.on_removed(t0, idx(&[("0x1", "steam"), ("0x2", "streaming_client")]));
        // Second output drops; steam's window is already destroyed by now.
        w.on_removed(t0, idx(&[("0x2", "streaming_client")]));

        assert_eq!(
            w.on_added(t0, Some(&idx(&[("0x2", "streaming_client")]))),
            idx(&[("0x1", "steam")]),
            "the window lost with the FIRST output must still be reported"
        );
    }

    // TV off for the evening: apps get closed normally in between, and the next
    // add must not blame an output change that happened hours ago.
    #[test]
    fn a_stale_snapshot_is_discarded_rather_than_blamed() {
        let mut w = MonitorWatch::default();
        let t0 = Instant::now();
        w.on_removed(t0, idx(&[("0x1", "steam")]));
        let much_later = t0 + MONITOR_SNAPSHOT_TTL + Duration::from_secs(1);
        assert!(w.on_added(much_later, Some(&idx(&[]))).is_empty());
        // ...and it is gone, not lurking for the next add.
        assert!(w
            .on_added(much_later, Some(&idx(&[("0x9", "other")])))
            .is_empty());
    }

    #[test]
    fn an_add_with_no_prior_removal_reports_nothing() {
        let mut w = MonitorWatch::default();
        assert!(w
            .on_added(Instant::now(), Some(&idx(&[("0x1", "steam")])))
            .is_empty());
    }

    #[test]
    fn a_snapshot_is_consumed_by_the_add_that_uses_it() {
        let mut w = MonitorWatch::default();
        let t0 = Instant::now();
        w.on_removed(t0, idx(&[("0x1", "steam")]));
        assert_eq!(
            w.on_added(t0, Some(&idx(&[("0x2", "other")]))),
            idx(&[("0x1", "steam")])
        );
        // The v2 twin of the same add must not re-report it.
        assert!(w.on_added(t0, Some(&idx(&[("0x2", "other")]))).is_empty());
    }

    // Hyprland emits the v1 and v2 add together, and a flapping connector
    // arrives in bursts. Each reconcile is awaited on the event reader, so
    // collapsing the burst is what keeps the reader from going deaf while
    // windows are still mapping.
    #[test]
    fn reconciles_are_debounced_across_a_burst() {
        let mut w = MonitorWatch::default();
        let t0 = Instant::now();
        assert!(w.should_reconcile(t0), "the first one always runs");
        assert!(!w.should_reconcile(t0), "the v2 twin collapses into it");
        assert!(!w.should_reconcile(t0 + Duration::from_millis(100)));
        assert!(w.should_reconcile(t0 + MONITOR_RECONCILE_DEBOUNCE));
    }

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
    fn dispatch_accepted_recognises_success_and_rejection() {
        assert!(dispatch_accepted("ok"));
        assert!(dispatch_accepted("ok\n"));
        assert!(dispatch_accepted("OK"));
        // Some dispatchers answer with nothing; that is not a rejection.
        assert!(dispatch_accepted(""));
        assert!(dispatch_accepted("   "));

        // The cases that used to slip through as `Ok(_)`: the transport
        // succeeded, but Hyprland refused the dispatch in the body.
        assert!(!dispatch_accepted("Invalid workspace"));
        assert!(!dispatch_accepted("Window not found"));
        assert!(!dispatch_accepted("okay, but not really"));
    }

    #[test]
    fn openwindow_class_is_the_third_field() {
        // `openwindow>>ADDRESS,WORKSPACENAME,CLASS,TITLE`; the title is the
        // remainder and may itself contain commas.
        assert_eq!(
            openwindow_class("0x1,2,streaming_client,Red Dead Redemption 2 [Streaming]"),
            Some("streaming_client")
        );
        assert_eq!(
            openwindow_class("0x1,2,steam,Steam, Big Picture, Mode"),
            Some("steam")
        );
    }

    #[test]
    fn openwindow_class_rejects_malformed_or_empty() {
        assert_eq!(openwindow_class(""), None);
        assert_eq!(openwindow_class("0x1,2"), None);
        // An empty class must not reach the registry — it would burn a workspace
        // slot on a surface the user can never switch to.
        assert_eq!(openwindow_class("0x1,2,,Title"), None);
    }
}

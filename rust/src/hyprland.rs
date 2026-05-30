//! Hyprland compositor subsystem (Phase 4): a long-lived async actor that owns a
//! direct connection to the Hyprland IPC sockets. It answers request/response
//! queries over an `mpsc` of [`HyprReq`] and pushes `hypr:*` [`Event`]s onto the
//! shared broadcast bus.
//!
//! READ ONLY: active-window class/title/address and the full client list, plus
//! active-window / fullscreen change events. One-shot compositor *actions*
//! (`hyprctl dispatch exec/closewindow/focuswindow/fullscreen`) deliberately
//! stay shell-outs in the QML.
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
use tokio::sync::{broadcast, mpsc};

/// Requests from the IPC server to the Hyprland actor. Each carries a `oneshot`
/// reply with a fully-formatted wire string.
#[derive(Debug)]
pub enum HyprReq {
    /// `hypr-active` -> compact JSON object `{class,title,address}` (`{}` if no
    /// active window).
    Active(Reply),
    /// `hypr-clients` -> compact JSON array of `{class,title,address,workspace}`.
    Clients(Reply),
}

/// Resolve the Hyprland IPC socket directory for the current instance.
///
/// Hyprland >= 0.40 uses `$XDG_RUNTIME_DIR/hypr/<sig>`; older versions used
/// `/tmp/hypr/<sig>`. Prefer whichever actually exists, defaulting to the modern
/// path when neither is present yet (Hyprland may start after the daemon — the
/// connect attempt then fails and is retried).
fn socket_dir() -> Result<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .map_err(|_| anyhow!("HYPRLAND_INSTANCE_SIGNATURE not set"))?;
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

/// Run the Hyprland actor until `rx` is closed.
///
/// Owns the request socket queries, services [`HyprReq`]s, and (via a spawned
/// task) streams `.socket2.sock` events onto `events_tx`. Never panics: a
/// missing/closed socket degrades queries to an empty document and the event
/// watcher retries with capped backoff so `hypr:*` events self-heal if Hyprland
/// starts after the daemon or restarts later.
pub async fn run(
    mut rx: mpsc::Receiver<HyprReq>,
    events_tx: broadcast::Sender<Event>,
) -> Result<()> {
    {
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                match watch_events(events_tx.clone()).await {
                    Ok(()) => backoff = Duration::from_secs(1), // ended cleanly; re-attach
                    Err(e) => tracing::warn!("hyprland: event listener stopped: {e}; retrying"),
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
        }
    }

    tracing::info!("hyprland actor stopped");
    Ok(())
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
/// `{class,title,address}` wire contract. Empty body or no `class` -> `{}`.
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

/// Serialize the `{class,title,address}` subset of one window object.
fn active_entry(v: &Value) -> String {
    json!({
        "class": v.get("class").and_then(Value::as_str).unwrap_or(""),
        "title": v.get("title").and_then(Value::as_str).unwrap_or(""),
        "address": v.get("address").and_then(Value::as_str).unwrap_or(""),
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
    })
}

/// Watch Hyprland's event socket (`.socket2.sock`) and fan `hypr:*` events onto
/// the broadcast bus. Reads newline-delimited `EVENT>>DATA` lines. Returns when
/// the socket closes (the caller retries with backoff); errors propagate so the
/// caller logs and retries.
async fn watch_events(events_tx: broadcast::Sender<Event>) -> Result<()> {
    let sock = socket_dir()?.join(".socket2.sock");
    let stream = UnixStream::connect(&sock).await?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        let Some((event, data)) = line.split_once(">>") else {
            continue;
        };
        match event {
            // `activewindow>>class,title` — class is everything before the first
            // comma (a title may contain commas). Empty when focus is lost
            // (`activewindow>>,`), matching the empty-class wire contract.
            "activewindow" => {
                let class = data.split_once(',').map(|(c, _)| c).unwrap_or(data);
                let _ = events_tx.send(Event::HyprActiveWindow(class.to_string()));
            }
            // `fullscreen>>0|1`.
            "fullscreen" => {
                let _ = events_tx.send(Event::HyprFullscreen(data.trim() == "1"));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_reshapes_to_contract() {
        // Hyprland's j/activewindow is verbose; we keep only class/title/address.
        let body = r#"{"address":"0x55","class":"steam","title":"Steam, Big Picture","pid":42,"workspace":{"id":1,"name":"1"}}"#;
        let out = parse_active(body);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("class").unwrap(), "steam");
        assert_eq!(v.get("title").unwrap(), "Steam, Big Picture");
        assert_eq!(v.get("address").unwrap(), "0x55");
        assert!(v.get("pid").is_none()); // dropped
    }

    #[test]
    fn active_empty_and_malformed_become_empty_object() {
        assert_eq!(parse_active(""), "{}");
        assert_eq!(parse_active("{}"), "{}"); // no class
        assert_eq!(parse_active("not json"), "{}");
    }

    #[test]
    fn clients_reshapes_each_entry_with_workspace_name() {
        let body = r#"[{"address":"0x1","class":"foo","title":"Foo","workspace":{"id":2,"name":"web"}},
                       {"address":"0x2","class":"bar","title":"Bar","workspace":{"id":3,"name":"games"}}]"#;
        let out = parse_clients(body);
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].get("workspace").unwrap(), "web");
        assert_eq!(arr[1].get("class").unwrap(), "bar");
    }

    #[test]
    fn clients_non_array_becomes_empty_array() {
        assert_eq!(parse_clients("{}"), "[]");
        assert_eq!(parse_clients(""), "[]");
    }
}

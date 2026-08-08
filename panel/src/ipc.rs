//! [`IpcTransport`] — the Unix-socket [`NodeTransport`] implementation, and
//! the PRIMARY data tier for the panel's local node.
//!
//! Wire protocol (authoritative: `daemon/src/ipc.rs`): the client writes ONE
//! command line terminated by `\n` (e.g. `sys-status\n`); the daemon replies
//! with ONE `\n`-terminated line. The reply is either a JSON payload on a
//! single line, or a short text token (`ok`, `connected:grabbed` /
//! `disconnected:released` for `status`, `unknown`, or `error:<message>`).
//! Requests are capped at 4096 bytes by the daemon; replies can be large, so
//! this client reads until the first `\n` rather than using a fixed buffer.
//!
//! The module is `cfg(unix)`-gated because `AF_UNIX` is the whole premise. A
//! Windows panel would need `HttpTransport` (and a remote-node config surface)
//! instead — see `docs/MULTI_NODE_PANEL.md`; nothing else here is portable
//! today anyway (`config.rs` calls `libc::getuid`, `exec.rs` shells to
//! `systemctl`).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tv_shell_protocol::Capabilities;

use crate::transport::{NodeTransport, NodeTransportExt, Reachability, TransportError};

/// Default per-request timeout (connect + write + read-one-line).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// A [`NodeTransport`] speaking the daemon's Unix-socket IPC protocol.
pub struct IpcTransport {
    sock: PathBuf,
    timeout: Duration,
}

impl IpcTransport {
    /// Build a transport for the socket at `sock`, using [`DEFAULT_TIMEOUT`].
    pub fn new(sock: PathBuf) -> Self {
        Self {
            sock,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    async fn command_inner(&self, line: &str) -> Result<String, TransportError> {
        let mut stream = UnixStream::connect(&self.sock)
            .await
            .map_err(|_| TransportError::Unreachable)?;
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|_| TransportError::Unreachable)?;

        let mut reader = BufReader::new(stream);
        let mut reply = String::new();
        let n = reader
            .read_line(&mut reply)
            .await
            .map_err(|_| TransportError::Unreachable)?;
        if n == 0 {
            // EOF before any data — the daemon closed the connection.
            return Err(TransportError::Unreachable);
        }
        let reply = reply.trim_end().to_string();
        if let Some(msg) = reply.strip_prefix("error:") {
            return Err(TransportError::Command(msg.to_string()));
        }
        Ok(reply)
    }
}

#[async_trait]
impl NodeTransport for IpcTransport {
    /// The daemon's `capabilities` handshake — a single-line JSON reply, so
    /// this is exactly a `command_json` over the same socket as everything
    /// else (`docs/IPC_PROTOCOL.md` § `capabilities`).
    async fn capabilities(&self) -> Result<Capabilities, TransportError> {
        self.command_json::<Capabilities>("capabilities").await
    }

    /// Send `line` (without a trailing `\n` — one is appended) and return the
    /// daemon's reply line, with the `error:` prefix translated to
    /// `Err(TransportError::Command(_))`.
    async fn command(&self, line: &str) -> Result<String, TransportError> {
        tokio::time::timeout(self.timeout, self.command_inner(line))
            .await
            .unwrap_or(Err(TransportError::Timeout))
    }

    async fn command_timeout(
        &self,
        line: &str,
        timeout: Duration,
    ) -> Result<String, TransportError> {
        tokio::time::timeout(timeout, self.command_inner(line))
            .await
            .unwrap_or(Err(TransportError::Timeout))
    }

    fn reachability(&self) -> Reachability {
        Reachability::LocalSocket(self.sock.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;

    #[derive(Debug, Deserialize, PartialEq)]
    struct SysStatus {
        os: String,
        kernel: String,
        hostname: String,
        uptime: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Pad {
        id: String,
        index: u32,
        name: String,
        grabbed: bool,
    }

    /// Spawn a one-shot fake daemon: accept a single connection, read one
    /// request line, reply with `response` (a trailing `\n` is appended),
    /// then close.
    ///
    /// Uses `/tmp` directly (short and stable) rather than
    /// `std::env::temp_dir()` — on macOS that resolves to a deep
    /// per-process `/var/folders/...` path that, combined with a
    /// descriptive test-socket name, can exceed `sockaddr_un`'s ~104-byte
    /// `SUN_LEN` limit.
    fn spawn_fake_daemon(name: &str, response: &'static str) -> PathBuf {
        let sock = PathBuf::from(format!(
            "/tmp/tvshp-{name}-{}-{}.sock",
            std::process::id(),
            uniquifier()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind fake daemon socket");
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                let _ = reader.read_line(&mut line).await;
                let _ = write_half
                    .write_all(format!("{response}\n").as_bytes())
                    .await;
            }
        });
        sock
    }

    /// Like [`spawn_fake_daemon`], but also captures the exact request line
    /// it received into the returned `Arc<Mutex<Option<String>>>` so a test
    /// can assert on it (e.g. `set_config`'s serialized JSON body).
    fn spawn_fake_daemon_capture(
        name: &str,
        response: &'static str,
    ) -> (PathBuf, Arc<Mutex<Option<String>>>) {
        let sock = PathBuf::from(format!(
            "/tmp/tvshp-{name}-{}-{}.sock",
            std::process::id(),
            uniquifier()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind fake daemon socket");
        let captured = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                let _ = reader.read_line(&mut line).await;
                *captured_clone.lock().unwrap() = Some(line.trim_end().to_string());
                let _ = write_half
                    .write_all(format!("{response}\n").as_bytes())
                    .await;
            }
        });
        (sock, captured)
    }

    /// Tiny non-cryptographic uniquifier so parallel tests don't collide on
    /// the same socket path (no extra dependency needed). Kept short (a
    /// small hex counter) to leave room under the `SUN_LEN` path limit.
    fn uniquifier() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    #[tokio::test]
    async fn status_happy_path_text() {
        let sock = spawn_fake_daemon("status", "connected:grabbed");
        // Give the listener a moment to be ready to accept.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let reply = client.command("status").await.unwrap();
        assert_eq!(reply, "connected:grabbed");
    }

    #[tokio::test]
    async fn sys_status_happy_path_json() {
        let sock = spawn_fake_daemon(
            "sys-status",
            r#"{"os":"x","kernel":"y","hostname":"z","uptime":"1h"}"#,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let status: SysStatus = client.command_json("sys-status").await.unwrap();
        assert_eq!(
            status,
            SysStatus {
                os: "x".into(),
                kernel: "y".into(),
                hostname: "z".into(),
                uptime: "1h".into(),
            }
        );
    }

    #[tokio::test]
    async fn get_pads_json_array() {
        let sock = spawn_fake_daemon(
            "get-pads",
            r#"[{"id":"uniq:a","index":0,"name":"Pad","grabbed":true}]"#,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let pads: Vec<Pad> = client.command_json("get-pads").await.unwrap();
        assert_eq!(
            pads,
            vec![Pad {
                id: "uniq:a".into(),
                index: 0,
                name: "Pad".into(),
                grabbed: true,
            }]
        );
    }

    #[tokio::test]
    async fn error_line_maps_to_command_error() {
        let sock = spawn_fake_daemon("error", "error:input-runtime-down");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let err = client.command("status").await.unwrap_err();
        match err {
            TransportError::Command(msg) => assert_eq!(msg, "input-runtime-down"),
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_config_happy_path() {
        let sock = spawn_fake_daemon("get-config", r#"{"themeMode":"dark","rumbleEnabled":true}"#);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let cfg = client.get_config().await.unwrap();
        assert_eq!(cfg["themeMode"], "dark");
        assert_eq!(cfg["rumbleEnabled"], true);
    }

    #[tokio::test]
    async fn set_config_happy_path_sends_expected_request_line() {
        // The real daemon echoes the merged document on success, not a bare
        // `ok` — set_config() must treat any non-error reply as success, so
        // exercise that with a realistic echoed-document reply.
        let (sock, captured) = spawn_fake_daemon_capture("set-config", r#"{"themeMode":"light"}"#);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let patch = serde_json::json!({"themeMode": "light"});
        client.set_config(&patch).await.unwrap();
        let sent = captured.lock().unwrap().clone().unwrap();
        assert_eq!(sent, r#"set-config {"themeMode":"light"}"#);
    }

    #[tokio::test]
    async fn set_config_error_reply_maps_to_command_error() {
        let sock = spawn_fake_daemon(
            "set-config-err",
            "error:set-config body must be a JSON object",
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let err = client.set_config(&serde_json::json!({})).await.unwrap_err();
        match err {
            TransportError::Command(msg) => {
                assert_eq!(msg, "set-config body must be a JSON object")
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn daemon_down_is_unreachable() {
        let sock = PathBuf::from(format!(
            "/tmp/tvshp-nonexistent-{}-{}.sock",
            std::process::id(),
            uniquifier()
        ));
        let client = IpcTransport::new(sock);
        let err = client.command("status").await.unwrap_err();
        assert!(err.is_unreachable(), "expected unreachable, got {err:?}");
    }

    /// The `capabilities` handshake over the real wire protocol — same
    /// one-shot fake-daemon harness as every other command here, because it
    /// *is* just another single-line JSON reply.
    #[tokio::test]
    async fn capabilities_parses_the_daemon_handshake() {
        let sock = spawn_fake_daemon(
            "caps",
            r#"{"node_id":"htpc-1","kind":"shell","agent_version":"0.2.2","platform":"linux","features":["shell.intent","shell.screenshot"]}"#,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        let client = IpcTransport::new(sock);
        let caps = client.capabilities().await.unwrap();
        assert_eq!(caps.node_id, "htpc-1");
        assert_eq!(caps.kind, tv_shell_protocol::NodeKind::Shell);
        assert_eq!(caps.platform, tv_shell_protocol::Platform::Linux);
        assert_eq!(caps.features.len(), 2);
    }

    /// A down daemon degrades the handshake to `Unreachable` — it must not
    /// panic, and it must not be mistaken for "a node with no features".
    #[tokio::test]
    async fn capabilities_on_a_down_daemon_is_unreachable() {
        let sock = PathBuf::from(format!(
            "/tmp/tvshp-caps-down-{}-{}.sock",
            std::process::id(),
            uniquifier()
        ));
        let client = IpcTransport::new(sock);
        let err = client.capabilities().await.unwrap_err();
        assert!(err.is_unreachable(), "expected unreachable, got {err:?}");
    }

    /// `reachability()` is a static descriptor: it reports the configured
    /// socket path and performs no I/O, so it answers the same for a socket
    /// nothing is listening on.
    #[tokio::test]
    async fn reachability_reports_the_configured_socket_without_probing() {
        let sock = PathBuf::from("/tmp/tvshp-never-bound.sock");
        let client = IpcTransport::new(sock.clone());
        assert_eq!(client.reachability(), Reachability::LocalSocket(sock));
        assert!(client.command("status").await.is_err());
        assert_eq!(
            client.reachability(),
            Reachability::LocalSocket(PathBuf::from("/tmp/tvshp-never-bound.sock"))
        );
    }
}

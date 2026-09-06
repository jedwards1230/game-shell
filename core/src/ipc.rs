//! Unix-socket IPC server.
//!
//! Same shape as `daemon/src/ipc.rs`, because §4 carries the framing and reply
//! grammar over unchanged: `LinesCodec` with a 4096-byte cap, one tokio task per
//! connection, accept errors logged and never fatal, the socket bound 0600 under
//! a tightened umask so it is private from the instant it exists.
//!
//! The socket path is config/env driven and **must not collide with v1's** (§11:
//! v1 and v2 share no socket, prefix or unit name), which
//! [`crate::config::socket_path`] guarantees.
//!
//! One task per connection means intents can arrive CONCURRENTLY, and a
//! base-layer switch is a write plus a verify that must not interleave with
//! another one (a verify that can observe a racing intent's window is not a
//! verify). Nothing here serializes them — the implementation does, behind the
//! trait: `GamescopeCompositor` wraps every switch in a
//! [`crate::baselayer::IntentGate`].
//!
//! Compositor work is behind the [`Compositor`] trait for one reason: it makes
//! the whole request/reply surface testable end-to-end with no X server, the way
//! v1's `fake_runtime` makes its IPC testable with no evdev. Every X call blocks,
//! so it runs on `spawn_blocking` rather than on the reactor.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::{Framed, LinesCodec};

use crate::atoms::AppId;
use crate::protocol::{self, Command};

/// Everything the IPC layer needs from the compositor.
///
/// Blocking by design — these are X round trips — so callers run them on the
/// blocking pool. Each method returns the finished reply line rather than a
/// domain type, so the one place a failure could be turned into `ok` by accident
/// is the one place that is unit-tested for it.
pub trait Compositor: Send + Sync + 'static {
    /// A `ScreenState` snapshot as compact JSON, or an `error:` line.
    fn screen_state(&self) -> String;
    /// Put an app on screen, verifying the switch. `ok` only if it took.
    fn show(&self, app_id: AppId) -> String;
    /// Return to the shell, verifying the switch.
    fn home(&self) -> String;
    /// Launch a command in a scope for an app id.
    fn launch(&self, app_id: AppId, command: &[String]) -> String;
}

/// Bind the socket (removing any stale file), chmod 0600, and serve forever.
pub async fn serve(sock_path: String, compositor: Arc<dyn Compositor>) -> Result<()> {
    let listener = bind(&sock_path)?;
    tracing::info!("listening on {sock_path}");
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let compositor = Arc::clone(&compositor);
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, compositor).await {
                        tracing::debug!("client connection ended: {e}");
                    }
                });
            }
            // Never fatal: one bad accept must not take the control surface down.
            Err(e) => tracing::warn!("accept error: {e}"),
        }
    }
}

/// Bind the listener with the v1 umask trick.
fn bind(sock_path: &str) -> Result<UnixListener> {
    // Stale socket: unconditional removal, error ignored — a leftover file from
    // a killed core must not stop the next one from starting.
    let _ = std::fs::remove_file(sock_path);
    // Create the socket private from the instant it exists. Binding then
    // chmod'ing leaves a TOCTOU window in which the socket carries umask-
    // dependent (possibly world-accessible) permissions and another local
    // process could connect. A 0o177 umask makes the kernel create the node
    // 0o600 atomically at bind; the explicit set_permissions below is then a
    // belt-and-braces assertion, since a umask can only clear bits.
    //
    // SAFETY: `umask` is a plain process-global setter that cannot fail. It is
    // process-global, though, and NOTHING here excludes other threads — tokio's
    // workers are already running by the time `serve` is called, so a file
    // another thread creates inside this window really does inherit 0o177. Two
    // reasons that is acceptable rather than merely tolerated: the window is a
    // single `bind()` call wide, and it fails CLOSED — the only effect is
    // over-restrictive permissions on an unrelated file, never over-permissive
    // ones, so it cannot leak anything. The v1 daemon does exactly this.
    let prev_umask = unsafe { libc::umask(0o177) };
    let bind_result = UnixListener::bind(sock_path);
    unsafe {
        libc::umask(prev_umask);
    }
    let listener = bind_result.with_context(|| format!("binding unix socket at {sock_path}"))?;
    std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0o600 on {sock_path}"))?;
    Ok(listener)
}

/// One command per line, one reply per command, until the client goes away.
async fn handle_client(stream: UnixStream, compositor: Arc<dyn Compositor>) -> Result<()> {
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(protocol::MAX_LINE));
    while let Some(line) = framed.next().await {
        let line = line.context("reading command line")?;
        let reply = dispatch(&compositor, Command::parse(&line)).await;
        framed.send(reply).await?;
    }
    Ok(())
}

/// Resolve a command to its reply line.
pub async fn dispatch(compositor: &Arc<dyn Compositor>, cmd: Command) -> String {
    match cmd {
        Command::Ping => protocol::resp_ok(),
        Command::ScreenState => {
            let c = Arc::clone(compositor);
            blocking(move || c.screen_state()).await
        }
        Command::Show(raw) => match raw.parse::<AppId>() {
            Ok(app_id) => {
                let c = Arc::clone(compositor);
                blocking(move || c.show(app_id)).await
            }
            Err(_) => protocol::resp_error(&format!("not an app id: {raw}")),
        },
        Command::ShowUsage => protocol::resp_usage("show <appid>"),
        Command::Home => {
            let c = Arc::clone(compositor);
            blocking(move || c.home()).await
        }
        Command::Launch { app_id, command } => match app_id.parse::<AppId>() {
            Ok(app_id) => {
                let c = Arc::clone(compositor);
                blocking(move || c.launch(app_id, &command)).await
            }
            Err(_) => protocol::resp_error(&format!("not an app id: {app_id}")),
        },
        Command::LaunchUsage => protocol::resp_usage("launch <appid> [cmd args...]"),
        Command::Unknown => protocol::resp_unknown(),
    }
}

/// Run a blocking compositor call off the reactor, degrading to an error reply.
async fn blocking<F>(f: F) -> String
where
    F: FnOnce() -> String + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| protocol::resp_error(&format!("internal task failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A stand-in compositor, so the IPC layer is exercised end-to-end with no
    /// X server — the same role `fake_runtime` plays in the v1 daemon's tests.
    struct FakeCompositor {
        /// When set, `show`/`home` report the switch as not observed.
        refuse_switch: bool,
    }

    impl Compositor for FakeCompositor {
        fn screen_state(&self) -> String {
            protocol::resp_json(&serde_json::json!({"focused_window": 8388625}))
        }
        fn show(&self, app_id: AppId) -> String {
            if self.refuse_switch {
                // The shape that must never be `ok`.
                protocol::resp_error(&format!("base layer did not take: asked for app {app_id}"))
            } else {
                protocol::resp_ok()
            }
        }
        fn home(&self) -> String {
            self.show(AppId::new(9001))
        }
        fn launch(&self, app_id: AppId, command: &[String]) -> String {
            protocol::resp_json(&serde_json::json!({
                "app_id": app_id.get(),
                "command": command,
            }))
        }
    }

    fn fake(refuse_switch: bool) -> Arc<dyn Compositor> {
        Arc::new(FakeCompositor { refuse_switch })
    }

    async fn reply(c: &Arc<dyn Compositor>, line: &str) -> String {
        dispatch(c, Command::parse(line)).await
    }

    #[tokio::test]
    async fn dispatch_covers_the_verb_table() {
        let c = fake(false);
        assert_eq!(reply(&c, "ping").await, "ok");
        assert_eq!(
            reply(&c, "screen-state").await,
            r#"{"focused_window":8388625}"#
        );
        assert_eq!(reply(&c, "show 9003").await, "ok");
        assert_eq!(reply(&c, "home").await, "ok");
        assert_eq!(
            reply(&c, "launch 9003 moonlight stream").await,
            r#"{"app_id":9003,"command":["moonlight","stream"]}"#
        );
    }

    #[tokio::test]
    async fn usage_and_unknown_are_distinguished() {
        let c = fake(false);
        assert_eq!(reply(&c, "show").await, "error:usage: show <appid>");
        assert_eq!(
            reply(&c, "launch").await,
            "error:usage: launch <appid> [cmd args...]"
        );
        assert_eq!(reply(&c, "frobnicate").await, "unknown");
        assert_eq!(reply(&c, "hypr-active").await, "unknown");
    }

    #[tokio::test]
    async fn a_bad_app_id_is_rejected_before_the_compositor_is_touched() {
        let c = fake(false);
        assert_eq!(reply(&c, "show nine").await, "error:not an app id: nine");
        assert_eq!(reply(&c, "show -1").await, "error:not an app id: -1");
        assert_eq!(
            reply(&c, "launch nine moonlight").await,
            "error:not an app id: nine"
        );
    }

    /// **This proves the IPC layer forwards an error, and nothing more.**
    ///
    /// `FakeCompositor::show` hardcodes the error string, so this test cannot
    /// tell whether a real timeout produces one — invert
    /// `baselayer::write_and_verify`'s `Err` to an `Ok` and this still passes.
    /// That gap is why `baselayer` grew a `BaseLayer` seam and
    /// `a_switch_that_never_takes_is_never_ok`, which is the actual guard. This
    /// one covers the layer above it: that `dispatch` does not turn an `error:`
    /// reply into `ok` on the way out.
    #[tokio::test]
    async fn the_ipc_layer_forwards_a_failed_switch_as_an_error() {
        let c = fake(true);
        let r = reply(&c, "show 9003").await;
        assert!(r.starts_with("error:"), "{r}");
        assert_ne!(r, "ok");
        let r = reply(&c, "home").await;
        assert!(r.starts_with("error:"), "{r}");
    }

    #[tokio::test]
    async fn no_reply_ever_contains_a_newline() {
        let c = fake(true);
        for line in ["ping", "show", "show 9003", "home", "launch", "frobnicate"] {
            let r = reply(&c, line).await;
            assert!(!r.contains('\n'), "{line} -> {r:?}");
        }
    }

    async fn send_line(stream: &mut UnixStream, line: &str) -> String {
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        // Replies are newline-framed; read to the first '\n' so a long JSON
        // reply is never truncated mid-document by a fixed-size read.
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
    async fn end_to_end_over_a_real_socket() {
        // Deliberately the short system temp dir and not a deep scratch path:
        // this binds a real Unix-domain socket and `sockaddr_un::sun_path` caps
        // the path at ~104 bytes. Same exception the v1 daemon documents.
        let sock = std::env::temp_dir()
            .join(format!("tv-core-ipc-test-{}.sock", std::process::id()))
            .to_string_lossy()
            .to_string();
        let server = tokio::spawn(serve(sock.clone(), fake(false)));

        for _ in 0..100 {
            if std::path::Path::new(&sock).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut s = UnixStream::connect(&sock).await.unwrap();
        assert_eq!(send_line(&mut s, "ping").await, "ok");
        assert_eq!(send_line(&mut s, "show 9003").await, "ok");
        assert_eq!(send_line(&mut s, "show").await, "error:usage: show <appid>");
        assert_eq!(send_line(&mut s, "nope").await, "unknown");
        // Several commands on one connection, as v1 allows.
        assert_eq!(send_line(&mut s, "home").await, "ok");

        // The socket must be private the moment it exists.
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket must be 0600");

        server.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn a_stale_socket_file_does_not_stop_a_restart() {
        let sock = std::env::temp_dir()
            .join(format!("tv-core-stale-{}.sock", std::process::id()))
            .to_string_lossy()
            .to_string();
        std::fs::write(&sock, b"not a socket").unwrap();
        let listener = bind(&sock).expect("a stale file must be removed, not fatal");
        drop(listener);
        let _ = std::fs::remove_file(&sock);
    }
}

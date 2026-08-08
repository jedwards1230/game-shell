//! `NodeTransport` — the seam between the panel's pages and whatever speaks
//! for a node.
//!
//! The panel used to hold a concrete `IpcClient` and every page called it
//! directly. That hard-wires "a node" to "a Unix socket on this machine",
//! which is exactly the assumption `docs/MULTI_NODE_PANEL.md` needs to break:
//! a remote node is reached over HTTP, not over `AF_UNIX`. This module names
//! the thing the pages actually depend on — *send a command line to a node and
//! get its reply* — so a second implementation can be dropped in without
//! touching a handler.
//!
//! ## Why the trait is split in two
//!
//! [`AppState`](crate::state::AppState) holds `Arc<dyn NodeTransport>`, so the
//! base trait must stay **object-safe**: no generic methods. The generic and
//! derived helpers (`command_json::<T>`, `get_config`, `set_config`) therefore
//! live on [`NodeTransportExt`], which is blanket-implemented for every
//! `NodeTransport` (including `dyn NodeTransport`) and defined **purely in
//! terms of [`NodeTransport::command`]** — so an implementation cannot make
//! them behave differently, and their behavior is unchanged by construction.
//!
//! ## Why the command is a `&str`, not a typed `Command` enum
//!
//! The multi-node spec sketches `command(&self, cmd: &Command) -> Response`.
//! No such wire vocabulary exists to reuse: the daemon's `Command` enum is
//! private to `daemon/src/protocol.rs` and nothing in `protocol/` (the
//! cross-crate contract) models a command. Inventing one here would be a new
//! surface — and would rewrite the error handling at every call site — rather
//! than a refactor. The panel keeps speaking the line-based protocol it
//! already speaks (`docs/IPC_PROTOCOL.md`).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tv_shell_protocol::Capabilities;

/// Errors a single transport command can produce.
///
/// This is the former `ipc::IpcError`, renamed but otherwise untouched: same
/// variants, same [`Display`](std::fmt::Display) strings, same
/// [`is_unreachable`](TransportError::is_unreachable). Pages render off these
/// strings, so they are part of the panel's observable behavior.
#[derive(Debug)]
pub enum TransportError {
    /// The node could not be reached (connect refused, socket file missing,
    /// or the request/read timed out).
    Unreachable,
    /// The request timed out.
    Timeout,
    /// The node replied `error:<message>` — `<message>` is carried here
    /// (the `error:` prefix is stripped).
    Command(String),
    /// The reply could not be parsed as the expected type.
    Parse(String),
}

impl TransportError {
    /// `true` for the two "the node is not there" variants (`Unreachable` and
    /// `Timeout`), letting callers render a single degraded state for both.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, TransportError::Unreachable | TransportError::Timeout)
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Unreachable => write!(f, "daemon unreachable"),
            TransportError::Timeout => write!(f, "daemon request timed out"),
            TransportError::Command(msg) => write!(f, "{msg}"),
            TransportError::Parse(msg) => write!(f, "failed to parse daemon reply: {msg}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// How a transport addresses its node.
///
/// A **static descriptor, not a probe**: it reports the address form this
/// transport was configured with and performs no I/O. "Is the node up?" is
/// answered by actually calling it and reading the
/// [`TransportError`](TransportError::is_unreachable) — asking twice, once
/// cheaply and once for real, is how a panel ends up rendering a status that
/// disagrees with its own content.
///
/// Only the local-socket form exists today because `IpcTransport` is the only
/// implementation; the remote/HTTP form arrives with `HttpTransport` and the
/// multi-node work (`docs/MULTI_NODE_PANEL.md` §3).
///
/// No page reads this yet — the multi-node nav that will is PR 4 — so it is
/// exercised by the transport unit tests only. Same `#[allow(dead_code)]`
/// treatment (and the same reason) as
/// [`crate::config::shell_journal_tag`]: a landed surface whose consumer is a
/// later milestone, kept honest by a test rather than deleted and re-derived.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// Reached over a local Unix-domain socket at this path — i.e. the node is
    /// this machine.
    LocalSocket(PathBuf),
}

/// What the panel needs from *anything* that speaks for a node.
///
/// Object-safe on purpose — see the module docs. Implementations are expected
/// to be cheap to share (`AppState` keeps one behind an `Arc` for the process
/// lifetime) and to be internally stateless per request.
#[async_trait]
pub trait NodeTransport: Send + Sync {
    /// The node's capability handshake (`docs/IPC_PROTOCOL.md` §
    /// `capabilities`). PR 4 builds the nav and route registration from this;
    /// nothing gates on it yet, so no handler calls it and the transport unit
    /// tests are its only caller (hence `dead_code`, as on [`Reachability`]).
    #[allow(dead_code)]
    async fn capabilities(&self) -> Result<Capabilities, TransportError>;

    /// Send `line` (without a trailing newline — the transport appends its own
    /// framing) and return the node's reply, with an `error:<msg>` reply
    /// surfaced as [`TransportError::Command`].
    async fn command(&self, line: &str) -> Result<String, TransportError>;

    /// Like [`command`](Self::command), but with a caller-supplied `timeout`
    /// instead of the transport's default — for commands whose protocol-level
    /// wait can exceed it (e.g. `capture-next`, which blocks up to 10s
    /// server-side waiting for a gamepad button press; see
    /// `pages::controllers`).
    async fn command_timeout(
        &self,
        line: &str,
        timeout: Duration,
    ) -> Result<String, TransportError>;

    /// How this transport addresses its node. Static; does no I/O.
    /// Test-only caller today — see [`Reachability`].
    #[allow(dead_code)]
    fn reachability(&self) -> Reachability;
}

/// The generic and derived helpers, kept off the object-safe base trait.
///
/// Blanket-implemented for every [`NodeTransport`] — including
/// `dyn NodeTransport` — and written **only** in terms of
/// [`NodeTransport::command`], so no implementation can override them into
/// behaving differently.
#[async_trait]
pub trait NodeTransportExt: NodeTransport {
    /// Like [`NodeTransport::command`], but parse the reply as JSON into `T`.
    async fn command_json<T: serde::de::DeserializeOwned + Send>(
        &self,
        line: &str,
    ) -> Result<T, TransportError> {
        let reply = self.command(line).await?;
        serde_json::from_str(&reply).map_err(|e| TransportError::Parse(e.to_string()))
    }

    /// Fetch the full settings document (`~/.config/tv-shell/settings.json`)
    /// via `get-config`. Stateless on the daemon side: a missing or
    /// unparseable file yields `{}` rather than an error (see
    /// `docs/IPC_PROTOCOL.md` § `get-config`).
    async fn get_config(&self) -> Result<serde_json::Value, TransportError> {
        self.command_json("get-config").await
    }

    /// Shallow-merge `patch` into `settings.json` via `set-config
    /// <json-object>` (read-modify-write; a top-level key with a JSON `null`
    /// value deletes that key; foreign keys the caller omits — notably the
    /// daemon-owned `keyBindings` — are preserved untouched).
    ///
    /// Confirmed against `daemon/src/ipc.rs`'s `Command::SetConfig` handler
    /// (`dispatch_stateless`): on success the daemon replies with the **full
    /// merged document** as compact JSON (`config::set_config`'s `Ok(merged)`
    /// returned verbatim) — NOT a bare `ok`. On failure it replies
    /// `error:<msg>` (missing body, invalid JSON, non-object body, or a
    /// write failure), which `command()` already maps to
    /// `TransportError::Command`. This method treats any non-error reply as
    /// success and discards the echoed document — callers that need the
    /// post-merge state should call [`Self::get_config`] again.
    async fn set_config(&self, patch: &serde_json::Value) -> Result<(), TransportError> {
        let body = serde_json::to_string(patch).map_err(|e| {
            TransportError::Parse(format!("failed to serialize set-config patch: {e}"))
        })?;
        self.command(&format!("set-config {body}")).await?;
        Ok(())
    }
}

#[async_trait]
impl<T: NodeTransport + ?Sized> NodeTransportExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// One scripted outcome for a command line. A separate enum rather than a
    /// stored `Result<_, TransportError>` because `TransportError` is
    /// deliberately not `Clone` (it carries the daemon's own message and is
    /// meant to be consumed once), so each call mints a fresh error.
    enum Scripted {
        Reply(&'static str),
        Command(&'static str),
        Unreachable,
        Timeout,
    }

    /// A second [`NodeTransport`] implementation, entirely in memory.
    ///
    /// This is what stops the trait being single-impl indirection: it proves
    /// the panel's transport seam is really dynamic — something with no
    /// socket, no daemon, and no I/O at all satisfies it and is reachable
    /// through `Arc<dyn NodeTransport>`.
    struct FakeTransport {
        /// Command line → scripted outcome. A line with no entry answers
        /// `Unreachable`, matching how the real transport reports "nothing
        /// was listening".
        script: HashMap<&'static str, Scripted>,
        /// Every line this transport was asked to send, in order.
        seen: Mutex<Vec<String>>,
        reachability: Reachability,
    }

    impl FakeTransport {
        fn new(script: Vec<(&'static str, Scripted)>) -> Self {
            Self {
                script: script.into_iter().collect(),
                seen: Mutex::new(Vec::new()),
                reachability: Reachability::LocalSocket(PathBuf::from("/tmp/fake-transport.sock")),
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("fake transport log").clone()
        }

        fn respond(&self, line: &str) -> Result<String, TransportError> {
            self.seen
                .lock()
                .expect("fake transport log")
                .push(line.to_string());
            match self.script.get(line) {
                Some(Scripted::Reply(r)) => Ok((*r).to_string()),
                Some(Scripted::Command(msg)) => Err(TransportError::Command((*msg).to_string())),
                Some(Scripted::Timeout) => Err(TransportError::Timeout),
                Some(Scripted::Unreachable) | None => Err(TransportError::Unreachable),
            }
        }
    }

    #[async_trait]
    impl NodeTransport for FakeTransport {
        async fn capabilities(&self) -> Result<Capabilities, TransportError> {
            self.command_json::<Capabilities>("capabilities").await
        }

        async fn command(&self, line: &str) -> Result<String, TransportError> {
            self.respond(line)
        }

        async fn command_timeout(
            &self,
            line: &str,
            _timeout: Duration,
        ) -> Result<String, TransportError> {
            self.respond(line)
        }

        fn reachability(&self) -> Reachability {
            self.reachability.clone()
        }
    }

    /// The four `Display` strings pages render, and the two variants that mean
    /// "the node is not there". Pinned because the degraded views assert on
    /// this text — see `crate::tests`'s hermetic suite.
    #[test]
    fn transport_error_display_and_unreachable_are_stable() {
        assert_eq!(
            TransportError::Unreachable.to_string(),
            "daemon unreachable"
        );
        assert_eq!(
            TransportError::Timeout.to_string(),
            "daemon request timed out"
        );
        assert_eq!(TransportError::Command("boom".into()).to_string(), "boom");
        assert_eq!(
            TransportError::Parse("bad json".into()).to_string(),
            "failed to parse daemon reply: bad json"
        );

        assert!(TransportError::Unreachable.is_unreachable());
        assert!(TransportError::Timeout.is_unreachable());
        assert!(!TransportError::Command("boom".into()).is_unreachable());
        assert!(!TransportError::Parse("bad json".into()).is_unreachable());
    }

    /// The seam is dynamic: every base-trait and extension-trait method is
    /// reached through `Arc<dyn NodeTransport>`, never through a concrete type.
    #[tokio::test]
    async fn ext_helpers_work_through_dyn_dispatch() {
        let fake = Arc::new(FakeTransport::new(vec![
            ("status", Scripted::Reply("connected:grabbed")),
            ("get-pads", Scripted::Reply(r#"[{"name":"Pad"}]"#)),
            ("get-config", Scripted::Reply(r#"{"themeMode":"dark"}"#)),
            (
                r#"set-config {"themeMode":"light"}"#,
                Scripted::Reply(r#"{"themeMode":"light"}"#),
            ),
        ]));
        let node: Arc<dyn NodeTransport> = fake.clone();

        assert_eq!(node.command("status").await.unwrap(), "connected:grabbed");
        assert_eq!(
            node.command_timeout("status", Duration::from_secs(9))
                .await
                .unwrap(),
            "connected:grabbed"
        );

        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Pad {
            name: String,
        }
        let pads: Vec<Pad> = node.command_json("get-pads").await.unwrap();
        assert_eq!(pads, vec![Pad { name: "Pad".into() }]);

        assert_eq!(node.get_config().await.unwrap()["themeMode"], "dark");
        node.set_config(&serde_json::json!({"themeMode": "light"}))
            .await
            .unwrap();

        // `set_config` must serialize the patch into a single `set-config
        // <json>` line — the same wire form `IpcTransport`'s own test pins.
        assert_eq!(
            fake.seen(),
            vec![
                "status",
                "status",
                "get-pads",
                "get-config",
                r#"set-config {"themeMode":"light"}"#,
            ]
        );
    }

    /// The failure paths matter as much as the happy ones — the panel exists
    /// to work when the node is wedged.
    #[tokio::test]
    async fn error_variants_propagate_unchanged_through_the_trait() {
        let node: Arc<dyn NodeTransport> = Arc::new(FakeTransport::new(vec![
            ("wedged", Scripted::Timeout),
            ("refused", Scripted::Unreachable),
            ("bad-cmd", Scripted::Command("input-runtime-down")),
            ("not-json", Scripted::Reply("plain text")),
        ]));

        assert!(matches!(
            node.command("wedged").await.unwrap_err(),
            TransportError::Timeout
        ));
        assert!(matches!(
            node.command("refused").await.unwrap_err(),
            TransportError::Unreachable
        ));
        // Unscripted lines behave like a node that is simply not there.
        assert!(node
            .command("never-scripted")
            .await
            .unwrap_err()
            .is_unreachable());
        match node.command("bad-cmd").await.unwrap_err() {
            TransportError::Command(msg) => assert_eq!(msg, "input-runtime-down"),
            other => panic!("expected Command error, got {other:?}"),
        }
        // A non-JSON reply is a Parse error raised by the ext helper, not by
        // the implementation — proving the derived helpers really are defined
        // in terms of `command` alone.
        let parsed: Result<serde_json::Value, _> = node.command_json("not-json").await;
        assert!(matches!(parsed.unwrap_err(), TransportError::Parse(_)));
    }

    #[tokio::test]
    async fn capabilities_rides_the_command_path() {
        let node: Arc<dyn NodeTransport> = Arc::new(FakeTransport::new(vec![(
            "capabilities",
            Scripted::Reply(
                r#"{"node_id":"htpc-1","kind":"shell","agent_version":"0.2.2",
                    "platform":"linux","features":["shell.intent"]}"#,
            ),
        )]));

        let caps = node.capabilities().await.unwrap();
        assert_eq!(caps.node_id, "htpc-1");
        assert_eq!(caps.kind, tv_shell_protocol::NodeKind::Shell);
        assert_eq!(caps.platform, tv_shell_protocol::Platform::Linux);
        assert!(!caps.features.is_empty());
    }

    #[tokio::test]
    async fn capabilities_surfaces_a_down_node_rather_than_panicking() {
        let node: Arc<dyn NodeTransport> = Arc::new(FakeTransport::new(vec![]));
        assert!(node.capabilities().await.unwrap_err().is_unreachable());
    }

    /// `reachability()` is a static descriptor: it answers without any I/O, so
    /// it is identical before and after a call that fails.
    #[tokio::test]
    async fn reachability_is_static_and_does_no_probing() {
        let node: Arc<dyn NodeTransport> = Arc::new(FakeTransport::new(vec![]));
        let before = node.reachability();
        assert_eq!(
            before,
            Reachability::LocalSocket(PathBuf::from("/tmp/fake-transport.sock"))
        );
        assert!(node.command("anything").await.is_err());
        assert_eq!(node.reachability(), before);
    }
}

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

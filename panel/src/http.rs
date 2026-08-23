//! [`HttpTransport`] — the bearer-auth HTTP [`NodeTransport`] implementation,
//! and the panel's data tier for a **sidecar** node.
//!
//! `docs/MULTI_NODE_PANEL.md` §4: a shell node runs the panel locally (its
//! recovery tier is inherently local — you cannot `systemctl restart` a wedged
//! unit from another box). A **sidecar** node has no local tier worth
//! recovering, so it is served **remotely** by a Linux-built panel over HTTP.
//! That is what this module is for, and it is why no Windows panel build is on
//! the path.
//!
//! Wire protocol (authoritative: `host/src/main.rs`): seven routes, all but
//! `/art/{appid}` behind `Authorization: Bearer <token>`, all answering JSON.
//!
//! | Command line | Request |
//! |---|---|
//! | `capabilities` | `GET /capabilities` |
//! | `library` | `GET /library` |
//! | `status` | `GET /status` |
//! | `open-bpm` | `POST /open-bpm` (no body) |
//! | `sleep` | `POST /sleep` (no body) |
//! | `launch <appid>` | `POST /launch` `{"appid":<u32>}` |
//! | `quit <appid>` | `POST /quit` `{"appid":<u32>}` |
//!
//! ## Why these line names and not the daemon's `steam-*` ones
//!
//! The daemon exposes a *proxy* vocabulary for the same sidecar —
//! `steam-library`, `steam-launch <appid>`, `steam-quit <appid>`,
//! `steam-bigpicture`, `steam-suspend` (`docs/IPC_PROTOCOL.md`). Reusing those
//! names here would be a lie about the reply: `steam-library` answers a
//! daemon-shaped envelope (`{"status":…,"recentlyPlayed":[…],"allGames":[…],
//! "host":…}`) built by `daemon/src/steam.rs`, while `GET /library` answers the
//! sidecar's own `LibraryResponse` (`{"games":[…]}`). One name, two shapes, and
//! a caller that parsed one would silently mis-parse the other.
//!
//! Re-deriving the daemon's envelope in the panel is the other way to make the
//! names honest, and it is worse: it duplicates health-status and active-host
//! logic that is the daemon's, in a crate that cannot depend on the daemon.
//!
//! So this transport speaks the **sidecar's own vocabulary, named after its own
//! routes**, and [`NodeTransport::command`]'s docs now say plainly that the
//! trait fixes the shape of a command and not the vocabulary. See that method
//! for why the split is the capability split rather than an abstraction leak
//! worth closing.
//!
//! ## Not gated on `cfg(unix)`
//!
//! Unlike [`crate::ipc`], nothing here is Unix-specific — `reqwest` builds
//! everywhere. The panel crate as a whole still does not build on Windows (see
//! `docs/MULTI_NODE_PANEL.md` §2's blocker list), and this module does not
//! change that; it removes the *reason* to want it to.

use std::time::Duration;

use async_trait::async_trait;
use tv_shell_protocol::Capabilities;

use crate::transport::{NodeTransport, NodeTransportExt, Reachability, TransportError};

/// Default per-request timeout (connect + send + read the whole body).
///
/// Matches [`crate::ipc`]'s 3s so a page's budget means the same thing against
/// either transport — `pages::nav` probes on 800ms, the dashboard tiles on the
/// default, and neither should behave differently because a node happens to be
/// remote.
///
/// A route whose *protocol-level* wait can exceed this must use
/// [`NodeTransport::command_timeout`] with its own budget, exactly as
/// `capture-next` does over IPC. `POST /launch` is the live example: on Linux
/// the sidecar waits for Big Picture to come up before returning.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// How much of a failing response body to carry in
/// [`TransportError::Http`]. The body lands in a rendered banner and a log
/// line, so it is bounded here rather than trusting a remote node to be terse.
const MAX_ERROR_BODY: usize = 512;

/// The HTTP method a mapped command uses. A two-variant enum rather than
/// `reqwest::Method` so [`route_for`] stays a pure value-returning function
/// that tests can assert on without constructing a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Get,
    Post,
}

/// What a command line maps to on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    verb: Verb,
    /// Path with a leading `/`, appended to the transport's base URL.
    path: &'static str,
    /// The exact JSON text to send as a `POST` body, or `None` for a bodyless
    /// request.
    ///
    /// Held as the serialized string rather than a `serde_json::Value` for two
    /// reasons: the workspace's `reqwest` is `default-features = false` and
    /// carries no `json` feature (so the body is set with `.body()` +
    /// `Content-Type` anyway), and a test asserting on this field is then
    /// asserting on the bytes that go on the wire rather than on a value that
    /// still has a serialization step between it and the socket.
    body: Option<String>,
}

/// Map a command line onto the sidecar route that serves it.
///
/// **Pure — no client, no I/O, no `self`.** The mapping is the part of this
/// transport most likely to be wrong (a typo'd path 404s, a missing body 422s),
/// and it is the part a test can pin exhaustively without a socket.
///
/// `Err` is a [`TransportError::Command`] because that is what it is: the node
/// is fine, the line just is not something a sidecar does (or is malformed).
/// Emphatically **not** `Unreachable` — nothing was even attempted, and
/// reporting a down node because the panel asked for `cec-scan` would be a lie
/// about a machine that is up.
///
/// The usage strings mirror the daemon's `error:usage: <cmd> <args>` convention
/// (`docs/IPC_PROTOCOL.md`) so an operator reading the Tools console sees one
/// format, not two.
fn route_for(line: &str) -> Result<Route, TransportError> {
    let line = line.trim();
    let (verb, rest) = match line.split_once(' ') {
        Some((v, r)) => (v, r.trim()),
        None => (line, ""),
    };

    let bodyless = |path: &'static str, v: Verb| {
        Ok(Route {
            verb: v,
            path,
            body: None,
        })
    };

    match verb {
        "capabilities" => bodyless("/capabilities", Verb::Get),
        "library" => bodyless("/library", Verb::Get),
        "status" => bodyless("/status", Verb::Get),
        "open-bpm" => bodyless("/open-bpm", Verb::Post),
        "sleep" => bodyless("/sleep", Verb::Post),
        "launch" => appid_route("/launch", rest, "launch"),
        "quit" => appid_route("/quit", rest, "quit"),
        "" => Err(TransportError::Command("empty command".to_string())),
        other => Err(TransportError::Command(format!(
            "unknown command {other:?} — a sidecar node serves only: capabilities, \
             library, status, open-bpm, sleep, launch <appid>, quit <appid>"
        ))),
    }
}

/// The two routes taking `{"appid":<u32>}`, sharing one parse + usage message.
///
/// `u32` matches the sidecar's own `LaunchRequest.appid` type and its
/// `/art/{appid}` path type — a value that would not fit is rejected here
/// rather than sent and 422'd.
fn appid_route(path: &'static str, rest: &str, cmd: &str) -> Result<Route, TransportError> {
    let appid: u32 = rest
        .parse()
        .map_err(|_| TransportError::Command(format!("usage: {cmd} <appid>")))?;
    Ok(Route {
        verb: Verb::Post,
        path,
        body: Some(serde_json::json!({ "appid": appid }).to_string()),
    })
}

/// Trim a base URL to the form paths are appended to: no trailing `/`.
///
/// `http://host:port/` and `http://host:port` must address the same node —
/// otherwise one of them builds `http://host:port//library`, which some servers
/// route and some 404, making a config typo an intermittent mystery.
fn normalize_base(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

/// A [`NodeTransport`] speaking a `tv-shell-host` sidecar's HTTP routes.
pub struct HttpTransport {
    /// Base URL, no trailing slash (see [`normalize_base`]).
    base: String,
    /// Bearer token sent on every request. Never rendered — see
    /// [`HttpTransport::reachability`].
    token: String,
    http: reqwest::Client,
    timeout: Duration,
}

impl HttpTransport {
    /// Build a transport for the sidecar at `base` (e.g.
    /// `"http://192.0.2.10:47995"`) authenticating with `token`, using
    /// [`DEFAULT_TIMEOUT`].
    ///
    /// **The `reqwest::Client` is built with NO timeout of its own**, and that
    /// is load-bearing rather than an omission. A client-level timeout would
    /// silently cap every request at `min(caller_budget, client_default)`,
    /// which breaks [`NodeTransport::command_timeout`]'s contract in the one
    /// direction that matters: a caller whose budget must *exceed* the default
    /// would get the default instead, with no error and no log. This transport
    /// therefore owns exactly one bound — the `tokio::time::timeout` in
    /// [`Self::send`] — so the number a caller passes is the number that
    /// applies.
    #[allow(dead_code)] // see the module-level note in `crate::config` on [[nodes]]
    pub fn new(base: &str, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder().build().unwrap_or_else(|e| {
            // Mirrors `BridgeClient::new`: a builder failure (bad system CA
            // store, OOM) is rare but real, and must not brick the panel.
            tracing::warn!(
                "panel: reqwest client builder failed ({e}) — falling back to the \
                 default client for the sidecar transport"
            );
            reqwest::Client::new()
        });
        Self {
            base: normalize_base(base),
            token: token.into(),
            http,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Build a transport for a resolved `[[nodes]]` entry.
    ///
    /// The seam between the config half of this change and the transport half:
    /// [`RemoteNode`] has already had its token read under the panel's own
    /// hygiene rules (config-dir-confined, 0600, non-empty) and its `base_url`
    /// validated, so this cannot be handed a credential from an arbitrary path.
    ///
    /// Its caller is the node switcher (`docs/MULTI_NODE_PANEL.md` §4,
    /// sequencing step 6) — see [`crate::config::AppConfig::remote_nodes`] for
    /// why the config lands before the thing that serves it.
    #[allow(dead_code)]
    pub fn for_node(node: &crate::config::RemoteNode) -> Self {
        Self::new(&node.base_url, node.token.clone())
    }

    /// Issue `route` under `timeout` and return the node's reply body.
    ///
    /// The `tokio::time::timeout` wraps **send AND body read**, not just the
    /// send: a peer that answers headers promptly and then dribbles (or never
    /// finishes) the body is exactly the wedged-peer case the bound exists for,
    /// and bounding only the send would leave it unbounded.
    async fn send(&self, route: &Route, timeout: Duration) -> Result<String, TransportError> {
        tokio::time::timeout(timeout, self.send_inner(route))
            .await
            .unwrap_or(Err(TransportError::Timeout))
    }

    async fn send_inner(&self, route: &Route) -> Result<String, TransportError> {
        let url = format!("{}{}", self.base, route.path);
        let req = match route.verb {
            Verb::Get => self.http.get(&url),
            Verb::Post => self.http.post(&url),
        }
        .bearer_auth(&self.token);
        let req = match &route.body {
            Some(body) => req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone()),
            None => req,
        };

        let resp = req.send().await.map_err(|e: reqwest::Error| {
            // `is_timeout()` cannot fire while the client carries no timeout of
            // its own (see `new`), but map it honestly rather than folding a
            // future config change into "unreachable".
            if e.is_timeout() {
                TransportError::Timeout
            } else {
                TransportError::Unreachable
            }
        })?;

        let status = resp.status();
        // Read the body before branching: a failing status' body is the node's
        // own explanation and is the most useful thing to show an operator.
        let body = resp.text().await.map_err(|_| TransportError::Unreachable)?;

        if !status.is_success() {
            return Err(TransportError::Http {
                status: status.as_u16(),
                body: truncate(body.trim(), MAX_ERROR_BODY),
            });
        }
        Ok(body.trim().to_string())
    }
}

/// Truncate to at most `max` **bytes**, never splitting a UTF-8 character, and
/// mark that it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[async_trait]
impl NodeTransport for HttpTransport {
    /// `GET /capabilities` — the sidecar's handshake, which is just another
    /// JSON reply on the same command path as everything else, exactly as it is
    /// for [`crate::ipc::IpcTransport`].
    ///
    /// Note what this means for a bad token: the handshake gets
    /// [`TransportError::Http`], which is **not** `is_unreachable()`, so
    /// `capabilities::handshake` classifies it as
    /// [`Handshake::Refused`](crate::capabilities::Handshake::Refused) and
    /// stops after ONE attempt with the node's own status in the banner —
    /// rather than retrying a wrong password four times over ~9s and then
    /// reporting a machine that is up as unreachable.
    async fn capabilities(&self) -> Result<Capabilities, TransportError> {
        self.command_json::<Capabilities>("capabilities").await
    }

    async fn command(&self, line: &str) -> Result<String, TransportError> {
        let route = route_for(line)?;
        self.send(&route, self.timeout).await
    }

    async fn command_timeout(
        &self,
        line: &str,
        timeout: Duration,
    ) -> Result<String, TransportError> {
        let route = route_for(line)?;
        self.send(&route, timeout).await
    }

    /// The configured base URL. Static; does no I/O, and **never includes the
    /// token** — this value is a display/diagnostic descriptor that ends up in
    /// logs and (later) the node switcher's UI.
    fn reachability(&self) -> Reachability {
        Reachability::Remote(self.base.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// desktop-2's REAL `GET /capabilities` payload, captured from the live
    /// sidecar (`host-v0.7.0` at 192.168.8.153:47995) on 2026-08-07:
    ///
    /// ```text
    /// {"node_id":"desktop","kind":"sidecar","agent_version":"0.7.0",
    ///  "platform":"windows","features":["steam_library","game_launch","sleep"]}
    /// ```
    ///
    /// Verbatim rather than reconstructed: a payload built from
    /// `Capabilities { .. }` in the test would prove the panel can parse what
    /// the panel serialized, which is not the question.
    const DESKTOP_2_CAPABILITIES: &str = r#"{"node_id":"desktop","kind":"sidecar","agent_version":"0.7.0","platform":"windows","features":["steam_library","game_launch","sleep"]}"#;

    const TOKEN: &str = "s3kret-sidecar-token";

    /// What the fake sidecar recorded about the last request it served.
    #[derive(Default)]
    struct Seen {
        /// Every `<METHOD> <path>` it was asked for, in order.
        requests: std::sync::Mutex<Vec<String>>,
        /// The last request body (empty for a bodyless request).
        last_body: std::sync::Mutex<String>,
        /// How many requests reached it at all — the witness for "this line
        /// never went on the wire".
        count: AtomicUsize,
    }

    /// A fake `tv-shell-host`: the same bearer check and the same route set as
    /// `host/src/main.rs`, served on an ephemeral loopback port.
    async fn spawn_sidecar() -> (String, Arc<Seen>) {
        let seen = Arc::new(Seen::default());

        async fn guard(
            seen: &Seen,
            headers: &HeaderMap,
            what: &str,
            body: &str,
        ) -> Option<StatusCode> {
            seen.count.fetch_add(1, Ordering::SeqCst);
            seen.requests.lock().unwrap().push(what.to_string());
            *seen.last_body.lock().unwrap() = body.to_string();
            let ok = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .is_some_and(|t| t == TOKEN);
            (!ok).then_some(StatusCode::UNAUTHORIZED)
        }

        let app = Router::new()
            .route(
                "/capabilities",
                get(|State(s): State<Arc<Seen>>, h: HeaderMap| async move {
                    match guard(&s, &h, "GET /capabilities", "").await {
                        Some(code) => (code, "unauthorized".to_string()),
                        None => (StatusCode::OK, DESKTOP_2_CAPABILITIES.to_string()),
                    }
                }),
            )
            .route(
                "/library",
                get(|State(s): State<Arc<Seen>>, h: HeaderMap| async move {
                    match guard(&s, &h, "GET /library", "").await {
                        Some(code) => (code, "unauthorized".to_string()),
                        None => (
                            StatusCode::OK,
                            r#"{"games":[{"appid":220,"name":"Half-Life 2","last_played":1754500000,"size_on_disk":6300000000,"installed":true}]}"#.to_string(),
                        ),
                    }
                }),
            )
            .route(
                "/launch",
                post(
                    |State(s): State<Arc<Seen>>, h: HeaderMap, body: String| async move {
                        match guard(&s, &h, "POST /launch", &body).await {
                            Some(code) => (code, "unauthorized".to_string()),
                            None => (StatusCode::OK, r#"{"ok":true,"appid":220}"#.to_string()),
                        }
                    },
                ),
            )
            .route(
                "/sleep",
                post(
                    |State(s): State<Arc<Seen>>, h: HeaderMap, body: String| async move {
                        match guard(&s, &h, "POST /sleep", &body).await {
                            Some(code) => (code, "unauthorized".to_string()),
                            None => (
                                StatusCode::OK,
                                r#"{"ok":false,"reason":"a game is running"}"#.to_string(),
                            ),
                        }
                    },
                ),
            )
            // Deliberately NO /status route: an older sidecar missing a route
            // the panel knows is exactly the 404 case under test.
            .with_state(Arc::clone(&seen));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    /// A listener that accepts connections and then never answers — the wedged
    /// peer. Accepted sockets are held (not dropped) so the connection stays
    /// open rather than closing, which would surface as a clean error instead
    /// of the hang under test.
    async fn spawn_hung_peer() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                match listener.accept().await {
                    Ok((sock, _)) => held.push(sock),
                    Err(_) => return,
                }
            }
        });
        format!("http://{addr}")
    }

    /// A bound-then-closed port: nothing is listening, so a connect is refused.
    async fn dead_address() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    // ── The mapping (pure, no I/O) ────────────────────────────────────────

    #[test]
    fn every_sidecar_route_has_a_command_line() {
        // The body column is the literal wire text, not a `Value` — see
        // `Route::body`.
        let cases: [(&str, Verb, &str, Option<&str>); 7] = [
            ("capabilities", Verb::Get, "/capabilities", None),
            ("library", Verb::Get, "/library", None),
            ("status", Verb::Get, "/status", None),
            ("open-bpm", Verb::Post, "/open-bpm", None),
            ("sleep", Verb::Post, "/sleep", None),
            (
                "launch 220",
                Verb::Post,
                "/launch",
                Some(r#"{"appid":220}"#),
            ),
            ("quit 220", Verb::Post, "/quit", Some(r#"{"appid":220}"#)),
        ];
        for (line, verb, path, body) in cases {
            let route = route_for(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
            assert_eq!(route.verb, verb, "{line:?}");
            assert_eq!(route.path, path, "{line:?}");
            assert_eq!(route.body.as_deref(), body, "{line:?}");
        }
    }

    /// Every path this transport can produce is one `host/src/main.rs` really
    /// registers. A typo here is a 404 at runtime against a healthy node.
    #[test]
    fn no_mapped_path_is_invented() {
        // The sidecar's own route list, minus the unauthenticated `/art/{appid}`
        // (not a `command` — it is an image URL QML embeds directly).
        let real = [
            "/library",
            "/launch",
            "/open-bpm",
            "/quit",
            "/sleep",
            "/status",
            "/capabilities",
        ];
        for line in [
            "capabilities",
            "library",
            "status",
            "open-bpm",
            "sleep",
            "launch 1",
            "quit 1",
        ] {
            let path = route_for(line).unwrap().path;
            assert!(real.contains(&path), "{line:?} maps to unknown path {path}");
        }
    }

    #[test]
    fn an_unknown_line_is_a_command_error_naming_what_a_sidecar_serves() {
        let err = route_for("cec-scan").unwrap_err();
        assert!(
            !err.is_unreachable(),
            "a command a sidecar doesn't serve does not mean the node is down"
        );
        assert_eq!(err.http_status(), None, "nothing went on the wire");
        let msg = err.to_string();
        assert!(msg.contains("cec-scan"), "{msg}");
        assert!(msg.contains("library"), "{msg}");
    }

    #[test]
    fn a_missing_or_bad_appid_is_a_usage_error_not_a_request() {
        for line in [
            "launch",
            "launch abc",
            "quit",
            "quit -1",
            "launch 4294967296",
        ] {
            let err = route_for(line).unwrap_err();
            let msg = err.to_string();
            assert!(msg.starts_with("usage: "), "{line:?} gave {msg:?}");
            assert!(!err.is_unreachable(), "{line:?}");
        }
    }

    #[test]
    fn base_url_normalization_collapses_a_trailing_slash() {
        assert_eq!(normalize_base("http://h:1/"), "http://h:1");
        assert_eq!(normalize_base("  http://h:1  "), "http://h:1");
        assert_eq!(normalize_base("http://h:1"), "http://h:1");
    }

    #[test]
    fn truncate_never_splits_a_utf8_character() {
        let s = "é".repeat(400); // 800 bytes
        let cut = truncate(&s, MAX_ERROR_BODY);
        assert!(cut.len() <= MAX_ERROR_BODY + "…".len());
        assert!(cut.ends_with('…'));
        // The real proof: it is still valid UTF-8 and re-parses as chars.
        assert!(cut.chars().all(|c| c == 'é' || c == '…'));
        assert_eq!(truncate("short", MAX_ERROR_BODY), "short");
    }

    // ── Against a live fake sidecar ───────────────────────────────────────

    /// The handshake against desktop-2's real payload.
    #[tokio::test]
    async fn capabilities_parses_the_live_desktop_2_payload() {
        let (base, _seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, TOKEN);
        let caps = t.capabilities().await.expect("handshake");

        assert_eq!(caps.node_id, "desktop");
        assert_eq!(caps.kind, tv_shell_protocol::NodeKind::Sidecar);
        assert_eq!(caps.agent_version, "0.7.0");
        assert_eq!(caps.platform, tv_shell_protocol::Platform::Windows);
        assert_eq!(
            caps.features,
            [
                tv_shell_protocol::Feature::SteamLibrary,
                tv_shell_protocol::Feature::GameLaunch,
                tv_shell_protocol::Feature::Sleep,
            ]
            .into_iter()
            .collect()
        );
    }

    /// **The variant's reason to exist.** A wrong/missing bearer must be
    /// neither `Command` (the node would render as healthy and the page would
    /// show `unwrap_or_default()` emptiness as fact) nor `Unreachable` (an auth
    /// misconfig would be indistinguishable from a powered-off box).
    #[tokio::test]
    async fn a_rejected_credential_is_http_401_and_neither_command_nor_unreachable() {
        let (base, _seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, "wrong-token");

        let err = t.command("library").await.unwrap_err();

        assert_eq!(err.http_status(), Some(401), "got {err:?}");
        assert!(err.is_auth_failure());
        assert!(
            !err.is_unreachable(),
            "a node that answered 401 is UP — reporting it unreachable sends the \
             operator to the wrong machine"
        );
        assert!(
            !matches!(err, TransportError::Command(_)),
            "Command would make the dashboard claim the node is healthy: {err:?}"
        );
        assert!(!matches!(err, TransportError::Parse(_)), "{err:?}");
        // The message has to name the fix, not just the number.
        let msg = err.to_string();
        assert!(msg.contains("401"), "{msg}");
        assert!(msg.contains("token"), "{msg}");
    }

    /// The same must hold for the handshake path specifically, because that one
    /// decides the entire registered route set. A 401 must land as `Refused`
    /// (one attempt, reason shown) and never as `Unreachable` (four attempts,
    /// ~9s, wrong advice).
    #[tokio::test]
    async fn a_401_handshake_is_refused_not_unreachable() {
        let (base, seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, "wrong-token");

        let snap = crate::capabilities::handshake(&t).await;

        match &snap.handshake {
            crate::capabilities::Handshake::Refused(why) => {
                assert!(why.contains("401"), "{why}");
            }
            other => panic!("a 401 must be Refused, got {other:?}"),
        }
        assert_eq!(
            seen.count.load(Ordering::SeqCst),
            1,
            "a rejected credential is an ANSWER — retrying it 4x buys ~9s of \
             startup and cannot change the outcome"
        );
        assert!(snap.features.is_empty(), "must fail closed");
    }

    /// A route the node does not have (an older agent) is a 404 — again
    /// neither `Unreachable` nor `Command`, and the message says which way to
    /// redeploy.
    #[tokio::test]
    async fn a_missing_route_is_http_404_not_unreachable() {
        let (base, _seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, TOKEN);

        let err = t.command("status").await.unwrap_err();

        assert_eq!(err.http_status(), Some(404), "got {err:?}");
        assert!(!err.is_unreachable(), "{err:?}");
        assert!(!err.is_auth_failure());
        assert!(err.to_string().contains("older than this panel"), "{err}");
    }

    /// A `POST` route sends the appid as a JSON body the sidecar's
    /// `Json<LaunchRequest>` extractor accepts — traced to the wire, not
    /// asserted on the `Route` value alone.
    #[tokio::test]
    async fn launch_puts_the_appid_on_the_wire_as_json() {
        let (base, seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, TOKEN);

        let reply = t.command("launch 220").await.unwrap();
        assert_eq!(reply, r#"{"ok":true,"appid":220}"#);

        assert_eq!(seen.requests.lock().unwrap().as_slice(), ["POST /launch"]);
        let body: serde_json::Value =
            serde_json::from_str(&seen.last_body.lock().unwrap()).expect("a JSON body");
        assert_eq!(body["appid"], 220);
        // It must deserialize into the sidecar's OWN request type, not merely
        // look right.
        let req: tv_shell_protocol::LaunchRequest =
            serde_json::from_value(body).expect("the sidecar's LaunchRequest");
        assert_eq!(req.appid, 220);
    }

    /// A bodyless POST really is bodyless — `/sleep` and `/open-bpm` take no
    /// body, and sending `null` would be a 422 from the extractor.
    #[tokio::test]
    async fn a_bodyless_post_sends_no_body() {
        let (base, seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, TOKEN);

        let reply = t.command("sleep").await.unwrap();
        assert_eq!(reply, r#"{"ok":false,"reason":"a game is running"}"#);
        assert_eq!(*seen.last_body.lock().unwrap(), "");
    }

    /// An unmapped line must not reach the network at all. Without the request
    /// counter this test would pass on a transport that sent the line and got a
    /// 404 — a different (and wrong) answer that reads the same at the caller.
    #[tokio::test]
    async fn an_unmapped_line_never_goes_on_the_wire() {
        let (base, seen) = spawn_sidecar().await;
        let t = HttpTransport::new(&base, TOKEN);

        let err = t.command("get-config").await.unwrap_err();

        assert!(matches!(err, TransportError::Command(_)), "{err:?}");
        assert_eq!(
            seen.count.load(Ordering::SeqCst),
            0,
            "the transport contacted the node for a command it cannot serve"
        );
    }

    /// Nothing listening ⇒ `Unreachable`, the one case that really is one.
    #[tokio::test]
    async fn a_refused_connection_is_unreachable() {
        let base = dead_address().await;
        let t = HttpTransport::new(&base, TOKEN);
        let err = t.command("library").await.unwrap_err();
        assert!(err.is_unreachable(), "got {err:?}");
        assert_eq!(err.http_status(), None);
    }

    /// The [`NodeTransport::command_timeout`] contract: an implementation MUST
    /// return within roughly the caller's timeout, whatever its own default is.
    /// `pages::nav` probes on an 800ms budget from a ~10s htmx poll precisely so
    /// a hung node cannot pile those polls up, and this transport reaching a
    /// node across the LAN is if anything MORE likely to hang than the local
    /// socket the contract was written for.
    ///
    /// Two-sided on purpose, exactly like `IpcTransport`'s
    /// `command_timeout_bounds_a_hung_peer`: asserting `Timeout` alone would
    /// also pass if the argument were ignored and [`DEFAULT_TIMEOUT`] (3s) used,
    /// so the elapsed bound is what actually discriminates. It is also why the
    /// `reqwest::Client` carries no timeout of its own — a client default would
    /// impose `min(caller, default)` and this test would not notice.
    #[tokio::test]
    async fn command_timeout_bounds_a_hung_peer() {
        let base = spawn_hung_peer().await;
        let t = HttpTransport::new(&base, TOKEN);

        let started = std::time::Instant::now();
        let err = t
            .command_timeout("library", Duration::from_millis(200))
            .await
            .expect_err("a peer that never replies must not resolve");
        let elapsed = started.elapsed();

        assert!(matches!(err, TransportError::Timeout), "got {err:?}");
        assert!(
            elapsed < Duration::from_millis(1500),
            "took {elapsed:?} — the caller's 200ms budget was not honored"
        );
    }

    /// The other half of the contract: a budget LONGER than the default must
    /// also be honored. This is the direction a generic
    /// `timeout(default, command())` wrapper would silently break, and the
    /// reason the bound is each implementation's obligation rather than the
    /// trait's. It is also the direction a `reqwest::Client`-level timeout
    /// breaks, which is why [`HttpTransport::new`] sets none.
    ///
    /// **The margin is load-bearing, not padding.** A bare
    /// `elapsed > DEFAULT_TIMEOUT` was tried and is a gate that cannot fail:
    /// under a client-level 3s timeout the request returns at ~3.002s, which
    /// satisfies `> 3s` by two milliseconds, and the mutation passed. The
    /// assertion has to sit between the capped time (`DEFAULT_TIMEOUT`) and
    /// the honored time (`DEFAULT_TIMEOUT + OVERRUN`), so it is checked
    /// against the midpoint.
    #[tokio::test]
    async fn a_budget_longer_than_the_default_is_not_capped_to_it() {
        /// How far past the default the caller asks for.
        const OVERRUN: Duration = Duration::from_millis(1200);
        /// Midway between "capped at the default" and "honored in full".
        const FLOOR: Duration = Duration::from_millis(DEFAULT_TIMEOUT.as_millis() as u64 + 600);

        let base = spawn_hung_peer().await;
        let t = HttpTransport::new(&base, TOKEN);

        let started = std::time::Instant::now();
        let err = t
            .command_timeout("library", DEFAULT_TIMEOUT + OVERRUN)
            .await
            .expect_err("the peer never replies");
        let elapsed = started.elapsed();

        assert!(matches!(err, TransportError::Timeout), "got {err:?}");
        assert!(
            elapsed > FLOOR,
            "returned after {elapsed:?}, short of {FLOOR:?} — the caller asked for \
             {:?} and was capped near the transport's own {DEFAULT_TIMEOUT:?} \
             default. A caller that needs longer (POST /launch waits for Big \
             Picture to come up) would be cut off with no error and no log.",
            DEFAULT_TIMEOUT + OVERRUN
        );
    }

    /// `command()` (no argument) uses the transport's own default, so a hung
    /// node cannot hang a page indefinitely either.
    #[tokio::test]
    async fn plain_command_is_bounded_by_the_transport_default() {
        let base = spawn_hung_peer().await;
        let t = HttpTransport::new(&base, TOKEN);

        let started = std::time::Instant::now();
        let err = t.command("library").await.expect_err("never replies");
        let elapsed = started.elapsed();

        assert!(matches!(err, TransportError::Timeout), "got {err:?}");
        assert!(
            elapsed < DEFAULT_TIMEOUT + Duration::from_millis(1500),
            "took {elapsed:?}, past the {DEFAULT_TIMEOUT:?} default"
        );
    }

    /// `reachability()` is a static descriptor — no I/O — and must not leak the
    /// bearer token into a value that ends up in logs and the node switcher.
    #[tokio::test]
    async fn reachability_reports_the_base_url_and_never_the_token() {
        let t = HttpTransport::new("http://192.168.8.153:47995/", TOKEN);
        let before = t.reachability();
        assert_eq!(
            before,
            Reachability::Remote("http://192.168.8.153:47995".to_string())
        );
        assert!(!format!("{before:?}").contains(TOKEN));

        // Unchanged by a call that fails — it describes configuration, not
        // liveness.
        assert!(t.command("library").await.is_err());
        assert_eq!(t.reachability(), before);
    }

    /// The extension helpers ride `command`, so they work here for free — the
    /// property `NodeTransportExt` is written to guarantee, checked against a
    /// SECOND implementation rather than assumed from the first.
    #[tokio::test]
    async fn ext_helpers_work_through_dyn_dispatch_on_this_transport() {
        let (base, _seen) = spawn_sidecar().await;
        let node: Arc<dyn NodeTransport> = Arc::new(HttpTransport::new(&base, TOKEN));

        let lib: tv_shell_protocol::LibraryResponse = node.command_json("library").await.unwrap();
        assert_eq!(lib.games.len(), 1);
        assert_eq!(lib.games[0].appid, 220);
        assert!(lib.games[0].installed);
    }
}

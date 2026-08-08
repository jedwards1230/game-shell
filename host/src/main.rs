//! tv-shell-host — a thin, cross-platform sidecar that answers two questions
//! for the tv-shell TV client: "what Steam games are installed?" and "launch
//! this one." Moonlight remains the stream engine; this service never touches
//! Sunshine config, so other Moonlight clients are unaffected.
//!
//! Endpoints (`/library`, `/launch`, `/open-bpm`, `/quit`, `/sleep`, `/status`
//! require `Authorization: Bearer <token>`; `/art/{appid}` is intentionally
//! PUBLIC — see below):
//!   GET  /library      → { games: [LibraryEntry, ...] }   (VDF/ACF enumeration)
//!   POST /launch       { appid }  → { ok: true }  (navigates Big Picture to the
//!                                                  game's page; user presses Play)
//!   POST /open-bpm     (no body)  → { ok: true }  (opens Big Picture's HOME
//!                                                  screen — no game selected)
//!   POST /quit         { appid }  → { ok, appid, reason }  (gracefully terminates
//!                                                  the running game — SIGTERM to
//!                                                  its process group, like Steam's
//!                                                  Stop; { ok: false, reason:
//!                                                  "not running" } — still HTTP
//!                                                  200 — when nothing matched)
//!   POST /sleep        (no body)  → { ok, reason }  (suspends the host to RAM;
//!                                                    REFUSED with { ok: false,
//!                                                    reason } — still HTTP 200 —
//!                                                    while a game is running or
//!                                                    a stream is live)
//!   GET  /status       → { version, running_appid, streaming }
//!   GET  /art/{appid}  → image/jpeg of the local Steam library art for `appid`,
//!                        or 404. PUBLIC (no bearer): cover art isn't sensitive
//!                        and QML's `Image.source` can't send an Authorization
//!                        header. `appid` is parsed as `u32` (non-numeric ⇒ 404),
//!                        so no raw string ever reaches a filesystem path.
//!
//! Config (env; legacy `GAME_SHELL_*` names honored as a fallback):
//!   TV_SHELL_HOST_TOKEN — bearer token. If unset AND `TV_SHELL_HOST_BIND` is
//!                         loopback-only, a CSPRNG token is generated and
//!                         logged on startup. If unset and the bind is
//!                         non-loopback, **the sidecar refuses to start** —
//!                         :47995 accepts `/launch`, `/quit`, and `/sleep`
//!                         (a machine-wide suspend), so serving that
//!                         unauthenticated on the LAN is not a safe default.
//!   TV_SHELL_HOST_PORT  — listen port (default 47995).
//!   TV_SHELL_HOST_BIND  — listen address (default 0.0.0.0 = all LAN ifaces).
//!
//! Optionally the sidecar also publishes its state to MQTT and accepts a few
//! commands there — additive, never a replacement for the HTTP routes above (the
//! QML shell's Steam widget depends on them). It is off unless
//! `TV_SHELL_MQTT_BROKER` is set; see [`mqtt`] for the full env surface.

mod launch;
mod mqtt;
mod power;
mod steam;

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::json;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tv_shell_protocol::{
    LaunchRequest, LibraryEntry, LibraryResponse, QuitResponse, SleepResponse, StatusResponse,
};

/// Default listen port. Picked outside Sunshine/Moonlight's 47984–47990 range to
/// avoid any collision with a co-hosted Sunshine.
const DEFAULT_PORT: u16 = 47995;

/// Shared service state: the bearer token (for constant-time compare).
struct AppState {
    token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let port: u16 = tv_shell_protocol::brand::env("HOST_PORT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let bind = tv_shell_protocol::brand::env("HOST_BIND").unwrap_or_else(|| "0.0.0.0".to_string());
    // Resolved (and, on refusal, aborted) BEFORE the listener binds below — see
    // `resolve_token`'s doc comment for the fail-closed rule.
    let token = resolve_token(&bind, port, tv_shell_protocol::brand::env("HOST_TOKEN"))?;

    // Resolve the MQTT configuration before serving — but NEVER let it stop the
    // HTTP listener coming up.
    //
    // MQTT is additive: the QML shell's Steam widget depends on the HTTP routes
    // below, and a typo in an MQTT env var must not break the TV's Steam row. On
    // Windows these arrive through per-user `win_environment` variables, the
    // fiddliest config channel in the design and the one most likely to carry a
    // typo — so the cost of that typo is "no MQTT device in Home Assistant", not
    // a dead sidecar with the cause in an unrelated subsystem.
    //
    // This keeps the §3 fail-closed rule intact: it constrains what we PUBLISH
    // (never invent a device identity), not whether an unrelated listener binds.
    // A missing device_id still refuses to publish; it just no longer takes HTTP
    // down with it.
    match mqtt::settings_from_env() {
        Ok(Some(settings)) => {
            // An unreadable CA degrades to the platform trust store rather than
            // disabling MQTT. That store is now the NORMAL path — the broker
            // presents a publicly-trusted certificate — so `ca_file` is only for
            // a private CA.
            let ca = match mqtt::load_ca(settings.ca_file.as_deref()).await {
                Ok(ca) => ca,
                Err(e) => {
                    tracing::warn!("MQTT: {e} — falling back to the platform trust store");
                    None
                }
            };
            tokio::spawn(mqtt::run(settings, ca));
        }
        Ok(None) => tracing::info!("MQTT disabled (TV_SHELL_MQTT_BROKER unset)"),
        Err(e) => tracing::error!(
            "MQTT DISABLED — configuration error: {e}. The sidecar is starting \
             normally without it and every HTTP route below still serves. Fix the \
             TV_SHELL_MQTT_* environment and restart to publish to Home Assistant."
        ),
    }

    let state = Arc::new(AppState { token });

    let app = Router::new()
        .route("/library", get(library))
        .route("/launch", post(launch_game))
        .route("/open-bpm", post(open_bpm))
        .route("/quit", post(quit_game))
        .route("/sleep", post(sleep))
        .route("/status", get(status))
        // PUBLIC — no bearer (cover art isn't sensitive; QML's Image.source can't
        // send an Authorization header). `appid` is typed `u32` so a non-numeric
        // path segment 404s before any filesystem access.
        .route("/art/{appid}", get(art))
        .with_state(state);

    let addr = format!("{bind}:{port}");
    tracing::info!("tv-shell-host listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Resolve the bearer token from the env, or — for a loopback-only bind —
/// generate one with a CSPRNG and log it. **Fails closed**: a non-loopback bind
/// with no explicit token refuses to start rather than mint one, because
/// `:{port}` accepts `/launch`, `/quit`, and `/sleep` (a machine-wide suspend),
/// so serving that unauthenticated on the LAN is not a safe default (S4 in
/// `docs/MULTI_NODE_PANEL.md`).
///
/// Pure aside from one `tracing::warn!` and the CSPRNG read, so this is
/// unit-tested directly against `bind`/`port`/token-option inputs rather than
/// through an integration harness that actually binds a socket.
fn resolve_token(bind: &str, port: u16, env_token: Option<String>) -> anyhow::Result<String> {
    if let Some(t) = env_token {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    if !is_loopback_bind(bind) {
        anyhow::bail!(
            "refusing to start: TV_SHELL_HOST_TOKEN is unset and TV_SHELL_HOST_BIND={bind:?} \
             (port {port}) is not loopback-only — an unauthenticated :{port} on the LAN would \
             let anyone reachable list, launch, quit, or SUSPEND this machine. Set \
             TV_SHELL_HOST_TOKEN to a stable bearer token (see docs/HOST_SETUP.md), or set \
             TV_SHELL_HOST_BIND=127.0.0.1 for a loopback-only dev run."
        );
    }
    let generated = generate_token()?;
    // Cleartext at startup is deliberate for the loopback dev path: this is the
    // only place the operator can read the token to copy it into the daemon's
    // config, and it's never written to disk. See the PR body for the full
    // reasoning.
    tracing::warn!(
        "TV_SHELL_HOST_TOKEN unset — generated a random token for this loopback-only run: \
         {generated}"
    );
    Ok(generated)
}

/// Whether `bind` is unambiguously loopback-only. Only a literal loopback IP
/// (`127.0.0.1`, `::1`, …) counts as loopback — a hostname like `localhost`
/// doesn't parse as an `IpAddr` and is treated as **not** loopback, because
/// `TcpListener::bind` resolves it independently (async DNS) and this check
/// must stay fail-closed: misclassifying an ambiguous value as loopback would
/// let the S4 refusal be silently bypassed by a hostname that later resolves
/// off-box.
fn is_loopback_bind(bind: &str) -> bool {
    bind.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Generate a 256-bit token (64 lowercase hex chars) from the OS CSPRNG via
/// `ring::rand::SystemRandom` — `ring` is already in the dependency graph
/// (pulled in by `rustls`'s `ring` feature above), so this adds no new crate.
/// Only ever called for a loopback-only bind (see [`resolve_token`]);
/// production deployments set `TV_SHELL_HOST_TOKEN` explicitly.
fn generate_token() -> anyhow::Result<String> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("failed to read from the OS CSPRNG"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time bearer check. Returns `Ok(())` when the `Authorization` header
/// is `Bearer <token>` and `<token>` matches; otherwise `Err(401)`.
fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    // ConstantTimeEq over bytes; exact match only — the stored token is trimmed
    // at resolution, so do NOT trim the presented token (a whitespace-padded copy
    // must fail). Length mismatch is handled by ct_eq returning 0.
    let ok: bool = presented.as_bytes().ct_eq(state.token.as_bytes()).into();
    if ok {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// `GET /library` — enumerate installed Steam games.
async fn library(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryResponse>, StatusCode> {
    authorize(&state, &headers)?;
    // VDF parsing touches the filesystem; run it off the async reactor.
    let games: Vec<LibraryEntry> = tokio::task::spawn_blocking(steam::enumerate)
        .await
        .unwrap_or_default();
    Ok(Json(LibraryResponse { games }))
}

/// `POST /launch` — navigate Big Picture to a Steam game's page by appid. This no
/// longer auto-starts the game (it fires `steam://nav/games/details/<appid>`); the
/// user presses Play. On Linux it waits for Big Picture to be up first.
async fn launch_game(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorize(&state, &headers)?;
    match launch::launch(req.appid) {
        Ok(()) => Ok(Json(json!({ "ok": true, "appid": req.appid }))),
        Err(e) => {
            tracing::warn!("launch {} failed: {e}", req.appid);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /open-bpm` — open Steam Big Picture's HOME screen (no game selected) by
/// firing `steam://open/bigpicture`. The companion to `/launch` (which navigates
/// BPM to a specific game's page): this just resets Steam to the Big Picture home.
/// No body. Requires the same bearer auth as `/launch`/`/library`/`/status`.
async fn open_bpm(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorize(&state, &headers)?;
    match launch::open_bigpicture() {
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        Err(e) => {
            tracing::warn!("open-bpm failed: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /quit` — gracefully terminate the running Steam game for `appid` (the
/// equivalent of Steam's Stop button). Finds the game's `reaper` launcher process
/// and sends SIGTERM to its process group so the whole game tree shuts down
/// cleanly (graceful only — never SIGKILL). Returns `{ ok: true, appid, reason:
/// null }` when a matching process was signalled, `{ ok: false, appid, reason:
/// "not running" }` when no such game is running (or the OS is unsupported). The
/// `/proc` scan + signal run off the async reactor via `spawn_blocking`, matching
/// `status()`.
///
/// **"Nothing to quit" is an HTTP 200, not an error** — exactly like `/sleep`'s
/// refusal: the body, not the status code, carries the decision. Serialized
/// through the shared [`QuitResponse`] so the daemon deserializes the same
/// contract and can surface the refusal instead of flattening it into "ok".
async fn quit_game(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<QuitResponse>, StatusCode> {
    authorize(&state, &headers)?;
    let appid = req.appid;
    let result = tokio::task::spawn_blocking(move || steam::quit(appid))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("quit task panicked: {e}")));
    match result {
        Ok(true) => Ok(Json(QuitResponse {
            ok: true,
            appid,
            reason: None,
        })),
        Ok(false) => Ok(Json(QuitResponse {
            ok: false,
            appid,
            reason: Some("not running".to_string()),
        })),
        Err(e) => {
            tracing::warn!("quit {appid} failed: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /sleep` — suspend the host machine to RAM. No body (like `/open-bpm`).
///
/// The host, not the caller, decides whether sleeping is safe: the same two
/// signals `/status` publishes — the foreground Steam appid and Sunshine's live/
/// resumable session flag — are gathered off the reactor via `spawn_blocking`
/// (matching `status()`) and fed to the pure [`power::suspend_refusal`]. A
/// refusal is an **HTTP 200 with `{ ok: false, reason }`**, not an error status:
/// "a game is running" is a normal answer the caller should show a human, not a
/// transport failure to retry. The response shape is exactly
/// `{ ok, reason }` in both branches — `reason` is JSON `null` on success, never
/// omitted — so a consumer binds one field unconditionally.
///
/// **Ordering.** `power::suspend()` returns once the OS suspend command has been
/// *spawned*, never waiting on it, so this handler completes and axum flushes the
/// JSON before the kernel freezes us (and, on Windows, without pinning a thread
/// for the entire sleep — `SetSuspendState` blocks until resume). The consequence
/// is deliberate and documented: `ok: true` means "accepted and dispatched", not
/// "the machine is now asleep". A failure to even *start* the suspend still
/// surfaces — as a 500, matching `/launch` and `/open-bpm`.
async fn sleep(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SleepResponse>, StatusCode> {
    authorize(&state, &headers)?;
    match request_sleep().await {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            tracing::warn!("sleep failed: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// The sleep decision itself, transport-independent: probe → refuse-or-dispatch.
///
/// Lifted out of [`sleep`] so the MQTT `sleep` command runs the **same** safety
/// gate. Duplicating it would let the two drift on exactly the check that stops a
/// suspend mid-game.
///
/// - `Ok(SleepResponse { ok: false, reason: Some(..) })` — refused. A refusal is a
///   normal answer, never an error: `sleep` still returns it as an HTTP 200.
/// - `Ok(SleepResponse { ok: true, reason: None })` — the suspend was *dispatched*
///   (see [`power::suspend`]); it does not mean the machine is asleep yet.
/// - `Err(..)` — the suspend could not even be started.
async fn request_sleep() -> anyhow::Result<SleepResponse> {
    // Both probes touch the OS off-band (a `/proc`/registry read and a blocking
    // loopback HTTP GET to Sunshine); keep them off the async reactor, exactly as
    // `status()` does. A panicked probe degrades to its safe value — and the safe
    // value for "is a game running?" is unknown-so-assume-idle only because the
    // streaming probe still guards the other half.
    let running = tokio::task::spawn_blocking(steam::running_appid)
        .await
        .unwrap_or(None);
    let streaming = tokio::task::spawn_blocking(steam::streaming)
        .await
        .unwrap_or(false);

    if let Some(reason) = power::suspend_refusal(running, streaming) {
        tracing::info!("sleep: refused — {reason}");
        return Ok(SleepResponse {
            ok: false,
            reason: Some(reason.to_string()),
        });
    }

    // Spawning the suspend command is a blocking syscall; keep it off the reactor.
    // It returns as soon as the child is spawned (see `power::suspend`), so this
    // await is short and the caller's response still gets flushed.
    tokio::task::spawn_blocking(power::suspend)
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("suspend task panicked: {e}")))?;
    Ok(SleepResponse {
        ok: true,
        reason: None,
    })
}

/// `GET /status` — version + the currently-running Steam appid (or null) +
/// whether a Moonlight/Sunshine stream is active. The running id is detected
/// per-OS (Linux: scanning `/proc` for Steam's `reaper` `SteamLaunch AppId=<n>`
/// launcher; Windows: `registry.vdf`'s `RunningAppID`), so it reflects the running
/// game regardless of how it was started. `running_appid: null` ⇒ nothing running
/// (or detection found no match). `streaming` is true when Sunshine reports a live
/// session (active OR resumable) via its GameStream `serverinfo` state — the same
/// signal the Moonlight client reads (false when Sunshine is idle or unreachable).
async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, StatusCode> {
    authorize(&state, &headers)?;
    // Both probes touch the OS off-band (a `/proc`/VDF read and a blocking
    // loopback HTTP GET to Sunshine); keep them off the async reactor.
    let running = tokio::task::spawn_blocking(steam::running_appid)
        .await
        .unwrap_or(None);
    let streaming = tokio::task::spawn_blocking(steam::streaming)
        .await
        .unwrap_or(false);
    // Serialized through the shared `StatusResponse` type so the daemon parses the
    // same contract — byte-identical to the previous hand-rolled
    // `json!({version, running_appid, streaming})` (same fields, same order).
    Ok(Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        running_appid: running,
        streaming,
    }))
}

/// `GET /art/{appid}` — serve the local Steam portrait library art (capsule,
/// else header) for `appid` as `image/jpeg`. PUBLIC: no `authorize()` call —
/// cover art isn't sensitive and QML's `Image.source` can't attach a bearer.
///
/// `appid` is extracted as a `u32` by axum's path deserializer, so a non-numeric
/// or out-of-range segment yields a 404 before this handler runs — a raw,
/// attacker-controlled string can never be interpolated into a filesystem path.
/// Missing art (or no Steam root) ⇒ 404. The blocking cache scan + read runs off
/// the async reactor via `spawn_blocking`, matching `status()`.
async fn art(Path(appid): Path<u32>) -> impl IntoResponse {
    let bytes = tokio::task::spawn_blocking(move || {
        steam::library_art_path(appid).and_then(|p| std::fs::read(p).ok())
    })
    .await
    .ok()
    .flatten();

    match bytes {
        Some(bytes) => ([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::AUTHORIZATION;

    fn state(tok: &str) -> AppState {
        AppState {
            token: tok.to_string(),
        }
    }

    fn bearer(tok: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, format!("Bearer {tok}").parse().unwrap());
        h
    }

    #[test]
    fn authorize_accepts_matching_token() {
        assert!(authorize(&state("sekret"), &bearer("sekret")).is_ok());
    }

    #[test]
    fn authorize_rejects_wrong_token() {
        assert_eq!(
            authorize(&state("sekret"), &bearer("nope")),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn authorize_rejects_token_with_trailing_spaces() {
        // The presented token must match exactly; a whitespace-padded copy of the
        // correct token must NOT be accepted (no trim on the presented side).
        assert_eq!(
            authorize(&state("sekret"), &bearer("sekret   ")),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn authorize_rejects_missing_header() {
        assert_eq!(
            authorize(&state("sekret"), &HeaderMap::new()),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn authorize_rejects_non_bearer() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, "Basic sekret".parse().unwrap());
        assert_eq!(
            authorize(&state("sekret"), &h),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn generated_token_is_64_hex_chars() {
        let t = generate_token().unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_token_is_not_deterministic_in_time_and_pid() {
        // The old splitmix generator was a deterministic function of
        // (SystemTime nanos, pid) — both fixed within one process, so two calls
        // in a row would previously have been closely related (a deterministic
        // function of the last). Two CSPRNG draws from the same process must
        // differ.
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_ne!(
            a, b,
            "two CSPRNG draws in the same process must not collide"
        );
    }

    // --- S4: fail-closed token/bind resolution ---------------------------

    #[test]
    fn refuses_non_loopback_bind_with_no_token() {
        let err = resolve_token("0.0.0.0", 47995, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TV_SHELL_HOST_TOKEN"), "message: {msg}");
        assert!(msg.contains("0.0.0.0"), "message: {msg}");
    }

    #[test]
    fn refuses_ipv6_wildcard_bind_with_no_token() {
        assert!(resolve_token("::", 47995, None).is_err());
    }

    #[test]
    fn refuses_unparseable_hostname_bind_with_no_token() {
        // A hostname doesn't parse as an IpAddr, so it must NOT be treated as
        // loopback — fail closed on ambiguity, not fail open.
        assert!(resolve_token("localhost", 47995, None).is_err());
    }

    #[test]
    fn loopback_bind_with_no_token_starts_and_generates_one() {
        let t = resolve_token("127.0.0.1", 47995, None).unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ipv6_loopback_bind_with_no_token_starts() {
        assert!(resolve_token("::1", 47995, None).is_ok());
    }

    #[test]
    fn explicit_token_wins_regardless_of_bind() {
        assert_eq!(
            resolve_token("0.0.0.0", 47995, Some("sekret".to_string())).unwrap(),
            "sekret"
        );
        assert_eq!(
            resolve_token("127.0.0.1", 47995, Some("sekret".to_string())).unwrap(),
            "sekret"
        );
    }

    #[test]
    fn empty_or_whitespace_token_on_non_loopback_still_refuses() {
        // An empty/blank env var is treated as unset (matches the trim-then-check
        // logic below), so it must not bypass the refusal on a LAN bind.
        assert!(resolve_token("0.0.0.0", 47995, Some(String::new())).is_err());
        assert!(resolve_token("0.0.0.0", 47995, Some("   ".to_string())).is_err());
    }

    #[test]
    fn is_loopback_bind_classifies_correctly() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("127.5.5.5"));
        assert!(is_loopback_bind("::1"));
        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("::"));
        assert!(!is_loopback_bind("192.168.1.10"));
        assert!(!is_loopback_bind("localhost"));
        assert!(!is_loopback_bind(""));
    }
}

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
//!   POST /quit         { appid }  → { ok: true }  (gracefully terminates the
//!                                                  running game — SIGTERM to its
//!                                                  process group, like Steam's
//!                                                  Stop; { ok: false } if not
//!                                                  running)
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
//!   TV_SHELL_HOST_TOKEN — bearer token. If unset, a random one is generated
//!                         and logged on startup.
//!   TV_SHELL_HOST_PORT  — listen port (default 47995).
//!   TV_SHELL_HOST_BIND  — listen address (default 0.0.0.0 = all LAN ifaces).

mod launch;
mod power;
mod steam;

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tv_shell_protocol::{
    LaunchRequest, LibraryEntry, LibraryResponse, SleepResponse, StatusResponse,
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

    let token = resolve_token();
    let port: u16 = tv_shell_protocol::brand::env("HOST_PORT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let bind = tv_shell_protocol::brand::env("HOST_BIND").unwrap_or_else(|| "0.0.0.0".to_string());

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

/// Resolve the bearer token from the env, or generate + log a fresh one. The
/// generated token is logged once at startup so an operator can copy it into the
/// daemon's config; it's never written to disk.
fn resolve_token() -> String {
    if let Some(t) = tv_shell_protocol::brand::env("HOST_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let generated = generate_token();
    tracing::warn!(
        "TV_SHELL_HOST_TOKEN unset — generated a random token for this run: {generated}"
    );
    generated
}

/// Generate a 256-bit hex token from the OS time + process id, hashed. Good
/// enough for a per-run dev token; production deployments set the env var. We
/// avoid pulling in a RNG crate to keep the dependency graph minimal.
fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    // Mix two 128-bit values into a 64-hex-char string via a simple splitmix-ish
    // scramble. Not cryptographically strong, but unpredictable enough for a
    // throwaway token; operators are warned to set the env var.
    let mut out = String::with_capacity(64);
    let mut x = nanos ^ (pid.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for _ in 0..4 {
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        out.push_str(&format!("{:016x}", (x as u64)));
    }
    out
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
/// cleanly (graceful only — never SIGKILL). Returns `{ ok: true, appid }` when a
/// matching process was signalled, `{ ok: false, appid, reason: "not running" }`
/// when no such game is running (or the OS is unsupported). The `/proc` scan +
/// signal run off the async reactor via `spawn_blocking`, matching `status()`.
async fn quit_game(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorize(&state, &headers)?;
    let appid = req.appid;
    let result = tokio::task::spawn_blocking(move || steam::quit(appid))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("quit task panicked: {e}")));
    match result {
        Ok(true) => Ok(Json(json!({ "ok": true, "appid": appid }))),
        Ok(false) => Ok(Json(
            json!({ "ok": false, "appid": appid, "reason": "not running" }),
        )),
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
        return Ok(Json(SleepResponse {
            ok: false,
            reason: Some(reason.to_string()),
        }));
    }

    // Spawning the suspend command is a blocking syscall; keep it off the reactor.
    // It returns as soon as the child is spawned (see `power::suspend`), so this
    // await is short and the response below still gets flushed.
    let result = tokio::task::spawn_blocking(power::suspend)
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("suspend task panicked: {e}")));
    match result {
        Ok(()) => Ok(Json(SleepResponse {
            ok: true,
            reason: None,
        })),
        Err(e) => {
            tracing::warn!("sleep failed: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
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
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

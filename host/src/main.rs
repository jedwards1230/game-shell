//! tv-shell-host — a thin, cross-platform sidecar that answers two questions
//! for the tv-shell TV client: "what Steam games are installed?" and "launch
//! this one." Moonlight remains the stream engine; this service never touches
//! Sunshine config, so other Moonlight clients are unaffected.
//!
//! Endpoints (`/library`, `/launch`, `/open-bpm`, `/quit`, `/sleep`, `/status`,
//! `/capabilities` require `Authorization: Bearer <token>`; `/art/{appid}` is
//! intentionally PUBLIC — see below):
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
//!   GET  /capabilities → { node_id, kind, agent_version, platform, features }
//!                        (what this node declares it can do — see `capabilities`)
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
    Capabilities, Feature, LaunchRequest, LibraryEntry, LibraryResponse, NodeKind, Platform,
    QuitResponse, SleepResponse, StatusResponse,
};

/// Default listen port. Picked outside Sunshine/Moonlight's 47984–47990 range to
/// avoid any collision with a co-hosted Sunshine.
const DEFAULT_PORT: u16 = 47995;

/// Fallback node identity when nothing else resolves. See [`resolve_node_id`].
const DEFAULT_NODE_ID: &str = "tv-shell-host";

/// Shared service state: the bearer token (for constant-time compare) and this
/// node's identity (resolved once at startup — it can't change while we run, and
/// resolving it per request would put a filesystem read on a hot path).
struct AppState {
    token: String,
    node_id: String,
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

    let state = Arc::new(AppState {
        token,
        node_id: resolve_node_id(),
    });
    tracing::info!("node_id: {}", state.node_id);

    let app = Router::new()
        .route("/library", get(library))
        .route("/launch", post(launch_game))
        .route("/open-bpm", post(open_bpm))
        .route("/quit", post(quit_game))
        .route("/sleep", post(sleep))
        .route("/status", get(status))
        .route("/capabilities", get(capabilities))
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

/// Read every node-identity candidate from the live environment and hand them to
/// [`pick_node_id`], which owns the ordering.
///
/// This half is impure by necessity (env vars + a procfs read) and holds no
/// policy; the precedence lives in the pure core so it is testable on every
/// platform. Resolved once at startup, not per request.
fn resolve_node_id() -> String {
    let mqtt_device_id = tv_shell_protocol::brand::env("MQTT_DEVICE_ID");
    let computername = std::env::var("COMPUTERNAME").ok();
    let procfs_hostname = linux_hostname();
    let env_hostname = std::env::var("HOSTNAME").ok();
    pick_node_id(
        mqtt_device_id.as_deref(),
        computername.as_deref(),
        procfs_hostname.as_deref(),
        env_hostname.as_deref(),
    )
}

/// Pick this node's stable identity from its candidate sources, in order:
///
/// 1. **`mqtt_device_id`** (`TV_SHELL_MQTT_DEVICE_ID`) — the sidecar's existing
///    *explicit, never-derived* identity (see [`mqtt`]). Reusing it means the
///    node that appears in Home Assistant and the node that answers
///    `/capabilities` cannot end up with two different names for one machine.
///    Read through `brand::env`, so the legacy `GAME_SHELL_*` spelling works.
/// 2. **The machine hostname**, best-effort and dependency-free:
///    `computername` (`COMPUTERNAME`, always set on Windows), then
///    `procfs_hostname` (`/proc/sys/kernel/hostname` on Linux), and only then
///    `env_hostname` (`HOSTNAME`). The procfs read outranks `HOSTNAME`
///    deliberately: it is the kernel's answer, whereas `HOSTNAME` is a bash-only
///    variable that a systemd service normally doesn't even see — and when it
///    *is* exported (a container, `docker exec`, an inherited shell) it can be
///    stale, which would have the sidecar answer to a name the machine no longer
///    has. `HOSTNAME` survives as a last resort for non-Linux unix hosts. There
///    is no dependency-free hostname source on macOS, and adding a crate for a
///    CI-only target is not worth it.
/// 3. **[`DEFAULT_NODE_ID`]** — a constant, never a generated/random value: an
///    identity that changes per run is worse than a duplicated one.
///
/// Blank and whitespace-only candidates are skipped, not accepted — a
/// `TV_SHELL_MQTT_DEVICE_ID=""` must fall through rather than name the node `""`.
///
/// Pure: it takes its inputs instead of reading them, the same shape the daemon
/// uses for `ipc::resolve_node_id(configured, hostname)` and
/// `daemon_config::resolve_osd_name`, so the precedence is pinned by tests on
/// every platform rather than varying with the machine running them.
fn pick_node_id(
    mqtt_device_id: Option<&str>,
    computername: Option<&str>,
    procfs_hostname: Option<&str>,
    env_hostname: Option<&str>,
) -> String {
    first_valid_id(&[mqtt_device_id, computername, procfs_hostname, env_hostname])
        .unwrap_or_else(|| DEFAULT_NODE_ID.to_string())
}

/// First candidate that trims to a valid [`tv_shell_protocol::mqtt::DeviceId`],
/// if any. Pure, so the precedence is testable on every platform.
///
/// Validation — not merely a non-blank check — is what makes claim 1 above true.
/// `mqtt.rs` runs `TV_SHELL_MQTT_DEVICE_ID` through `DeviceId::new` before
/// publishing, so accepting an id here that MQTT would reject is exactly the
/// "two different names for one machine" split the precedence exists to prevent.
/// It also caps the field's length and alphabet, keeping `/capabilities` bounded.
fn first_valid_id(candidates: &[Option<&str>]) -> Option<String> {
    candidates.iter().flatten().find_map(|s| {
        tv_shell_protocol::mqtt::DeviceId::new(s.trim())
            .ok()
            .map(|d| d.as_str().to_string())
    })
}

/// `/proc/sys/kernel/hostname`, or `None` off Linux / on any read error.
fn linux_hostname() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname").ok()
}

/// Everything this sidecar declares it can do.
///
/// **Declared, never inferred, and never health-derived** — each entry answers
/// "is this wired on this build target?", not "would it succeed right now". A
/// Steam that happens to be closed does not remove `game_launch`.
///
/// | Feature | Gate |
/// |---|---|
/// | `steam_library` | unconditional — [`steam::enumerate`] has install roots for all three targets |
/// | `game_launch` | `linux`/`windows` — [`steam::quit`] and [`steam::running_appid`] are only wired there; macOS can open the launch URL but can never stop or even see a running game, so claiming it would make `/quit` a silent no-op the panel presents as a working button |
/// | `sleep` | `linux`/`windows` — exactly [`power::suspend`]'s own `cfg` |
///
/// Everything else in [`Feature`] is a shell-node or panel-tier concern this
/// binary has no route for, so it is never claimed.
fn capability_features() -> std::collections::BTreeSet<Feature> {
    let mut f = std::collections::BTreeSet::new();
    f.insert(Feature::SteamLibrary);
    if cfg!(any(target_os = "linux", target_os = "windows")) {
        f.insert(Feature::GameLaunch);
        f.insert(Feature::Sleep);
    }
    f
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

/// `GET /capabilities` — what this node declares it can do, as the shared
/// [`Capabilities`] wire type. The panel builds its nav **and registers its
/// routes** from this set; it never probes or guesses.
///
/// Bearer-authenticated like every route but `/art/{appid}`: the feature set is
/// an inventory of what is reachable on this machine, which is exactly the map
/// an attacker would want, and the daemon that consumes it already holds a token.
///
/// Pure compile-time + startup-resolved data (no probes, no I/O), so it never
/// touches the blocking pool — unlike [`status`], and deliberately: a capability
/// answer must stay available while a probe would hang.
async fn capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Capabilities>, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(Capabilities {
        node_id: state.node_id.clone(),
        kind: NodeKind::Sidecar,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: Platform::current(),
        features: capability_features(),
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
            node_id: "test-node".to_string(),
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

    // --- /capabilities -----------------------------------------------------

    #[tokio::test]
    async fn capabilities_requires_a_bearer() {
        // Same posture as every route but /art: the feature set is an inventory
        // of what's reachable here and must not be readable unauthenticated.
        let st = Arc::new(state("sekret"));
        assert!(matches!(
            capabilities(State(st.clone()), HeaderMap::new()).await,
            Err(StatusCode::UNAUTHORIZED)
        ));
        assert!(capabilities(State(st), bearer("sekret")).await.is_ok());
    }

    #[tokio::test]
    async fn capabilities_declares_a_sidecar_with_the_real_platform() {
        let st = Arc::new(state("sekret"));
        let Json(caps) = capabilities(State(st), bearer("sekret")).await.unwrap();
        assert_eq!(caps.kind, NodeKind::Sidecar);
        assert_eq!(caps.platform, Platform::current());
        assert_eq!(caps.agent_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(caps.node_id, "test-node");
        // Library enumeration is wired on every target we build for.
        assert!(caps.features.contains(&Feature::SteamLibrary));
        // Launch/quit and suspend follow the same cfg their implementations do.
        let os_wired = cfg!(any(target_os = "linux", target_os = "windows"));
        assert_eq!(caps.features.contains(&Feature::GameLaunch), os_wired);
        assert_eq!(caps.features.contains(&Feature::Sleep), os_wired);
        // Shell-node and panel-tier features are never claimed by the sidecar.
        for never in [
            Feature::Cec,
            Feature::Controllers,
            Feature::Widgets,
            Feature::SettingsStore,
            Feature::ShellLifecycle,
            Feature::Screenshot,
            Feature::DevDeploy,
        ] {
            assert!(!caps.features.contains(&never), "must not claim {never:?}");
        }
    }

    #[test]
    fn node_id_precedence_skips_blank_candidates() {
        assert_eq!(
            first_valid_id(&[None, Some("  "), Some(" pc-1 ")]),
            Some("pc-1".to_string())
        );
        assert_eq!(first_valid_id(&[]), None);
        assert_eq!(first_valid_id(&[None, Some("")]), None);
    }

    #[test]
    fn node_id_skips_candidates_mqtt_would_reject() {
        // An id MQTT refuses must not become the node's name here either —
        // otherwise HA and /capabilities disagree about one machine. `a/b`
        // contains a topic separator; the long one blows DeviceId's length cap.
        assert_eq!(
            first_valid_id(&[Some("a/b"), Some("good-1")]),
            Some("good-1".to_string())
        );
        assert_eq!(first_valid_id(&[Some("has space"), None]), None);
        assert_eq!(first_valid_id(&[Some(&"x".repeat(200))]), None);
        // A dotted hostname is not a valid DeviceId, so it falls through rather
        // than being reported — DEFAULT_NODE_ID is the documented degradation.
        assert_eq!(
            pick_node_id(None, None, Some("box.local"), None),
            DEFAULT_NODE_ID
        );
    }

    #[test]
    fn pick_node_id_follows_the_documented_precedence() {
        // Every candidate present: the explicit mqtt device_id wins outright.
        assert_eq!(
            pick_node_id(Some("htpc-1"), Some("WINBOX"), Some("procfs"), Some("envh")),
            "htpc-1"
        );
        // Then COMPUTERNAME (always set on Windows)...
        assert_eq!(
            pick_node_id(None, Some("WINBOX"), Some("procfs"), Some("envh")),
            "WINBOX"
        );
        // ...then the kernel's own answer from procfs...
        assert_eq!(
            pick_node_id(None, None, Some("procfs"), Some("envh")),
            "procfs"
        );
        // ...and only then the (possibly stale, bash-only) HOSTNAME.
        assert_eq!(pick_node_id(None, None, None, Some("envh")), "envh");
        // Nothing resolvable → the constant, never a generated value.
        assert_eq!(pick_node_id(None, None, None, None), DEFAULT_NODE_ID);

        // A BLANK higher-precedence candidate falls through rather than naming
        // the node "" — `TV_SHELL_MQTT_DEVICE_ID=""` must not win.
        assert_eq!(
            pick_node_id(Some(""), Some("  "), Some(" pc-1 "), Some("envh")),
            "pc-1"
        );
        // Blank all the way down is the same as absent.
        assert_eq!(
            pick_node_id(Some(" "), Some(""), Some("\n"), Some("\t")),
            DEFAULT_NODE_ID
        );
        // Procfs reads carry the kernel's trailing newline — it must be trimmed,
        // not treated as content (`linux_hostname` returns the raw file).
        assert_eq!(pick_node_id(None, None, Some("box\n"), None), "box");
    }
}

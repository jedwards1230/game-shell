//! LAN HTTP control bridge (#151): a small, opt-in HTTP/1.1 listener that maps
//! `POST /intent/<target>`, `POST /key/<name>`, and `GET /screenshot` onto the
//! daemon's existing intent/key broadcast paths and the `grim` screenshotter.
//!
//! **Opt-in**: the bridge only starts when the `TV_SHELL_HTTP_BIND`
//! environment variable is set to a `host:port` address. When it is unset, no
//! socket is opened and no control surface is exposed.
//!
//! **Auth**: controlled by `TV_SHELL_HTTP_AUTH_ENABLED` (default: enabled).
//! Set to `0` or `false` to skip auth entirely (local-only dev). When enabled
//! (the default), `TV_SHELL_HTTP_TOKEN` must be set; every request must carry
//! `Authorization: Bearer <token>` (constant-time comparison, #151). If auth is
//! enabled but no token is configured, all requests are rejected with 401 and a
//! loud warning is logged. When the token variable is unset AND auth is disabled,
//! any request from the bound interface is accepted — the operator is responsible
//! for binding to a trusted interface. A `tracing::warn!` is emitted when
//! binding to an unspecified address without a token.
//!
//! **Screenshot**: `GET /screenshot` (also `GET /screenshot.png`) shells out to
//! `grim -` which writes a PNG to stdout. The PNG bytes are returned with
//! `Content-Type: image/png`. Auth applies to this route (it exposes screen
//! content). On grim failure a 500 text response is returned.
//!
//! **DoS hardening (#151)**:
//! - Each accepted connection is wrapped in a 5-second `tokio::time::timeout`
//!   so a slow sender that never sends the blank-line HTTP terminator cannot
//!   hold a task slot indefinitely.
//! - An atomic `ACTIVE_CONNS` counter caps concurrent connections at
//!   `MAX_ACTIVE_CONNS` (128). Connections beyond the cap receive an immediate
//!   503 and are dropped, preventing fd / task-pool exhaustion.
//!
//! **No new heavy dependencies**: this module uses only `tokio::net::TcpListener`,
//! `tokio::time`, `tokio::process`, and the `subtle` crate (already a transitive
//! dep, now direct). `reqwest` is client-only (`health.rs`) and `hyper` is only
//! a transitive dep; neither is used here.
//!
//! **Cross-platform**: this module has no Linux-only imports. The `serve`
//! function can be called on any platform. The binary (`main.rs`) is
//! Linux-gated, so in practice the listener only runs on Linux — but the pure
//! parser and unit tests compile and run on macOS / CI.

use crate::bridge_core;
use crate::display_owner::SharedDisplayOwner;
use crate::ipc::DbusSenders;
use crate::protocol::{self, Command, Event};
use crate::shell_state::{self, SharedShellState};
use crate::state::Control;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

// ─── Error types ────────────────────────────────────────────────────────────

/// Errors the HTTP request parser can produce, mapped to HTTP status codes.
#[derive(Debug, PartialEq, Eq)]
pub enum HttpError {
    /// The path did not match any known route → 404.
    NotFound,
    /// The method was not POST (or GET for screenshot routes) → 405.
    MethodNotAllowed,
}

// ─── Parsed action ──────────────────────────────────────────────────────────

/// The action decoded from a successful request.
#[derive(Debug, PartialEq, Eq)]
pub enum HttpAction {
    /// Forward to `Control::Intent { name }`.
    /// The `name` is the full remainder after `/intent/`, percent-decoded.
    Intent(String),
    /// Forward to `Control::Key { name }`.
    /// The `name` is the single token after `/key/`, percent-decoded.
    Key(String),
    /// Capture the current screen via `grim -` and return the PNG.
    /// `flash` is `true` when the caller passed `?flash=1`; the handler
    /// broadcasts `Event::ScreenshotFlash` after a successful capture so the
    /// QML shell can paint a brief white vignette as feedback.
    Screenshot { flash: bool },
    // ── Dev-control routes (#167) ────────────────────────────────────────────
    /// `POST /dev/deploy[?ref=<ref>]` — git fetch + checkout + reset.
    DevDeploy { git_ref: Option<String> },
    /// `POST /dev/build` — build via scripts/build-daemon.sh + install binary.
    DevBuild,
    /// `POST /dev/restart-shell` — pkill quickshell, relaunch detached.
    DevRestartShell,
    /// `POST /dev/restart-daemon` — write ok response then re-exec daemon.
    DevRestartDaemon,
    /// `GET /dev/logs[?lines=N&filter=<str>]` — tail /tmp/qs-log.txt.
    DevLogs {
        lines: usize,
        filter: Option<String>,
    },
    /// `GET /dev/status` — JSON status blob.
    DevStatus,
    /// `GET /metrics` — Prometheus/OpenMetrics exposition text. BYPASSES bearer
    /// auth (scrapers don't send tokens) and is always available when the bridge
    /// is bound.
    Metrics,
    // ── Power-control routes ─────────────────────────────────────────────────
    /// `GET /status` — the shell's reported UI state + its freshness, as JSON.
    ///
    /// Named `ShellStatus`, not `Status`, to stay unambiguous next to the
    /// pre-existing [`HttpAction::DevStatus`] (`GET /dev/status`, a build/PID
    /// blob). Authenticated like every route except `/metrics`.
    ShellStatus,
    /// `POST /suspend` — suspend this machine via logind, honouring the
    /// `power-can-suspend` refusal.
    Suspend,
}

// ─── Pure parser (unit-testable on any host) ────────────────────────────────

/// Decode a single `%XX` escape at position `i` in the byte slice.
/// Returns `(decoded_byte, consumed_len)` or `None` on malformed input.
fn decode_pct(bytes: &[u8], i: usize) -> Option<(u8, usize)> {
    if i + 2 >= bytes.len() {
        return None;
    }
    let hi = (bytes[i + 1] as char).to_digit(16)?;
    let lo = (bytes[i + 2] as char).to_digit(16)?;
    Some(((hi as u8) << 4 | lo as u8, 3))
}

/// Decode a percent-encoded URL path component (e.g. `settings%3Abluetooth`
/// → `settings:bluetooth`). Only the path segment (no query string) is
/// expected.
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            // `+` as space is query-string only; in a path it is literal.
            out.push(b'+');
            i += 1;
        } else if bytes[i] == b'%' {
            if let Some((byte, len)) = decode_pct(bytes, i) {
                out.push(byte);
                i += len;
            } else {
                // Malformed escape — pass through literally.
                out.push(bytes[i]);
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // Path components are UTF-8; lossy-convert to avoid panicking on
    // garbage input.
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a URL query string (`key=value&key2=value2`) into a list of pairs.
/// Percent-encoding on values is NOT decoded here (the dev routes use simple
/// ASCII values); keys are compared verbatim.
pub fn parse_query(qs: &str) -> Vec<(String, String)> {
    if qs.is_empty() {
        return Vec::new();
    }
    qs.split('&')
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = it.next()?;
            let v = it.next().unwrap_or("");
            if k.is_empty() {
                return None;
            }
            Some((url_decode(k), url_decode(v)))
        })
        .collect()
}

/// Parse an HTTP request line (`METHOD SP PATH SP HTTP/...`) and map it to an
/// [`HttpAction`].
///
/// - `GET /screenshot`          → [`HttpAction::Screenshot`]
/// - `GET /screenshot.png`      → [`HttpAction::Screenshot`]
/// - Non-GET for `/screenshot[.png]` → [`HttpError::MethodNotAllowed`]
/// - `POST /intent/<target>`    → [`HttpAction::Intent`] (`<target>` percent-decoded;
///   empty target → [`HttpError::NotFound`]).
/// - `POST /key/<name>`         → [`HttpAction::Key`] (percent-decoded; empty →
///   [`HttpError::NotFound`]).
/// - `GET /status`              → [`HttpAction::ShellStatus`] (distinct from `/dev/status`)
/// - `POST /suspend`            → [`HttpAction::Suspend`]
/// - `GET /dev/status`          → [`HttpAction::DevStatus`]
/// - `GET /dev/logs`            → [`HttpAction::DevLogs`] (`?lines=N&filter=F`)
/// - `POST /dev/deploy`         → [`HttpAction::DevDeploy`] (`?ref=<git-ref>`)
/// - `POST /dev/build`          → [`HttpAction::DevBuild`]
/// - `POST /dev/restart-shell`  → [`HttpAction::DevRestartShell`]
/// - `POST /dev/restart-daemon` → [`HttpAction::DevRestartDaemon`]
/// - Non-POST for any other path → [`HttpError::MethodNotAllowed`].
/// - Unknown POST path           → [`HttpError::NotFound`].
///
/// Query strings are stripped for non-dev routes; dev routes preserve them for
/// parameter extraction.
pub fn parse_request_line(method: &str, path: &str) -> Result<HttpAction, HttpError> {
    // Split path and query string — dev routes use query params.
    let (bare_path, qs) = match path.find('?') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, ""),
    };

    // Metrics route: GET only. Prometheus/OpenMetrics exposition text. Auth is
    // bypassed for this route in handle_connection (scrapers don't send tokens).
    if bare_path == "/metrics" {
        if method == "GET" {
            return Ok(HttpAction::Metrics);
        } else {
            return Err(HttpError::MethodNotAllowed);
        }
    }

    // Screenshot routes: GET only.
    // Optional ?flash=1 causes a post-capture flash event on the IPC event bus.
    if bare_path == "/screenshot" || bare_path == "/screenshot.png" {
        if method == "GET" {
            let flash = parse_query(qs)
                .iter()
                .any(|(k, v)| k == "flash" && v == "1");
            return Ok(HttpAction::Screenshot { flash });
        } else {
            return Err(HttpError::MethodNotAllowed);
        }
    }

    // Shell-status route: GET only. MUST be matched here, above the
    // POST-or-405 gate below — otherwise a `GET /status` is rejected 405 before
    // it can ever reach a prefix match. Exact match, so there is no collision
    // with `/dev/status` (which the `/dev/` branch below owns anyway).
    if bare_path == "/status" {
        if method == "GET" {
            return Ok(HttpAction::ShellStatus);
        } else {
            return Err(HttpError::MethodNotAllowed);
        }
    }

    // Dev routes (#167).
    if bare_path.starts_with("/dev/") {
        let params = parse_query(qs);
        return parse_dev_route(method, bare_path, &params);
    }

    // All remaining routes require POST.
    if method != "POST" {
        return Err(HttpError::MethodNotAllowed);
    }

    // Suspend this machine. POST-only, so it sits below the gate above (a
    // `GET /suspend` correctly yields 405, not an accidental power-off).
    if bare_path == "/suspend" {
        return Ok(HttpAction::Suspend);
    }

    if let Some(rest) = bare_path.strip_prefix("/intent/") {
        let name = url_decode(rest);
        if name.is_empty() {
            return Err(HttpError::NotFound);
        }
        return Ok(HttpAction::Intent(name));
    }

    if let Some(rest) = bare_path.strip_prefix("/key/") {
        let name = url_decode(rest);
        if name.is_empty() {
            return Err(HttpError::NotFound);
        }
        return Ok(HttpAction::Key(name));
    }

    Err(HttpError::NotFound)
}

/// Parse a `/dev/*` route.  Called only when `bare_path` starts with `/dev/`.
fn parse_dev_route(
    method: &str,
    bare_path: &str,
    params: &[(String, String)],
) -> Result<HttpAction, HttpError> {
    let get = |k: &str| -> Option<&str> {
        params
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };

    match (method, bare_path) {
        ("GET", "/dev/status") => Ok(HttpAction::DevStatus),
        ("GET", "/dev/logs") => {
            let lines = get("lines")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(100)
                .min(1000);
            let filter = get("filter").map(str::to_owned);
            Ok(HttpAction::DevLogs { lines, filter })
        }
        ("POST", "/dev/deploy") => {
            let git_ref = get("ref").map(str::to_owned);
            Ok(HttpAction::DevDeploy { git_ref })
        }
        ("POST", "/dev/build") => Ok(HttpAction::DevBuild),
        ("POST", "/dev/restart-shell") => Ok(HttpAction::DevRestartShell),
        ("POST", "/dev/restart-daemon") => Ok(HttpAction::DevRestartDaemon),
        // Known dev paths with wrong method.
        (_, "/dev/status") | (_, "/dev/logs") => Err(HttpError::MethodNotAllowed),
        (_, "/dev/deploy")
        | (_, "/dev/build")
        | (_, "/dev/restart-shell")
        | (_, "/dev/restart-daemon") => Err(HttpError::MethodNotAllowed),
        // Unknown /dev/* path.
        _ => Err(HttpError::NotFound),
    }
}

// ─── HTTP response helpers ───────────────────────────────────────────────────

/// Translate a `dispatch_intent`/`dispatch_key` reply into an HTTP response:
/// `None` → 503 "daemon unavailable"; `error:<body>` → 400 with the body
/// (prefix stripped, whitespace preserved as before); otherwise → 200 "ok".
fn dispatch_http_response(reply: Option<String>) -> String {
    match bridge_core::interpret_reply(reply) {
        bridge_core::DispatchOutcome::Unavailable => http_response(503, "daemon unavailable"),
        bridge_core::DispatchOutcome::Err(body) => http_response(400, &body),
        bridge_core::DispatchOutcome::Ok => http_response(200, "ok"),
    }
}

/// Render a minimal HTTP/1.1 response with the given status code and text body.
/// The connection is always closed after the response.
pub fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Render a 200 response with a JSON body.
pub fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Build the header block for a binary PNG response.
/// Returns the raw header bytes (including the final CRLFCRLF). The caller must
/// write these bytes then immediately write the raw PNG bytes — do NOT use
/// `http_response` for binary bodies since it assumes a UTF-8 `&str` body.
///
/// Capture provenance rides in `X-TvShell-*` headers (not the body), so the
/// PNG stays byte-identical to a metadata-unaware client (`curl -o shot.png`).
/// All four values are header-safe: git SHAs/branch names and the RFC3339
/// timestamp contain no CR/LF.
fn png_response_header(png_len: usize, meta: &bridge_core::CaptureMeta) -> Vec<u8> {
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: image/png\r\n\
         Content-Length: {png_len}\r\n\
         X-TvShell-Sha: {sha}\r\n\
         X-TvShell-Branch: {branch}\r\n\
         X-TvShell-Version: {version}\r\n\
         X-TvShell-Captured-At: {captured_at}\r\n\
         Connection: close\r\n\r\n",
        sha = meta.sha,
        branch = meta.branch,
        version = meta.version,
        captured_at = meta.captured_at,
    );
    header.into_bytes()
}

// ─── Per-connection handler ──────────────────────────────────────────────────

/// Read up to `MAX_REQUEST_BYTES` from the TCP stream and return the raw
/// header block (up to and including the blank line). Returns `None` on EOF
/// or if no header terminator is found within the limit.
const MAX_REQUEST_BYTES: usize = 4096;

async fn read_headers<S>(stream: &mut S) -> Option<Vec<u8>>
where
    S: AsyncReadExt + Unpin,
{
    let mut buf = vec![0u8; MAX_REQUEST_BYTES];
    let mut filled = 0;

    loop {
        if filled == buf.len() {
            // Request too large — return what we have to let the parser
            // attempt to reject it gracefully.
            break;
        }
        match stream.read(&mut buf[filled..]).await {
            Ok(0) => return None, // EOF
            Ok(n) => {
                filled += n;
                // Look for the blank line that terminates HTTP headers.
                if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    Some(buf[..filled].to_vec())
}

/// Extract the `Authorization` header value from raw header bytes.
fn extract_authorization(raw: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(raw).ok()?;
    for line in text.lines() {
        // Case-insensitive match on the header name.
        let Some(colon_pos) = line.find(':') else {
            continue;
        };
        let header_name = line[..colon_pos].trim();
        if header_name.eq_ignore_ascii_case("authorization") {
            return Some(line[colon_pos + 1..].trim().to_owned());
        }
    }
    None
}

/// Constant-time string comparison — delegates to the shared helper in
/// `bridge_core` so both the HTTP bridge and the MCP auth middleware use
/// identical comparison logic.
fn ct_eq_str(a: &str, b: &str) -> bool {
    bridge_core::ct_eq_str(a, b)
}

/// Handle a single TCP connection: parse the HTTP/1.1 request, check auth,
/// dispatch to the control channel or screenshotter, and write a response.
#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    token: Option<&str>,
    auth_enabled: bool,
    control_tx: &mpsc::Sender<Control>,
    events_tx: &broadcast::Sender<Event>,
    shutdown: &tokio_util::sync::CancellationToken,
    reexec_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    metrics: &std::sync::Arc<crate::metrics::Metrics>,
    // Shell-reported UI state cache, written by the IPC `shell-state` push and
    // read by `GET /status`.
    ui_state: &SharedShellState,
    // Passive CEC display-ownership cache, written by the CEC actor and read by
    // the same `GET /status`. Always present — in a build without
    // `--features cec` nothing writes it and it reports "unknown", which is the
    // honest answer.
    display_owner: &SharedDisplayOwner,
    // D-Bus actor handles, for `POST /suspend`. Empty on non-Linux (see
    // `ipc::DbusSenders`), which is exactly what keeps this route portable.
    dbus: &DbusSenders,
) {
    // Bound the header read with a short timeout (anti-slowloris); the longer
    // outer DEV_TIMEOUT_SECS budget covers dev-op subprocess execution only.
    let raw = match tokio::time::timeout(
        std::time::Duration::from_secs(READ_TIMEOUT_SECS),
        read_headers(&mut stream),
    )
    .await
    {
        Ok(Some(b)) => b,
        Ok(None) => {
            tracing::debug!("http: client disconnected before headers");
            return;
        }
        Err(_) => {
            tracing::debug!("http: header read timed out");
            return;
        }
    };

    // Parse the request line (first line of the header block).
    let text = match std::str::from_utf8(&raw) {
        Ok(t) => t,
        Err(_) => {
            let resp = http_response(400, "invalid UTF-8 in request");
            let _ = stream.write_all(resp.as_bytes()).await;
            return;
        }
    };

    let first_line = text.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        let resp = http_response(400, "malformed request line");
        let _ = stream.write_all(resp.as_bytes()).await;
        return;
    }
    let method = parts[0];
    let path = parts[1];

    // Route first so we can exempt the unauthenticated `/metrics` scrape from
    // the bearer-auth check below. Routing is a pure string parse with no side
    // effects, so doing it before auth leaks nothing.
    let action = match parse_request_line(method, path) {
        Ok(a) => a,
        Err(HttpError::NotFound) => {
            let resp = http_response(404, "not found");
            let _ = stream.write_all(resp.as_bytes()).await;
            return;
        }
        Err(HttpError::MethodNotAllowed) => {
            let resp = http_response(405, "method not allowed");
            let _ = stream.write_all(resp.as_bytes()).await;
            return;
        }
    };

    // Auth check — constant-time comparison to prevent timing oracles (#151).
    // When auth is enabled but no token is configured, reject all requests
    // (secure by default — you cannot authenticate without a token).
    //
    // `/metrics` is EXEMPT: Prometheus/node_exporter scrapers don't send a
    // bearer token, and the endpoint exposes only aggregate counters + resource
    // gauges (no screen content, no control). It is always available.
    let auth_exempt = matches!(action, HttpAction::Metrics);
    if auth_enabled && !auth_exempt {
        match token {
            None => {
                // Auth enabled but no token set — reject all requests.
                let resp = http_response(401, "unauthorized");
                let _ = stream.write_all(resp.as_bytes()).await;
                return;
            }
            Some(expected) => {
                let bearer = format!("Bearer {expected}");
                let auth = extract_authorization(&raw);
                let provided = auth.as_deref().unwrap_or("");
                if !ct_eq_str(provided, &bearer) {
                    let resp = http_response(401, "unauthorized");
                    let _ = stream.write_all(resp.as_bytes()).await;
                    return;
                }
            }
        }
    }

    // Dispatch to the daemon control channel or screenshotter.
    match action {
        HttpAction::Intent(name) => {
            let reply = bridge_core::dispatch_intent(control_tx, name).await;
            let resp = dispatch_http_response(reply);
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::Key(name) => {
            let reply = bridge_core::dispatch_key(control_tx, name).await;
            let resp = dispatch_http_response(reply);
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::Screenshot { flash } => {
            // Delegate capture to bridge_core so the grim logic is shared with MCP.
            match bridge_core::capture_screenshot(events_tx, flash).await {
                Ok(png) => {
                    // Binary-safe response: write the header then the raw PNG bytes.
                    // We do NOT use http_response() here — that function takes a &str
                    // body and would corrupt arbitrary binary data.
                    let meta = bridge_core::capture_meta().await;
                    let header = png_response_header(png.len(), &meta);
                    let _ = stream.write_all(&header).await;
                    let _ = stream.write_all(&png).await;
                }
                Err(msg) => {
                    let resp = http_response(500, &msg);
                    let _ = stream.write_all(resp.as_bytes()).await;
                }
            }
        }

        // ── Dev-control routes (#167) ────────────────────────────────────────
        HttpAction::DevDeploy { git_ref } => {
            // Observability: count attempts by outcome (ok|error).
            let resp = handle_dev_deploy(git_ref.as_deref(), metrics).await;
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::DevBuild => {
            metrics.inc_build();
            let resp = handle_dev_build().await;
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::DevRestartShell => {
            metrics.inc_restart_shell();
            let resp = handle_dev_restart_shell(metrics).await;
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::DevRestartDaemon => {
            // Count the re-exec request before replacing the process image (the
            // new process starts its own counters at zero).
            metrics.inc_restart_daemon();
            // Write the response FIRST, then signal main to re-exec.
            let resp = http_response(200, "ok, re-execing\n");
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.flush().await;
            bridge_core::request_reexec(reexec_flag, shutdown);
        }
        HttpAction::DevLogs { lines, filter } => {
            let resp = handle_dev_logs(lines, filter.as_deref());
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::DevStatus => {
            let resp = handle_dev_status().await;
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::Metrics => {
            let resp = handle_metrics(metrics).await;
            let _ = stream.write_all(resp.as_bytes()).await;
        }

        // ── Power-control routes ─────────────────────────────────────────────
        HttpAction::ShellStatus => {
            let resp = handle_shell_status(ui_state, display_owner).await;
            let _ = stream.write_all(resp.as_bytes()).await;
        }
        HttpAction::Suspend => {
            // Write + flush the response BEFORE the kernel can freeze us — a
            // successful logind Suspend can return and then stop the world
            // mid-write, which would leave the caller hanging.
            let resp = handle_suspend(dbus).await;
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.flush().await;
        }
    }
}

/// `GET /metrics` — render the Prometheus/OpenMetrics exposition text.
///
/// Build identity is resolved live (async git via `resolve_build_info`) so a
/// `/dev/deploy` HEAD swap is reflected on the next scrape; `sys_metrics()`
/// samples CPU over a ~200ms window, so the render itself runs on the blocking
/// pool. The same renderer feeds the textfile writer, so the two never drift.
/// Content-Type is the Prometheus text exposition media type.
async fn handle_metrics(metrics: &std::sync::Arc<crate::metrics::Metrics>) -> String {
    let counters = std::sync::Arc::clone(metrics);
    let build = crate::metrics::resolve_build_info().await;
    let body = tokio::task::spawn_blocking(move || {
        crate::metrics::render_blocking(&counters, Some(build))
    })
    .await
    .unwrap_or_else(|e| format!("# render task failed: {e}\n"));
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

// ─── Dev-control handlers (#167) ────────────────────────────────────────────
// These thin wrappers translate bridge_core Results into HTTP response strings.

/// `POST /dev/deploy[?ref=<ref>]` — git fetch + checkout + reset to remote.
/// Records the outcome on `tv_shell_deploy_total{outcome="ok|error"}`.
async fn handle_dev_deploy(
    git_ref: Option<&str>,
    metrics: &std::sync::Arc<crate::metrics::Metrics>,
) -> String {
    match bridge_core::dev_deploy(git_ref).await {
        Ok(body) => {
            metrics.inc_deploy(true);
            http_response(200, &body)
        }
        Err(msg) => {
            metrics.inc_deploy(false);
            http_response(500, &msg)
        }
    }
}

/// `POST /dev/build` — build via scripts/build-daemon.sh + install the binary.
async fn handle_dev_build() -> String {
    match bridge_core::dev_build().await {
        Ok(body) => http_response(200, &body),
        Err(msg) => http_response(500, &msg),
    }
}

/// `POST /dev/restart-shell` — restart quickshell (single-instance). The whole
/// sequence is serialized process-wide (a concurrent call is rejected with
/// "restart already in progress"), and it prefers `systemctl --user restart` of
/// the Quickshell unit, falling back to `pkill` + detached spawn only when the
/// unit isn't active. See `bridge_core::dev_restart_shell`.
async fn handle_dev_restart_shell(metrics: &std::sync::Arc<crate::metrics::Metrics>) -> String {
    match bridge_core::dev_restart_shell(metrics).await {
        Ok(body) => http_response(200, &body),
        Err(msg) => http_response(500, &msg),
    }
}

/// `GET /dev/logs[?lines=N&filter=F]` — tail /tmp/qs-log.txt.
fn handle_dev_logs(lines: usize, filter: Option<&str>) -> String {
    match bridge_core::get_logs(lines, filter) {
        Ok(body) => http_response(200, &body),
        Err(msg) => http_response(500, &msg),
    }
}

/// `GET /dev/status` — JSON status blob.
async fn handle_dev_status() -> String {
    let info = bridge_core::get_status().await;
    let json = serde_json::to_string(&info).unwrap_or_else(|e| format!(r#"{{"error":"{e}"}}"#));
    json_response(&json)
}

// ─── Power-control handlers ──────────────────────────────────────────────────

/// The `GET /status` body: the shell-state fields and the CEC display-ownership
/// fields, flattened into ONE flat JSON object.
///
/// Two independent signals, one document: the shell knows what it is rendering,
/// CEC knows which input the display is showing. They answer different halves of
/// "is anyone looking at this box?" — an app can be running (`shell_state:
/// "appRunning"`) while the TV has been switched to another input — and a
/// consumer generally needs both. Kept flat (`#[serde(flatten)]`) so existing
/// readers of the shell fields are untouched.
#[derive(Debug, serde::Serialize)]
struct StatusBody {
    #[serde(flatten)]
    shell: shell_state::ShellStatus,
    #[serde(flatten)]
    display: crate::display_owner::DisplayOwnerStatus,
}

/// `GET /status` — publish the shell's last-reported state plus its freshness,
/// and who currently owns the display over CEC.
///
/// **Reports; does not decide.** There is deliberately no `busy` field: the
/// consumer (Home Assistant) owns the suspend policy so the rule can change
/// without a daemon release. `shell_state` is the LAST-KNOWN value — a consumer
/// must gate on `stale` before trusting it (a stale `"idle"` means the shell
/// stopped reporting, not that the box is idle).
///
/// The CEC fields carry the OPPOSITE fail-safe from the shell fields, and it
/// matters: only `cec_display_ownership: "owned_by_other"` is positive evidence
/// that someone switched the display to another input. `"unknown"` means we have
/// no evidence at all and must never be read as "nobody is watching" — see
/// [`crate::display_owner`].
async fn handle_shell_status(
    ui_state: &SharedShellState,
    display_owner: &SharedDisplayOwner,
) -> String {
    // Sample the (subprocess) liveness probe BEFORE taking the read lock, so no
    // lock is ever held across an `.await`.
    let shell_running = bridge_core::quickshell_running().await;
    let now = shell_state::now_unix();
    let snapshot = ui_state.read().await.clone();
    // Lock-free atomics — no await, nothing held.
    let owner_snapshot = display_owner.snapshot();
    let body = StatusBody {
        shell: shell_state::status(&snapshot, now, shell_running),
        display: crate::display_owner::status(&owner_snapshot, now),
    };
    let json = serde_json::to_string(&body).unwrap_or_else(|e| format!(r#"{{"error":"{e}"}}"#));
    json_response(&json)
}

/// The outcome of a `POST /suspend`, derived from the daemon's power replies.
///
/// `pub(crate)` (a visibility-only change, no behaviour change) so the MQTT
/// `suspend` command reuses the exact same two-step gate instead of duplicating
/// it — the truth-table test below stays the single source of truth for both.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SuspendOutcome {
    /// logind accepted the suspend request. (Accepted, not completed — the
    /// process may be frozen before anything further could be observed.)
    Accepted,
    /// This machine reports it cannot suspend. A refusal, not a failure: the
    /// `power-can-suspend` reply deliberately degrades a bus error to `no`, and
    /// a non-Linux build answers `error:unsupported on this platform` — both
    /// land here rather than looking like a broken daemon.
    Refused,
    /// The suspend call itself failed; carries the daemon's reason with the
    /// `error:` prefix stripped.
    Failed(String),
}

/// Pure decision: map the raw `power-can-suspend` / `power-suspend` IPC replies
/// onto a [`SuspendOutcome`].
///
/// Reply vocabulary (`daemon/src/power.rs`): `power-can-suspend` answers
/// `yes`/`no`; `power-suspend` answers `ok`, `error:logind unavailable`, or
/// `error:suspend failed: <e>`. Either can also be
/// `error:unsupported on this platform` when the actor isn't wired.
///
/// `suspend` is `None` when the gate said no and we therefore never asked. That
/// combination can't occur by construction, but the function stays total: a
/// `yes` with no attempt is reported as a failure rather than silently "ok".
pub(crate) fn interpret_suspend(can_suspend: &str, suspend: Option<&str>) -> SuspendOutcome {
    // Anything other than a plain `yes` is a refusal — including the degraded
    // `no` a bus error produces and the non-Linux unsupported line.
    if can_suspend.trim() != "yes" {
        return SuspendOutcome::Refused;
    }
    match suspend.map(str::trim) {
        Some("ok") => SuspendOutcome::Accepted,
        Some(reply) => SuspendOutcome::Failed(
            reply
                .strip_prefix("error:")
                .unwrap_or(reply)
                .trim()
                .to_string(),
        ),
        None => SuspendOutcome::Failed("suspend was not attempted".to_string()),
    }
}

/// `POST /suspend` — suspend this machine via the existing `power-suspend` IPC
/// command (logind `Suspend(false)`); `daemon/src/power.rs` is untouched.
///
/// Cross-platform by construction: both commands go through
/// `ipc::dispatch_dbus`, whose non-Linux stub replies
/// `error:unsupported on this platform`, so on macOS this route compiles and
/// answers `409 suspend refused` instead of being `#[cfg]`-ed away.
async fn handle_suspend(dbus: &DbusSenders) -> String {
    let can = crate::ipc::dispatch_dbus(dbus, &Command::PowerCanSuspend)
        .await
        .unwrap_or_else(protocol::resp_unsupported);

    // Honour the refusal: don't even attempt the suspend when the gate says no.
    let suspend = if can.trim() == "yes" {
        Some(
            crate::ipc::dispatch_dbus(dbus, &Command::PowerSuspend)
                .await
                .unwrap_or_else(protocol::resp_unsupported),
        )
    } else {
        None
    };

    match interpret_suspend(&can, suspend.as_deref()) {
        SuspendOutcome::Accepted => http_response(200, "ok"),
        SuspendOutcome::Refused => {
            tracing::info!("http: /suspend refused (power-can-suspend replied {can:?})");
            http_response(
                409,
                "suspend refused: this system reports it cannot suspend",
            )
        }
        SuspendOutcome::Failed(reason) => {
            tracing::warn!("http: /suspend failed: {reason}");
            http_response(500, &format!("suspend failed: {reason}"))
        }
    }
}

// ─── Public serve entry point ────────────────────────────────────────────────

/// Maximum number of concurrently active HTTP connections (#151 DoS hardening).
const MAX_ACTIVE_CONNS: usize = 128;

/// Read timeout for standard connections in seconds (#151 DoS hardening).
const READ_TIMEOUT_SECS: u64 = 5;

/// Extended timeout for /dev/* connections: cargo build (~15 s typical) and
/// git fetch can easily exceed the 5-second limit.  180 s covers a cold cargo
/// build with dependency downloads on a slow LAN connection.  Auth is checked
/// before routing, so the longer window is never exposed to unauthenticated
/// senders.
const DEV_TIMEOUT_SECS: u64 = 180;

/// Bind a TCP listener to `addr` and serve the LAN HTTP control bridge until
/// the process exits.
///
/// Each accepted connection is handled in its own spawned task, wrapped in a
/// `READ_TIMEOUT_SECS`-second timeout so a slow sender cannot hold a task slot
/// indefinitely. Concurrent connections are capped at `MAX_ACTIVE_CONNS`;
/// connections beyond the cap receive an immediate 503 and are dropped.
///
/// The `token` parameter is the optional bearer token from
/// `TV_SHELL_HTTP_TOKEN`. The `auth_enabled` parameter is read from
/// `TV_SHELL_HTTP_AUTH_ENABLED` (default `true`). When auth is enabled but no
/// token is configured, all requests are rejected with 401 and a loud warning is
/// logged. When auth is disabled, a warning is logged and auth is skipped
/// entirely (for local-only dev). A warning is also emitted when binding to an
/// unspecified address (0.0.0.0 or ::) without a token.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    addr: std::net::SocketAddr,
    token: Option<String>,
    auth_enabled: bool,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
    shutdown: tokio_util::sync::CancellationToken,
    reexec_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
    ui_state: SharedShellState,
    display_owner: SharedDisplayOwner,
    dbus: DbusSenders,
) {
    // Treat an empty token as no token at all, so TV_SHELL_HTTP_TOKEN="" fails
    // closed (rejects all) rather than accepting an empty `Bearer ` credential.
    let token = token.filter(|t| !t.is_empty());

    if !auth_enabled {
        tracing::warn!(
            "http bridge: AUTH DISABLED (TV_SHELL_HTTP_AUTH_ENABLED=0) — \
             any host on the network can send control commands without authentication"
        );
    } else if token.is_none() {
        // Auth is enabled but no token is configured — all requests will be
        // rejected with 401. Log a loud warning so the operator knows to set it.
        tracing::warn!(
            "http bridge: auth is ENABLED but TV_SHELL_HTTP_TOKEN is not set — \
             all requests will be rejected with 401 (set the token or disable auth \
             with TV_SHELL_HTTP_AUTH_ENABLED=0)"
        );
    } else if addr.ip().is_unspecified() {
        // Token is set but we're binding to 0.0.0.0/:: — still worth a note.
        tracing::warn!(
            "http bridge: binding to {} with bearer auth — \
             any host on the network can attempt authentication",
            addr
        );
    }

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("http bridge: failed to bind {addr}: {e}");
            return;
        }
    };
    tracing::info!("Listening on http://{addr}");

    // Atomic connection counter for the cap (#151 DoS hardening).
    let active_conns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let current = active_conns.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if current >= MAX_ACTIVE_CONNS {
                    active_conns.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::debug!(
                        "http: connection from {peer} rejected — at cap ({MAX_ACTIVE_CONNS})"
                    );
                    // Spawn a minimal task to send 503 and drop the stream.
                    tokio::spawn(async move {
                        let mut s = stream;
                        let resp = http_response(503, "too many connections");
                        let _ = s.write_all(resp.as_bytes()).await;
                    });
                    continue;
                }
                tracing::debug!("http: connection from {peer}");
                let token = token.clone();
                let control_tx = control_tx.clone();
                let events_tx = events_tx.clone();
                let conns = active_conns.clone();
                let shutdown = shutdown.clone();
                let reexec_flag = reexec_flag.clone();
                let metrics = metrics.clone();
                let ui_state = ui_state.clone();
                let display_owner = display_owner.clone();
                let dbus = dbus.clone();
                tokio::spawn(async move {
                    // Use DEV_TIMEOUT_SECS (180 s) for all connections.
                    // /dev/build and /dev/deploy need up to ~15 s and ~30 s
                    // respectively; standard ops finish in <1 s so the extra
                    // budget is free.  Auth is checked before routing, so the
                    // longer timeout is never exposed to unauthenticated senders.
                    // The 503 connection-cap still protects against floods.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(DEV_TIMEOUT_SECS),
                        handle_connection(
                            stream,
                            token.as_deref(),
                            auth_enabled,
                            &control_tx,
                            &events_tx,
                            &shutdown,
                            &reexec_flag,
                            &metrics,
                            &ui_state,
                            &display_owner,
                            &dbus,
                        ),
                    )
                    .await;
                    conns.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
            }
            Err(e) => {
                tracing::debug!("http: accept error: {e}");
            }
        }
    }
}

// ─── Unit tests (pure parser — runs on any host) ────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_request_line ───────────────────────────────────────────────────

    #[test]
    fn intent_simple() {
        assert_eq!(
            parse_request_line("POST", "/intent/menu"),
            Ok(HttpAction::Intent("menu".into()))
        );
    }

    #[test]
    fn intent_with_colon() {
        assert_eq!(
            parse_request_line("POST", "/intent/settings:bluetooth"),
            Ok(HttpAction::Intent("settings:bluetooth".into()))
        );
    }

    #[test]
    fn intent_percent_encoded_colon() {
        // Home Assistant encodes `:` as `%3A` (or `%3a`).
        assert_eq!(
            parse_request_line("POST", "/intent/settings%3Abluetooth"),
            Ok(HttpAction::Intent("settings:bluetooth".into()))
        );
        assert_eq!(
            parse_request_line("POST", "/intent/settings%3abluetooth"),
            Ok(HttpAction::Intent("settings:bluetooth".into()))
        );
    }

    #[test]
    fn intent_deep_link() {
        assert_eq!(
            parse_request_line("POST", "/intent/overlay:volume"),
            Ok(HttpAction::Intent("overlay:volume".into()))
        );
    }

    #[test]
    fn key_simple() {
        assert_eq!(
            parse_request_line("POST", "/key/up"),
            Ok(HttpAction::Key("up".into()))
        );
    }

    #[test]
    fn key_with_query_string_stripped() {
        // Query strings are stripped before routing.
        assert_eq!(
            parse_request_line("POST", "/key/select?foo=bar"),
            Ok(HttpAction::Key("select".into()))
        );
    }

    #[test]
    fn get_screenshot() {
        assert_eq!(
            parse_request_line("GET", "/screenshot"),
            Ok(HttpAction::Screenshot { flash: false })
        );
    }

    #[test]
    fn get_screenshot_png() {
        assert_eq!(
            parse_request_line("GET", "/screenshot.png"),
            Ok(HttpAction::Screenshot { flash: false })
        );
    }

    #[test]
    fn get_screenshot_with_unknown_query_no_flash() {
        // Unknown query params are ignored; flash defaults to false.
        assert_eq!(
            parse_request_line("GET", "/screenshot?foo=bar"),
            Ok(HttpAction::Screenshot { flash: false })
        );
    }

    #[test]
    fn get_screenshot_flash_on() {
        // ?flash=1 enables the post-capture flash event (#166).
        assert_eq!(
            parse_request_line("GET", "/screenshot?flash=1"),
            Ok(HttpAction::Screenshot { flash: true })
        );
    }

    #[test]
    fn get_screenshot_flash_other_values_no_flash() {
        // Only "1" triggers flash; "true"/"yes"/"0" etc. do not.
        assert_eq!(
            parse_request_line("GET", "/screenshot?flash=true"),
            Ok(HttpAction::Screenshot { flash: false })
        );
        assert_eq!(
            parse_request_line("GET", "/screenshot?flash=0"),
            Ok(HttpAction::Screenshot { flash: false })
        );
        assert_eq!(
            parse_request_line("GET", "/screenshot.png?flash=1"),
            Ok(HttpAction::Screenshot { flash: true })
        );
    }

    #[test]
    fn get_screenshot_flash_combined_with_other_params() {
        // flash=1 may appear alongside other params.
        assert_eq!(
            parse_request_line("GET", "/screenshot?foo=bar&flash=1"),
            Ok(HttpAction::Screenshot { flash: true })
        );
    }

    #[test]
    fn post_screenshot_method_not_allowed() {
        assert_eq!(
            parse_request_line("POST", "/screenshot"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn get_method_not_allowed_for_intent() {
        assert_eq!(
            parse_request_line("GET", "/intent/menu"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn get_unknown_path_method_not_allowed() {
        // A GET to an unknown path still gets 405 (not 404) — method check
        // happens before path check for non-screenshot routes.
        assert_eq!(
            parse_request_line("GET", "/foo"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn unknown_path_not_found() {
        assert_eq!(parse_request_line("POST", "/foo"), Err(HttpError::NotFound));
    }

    #[test]
    fn empty_intent_leaf_not_found() {
        assert_eq!(
            parse_request_line("POST", "/intent/"),
            Err(HttpError::NotFound)
        );
    }

    #[test]
    fn empty_key_leaf_not_found() {
        assert_eq!(
            parse_request_line("POST", "/key/"),
            Err(HttpError::NotFound)
        );
    }

    #[test]
    fn root_path_not_found() {
        assert_eq!(parse_request_line("POST", "/"), Err(HttpError::NotFound));
    }

    // ── url_decode ───────────────────────────────────────────────────────────

    #[test]
    fn url_decode_plain() {
        assert_eq!(url_decode("hello"), "hello");
    }

    #[test]
    fn url_decode_colon_percent() {
        assert_eq!(url_decode("settings%3Abluetooth"), "settings:bluetooth");
        assert_eq!(url_decode("settings%3abluetooth"), "settings:bluetooth");
    }

    #[test]
    fn url_decode_plus_is_literal_in_path() {
        assert_eq!(url_decode("hello+world"), "hello+world");
    }

    #[test]
    fn url_decode_malformed_pct_passes_through() {
        // `%ZZ` is not valid hex — pass the `%` through literally.
        assert_eq!(url_decode("%ZZ"), "%ZZ");
    }

    // ── http_response ────────────────────────────────────────────────────────

    #[test]
    fn http_response_200_well_formed() {
        let resp = http_response(200, "ok");
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp.contains("Content-Length: 2\r\n"));
        assert!(resp.contains("Connection: close\r\n"));
        assert!(resp.ends_with("\r\n\r\nok"));
    }

    #[test]
    fn http_response_400_well_formed() {
        let body = "unknown intent 'foo'";
        let resp = http_response(400, body);
        assert!(resp.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(resp.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(resp.ends_with(&format!("\r\n\r\n{body}")));
    }

    #[test]
    fn http_response_401_well_formed() {
        let resp = http_response(401, "unauthorized");
        assert!(resp.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
    }

    #[test]
    fn http_response_404_well_formed() {
        let resp = http_response(404, "not found");
        assert!(resp.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[test]
    fn http_response_405_well_formed() {
        let resp = http_response(405, "method not allowed");
        assert!(resp.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
    }

    #[test]
    fn http_response_500_well_formed() {
        let resp = http_response(500, "grim failed: exit 1");
        assert!(resp.starts_with("HTTP/1.1 500 Internal Server Error\r\n"));
    }

    #[test]
    fn http_response_503_well_formed() {
        let resp = http_response(503, "daemon unavailable");
        assert!(resp.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    }

    // ── png_response_header ──────────────────────────────────────────────────

    #[test]
    fn png_response_header_well_formed() {
        let meta = bridge_core::CaptureMeta {
            captured_at: "2026-06-14T18:26:00Z".to_owned(),
            sha: "a1b2c3d".to_owned(),
            branch: "feat/screenshot-metadata".to_owned(),
            version: "0.1.0",
        };
        let header = png_response_header(1234, &meta);
        let s = std::str::from_utf8(&header).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Content-Type: image/png\r\n"));
        assert!(s.contains("Content-Length: 1234\r\n"));
        assert!(s.contains("X-TvShell-Sha: a1b2c3d\r\n"));
        assert!(s.contains("X-TvShell-Branch: feat/screenshot-metadata\r\n"));
        assert!(s.contains("X-TvShell-Version: 0.1.0\r\n"));
        assert!(s.contains("X-TvShell-Captured-At: 2026-06-14T18:26:00Z\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    // ── extract_authorization ────────────────────────────────────────────────

    #[test]
    fn extract_auth_present() {
        let raw = b"POST /intent/menu HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer secret\r\n\r\n";
        assert_eq!(extract_authorization(raw), Some("Bearer secret".to_owned()));
    }

    #[test]
    fn extract_auth_absent() {
        let raw = b"POST /intent/menu HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(extract_authorization(raw), None);
    }

    // ── ct_eq_str ────────────────────────────────────────────────────────────

    #[test]
    fn ct_eq_str_equal() {
        assert!(ct_eq_str("Bearer secret123", "Bearer secret123"));
    }

    #[test]
    fn ct_eq_str_different() {
        assert!(!ct_eq_str("Bearer secret123", "Bearer secret124"));
        assert!(!ct_eq_str("Bearer a", "Bearer bb"));
        assert!(!ct_eq_str("", "Bearer x"));
    }

    // ── parse_query ──────────────────────────────────────────────────────────

    #[test]
    fn parse_query_empty() {
        assert_eq!(parse_query(""), Vec::<(String, String)>::new());
    }

    #[test]
    fn parse_query_single() {
        assert_eq!(
            parse_query("ref=main"),
            vec![("ref".to_owned(), "main".to_owned())]
        );
    }

    #[test]
    fn parse_query_multiple() {
        let q = parse_query("lines=50&filter=error");
        assert_eq!(q.len(), 2);
        assert_eq!(q[0], ("lines".to_owned(), "50".to_owned()));
        assert_eq!(q[1], ("filter".to_owned(), "error".to_owned()));
    }

    #[test]
    fn parse_query_missing_value() {
        let q = parse_query("foo");
        assert_eq!(q, vec![("foo".to_owned(), "".to_owned())]);
    }

    #[test]
    fn parse_query_value_with_equals() {
        let q = parse_query("foo=bar=baz");
        assert_eq!(q, vec![("foo".to_owned(), "bar=baz".to_owned())]);
    }

    // ── dev routes ───────────────────────────────────────────────────────────

    #[test]
    fn dev_status_get() {
        assert_eq!(
            parse_request_line("GET", "/dev/status"),
            Ok(HttpAction::DevStatus)
        );
    }

    #[test]
    fn dev_status_post_method_not_allowed() {
        assert_eq!(
            parse_request_line("POST", "/dev/status"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn dev_logs_defaults() {
        assert_eq!(
            parse_request_line("GET", "/dev/logs"),
            Ok(HttpAction::DevLogs {
                lines: 100,
                filter: None
            })
        );
    }

    #[test]
    fn dev_logs_with_params() {
        assert_eq!(
            parse_request_line("GET", "/dev/logs?lines=50&filter=error"),
            Ok(HttpAction::DevLogs {
                lines: 50,
                filter: Some("error".to_owned())
            })
        );
    }

    #[test]
    fn dev_logs_lines_only() {
        assert_eq!(
            parse_request_line("GET", "/dev/logs?lines=200"),
            Ok(HttpAction::DevLogs {
                lines: 200,
                filter: None
            })
        );
    }

    #[test]
    fn dev_logs_post_method_not_allowed() {
        assert_eq!(
            parse_request_line("POST", "/dev/logs"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn dev_deploy_no_ref() {
        assert_eq!(
            parse_request_line("POST", "/dev/deploy"),
            Ok(HttpAction::DevDeploy { git_ref: None })
        );
    }

    #[test]
    fn dev_deploy_with_ref() {
        assert_eq!(
            parse_request_line("POST", "/dev/deploy?ref=feat/my-branch"),
            Ok(HttpAction::DevDeploy {
                git_ref: Some("feat/my-branch".to_owned())
            })
        );
    }

    #[test]
    fn dev_deploy_get_method_not_allowed() {
        assert_eq!(
            parse_request_line("GET", "/dev/deploy"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn dev_build() {
        assert_eq!(
            parse_request_line("POST", "/dev/build"),
            Ok(HttpAction::DevBuild)
        );
    }

    #[test]
    fn dev_build_get_method_not_allowed() {
        assert_eq!(
            parse_request_line("GET", "/dev/build"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn dev_restart_shell() {
        assert_eq!(
            parse_request_line("POST", "/dev/restart-shell"),
            Ok(HttpAction::DevRestartShell)
        );
    }

    #[test]
    fn dev_restart_daemon() {
        assert_eq!(
            parse_request_line("POST", "/dev/restart-daemon"),
            Ok(HttpAction::DevRestartDaemon)
        );
    }

    #[test]
    fn dev_restart_daemon_get_method_not_allowed() {
        assert_eq!(
            parse_request_line("GET", "/dev/restart-daemon"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn dev_unknown_path_not_found() {
        assert_eq!(
            parse_request_line("POST", "/dev/nonexistent"),
            Err(HttpError::NotFound)
        );
    }

    // ── /metrics ─────────────────────────────────────────────────────────────

    #[test]
    fn metrics_get() {
        assert_eq!(
            parse_request_line("GET", "/metrics"),
            Ok(HttpAction::Metrics)
        );
    }

    #[test]
    fn metrics_with_query_stripped() {
        // Query strings are stripped before the path match.
        assert_eq!(
            parse_request_line("GET", "/metrics?foo=bar"),
            Ok(HttpAction::Metrics)
        );
    }

    #[test]
    fn metrics_post_method_not_allowed() {
        assert_eq!(
            parse_request_line("POST", "/metrics"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    // ── /status + /suspend (power control) ───────────────────────────────────

    #[test]
    fn shell_status_get() {
        assert_eq!(
            parse_request_line("GET", "/status"),
            Ok(HttpAction::ShellStatus)
        );
        // Query strings are stripped before the path match.
        assert_eq!(
            parse_request_line("GET", "/status?foo=bar"),
            Ok(HttpAction::ShellStatus)
        );
    }

    #[test]
    fn shell_status_is_matched_before_the_post_only_gate() {
        // Regression guard for the route-ordering trap: `parse_request_line`
        // rejects every non-POST method before the generic prefix matches, so a
        // `/status` arm placed below that line would 405 every GET. If this ever
        // returns MethodNotAllowed, the arm has drifted below the gate.
        assert_ne!(
            parse_request_line("GET", "/status"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn shell_status_post_method_not_allowed() {
        assert_eq!(
            parse_request_line("POST", "/status"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn status_body_flattens_both_halves_into_one_object() {
        // The wire contract: ONE flat object carrying the shell fields and the
        // CEC fields side by side. A nested sub-object here would silently break
        // every existing consumer's flat key lookup.
        let body = StatusBody {
            shell: shell_state::status(&shell_state::ShellState::default(), 1_000, false),
            display: crate::display_owner::status(
                &crate::display_owner::Snapshot::default(),
                1_000,
            ),
        };
        let json = serde_json::to_string(&body).unwrap();
        for key in [
            r#""shell_state":null"#,
            r#""media_playing":false"#,
            r#""stale":true"#,
            r#""cec_display_ownership":"unknown""#,
            r#""cec_display_owner":null"#,
            r#""cec_display_owner_tracking":false"#,
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        // Flat, not nested: no sub-object wrappers leaked in.
        assert!(!json.contains(r#""shell":"#), "got: {json}");
        assert!(!json.contains(r#""display":"#), "got: {json}");
    }

    #[test]
    fn shell_status_does_not_shadow_dev_status() {
        // `/dev/status` keeps its own (differently-shaped) blob.
        assert_eq!(
            parse_request_line("GET", "/dev/status"),
            Ok(HttpAction::DevStatus)
        );
    }

    #[test]
    fn suspend_post() {
        assert_eq!(
            parse_request_line("POST", "/suspend"),
            Ok(HttpAction::Suspend)
        );
        assert_eq!(
            parse_request_line("POST", "/suspend?x=1"),
            Ok(HttpAction::Suspend)
        );
    }

    #[test]
    fn suspend_get_method_not_allowed() {
        // A power-off must never be reachable by a bare GET (link prefetch,
        // browser address bar, naive crawler).
        assert_eq!(
            parse_request_line("GET", "/suspend"),
            Err(HttpError::MethodNotAllowed)
        );
    }

    #[test]
    fn interpret_suspend_truth_table() {
        // Table-driven so the inputs aren't compile-time constants (a literal
        // argument trips clippy's assertions_on_constants).
        let cases: [(&str, Option<&str>, SuspendOutcome); 8] = [
            // The gate said yes and logind accepted.
            ("yes", Some("ok"), SuspendOutcome::Accepted),
            // Replies arrive newline-trimmed already, but be defensive.
            ("yes\n", Some("ok\n"), SuspendOutcome::Accepted),
            // Gate refused: a plain `no` (also what a logind bus error degrades
            // to — `handle_can_suspend` deliberately never errors).
            ("no", None, SuspendOutcome::Refused),
            // Gate refused on a non-Linux / unwired build.
            (
                "error:unsupported on this platform",
                None,
                SuspendOutcome::Refused,
            ),
            // Anything unrecognised from the gate is treated as a refusal, not
            // as permission.
            ("challenge", None, SuspendOutcome::Refused),
            // Gate said yes but the suspend call failed — the daemon's reason
            // survives, with the `error:` prefix stripped.
            (
                "yes",
                Some("error:logind unavailable"),
                SuspendOutcome::Failed("logind unavailable".into()),
            ),
            (
                "yes",
                Some("error:suspend failed: boom"),
                SuspendOutcome::Failed("suspend failed: boom".into()),
            ),
            // Unreachable by construction, but the function stays total: a
            // `yes` with no attempt is a failure, never a silent success.
            (
                "yes",
                None,
                SuspendOutcome::Failed("suspend was not attempted".into()),
            ),
        ];
        for (can, suspend, want) in cases {
            assert_eq!(
                interpret_suspend(can, suspend),
                want,
                "interpret_suspend({can:?}, {suspend:?})"
            );
        }
    }

    #[test]
    fn suspend_refusal_is_409_not_500() {
        // A refusal must be distinguishable from a broken daemon on the wire.
        assert!(http_response(409, "suspend refused").starts_with("HTTP/1.1 409 Conflict"));
    }
}

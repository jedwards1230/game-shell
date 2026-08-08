//! Daemon HTTP bridge client — the DEV-OPS data tier for the panel.
//!
//! The daemon exposes an opt-in HTTP bridge (`[http].bind` in config.toml,
//! default off). Routes used here (authoritative: `daemon/src/http.rs`):
//!
//! - `POST /dev/deploy?ref=<ref>`
//! - `POST /dev/build`
//! - `POST /dev/restart-shell`
//! - `POST /dev/restart-daemon`
//! - `GET  /dev/logs?lines=N&filter=<str>` — text log body (default 100,
//!   capped 1000)
//! - `GET  /dev/status` — JSON status blob
//!
//! When the daemon has auth enabled, an `Authorization: Bearer <token>`
//! header is required; this client attaches one whenever a token is
//! configured.

use std::time::Duration;

use async_trait::async_trait;

/// Client timeout. 190s to comfortably exceed the daemon's own `/dev/build`
/// timeout budget (180s, `DEV_TIMEOUT_SECS` in `daemon/src/http.rs`).
const CLIENT_TIMEOUT: Duration = Duration::from_secs(190);

/// Errors a bridge call can produce.
#[derive(Debug)]
pub enum BridgeError {
    /// No `[http].bind` is configured — the bridge client has no base URL.
    NotConfigured,
    /// The bridge could not be reached (connect refused/timeout/DNS/etc).
    Unreachable(String),
    /// The bridge responded with a non-2xx status.
    Status(u16, String),
}

impl BridgeError {
    /// `true` only for [`BridgeError::NotConfigured`] — i.e. whether the
    /// client has a base URL at all is a separate concern from whether it's
    /// currently reachable.
    pub fn is_configured(&self) -> bool {
        !matches!(self, BridgeError::NotConfigured)
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::NotConfigured => write!(f, "HTTP bridge not configured"),
            BridgeError::Unreachable(msg) => write!(f, "HTTP bridge unreachable: {msg}"),
            BridgeError::Status(code, body) => write!(f, "HTTP bridge returned {code}: {body}"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// PNG bytes plus capture provenance, returned by [`BridgeClient::screenshot`].
#[derive(Debug)]
pub struct ScreenshotResponse {
    pub png: Vec<u8>,
    pub sha: String,
    pub branch: String,
    pub version: String,
    pub captured_at: String,
}

/// The daemon's dev-ops surface, as the panel's pages depend on it.
///
/// Deliberately **separate from [`NodeTransport`](crate::transport::NodeTransport)**:
/// this is a different tier. `NodeTransport` is one shape —
/// `command(line) -> reply` — over the control surface; the bridge is a fixed
/// set of named HTTP operations (deploy, build, restart, screenshot, logs)
/// that has no line protocol to hide behind. Folding the two together would
/// mean inventing a command vocabulary for operations that never had one.
///
/// Object-safe under `async_trait`, so `AppState` can hold
/// `Arc<dyn DevBridge>` for the same reason it holds `Arc<dyn NodeTransport>`:
/// the pages depend on *what* the tier does, not on `reqwest`.
#[async_trait]
pub trait DevBridge: Send + Sync {
    /// `GET /dev/status` — JSON status blob (returned as raw text; callers
    /// that need structure can `serde_json::from_str` it).
    async fn dev_status(&self) -> Result<String, BridgeError>;

    /// `GET /dev/logs?lines=N&filter=<str>` — text log body.
    async fn dev_logs(&self, lines: usize, filter: Option<&str>) -> Result<String, BridgeError>;

    /// `POST /dev/deploy?ref=<ref>`.
    async fn deploy(&self, git_ref: Option<&str>) -> Result<String, BridgeError>;

    /// `POST /dev/build`.
    async fn build(&self) -> Result<String, BridgeError>;

    /// `POST /dev/restart-shell`.
    async fn restart_shell(&self) -> Result<String, BridgeError>;

    /// `POST /dev/restart-daemon`.
    async fn restart_daemon(&self) -> Result<String, BridgeError>;

    /// `GET /screenshot` — the current display frame as PNG bytes, with
    /// capture provenance from the response headers.
    async fn screenshot(&self) -> Result<ScreenshotResponse, BridgeError>;
}

/// A client for the daemon's opt-in LAN HTTP dev-ops bridge.
pub struct BridgeClient {
    base: Option<String>,
    token: Option<String>,
    http: reqwest::Client,
}

impl BridgeClient {
    /// Build a client. `base` is `Some("http://host:port")` when
    /// `[http].bind` is configured; `token` is the bearer token to attach
    /// (when the daemon has auth enabled).
    pub fn new(base: Option<String>, token: Option<String>) -> Self {
        // A builder failure (bad system CA store, OOM) is rare but real. Fall
        // back to a default client so the panel still starts — the bridge is an
        // optional, opt-in surface and must not brick the panel — but log loudly:
        // the default client has NO request timeout, so the CLIENT_TIMEOUT ceiling
        // on dev ops is lost until the process is restarted.
        let http = reqwest::Client::builder()
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "bridge: reqwest client build failed ({e}); falling back to a \
                     default client WITHOUT the {CLIENT_TIMEOUT:?} request timeout"
                );
                reqwest::Client::new()
            });
        Self { base, token, http }
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, BridgeError> {
        let base = self.base.as_ref().ok_or(BridgeError::NotConfigured)?;
        let url = format!("{base}{path}");
        let mut req = self.http.get(&url).query(query);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        Self::send(req).await
    }

    async fn post(&self, path: &str, query: &[(&str, &str)]) -> Result<String, BridgeError> {
        let base = self.base.as_ref().ok_or(BridgeError::NotConfigured)?;
        let url = format!("{base}{path}");
        let mut req = self.http.post(&url).query(query);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        Self::send(req).await
    }

    async fn send(req: reqwest::RequestBuilder) -> Result<String, BridgeError> {
        let resp = req
            .send()
            .await
            .map_err(|e| BridgeError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| BridgeError::Unreachable(e.to_string()))?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(BridgeError::Status(status.as_u16(), body))
        }
    }
}

#[async_trait]
impl DevBridge for BridgeClient {
    async fn dev_status(&self) -> Result<String, BridgeError> {
        self.get("/dev/status", &[]).await
    }

    async fn dev_logs(&self, lines: usize, filter: Option<&str>) -> Result<String, BridgeError> {
        let lines_str = lines.to_string();
        let mut query: Vec<(&str, &str)> = vec![("lines", &lines_str)];
        if let Some(f) = filter {
            query.push(("filter", f));
        }
        self.get("/dev/logs", &query).await
    }

    async fn deploy(&self, git_ref: Option<&str>) -> Result<String, BridgeError> {
        let query: Vec<(&str, &str)> = match git_ref {
            Some(r) => vec![("ref", r)],
            None => vec![],
        };
        self.post("/dev/deploy", &query).await
    }

    async fn build(&self) -> Result<String, BridgeError> {
        self.post("/dev/build", &[]).await
    }

    async fn restart_shell(&self) -> Result<String, BridgeError> {
        self.post("/dev/restart-shell", &[]).await
    }

    async fn restart_daemon(&self) -> Result<String, BridgeError> {
        self.post("/dev/restart-daemon", &[]).await
    }

    /// Reads capture provenance from the `X-TvShell-{Sha,Branch,Version,
    /// Captured-At}` response headers (`docs/CONTROL_SURFACE.md`). Also
    /// accepts the legacy `X-GameShell-*` header names via a prefix check,
    /// so this keeps working against a daemon mid-rename that hasn't picked
    /// up the `X-TvShell-*` names yet.
    async fn screenshot(&self) -> Result<ScreenshotResponse, BridgeError> {
        let base = self.base.as_ref().ok_or(BridgeError::NotConfigured)?;
        let url = format!("{base}/screenshot");
        let mut req = self.http.get(&url);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| BridgeError::Unreachable(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BridgeError::Status(status.as_u16(), body));
        }
        let sha = provenance_header(&resp, "Sha");
        let branch = provenance_header(&resp, "Branch");
        let version = provenance_header(&resp, "Version");
        let captured_at = provenance_header(&resp, "Captured-At");
        let png = resp
            .bytes()
            .await
            .map_err(|e| BridgeError::Unreachable(e.to_string()))?
            .to_vec();
        Ok(ScreenshotResponse {
            png,
            sha,
            branch,
            version,
            captured_at,
        })
    }
}

/// Read a capture-provenance header by `suffix` (e.g. `"Sha"`), preferring
/// `X-TvShell-<suffix>` and falling back to the legacy `X-GameShell-<suffix>`
/// name. Missing/non-UTF8 header yields `""`.
fn provenance_header(resp: &reqwest::Response, suffix: &str) -> String {
    for prefix in ["X-TvShell-", "X-GameShell-"] {
        if let Some(v) = resp.headers().get(format!("{prefix}{suffix}").as_str()) {
            if let Ok(s) = v.to_str() {
                return s.to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_configured_when_base_is_none() {
        let client = BridgeClient::new(None, None);
        let err = client.dev_status().await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConfigured));
        assert!(!err.is_configured());
    }

    #[tokio::test]
    async fn screenshot_not_configured_when_base_is_none() {
        let client = BridgeClient::new(None, None);
        let err = client.screenshot().await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConfigured));
    }

    /// Hand-roll a minimal HTTP/1.1 response (mirrors `ipc.rs`'s tests
    /// hand-rolling the Unix-socket wire protocol) rather than pulling in a
    /// test HTTP server crate, to check both the `X-TvShell-*` primary
    /// header names and the `X-GameShell-*` legacy fallback.
    async fn spawn_screenshot_server(header_prefix: &'static str) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await; // drain the request
                let body: &[u8] = b"\x89PNGfakebytes";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: image/png\r\n\
                     Content-Length: {len}\r\n\
                     {prefix}Sha: abc123\r\n\
                     {prefix}Branch: main\r\n\
                     {prefix}Version: 0.1.0\r\n\
                     {prefix}Captured-At: 2026-01-01T00:00:00Z\r\n\
                     \r\n",
                    len = body.len(),
                    prefix = header_prefix,
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn screenshot_parses_tvshell_provenance_headers_and_body() {
        let addr = spawn_screenshot_server("X-TvShell-").await;
        let client = BridgeClient::new(Some(format!("http://{addr}")), None);
        let shot = client.screenshot().await.unwrap();
        assert_eq!(shot.sha, "abc123");
        assert_eq!(shot.branch, "main");
        assert_eq!(shot.version, "0.1.0");
        assert_eq!(shot.captured_at, "2026-01-01T00:00:00Z");
        assert_eq!(shot.png, b"\x89PNGfakebytes");
    }

    #[tokio::test]
    async fn screenshot_falls_back_to_legacy_gameshell_headers() {
        let addr = spawn_screenshot_server("X-GameShell-").await;
        let client = BridgeClient::new(Some(format!("http://{addr}")), None);
        let shot = client.screenshot().await.unwrap();
        assert_eq!(shot.sha, "abc123");
        assert_eq!(shot.branch, "main");
    }
}

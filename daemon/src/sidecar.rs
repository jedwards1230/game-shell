//! Reusable client plumbing for a remote tv-shell widget **sidecar**.
//!
//! A widget that needs heavy backend logic ships a sidecar process; the daemon
//! reaches it over the LAN. [`crate::steam`] (the Steam library/launch backend,
//! `tv-shell-host`, running on the gaming PC) is the first — and today only —
//! consumer.
//!
//! **The daemon is an HTTP _client_, not a process supervisor.** The sidecar runs
//! on a *different machine* (see `docs/HOST_SETUP.md`), so there is no spawn,
//! health-restart, or shutdown here — "lifecycle" is reachability (is it
//! answering?), not process management. This module owns the three things every
//! such sidecar client needs, so a future widget backend reuses them instead of
//! re-deriving them:
//!   1. holding a base URL + bearer token ([`Sidecar::from_parts`]),
//!   2. a reachability probe ([`Sidecar::probe`]) over the shared
//!      [`crate::service_health`] vocabulary, and
//!   3. bearer HTTP request helpers with response size caps and typed JSON
//!      decoding ([`Sidecar::get_json`] / [`Sidecar::get_classified`] /
//!      [`Sidecar::post`]).
//!
//! All requests use the shared [`crate::service_health::build_client`] policy
//! (short timeouts, self-signed-tolerant) so every remote-service fetch in the
//! daemon shares one client configuration.

use crate::service_health::{build_client, classify_code, probe_get, ServiceStatus};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A configured remote sidecar endpoint: its base URL and bearer token.
#[derive(Clone)]
pub struct Sidecar {
    base: String,
    token: String,
}

// Custom Debug that redacts the bearer token — never print the secret, even if
// something downstream debug-formats a `Sidecar`.
impl std::fmt::Debug for Sidecar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidecar")
            .field("base", &self.base)
            .field("token", &"***")
            .finish()
    }
}

/// Error from a sidecar request.
#[derive(Debug)]
pub enum SidecarError {
    /// Transport failure or a non-2xx HTTP status (`error_for_status`).
    Http(reqwest::Error),
    /// The body did not decode as the expected type.
    Json(serde_json::Error),
    /// The body exceeded the caller's size cap. Either `Content-Length` advertised
    /// more than the cap (refused before reading a byte), or — when the header was
    /// absent or lying — the streamed body crossed the cap and was abandoned
    /// without buffering it whole. A DoS guard against a rogue/compromised host;
    /// the request timeout alone doesn't bound body size. The `u64` is the size
    /// seen at the point of refusal.
    TooLarge(u64),
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Http(e) => write!(f, "http: {e}"),
            SidecarError::Json(e) => write!(f, "json: {e}"),
            SidecarError::TooLarge(n) => write!(f, "response body too large: {n} bytes"),
        }
    }
}

impl std::error::Error for SidecarError {}

impl Sidecar {
    /// Build from a base URL + bearer token. The base is trimmed and a trailing
    /// slash removed so path joins are clean; returns `None` when either the base
    /// or the token is empty (the widget is effectively unconfigured). The token
    /// is kept verbatim — the host compares it byte-for-byte, so trimming it here
    /// would silently break a token whose file legitimately contains those bytes.
    pub fn from_parts(base: &str, token: &str) -> Option<Self> {
        let base = base.trim().trim_end_matches('/').to_string();
        if base.is_empty() || token.is_empty() {
            return None;
        }
        Some(Self {
            base,
            token: token.to_string(),
        })
    }

    /// The trailing-slash-trimmed base URL — callers use it to build art/asset
    /// URLs that point back at the sidecar.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The host part of this sidecar's base URL (see [`url_host`]).
    pub fn host(&self) -> Option<String> {
        url_host(&self.base)
    }

    /// `Authorization` header value for every request.
    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Reachability probe: authenticated `GET {base}/status`, classified by the
    /// shared health vocabulary (401 ⇒ `Error`/bad token, 5xx ⇒ `Unreachable`,
    /// transport failure ⇒ `Unreachable`). The lightweight check a background
    /// poller uses to answer "is the sidecar up?" without fetching real data.
    pub async fn probe(&self) -> ServiceStatus {
        let url = format!("{}/status", self.base);
        probe_get(&url, &[("Authorization", &self.bearer())]).await
    }

    /// `GET {base}{path}` with the bearer + `Accept: application/json`, returning
    /// the reachability classification and — only when reachable (`Ok`) — the
    /// response body as text. A body exceeding `max_bytes` (advertised via
    /// `Content-Length` or detected while streaming) is dropped (`None`) while the
    /// `Ok` status is kept. Use when the caller needs BOTH the
    /// reachability signal and the body from a single request (e.g. `/status`,
    /// whose body also carries the foreground-game id).
    pub async fn get_classified(
        &self,
        path: &str,
        max_bytes: u64,
    ) -> (ServiceStatus, Option<String>) {
        let url = format!("{}{}", self.base, path);
        // A client we can't even build means we never reached the server — that's
        // Unreachable (transient), not Error (server reached us and rejected us).
        let client = match build_client() {
            Ok(c) => c,
            Err(_) => return (ServiceStatus::Unreachable, None),
        };
        let resp = match client
            .get(&url)
            .header("Authorization", self.bearer())
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return (ServiceStatus::Unreachable, None),
        };
        let status = classify_code(resp.status().as_u16());
        if status != ServiceStatus::Ok {
            return (status, None);
        }
        if resp.content_length().is_some_and(|len| len > max_bytes) {
            tracing::warn!("sidecar GET {url} body too large (> {max_bytes} bytes); ignoring");
            return (status, None);
        }
        // `Content-Length` is optional (and forgeable) in HTTP, so the early-reject
        // above is only a fast path — stream the body with a hard cap so a
        // header-less or lying response can't read unbounded data into memory.
        match read_body_capped(resp, max_bytes).await {
            Ok(Ok(bytes)) => match String::from_utf8(bytes) {
                Ok(body) => (status, Some(body)),
                Err(_) => (status, None),
            },
            Ok(Err(_)) => {
                tracing::warn!("sidecar GET {url} body too large (> {max_bytes} bytes); ignoring");
                (status, None)
            }
            Err(_) => (status, None),
        }
    }

    /// `GET {base}{path}` with the bearer + `Accept: application/json`, then decode
    /// the body into `T`. A non-2xx is an error (`error_for_status`), a body over
    /// `max_bytes` is refused (up-front via `Content-Length`, else by capping the
    /// stream) as [`SidecarError::TooLarge`], and a decode failure surfaces as
    /// [`SidecarError::Json`] (which closes daemon↔sidecar schema drift — a host
    /// that changed the shape fails loudly here instead of silently dropping
    /// fields, the way the previous untyped `Value` parse did).
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        max_bytes: u64,
    ) -> Result<T, SidecarError> {
        let url = format!("{}{}", self.base, path);
        let client = build_client().map_err(SidecarError::Http)?;
        let resp = client
            .get(&url)
            .header("Authorization", self.bearer())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(SidecarError::Http)?
            .error_for_status()
            .map_err(SidecarError::Http)?;
        if let Some(len) = resp.content_length() {
            if len > max_bytes {
                return Err(SidecarError::TooLarge(len));
            }
        }
        // `Content-Length` is optional (and forgeable) in HTTP, so the early-reject
        // above is only a fast path — stream the body with a hard cap so a
        // header-less or lying response can't read unbounded data into memory.
        let bytes = match read_body_capped(resp, max_bytes)
            .await
            .map_err(SidecarError::Http)?
        {
            Ok(bytes) => bytes,
            Err(seen) => return Err(SidecarError::TooLarge(seen)),
        };
        serde_json::from_slice(&bytes).map_err(SidecarError::Json)
    }

    /// `POST {base}{path}` with the bearer and an optional JSON body. Any non-2xx
    /// is an error. `Content-Type: application/json` is set only when a body is
    /// present (a bodyless action like `/open-bpm` sends neither).
    pub async fn post(&self, path: &str, body: Option<&Value>) -> Result<(), reqwest::Error> {
        let url = format!("{}{}", self.base, path);
        let client = build_client()?;
        let mut req = client.post(&url).header("Authorization", self.bearer());
        if let Some(body) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let resp = req.send().await?.error_for_status()?;
        // Drain + discard the response under a hard cap so a rogue/compromised
        // sidecar can't OOM us by streaming an unbounded body. The POST already
        // succeeded (2xx) and these endpoints' response bodies are irrelevant to
        // the caller — capping the drain keeps it a real DoS guard, matching
        // get_json()/get_classified().
        const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
        let _ = read_body_capped(resp, MAX_RESPONSE_BYTES).await?;
        Ok(())
    }

    /// `POST {base}{path}` like [`Self::post`], but DECODES the response body into
    /// `T` instead of discarding it.
    ///
    /// Use this for an action endpoint whose reply carries a decision the daemon
    /// must act on rather than just a 2xx — `/sleep`, whose `{ok, reason}` body
    /// distinguishes "suspended" from "refused, because a game is running". Both
    /// are HTTP 200, so `post`'s status-only contract would silently swallow the
    /// refusal. Same size-cap and error vocabulary as [`Self::get_json`].
    pub async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&Value>,
        max_bytes: u64,
    ) -> Result<T, SidecarError> {
        let url = format!("{}{}", self.base, path);
        let client = build_client().map_err(SidecarError::Http)?;
        let mut req = client
            .post(&url)
            .header("Authorization", self.bearer())
            .header("Accept", "application/json");
        if let Some(body) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let resp = req
            .send()
            .await
            .map_err(SidecarError::Http)?
            .error_for_status()
            .map_err(SidecarError::Http)?;
        if let Some(len) = resp.content_length() {
            if len > max_bytes {
                return Err(SidecarError::TooLarge(len));
            }
        }
        // `Content-Length` is optional (and forgeable), so the early-reject above
        // is only a fast path — stream with a hard cap, matching `get_json`.
        let bytes = match read_body_capped(resp, max_bytes)
            .await
            .map_err(SidecarError::Http)?
        {
            Ok(bytes) => bytes,
            Err(seen) => return Err(SidecarError::TooLarge(seen)),
        };
        serde_json::from_slice(&bytes).map_err(SidecarError::Json)
    }
}

/// The host part (IP or hostname, no port/path) of an `http(s)://` base URL —
/// e.g. `"http://192.0.2.1:47995"` → `"192.0.2.1"`. This is what the shell shows
/// in a server picker and passes to `wol <host>`; the full URL (and token) never
/// leave the daemon. `None` for an empty/degenerate URL. Hand-rolled (scheme →
/// authority → strip port) so we don't pull in a URL crate for one field; an
/// IPv6 literal's brackets are stripped but its inner colons are preserved.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .split_once("://")
        .map_or(url.trim(), |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Userinfo (rare, but legal) ends at '@'.
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = hostport.strip_prefix('[') {
        // Bracketed IPv6 literal: host is everything inside the brackets.
        v6.split(']').next().unwrap_or("")
    } else {
        // Only the LAST colon can start a port; an unbracketed name has at most
        // one (`host:port`).
        hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Stream a response body into memory under a hard `max_bytes` cap.
///
/// `Content-Length` is optional — and forgeable — in HTTP, so the only sound
/// bound is to count bytes as chunks arrive and stop the instant the running
/// total crosses the cap, never buffering more than a single chunk past it. This
/// is what makes the size cap a real DoS guard rather than a post-hoc check on an
/// already fully-buffered body.
///
/// Returns `Ok(Ok(body))` when the body fits, `Ok(Err(seen))` when it exceeds the
/// cap (`seen` = bytes read at the point of refusal, a lower bound on the true
/// size), and `Err(_)` for a transport failure while reading.
async fn read_body_capped(
    mut resp: reqwest::Response,
    max_bytes: u64,
) -> Result<Result<Vec<u8>, u64>, reqwest::Error> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        let total = buf.len() as u64 + chunk.len() as u64;
        if total > max_bytes {
            return Ok(Err(total));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(Ok(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_parts_trims_trailing_slash_and_whitespace() {
        let sc = Sidecar::from_parts("  http://host:47995/  ", "tok").unwrap();
        assert_eq!(sc.base(), "http://host:47995");
        assert_eq!(sc.bearer(), "Bearer tok");
    }

    #[test]
    fn from_parts_keeps_token_verbatim() {
        // The host compares the bearer byte-for-byte, so a token is NOT trimmed.
        let sc = Sidecar::from_parts("http://h", " spaced ").unwrap();
        assert_eq!(sc.bearer(), "Bearer  spaced ");
    }

    #[test]
    fn from_parts_none_when_base_empty() {
        assert!(Sidecar::from_parts("   ", "tok").is_none());
        assert!(Sidecar::from_parts("", "tok").is_none());
    }

    #[test]
    fn from_parts_none_when_token_empty() {
        assert!(Sidecar::from_parts("http://h", "").is_none());
    }

    #[test]
    fn url_host_extracts_host_from_common_shapes() {
        assert_eq!(url_host("http://192.0.2.1:47995"), Some("192.0.2.1".into()));
        assert_eq!(
            url_host("http://gaming-pc:47995/"),
            Some("gaming-pc".into())
        );
        assert_eq!(url_host("https://host"), Some("host".into()));
        assert_eq!(url_host("host:47995"), Some("host".into()));
        assert_eq!(url_host("http://u:p@host:1/x?q#f"), Some("host".into()));
        assert_eq!(url_host("http://[fe80::1]:47995"), Some("fe80::1".into()));
        assert_eq!(url_host(""), None);
        assert_eq!(url_host("http://"), None);
    }

    #[test]
    fn sidecar_host_matches_url_host() {
        let sc = Sidecar::from_parts("http://192.0.2.1:47995/", "tok").unwrap();
        assert_eq!(sc.host(), Some("192.0.2.1".into()));
    }

    // Build a `reqwest::Response` from an in-memory body with NO `Content-Length`
    // header — the exact shape (legal HTTP, header absent) the streaming cap must
    // defend against. No live server needed.
    fn response_with_body(body: Vec<u8>) -> reqwest::Response {
        reqwest::Response::from(http::Response::new(body))
    }

    #[tokio::test]
    async fn read_body_capped_returns_body_within_cap() {
        let body = b"hello world".to_vec();
        let out = read_body_capped(response_with_body(body.clone()), 1024)
            .await
            .expect("transport should not fail on an in-memory body");
        assert_eq!(out, Ok(body));
    }

    #[tokio::test]
    async fn read_body_capped_accepts_body_exactly_at_cap() {
        let body = vec![b'y'; 16];
        let out = read_body_capped(response_with_body(body.clone()), 16)
            .await
            .expect("transport");
        assert_eq!(out, Ok(body));
    }

    #[tokio::test]
    async fn read_body_capped_rejects_oversized_without_content_length() {
        // 100-byte body, 10-byte cap, and NO Content-Length: the body must still be
        // rejected (the DoS guard) rather than read unbounded into memory. `seen`
        // is the byte count observed when the cap was crossed.
        let body = vec![b'x'; 100];
        let out = read_body_capped(response_with_body(body), 10)
            .await
            .expect("transport");
        match out {
            Err(seen) => assert!(seen > 10, "seen={seen} should exceed the 10-byte cap"),
            Ok(_) => panic!("oversized body should be rejected, not returned"),
        }
    }
}

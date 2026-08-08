//! Panel authentication (S1) — one `axum::middleware::from_fn_with_state`
//! layer in front of every route.
//!
//! Two credentials, one secret: a browser session **cookie** set by the
//! `/login` form, and an `Authorization: Bearer` header for scripted access.
//! Both carry the same value — the contents of `[panel].token_file` — and both
//! are compared **constant-time** (`subtle::ConstantTimeEq`, the same primitive
//! the daemon's `bridge_core::ct_eq_str` uses).
//!
//! Deliberate design calls, all documented in `docs/PANEL.md`:
//!
//! - **The cookie value IS the token.** No session store, no separate session
//!   id: the panel has exactly one credential, so a session table would add
//!   state without adding a security property.
//! - **`Secure` is deliberately omitted** from the cookie. The panel is served
//!   over plain HTTP on the LAN; `Secure` would make login impossible.
//!   `HttpOnly` + `SameSite=Strict` + `Path=/` are all set.
//! - **Exactly four routes are exempt**: `GET /assets/htmx.min.js`,
//!   `GET /assets/style.css`, `GET /login`, `POST /login`. Everything else —
//!   including `GET /nav/daemon-status` — is gated.
//! - **Fail closed**: auth on (`[panel].token_file` configured) with no
//!   resolvable token rejects every request. Startup already refuses that
//!   combination (`config::AppConfig::validate`); this is defence in depth.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use subtle::ConstantTimeEq;

use crate::config::AppConfig;
use crate::state::SharedState;

/// Name of the session cookie the `/login` form sets.
pub const SESSION_COOKIE: &str = "tv_shell_panel_session";

/// The auth-exempt request paths. `/login` covers both its `GET` (the form)
/// and its `POST` (the submission) — four exempt routes across three paths.
/// Everything else the router registers is authenticated.
pub const PUBLIC_PATHS: [&str; 3] = ["/assets/htmx.min.js", "/assets/style.css", "/login"];

/// Cookie attributes for the session cookie. See the module docs for why
/// `Secure` is absent.
const COOKIE_ATTRS: &str = "HttpOnly; SameSite=Strict; Path=/";

/// Whether `path` is one of the four documented auth exemptions.
pub fn is_public(path: &str) -> bool {
    PUBLIC_PATHS.contains(&path)
}

/// Constant-time string comparison — the panel's counterpart to the daemon's
/// `bridge_core::ct_eq_str`, using the same `subtle::ConstantTimeEq` on the
/// UTF-8 bytes. A length mismatch leaks only the length.
pub fn ct_eq_str(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Build the `Set-Cookie` value for a successful login.
pub fn session_cookie(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}; {COOKIE_ATTRS}")
}

/// Extract the credential a request presents: `Authorization: Bearer <token>`
/// first, then the session cookie. Returns `None` when neither is present.
pub fn presented_token(headers: &HeaderMap) -> Option<&str> {
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(strip_bearer)
    {
        return Some(bearer);
    }
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(session_cookie_value)
}

/// `"Bearer <token>"` → `Some("<token>")`, scheme compared case-insensitively
/// per RFC 7235. Any other scheme (or no scheme) yields `None`.
fn strip_bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("Bearer")
        .then(|| rest.trim())
        .filter(|t| !t.is_empty())
}

/// Pull the session cookie's value out of a `Cookie:` header. Hand-rolled on
/// purpose: parsing one `name=value` pair out of a semicolon list does not
/// justify a cookie crate in a binary that ships to an HTPC.
fn session_cookie_value(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == SESSION_COOKIE)
            .then(|| value.trim())
            .filter(|v| !v.is_empty())
    })
}

/// The single authorization decision. `true` ⇒ the request may proceed.
///
/// - Auth off (`[panel].token_file` absent) ⇒ everything is allowed; this
///   preserves the loopback dev experience.
/// - Auth on but no token resolved ⇒ **nothing** is allowed (fail closed).
/// - Otherwise the presented credential must equal the token, constant-time.
pub fn authorize(cfg: &AppConfig, headers: &HeaderMap) -> bool {
    if !cfg.auth_enabled() {
        return true;
    }
    let Some(expected) = cfg.panel_token.as_deref() else {
        return false;
    };
    let Some(presented) = presented_token(headers) else {
        return false;
    };
    ct_eq_str(presented, expected)
}

/// The auth layer. Applied with `Router::route_layer`, so it runs for every
/// MATCHED route (an unregistered path is a plain 404 — see `main::build_router`,
/// where `[panel].allow_dangerous = false` simply does not register the
/// root-equivalent routes).
pub async fn require_auth(
    State(state): State<SharedState>,
    request: Request,
    next: Next,
) -> Response {
    if is_public(request.uri().path()) || authorize(&state.cfg, request.headers()) {
        return next.run(request).await;
    }
    unauthorized(request.headers())
}

/// Response shape for an unauthenticated request.
///
/// htmx first: an `HX-Request` swap must NEVER receive an HTML login page (it
/// would be spliced into whatever target the caller declared — e.g. the nav
/// status dot). A browser navigation gets a redirect to the login form.
/// Anything else (curl, a script) gets a plain `401`.
pub fn unauthorized(headers: &HeaderMap) -> Response {
    if is_htmx(headers) {
        return plain_401("unauthorized: panel session expired — reload the page to sign in\n");
    }
    if accepts_html(headers) {
        return Redirect::to("/login").into_response();
    }
    plain_401("unauthorized: POST /login or send `Authorization: Bearer <token>`\n")
}

fn plain_401(body: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers
        .get("HX-Request")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_token(token: Option<&str>) -> AppConfig {
        AppConfig {
            panel_token_file: Some("~/.config/tv-shell/panel-token".to_string()),
            panel_token: token.map(str::to_string),
            ..AppConfig::default()
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn ct_eq_str_matches_only_identical_strings() {
        assert!(ct_eq_str("token", "token"));
        assert!(!ct_eq_str("token", "tokeN"));
        assert!(!ct_eq_str("token", "token-longer"));
        assert!(!ct_eq_str("", "token"));
        assert!(ct_eq_str("", ""));
    }

    /// The auth decision must go through the constant-time helper — not `==`.
    /// Pinning the call site in source keeps a future refactor from quietly
    /// swapping in a short-circuiting comparison.
    #[test]
    fn authorize_compares_constant_time() {
        let src = include_str!("auth.rs");
        assert!(
            src.contains("ct_eq_str(presented, expected)"),
            "authorize() must compare the credential with ct_eq_str"
        );
        assert!(
            src.contains("use subtle::ConstantTimeEq;"),
            "ct_eq_str must be backed by subtle::ConstantTimeEq"
        );
    }

    #[test]
    fn exactly_three_public_paths_covering_four_routes() {
        assert_eq!(
            PUBLIC_PATHS,
            ["/assets/htmx.min.js", "/assets/style.css", "/login"]
        );
        assert!(is_public("/login"));
        assert!(!is_public("/nav/daemon-status"));
        assert!(!is_public("/"));
        assert!(!is_public("/assets/../"));
    }

    #[test]
    fn auth_disabled_allows_everything() {
        let cfg = AppConfig::default();
        assert!(!cfg.auth_enabled());
        assert!(authorize(&cfg, &HeaderMap::new()));
    }

    #[test]
    fn auth_enabled_without_a_token_fails_closed() {
        let cfg = cfg_with_token(None);
        assert!(cfg.auth_enabled());
        assert!(!authorize(&cfg, &HeaderMap::new()));
        assert!(!authorize(
            &cfg,
            &headers(&[("authorization", "Bearer anything")])
        ));
    }

    #[test]
    fn bearer_and_cookie_are_both_accepted() {
        let cfg = cfg_with_token(Some("s3kret"));
        assert!(authorize(
            &cfg,
            &headers(&[("authorization", "Bearer s3kret")])
        ));
        assert!(authorize(
            &cfg,
            &headers(&[("authorization", "bearer s3kret")])
        ));
        assert!(authorize(
            &cfg,
            &headers(&[("cookie", "tv_shell_panel_session=s3kret")])
        ));
        assert!(authorize(
            &cfg,
            &headers(&[("cookie", "other=1; tv_shell_panel_session=s3kret; x=2")])
        ));
    }

    #[test]
    fn wrong_or_missing_credentials_are_rejected() {
        let cfg = cfg_with_token(Some("s3kret"));
        assert!(!authorize(&cfg, &HeaderMap::new()));
        assert!(!authorize(
            &cfg,
            &headers(&[("authorization", "Bearer wrong")])
        ));
        assert!(!authorize(
            &cfg,
            &headers(&[("authorization", "Basic czo=")])
        ));
        assert!(!authorize(
            &cfg,
            &headers(&[("cookie", "tv_shell_panel_session=wrong")])
        ));
        assert!(!authorize(
            &cfg,
            &headers(&[("cookie", "unrelated=s3kret")])
        ));
    }

    #[test]
    fn session_cookie_carries_the_hardened_flags_and_no_secure() {
        let c = session_cookie("s3kret");
        assert!(c.starts_with("tv_shell_panel_session=s3kret;"));
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("SameSite=Strict"));
        assert!(c.contains("Path=/"));
        assert!(
            !c.contains("Max-Age") && !c.contains("Expires"),
            "must be a session cookie: {c}"
        );
        assert!(
            !c.contains("Secure"),
            "Secure is deliberately omitted — the panel is plain HTTP on the LAN: {c}"
        );
    }

    #[test]
    fn htmx_gets_a_plain_401_never_a_login_page() {
        let resp = unauthorized(&headers(&[
            ("hx-request", "true"),
            ("accept", "text/html, */*"),
        ]));
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    fn browser_navigation_is_redirected_to_login() {
        let resp = unauthorized(&headers(&[(
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9",
        )]));
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/login");
    }

    #[test]
    fn scripted_client_gets_a_401() {
        let resp = unauthorized(&headers(&[("accept", "*/*")]));
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = unauthorized(&HeaderMap::new());
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

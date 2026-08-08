//! `/login` — the panel's only unauthenticated HTML surface (S1).
//!
//! A single-field form that exchanges the `[panel].token_file` secret for the
//! session cookie described in [`crate::auth`]. Deliberately standalone (it
//! does NOT extend `base.html`): the layout's nav links and its
//! `/nav/daemon-status` htmx poll are all authenticated, so rendering them to
//! a signed-out visitor would produce a page of 401s.

use askama::Template;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth;
use crate::state::SharedState;

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    /// Inline error banner text; empty on a first visit.
    error: String,
    /// `false` when `[panel].token_file` is unset — the form is pointless
    /// then, so the page explains that instead of inviting a login.
    auth_enabled: bool,
}

#[derive(Deserialize)]
pub struct LoginForm {
    token: String,
}

fn render(status: StatusCode, auth_enabled: bool, error: &str) -> Response {
    let tmpl = LoginTemplate {
        error: error.to_string(),
        auth_enabled,
    };
    match tmpl.render() {
        Ok(html) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            html,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("template render error: {e}"),
        )
            .into_response(),
    }
}

/// `GET /login` — the sign-in form (auth-exempt).
pub async fn page(State(state): State<SharedState>) -> Response {
    render(StatusCode::OK, state.cfg.auth_enabled(), "")
}

/// `POST /login` — validate the submitted token constant-time and, on a
/// match, set the session cookie and send the operator to the dashboard.
///
/// A failure re-renders the form with `401` and the SAME message regardless of
/// why it failed, so the response distinguishes nothing beyond "wrong".
pub async fn submit(State(state): State<SharedState>, Form(form): Form<LoginForm>) -> Response {
    if !state.cfg.auth_enabled() {
        return render(
            StatusCode::BAD_REQUEST,
            false,
            "Panel authentication is not configured ([panel].token_file is unset).",
        );
    }
    let submitted = form.token.trim();
    let ok = state
        .cfg
        .panel_token
        .as_deref()
        .is_some_and(|expected| auth::ct_eq_str(submitted, expected));
    if !ok {
        return render(StatusCode::UNAUTHORIZED, true, "Incorrect token.");
    }

    let cookie = match HeaderValue::from_str(&auth::session_cookie(submitted)) {
        Ok(v) => v,
        // Unreachable in practice: the value just matched a token read from a
        // file and trimmed, so it holds no control characters. Degrade to the
        // form rather than panicking a long-running daemon.
        Err(_) => {
            return render(
                StatusCode::BAD_REQUEST,
                true,
                "That token cannot be stored in a cookie (unexpected characters).",
            )
        }
    };
    let mut resp = Redirect::to("/").into_response();
    resp.headers_mut().insert(header::SET_COOKIE, cookie);
    resp
}

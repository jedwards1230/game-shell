//! Page handlers. Every page is fully implemented: `dashboard`, `logs`,
//! `dev`, `settings`, `widgets`, `tools`, `processes` (M1-M3), `controllers`
//! and `cec` (M4), plus `services` and `updates` — the two halves the
//! Processes page was split into in `docs/PANEL_IA.md` phase 2.
//!
//! Two modules here are not pages: `redirects` holds the forwarding addresses
//! from the pre-IA paths (`docs/PANEL_IA.md` phase 1), and `units` holds the
//! single systemd-unit-state presentation helper `dashboard`, `dev` and
//! `services` all render.

pub mod cec;
pub mod controllers;
pub mod dashboard;
pub mod dev;
pub mod login;
pub mod logs;
pub mod media;
pub mod nav;
pub mod processes;
pub mod redirects;
pub mod services;
pub mod settings;
pub mod tools;
pub mod units;
pub mod updates;
pub mod widgets;

use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

/// Render any askama template to a 200 HTML response, or a 500 plain-text
/// response on a render error.
pub fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("template render error: {e}"),
        )
            .into_response(),
    }
}

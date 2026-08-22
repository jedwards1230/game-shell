//! Page handlers, one module and one template per page, named for the page.
//!
//! The set follows `docs/PANEL_IA.md`'s six groups: `dashboard` (Overview);
//! `services`, `processes`, `updates` and `logs` (System); `appearance`,
//! `widgets`, `apps` and `advanced` (Shell); `controllers`, `display_audio`,
//! `cec` and `network` (Devices); `navigation` and `launcher` (Remote); and
//! `dev`, `screenshot` and `console` (Dev), plus `login`.
//!
//! Four modules here are **not** pages:
//!
//! * `redirects` — the forwarding addresses from the pre-IA paths (phase 1).
//! * `units` — the single systemd-unit-state presentation helper `dashboard`,
//!   `dev` and `services` all render.
//! * `settings` — the `settings.json` schema, form renderer and scoped
//!   save-patch builder the five settings forms share (phase 3, when the
//!   Settings page dissolved).
//! * `ipc_console` — the IPC result partial, reply pretty-printer and argument
//!   validators the pages the Tools console dissolved into share (phase 4).
//!
//! The last two are what is left of the two dissolved grab-bag pages: neither
//! Settings nor Tools had a subject, but each had real shared machinery, so
//! the machinery stayed and the page went.

pub mod advanced;
pub mod appearance;
pub mod apps;
pub mod cec;
pub mod console;
pub mod controllers;
pub mod dashboard;
pub mod dev;
pub mod display_audio;
pub mod ipc_console;
pub mod launcher;
pub mod login;
pub mod logs;
pub mod nav;
pub mod navigation;
pub mod network;
pub mod processes;
pub mod redirects;
pub mod screenshot;
pub mod services;
pub mod settings;
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

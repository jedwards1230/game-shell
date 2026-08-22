//! Permanent-enough redirects from the pre-IA paths to their new homes
//! (`docs/PANEL_IA.md` phase 1).
//!
//! The panel's URLs are bookmarked, pasted into issues, and typed from memory,
//! so moving a page has to leave a forwarding address rather than a 404. One
//! `pub async fn` per redirect — deliberately not a single generic handler
//! parameterized over the path, because `crate::tests`'s `main.rs` parser reads
//! `.route("<literal>", get(<handler path>))` and a table-driven registration
//! would hide the old paths from both it and `route_table()`.
//!
//! **Each redirect is registered in the same `build_router` block as its
//! target**, so a redirect can never land on a route the capability snapshot
//! gated away: if the new page 404s, the old path 404s too, which is the honest
//! answer.
//!
//! [`Redirect::to`] is a **303 See Other** — uncached, and it rewrites the
//! method to GET. Both are what we want here: these are all page GETs, and a
//! 301 would stick in browser caches long past the next phase's re-routing.
//!
//! Partial routes (`/dashboard/tiles`, `/dashboard/updates-tile`) get no
//! redirect: they are htmx poll targets, never bookmarked, and their only
//! callers are templates in this repo that moved with them.

use axum::response::Redirect;

/// `GET /dashboard` → `/` (the Overview page keeps `/` as its canonical path).
pub async fn dashboard() -> Redirect {
    Redirect::to("/")
}

/// `GET /processes` → `/system/processes`.
pub async fn processes() -> Redirect {
    Redirect::to("/system/processes")
}

/// `GET /logs` → `/system/logs`.
pub async fn logs() -> Redirect {
    Redirect::to("/system/logs")
}

/// `GET /settings` → `/shell/settings`.
pub async fn settings() -> Redirect {
    Redirect::to("/shell/settings")
}

/// `GET /widgets` → `/shell/widgets`.
pub async fn widgets() -> Redirect {
    Redirect::to("/shell/widgets")
}

/// `GET /media` → `/shell/media`.
pub async fn media() -> Redirect {
    Redirect::to("/shell/media")
}

/// `GET /tools` → `/remote/tools`.
pub async fn tools() -> Redirect {
    Redirect::to("/remote/tools")
}

/// `GET /controllers` → `/devices/controllers`.
pub async fn controllers() -> Redirect {
    Redirect::to("/devices/controllers")
}

/// `GET /cec` → `/devices/cec`.
pub async fn cec() -> Redirect {
    Redirect::to("/devices/cec")
}

/// `GET /dev` → `/dev/recovery`.
///
/// The one redirect whose old path is a *prefix* of live routes
/// (`/dev/restart-daemon`, `/dev/deploy`, …). Those are unaffected — axum
/// matches whole paths, not prefixes.
pub async fn dev() -> Redirect {
    Redirect::to("/dev/recovery")
}

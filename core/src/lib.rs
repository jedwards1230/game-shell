//! `tv-shell-core` — the v2 successor to `daemon/` (`tv-shell-input`).
//!
//! Built **beside** v1, not instead of it (V2_DESIGN §11): `daemon/`, `host/`,
//! `protocol/` and `panel/` are untouched and still build, v1 keeps booting on
//! the couch, and the two share no config file, socket or unit name.
//!
//! What this crate owns today:
//!
//! | Module | Owns |
//! |---|---|
//! | [`atoms`] | The typed X root-atom layer — gamescope's published state (§5) |
//! | [`screen`] | [`screen::ScreenState`], the one snapshot that replaces v1's `hypr-active`/`hypr-clients`/`hypr-monitors` |
//! | [`launch`] | Scoped launching: `systemd-run --user --scope`, and reading a scope back out of a cgroup path |
//! | [`baselayer`] | `show`/`home` as one write plus one bounded verify |
//! | [`config`] | `~/.config/tv-shell/core.toml` — a separate file from v1's |
//! | [`protocol`] | The IPC grammar, carried over from v1 unchanged in contract (§4) |
//! | [`ipc`] | The Unix-socket server |
//! | [`compositor`] | The seam between the two: verbs → X primitives |
//!
//! Explicitly **not** here yet, each a follow-up: uinput/input presenters, CEC,
//! the QML shell, panel changes, HTTP/MCP/MQTT/metrics, the forced-paint
//! heartbeat, and per-app Xwayland server creation.
//!
//! It is a lib plus a thin bin for the same reason the daemon is: `pub` items in
//! a library are public API and are never "dead", so `clippy -D warnings` stays
//! clean even where a module is not yet wired into `main`.

pub mod atoms;
pub mod baselayer;
pub mod compositor;
pub mod config;
pub mod ipc;
pub mod launch;
pub mod protocol;
pub mod screen;

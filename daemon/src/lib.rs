//! tv-shell-input library: every daemon module lives here as public API.
//!
//! The binary (`main.rs`) only wires these together. Exposing the modules as a
//! library's public surface keeps the cross-platform modules out of dead-code
//! analysis on non-Linux hosts — where the Linux-only `main` (and the evdev/
//! D-Bus/Hyprland modules) are `cfg`-excluded, so a bin-only crate would flag
//! the entire daemon as unused. As a lib, `pub` items are the public API and
//! are never "dead", so `cargo clippy -D warnings` is clean on macOS too.

pub mod apps;
pub mod config;
// Shared test-only scratch-path helper (sandboxed Stop-hook `temp_dir()` fix).
// Cross-platform; used by every module's `#[cfg(test)]` unit tests below.
#[cfg(test)]
pub(crate) mod testutil;
// Typed daemon configuration (~/.config/tv-shell/config.toml). Cross-platform
// (toml + serde); replaces the old daemon.env env-var loader. Carries the
// startup validate() that refuses an unauthenticated LAN control surface.
pub mod controllerdb;
pub mod daemon_config;
pub mod device;
pub mod health;
pub mod ipc;
// Moonlight local-config "forget" (creds-free unpair): line-based edits to
// Moonlight.conf. Cross-platform — pure file editing, no Linux-only imports.
pub mod moonlight;
// Stateless network reads (net-throughput + net-ping) for the QML shell.
// Cross-platform: pure parse helpers + a sysfs read (Linux) and a `ping`
// subprocess, served like `wol`/`sunshine-status` — not via the NM D-Bus actor.
pub mod netinfo;
pub mod notifications;
// Plex hubs fetch (On Deck + Recently Added) for the home-screen Plex widget.
// Cross-platform: stateless reqwest + JSON, like `health` (Sunshine).
pub mod plex;
pub mod protocol;
pub mod recents;
// Web-app registry + .desktop generation (#187 P1/P3). Cross-platform (pure
// path/string/JSON work), so it builds and unit-tests alongside `apps`.
pub mod webapps;
// Reusable client plumbing for a remote widget sidecar (base URL + bearer,
// reachability probe, typed/size-capped bearer HTTP helpers). The daemon is an
// HTTP client to a sidecar on another machine — NOT a process supervisor.
// Cross-platform; `steam` is the first consumer.
pub mod sidecar;
// Steam library proxy (steam-library + steam-launch) for the home-screen Steam
// widget. Cross-platform: stateless reqwest + JSON to the tv-shell-host
// sidecar (over HTTP, via `sidecar`), like `plex`.
pub mod steam;
// Generic remote-service health: shared probe + status vocabulary + background
// poller that broadcasts `health:<json>` events. Cross-platform (reqwest + tokio
// timer), like `plex`/`health`.
pub mod service_health;
// Shell-reported UI state cache (`shell-state` IPC in, `GET /status` out) plus
// its pure staleness predicate. Cross-platform: pure data + tokio::sync, and
// deliberately NOT routed through the Linux-only input runtime the way
// `shell-focus` is, so the HTTP bridge can read it.
pub mod shell_state;
// Passive CEC display-ownership cache (`<Active Source>` in, `GET /status`
// out) plus its pure tri-state predicate. Cross-platform: plain atomics +
// serde, deliberately NOT inside the Linux+`cec`-gated `cec` module, so the
// HTTP bridge can read it in every build (and honestly report "unknown" in a
// build that has no CEC at all).
pub mod display_owner;
pub mod state;
pub mod system;
// Observability metrics: app-specific counters + Prometheus/OpenMetrics text
// renderer shared by the `/metrics` HTTP route and the textfile writer.
// Cross-platform: no Linux-only imports (sys gauges degrade to zero on non-Linux).
pub mod metrics;
// Wake-on-LAN magic-packet sender for the home-screen "Wake host" card.
// Cross-platform: pure packet/parse helpers + a blocking UDP broadcast and
// `ip neigh` shell-out, like `steam`/`plex`.
pub mod wol;

// Session-environment self-discovery and daemon.env loading (#165).
// Cross-platform: no Linux-only imports.
pub mod session_env;

// Shared action logic used by both the HTTP bridge and the MCP server.
// Cross-platform: no Linux-only imports.
pub mod bridge_core;

// LAN HTTP control bridge — cross-platform (tokio only; no Linux-only imports).
pub mod http;

// MCP server (rmcp 1.7.0): opt-in via `--features mcp`. Linux-gated AND
// feature-gated so the default build (macOS dev boxes, CI default leg) never
// links rmcp or axum.
#[cfg(feature = "mcp")]
pub mod mcp;

// evdev/uinput input runtime — Linux kernel interfaces.
#[cfg(target_os = "linux")]
pub mod input;

// D-Bus backbone (Linux-only): BlueZ via `bluer`; NetworkManager/logind/UPower
// via `zbus`.
#[cfg(target_os = "linux")]
pub mod bluetooth;
#[cfg(target_os = "linux")]
pub mod network;
#[cfg(target_os = "linux")]
pub mod power;

// Hyprland IPC over its Unix sockets directly (no crate; Linux-only).
#[cfg(target_os = "linux")]
pub mod hyprland;

// HDMI-CEC actor (#94): persistent libcec connection via cec-rs. Linux-only AND
// feature-gated (`cec`) — libcec-sys links a C lib, so default builds exclude it
// to keep the no-system-C-deps invariant (evdev/zbus/bluer are pure Rust).
#[cfg(all(target_os = "linux", feature = "cec"))]
pub mod cec;

// File-watch actor: inotify-watches settings.json for external edits and
// broadcasts config:changed. Uses notify-debouncer-full (Linux-only crate).
#[cfg(target_os = "linux")]
pub mod watch;

// logind session-active watcher: releases the gamepad grab while our session is
// backgrounded (e.g. VT-switched away) so the foreground DE gets the controller.
// zbus + logind; Linux-only.
#[cfg(target_os = "linux")]
pub mod session;

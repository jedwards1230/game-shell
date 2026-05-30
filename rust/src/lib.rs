//! game-shell-input library: every daemon module lives here as public API.
//!
//! The binary (`main.rs`) only wires these together. Exposing the modules as a
//! library's public surface keeps the cross-platform modules out of dead-code
//! analysis on non-Linux hosts — where the Linux-only `main` (and the evdev/
//! D-Bus/Hyprland modules) are `cfg`-excluded, so a bin-only crate would flag
//! the entire daemon as unused. As a lib, `pub` items are the public API and
//! are never "dead", so `cargo clippy -D warnings` is clean on macOS too.

pub mod apps;
pub mod config;
pub mod device;
pub mod health;
pub mod ipc;
pub mod protocol;
pub mod recents;
pub mod state;

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

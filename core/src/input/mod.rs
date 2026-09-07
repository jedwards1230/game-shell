//! The pad fleet: discovery, exclusive grab, and the permanent per-player
//! uinput presenters (V2_DESIGN §7).
//!
//! # Scope
//!
//! This is the *foundation* of §7 and deliberately not all of it. What is here:
//! DB-match-or-reject discovery, stable per-player slots, hot join and leave,
//! `EVIOCGRAB`, presenters that exist for the life of the session, a 1:1
//! passthrough, and the read-only `input-state` verb. On its own it is
//! **behaviourally invisible**: the pad is grabbed and re-presented, and what
//! reads the presenter sees the same input it saw from the physical pad.
//!
//! What is NOT here, each a follow-up: routing to a shell and the
//! `gamepad`/`keyboard` contracts (there is no shell to route to yet), the Meta
//! hold and safety combos (`intent home` with no shell lands on an empty
//! compositor — a black television), rumble/battery/LED, and the companion
//! touchpad/motion-node inhibition §7 calls for (SteamOS's `ds-inhibit` shape).
//!
//! # Default off
//!
//! `[input].enabled` is `false` unless an operator sets it. With it off
//! [`start`] returns `None` before constructing anything, so there is no code
//! path from a disabled core to `/dev/input` or `/dev/uinput`. See
//! [`config::InputConfig`].
//!
//! # Module layout
//!
//! | Module | Owns | Testable in CI |
//! |---|---|---|
//! | [`identity`] | SDL GUID, controller DB, wire id, slot allocation | yes |
//! | [`discovery`] | The claim-or-refuse gate, and devnode ownership | yes |
//! | [`presenter`] | The canonical profile, rescaling, translation, quiesce | yes |
//! | [`fleet`] | Membership, slot stability, the join/leave plan | yes |
//! | [`session`] | The lifecycle: create once, claim, forward, retire | yes (recording double) |
//! | [`backend`] | The hardware seam | n/a (a trait) |
//! | `evdev_backend` | evdev/uinput syscalls | **no** — needs a seat |
//! | `runtime` | The poll/read loop and the thread it runs on | **no** — needs a backend |

pub mod backend;
pub mod config;
pub mod discovery;
pub mod fleet;
pub mod identity;
pub mod presenter;
pub mod session;

#[cfg(target_os = "linux")]
mod evdev_backend;
#[cfg(target_os = "linux")]
mod runtime;

pub use config::{InputConfig, ResolvedInput};
pub use session::InputReport;

use tokio::sync::watch;

/// A handle on the running input layer, held by the IPC surface.
///
/// Reads are snapshots off a `watch` channel rather than a request/reply round
/// trip. That is deliberate: `input-state` is a diagnostic an operator reaches
/// for when something is wrong, so it must answer even if the input loop is
/// wedged. A round trip would hang exactly when it was most needed; a snapshot
/// answers, and the report carries [`InputReport::last_poll_unix_ms`] so a
/// stopped loop is visible as a timestamp that stops advancing rather than as a
/// plausible-looking but stale answer.
///
/// Only `runtime` (Linux) ever constructs one, so on any other host every field
/// here is written by nothing — hence the `allow`. The type itself stays
/// unconditional so `start`'s signature does not change per platform, which is
/// what lets the pure modules and their tests build and run anywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct InputHandle {
    reports: watch::Receiver<InputReport>,
    /// Dropped on shutdown; the runtime's loop selects on its closure.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl InputHandle {
    /// A cloneable read-only view, for the IPC surface.
    pub fn reports(&self) -> InputReports {
        InputReports(Some(self.reports.clone()))
    }

    /// The most recent report.
    pub fn report(&self) -> InputReport {
        self.reports.borrow().clone()
    }

    /// Stop the input loop and wait for it to release every pad.
    ///
    /// Best-effort and bounded by the loop noticing: a core that is killed
    /// instead releases everything anyway, because a grab lives on a file
    /// descriptor the kernel closes (see `evdev_backend`).
    pub fn shutdown(&mut self) {
        drop(self.shutdown.take());
        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                tracing::warn!("the input runtime thread panicked on shutdown");
            }
        }
    }
}

/// The read side of the input layer, held by the IPC surface.
///
/// `None` inside means the layer is disabled or failed to start, and
/// [`InputReports::report`] then answers with [`InputReport::disabled`] — an
/// honest empty report rather than an error, because "input is off" is a state
/// the verb exists to report, not a failure to answer.
#[derive(Clone)]
pub struct InputReports(Option<watch::Receiver<InputReport>>);

impl InputReports {
    /// The view a core with no input layer hands to IPC.
    pub fn disabled() -> InputReports {
        InputReports(None)
    }

    /// The most recent report, or the disabled one.
    pub fn report(&self) -> InputReport {
        match &self.0 {
            Some(rx) => rx.borrow().clone(),
            None => InputReport::disabled(),
        }
    }
}

/// Start the input layer if — and only if — it is enabled.
///
/// Returns `None` when `[input].enabled` is false, **before** anything is
/// enumerated, opened or created. That is the whole safety contract: a core
/// running with the default config is byte-identical, at the device layer, to
/// one built without this module.
///
/// An input layer that fails to start is logged and returns `None` rather than
/// taking the core down: the compositor half — the base layer, launching, the
/// escape hatches — is what keeps a television showing something, and it must
/// not be hostage to `/dev/uinput` permissions.
#[cfg(target_os = "linux")]
pub fn start(config: &InputConfig) -> Option<InputHandle> {
    if !config.enabled {
        tracing::info!("[input] is disabled; no input device will be enumerated or opened");
        return None;
    }
    let resolved = match config.resolve() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("input layer not started: {e}");
            return None;
        }
    };
    match runtime::spawn(resolved) {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!("input layer not started: {e}");
            None
        }
    }
}

/// Non-Linux hosts have no evdev or uinput, so there is nothing to start.
///
/// The crate still compiles and its rules are still tested there — the point of
/// keeping every decision out of the backend.
#[cfg(not(target_os = "linux"))]
pub fn start(config: &InputConfig) -> Option<InputHandle> {
    if config.enabled {
        tracing::warn!("[input].enabled is set, but evdev and uinput are Linux-only");
    }
    None
}

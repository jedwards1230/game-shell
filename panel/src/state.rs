//! Shared application state threaded through every axum handler via
//! `State<SharedState>`.

use std::sync::Arc;

use crate::bridge::DevBridge;
use crate::config::AppConfig;
use crate::exec::Recovery;
use crate::transport::NodeTransport;
use crate::updates::UpdatesState;

/// The panel's shared state: resolved config plus the three data-tier
/// clients (node transport primary, HTTP bridge dev-ops, direct-exec
/// recovery) and the Updates feature's cache/job state.
pub struct AppState {
    pub cfg: AppConfig,
    /// The node this panel speaks for. Held as a trait object so the pages
    /// depend on *what* a node can do, not on the Unix socket that happens to
    /// serve the local one — see [`crate::transport`].
    pub node: Arc<dyn NodeTransport>,
    /// The daemon's opt-in HTTP dev-ops tier, held as a trait object for the
    /// same reason — see [`crate::bridge::DevBridge`].
    pub bridge: Arc<dyn DevBridge>,
    pub recovery: Recovery,
    pub updates: UpdatesState,
}

/// `Arc`-wrapped state, cloned cheaply into every handler.
pub type SharedState = Arc<AppState>;

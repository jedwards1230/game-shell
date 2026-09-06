//! `tv-shell-core` entry point.
//!
//! Startup order matters and is the §9 "core is stateless" contract:
//!
//! 1. Load and **validate** config before anything uses a value from it.
//! 2. Connect to X and intern the atoms — a name typo fails here, not at the
//!    first switch.
//! 3. **Reconcile**: read the base-layer list back as the last intent. The core
//!    never writes "home" on boot; that would yank a live game.
//! 4. Serve IPC.

use std::process::ExitCode;

use tv_shell_core::compositor::GamescopeCompositor;
use tv_shell_core::config::{self, CoreConfig};
use tv_shell_core::ipc;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let config = match CoreConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = config.validate() {
        tracing::error!("{e}");
        return ExitCode::FAILURE;
    }

    let compositor = match GamescopeCompositor::connect(config, None) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("connecting to the compositor: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    compositor.reconcile_on_start();

    let sock_path = config::socket_path();
    if let Err(e) = ipc::serve(sock_path, tv_shell_core::compositor::shared(compositor)).await {
        tracing::error!("ipc server: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Structured logs to stderr, filtered by `RUST_LOG` (default `info`).
///
/// journald capture comes from the unit, not from a journald layer here: the
/// core runs under `systemd --user` and stderr is already routed. Keeping the
/// binary journald-free means it also runs readably from a shell during
/// development.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

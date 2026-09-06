//! `tv-shell-core` entry point.
//!
//! Startup order matters and is the §9 "core is stateless" contract:
//!
//! 1. Load and **validate** config before anything uses a value from it.
//! 2. Connect to X and intern the atoms — a name typo fails here, not at the
//!    first switch.
//! 3. **Reconcile**: read the base-layer list back as the last intent. The core
//!    never writes "home" on boot; that would yank a live game.
//! 4. Serve IPC until a signal says to stop.
//! 5. **Then**, and only for a compositor that reconcile showed is FRESH, run
//!    the boot client (see [`boot`]). It is step 5 rather than step 3½ because
//!    the socket must exist before anything slow does — an operator debugging a
//!    session that will not come up needs the control surface more than the
//!    session needs its first app.
//!
//! There is also one non-serving mode, `write-session-env <path>`, which the
//! session script runs before starting the target. It is here rather than in a
//! shell helper because it is the one place `[display]` in `core.toml` becomes
//! the mode gamescope is actually started at — see [`CoreConfig::session_env`].

use std::process::ExitCode;
use std::sync::Arc;

use tv_shell_core::boot;
use tv_shell_core::compositor::GamescopeCompositor;
use tv_shell_core::config::{self, CoreConfig};
use tv_shell_core::ipc;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => serve().await,
        Some("write-session-env") => write_session_env(args.get(1).map(String::as_str)),
        Some(other) => {
            tracing::error!(
                "unknown argument {other:?}; usage: tv-shell-core [write-session-env <path>]"
            );
            ExitCode::FAILURE
        }
    }
}

/// Render `[display]`/`[session]` into the env file the gamescope unit reads.
///
/// Loud on every failure, because the unit's `EnvironmentFile=` has no leading
/// `-`: a mode this did not write is a compositor that does not start, and the
/// operator should learn the reason here (a named bad config key) rather than
/// there (a missing file).
fn write_session_env(path: Option<&str>) -> ExitCode {
    let Some(path) = path else {
        tracing::error!("usage: tv-shell-core write-session-env <path>");
        return ExitCode::FAILURE;
    };
    let config = match CoreConfig::load().and_then(|c| c.validate().map(|()| c)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    match std::fs::write(path, config.session_env()) {
        Ok(()) => {
            tracing::info!("wrote the session environment to {path}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("writing {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The serving mode: connect, reconcile, then run the IPC server until a signal.
async fn serve() -> ExitCode {
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

    // Read before `config` moves into the compositor.
    let boot_app = config.boot_app();
    let restart_policy = boot::RestartPolicy::from_config(&config);

    let compositor = match GamescopeCompositor::connect(config, None) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("connecting to the compositor: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    // The reconcile is the ONLY input to the boot decision: a fresh compositor
    // gets the boot app, a session someone is already using does not, and a
    // failed read gets nothing at all (§5/§9 — see `boot`). Decided here, while
    // the observation is fresh; acted on below, after the socket is up.
    let reconciled = compositor.reconcile_on_start();
    let boot_decision = boot::decide(
        boot_app,
        reconciled.as_ref().map(|r| boot::Observed {
            base_layer: &r.base_layer,
            on_screen: r.on_screen,
        }),
    );

    let sock_path = config::socket_path();
    let compositor = tv_shell_core::compositor::shared(compositor);
    let server = ipc::serve(sock_path.clone(), Arc::clone(&compositor));

    // AFTER the listener exists, and off the reactor. A cold app start can take
    // seconds — the map bound alone is 30 s — and §9 makes the control surface
    // the thing an operator reaches for when the session is wedged. Running the
    // boot client before `serve` would leave the socket absent for exactly as
    // long as the app took, which is precisely when someone would be trying to
    // reach it. The boot client drives the same `Compositor` the IPC layer does,
    // so its `show` takes the same `IntentGate` as an operator's rather than
    // racing one.
    let boot_compositor = Arc::clone(&compositor);
    tokio::spawn(async move {
        let run = move || {
            // `start` launches and shows; `supervise` then blocks for the life
            // of the app, relaunching it on a crash (§9 durability). A skip
            // decision returns None and nothing is supervised — which is how a
            // core restart under a live game costs exactly one reconcile.
            if let Some(supervised) = boot::start(&boot_compositor, boot_decision) {
                boot::supervise(
                    &boot_compositor,
                    supervised,
                    restart_policy,
                    std::thread::sleep,
                );
            }
        };
        if let Err(e) = tokio::task::spawn_blocking(run).await {
            tracing::error!("boot client task failed: {e}");
        }
    });

    // `ipc::serve` loops forever, so without this the `ExitCode::SUCCESS` below
    // was unreachable and a SIGTERM (which is how systemd stops the unit, every
    // single time) killed the process with the socket file still on disk. The
    // next start does remove a stale file, so nothing broke — but "nothing broke
    // because something downstream cleans up after us" is not a shutdown path,
    // and a core that cannot exit cleanly cannot later flush anything it owns.
    let outcome = tokio::select! {
        result = server => match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("ipc server: {e:#}");
                ExitCode::FAILURE
            }
        },
        signal = terminate() => {
            tracing::info!("{signal}; shutting down");
            ExitCode::SUCCESS
        }
    };

    // Unlink our own socket. Best-effort: the file may already be gone, and a
    // failure here must not turn a clean shutdown into a failed one.
    if let Err(e) = std::fs::remove_file(&sock_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("removing {sock_path}: {e}");
        }
    }
    outcome
}

/// Resolve when the process is asked to stop, naming which signal did it.
async fn terminate() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    // A failure to install a handler is not worth aborting a working core over,
    // so it degrades to "this signal will not be caught" — which is the
    // behaviour we had before, unchanged.
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot handle SIGTERM: {e}");
            return std::future::pending().await;
        }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot handle SIGINT: {e}");
            sigterm.recv().await;
            return "SIGTERM";
        }
    };
    tokio::select! {
        _ = sigterm.recv() => "SIGTERM",
        _ = sigint.recv() => "SIGINT",
    }
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

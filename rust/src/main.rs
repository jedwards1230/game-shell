//! game-shell input daemon.
//!
//! Grabs a gamepad exclusively via `EVIOCGRAB`, emits keyboard/mouse events via
//! uinput, and serves a newline-delimited IPC protocol on a Unix socket
//! (see `docs/IPC_PROTOCOL.md`). Formerly a drop-in replacement for the
//! now-deleted `gamepad-input.py`, keeping its wire protocol so the QML shell
//! is unchanged.
//!
//! Runtime topology: the IPC server and signal handling run on a multi-thread
//! tokio runtime; the input subsystem runs on its own OS thread with a
//! current-thread runtime, keeping real-time input timing off the IPC
//! scheduler. The two communicate over an `mpsc` control channel and a
//! `broadcast` event bus.

// Daemon modules live in the library crate (`game_shell_input`); this binary
// only wires them together. (lib+bin split — see lib.rs — so the cross-platform
// modules aren't dead-code on non-Linux hosts where `main` is cfg-excluded.)
#[cfg(target_os = "linux")]
use game_shell_input::{bluetooth, hyprland, input, ipc, network, power, protocol, state};

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use tokio::sync::{broadcast, mpsc};

    init_tracing();

    let uid = unsafe { libc::getuid() };
    let sock_path = std::env::var("GAME_SHELL_SOCK")
        .unwrap_or_else(|_| format!("/run/user/{uid}/game-shell-input.sock"));

    let (events_tx, _events_rx) = broadcast::channel::<protocol::Event>(256);
    let (control_tx, control_rx) = mpsc::channel::<state::Control>(64);

    // Input subsystem on a dedicated OS thread with its own current-thread
    // runtime (isolated timing).
    let input_events = events_tx.clone();
    let input_thread = std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build input runtime");
            rt.block_on(input::run(control_rx, input_events));
        })?;

    // Main runtime: IPC server + signal handling + Phase 3 D-Bus actors.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // Spawn the Phase 3 D-Bus actors on the IPC runtime. Each owns its own
        // connection and pushes events onto the shared broadcast bus. They log
        // and never panic the daemon if BlueZ/NetworkManager/logind/UPower are
        // absent, so spawning them unconditionally is safe.
        let dbus = spawn_dbus_actors(&events_tx);

        let ipc_task = tokio::spawn(ipc::serve(
            sock_path,
            control_tx.clone(),
            events_tx.clone(),
            dbus,
        ));
        wait_for_signal().await;
        tracing::info!("signal received, shutting down");
        let _ = control_tx.send(state::Control::Shutdown).await;
        ipc_task.abort();
    });

    // Let the input thread reset stick state and close uinput devices.
    let _ = input_thread.join();
    Ok(())
}

/// Build the Phase 3 D-Bus actor channels and spawn each actor on the current
/// (IPC) runtime. Returns the [`ipc::DbusSenders`] the IPC server uses to route
/// Bluetooth/Network/Power commands. Channels use the same size (64) as the
/// input control channel. Each actor logs and exits cleanly if its service is
/// absent; the IPC side degrades those commands to `error:*`.
#[cfg(target_os = "linux")]
fn spawn_dbus_actors(
    events_tx: &tokio::sync::broadcast::Sender<protocol::Event>,
) -> ipc::DbusSenders {
    use tokio::sync::mpsc;

    let (bt_tx, bt_rx) = mpsc::channel(64);
    let (net_tx, net_rx) = mpsc::channel(64);
    let (power_tx, power_rx) = mpsc::channel(64);
    let (hypr_tx, hypr_rx) = mpsc::channel(64);

    {
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = bluetooth::run(bt_rx, events_tx).await {
                tracing::warn!("bluetooth actor exited: {e}");
            }
        });
    }
    {
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = network::run(net_rx, events_tx).await {
                tracing::warn!("network actor exited: {e}");
            }
        });
    }
    {
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = power::run(power_rx, events_tx).await {
                tracing::warn!("power actor exited: {e}");
            }
        });
    }
    {
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = hyprland::run(hypr_rx, events_tx).await {
                tracing::warn!("hyprland actor exited: {e}");
            }
        });
    }

    ipc::DbusSenders {
        bt: Some(bt_tx),
        net: Some(net_tx),
        power: Some(power_tx),
        hypr: Some(hypr_tx),
    }
}

#[cfg(target_os = "linux")]
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}

#[cfg(target_os = "linux")]
async fn wait_for_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut intr = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = intr.recv() => {}
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("game-shell-input only runs on Linux (requires evdev/uinput).");
    std::process::exit(1);
}

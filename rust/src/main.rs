//! game-shell input daemon.
//!
//! Grabs a gamepad exclusively via `EVIOCGRAB`, emits keyboard/mouse events via
//! uinput, and serves a newline-delimited IPC protocol on a Unix socket
//! (see `docs/IPC_PROTOCOL.md`). A drop-in replacement for `gamepad-input.py`
//! with an identical wire protocol, so the QML shell is unchanged.
//!
//! Runtime topology: the IPC server and signal handling run on a multi-thread
//! tokio runtime; the input subsystem runs on its own OS thread with a
//! current-thread runtime, keeping real-time input timing off the IPC
//! scheduler. The two communicate over an `mpsc` control channel and a
//! `broadcast` event bus.

mod config;
mod device;
mod ipc;
mod protocol;
mod state;

#[cfg(target_os = "linux")]
mod input;

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

    // Main runtime: IPC server + signal handling.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let ipc_task = tokio::spawn(ipc::serve(sock_path, control_tx.clone(), events_tx.clone()));
        wait_for_signal().await;
        tracing::info!("signal received, shutting down");
        let _ = control_tx.send(state::Control::Shutdown).await;
        ipc_task.abort();
    });

    // Let the input thread reset stick state and close uinput devices.
    let _ = input_thread.join();
    Ok(())
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

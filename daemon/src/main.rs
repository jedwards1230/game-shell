//! tv-shell input daemon.
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

// Daemon modules live in the library crate (`tv_shell_input`); this binary
// only wires them together. (lib+bin split — see lib.rs — so the cross-platform
// modules aren't dead-code on non-Linux hosts where `main` is cfg-excluded.)
#[cfg(target_os = "linux")]
use tv_shell_input::{
    bluetooth, http, hyprland, input, ipc, network, power, protocol, session, session_env, state,
    watch, wol,
};

#[cfg(all(target_os = "linux", feature = "mcp"))]
use tv_shell_input::mcp;

#[cfg(target_os = "linux")]
use tv_shell_input::ipc::{ControllerDbState, SharedControllerDbState};

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::sync::Arc;
    // NB: `tokio::sync::watch` is referenced fully-qualified below, not imported —
    // a bare `watch` here would shadow this crate's own `watch` (file-watch)
    // module imported at the top of the file (`watch::run`).
    use tokio::sync::{broadcast, mpsc, Notify};

    // Load the typed config (~/.config/tv-shell/config.toml) FIRST — before
    // tracing init, so the logging backend can be driven by
    // [observability].log_journal. A missing file is fine (all-default ⇒ no
    // control surface); a malformed file or an unsafe combination (LAN bind +
    // dev tools + no auth, without [dev].allow_insecure_lan) is a hard startup
    // failure (the `?` surfaces it to stderr even before tracing is up). Validate
    // BEFORE installing the global / opening any socket.
    let daemon_cfg = tv_shell_input::daemon_config::DaemonConfig::load()?;
    daemon_cfg.validate()?;

    init_tracing(daemon_cfg.observability.log_journal);

    // One-time, per-file backfill of real user state from a legacy
    // ~/.config/game-shell into ~/.config/tv-shell (#rename bug): when Ansible
    // pre-creates the new dir for deployment files, config_dir()'s dir-level
    // legacy fallback stops firing, so settings.json / host-macs.json would be
    // silently re-defaulted. Idempotent + never overwrites; run BEFORE any socket
    // is opened or settings are read. Deployment-owned files (config.toml/
    // targets.json/tokens) and data-dir files are deliberately NOT migrated.
    for migrated in
        tv_shell_protocol::brand::migrate_legacy_config_files(&["settings.json", "host-macs.json"])
    {
        tracing::info!("migrated legacy state file into the tv-shell config dir: {migrated}");
    }

    tv_shell_input::daemon_config::init_global(daemon_cfg.clone());

    let uid = unsafe { libc::getuid() };
    // TV_SHELL_SOCK (legacy GAME_SHELL_SOCK) override; default socket basename
    // comes from brand::socket_name() so the daemon and QML shell agree on it.
    let sock_path = tv_shell_protocol::brand::env("SOCK").unwrap_or_else(|| {
        format!(
            "/run/user/{uid}/{}",
            tv_shell_protocol::brand::socket_name()
        )
    });

    let (events_tx, _events_rx) = broadcast::channel::<protocol::Event>(256);
    let (control_tx, control_rx) = mpsc::channel::<state::Control>(64);

    // Coalescing (latest-wins) focused-window class channel: the Hyprland actor
    // publishes each `activewindow` change here and the input runtime follows
    // compositor focus off it. A `watch` (not the control mpsc) because focus is
    // STATE — only the newest value matters, so it can never back up or drop on a
    // busy input loop. The initial "" (no toplevel focused) never fires
    // `.changed()` (watch only signals values sent AFTER construction).
    let (active_window_tx, active_window_rx) = tokio::sync::watch::channel::<String>(String::new());

    // Shell screen-ownership, published by the input runtime (`shell-focus
    // on|off`) and consumed by the Hyprland actor's kiosk fullscreen backstop.
    // A `watch` for the same reason as above: ownership is STATE, and only the
    // newest value matters. Starts `true` — the shell boots owning the screen,
    // matching the input runtime's initial `Presenter::Shell`; that also makes a
    // daemon restart mid-session safe, since the shell's heartbeat re-asserts
    // the truth within one tick.
    let (shell_focus_tx, shell_focus_rx) = tokio::sync::watch::channel::<bool>(true);

    // Observability counters, shared between the input runtime (which records
    // intents/transitions/pad-join-leave/input-events) and the metrics exporter
    // (textfile writer + `/metrics` HTTP route). Count this start as a restart:
    // the daemon re-execs on /dev/restart-daemon and is otherwise supervised, so
    // the running total is the shell-input restart count for this boot session.
    let metrics = tv_shell_input::metrics::Metrics::new();
    metrics.inc_shell_restarts();

    // Dedicated channel for the file-watch actor to signal the input runtime
    // of external settings.json changes. A separate Notify (rather than a
    // broadcast receiver) avoids adding a permanent receiver on the global
    // Event bus that would defeat receiver_count()==0 fast-paths (#163).
    let config_changed = Arc::new(Notify::new());

    // Input subsystem on a dedicated OS thread with its own current-thread
    // runtime (isolated timing). `run_supervised` owns `control_rx` across
    // restarts and respawns the input event loop on a panic (a fresh `Fleet` per
    // attempt → dropped fds → released grabs), so a single panic no longer leaves
    // the controller stuck with only a whole-daemon restart to recover it.
    let input_events = events_tx.clone();
    let input_config_changed = Arc::clone(&config_changed);
    let input_metrics = Arc::clone(&metrics);
    let input_thread = std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build input runtime");
            rt.block_on(input::run_supervised(
                control_rx,
                input_events,
                input_config_changed,
                input_metrics,
                active_window_rx,
                shell_focus_tx,
            ));
        })?;

    // Unified shutdown signal: a single CancellationToken that is cancelled
    // on SIGTERM/SIGINT OR when a re-exec is requested (HTTP /dev/restart-daemon
    // or MCP dev_restart_daemon). Using a single multi-consumer token avoids the
    // race where Notify::notify_one() wakes only ONE waiter — if both the main
    // select! and an MCP cancel task are both waiting on the same Notify, only
    // one of them wakes, meaning the other never proceeds (#mcp-bridge review).
    //
    // The AtomicBool carries the re-exec intent across the runtime boundary:
    // it is set to true by request_reexec() before cancel() is called, then
    // read after input_thread.join() (after the runtime has fully shut down)
    // to decide whether to exec() the new binary.
    let shutdown = tokio_util::sync::CancellationToken::new();
    let reexec_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Keep a handle outside the runtime closure for the post-join re-exec check.
    let reexec_flag_check = reexec_flag.clone();

    // Main runtime: IPC server + signal handling + Phase 3 D-Bus actors.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // Spawn the Phase 3 D-Bus actors on the IPC runtime. Each owns its own
        // connection and pushes events onto the shared broadcast bus. They log
        // and never panic the daemon if BlueZ/NetworkManager/logind/UPower are
        // absent, so spawning them unconditionally is safe.
        let dbus = spawn_dbus_actors(&events_tx, &control_tx, &active_window_tx);

        // Spawn the file-watch actor. It inotify-watches settings.json for
        // external edits and signals the input runtime via config_changed.
        // Fire-and-forget like the D-Bus actors — it logs and degrades
        // gracefully if inotify fails.
        {
            let watch_config_changed = Arc::clone(&config_changed);
            tokio::spawn(async move {
                watch::run(watch_config_changed).await;
            });
        }

        // Metrics textfile-collector writer (observability): periodically renders
        // the Prometheus/OpenMetrics exposition and writes it atomically to
        // [observability].metrics_textfile. Disabled (no file) when that key is
        // unset. Fire-and-forget like the actors above — logs and degrades
        // gracefully, never panics. The `/metrics` HTTP route is unaffected.
        {
            let writer_metrics = Arc::clone(&metrics);
            let textfile = daemon_cfg.observability.metrics_textfile.clone();
            let interval = daemon_cfg.metrics_interval_secs();
            tokio::spawn(async move {
                tv_shell_input::metrics::run_textfile_writer(writer_metrics, textfile, interval)
                    .await;
            });
        }

        // logind session watcher: releases the gamepad grab while our session is
        // backgrounded (VT-switched away) and re-grabs on return. Fire-and-forget
        // like the D-Bus actors — logs and degrades gracefully if logind is
        // absent (grab simply stays held, the pre-feature behaviour).
        {
            let session_control_tx = control_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = session::run(session_control_tx).await {
                    tracing::warn!("logind session actor exited: {e}");
                }
            });
        }

        // Remote-service health poller: probes always-on services (Plex) on a
        // timer and broadcasts `health:<json>` events on the global bus so the
        // shell's widgets can render a graceful "server unavailable" state.
        // Fire-and-forget like the actors above — never panics, emits nothing
        // for unconfigured services.
        {
            let health_events = events_tx.clone();
            tokio::spawn(async move {
                tv_shell_input::service_health::run(health_events).await;
            });
        }

        // Proactive Wake-on-LAN for the ACTIVE Steam host, gated on
        // [steam].wake_active_host_on_start (default OFF — waking someone else's
        // machine is opt-in). Fire-and-forget like the actors above: it resolves
        // to a log line whether it wakes the host, finds nothing configured, or
        // can't resolve a MAC, and can never delay or fail startup.
        tokio::spawn(wol::wake_active_host_if_enabled("startup"));

        // Initialize the controller DB state once at startup. The IPC server
        // shares it across connections (Arc<RwLock<_>>).
        let db_state: SharedControllerDbState =
            std::sync::Arc::new(tokio::sync::RwLock::new(ControllerDbState::initial()));

        let ipc_task = tokio::spawn(ipc::serve(
            sock_path,
            control_tx.clone(),
            events_tx.clone(),
            dbus,
            db_state,
        ));

        // LAN HTTP control bridge (#151): opt-in via [http].bind in config.toml.
        // Absent ⇒ no socket is opened and no control surface is exposed. The
        // address + the dangerous-combo refusal were already parsed/validated
        // above (DaemonConfig::validate), so http_bind() here is infallible-by-
        // construction; we still match defensively. The metrics handle backs the
        // bridge's auth-exempt GET /metrics route (#268).
        match daemon_cfg.http_bind() {
            Ok(Some(addr)) => {
                // validate() already resolved + vetted the token file at startup
                // (aborting on a bad path / perms), so this is Ok here.
                let token = daemon_cfg.http_token().unwrap_or(None);
                let auth_enabled = daemon_cfg.http.auth_enabled;
                tokio::spawn(http::serve(
                    addr,
                    token,
                    auth_enabled,
                    control_tx.clone(),
                    events_tx.clone(),
                    shutdown.clone(),
                    reexec_flag.clone(),
                    Arc::clone(&metrics),
                ));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("{e}"),
        }

        // MCP server (#mcp-bridge): opt-in via [mcp].bind in config.toml.
        // Absent ⇒ no socket is opened. The dangerous-combo refusal already ran
        // in DaemonConfig::validate above.
        // Pass a child token so the MCP server stops cleanly whenever the
        // shared shutdown token is cancelled (signal or re-exec). The single
        // CancellationToken design (Fix 1) eliminates the previous race where
        // Notify::notify_one() could wake the MCP cancel task but NOT the main
        // select!, leaving the daemon stuck with no re-exec.
        #[cfg(feature = "mcp")]
        {
            match daemon_cfg.mcp_bind() {
                Ok(Some(addr)) => {
                    // Ok by construction — validate() vetted the token file at startup.
                    let token = daemon_cfg.http_token().unwrap_or(None);
                    let auth_enabled = daemon_cfg.http.auth_enabled;
                    tokio::spawn(mcp::serve(
                        addr,
                        token,
                        auth_enabled,
                        daemon_cfg.mcp.dev,
                        daemon_cfg.mcp.allowed_hosts.clone(),
                        control_tx.clone(),
                        events_tx.clone(),
                        shutdown.clone(),
                        reexec_flag.clone(),
                        Arc::clone(&metrics),
                    ));
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("{e}"),
            }
        }

        tokio::select! {
            _ = wait_for_signal() => {
                tracing::info!("signal received, shutting down");
            }
            _ = shutdown.cancelled() => {
                tracing::info!("shutdown requested, stopping");
            }
        }

        let _ = control_tx.send(state::Control::Shutdown).await;
        ipc_task.abort();
    });

    // Let the input thread reset stick state and close uinput devices.
    // Gamepad grabs are released in input::run's shutdown path before this
    // returns, so the re-exec'd daemon can re-grab cleanly.
    let _ = input_thread.join();

    // Re-exec if the HTTP /dev/restart-daemon endpoint requested it.
    // Performed AFTER input_thread.join() so pads are released before the
    // new process image starts grabbing them again.
    if reexec_flag_check.load(std::sync::atomic::Ordering::Acquire) {
        use std::os::unix::process::CommandExt;
        // Re-exec the canonical install-path binary, NOT current_exe(): a fresh
        // `/dev/build` replaces the binary inode, so /proc/self/exe resolves to
        // "…/tv-shell-input (deleted)" and exec()ing that path fails ENOENT.
        // The install path always points at the just-built binary.
        let exe = session_env::input_bin();
        let err = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .exec();
        // exec() only returns on error.
        return Err(anyhow::anyhow!("re-exec failed ({}): {err}", exe.display()));
    }

    Ok(())
}

/// Build the Phase 3 D-Bus actor channels and spawn each actor on the current
/// (IPC) runtime. Returns the [`ipc::DbusSenders`] the IPC server uses to route
/// Bluetooth/Network/Power commands. Channels use the same size (64) as the
/// input control channel. Each actor logs and exits cleanly if its service is
/// absent; the IPC side degrades those commands to `error:*`.
///
/// `control_tx` is a clone of the input runtime's control channel. It is handed
/// to the CEC actor (under `--features cec`) so CEC remote keypresses can be
/// injected as `Control::Key` nav events — gated by `TV_SHELL_CEC_LIFECYCLE`.
///
/// `active_window_tx` is the sender half of the coalescing focused-window watch
/// channel, handed to the Hyprland actor so `activewindow` changes drive the
/// input runtime's follow-focus presenter (latest-wins, never dropped).
#[cfg(target_os = "linux")]
fn spawn_dbus_actors(
    events_tx: &tokio::sync::broadcast::Sender<protocol::Event>,
    // Only consumed by the CEC actor (gated `--features cec`); unused in the
    // default C-free build now that the Hyprland actor rides the watch channel.
    #[cfg_attr(not(feature = "cec"), allow(unused_variables))]
    control_tx: &tokio::sync::mpsc::Sender<state::Control>,
    active_window_tx: &tokio::sync::watch::Sender<String>,
) -> ipc::DbusSenders {
    use tokio::sync::mpsc;

    let (bt_tx, bt_rx) = mpsc::channel(64);
    let (net_tx, net_rx) = mpsc::channel(64);
    let (power_tx, power_rx) = mpsc::channel(64);
    let (hypr_tx, hypr_rx) = mpsc::channel(64);
    // HDMI-CEC actor (#94): only spawned when the daemon is built `--features
    // cec`. The module/channel/spawn are all gated so the default build links no
    // libcec.
    #[cfg(feature = "cec")]
    let (cec_tx, cec_rx) = mpsc::channel(64);
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
        // Hand the power actor a clone of the CEC channel so logind
        // PrepareForSleep can drive the CEC lifecycle (standby on suspend, wake
        // on resume). Only present under `--features cec`; the CEC actor no-ops
        // these unless TV_SHELL_CEC_LIFECYCLE is enabled.
        #[cfg(feature = "cec")]
        let power_cec_tx = Some(cec_tx.clone());
        tokio::spawn(async move {
            #[cfg(feature = "cec")]
            let result = power::run(power_rx, events_tx, power_cec_tx).await;
            #[cfg(not(feature = "cec"))]
            let result = power::run(power_rx, events_tx).await;
            if let Err(e) = result {
                tracing::warn!("power actor exited: {e}");
            }
        });
    }
    {
        let events_tx = events_tx.clone();
        // Sender of the coalescing focused-window watch channel so the Hyprland
        // actor can publish `activewindow` focus changes (latest-wins) for the
        // input runtime's follow-focus presenter (see hyprland.rs::run doc comment).
        let hypr_active_window_tx = active_window_tx.clone();
        let hypr_shell_focus_rx = shell_focus_rx.clone();
        tokio::spawn(async move {
            if let Err(e) = hyprland::run(
                hypr_rx,
                events_tx,
                hypr_active_window_tx,
                hypr_shell_focus_rx,
            )
            .await
            {
                tracing::warn!("hyprland actor exited: {e}");
            }
        });
    }
    #[cfg(feature = "cec")]
    {
        let events_tx = events_tx.clone();
        // Clone of the input control channel so the CEC actor can inject remote
        // keypresses as nav `Control::Key` events (gated by the lifecycle flag).
        let cec_control_tx = control_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = tv_shell_input::cec::run(cec_rx, events_tx, cec_control_tx).await {
                tracing::warn!("cec actor exited: {e}");
            }
        });
    }

    ipc::DbusSenders {
        bt: Some(bt_tx),
        net: Some(net_tx),
        power: Some(power_tx),
        hypr: Some(hypr_tx),
        #[cfg(feature = "cec")]
        cec: Some(cec_tx),
    }
}

/// Decide whether to log to the systemd journal.
///
/// `[observability].log_journal` is the escape hatch: `Some(true)` forces
/// journald on, `Some(false)` forces it off (stdout). When `None` (the default),
/// auto-detect: a systemd-spawned service has `JOURNAL_STREAM` set, which means
/// our stdout is already wired to the journal — but the `tracing-journald` layer
/// adds structured fields + syslog priority mapping that plain stdout loses, so
/// we prefer it when the journal socket is reachable.
#[cfg(target_os = "linux")]
fn want_journal(log_journal: Option<bool>) -> bool {
    match log_journal {
        Some(v) => v,
        // None → auto: the presence of JOURNAL_STREAM means we were launched
        // under journald's control (systemd unit / `systemd-run`).
        None => std::env::var_os("JOURNAL_STREAM").is_some(),
    }
}

/// Initialise tracing.
///
/// **Linux**: log to the systemd journal via `tracing-journald` when a journal
/// is available (structured fields + syslog priority mapping), otherwise fall
/// back to the plain stdout `fmt` layer. `[observability].log_journal` forces
/// the choice. The `RUST_LOG`/`EnvFilter` behaviour (default `info`) is
/// identical on both paths.
#[cfg(target_os = "linux")]
fn init_tracing(log_journal: Option<bool>) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    // The EnvFilter is constructed per branch (it isn't `Clone`) so both paths
    // honour RUST_LOG identically with a default of `info`.
    let new_filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Try journald when requested/auto-detected; fall back to stdout on any
    // failure (no journal socket) so the daemon is never left without logging.
    if want_journal(log_journal) {
        match tracing_journald::layer() {
            Ok(journald) => {
                tracing_subscriber::registry()
                    .with(new_filter())
                    .with(journald)
                    .init();
                return;
            }
            Err(e) => {
                eprintln!("tv-shell-input: journald unavailable ({e}), logging to stdout");
            }
        }
    }

    // Stdout layer (journal disabled, unavailable, or forced off). journald adds
    // its own timestamps + priority, so the stdout path keeps the original
    // compact format (no target, no time — the journal/console adds those).
    fmt()
        .with_env_filter(new_filter())
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
    eprintln!("tv-shell-input only runs on Linux (requires evdev/uinput).");
    std::process::exit(1);
}

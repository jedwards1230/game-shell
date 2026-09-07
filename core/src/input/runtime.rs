//! The loop that drives [`InputSession`]: a discovery tick and a pad read,
//! multiplexed, on a thread of their own.
//!
//! # Why its own thread
//!
//! The same reason v1's input runtime has one. Pad reads are latency-sensitive
//! and constant; the core's other work is X round trips that block for
//! milliseconds at a time. Sharing a scheduler makes input jitter a function of
//! how busy the compositor half is. A dedicated current-thread runtime keeps
//! them independent, and keeps every mutation of the session single-owner — no
//! lock held across an `await`.
//!
//! # Why a poll and not only a listener
//!
//! V2_DESIGN §10, from v1's residual defect: an attached event listener that
//! processed nothing, visible only as a widening gap between events seen and
//! windows seen. Membership here is recomputed from a full enumeration on a
//! timer, so the failure mode "the notifier stopped and nothing noticed" does
//! not exist. The stream read is an optimisation on top — it retires a yanked
//! pad in milliseconds instead of at the next tick — never the sole sensor.
//!
//! # What a panic on this thread does
//!
//! It takes the input layer down and nothing else, and it **releases every
//! grab**. The loop below reads one event, then acts on it; a panic between
//! those two points leaves that event unprocessed and the session mid-state. But
//! the panic unwinds this thread only — the core's compositor half, its IPC
//! socket and the escape hatches all keep running on other threads — and
//! unwinding drops `session`, which drops the backend, which closes every pad's
//! descriptor. `EVIOCGRAB` lives on the descriptor, so the controllers are
//! handed back on the way out; the uinput presenters disappear with it.
//!
//! So the failure mode is "input stops working until the core is restarted",
//! never "input is wedged and the pad is still held hostage". That is the shape
//! that matters here: the couch can always fall back to the physical pad.
//!
//! There is deliberately **no `catch_unwind`** and no supervisor restarting this
//! thread. A loop that panicked once will panic again on the next event of the
//! same shape, and a self-restarting one would grab and release the fleet in a
//! tight cycle — which every game sees as a controller reconnect storm, strictly
//! worse than the pad simply reverting to direct use. `Upholds=` on the unit
//! restarts the whole core instead, which is a clean start rather than a partial
//! one.

use tokio::sync::{oneshot, watch};

use super::config::ResolvedInput;
use super::evdev_backend::EvdevBackend;
use super::session::{InputReport, InputSession};
use super::InputHandle;

/// Start the input runtime on its own thread.
///
/// Blocks until the session has been constructed — that is, until every
/// presenter exists — so a caller that gets an `Ok` back knows the fleet has
/// somewhere to present to. Everything after that is asynchronous.
pub fn spawn(resolved: ResolvedInput) -> anyhow::Result<InputHandle> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (report_tx, report_rx) = watch::channel(InputReport::disabled());
    // Carries the outcome of constructing the session, so a failure to create
    // the presenters is reported to the caller rather than logged on a thread
    // nobody is watching.
    let (started_tx, started_rx) = std::sync::mpsc::channel();

    let join = std::thread::Builder::new()
        .name("tv-shell-input".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = started_tx.send(Err(format!("building the input runtime: {e}")));
                    return;
                }
            };
            rt.block_on(run(resolved, started_tx, report_tx, shutdown_rx));
        })
        .map_err(|e| anyhow::anyhow!("spawning the input thread: {e}"))?;

    match started_rx.recv() {
        Ok(Ok(())) => Ok(InputHandle {
            reports: report_rx,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(anyhow::anyhow!(e))
        }
        // The thread died without reporting: it panicked before `run`.
        Err(_) => {
            let _ = join.join();
            Err(anyhow::anyhow!(
                "the input thread stopped before it started"
            ))
        }
    }
}

async fn run(
    resolved: ResolvedInput,
    started: std::sync::mpsc::Sender<Result<(), String>>,
    reports: watch::Sender<InputReport>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut session = match InputSession::start(EvdevBackend::new(), &resolved) {
        Ok(s) => {
            let _ = started.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = started.send(Err(e.to_string()));
            return;
        }
    };
    let _ = reports.send(session.report());

    let mut tick = tokio::time::interval(resolved.poll_interval);
    // A tick missed because a burst of input kept us busy should be skipped, not
    // replayed: catching up would run several full enumerations back to back for
    // no gain.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // The backend is borrowed only for the duration of this expression, so
        // the arms below are free to use `session` mutably afterwards.
        let event = tokio::select! {
            _ = tick.tick() => None,
            next = session.backend_mut().next_event() => Some(next),
            _ = &mut shutdown => break,
        };

        match event {
            None => {
                for joined in session.poll() {
                    tracing::debug!(slot = joined.slot, path = %joined.path.display(), "joined");
                }
                let _ = reports.send(session.report());
            }
            Some((path, Ok(ev))) => {
                session.forward(&path, ev.event_type().0, ev.code(), ev.value());
            }
            Some((path, Err(e))) => {
                // The usual cause is the pad being unplugged. Retire it now
                // rather than waiting for the next enumeration to miss it.
                tracing::info!("pad at {} stopped reading: {e}", path.display());
                session.on_stream_error(&path);
                let _ = reports.send(session.report());
            }
        }
    }

    tracing::info!("input runtime stopping; releasing every pad");
    session.shutdown();
    let _ = reports.send(session.report());
}

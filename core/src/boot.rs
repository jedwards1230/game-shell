//! The boot client — starting the session's first app, and the rule that stops
//! it stealing a live one.
//!
//! # The problem this module is careful about
//!
//! §5 and §9 say the core **never writes the base layer at startup**: a core
//! restart under a running game would yank the screen away from it. §9 also
//! wants `Restart=always` on the core unit, so a restart under a live session is
//! not a rare event — it is the designed recovery path, and it is the case where
//! getting this wrong is most expensive (the user is *playing something*).
//!
//! But a freshly booted session has to start *something*, or the television
//! comes up black. Those two requirements look contradictory and are not,
//! because they are about different worlds:
//!
//! | | base layer | on screen | what it is |
//! |---|---|---|---|
//! | fresh session | empty (atom absent) | nothing | nobody has used this compositor yet |
//! | core restart under a game | populated | the game | someone is using it right now |
//! | user quit back to the shell | populated (shell id) | shell | someone used it and stopped |
//! | X read failed | unknown | unknown | we do not know, so we must not act |
//!
//! **A boot launch is not "the core started"; it is "this compositor has never
//! been used".** That is an observation, and [`decide`] makes it from the same
//! reconcile the core already performs — no flag, no marker file, no "first
//! run" state on disk. The core stays stateless (§9), which is what makes this
//! safe under a restart the operator did not plan.
//!
//! Two consequences worth stating because they are deliberate, not oversights:
//!
//! * **A core restart never relaunches the boot app.** Even if the user quit the
//!   app and is sitting on the shell, the base layer holds the shell id, so the
//!   session reads as in-use and the core leaves it alone. Resurrecting an app
//!   somebody closed is worse than doing nothing.
//! * **An unreadable X state does not launch.** A failed read is not evidence of
//!   an empty session; it is no evidence at all. Fail closed — the [`decide`]
//!   caller passes `None` and gets [`BootDecision::SessionUnreadable`].
//!
//! # Ordering
//!
//! `main` runs this AFTER the IPC socket is listening. A cold app start can take
//! seconds (the map bound is 30 s by default), and §9 makes the control surface
//! the thing an operator reaches for when the session is wedged — so the socket
//! must not be waiting on an app. Everything the boot sequence does goes through
//! the same [`Compositor`] the IPC layer uses, so its `show` takes the same
//! `IntentGate` as an operator's: a boot in flight and a concurrent `show`
//! serialize instead of racing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::atoms::AppId;
use crate::config::{CoreConfig, RelaunchPolicy};
use crate::ipc::Compositor;

/// What the boot client decided to do, and why.
///
/// The skip reasons are distinct variants rather than one `Skip` because they
/// are the log line an operator reads when the television is black and they want
/// to know whether the core chose not to act or failed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootDecision {
    /// `boot_app = 0`: nothing configured, nothing to do.
    NotConfigured,
    /// A fresh compositor. Launch and show this class.
    Launch(AppId),
    /// The base layer already names something — someone is using this session.
    BaseLayerInUse,
    /// A window is already on screen.
    AppOnScreen(AppId),
    /// The reconcile could not read the session. No evidence, so no action.
    SessionUnreadable,
}

impl BootDecision {
    /// The sentence logged for this decision.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotConfigured => "no boot_app configured",
            Self::Launch(_) => "the compositor is fresh (empty base layer, nothing on screen)",
            Self::BaseLayerInUse => {
                "the base layer already names an app, so this session is in use — \
                 a boot launch here would take the screen from it"
            }
            Self::AppOnScreen(_) => {
                "an app is already on screen, so this session is in use — \
                 a boot launch here would take the screen from it"
            }
            Self::SessionUnreadable => {
                "the session state could not be read; a failed read is not evidence of an \
                 empty session, so the core does not act on it"
            }
        }
    }
}

/// Decide whether to run the boot client. **Pure, and the whole safety rule.**
///
/// `observed` is `None` when the reconcile failed — see the module docs for why
/// that is a refusal rather than a "probably fine".
///
/// Both halves of the emptiness test are required and neither is redundant:
/// the base layer is what the core and Steam write, while `on_screen` is what
/// the compositor actually resolved — §5's whole point is that those can
/// disagree, and either one being non-empty means somebody is using the session.
pub fn decide(boot_app: Option<AppId>, observed: Option<Observed<'_>>) -> BootDecision {
    let Some(boot_app) = boot_app else {
        return BootDecision::NotConfigured;
    };
    let Some(observed) = observed else {
        return BootDecision::SessionUnreadable;
    };
    if let Some(on_screen) = observed.on_screen {
        return BootDecision::AppOnScreen(on_screen);
    }
    if !observed.base_layer.is_empty() {
        return BootDecision::BaseLayerInUse;
    }
    BootDecision::Launch(boot_app)
}

/// What the reconcile saw, as the two facts [`decide`] needs.
#[derive(Debug, Clone, Copy)]
pub struct Observed<'a> {
    /// `GAMESCOPECTRL_BASELAYER_APPID` as it currently reads.
    pub base_layer: &'a [AppId],
    /// The app id of the base window, if one resolved.
    pub on_screen: Option<AppId>,
}

/// Proof that THIS core launched the app it is about to supervise.
///
/// **This type is the structural half of the rule the whole module is about.**
/// It is constructed in exactly one place — [`start`], after a confirmed launch
/// — and [`supervise`] cannot be called without one. So "the app exited" is a
/// message on a channel we received *by launching the process*, and "the core
/// restarted" is a fresh [`decide`] against the running world. There is no code
/// path that turns an observation of the world into a relaunch, which is the
/// conflation that would let a restart stomp a running game.
pub struct Supervised {
    app_id: AppId,
    exits: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    launched_at: Instant,
}

impl Supervised {
    /// The app being supervised.
    pub fn app_id(&self) -> AppId {
        self.app_id
    }
}

/// Launch the boot app and put it on screen, returning the supervision handle.
///
/// Every failure is logged and returns `None` — a boot client that could not
/// start is not a reason to take the core down, because the core is the thing an
/// operator uses to find out why. The replies are the IPC reply strings, which
/// already carry the diagnosis (`launch` names the scope or the pid, `show`
/// distinguishes "never mapped" from "not observed"), so they are logged
/// verbatim rather than re-worded into something less specific.
pub fn start(compositor: &Arc<dyn Compositor>, decision: BootDecision) -> Option<Supervised> {
    let BootDecision::Launch(app_id) = decision else {
        tracing::info!(reason = decision.reason(), "boot client: not launching");
        return None;
    };
    tracing::info!(app_id = %app_id, reason = decision.reason(), "boot client: launching");
    launch_and_show(compositor, app_id)
}

/// One launch + show, shared by the first start and every relaunch.
fn launch_and_show(compositor: &Arc<dyn Compositor>, app_id: AppId) -> Option<Supervised> {
    // The class supplies the command AND the environment — the supervised form
    // takes no argv at all, so a relaunch cannot drift from the first launch.
    let exits = match compositor.launch_supervised(app_id) {
        Ok(exits) => exits,
        Err(reply) => {
            tracing::error!(app_id = %app_id, %reply, "boot client: launch failed; not showing");
            return None;
        }
    };
    let launched_at = Instant::now();
    tracing::info!(app_id = %app_id, "boot client: launched");

    let reply = compositor.show(app_id);
    if reply.starts_with("error:") {
        // The process IS running, so it is still supervised: a show that did not
        // take is a compositor problem, not a reason to abandon a live app and
        // stop noticing when it dies.
        tracing::error!(app_id = %app_id, %reply, "boot client: show failed; supervising anyway");
    } else {
        tracing::info!(app_id = %app_id, "boot client: on screen");
    }
    Some(Supervised {
        app_id,
        exits,
        launched_at,
    })
}

/// Keep the boot app alive. Blocks until supervision ends.
///
/// The loop is: wait for the exit the launch handed us, classify it, ask
/// [`after_exit`] what to do, log the answer, and either relaunch or stop. Every
/// decision is made by that pure function; this half only performs it.
///
/// `sleep` is injected so the backoff behaviour is testable without a test that
/// actually sleeps for a minute.
pub fn supervise(
    compositor: &Arc<dyn Compositor>,
    mut current: Supervised,
    policy: RestartPolicy,
    mut sleep: impl FnMut(Duration),
) {
    let mut fast_exits = FastExits::default();
    loop {
        let app_id = current.app_id;
        // The ONLY way this loop learns an app is gone. A closed channel means
        // the reaper thread went away without reporting — treat it as a failed
        // exit rather than silently stopping, since a supervisor that quits
        // without a log line is the §9 complaint about v1.
        let (kind, status) = match current.exits.recv() {
            Ok(status) => (ExitKind::of(&status), Some(status)),
            Err(_) => (ExitKind::Failed, None),
        };
        let ran_for = current.launched_at.elapsed();
        fast_exits = fast_exits.record(ran_for, policy.fast_exit);

        let action = after_exit(policy, fast_exits, kind, compositor.on_screen_app(), app_id);
        let ran_ms = ran_for.as_millis() as u64;
        let status = status.map(|s| s.to_string()).unwrap_or_else(|| {
            "the launcher's reaper went away without reporting a status".to_string()
        });

        let delay = match action {
            NextAction::Relaunch { delay } => {
                tracing::info!(
                    app_id = %app_id, ran_ms, %status, reason = action.reason(),
                    "boot client: relaunching",
                );
                delay
            }
            // WARN, and it carries the count. A backoff that logs at info is a
            // session that looks idle while it is actually failing.
            NextAction::BackOff {
                delay,
                consecutive_fast_exits,
            } => {
                tracing::warn!(
                    app_id = %app_id, ran_ms, %status, consecutive_fast_exits,
                    delay_secs = delay.as_secs(), reason = action.reason(),
                    "boot client: backing off",
                );
                delay
            }
            NextAction::Yield { to } => {
                tracing::info!(
                    app_id = %app_id, ran_ms, on_screen = ?to, reason = action.reason(),
                    "boot client: yielding; supervision ends",
                );
                return;
            }
            NextAction::Stop { why } => {
                match why {
                    // The one variant that is a problem rather than a choice.
                    StopReason::GaveUp { .. } => tracing::error!(
                        app_id = %app_id, ran_ms, %status, ?why, reason = action.reason(),
                        "boot client: giving up; the television will show the shell until \
                         someone intervenes",
                    ),
                    _ => tracing::info!(
                        app_id = %app_id, ran_ms, %status, ?why, reason = action.reason(),
                        "boot client: supervision ends",
                    ),
                }
                return;
            }
        };

        sleep(delay);

        // RELAUNCH, retrying here rather than falling back to the wait above.
        //
        // The wait consumes `current`'s exit channel, so looping back to it with
        // a handle whose process never started would `recv()` an instant `Err`
        // and re-enter the decision with a fabricated exit — counting failures
        // that never happened. A launch that will not start is its own state and
        // is retried in its own loop, which leaves `current` untouched until a
        // launch really succeeds.
        current = loop {
            match launch_and_show(compositor, app_id) {
                Some(next) => break next,
                None => {
                    // Counted as a fast exit: repeated failures to START are the
                    // same crash-loop shape as repeated instant exits, and reach
                    // the same backoff instead of retrying every two seconds.
                    fast_exits = fast_exits.record(Duration::ZERO, policy.fast_exit);
                    let action = after_exit(
                        policy,
                        fast_exits,
                        ExitKind::Failed,
                        compositor.on_screen_app(),
                        app_id,
                    );
                    match action {
                        NextAction::Stop { why } => {
                            tracing::error!(
                                app_id = %app_id, ?why, reason = action.reason(),
                                "boot client: relaunch failed and supervision ends",
                            );
                            return;
                        }
                        NextAction::Yield { to } => {
                            tracing::info!(
                                app_id = %app_id, on_screen = ?to, reason = action.reason(),
                                "boot client: relaunch failed and something else has the screen",
                            );
                            return;
                        }
                        NextAction::BackOff {
                            delay,
                            consecutive_fast_exits,
                        } => {
                            tracing::warn!(
                                app_id = %app_id, consecutive_fast_exits,
                                delay_secs = delay.as_secs(),
                                "boot client: relaunch failed; backing off",
                            );
                            sleep(delay);
                        }
                        NextAction::Relaunch { delay } => {
                            tracing::warn!(
                                app_id = %app_id, delay_secs = delay.as_secs(),
                                "boot client: relaunch failed; trying again",
                            );
                            sleep(delay);
                        }
                    }
                }
            }
        };
    }
}

// ---------------------------------------------------------------------------
// Supervision — keeping the boot app alive without ever fighting a live session
// ---------------------------------------------------------------------------

/// How an exit is classified. **The whole relaunch policy keys on this**, so it
/// is a type rather than a bool: "the app failed" and "the user quit" are
/// different events that happen to arrive down the same channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// Exited 0. Evidence of intent: somebody chose to leave.
    Clean,
    /// Non-zero, or killed by a signal. The case durability is about.
    Failed,
}

impl ExitKind {
    /// Classify a process exit status.
    ///
    /// A signal is `Failed`: `code()` is `None` for a signalled process, and
    /// treating "no exit code" as clean would make a SIGSEGV — the exact crash
    /// this supervisor exists for — look like the user pressing Quit.
    pub fn of(status: &std::process::ExitStatus) -> Self {
        match status.code() {
            Some(0) => Self::Clean,
            _ => Self::Failed,
        }
    }
}

/// Counts consecutive fast exits. The prototype's `fast_exits` variable, typed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FastExits(pub u32);

impl FastExits {
    /// Fold one exit in: a fast exit increments, a long-lived one RESETS.
    ///
    /// The reset is the half that matters. Without it an app that runs happily
    /// for hours and then dies twice would inherit a stale count and back off as
    /// though it were crash-looping.
    pub fn record(self, ran_for: Duration, fast_exit: Duration) -> Self {
        if ran_for < fast_exit {
            Self(self.0.saturating_add(1))
        } else {
            Self(0)
        }
    }
}

/// The tunables, lifted straight from `dev/gamescope/client.sh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    pub relaunch: RelaunchPolicy,
    pub fast_exit: Duration,
    pub fast_exit_limit: u32,
    pub backoff: Duration,
    pub delay: Duration,
    /// 0 = never give up.
    pub give_up_after: u32,
}

impl RestartPolicy {
    /// Read the policy out of `[session]`.
    pub fn from_config(c: &CoreConfig) -> Self {
        Self {
            relaunch: c.session.boot_relaunch,
            fast_exit: Duration::from_secs(c.session.boot_fast_exit_secs),
            fast_exit_limit: c.session.boot_fast_exit_limit,
            backoff: Duration::from_secs(c.session.boot_backoff_secs),
            delay: Duration::from_secs(c.session.boot_relaunch_delay_secs),
            give_up_after: c.session.boot_give_up_after,
        }
    }
}

/// What the supervisor does after the app it launched exited.
///
/// **Four distinct variants rather than an `Option<Duration>` plus flags**, so
/// the journal line and the code path agree with each other and an operator
/// reading either at 11pm learns the same thing. Every one of them is a state
/// somebody will eventually have to diagnose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextAction {
    /// Start it again after `delay`. The ordinary case.
    Relaunch { delay: Duration },
    /// Start it again, but slowly, because it keeps dying immediately.
    /// Logged at WARN with the count, because a silent backoff is a session
    /// that looks idle when it is actually failing (§9's complaint about v1).
    BackOff {
        delay: Duration,
        consecutive_fast_exits: u32,
    },
    /// Stop supervising: something ELSE is on screen now, so relaunching would
    /// take the screen from whatever the user moved on to.
    Yield { to: Option<AppId> },
    /// Stop supervising for good, and say why.
    Stop { why: StopReason },
}

/// Why supervision ended. Distinct variants for the same reason as [`NextAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// A clean exit under the `on-failure` policy: the user quit, and the shell
    /// is behind the app to catch them.
    UserQuit,
    /// `relaunch = "never"`.
    PolicyNever,
    /// The give-up bound was configured and reached.
    GaveUp { consecutive_fast_exits: u32 },
}

impl NextAction {
    /// The sentence logged for this action.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Relaunch { .. } => "the app exited; starting it again",
            Self::BackOff { .. } => {
                "the app keeps exiting immediately; backing off rather than hot-spinning \
                 (a fixed runtime is picked up on the next attempt)"
            }
            Self::Yield { .. } => {
                "something else is on screen now, so relaunching would take it from the user"
            }
            Self::Stop {
                why: StopReason::UserQuit,
            } => {
                "the app exited cleanly, so this was a quit, not a crash; the shell has the screen"
            }
            Self::Stop {
                why: StopReason::PolicyNever,
            } => "relaunch policy is `never`",
            Self::Stop {
                why: StopReason::GaveUp { .. },
            } => "the give-up bound was reached; the app will NOT be started again",
        }
    }
}

/// Decide what to do after the supervised app exited. **Pure.**
///
/// `on_screen` is what the compositor says is on screen NOW — the guard against
/// the one way a relaunch could stomp a live session: the user quit the boot app,
/// started something else, and the boot app's process only then went away. If
/// anything other than our own id (or nothing) is on screen, we yield.
pub fn after_exit(
    policy: RestartPolicy,
    fast_exits: FastExits,
    exit: ExitKind,
    on_screen: Option<AppId>,
    app_id: AppId,
) -> NextAction {
    // The user moved on. Checked FIRST, because every other branch would start a
    // process and take the screen.
    if let Some(other) = on_screen {
        if other != app_id {
            return NextAction::Yield { to: Some(other) };
        }
    }
    match policy.relaunch {
        RelaunchPolicy::Never => {
            return NextAction::Stop {
                why: StopReason::PolicyNever,
            }
        }
        RelaunchPolicy::OnFailure if exit == ExitKind::Clean => {
            return NextAction::Stop {
                why: StopReason::UserQuit,
            }
        }
        RelaunchPolicy::OnFailure | RelaunchPolicy::Always => {}
    }
    if policy.give_up_after > 0 && fast_exits.0 >= policy.give_up_after {
        return NextAction::Stop {
            why: StopReason::GaveUp {
                consecutive_fast_exits: fast_exits.0,
            },
        };
    }
    if fast_exits.0 >= policy.fast_exit_limit {
        return NextAction::BackOff {
            delay: policy.backoff,
            consecutive_fast_exits: fast_exits.0,
        };
    }
    NextAction::Relaunch {
        delay: policy.delay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const BOOT: AppId = AppId::new(9003);
    const SHELL: AppId = AppId::new(9001);
    const OTHER: AppId = AppId::new(4242);

    fn fresh() -> Observed<'static> {
        Observed {
            base_layer: &[],
            on_screen: None,
        }
    }

    // -- the boot decision (fresh vs in-use) ---------------------------------

    #[test]
    fn a_fresh_compositor_launches_the_boot_app() {
        assert_eq!(
            decide(Some(BOOT), Some(fresh())),
            BootDecision::Launch(BOOT)
        );
    }

    #[test]
    fn nothing_configured_launches_nothing() {
        assert_eq!(decide(None, Some(fresh())), BootDecision::NotConfigured);
    }

    /// THE RULE. A core restart under a live game must not relaunch or steal the
    /// screen — §5/§9. This is the case that costs a television if it regresses.
    ///
    /// Mutation-check: make `decide` ignore `on_screen`, or return `Launch`
    /// whenever `boot_app` is set, and this goes red.
    #[test]
    fn a_restart_under_a_live_app_never_launches() {
        let live = Observed {
            base_layer: &[BOOT, SHELL],
            on_screen: Some(BOOT),
        };
        assert_eq!(
            decide(Some(BOOT), Some(live)),
            BootDecision::AppOnScreen(BOOT)
        );

        // Either signal ALONE is still "in use": §5's point is that the atom and
        // the resolved window can disagree, so neither may be the only test.
        let atom_only = Observed {
            base_layer: &[BOOT, SHELL],
            on_screen: None,
        };
        assert_eq!(
            decide(Some(BOOT), Some(atom_only)),
            BootDecision::BaseLayerInUse
        );

        let window_only = Observed {
            base_layer: &[],
            on_screen: Some(BOOT),
        };
        assert_eq!(
            decide(Some(BOOT), Some(window_only)),
            BootDecision::AppOnScreen(BOOT)
        );
    }

    /// A user who quit back to the shell is NOT a fresh session: the core does
    /// not resurrect what they closed.
    #[test]
    fn a_session_returned_to_the_shell_is_not_relaunched() {
        let at_shell = Observed {
            base_layer: &[SHELL],
            on_screen: Some(SHELL),
        };
        assert_eq!(
            decide(Some(BOOT), Some(at_shell)),
            BootDecision::AppOnScreen(SHELL)
        );
    }

    /// Fail closed: an unreadable session is no evidence, not weak evidence.
    #[test]
    fn an_unreadable_session_does_not_launch() {
        assert_eq!(decide(Some(BOOT), None), BootDecision::SessionUnreadable);
    }

    #[test]
    fn every_reason_says_something() {
        for d in [
            BootDecision::NotConfigured,
            BootDecision::BaseLayerInUse,
            BootDecision::AppOnScreen(BOOT),
            BootDecision::SessionUnreadable,
            BootDecision::Launch(BOOT),
        ] {
            assert!(!d.reason().is_empty(), "{d:?}");
        }
        for a in [
            NextAction::Relaunch {
                delay: Duration::ZERO,
            },
            NextAction::BackOff {
                delay: Duration::ZERO,
                consecutive_fast_exits: 3,
            },
            NextAction::Yield { to: Some(OTHER) },
            NextAction::Stop {
                why: StopReason::UserQuit,
            },
            NextAction::Stop {
                why: StopReason::PolicyNever,
            },
            NextAction::Stop {
                why: StopReason::GaveUp {
                    consecutive_fast_exits: 5,
                },
            },
        ] {
            assert!(!a.reason().is_empty(), "{a:?}");
        }
    }

    // -- the restart decision -------------------------------------------------

    fn policy() -> RestartPolicy {
        RestartPolicy::from_config(&CoreConfig::default())
    }

    #[test]
    fn the_default_policy_is_the_prototypes_measured_constants() {
        // dev/gamescope/client.sh:211-213. If the kit's numbers move, these
        // should be revisited deliberately rather than drift apart in silence.
        let p = policy();
        assert_eq!(p.fast_exit, Duration::from_secs(10));
        assert_eq!(p.fast_exit_limit, 3);
        assert_eq!(p.backoff, Duration::from_secs(60));
        assert_eq!(p.delay, Duration::from_secs(2));
        assert_eq!(p.relaunch, RelaunchPolicy::OnFailure);
        assert_eq!(p.give_up_after, 0, "never give up by default");
    }

    /// A crash relaunches; a clean exit does not.
    ///
    /// Mutation-check: make `after_exit` ignore `ExitKind` and this goes red.
    #[test]
    fn a_crash_relaunches_and_a_quit_does_not() {
        assert_eq!(
            after_exit(policy(), FastExits(0), ExitKind::Failed, None, BOOT),
            NextAction::Relaunch {
                delay: Duration::from_secs(2)
            }
        );
        assert_eq!(
            after_exit(policy(), FastExits(0), ExitKind::Clean, None, BOOT),
            NextAction::Stop {
                why: StopReason::UserQuit
            }
        );
    }

    /// `always` restores the prototype's behaviour; `never` disables it.
    #[test]
    fn the_relaunch_policy_is_honoured() {
        let always = RestartPolicy {
            relaunch: RelaunchPolicy::Always,
            ..policy()
        };
        assert!(matches!(
            after_exit(always, FastExits(0), ExitKind::Clean, None, BOOT),
            NextAction::Relaunch { .. }
        ));
        let never = RestartPolicy {
            relaunch: RelaunchPolicy::Never,
            ..policy()
        };
        assert_eq!(
            after_exit(never, FastExits(0), ExitKind::Failed, None, BOOT),
            NextAction::Stop {
                why: StopReason::PolicyNever
            }
        );
    }

    /// THE OTHER WAY A RELAUNCH COULD STOMP A LIVE SESSION: the user quit the
    /// boot app, started something else, and only then did the old process go
    /// away. Checked before every relaunch, and before the policy.
    ///
    /// Mutation-check: move the `on_screen` guard below the policy match, or
    /// delete it, and this goes red.
    #[test]
    fn something_else_on_screen_always_wins_over_a_relaunch() {
        for kind in [ExitKind::Failed, ExitKind::Clean] {
            for p in [
                policy(),
                RestartPolicy {
                    relaunch: RelaunchPolicy::Always,
                    ..policy()
                },
            ] {
                assert_eq!(
                    after_exit(p, FastExits(9), kind, Some(OTHER), BOOT),
                    NextAction::Yield { to: Some(OTHER) },
                    "a relaunch must never take the screen from another app"
                );
            }
        }
        // Our own id on screen is NOT someone else — a stale window while the
        // process dies must not stop the relaunch.
        assert!(matches!(
            after_exit(policy(), FastExits(0), ExitKind::Failed, Some(BOOT), BOOT),
            NextAction::Relaunch { .. }
        ));
    }

    /// Fast exits accumulate into a backoff, and a long run RESETS the count.
    ///
    /// Mutation-check: drop the `else { Self(0) }` reset in `FastExits::record`
    /// and the last assertion goes red.
    #[test]
    fn repeated_fast_exits_back_off_and_a_long_run_clears_them() {
        let p = policy();
        let fast = Duration::from_secs(1);
        let long = Duration::from_secs(3600);

        let mut n = FastExits::default();
        for _ in 0..3 {
            n = n.record(fast, p.fast_exit);
        }
        assert_eq!(n, FastExits(3));
        assert_eq!(
            after_exit(p, n, ExitKind::Failed, None, BOOT),
            NextAction::BackOff {
                delay: Duration::from_secs(60),
                consecutive_fast_exits: 3,
            }
        );

        // Two fast exits is not yet a crash-loop.
        assert!(matches!(
            after_exit(p, FastExits(2), ExitKind::Failed, None, BOOT),
            NextAction::Relaunch { .. }
        ));

        // An app that ran for an hour and then died is not crash-looping, even
        // if it crash-looped yesterday.
        assert_eq!(n.record(long, p.fast_exit), FastExits(0));
    }

    #[test]
    fn the_give_up_bound_is_off_by_default_and_works_when_set() {
        // Off: a huge fast-exit count still only backs off, forever.
        assert!(matches!(
            after_exit(policy(), FastExits(9999), ExitKind::Failed, None, BOOT),
            NextAction::BackOff { .. }
        ));
        let bounded = RestartPolicy {
            give_up_after: 5,
            ..policy()
        };
        assert_eq!(
            after_exit(bounded, FastExits(5), ExitKind::Failed, None, BOOT),
            NextAction::Stop {
                why: StopReason::GaveUp {
                    consecutive_fast_exits: 5
                }
            }
        );
    }

    /// A signalled process has no exit code. Treating that as clean would make a
    /// SIGSEGV — the exact crash this exists for — look like a user quitting.
    #[test]
    fn a_signalled_exit_is_a_failure_not_a_clean_one() {
        use std::os::unix::process::ExitStatusExt as _;
        let segv = std::process::ExitStatus::from_raw(11);
        assert_eq!(segv.code(), None, "a signalled status has no code");
        assert_eq!(ExitKind::of(&segv), ExitKind::Failed);

        let ok = std::process::Command::new("true").status().unwrap();
        assert_eq!(ExitKind::of(&ok), ExitKind::Clean);
        let bad = std::process::Command::new("false").status().unwrap();
        assert_eq!(ExitKind::of(&bad), ExitKind::Failed);
    }

    // -- the runner -----------------------------------------------------------

    /// Records calls, and hands out exit statuses from a script.
    struct Recorder {
        calls: Mutex<Vec<String>>,
        /// One entry per launch, popped in order: `Some(status)` to report an
        /// exit, `None` to make the launch itself fail.
        script: Mutex<Vec<Option<std::process::ExitStatus>>>,
        on_screen: Mutex<Option<AppId>>,
    }

    fn status(code: i32) -> std::process::ExitStatus {
        std::process::Command::new(if code == 0 { "true" } else { "false" })
            .status()
            .unwrap()
    }

    impl Recorder {
        fn new(script: Vec<Option<std::process::ExitStatus>>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                script: Mutex::new(script),
                on_screen: Mutex::new(None),
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Compositor for Recorder {
        fn screen_state(&self) -> String {
            "{}".into()
        }
        fn show(&self, app_id: AppId) -> String {
            self.calls.lock().unwrap().push(format!("show {app_id}"));
            "ok".into()
        }
        fn home(&self) -> String {
            panic!("the boot client must never call home")
        }
        fn launch(&self, _: AppId, _: &[String]) -> String {
            panic!("the boot client must use the SUPERVISED launch")
        }
        fn launch_supervised(
            &self,
            app_id: AppId,
        ) -> Result<std::sync::mpsc::Receiver<std::process::ExitStatus>, String> {
            let mut script = self.script.lock().unwrap();
            // An EXHAUSTED script FAILS the launch, which ends supervision.
            // Deliberately not "one more clean exit": a test whose expected
            // behaviour is "stop" would then pass by HANGING if the code under
            // test ever decided to relaunch instead, and a hang is a far worse
            // failure than a red assertion — it burns a CI slot and tells you
            // nothing. Verified: with the exit-kind check mutated out, the
            // clean-exit test now fails on its call list in milliseconds.
            let next = if script.is_empty() {
                None
            } else {
                script.remove(0)
            };
            match next {
                Some(st) => {
                    self.calls.lock().unwrap().push(format!("launch {app_id}"));
                    let (tx, rx) = std::sync::mpsc::channel();
                    tx.send(st).unwrap();
                    Ok(rx)
                }
                None => {
                    self.calls
                        .lock()
                        .unwrap()
                        .push(format!("launch-failed {app_id}"));
                    Err("error: the app was NOT launched".into())
                }
            }
        }
        fn on_screen_app(&self) -> Option<AppId> {
            *self.on_screen.lock().unwrap()
        }
    }

    /// The default policy PLUS a give-up bound.
    ///
    /// Every runner test uses this so the loop can only ever terminate: with
    /// `give_up_after = 0` (the shipped default, and the right one for an
    /// appliance) a regression that relaunches when it should stop would HANG
    /// the suite instead of failing it — which is how a mutation check burns a
    /// CI slot and tells you nothing. The bound is above `fast_exit_limit`, so
    /// the backoff still engages first and the delay assertions are unaffected.
    fn bounded() -> RestartPolicy {
        RestartPolicy {
            give_up_after: 5,
            ..policy()
        }
    }

    fn run_supervised(rec: &Arc<Recorder>, policy: RestartPolicy) -> Vec<Duration> {
        let c: Arc<dyn Compositor> = rec.clone();
        let slept = Arc::new(Mutex::new(Vec::new()));
        let s = slept.clone();
        if let Some(sup) = start(&c, BootDecision::Launch(BOOT)) {
            supervise(&c, sup, policy, move |d| s.lock().unwrap().push(d));
        }
        let out = slept.lock().unwrap().clone();
        out
    }

    /// A failed FIRST launch never reaches `show`.
    ///
    /// Mutation-check: make `launch_and_show` continue past the launch error.
    #[test]
    fn a_failed_boot_launch_never_shows() {
        let rec = Recorder::new(vec![None]);
        run_supervised(&rec, bounded());
        assert_eq!(rec.calls(), vec!["launch-failed 9003".to_string()]);
    }

    /// The good path launches, shows, and stops on the clean exit.
    #[test]
    fn a_clean_exit_ends_supervision_without_relaunching() {
        let rec = Recorder::new(vec![Some(status(0))]);
        let slept = run_supervised(&rec, bounded());
        assert_eq!(
            rec.calls(),
            vec!["launch 9003".to_string(), "show 9003".to_string()]
        );
        assert!(slept.is_empty(), "a quit should not sleep before anything");
    }

    /// A crash relaunches AND re-shows — a new process has a new window, so the
    /// base layer has to be re-asserted, exactly as the prototype does.
    #[test]
    fn a_crash_relaunches_and_re_shows() {
        let rec = Recorder::new(vec![Some(status(1)), Some(status(0))]);
        let slept = run_supervised(&rec, bounded());
        assert_eq!(
            rec.calls(),
            vec![
                "launch 9003".to_string(),
                "show 9003".to_string(),
                "launch 9003".to_string(),
                "show 9003".to_string(),
            ]
        );
        assert_eq!(slept, vec![Duration::from_secs(2)]);
    }

    /// Three instant crashes reach the backoff delay rather than hot-spinning.
    #[test]
    fn a_crash_loop_reaches_the_backoff_delay() {
        let rec = Recorder::new(vec![
            Some(status(1)),
            Some(status(1)),
            Some(status(1)),
            Some(status(0)),
        ]);
        let slept = run_supervised(&rec, bounded());
        assert_eq!(
            slept,
            vec![
                Duration::from_secs(2),
                Duration::from_secs(2),
                Duration::from_secs(60),
            ],
            "the third consecutive fast exit must stretch to the backoff"
        );
    }

    /// The supervisor stops the moment something else owns the screen, even
    /// mid-crash-loop.
    #[test]
    fn the_supervisor_yields_to_another_app() {
        let rec = Recorder::new(vec![Some(status(1))]);
        *rec.on_screen.lock().unwrap() = Some(OTHER);
        run_supervised(&rec, bounded());
        assert_eq!(
            rec.calls(),
            vec!["launch 9003".to_string(), "show 9003".to_string()],
            "there must be no second launch once another app is on screen"
        );
    }

    /// A skip decision must not touch the compositor AT ALL — not even a read.
    #[test]
    fn a_skip_calls_nothing() {
        struct Exploding;
        impl Compositor for Exploding {
            fn screen_state(&self) -> String {
                panic!("boot client touched the compositor on a skip")
            }
            fn show(&self, _: AppId) -> String {
                panic!("boot client called show on a skip")
            }
            fn home(&self) -> String {
                panic!("boot client called home on a skip")
            }
            fn launch(&self, _: AppId, _: &[String]) -> String {
                panic!("boot client called launch on a skip")
            }
            fn launch_supervised(
                &self,
                _: AppId,
            ) -> Result<std::sync::mpsc::Receiver<std::process::ExitStatus>, String> {
                panic!("boot client launched on a skip")
            }
            fn on_screen_app(&self) -> Option<AppId> {
                panic!("boot client read the screen on a skip")
            }
        }
        let c: Arc<dyn Compositor> = Arc::new(Exploding);
        for d in [
            BootDecision::NotConfigured,
            BootDecision::BaseLayerInUse,
            BootDecision::AppOnScreen(BOOT),
            BootDecision::SessionUnreadable,
        ] {
            assert!(start(&c, d).is_none(), "{d:?} must not launch");
        }
    }
}

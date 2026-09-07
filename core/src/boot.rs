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
    /// The boot app is ALREADY RUNNING and nothing here started it — a core
    /// restart under a live session. Supervise it without touching it.
    ///
    /// Distinct from [`Self::AppOnScreen`] because the actions differ: this one
    /// attaches a watcher, that one walks away. Both leave the screen alone.
    Adopt(AppId),
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
            Self::Adopt(_) => {
                "the boot app is already on screen and this core did not start it (a core \
                 restart under a live session), so it is adopted for supervision — WITHOUT \
                 launching or touching the screen"
            }
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
        // OUR app, running, unsupervised — a core restart under a live session.
        // Adoption is not a relaunch: it attaches a watcher and performs no
        // compositor action at all, so the "a restart never steals the screen"
        // property is untouched. Anything ELSE on screen is someone else's
        // session and we walk away from it.
        return if on_screen == boot_app {
            BootDecision::Adopt(boot_app)
        } else {
            BootDecision::AppOnScreen(on_screen)
        };
    }
    if !observed.base_layer.is_empty() {
        return BootDecision::BaseLayerInUse;
    }
    BootDecision::Launch(boot_app)
}

/// The id a [`Compositor`] reports when it could not read the screen at all.
///
/// The supervisor treats `None` as "the coast is clear", so an implementation
/// answering `None` on a failed read would let a relaunch through on no
/// evidence. It answers with this instead, which is not any real app and so
/// always yields — fail closed. A named constant rather than a bare `u32::MAX`
/// at two sites, so they cannot drift and so a log line can say "unreadable"
/// instead of an id nobody will find in a config file at 1 a.m.
pub const SCREEN_UNREADABLE: AppId = AppId::new(u32::MAX);

/// Render an on-screen id for a log line, naming the unreadable sentinel.
pub fn describe_on_screen(on_screen: Option<AppId>) -> String {
    match on_screen {
        Some(id) if id == SCREEN_UNREADABLE => "unreadable (the screen could not be read)".into(),
        Some(id) => id.to_string(),
        None => "nothing".into(),
    }
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
    how: ExitSource,
    launched_at: Instant,
}

/// How a [`Supervised`] app's exit will be learned about.
///
/// The two arms are the whole difference between a launch and an adoption, and
/// keeping them as one type is what lets `supervise` treat them identically
/// everywhere else.
enum ExitSource {
    /// We forked it, so the reaper hands us the real status.
    Launched(std::sync::mpsc::Receiver<std::process::ExitStatus>),
    /// We found it already running. The cgroup scope is the only signal
    /// available: it disappears when the last process in it exits. That tells us
    /// THAT the app went away and never WHY — hence [`ExitKind::Unknown`].
    ///
    /// The scope name is the identity, not the pid: it embeds a per-launch tag,
    /// so it cannot be confused by pid reuse the way a bare `/proc/<pid>` check
    /// could.
    Adopted { pid: u32, scope: String },
}

/// How often an adopted app's scope is checked for having gone away.
///
/// A constant rather than a config key: it trades notice latency against wakeups
/// and there is no deployment-specific right answer — the relaunch delay is 2 s
/// anyway, so a finer poll would not make the television recover sooner.
const ADOPTED_POLL: Duration = Duration::from_secs(2);

impl Supervised {
    /// The app being supervised.
    pub fn app_id(&self) -> AppId {
        self.app_id
    }

    /// Was this app adopted rather than launched by this core?
    pub fn is_adopted(&self) -> bool {
        matches!(self.how, ExitSource::Adopted { .. })
    }

    /// Block until the app exits, and say what is known about how.
    ///
    /// The one place either source is waited on, so `supervise` does not branch
    /// on how the app got here. `sleep` is injected for the adopted poll so a
    /// test does not wait real seconds.
    fn wait(&self, mut sleep: impl FnMut(Duration)) -> (ExitKind, String) {
        match &self.how {
            ExitSource::Launched(exits) => match exits.recv() {
                Ok(status) => (ExitKind::of(&status), status.to_string()),
                // A closed channel means the reaper went away without reporting.
                // Treated as a failure rather than silently stopping: a
                // supervisor that quits with no log line is §9's complaint.
                Err(_) => (
                    ExitKind::Failed,
                    "the launcher's reaper went away without reporting a status".to_string(),
                ),
            },
            ExitSource::Adopted { pid, scope } => {
                while crate::launch::scope_of(*pid).is_some_and(|s| s.unit == *scope) {
                    sleep(ADOPTED_POLL);
                }
                (
                    ExitKind::Unknown,
                    format!("adopted app left its scope {scope} (status unknowable)"),
                )
            }
        }
    }
}

/// Adopt an app this core did NOT launch, so a restarted core still supervises it.
///
/// **Adoption performs no compositor action.** It does not launch, does not
/// write the base layer, and does not show anything — it attaches a watcher to
/// something already on screen. That is what keeps it from becoming the very
/// thing the boot decision exists to prevent: there is no path here that turns
/// an observation into a relaunch of something the user quit, because there is
/// no launch in it at all. What happens LATER, when the adopted app exits, is
/// the ordinary [`after_exit`] decision plus [`ExitKind::Unknown`]'s refusal to
/// guess.
///
/// Returns `None` when the app's pid cannot be resolved (nothing to watch) or it
/// is not in a scope we can name (nothing to watch it BY) — both of which mean
/// the honest answer is "not supervised", logged, rather than a watcher on a
/// guess.
pub fn adopt(compositor: &Arc<dyn Compositor>, app_id: AppId) -> Option<Supervised> {
    let Some(pid) = compositor.running_app_pid(app_id) else {
        tracing::warn!(
            app_id = %app_id,
            "boot client: cannot adopt — the running app resolves to no pid, so there is \
             nothing to watch. It stays UNSUPERVISED until the next fresh session.",
        );
        return None;
    };
    let Some(scope) = crate::launch::scope_of(pid) else {
        tracing::warn!(
            app_id = %app_id, pid,
            "boot client: cannot adopt — pid {pid} is in no cgroup scope this core can name, \
             so its exit cannot be observed. It stays UNSUPERVISED.",
            pid = pid,
        );
        return None;
    };
    tracing::info!(
        app_id = %app_id, pid, scope = %scope.unit,
        "boot client: adopting the running app; NOT launching and NOT touching the screen",
    );
    Some(Supervised {
        app_id,
        how: ExitSource::Adopted {
            pid,
            scope: scope.unit,
        },
        // Adopted apps have been up for an unknown time. `Instant::now()` makes
        // the FIRST measured lifetime start here rather than pretending to know
        // when it launched — which would be a fabricated number in a log line.
        // The only cost is that an app adopted and then immediately crashed
        // counts as one fast exit, which is true enough to act on.
        launched_at: Instant::now(),
    })
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
    match decision {
        BootDecision::Launch(app_id) => {
            tracing::info!(app_id = %app_id, reason = decision.reason(), "boot client: launching");
            launch_and_show(compositor, app_id)
        }
        // A core restart under a live session. Supervision is re-armed WITHOUT
        // launching or showing anything — see `adopt`.
        BootDecision::Adopt(app_id) => {
            tracing::info!(app_id = %app_id, reason = decision.reason(), "boot client: adopting");
            adopt(compositor, app_id)
        }
        _ => {
            tracing::info!(reason = decision.reason(), "boot client: not launching");
            None
        }
    }
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
        how: ExitSource::Launched(exits),
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
        // The ONLY way this loop learns an app is gone — a channel we hold
        // because we forked the process, or a scope we watched because we
        // adopted it. Never an inference from the world.
        let (kind, status) = current.wait(&mut sleep);
        let ran_for = current.launched_at.elapsed();
        fast_exits = fast_exits.record(ran_for, policy.fast_exit);

        let action = after_exit(policy, fast_exits, kind, compositor.on_screen_app(), app_id);
        let ran_ms = ran_for.as_millis() as u64;

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
                    app_id = %app_id, ran_ms, on_screen = %describe_on_screen(to),
                    reason = action.reason(),
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
                                app_id = %app_id, on_screen = %describe_on_screen(to),
                                reason = action.reason(),
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
    /// **The app exited and we cannot know how.**
    ///
    /// This is the honest state for an ADOPTED app: `wait()` may only be called
    /// on a process you forked, so a core that found the app already running has
    /// no way to read its exit status. Watching the cgroup scope disappear tells
    /// us THAT it exited, never WHY.
    ///
    /// It is a third variant rather than being folded into `Failed` because the
    /// fold would be a guess with a bad failure mode: under `on-failure` it
    /// would relaunch an app the user had just quit, and the team lead ranked
    /// that worse than a black screen (you cannot get out of it). The policy
    /// decides explicitly instead — see [`after_exit`].
    Unknown,
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
    /// An ADOPTED app exited and `on-failure` cannot tell a crash from a quit.
    UnknowableExit,
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
                "the app exited cleanly, so this was a quit and not a crash. Whatever is behind \
                 it owns the screen now — and if nothing else draws on this compositor that is an \
                 EMPTY one, i.e. a black television: set boot_relaunch = \"always\""
            }
            Self::Stop {
                why: StopReason::PolicyNever,
            } => "relaunch policy is `never`",
            Self::Stop {
                why: StopReason::GaveUp { .. },
            } => "the give-up bound was reached; the app will NOT be started again",
            Self::Stop {
                why: StopReason::UnknowableExit,
            } => {
                "this app was ADOPTED, so its exit status is unknowable — a status can only \
                 be read for a process this core forked. Under boot_relaunch = \"on-failure\" \
                 that cannot be told apart from a quit, so it is not relaunched. Set \
                 boot_relaunch = \"always\" to keep an adopted app alive across crashes"
            }
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
        // `on-failure` needs to know whether this WAS a failure, and for an
        // adopted app nothing can tell it. Refusing is the safe half of an
        // unavoidable trade: guessing "crash" resurrects an app the user quit
        // and leaves them unable to escape it, while guessing "quit" costs a
        // relaunch nobody sees. `always` has no such problem — it does not need
        // to know — so an operator who wants adopted apps kept alive sets it,
        // which is what the deployed box runs.
        RelaunchPolicy::OnFailure if exit == ExitKind::Unknown => {
            return NextAction::Stop {
                why: StopReason::UnknowableExit,
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
        // OUR app, already running: adopted for supervision, NOT launched.
        // Adoption performs no compositor action — asserted below in
        // `adopting_never_launches_and_never_touches_the_screen`.
        let live = Observed {
            base_layer: &[BOOT, SHELL],
            on_screen: Some(BOOT),
        };
        assert_eq!(decide(Some(BOOT), Some(live)), BootDecision::Adopt(BOOT));

        // SOMEONE ELSE's app: we walk away entirely.
        let theirs = Observed {
            base_layer: &[OTHER],
            on_screen: Some(OTHER),
        };
        assert_eq!(
            decide(Some(BOOT), Some(theirs)),
            BootDecision::AppOnScreen(OTHER)
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
            BootDecision::Adopt(BOOT)
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

    /// THE PREMISE CORRECTION, pinned.
    ///
    /// `on-failure` was justified by "v2 has a shell behind the app". It does
    /// not: §13 Q1 is open and `tv-shell-gamescope-child.sh` is still
    /// `exec sleep infinity`, so on a deployment without a shell a clean quit
    /// lands on an EMPTY compositor — a black television. The reasoning was
    /// sound and its input was six months early.
    ///
    /// The `UserQuit` reason is what an operator reads at 1 a.m. staring at a
    /// black screen, so it must not assert the shell that is missing; it must
    /// name the consequence and the override. Asserted here because a comment
    /// nothing checks is how the premise got stale in the first place.
    #[test]
    fn the_quit_reason_does_not_claim_a_shell_that_may_not_exist() {
        let reason = NextAction::Stop {
            why: StopReason::UserQuit,
        }
        .reason();
        assert!(
            !reason.contains("the shell has the screen"),
            "the reason asserts a shell v2 does not have yet: {reason}"
        );
        assert!(
            reason.contains("black television"),
            "the reason must name the consequence of a quit with nothing behind it: {reason}"
        );
        assert!(
            reason.contains("boot_relaunch"),
            "and must name the override that fixes it: {reason}"
        );
    }

    /// The unreadable-screen sentinel reads as words, not as a fake app id.
    #[test]
    fn an_unreadable_screen_is_described_not_numbered() {
        let described = describe_on_screen(Some(SCREEN_UNREADABLE));
        assert!(described.contains("unreadable"), "{described}");
        assert!(
            !described.contains(&u32::MAX.to_string()),
            "a log line must not print the sentinel as an id: {described}"
        );
        assert_eq!(describe_on_screen(Some(BOOT)), "9003");
        assert_eq!(describe_on_screen(None), "nothing");

        // And the sentinel must still FAIL CLOSED: it is not the supervised app,
        // so it yields.
        assert_eq!(
            after_exit(
                policy(),
                FastExits(0),
                ExitKind::Failed,
                Some(SCREEN_UNREADABLE),
                BOOT
            ),
            NextAction::Yield {
                to: Some(SCREEN_UNREADABLE)
            }
        );
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

    // -- adoption -------------------------------------------------------------
    //
    // THE GAP THIS CLOSES, found on hardware 2026-09-06: after a core restart
    // the running boot app was unsupervised PERMANENTLY. The cause was the
    // property that makes a restart safe — `Supervised` can only be constructed
    // by a launch from this core — so a core that correctly declined to launch
    // also adopted nothing. Since the core unit is `Restart=always`, crash
    // durability lapsed the first time the core restarted and stayed lapsed.

    /// **THE SAFETY PROPERTY.** Adoption attaches a watcher and does nothing
    /// else: no launch, no show, no base-layer write. That is what keeps it from
    /// becoming the thing the boot decision exists to prevent.
    ///
    /// Mutation-check: make `adopt` call `launch_and_show`, or make `start`'s
    /// `Adopt` arm fall through to the `Launch` arm, and this goes red.
    #[test]
    fn adopting_never_launches_and_never_touches_the_screen() {
        /// Every screen-touching method panics; only the two reads adoption is
        /// allowed to make are implemented.
        struct AdoptOnly {
            pid: Option<u32>,
        }
        impl Compositor for AdoptOnly {
            fn screen_state(&self) -> String {
                "{}".into()
            }
            fn show(&self, _: AppId) -> String {
                panic!("adoption showed something — it must not touch the screen")
            }
            fn home(&self) -> String {
                panic!("adoption called home")
            }
            fn launch(&self, _: AppId, _: &[String]) -> String {
                panic!("adoption launched something — it must not start a process")
            }
            fn launch_supervised(
                &self,
                _: AppId,
            ) -> Result<std::sync::mpsc::Receiver<std::process::ExitStatus>, String> {
                panic!("adoption launched something — it must not start a process")
            }
            fn on_screen_app(&self) -> Option<AppId> {
                Some(BOOT)
            }
            fn running_app_pid(&self, _: AppId) -> Option<u32> {
                self.pid
            }
        }

        // A pid that is real (this process) but in no `app-steam-app*` scope, so
        // `adopt` reaches its second refusal rather than watching a guess.
        let c: Arc<dyn Compositor> = Arc::new(AdoptOnly {
            pid: Some(std::process::id()),
        });
        // Whatever it decides, it must not have launched or shown anything —
        // the panics above are the assertion.
        let _ = start(&c, BootDecision::Adopt(BOOT));

        // And with no pid at all it refuses cleanly rather than watching nothing.
        let c: Arc<dyn Compositor> = Arc::new(AdoptOnly { pid: None });
        assert!(
            start(&c, BootDecision::Adopt(BOOT)).is_none(),
            "an app with no resolvable pid must not be adopted"
        );
    }

    /// An adopted app's exit is UNKNOWABLE, and the policy must say so rather
    /// than guess.
    ///
    /// `wait()` may only be called on a process you forked, so a core that found
    /// the app already running can watch its scope disappear but can never read
    /// a status. Folding that into `Failed` would relaunch an app the user had
    /// just quit — ranked worse than a black screen, because the user cannot get
    /// out of it.
    ///
    /// Mutation-check: make `after_exit` treat `Unknown` like `Failed` and this
    /// goes red.
    #[test]
    fn an_unknowable_exit_is_refused_under_on_failure_and_relaunched_under_always() {
        assert_eq!(
            after_exit(policy(), FastExits(0), ExitKind::Unknown, None, BOOT),
            NextAction::Stop {
                why: StopReason::UnknowableExit
            },
            "on-failure cannot tell an adopted crash from an adopted quit, so it must not guess"
        );

        let always = RestartPolicy {
            relaunch: RelaunchPolicy::Always,
            ..policy()
        };
        assert!(
            matches!(
                after_exit(always, FastExits(0), ExitKind::Unknown, None, BOOT),
                NextAction::Relaunch { .. }
            ),
            "`always` does not need to know why, so an adopted app stays alive under it"
        );

        // And the refusal must SAY what to set — this is the one stop reason an
        // operator can act on.
        let why = NextAction::Stop {
            why: StopReason::UnknowableExit,
        }
        .reason();
        assert!(why.contains("adopted") || why.contains("ADOPTED"), "{why}");
        assert!(
            why.contains("always"),
            "the reason must name the fix: {why}"
        );
    }

    /// The screen guard still wins over an adopted app's exit, exactly as it
    /// does for a launched one — adoption must not create a second path that
    /// can stomp whatever the user moved on to.
    #[test]
    fn an_adopted_exit_still_yields_to_another_app() {
        let always = RestartPolicy {
            relaunch: RelaunchPolicy::Always,
            ..policy()
        };
        assert_eq!(
            after_exit(always, FastExits(0), ExitKind::Unknown, Some(OTHER), BOOT),
            NextAction::Yield { to: Some(OTHER) }
        );
    }

    /// `ExitKind::Unknown` is only ever produced by the adopted path, and a
    /// launched app never reports it — otherwise the `on-failure` refusal would
    /// start firing for apps whose status we really do have.
    #[test]
    fn only_an_adopted_app_reports_an_unknowable_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(status(1)).unwrap();
        let launched = Supervised {
            app_id: BOOT,
            how: ExitSource::Launched(rx),
            launched_at: Instant::now(),
        };
        assert!(!launched.is_adopted());
        let (kind, described) = launched.wait(|_| {});
        assert_eq!(kind, ExitKind::Failed);
        assert!(!described.is_empty());
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
        fn running_app_pid(&self, _: AppId) -> Option<u32> {
            // The runner tests never adopt; adoption has its own tests.
            None
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
            fn running_app_pid(&self, _: AppId) -> Option<u32> {
                panic!("boot client looked for a pid on a skip")
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

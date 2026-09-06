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

use crate::atoms::AppId;
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

/// Run the boot sequence: launch the class, then put it on screen.
///
/// Every failure is logged and returns — a boot client that could not start is
/// not a reason to take the core down, because the core is the thing an operator
/// uses to find out why. The replies are the IPC reply strings, which already
/// carry the diagnosis (`launch` names the scope or the pid, `show` distinguishes
/// "never mapped" from "not observed"), so they are logged verbatim rather than
/// re-worded into something less specific.
pub fn run(compositor: &Arc<dyn Compositor>, decision: BootDecision) {
    let BootDecision::Launch(app_id) = decision else {
        tracing::info!(reason = decision.reason(), "boot client: not launching");
        return;
    };
    tracing::info!(app_id = %app_id, reason = decision.reason(), "boot client: launching");

    // The class supplies the command AND the environment — an empty command is
    // the "from the [[app]] table" form (see `compositor::resolve_launch`).
    let reply = compositor.launch(app_id, &[]);
    if reply.starts_with("error:") {
        tracing::error!(app_id = %app_id, %reply, "boot client: launch failed; not showing");
        return;
    }
    tracing::info!(app_id = %app_id, %reply, "boot client: launched");

    let reply = compositor.show(app_id);
    if reply.starts_with("error:") {
        tracing::error!(app_id = %app_id, %reply, "boot client: show failed");
        return;
    }
    tracing::info!(app_id = %app_id, "boot client: on screen");
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: AppId = AppId::new(9003);
    const SHELL: AppId = AppId::new(9001);

    fn fresh() -> Observed<'static> {
        Observed {
            base_layer: &[],
            on_screen: None,
        }
    }

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
        // Both signals present, as they are mid-game.
        let live = Observed {
            base_layer: &[BOOT, SHELL],
            on_screen: Some(BOOT),
        };
        assert_eq!(
            decide(Some(BOOT), Some(live)),
            BootDecision::AppOnScreen(BOOT)
        );

        // And either one alone is still "in use": §5's point is that the atom
        // and the resolved window can disagree, so neither may be the only test.
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

    /// A user who quit back to the shell is NOT a fresh session: the base layer
    /// holds the shell id, so the core does not resurrect what they closed.
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
    ///
    /// Mutation-check: treat `None` as fresh and this goes red.
    #[test]
    fn an_unreadable_session_does_not_launch() {
        assert_eq!(decide(Some(BOOT), None), BootDecision::SessionUnreadable);
    }

    #[test]
    fn every_skip_reason_says_something() {
        for d in [
            BootDecision::NotConfigured,
            BootDecision::BaseLayerInUse,
            BootDecision::AppOnScreen(BOOT),
            BootDecision::SessionUnreadable,
            BootDecision::Launch(BOOT),
        ] {
            assert!(!d.reason().is_empty(), "{d:?}");
        }
    }

    /// The runner is a state machine over IPC reply strings, so a compositor
    /// whose `launch` fails must never reach `show` — otherwise a boot with a
    /// dead app would still write the base layer, which is the §5 "one write,
    /// then verify" contract broken from the inside.
    ///
    /// Mutation-check: drop the `return` from `run`'s launch-failure arm and
    /// this goes red.
    #[test]
    fn a_failed_boot_launch_never_shows() {
        use std::sync::Mutex;

        struct Recorder {
            launch_reply: String,
            calls: Mutex<Vec<String>>,
        }
        impl Compositor for Recorder {
            fn screen_state(&self) -> String {
                panic!("the boot client has no business reading screen state")
            }
            fn show(&self, app_id: AppId) -> String {
                self.calls.lock().unwrap().push(format!("show {app_id}"));
                "ok".into()
            }
            fn home(&self) -> String {
                panic!("the boot client must never call home")
            }
            fn launch(&self, app_id: AppId, command: &[String]) -> String {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("launch {app_id} argv={}", command.len()));
                self.launch_reply.clone()
            }
        }

        // Keep the concrete handle so the call log can be read back; the runner
        // gets the same object as a trait object.
        let record = |launch_reply: &str| {
            let rec = Arc::new(Recorder {
                launch_reply: launch_reply.to_string(),
                calls: Mutex::new(Vec::new()),
            });
            let as_compositor: Arc<dyn Compositor> = rec.clone();
            run(&as_compositor, BootDecision::Launch(BOOT));
            let calls = rec.calls.lock().unwrap().clone();
            calls
        };

        // A launch that was not confirmed stops the sequence dead.
        assert_eq!(
            record("error: the app was NOT launched"),
            vec!["launch 9003 argv=0".to_string()],
            "a failed launch must not be followed by a show"
        );

        // The good path does both, and passes an EMPTY argv — the "from the
        // [[app]] table" form. A boot client that passed a command of its own
        // would be a second place the launch environment had to be remembered.
        assert_eq!(
            record("{\"pid\":1}"),
            vec!["launch 9003 argv=0".to_string(), "show 9003".to_string()],
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
        }
        let c: Arc<dyn Compositor> = Arc::new(Exploding);
        for d in [
            BootDecision::NotConfigured,
            BootDecision::BaseLayerInUse,
            BootDecision::AppOnScreen(BOOT),
            BootDecision::SessionUnreadable,
        ] {
            run(&c, d);
        }
    }
}

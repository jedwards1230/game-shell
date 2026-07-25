//! Grab / handoff / presenter-routing state machine and its pure decision
//! helpers (plus the presenter unit tests).
//!
//! Split out of the former monolithic `input.rs` (behavior-preserving).

use super::*;

/// Switch the fleet to the **shell presenter** (the `grab` IPC, and — since
/// follow-focus — a compositor focus change back to the shell home; see
/// [`focus_presenter_target`]). Per-fleet mode toggle (Phase 5): set the mode,
/// ensure every pad is physically grabbed, and tear down any per-player
/// virtual gamepads. The physical grab is *kept* — the shell presenter routes
/// pad input to nav keys + `intent:*` on the shared virtual keyboard/mouse.
pub(crate) fn grab_all(sh: &mut Shared, fleet: &mut Fleet) {
    info!(pads = fleet.pads.len(), "presenter -> Shell (grab)");
    sh.metrics.inc_transitions();
    sh.presenter = Presenter::Shell;
    sh.handoff_pinned = false; // leaving any Handoff clears the Moonlight pin
    for pad in fleet.pads.values_mut() {
        pad.grab(sh); // no-op if already grabbed; re-grabs if somehow released
        pad.enter_shell(sh);
    }
    check_grab_invariant(sh, fleet);
}

/// Switch the fleet to the **keyboard presenter** (a follow-focus transition to a
/// window with a `keyboard` input contract, e.g. `tv.plex.Plex`). Mechanically
/// this is [`grab_all`] with a different presenter label: keep every pad
/// physically grabbed (nothing leaks to the compositor) and tear down any
/// per-player virtual pad — the focused app is driven by the shell key-map on the
/// shared virtual keyboard/mouse (`handle_shell`), NOT a virtual gamepad. Dropping
/// the virtual pad here is the fix's core: with no virtual pad alive, Steam has
/// nothing to exclusive-grab, so a focused Plex actually receives the d-pad.
pub(crate) fn keyboard_all(sh: &mut Shared, fleet: &mut Fleet) {
    info!(pads = fleet.pads.len(), "presenter -> Keyboard (contract)");
    sh.metrics.inc_transitions();
    sh.presenter = Presenter::Keyboard;
    sh.handoff_pinned = false;
    for pad in fleet.pads.values_mut() {
        pad.grab(sh); // keep the physical grab (shell-style key emulation)
        pad.enter_shell(sh); // drop any virtual pad — no gamepad in this context
    }
    check_grab_invariant(sh, fleet);
}

/// Switch the fleet to the **game presenter** (the `release` IPC, and — since
/// follow-focus — a compositor focus change to a real app toplevel; see
/// [`focus_presenter_target`]). Per-fleet mode toggle (Phase 5): set the mode,
/// **keep** the physical grab (so nothing leaks to the compositor), and create
/// one clean virtual gamepad per pad. The game reads the virtual pads; Home is
/// intercepted into `intent:home-*`.
pub(crate) fn release_all(sh: &mut Shared, fleet: &mut Fleet) {
    info!(pads = fleet.pads.len(), "presenter -> Game (release)");
    sh.metrics.inc_transitions();
    sh.presenter = Presenter::Game;
    sh.handoff_pinned = false; // leaving any Handoff clears the Moonlight pin
    for pad in fleet.pads.values_mut() {
        // Keep the physical grab; only ensure it's grabbed (it is, post-join).
        pad.grab(sh);
        pad.enter_game(sh);
    }
    check_grab_invariant(sh, fleet);
}

/// The **`handoff` IPC** entry point (#221): hand the physical pads directly to a
/// Moonlight stream. Delegates to [`enter_handoff`] with `pinned = true` so
/// follow-focus never overrides it while the stream window holds focus — only the
/// shell's explicit `grab` ends it. (A `handoff` *contract* reaches the same
/// presenter via [`enter_handoff`] with `pinned = false`, from `apply_focus_change`.)
pub(crate) fn handoff_all(sh: &mut Shared, fleet: &mut Fleet) {
    // The explicit `handoff` IPC is the Moonlight-stream path: PIN it so
    // follow-focus never overrides it while the stream window holds focus.
    enter_handoff(sh, fleet, true);
}

/// Enter the **handoff presenter**: drop any virtual twin and, unless an overlay
/// forces the grab, release the physical `EVIOCGRAB` so SDL/Moonlight reads the
/// real evdev node. `pinned` records *why* Handoff was entered:
///
/// * `true` — the explicit `handoff` IPC (Moonlight stream, #221). Follow-focus
///   must not override it (the streamed window holds focus for the whole stream);
///   only the shell's explicit `grab` ends it.
/// * `false` — a `handoff` **input contract** matched by follow-focus. This one
///   *does* follow focus: moving to another app (or back to the shell) arbitrates
///   normally, so a contract-driven handoff can never strand the pads ungrabbed.
pub(crate) fn enter_handoff(sh: &mut Shared, fleet: &mut Fleet, pinned: bool) {
    info!(
        pads = fleet.pads.len(),
        pinned, "presenter -> Handoff (handoff)"
    );
    sh.metrics.inc_transitions();
    sh.presenter = Presenter::Handoff;
    sh.handoff_pinned = pinned;
    // Reconcile the grab against `should_grab` rather than unconditionally
    // ungrabbing: with an overlay focused, the invariant is that the physical
    // pad stays grabbed even over Handoff (#262), so the app can't read the raw
    // evdev node while a shell overlay is open. Mirrors `set_overlay_focus`.
    let grab = should_grab(sh.shell_focus, sh.overlay_focus, sh.presenter);
    for pad in fleet.pads.values_mut() {
        pad.enter_shell(sh); // drop any virtual pad
        if grab {
            pad.grab(sh); // keep the physical grab (overlay focused over Handoff)
        } else {
            pad.ungrab(sh); // release the grab so SDL reads the real node
        }
    }
    check_grab_invariant(sh, fleet);
}

/// Which presenter's handler processes pad events, given the shell-focus and
/// overlay-focus flags and the base presenter (#262).
///
/// EITHER flag forces [`Presenter::Shell`] over any base:
///
/// * `shell_focus` — the shell's layer surface is mapped and owns the screen.
///   This is the shell's own authoritative declaration, and it must win over the
///   base presenter because the base is derived from compositor focus, which
///   **cannot see the shell at all**: the shell is a wlr-layer-shell surface, so
///   Hyprland's `activewindow` keeps naming whatever *toplevel* was focused last
///   (a backgrounded Steam, say) the whole time the shell owns the screen. Left
///   ungated, follow-focus resolves that stale class and flips the pad to the
///   app while the user is looking at the home screen.
/// * `overlay_focus` — a modal shell overlay is open *over* a running app, so
///   the pad drives the overlay rather than the app underneath.
///
/// Otherwise the base presenter routes as usual. Pure, so the routing decision
/// is unit-tested without a controller.
pub(crate) fn route_presenter(
    shell_focus: bool,
    overlay_focus: bool,
    presenter: Presenter,
) -> Presenter {
    if shell_focus || overlay_focus {
        Presenter::Shell
    } else {
        presenter
    }
}

/// Whether every pad should hold the physical `EVIOCGRAB`, given the shell-focus
/// and overlay-focus flags and the base presenter (#262). Grabbed in every state
/// except a [`Presenter::Handoff`] base with neither flag set — the one case
/// where SDL/Moonlight must read the raw evdev node directly. Either flag
/// therefore forces the grab even over Handoff (the shell owning the screen, or
/// an overlay open above the app, both mean the pad drives the shell). Pure —
/// the grab transitions in [`set_overlay_focus`] / [`set_shell_focus`] are
/// unit-tested without a controller.
pub(crate) fn should_grab(shell_focus: bool, overlay_focus: bool, presenter: Presenter) -> bool {
    shell_focus || overlay_focus || presenter != Presenter::Handoff
}

/// Whether the current presenter means a focused *app* owns the screen (rather
/// than the shell home). True for [`Presenter::Keyboard`]/[`Presenter::Game`]/
/// [`Presenter::Handoff`], false for [`Presenter::Shell`]. Drives whether the
/// force-quit combo (Back+Home+LB+RB) also emits the app-quit keyboard chord — a
/// controller escape from an app that captured input, for a couch user with no
/// keyboard in reach. The Shell home has no app to quit. `Keyboard` is included
/// (a keyboard-contract app like Plex is receiving our emulated keys and owns the
/// screen); its exclusion was the on-device "locked inside Plex" regression. Pure,
/// so the escape policy is unit-tested without a controller.
pub(crate) fn presenter_owns_app(presenter: Presenter) -> bool {
    !matches!(presenter, Presenter::Shell)
}

/// Decide whether a KEY event should be forwarded to the Game presenter's
/// virtual pad, given the pad's `masked` set (buttons held at the shell→app
/// flip; see [`PadDevice::masked_keys`]) and the event's `code`/`value`.
///
/// A **masked** button must not reach the app until it has been released and
/// pressed again — the app never saw the corresponding down (the physical press
/// was consumed by the shell to launch the app), so a lone up (or the stale
/// held-down / autorepeat) would be a phantom activation (the Steam-BPM A-leak,
/// #295 follow-up). Values follow evdev KEY semantics: `1` press, `0` release,
/// `2` autorepeat.
///
/// * `code` not in `masked` → forward normally (`true`). A fresh press after the
///   user let go is unaffected.
/// * `code` in `masked`, `value == 0` (release) → remove it from `masked` and
///   **swallow** (`false`): the mask is now cleared, so the *next* press of this
///   code forwards, but this lone up itself is dropped.
/// * `code` in `masked`, `value == 1 | 2` (press/repeat) → **swallow**
///   (`false`): it was held across the flip.
///
/// Mutates `masked` (clears the code on its release). Pure otherwise, so the
/// swallow decision is unit-tested without a controller. ABS axes/sticks/d-pad
/// are never masked — only digital buttons leak this way — so this is called
/// only from the KEY arm of [`PadDevice::handle_game`].
pub(crate) fn mask_forward_decision(masked: &mut HashSet<u16>, code: u16, value: i32) -> bool {
    if !masked.contains(&code) {
        return true; // not masked -> forward as usual
    }
    if value == 0 {
        // Release of a masked button: the mask lifts here, but the lone up is
        // swallowed (the app never saw the down).
        masked.remove(&code);
    }
    // Press/repeat/release of a still-masked button: never forward.
    false
}

/// Whether an ABS `value` lies within `[center - deadzone, center + deadzone]` —
/// the neutral zone used by the axis flip-mask. The discrete d-pad hat passes
/// `center = 0, deadzone = 0` (neutral only at exactly `0`); the analog sticks
/// pass their calibrated center + deadzone. Pure, so the neutrality rule is
/// unit-tested without a controller.
pub(crate) fn abs_in_neutral_zone(value: i32, center: i32, deadzone: i32) -> bool {
    (value - center).abs() <= deadzone
}

/// Decide whether an ABS event should be forwarded to the Game presenter's
/// virtual pad, given the pad's `masked` axis set (continuous axes deflected at
/// the shell→app flip; see [`PadDevice::masked_axes`]), the event's `code`, and
/// whether the value is `neutral` (within the axis's deadzone —
/// [`abs_in_neutral_zone`]).
///
/// A **masked** axis must not reach the game until it has physically returned to
/// neutral — otherwise the direction the user was holding to reach the launched
/// card would latch the fresh virtual pad (a runaway Steam Big Picture scroll,
/// the axis sibling of the #295 A-leak). The vpad sits at its own neutral default
/// while masked, so a swallowed event costs nothing.
///
/// * `code` not in `masked` → forward normally (`true`). A fresh deflection after
///   the user re-centered is unaffected.
/// * `code` in `masked`, `neutral == true` → remove it from `masked` and
///   **swallow** (`false`): the mask lifts here, so the *next* deflection of this
///   axis forwards, but this neutral report is dropped (the vpad already rests at
///   neutral).
/// * `code` in `masked`, `neutral == false` (still deflected) → **swallow**
///   (`false`): it was held across the flip.
///
/// Mutates `masked` (clears the code once it reads neutral). Pure otherwise.
pub(crate) fn mask_axis_forward_decision(
    masked: &mut HashSet<u16>,
    code: u16,
    neutral: bool,
) -> bool {
    if !masked.contains(&code) {
        return true; // not masked -> forward as usual
    }
    if neutral {
        // The axis returned to neutral: the mask lifts, but this neutral report is
        // swallowed (the vpad already rests at its neutral default).
        masked.remove(&code);
    }
    // Still-deflected, or the neutral event that lifts the mask: never forward.
    false
}

/// Toggle overlay-focus (#262): a modal shell overlay opened (`on`) or closed
/// (`off`) over a running app. Idempotent (a no-op when already in the requested
/// state). The base presenter in `sh.presenter` is deliberately left untouched —
/// it *remembers* the routing to restore — while the grab is reconciled to match
/// [`should_grab`] for the new (overlay, base) pair:
///
/// * ON → [`should_grab`] is always `true`, so every pad is grabbed. For a
///   `Handoff` base this re-takes the `EVIOCGRAB` the app was reading raw; for
///   `Shell`/`Game` it is an idempotent no-op. `Game`'s virtual pads are left in
///   place (routing goes to the shell key-map, so they simply receive nothing
///   until overlay-focus off, then resume forwarding).
/// * OFF → [`should_grab`] is `false` only for a `Handoff` base, which re-ungrabs
///   so SDL/Moonlight reads the raw node again; every other base keeps the grab.
pub(crate) fn set_overlay_focus(sh: &mut Shared, fleet: &mut Fleet, on: bool) {
    if sh.overlay_focus == on {
        return;
    }
    sh.overlay_focus = on;
    let grab = should_grab(sh.shell_focus, sh.overlay_focus, sh.presenter);
    info!(
        overlay_focus = on,
        base = ?sh.presenter,
        grab,
        pads = fleet.pads.len(),
        "overlay-focus toggled"
    );
    for pad in fleet.pads.values_mut() {
        // Overlay open/close flips the routed presenter (an app presenter ⇄ Shell)
        // without an enter_shell/enter_game, so drop any partial combo buffer here
        // too — otherwise a sequence buffered under the app presenter could strand
        // or replay to the wrong surface.
        pad.reset_combo_buffer(sh);
        if grab {
            pad.grab(sh); // idempotent + session-aware
        } else {
            pad.ungrab(sh); // idempotent
        }
    }
    check_grab_invariant(sh, fleet);
}

/// Declare whether the SHELL currently owns (or shares) the screen.
///
/// This is the shell's authoritative statement of screen ownership, mirroring
/// its own `visible:` binding in `shell.qml` — the one place that already knows
/// "the shell should own or share the screen now". The daemon cannot derive this
/// itself: the shell is a wlr-layer-shell surface and never appears in
/// Hyprland's `activewindow`, so compositor focus keeps naming a backgrounded
/// toplevel while the shell is on screen.
///
/// Like [`set_overlay_focus`] this deliberately leaves `sh.presenter` untouched —
/// it *remembers* the base routing to restore when the shell hands the screen
/// back to an app — and only reconciles routing ([`route_presenter`]) and the
/// grab ([`should_grab`]). That keeps it safe to re-assert at any time, which
/// matters because the shell re-sends it on a heartbeat: a missed edge (an app
/// that closes to background without a close event, a launch that times out, a
/// daemon restart mid-session) then self-heals on the next tick instead of
/// wedging the pad against the wrong surface until someone notices.
///
/// Idempotent — a no-op when already in the requested state, so the heartbeat
/// costs nothing in the steady state.
pub(crate) fn set_shell_focus(sh: &mut Shared, fleet: &mut Fleet, on: bool) {
    if sh.shell_focus == on {
        return;
    }
    sh.shell_focus = on;
    // Publish to the Hyprland actor so the kiosk fullscreen backstop stops
    // force-focusing a stale active window while the shell owns the screen.
    let _ = sh.shell_focus_tx.send(on);
    let grab = should_grab(sh.shell_focus, sh.overlay_focus, sh.presenter);
    info!(
        shell_focus = on,
        base = ?sh.presenter,
        grab,
        pads = fleet.pads.len(),
        "shell-focus toggled"
    );
    for pad in fleet.pads.values_mut() {
        // Same reasoning as `set_overlay_focus`: the routed presenter flips
        // (app presenter ⇄ Shell) without an enter_shell/enter_game, so drop any
        // partial combo buffer rather than replay it to the wrong surface.
        pad.reset_combo_buffer(sh);
        if grab {
            pad.grab(sh); // idempotent + session-aware
        } else {
            pad.ungrab(sh); // idempotent
        }
    }
    check_grab_invariant(sh, fleet);
}

/// Apply a settled compositor focus change to the presenter (follow-focus).
///
/// Runs from the [`Internal::FocusSettle`] handler once the focus has held for
/// [`FOCUS_SETTLE_MS`] (armed by [`schedule_focus_change`] off the `active_window`
/// watch arm) — the debounce collapses launch/close focus flaps so this applies
/// at most one net transition per settle. `class` is empty when only the shell's
/// own layer-shell surface remains (no toplevel focused). Delegates the decision
/// to [`focus_presenter_target`] (which consults the per-app [`InputContracts`])
/// and routes through the same presenter transitions as an explicit `grab`/`release`
/// (each of which asserts [`check_grab_invariant`] on its way out), so the
/// invariant is checked after any transition this triggers.
pub(crate) fn apply_focus_change(sh: &mut Shared, fleet: &mut Fleet, class: &str) {
    if let Some(target) = focus_presenter_target(
        sh.presenter,
        sh.handoff_pinned,
        sh.shell_focus,
        &sh.contracts,
        class,
    ) {
        info!(
            class = %class,
            from = ?sh.presenter,
            to = ?target,
            "presenter follow-focus"
        );
        match target {
            Presenter::Shell => grab_all(sh, fleet),
            Presenter::Keyboard => keyboard_all(sh, fleet),
            Presenter::Game => release_all(sh, fleet),
            // A `handoff` *contract* matched by follow-focus. Enter Handoff
            // UNpinned so it still follows focus away again — unlike the Moonlight
            // `handoff` IPC, which pins (handoff_all).
            Presenter::Handoff => enter_handoff(sh, fleet, false),
        }
    }
}

/// Arm (or re-arm) the follow-focus settle debounce for the newest focused-window
/// `class` ([`FOCUS_SETTLE_MS`]). Stores the pending class, bumps the settle
/// generation, and spawns a one-shot timer that posts [`Internal::FocusSettle`];
/// the handler applies the transition only if its generation is still live, so a
/// burst of focus flaps during an app launch/close collapses to the single last
/// change. Every focus event re-arms (superseding any prior timer), so a rapid
/// flap can never tear down + rebuild the virtual pad mid-transition. Explicit
/// IPC (`grab`/`release`/`handoff`/overlay-focus) bypasses this and applies
/// instantly — only the noisy compositor focus signal is debounced.
pub(crate) fn schedule_focus_change(sh: &mut Shared, class: &str) {
    // Abort any prior in-flight settle timer so at most one is ever alive: a
    // focus-churn burst re-arms rather than piling up sleeping tasks. Correctness
    // still rests on the `focus_settle_gen` check when the timer fires — this only
    // frees the superseded tasks eagerly.
    if let Some(t) = sh.focus_settle_task.take() {
        t.abort();
    }
    sh.pending_focus_class = Some(class.to_string());
    let generation = sh.next_generation();
    sh.focus_settle_gen = generation;
    let tx = sh.internal_tx.clone();
    sh.focus_settle_task = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(FOCUS_SETTLE_MS)).await;
        let _ = tx.send(Internal::FocusSettle { generation }).await;
    }));
}

/// Pure per-pad grab invariant predicate: while the session is active a pad's
/// physical grab must match the presenter policy `expected`
/// (`should_grab(shell_focus, overlay_focus, presenter)`); while inactive any grab state is
/// acceptable (pads are intentionally all ungrabbed — see [`PadDevice::grab`]'s
/// session early-return and the `SetSessionActive` handling). Factored out so the
/// invariant logic is unit-tested without a `Fleet`/`Shared` (which own uinput
/// devices).
pub(crate) fn grab_ok(session_active: bool, pad_grabbed: bool, expected: bool) -> bool {
    !session_active || pad_grabbed == expected
}

/// Assert the fleet's physical grab state matches the intended presenter policy
/// after a transition, catching silent grab-state drift (a stuck or leaked
/// controller grab).
///
/// While `session_active`, every pad's `grabbed` flag must equal
/// `should_grab(shell_focus, overlay_focus, presenter)`. `grab_all`/`keyboard_all`/`release_all`
/// call `pad.grab()` unconditionally, which is consistent with `should_grab`
/// because `should_grab(_, _, Shell)`, `should_grab(_, _, Keyboard)`, and
/// `should_grab(_, _, Game)` are all always `true` (only a `Handoff` base with
/// neither flag ungrabs, and `enter_handoff` already routes each pad through
/// `should_grab`) — so the unconditional grab is correct by the truth table, and
/// this asserts exactly that. On a violation it `error!`s (pad id,
/// expected vs actual, presenter, overlay-focus), bumps a metrics counter, and
/// `debug_assert!`s so it panics in dev/test but never in release.
pub(crate) fn check_grab_invariant(sh: &Shared, fleet: &Fleet) {
    let expected = should_grab(sh.shell_focus, sh.overlay_focus, sh.presenter);
    for pad in fleet.pads.values() {
        if !grab_ok(sh.session_active, pad.grabbed, expected) {
            error!(
                pad = %pad.wire_id,
                expected,
                actual = pad.grabbed,
                presenter = ?sh.presenter,
                overlay_focus = sh.overlay_focus,
                "grab invariant violated"
            );
            sh.metrics.inc_grab_invariant_violations();
            debug_assert!(
                grab_ok(sh.session_active, pad.grabbed, expected),
                "grab invariant violated for pad {}: expected grabbed={}, actual grabbed={} (presenter={:?}, overlay_focus={})",
                pad.wire_id,
                expected,
                pad.grabbed,
                sh.presenter,
                sh.overlay_focus,
            );
        }
    }
}

/// Decide whether a Hyprland focused-window class report should change the
/// fleet's presenter, following PR #294's "react continuously to whatever
/// Hyprland now considers active" pattern — applied to the input presenter
/// rather than kiosk fullscreen enforcement.
///
/// `focused_class` is empty when no toplevel is focused — i.e. only the
/// shell's own layer-shell surface remains, which never appears in
/// Hyprland's `activewindow` at all (see `hyprland.rs`'s `needs_fullscreen`
/// doc comment for the same fact used there). An empty class always maps to
/// [`Presenter::Shell`] (the shell owns input). A non-empty class routes through
/// its **input contract** ([`InputContracts::resolve`] → [`contract_presenter`]):
/// `gamepad`→Game (the default for unknown classes, so a class-agnostic app like
/// a Steam Remote Play `streaming_client` window still gets a real gamepad),
/// `keyboard`→Keyboard (e.g. Plex), `handoff`→Handoff.
///
/// Returns `None` when no change is warranted: the resolved target already
/// matches `current`, or `current` is a **pinned** [`Presenter::Handoff`].
/// Pinned Handoff (#221, the Moonlight stream via the explicit `handoff` IPC) is
/// a deliberate exception — follow-focus must never override it while the streamed
/// window holds compositor focus for the whole stream; the shell ends it via the
/// `grab` IPC. A Handoff reached instead via a `handoff` *contract* is NOT pinned
/// and arbitrates like any other presenter, so it can never strand the pads.
pub(crate) fn focus_presenter_target(
    current: Presenter,
    handoff_pinned: bool,
    shell_focus: bool,
    contracts: &InputContracts,
    focused_class: &str,
) -> Option<Presenter> {
    if current == Presenter::Handoff && handoff_pinned {
        return None;
    }
    // The shell owns the screen: ignore compositor focus entirely.
    //
    // The shell is a wlr-layer-shell surface and therefore NEVER appears in
    // Hyprland's `activewindow` (see `hyprland::needs_fullscreen`). Taking
    // exclusive keyboard focus does not clear the active *toplevel* either, so
    // while the shell is up `focused_class` keeps naming whatever app was
    // focused last — typically a backgrounded, still-fullscreen Steam. Acting on
    // that stale class is exactly the "controller drives the app while I'm
    // looking at the home screen" desync: follow-focus flips to Game, the pad is
    // handed to a virtual gamepad, and the shell goes dead. The empty-class case
    // below is a genuine signal but not a reliable one, so it can only ever ADD
    // a Shell transition — it can never be trusted to prove an app owns the
    // screen.
    if shell_focus {
        return None;
    }
    let target = if focused_class.is_empty() {
        Presenter::Shell
    } else {
        contract_presenter(contracts.resolve(focused_class))
    };
    if target == current {
        None
    } else {
        Some(target)
    }
}

/// Map a resolved [`InputContract`] to the presenter that honors it. The single
/// point of truth for the contract→presenter correspondence.
pub(crate) fn contract_presenter(contract: InputContract) -> Presenter {
    match contract {
        InputContract::Gamepad => Presenter::Game,
        InputContract::Keyboard => Presenter::Keyboard,
        InputContract::Handoff => Presenter::Handoff,
    }
}

/// What a Meta (BTN_MODE / Guide) TAP delivers, per (routed) presenter. Pure, so
/// the gesture map is unit-tested without a controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetaTapAction {
    /// Publish `intent:home-tap` (open the shell drawer). Shell home.
    HomeTap,
    /// Replay a real Guide press+release onto the game's virtual pad. Game.
    ReplayToPad,
    /// Deliver nothing — a keyboard-contract app has no Guide concept. Keyboard.
    Swallow,
}

/// Resolve a Meta TAP's delivery from the effective (routed) presenter. Handoff
/// is handled inline in `handle_handoff` (never routed here); mapped to
/// `Swallow` for exhaustiveness.
pub(crate) fn meta_tap_action(routed: Presenter) -> MetaTapAction {
    match routed {
        Presenter::Shell => MetaTapAction::HomeTap,
        Presenter::Game => MetaTapAction::ReplayToPad,
        Presenter::Keyboard => MetaTapAction::Swallow,
        Presenter::Handoff => MetaTapAction::Swallow,
    }
}

/// The intent a Meta HOLD fires publishes, per (routed) presenter. Only the Shell
/// home's hold publishes `intent:home-hold` (the idle reset-to-clean-home); an
/// APP presenter's hold publishes `intent:home-tap` — which, while an app is
/// running, toggles the shell's *controllable overlay drawer* + overlay-focus (a
/// non-destructive everyday escape that works regardless of who holds compositor
/// toplevel focus), rather than the heavier `home-hold` full return-to-home.
/// Pure — unit-tested without a controller.
pub(crate) fn hold_fire_intent(routed: Presenter) -> &'static str {
    match routed {
        Presenter::Shell => "home-hold",
        Presenter::Keyboard | Presenter::Game | Presenter::Handoff => "home-tap",
    }
}

#[cfg(test)]
mod presenter_tests {
    use super::*;
    use std::collections::HashMap;

    /// Contracts with only the built-in defaults (no user overrides):
    /// `steam`→gamepad, `tv.plex.Plex`→keyboard, unknown→gamepad.
    fn default_contracts() -> InputContracts {
        InputContracts::default()
    }

    #[test]
    fn shell_to_game_when_gamepad_app_focused() {
        // An unknown class defaults to the gamepad contract -> Game presenter
        // (preserves the pre-contract "any app focused ⇒ virtual pad" behavior).
        assert_eq!(
            focus_presenter_target(
                Presenter::Shell,
                false,
                false,
                &default_contracts(),
                "steam_app_12345"
            ),
            Some(Presenter::Game)
        );
    }

    #[test]
    fn shell_to_keyboard_when_plex_focused() {
        // Plex carries the built-in keyboard contract -> Keyboard presenter (no
        // virtual pad, so Steam can't grab it and Plex gets the d-pad).
        assert_eq!(
            focus_presenter_target(
                Presenter::Shell,
                false,
                false,
                &default_contracts(),
                "tv.plex.Plex"
            ),
            Some(Presenter::Keyboard)
        );
    }

    #[test]
    fn game_to_keyboard_when_focus_moves_plex() {
        // Steam (Game) focused, user opens Plex: flip Game -> Keyboard, which
        // tears down the virtual pad (breaking Steam's exclusive grab).
        assert_eq!(
            focus_presenter_target(
                Presenter::Game,
                false,
                false,
                &default_contracts(),
                "tv.plex.Plex"
            ),
            Some(Presenter::Keyboard)
        );
    }

    #[test]
    fn user_override_beats_builtin() {
        // A user can force Plex to a gamepad contract via config.
        let mut over = HashMap::new();
        over.insert("tv.plex.Plex".to_string(), InputContract::Gamepad);
        let contracts = InputContracts::new(over);
        assert_eq!(
            focus_presenter_target(Presenter::Shell, false, false, &contracts, "tv.plex.Plex"),
            Some(Presenter::Game)
        );
    }

    #[test]
    fn game_to_shell_when_focus_empty() {
        assert_eq!(
            focus_presenter_target(Presenter::Game, false, false, &default_contracts(), ""),
            Some(Presenter::Shell)
        );
    }

    #[test]
    fn no_change_when_target_matches_current() {
        // Already Shell, still no toplevel focused -> no-op (no thrash).
        assert_eq!(
            focus_presenter_target(Presenter::Shell, false, false, &default_contracts(), ""),
            None
        );
        // Already Game, focus moved to a DIFFERENT gamepad-contract app -> still
        // Game, no-op (switching between two app windows must not flap the
        // presenter, which would thrash the virtual pad).
        assert_eq!(
            focus_presenter_target(
                Presenter::Game,
                false,
                false,
                &default_contracts(),
                "another_app"
            ),
            None
        );
    }

    #[test]
    fn pinned_handoff_is_never_touched_by_follow_focus() {
        // A Moonlight stream (Handoff via the explicit `handoff` IPC) is PINNED:
        // the stream window holding focus for the whole stream must not downgrade
        // it to Game (#221), nor does focus returning to the shell end it — only
        // an explicit `grab` IPC does.
        assert_eq!(
            focus_presenter_target(
                Presenter::Handoff,
                true,
                false,
                &default_contracts(),
                "steam_app_moonlight"
            ),
            None
        );
        assert_eq!(
            focus_presenter_target(Presenter::Handoff, true, false, &default_contracts(), ""),
            None
        );
    }

    #[test]
    fn contract_handoff_follows_focus() {
        // A `handoff` CONTRACT matched by follow-focus is NOT pinned, so it still
        // arbitrates: focus to a raw-node app -> Handoff; back to the shell ->
        // Shell (no stranding). First, a class mapped to handoff enters Handoff.
        let mut over = HashMap::new();
        over.insert("com.example.RawPad".to_string(), InputContract::Handoff);
        let contracts = InputContracts::new(over);
        assert_eq!(
            focus_presenter_target(
                Presenter::Shell,
                false,
                false,
                &contracts,
                "com.example.RawPad"
            ),
            Some(Presenter::Handoff)
        );
        // Now in an UNpinned Handoff, focus returning to the shell moves out.
        assert_eq!(
            focus_presenter_target(Presenter::Handoff, false, false, &contracts, ""),
            Some(Presenter::Shell)
        );
        // ...and focus to a gamepad app moves to Game.
        assert_eq!(
            focus_presenter_target(Presenter::Handoff, false, false, &contracts, "steam"),
            Some(Presenter::Game)
        );
    }

    #[test]
    fn contract_presenter_mapping() {
        assert_eq!(contract_presenter(InputContract::Gamepad), Presenter::Game);
        assert_eq!(
            contract_presenter(InputContract::Keyboard),
            Presenter::Keyboard
        );
        assert_eq!(
            contract_presenter(InputContract::Handoff),
            Presenter::Handoff
        );
    }

    #[test]
    fn meta_tap_action_per_presenter() {
        // Shell home: a tap opens the drawer. Game: replay a real Guide to the pad.
        // Keyboard: nothing (a keyboard-contract app has no Guide concept).
        assert_eq!(meta_tap_action(Presenter::Shell), MetaTapAction::HomeTap);
        assert_eq!(meta_tap_action(Presenter::Game), MetaTapAction::ReplayToPad);
        assert_eq!(meta_tap_action(Presenter::Keyboard), MetaTapAction::Swallow);
    }

    #[test]
    fn hold_fire_intent_per_presenter() {
        // Only the Shell home's hold fires the heavy `home-hold` reset; every app
        // presenter's hold fires `home-tap` (the controllable overlay drawer).
        assert_eq!(hold_fire_intent(Presenter::Shell), "home-hold");
        assert_eq!(hold_fire_intent(Presenter::Keyboard), "home-tap");
        assert_eq!(hold_fire_intent(Presenter::Game), "home-tap");
        assert_eq!(hold_fire_intent(Presenter::Handoff), "home-tap");
    }

    #[test]
    fn overlay_focus_routes_to_shell_over_any_base() {
        // ON: the shell handler runs regardless of the base presenter, so the
        // pad drives the modal overlay, not the app (#262).
        assert_eq!(
            route_presenter(false, true, Presenter::Game),
            Presenter::Shell
        );
        assert_eq!(
            route_presenter(false, true, Presenter::Handoff),
            Presenter::Shell
        );
        assert_eq!(
            route_presenter(false, true, Presenter::Shell),
            Presenter::Shell
        );
        assert_eq!(
            route_presenter(false, true, Presenter::Keyboard),
            Presenter::Shell
        );
        // OFF: the base presenter routes as usual (no behavior change).
        assert_eq!(
            route_presenter(false, false, Presenter::Game),
            Presenter::Game
        );
        assert_eq!(
            route_presenter(false, false, Presenter::Handoff),
            Presenter::Handoff
        );
        assert_eq!(
            route_presenter(false, false, Presenter::Shell),
            Presenter::Shell
        );
        // Keyboard passes through (routed to the shell key-map by handle_event).
        assert_eq!(
            route_presenter(false, false, Presenter::Keyboard),
            Presenter::Keyboard
        );
    }

    #[test]
    fn overlay_focus_forces_grab_over_handoff() {
        // ON: grabbed regardless of base — critical for Handoff, normally
        // ungrabbed, so the app stops seeing raw events (#262).
        assert!(should_grab(false, true, Presenter::Handoff));
        assert!(should_grab(false, true, Presenter::Game));
        assert!(should_grab(false, true, Presenter::Shell));
        assert!(should_grab(false, true, Presenter::Keyboard));
        // OFF: the base grab state is restored — only a Handoff base is ungrabbed
        // (re-ungrab so SDL/Moonlight reads the raw node again).
        assert!(!should_grab(false, false, Presenter::Handoff));
        assert!(should_grab(false, false, Presenter::Game));
        assert!(should_grab(false, false, Presenter::Shell));
        // Keyboard keeps the grab (shell-style emulation; no raw-node handoff).
        assert!(should_grab(false, false, Presenter::Keyboard));
    }

    #[test]
    fn force_quit_chord_reaches_every_app_owning_presenter() {
        // The force-quit combo emits the app-quit keyboard chord whenever a
        // focused app owns the screen — Keyboard (e.g. Plex), Game, or Handoff —
        // so a couch user can always escape an app that captured input. The Shell
        // home has no app to quit. Keyboard's inclusion is the fix for the
        // on-device "locked inside Plex, no controller path back" regression.
        assert!(presenter_owns_app(Presenter::Keyboard));
        assert!(presenter_owns_app(Presenter::Game));
        assert!(presenter_owns_app(Presenter::Handoff));
        assert!(!presenter_owns_app(Presenter::Shell));
    }

    #[test]
    fn handoff_keeps_grab_when_overlay_focused() {
        // Regression (PR #296): switching to Handoff while an overlay is focused
        // must NOT drop the grab — the app would otherwise read the raw pad node
        // while a shell overlay is open (#262). `handoff_all` sets the presenter
        // to Handoff and leaves `overlay_focus` untouched, then reconciles each
        // pad against `should_grab(sh.shell_focus, sh.overlay_focus, sh.presenter)`. With an
        // overlay focused, that pair must resolve to a grab.
        let overlay_focus = true;
        let presenter_after_handoff = Presenter::Handoff;
        assert!(should_grab(false, overlay_focus, presenter_after_handoff));

        // Without an overlay, Handoff correctly ungrabs (the raw-node case).
        assert!(!should_grab(false, false, presenter_after_handoff));
    }

    // --- flip-mask (#295 follow-up: swallow buttons held at shell→app flip) ---

    /// A button held at the flip (present in the mask) has its press/repeat
    /// swallowed, then its release clears the mask and is itself swallowed, and
    /// a subsequent fresh press forwards normally.
    #[test]
    fn masked_button_swallowed_until_released_then_fresh_press_forwards() {
        const BTN_A: u16 = cfg::BTN_SOUTH;
        // Simulate `enter_game` capturing BTN_A as held at the flip.
        let mut masked: HashSet<u16> = HashSet::new();
        masked.insert(BTN_A);

        // A stale autorepeat of the still-held A is swallowed (never forwarded).
        assert!(!mask_forward_decision(&mut masked, BTN_A, 2));
        assert!(masked.contains(&BTN_A), "repeat must not clear the mask");

        // The release (value 0) clears the mask AND is swallowed (the app never
        // saw the corresponding down, so a lone up would be a phantom event).
        assert!(!mask_forward_decision(&mut masked, BTN_A, 0));
        assert!(
            !masked.contains(&BTN_A),
            "release must clear the code from the mask"
        );

        // A fresh press after the user let go is no longer masked -> forwards.
        assert!(mask_forward_decision(&mut masked, BTN_A, 1));
        // ...as does its release.
        assert!(mask_forward_decision(&mut masked, BTN_A, 0));
    }

    /// A masked button's *press* (value 1) — the case where the physical button
    /// was released and re-pressed while the mask still stands (e.g. the user
    /// mashed it before letting go cleanly) — is also swallowed; only a value-0
    /// release lifts the mask.
    #[test]
    fn masked_button_press_is_swallowed() {
        const BTN_A: u16 = cfg::BTN_SOUTH;
        let mut masked: HashSet<u16> = HashSet::new();
        masked.insert(BTN_A);
        assert!(!mask_forward_decision(&mut masked, BTN_A, 1));
        assert!(masked.contains(&BTN_A), "press must not clear the mask");
    }

    /// A button that was NOT held at the flip (absent from the mask) forwards
    /// unconditionally — normal post-flip gameplay is unaffected, whatever the
    /// value.
    #[test]
    fn unmasked_button_always_forwards() {
        const BTN_B: u16 = cfg::BTN_EAST;
        let mut masked: HashSet<u16> = HashSet::new();
        // Mask holds a DIFFERENT code; B was not held at the flip.
        masked.insert(cfg::BTN_SOUTH);
        assert!(mask_forward_decision(&mut masked, BTN_B, 1));
        assert!(mask_forward_decision(&mut masked, BTN_B, 2));
        assert!(mask_forward_decision(&mut masked, BTN_B, 0));
        // The unrelated masked code is untouched by decisions about B.
        assert!(masked.contains(&cfg::BTN_SOUTH));
    }

    /// An empty mask (nothing held at the flip — the common launch-with-nothing
    /// -held case) forwards everything.
    #[test]
    fn empty_mask_forwards_everything() {
        let mut masked: HashSet<u16> = HashSet::new();
        assert!(mask_forward_decision(&mut masked, cfg::BTN_SOUTH, 1));
        assert!(mask_forward_decision(&mut masked, cfg::BTN_SOUTH, 0));
        assert!(masked.is_empty());
    }

    #[test]
    fn flip_mask_is_subset_of_held_keys() {
        // `enter_game` snapshots the flip-mask as `clear() + extend(held_keys)`,
        // so the mask is always a subset of the currently-held buttons (the 3a
        // invariant asserted by the debug_assert in `enter_game`). This mirrors
        // that operation without a `PadDevice` (which owns uinput devices).
        let mut held: HashSet<u16> = HashSet::new();
        held.insert(cfg::BTN_SOUTH);
        held.insert(cfg::BTN_EAST);

        let mut masked: HashSet<u16> = HashSet::new();
        masked.clear();
        masked.extend(held.iter().copied());
        assert!(
            masked.iter().all(|c| held.contains(c)),
            "flip-mask must be a subset of held_keys"
        );

        // An empty held set snapshots an empty mask — still a (trivial) subset.
        let empty: HashSet<u16> = HashSet::new();
        let mut masked2: HashSet<u16> = HashSet::new();
        masked2.extend(empty.iter().copied());
        assert!(masked2.iter().all(|c| empty.contains(c)));
        assert!(masked2.is_empty());
    }

    // --- axis flip-mask (swallow an axis deflected at the shell→app flip) ---

    /// The reported repro: the **d-pad hat** held to navigate to the launched
    /// card is deflected at the flip. It must be swallowed until it returns to
    /// neutral (`0`), and only then does a fresh deflection forward — so the fresh
    /// virtual pad never latches the held direction into a runaway Steam Big
    /// Picture scroll. Neutrality is derived with the same rule the runtime uses
    /// for the discrete hat (`abs_in_neutral_zone(value, 0, 0)` == `value == 0`).
    #[test]
    fn masked_hat_swallowed_until_neutral_then_fresh_deflection_forwards() {
        const HAT: u16 = cfg::ABS_HAT0X;
        // Simulate `enter_game` snapshotting the hat as held LEFT (-1) at the flip.
        let mut masked: HashSet<u16> = HashSet::new();
        masked.insert(HAT);

        // A still-deflected LEFT report is swallowed and does NOT lift the mask.
        assert!(!mask_axis_forward_decision(
            &mut masked,
            HAT,
            abs_in_neutral_zone(-1, 0, 0)
        ));
        assert!(
            masked.contains(&HAT),
            "deflection must not clear the axis mask"
        );

        // The return to center (0) lifts the mask AND is swallowed (the vpad hat
        // already rests at its neutral default, so nothing needs forwarding).
        assert!(!mask_axis_forward_decision(
            &mut masked,
            HAT,
            abs_in_neutral_zone(0, 0, 0)
        ));
        assert!(
            !masked.contains(&HAT),
            "neutral must clear the axis from the mask"
        );

        // A fresh LEFT deflection after re-centering forwards normally.
        assert!(mask_axis_forward_decision(
            &mut masked,
            HAT,
            abs_in_neutral_zone(-1, 0, 0)
        ));
    }

    /// An **analog stick** deflected past its calibrated deadzone at the flip is
    /// swallowed until it settles back inside the deadzone, then re-deflections
    /// forward — the stick sibling of the hat case above.
    #[test]
    fn masked_stick_swallowed_until_inside_deadzone() {
        const AXIS: u16 = cfg::ABS_X;
        const CENTER: i32 = 128;
        const DEADZONE: i32 = 30;
        // Full-left (value 0) at the flip — well past the deadzone.
        let mut masked: HashSet<u16> = HashSet::new();
        masked.insert(AXIS);

        assert!(!mask_axis_forward_decision(
            &mut masked,
            AXIS,
            abs_in_neutral_zone(0, CENTER, DEADZONE)
        ));
        assert!(
            masked.contains(&AXIS),
            "deflection must not clear the axis mask"
        );

        // Drift back to just inside the deadzone lifts the mask (and is swallowed).
        assert!(!mask_axis_forward_decision(
            &mut masked,
            AXIS,
            abs_in_neutral_zone(CENTER + DEADZONE, CENTER, DEADZONE)
        ));
        assert!(
            !masked.contains(&AXIS),
            "inside-deadzone must clear the mask"
        );

        // A fresh deflection past the deadzone forwards normally.
        assert!(mask_axis_forward_decision(
            &mut masked,
            AXIS,
            abs_in_neutral_zone(255, CENTER, DEADZONE)
        ));
    }

    /// An axis that was neutral (centered) at the flip is never added to the mask,
    /// so it forwards immediately at any value — an idle stick keeps working the
    /// instant the game starts (a centered stick sends no events, but a stray
    /// centered report must not be swallowed either).
    #[test]
    fn unmasked_axis_forwards_immediately() {
        const AXIS: u16 = cfg::ABS_Y;
        let mut masked: HashSet<u16> = HashSet::new();
        // A DIFFERENT axis was held at the flip; ABS_Y was neutral, so it is absent.
        masked.insert(cfg::ABS_X);
        assert!(mask_axis_forward_decision(
            &mut masked,
            AXIS,
            abs_in_neutral_zone(0, 128, 30)
        ));
        assert!(mask_axis_forward_decision(
            &mut masked,
            AXIS,
            abs_in_neutral_zone(255, 128, 30)
        ));
        assert!(mask_axis_forward_decision(
            &mut masked,
            AXIS,
            abs_in_neutral_zone(128, 128, 30)
        ));
        // The unrelated masked axis is untouched by decisions about ABS_Y.
        assert!(masked.contains(&cfg::ABS_X));
    }

    /// `abs_in_neutral_zone` treats the deadzone as inclusive on both edges and
    /// the discrete hat (`deadzone = 0`) as neutral only at exactly `0`.
    #[test]
    fn abs_neutral_zone_boundaries() {
        // Hat: only 0 is neutral.
        assert!(abs_in_neutral_zone(0, 0, 0));
        assert!(!abs_in_neutral_zone(-1, 0, 0));
        assert!(!abs_in_neutral_zone(1, 0, 0));
        // Stick: inclusive band around center.
        assert!(abs_in_neutral_zone(128, 128, 30));
        assert!(abs_in_neutral_zone(158, 128, 30)); // upper edge
        assert!(abs_in_neutral_zone(98, 128, 30)); // lower edge
        assert!(!abs_in_neutral_zone(159, 128, 30));
        assert!(!abs_in_neutral_zone(97, 128, 30));
    }

    #[test]
    fn grab_invariant_predicate() {
        // Session active: the pad's grab must match the presenter policy.
        assert!(grab_ok(true, true, true)); // grabbed, expected grabbed -> ok
        assert!(grab_ok(true, false, false)); // ungrabbed, expected ungrabbed -> ok
        assert!(!grab_ok(true, false, true)); // ungrabbed but should be grabbed -> drift
        assert!(!grab_ok(true, true, false)); // grabbed but should be ungrabbed -> drift

        // Session inactive: pads are intentionally all ungrabbed, so ANY grab
        // state passes regardless of the presenter policy (the check is scoped to
        // session_active).
        assert!(grab_ok(false, false, true));
        assert!(grab_ok(false, true, false));
        assert!(grab_ok(false, false, false));
        assert!(grab_ok(false, true, true));
    }

    #[test]
    fn grab_invariant_matches_should_grab_after_transitions() {
        // The policy `check_grab_invariant` asserts: after grab_all/keyboard_all/
        // release_all the pads are grabbed (Shell/Keyboard/Game always grab), and
        // should_grab agrees.
        assert!(grab_ok(
            true,
            true,
            should_grab(false, false, Presenter::Shell)
        ));
        assert!(grab_ok(
            true,
            true,
            should_grab(false, false, Presenter::Keyboard)
        ));
        assert!(grab_ok(
            true,
            true,
            should_grab(false, false, Presenter::Game)
        ));
        // Handoff with no overlay ungrabs, and an ungrabbed pad then satisfies it.
        assert!(grab_ok(
            true,
            false,
            should_grab(false, false, Presenter::Handoff)
        ));
        // Handoff WITH an overlay keeps the grab (should_grab true).
        assert!(grab_ok(
            true,
            true,
            should_grab(false, true, Presenter::Handoff)
        ));
    }

    #[test]
    fn panic_payload_extracts_str_and_string() {
        // &str payload (the common `panic!("msg")` case).
        let p: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_payload_str(p.as_ref()), "boom");
        // String payload (e.g. `panic!("{}", e)`).
        let p: Box<dyn std::any::Any + Send> = Box::new(String::from("boom2"));
        assert_eq!(panic_payload_str(p.as_ref()), "boom2");
        // Non-string payload degrades to a placeholder rather than being lost.
        let p: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(panic_payload_str(p.as_ref()), "<non-string panic payload>");
    }
}

#[cfg(test)]
mod shell_focus_tests {
    use super::*;

    fn default_contracts() -> InputContracts {
        InputContracts::default()
    }

    /// The regression this whole change exists for.
    ///
    /// The shell is a wlr-layer-shell surface, so while it owns the screen
    /// Hyprland's `activewindow` keeps naming the last-focused *toplevel* — a
    /// backgrounded, still-fullscreen Steam. Before the `shell_focus` gate,
    /// follow-focus resolved that stale class and flipped the pad to Game: the
    /// user was looking at the home screen while the controller drove Steam
    /// (repeated nav taps, a held direction reading as "stuck navigating right").
    #[test]
    fn stale_active_class_cannot_steal_the_pad_while_shell_owns_screen() {
        assert_eq!(
            focus_presenter_target(
                Presenter::Shell,
                false,
                true, // shell owns the screen
                &default_contracts(),
                "steam", // ...but the compositor still names Steam
            ),
            None,
            "follow-focus must not leave Shell while the shell owns the screen"
        );
    }

    /// The inverse: once the shell hands the screen over, follow-focus resumes.
    #[test]
    fn follow_focus_resumes_when_shell_releases_the_screen() {
        assert_eq!(
            focus_presenter_target(
                Presenter::Shell,
                false,
                false,
                &default_contracts(),
                "steam",
            ),
            Some(Presenter::Game)
        );
    }

    /// A pinned Handoff still wins over shell-focus (unchanged precedence).
    #[test]
    fn pinned_handoff_still_beats_shell_focus() {
        assert_eq!(
            focus_presenter_target(Presenter::Handoff, true, true, &default_contracts(), ""),
            None
        );
    }

    #[test]
    fn shell_focus_routes_every_base_to_the_shell_keymap() {
        for base in [
            Presenter::Shell,
            Presenter::Keyboard,
            Presenter::Game,
            Presenter::Handoff,
        ] {
            assert_eq!(route_presenter(true, false, base), Presenter::Shell);
        }
    }

    /// Handoff normally ungrabs so SDL reads the raw node; the shell owning the
    /// screen must force the grab back, exactly as overlay-focus does.
    #[test]
    fn shell_focus_forces_the_grab_even_over_handoff() {
        assert!(!should_grab(false, false, Presenter::Handoff));
        assert!(should_grab(true, false, Presenter::Handoff));
    }

    /// Neither flag set leaves the base presenter's routing untouched.
    #[test]
    fn neither_flag_is_transparent() {
        assert_eq!(
            route_presenter(false, false, Presenter::Game),
            Presenter::Game
        );
        assert_eq!(
            route_presenter(false, false, Presenter::Handoff),
            Presenter::Handoff
        );
    }
}

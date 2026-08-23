import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick
import QtQuick.Layouts
import "components" as Components

ShellRoot {
    id: root

    property string state: "idle"
    property bool overlayDrawerOpen: false
    property var _applications: []
    property var _layout: null

    // True exactly when a modal shell overlay (the overlay nav drawer or the
    // Session QAM) is mapped OVER a running app — i.e. the same condition that
    // makes the shell PanelWindow visible in `appRunning` (see its `visible:`
    // binding). While true, the daemon must route pad input to the SHELL
    // (buttons→keyboard) instead of to the app, or A on the drawer leaks into
    // the app (e.g. Steam reopens a stream). Deriving both the daemon flag and
    // the surface's keyboardFocus off this ONE boolean keeps them from
    // desyncing: any close path (B, item select, app exit, forceQuit, home,
    // QAM close) flips `state` off `appRunning` or clears the overlay bools, so
    // it can never get stuck on. `_layout` is null until the first screen's
    // ShellLayout completes; state is `idle` before then, so the guard holds.
    readonly property bool overlayOverApp: root.state === "appRunning" && root._layout !== null && (root.overlayDrawerOpen || root._layout.sessionQam.active || root._layout.overlayNavDrawer.active)
    onOverlayOverAppChanged: inputManager.setOverlayFocus(root.overlayOverApp)

    // True exactly when the shell surface should be MAPPED — i.e. the shell owns
    // (or shares, via an over-app overlay) the screen. This is the single
    // predicate the PanelWindow derives BOTH `visible` and `keyboardFocus` from,
    // so the surface can never be drawn while declining keyboard focus. That
    // divergence was the bug: the old `keyboardFocus` keyed off `root.state`
    // alone and its comment reasoned the None case was "moot because the surface
    // is unmapped" — an invariant nothing enforced.
    //
    // It is ALSO what the daemon is told (`shell-focus`), because the daemon
    // cannot work it out: the shell is a wlr-layer-shell surface, so Hyprland's
    // `activewindow` keeps naming the last-focused toplevel (a backgrounded,
    // still-fullscreen app) the entire time the shell is on screen. Left to
    // infer, the daemon's follow-focus hands the pad to that app while the user
    // is looking at the home screen, and the kiosk fullscreen backstop
    // force-focuses it out from under the shell.
    readonly property bool shellOwnsScreen: (root.state !== "appRunning" && root.state !== "streaming" && root.state !== "reconnecting" && root.state !== "launching") || root.overlayDrawerOpen || (root._layout !== null && (root._layout.sessionQam.active || root._layout.overlayNavDrawer.active))
    onShellOwnsScreenChanged: inputManager.setShellFocus(root.shellOwnsScreen)

    // Push the shell's own state machine + media-playing predicate to the daemon
    // (`shell-state`), which surfaces it on `/status` so Home Assistant can tell
    // whether htpc-1 is safe to suspend. The daemon can't work either value out:
    // its compositor-level UiState is explicitly NOT this QML state machine, and
    // MPRIS playback is read QML-side. `root.state` goes over the wire verbatim.
    //
    // media-playing is sourced from IdleInhibitController's `mediaPlaying` — the
    // SAME predicate that decides whether to hold the Wayland idle-inhibitor.
    // Recomputing it here would let the daemon and the inhibitor disagree about
    // what "media playing" means, i.e. suspend mid-playback.
    //
    // Pushed on edge below (both change signals fire only on an actual value
    // change, so this costs nothing on a binding re-evaluation) and re-asserted
    // on the heartbeat as the self-heal backstop.
    function _pushShellState() {
        inputManager.setShellState(root.state, idleInhibit.mediaPlaying);
    }

    onStateChanged: root._pushShellState()

    // Re-assert screen ownership on a heartbeat. Every edge-triggered signal in
    // this system has a way to be missed — an app that closes to background
    // without a close event (Steam; see appQuirks.js `quitCommand`), a launch
    // that times out, a daemon restart mid-session — and a missed edge used to
    // wedge the controller against the wrong surface until someone noticed and
    // restarted something. The daemon's handler is idempotent, so re-sending the
    // current truth costs nothing in the steady state and bounds any desync to
    // one tick instead of forever.
    Timer {
        interval: 3000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            inputManager.setShellFocus(root.shellOwnsScreen);
            // Same heartbeat, on purpose: the periodic re-send IS the liveness
            // signal that lets the daemon's /status distinguish "the shell is
            // idle" from "the shell is dead".
            root._pushShellState();
        }
    }

    // #193 launch-overlay state — drives the dedicated Overlay-layer "Launching…"
    // window below. Shown from launchStarted until windowConfirmed (or a safety
    // timeout), so the previous app never bleeds through the launch gap.
    property bool _launchOverlayActive: false
    property string _launchAppName: ""
    property string _launchAppIcon: ""

    // `state` immediately before a local-app launch began, so a failed/timed-out
    // launch restores the correct screen (`idle` if nothing was running, or
    // `appRunning` if a second app was launched over a first one already in the
    // foreground) instead of always bouncing to the home screen.
    property string _preLaunchState: "idle"

    // Emitted on any user activity (controller, keyboard, mouse) so child
    // components can reset their own inactivity timers without referencing IDs
    // across Variants scope boundaries.
    signal userActivityDetected

    // Declarative state machine — ShellRoot doesn't inherit Item,
    // so we host states/transitions on an internal Item wrapper.
    Item {
        id: stateMachine
        state: root.state

        states: [
            State {
                name: "idle"
                PropertyChanges {
                    // restoreEntryValues:false — when idle is entered with the
                    // overlay drawer open (Home-tap over an app, then Home from
                    // the nav), force it closed but do NOT restore the prior
                    // `true` on exit. Without this, launching the next app
                    // restores overlayDrawerOpen=true and the drawer reopens
                    // over the fresh app.
                    restoreEntryValues: false
                    root.overlayDrawerOpen: false
                }
            },
            State {
                name: "launching"
            },
            State {
                name: "streaming"
            },
            State {
                name: "reconnecting"
            },
            State {
                name: "appRunning"
            }
        ]

        transitions: [
            Transition {
                from: "*"
                to: "idle"
            }
        ]
    }

    Process {
        id: streamQuitProc
        onExited: {
            Components.NotificationManager.info("stream", "Stream Session Ended");
        }
    }

    // === Hoisted auto-suspend idle timer (issue #162) ===
    // Lives at the shell root so it is always running, independent of which
    // settings page (if any) is visible. Restarted whenever sleepTimerMinutes
    // changes (folds in the Ea coupled fix from #162).
    //
    // Activity that resets the countdown:
    //   - InputManager intent signals: controllerWake, intentHome, intentHomeTap,
    //     intentHomeHold (global/automation intents — these are the wakeup surface)
    //   - Shell state transitions: returnToShell() (launching / returning to idle)
    //   - Any real keypress: ShellLayout's root Keys.onPressed observer emits
    //     userActivity() for every non-auto-repeat key (D-pad / A / B / arrows /
    //     Enter / Esc) without consuming the event, so ordinary navigation now
    //     keeps the shell awake (#162). Wired via ShellLayout.onUserActivity below.
    // The timer fires only when idle (state === "idle") to avoid suspending
    // during an active stream or app session.

    Components.AutoSuspendController {
        id: autoSuspend
        shellState: root.state
    }

    // #195 idle-inhibit policy — computes WHETHER to assert a Wayland
    // idle-inhibitor (streaming, or appRunning while an MPRIS player is Playing).
    // Drives the dedicated Background-layer inhibitor window near the bottom of
    // this file. Pure/headless (see IdleInhibitController.qml).
    Components.IdleInhibitController {
        id: idleInhibit
        shellState: root.state

        // Second edge for the `shell-state` push above: media start/stop must
        // reach the daemon immediately, not up to a heartbeat late — that window
        // is exactly "Home Assistant suspends the box as playback begins".
        onMediaPlayingChanged: root._pushShellState()
    }

    // Mute a backgrounded native stream app (Steam Remote Play) so its audio
    // doesn't play behind the shell; unmute on refocus / exit. Narrowly scoped
    // to the allowlisted stream classes inside the component. Bound to the same
    // two signals it reasons over: the shell state and the running app's class.
    Components.StreamAudioMuter {
        id: streamAudioMuter
        shellState: root.state
        runningAppClass: appLifecycle.runningAppClass
    }

    function _resetIdleTimer() {
        autoSuspend.resetTimer();
    }

    // #193: backstop so the launch overlay can't get stuck if a window never maps
    // (launch failed silently, app exited instantly, etc.). The window poller hides
    // it the moment the app actually appears, so on the happy path this never
    // fires — it's sized longer than a worst-case cold flatpak launch (~15-20s) so
    // it never pre-empts a slow-but-valid start (which was the 1Password-bleeds-
    // through-for-15s symptom).
    Timer {
        id: launchOverlayTimeout
        interval: 30000
        repeat: false
        onTriggered: {
            root._launchOverlayActive = false;
            // Backstop for the "launching" state flip above: if a window never
            // mapped and appLaunchFailed never fired either (app hung / exited
            // 0 without ever opening a window), don't strand the shell
            // permanently hidden — restore whatever was showing before this
            // launch attempt. Unconditional (see the onLaunchStarted note):
            // `state` is "appRunning" by now regardless, since this timer
            // firing at all means onWindowConfirmed (which stops it) never
            // ran, i.e. this launch never actually produced a window.
            root.state = root._preLaunchState;
        }
    }

    Component.onCompleted: {
        inputManager.grab();
        inputManager.startListening();
    }

    Components.InputManager {
        id: inputManager
        onForceQuitRequested: root.forceQuit()
        onEndSessionRequested: inputManager.endSession()
        onSuspendStreamRequested: {
            if (root.state === "streaming" || root.state === "reconnecting")
                streamManager.suspend();
        }
        onControllerWake: {
            root._resetIdleTimer();
            root.userActivityDetected();
            // Only wake the AV system if the user has enabled wake-on-controller (#130).
            if (root.state === "idle" && Components.SettingsStore.wakeOnController)
                avController.wake();
        }

        // --- Control-surface intents (de-overloaded "home") ---
        // intent:home is the GLOBAL escape (keyboard Super+Escape / automation):
        // always leave the running app, regardless of focus. Fires instantly.
        onIntentHome: {
            root._resetIdleTimer();
            root.userActivityDetected();
            root.returnToShell();
        }

        // intent:home-tap is the gamepad Home neutral (short press). The shell
        // owns the focus, so it decides: over a running app -> toggle the app
        // overlay drawer (the full return-to-shell is Home-hold / intent:home);
        // on the home screen -> toggle the nav drawer (the `menu` action).
        onIntentHomeTap: {
            root._resetIdleTimer();
            root.userActivityDetected();
            if (root.state === "appRunning") {
                root.overlayDrawerOpen = !root.overlayDrawerOpen;
            } else if (root.state === "idle" && root._layout) {
                // Only wake the AV system if the user has enabled wake-on-controller (#130).
                if (Components.SettingsStore.wakeOnController)
                    avController.wake();
                root._layout.toggleMenu();
            }
        }

        // intent:home-hold = reset. Fired by the gamepad Home long-press AND by
        // keyboard Super+Backspace.
        onIntentHomeHold: {
            root._resetIdleTimer();
            root.userActivityDetected();
            root.resetToHome();
        }

        // intent:menu toggles the nav drawer. Fired by bare Super (keyboard),
        // the gamepad Home-tap on the home screen, and the on-screen menu
        // button. Home-screen only: a bare Super press also precedes a
        // Super+<key> chord, so over a running app `menu` is a deliberate no-op
        // (the chord's intent:home/home-hold does the real work, no overlay flash).
        onIntentMenu: {
            root.userActivityDetected();
            if (root.state === "idle" && root._layout)
                root._layout.toggleMenu();
        }

        // intent:settings / intent:power open their panels from the home screen.
        onIntentSettings: {
            root.userActivityDetected();
            if (root.state === "idle" && root._layout) {
                root._layout.openSettings("");
            }
        }
        onIntentPower: {
            root.userActivityDetected();
            if (root.state === "idle" && root._layout) {
                root._layout.powerOverlay.opened = true;
                root._layout.powerOverlay.forceActiveFocus();
            }
        }

        // Deep-link intent handlers — open a specific view in one command.
        // All guarded by root.state === "idle" to match coarse intents.
        onIntentSettingsPage: page => {
            root.userActivityDetected();
            if (root.state === "idle" && root._layout) {
                let ok = root._layout.openSettings(page);
                if (!ok)
                    console.log("shell: unknown settings page deep-link:", page);
            }
        }
        onIntentOverlay: target => {
            root.userActivityDetected();
            if (!root._layout)
                return;
            // The Session QAM opens on the home screen AND over a running local
            // app — it rides the shell's overlay surface (the window maps while
            // it's open, dimming the app via the drawer scrim) and returns to
            // the app on close. The other overlays stay idle-only for now.
            if (target === "session") {
                // Toggle: the View button (and Super+Right) both open and close
                // the QAM — a second press while it's open closes it.
                if (root.state === "idle" || root.state === "appRunning") {
                    if (root._layout.sessionQam.opened)
                        root._layout.sessionQam.close();
                    else
                        root._layout.sessionQam.open();
                }
                return;
            }
            if (root.state === "idle") {
                if (target === "volume")
                    root._layout.volumeOverlay.openAt(null);
                else if (target === "network")
                    root._layout.networkOverlay.openAt(null);
                else
                    console.log("shell: unknown overlay target deep-link:", target);
            }
        }
        onIntentApp: appId => {
            root.userActivityDetected();
            if (root.state === "idle") {
                let apps = root._applications || [];
                let match = null;
                for (let i = 0; i < apps.length; i++) {
                    if (apps[i].wmClass && apps[i].wmClass === appId) {
                        match = apps[i];
                        break;
                    }
                }
                if (match)
                    appLifecycle.checkAndLaunchApp(match);
                else
                    console.log("shell: no app for deep-link:", appId);
            }
        }
    }

    Components.AVController {
        id: avController
        shellState: root.state
    }

    Components.AppLifecycleManager {
        id: appLifecycle
        shellState: root.state
        applications: root._applications
        prewarmApps: Components.SettingsStore.prewarmApps
        onAppLaunched: {
            root.state = "appRunning";
            // #99: short haptic confirmation on a successful app launch.
            if (inputManager)
                inputManager.rumblePulse(120);
        }
        onAppClosed: {
            appLifecycle.runningAppClass = "";
            root.returnToShell();
        }
        // App failed to launch (non-zero exit from the launcher process).
        // Stronger double-ish pulse so the failure is felt, not just logged.
        onAppLaunchFailed: {
            root._launchOverlayActive = false;
            launchOverlayTimeout.stop();
            // Restore whatever was showing before this launch attempt (idle, or
            // appRunning if a second app was launched over a first) rather than
            // always bouncing to home. Unconditional, NOT guarded on
            // `state === "launching"`: launchDesktopApp() fires appLaunched()
            // synchronously right after launchStarted() (see the note below),
            // so by the time a launcher process can actually exit non-zero,
            // `state` has already moved on to "appRunning" — a guard here
            // would never fire and this restore would be dead code.
            root.state = root._preLaunchState;
            if (inputManager)
                inputManager.rumblePulse(250);
        }
        // #193: show the "Launching…" overlay the instant a launch begins, hide
        // it once the window is confirmed mapped (or the safety timeout fires).
        //
        // Also flip `state` to "launching" here, immediately — mirroring
        // `onStreamRequested` below. The exclusive-keyboard-focus shell surface
        // (the PanelWindow further down) is only unmapped when `state` excludes
        // "idle", and Hyprland only applies the `windowrule = fullscreen` effect
        // to a new window if that window also wins initial keyboard focus on the
        // same map. Without this, `state` stayed "idle" for the ENTIRE local-app
        // launch (it only became "appRunning" once the window was confirmed,
        // well after the window had already mapped), so the shell kept exclusive
        // keyboard focus through the exact moment Hyprland decided whether to
        // fullscreen the new window — starving it of focus and silently
        // defeating the windowrule for every local app launch, regardless of
        // class. Flipping state here, before the process is even spawned,
        // releases that focus in time.
        //
        // Note: AppLifecycleManager.launchDesktopApp() fires appLaunched()
        // synchronously, right after launchStarted() in the same call — so
        // `state` collapses "launching" -> "appRunning" within the same tick
        // for a fresh launch, well before the window actually maps. That's
        // harmless for the fix above (both "launching" and "appRunning" are
        // excluded from the shell surface's `visible` binding, so focus is
        // released either way); it just means `state` is never observably
        // "launching" for very long, which is why the failure/timeout
        // restores above and below don't gate on it.
        onLaunchStarted: app => {
            root._launchAppName = (app && app.name) ? app.name : "";
            root._launchAppIcon = (app && app.icon) ? app.icon : "";
            root._launchOverlayActive = true;
            // Don't overwrite `_preLaunchState` if we're already mid-launch
            // (state still "launching") — otherwise an overlapping second
            // launch would capture "launching" itself as the restore target,
            // which is never a valid state to fall back to.
            if (root.state !== "launching")
                root._preLaunchState = root.state;
            root.state = "launching";
            launchOverlayTimeout.restart();
        }
        onWindowConfirmed: {
            root._launchOverlayActive = false;
            launchOverlayTimeout.stop();
        }
    }

    Components.StreamManager {
        id: streamManager
        shellState: root.state
        onStreamStarted: {
            root.state = "streaming";
            // #99: short haptic confirmation on stream launch.
            if (inputManager)
                inputManager.rumblePulse(120);
        }
        onStreamEnded: {
            Components.NotificationManager.info("stream", "Stream Suspended");
            root.returnToShell();
        }
        onStreamSuspended: {
            Components.NotificationManager.info("stream", "Stream Suspended");
            root.returnToShell();
        }
        onStreamCrashed: attempts => {
            root.state = "reconnecting";
            // #99: stronger pulse to signal the stream dropped / is reconnecting.
            if (inputManager)
                inputManager.rumblePulse(250);
        }
        onStreamFailed: {
            root.state = "idle";
            inputManager.grab();
            // #99: stronger pulse on terminal stream failure.
            if (inputManager)
                inputManager.rumblePulse(250);
        }
        onRequestOverlayShow: msg => {
            if (root._layout)
                root._layout.overlay.show(msg);
        }
        onRequestOverlayHide: {
            if (root._layout)
                root._layout.overlay.hide();
        }
        onRequestInputRelease: inputManager.release()
        onRequestInputHandoff: inputManager.handoff()
        onRequestInputGrab: inputManager.grab()
        onSessionConflictDetected: (runningApp, hostName) => {
            if (root._layout) {
                root._layout.sessionDialog.runningApp = runningApp;
                root._layout.sessionDialog.hostName = hostName;
                root._layout.sessionDialog.opened = true;
            }
        }
        onSessionCheckCancelled: {
            root.state = "idle";
            inputManager.grab();
            if (root._layout)
                root._layout.focusHome();
        }
    }

    function forceQuit() {
        streamManager.forceKill();
        if (root.state === "appRunning")
            closeAndReturnToShell();
        appLifecycle.runningAppClass = "";
        root.state = "idle";
        inputManager.grab();
        root._launchOverlayActive = false;
        launchOverlayTimeout.stop();
        if (root._layout) {
            root._layout.overlay.hide();
            root._layout.sessionDialog.opened = false;
            root._layout.navDrawer.opened = false;
            root._layout.closeSettings();
            root._layout.notificationCenter.opened = false;
            root._layout.powerOverlay.opened = false;
            root._layout.focusHome();
        }
    }

    function returnToShell() {
        root.state = "idle";
        inputManager.grab();
        root._resetIdleTimer();
        root._launchOverlayActive = false;
        launchOverlayTimeout.stop();
        if (root._layout) {
            root._layout.overlay.hide();
            root._layout.closeSettings();
            root._layout.powerOverlay.opened = false;
            root._layout.focusHome();
        }
    }

    function closeAndReturnToShell() {
        appLifecycle.closeApp();
        returnToShell();
    }

    // Full reset to a clean home screen. Triggered by intent:home-hold — the
    // gamepad Home-hold and the keyboard Super+Backspace. Over a running app it
    // returns to the shell; on the home screen it dismisses every
    // overlay/drawer and refocuses Home.
    function resetToHome() {
        if (root.state === "appRunning") {
            root.returnToShell();
        } else if (root.state === "idle" && root._layout) {
            root._layout.navDrawer.opened = false;
            root._layout.closeSettings();
            root._layout.notificationCenter.opened = false;
            root._layout.powerOverlay.opened = false;
            root._layout.focusHome();
        }
    }

    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property var modelData
            screen: modelData
            // Map the shell surface over an app while a drawer is open OR still
            // animating, so the close slide-out plays before the window unmaps
            // (otherwise the drawer vanishes in place over an app). Key off the
            // drawers' `.active` bool — NOT `.visible`, which Qt couples to
            // parent-chain visibility and would break this very binding.
            visible: root.shellOwnsScreen

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: root.state === "appRunning" ? "transparent" : Components.Theme.background
            // Overlay layer, NOT the PanelWindow default (Top): Hyprland renders
            // a fullscreen window above the Top layer, and the kiosk model keeps
            // every app window fullscreen (daemon re-asserts `fullscreen 0 set`).
            // On Top, `returnToShell()` over a still-running local app mapped the
            // home screen UNDER fullscreen Steam with exclusive keyboard focus —
            // the user saw the app while the D-pad drove an invisible shell — and
            // the over-app drawers (home-tap overlay drawer / Session QAM) could
            // never display at all. The `visible:` binding above already encodes
            // exactly "the shell should own or share the screen now", so Overlay
            // (which stacks above fullscreen — same reasoning as the
            // screenshot-flash window below) makes that binding truthful in every
            // mapped state; when an app should own the screen the surface is
            // unmapped, so nothing sits above the app. Streams were never
            // affected (escape tears the stream client window down entirely).
            WlrLayershell.layer: WlrLayer.Overlay
            // Exclusive keyboard focus so non-Hyprland-bound keys (arrows,
            // Enter, Esc, etc.) reach focused QML widgets. Without this,
            // the compositor gives keyboard input to whatever non-layer
            // window happens to have focus.
            //
            // Over a running app the surface only maps when a modal overlay
            // (overlay nav drawer / Session QAM) is open — see the `visible:`
            // binding above. Derive keyboardFocus from the SAME `overlayOverApp`
            // boolean that drives the daemon focus flag (root, ~L27) so the two
            // can never desync: in `appRunning`, `!overlayOverApp` is exactly
            // "no overlay open", so the surface re-asserts Exclusive at the
            // moment it maps over the app (a None→Exclusive transition forces a
            // fresh keyboard grab, rather than relying on a stale static
            // request); otherwise the daemon-emitted keyboard events (A→Enter)
            // could still land on the app's surface. In `appRunning` with no
            // overlay the surface is unmapped, so its focus value (None) is
            // moot. Idle and every other mapped state keep the prior Exclusive
            // value unchanged.
            // Derived from the SAME predicate as `visible` above, so "mapped but
            // not focused" is unrepresentable: if the shell is on screen it owns
            // the keyboard. The previous form keyed off `root.state` separately
            // and could leave the surface drawn with focus None — the compositor
            // then kept keyboard focus on the app underneath, and every key
            // (and every daemon-emitted nav key) landed on that app instead of
            // the shell the user was looking at.
            WlrLayershell.keyboardFocus: root.shellOwnsScreen ? WlrKeyboardFocus.Exclusive : WlrKeyboardFocus.None

            Binding {
                target: Components.NotificationManager
                property: "shellVisible"
                value: root.state === "idle"
            }

            Components.ShellLayout {
                id: layout

                // #416: safe-area overscan. The persisted `overscan` percent
                // insets the shell's content rect so nothing lands in the
                // region a TV that overscans crops away. Per-axis on purpose (a
                // percent of each dimension, not of one of them), and measured
                // off `parent` — the window's content item — so the margins
                // never feed back into this item's own size. The exposed border
                // is then the PanelWindow's `color`: Theme.background whenever
                // the shell owns the screen.
                //
                // This is the ONLY consumer of the key. Delete these bindings
                // and the Overscan control on Settings > Display (and the
                // panel's Display group) goes back to reporting an effect that
                // nothing applies — which is what it did until #416. The
                // settings-consumer gate in panel/src/tests.rs names this file
                // for exactly that reason.
                readonly property int overscanX: Math.round(parent.width * Components.SettingsStore.overscan / 100)
                readonly property int overscanY: Math.round(parent.height * Components.SettingsStore.overscan / 100)

                anchors.fill: parent
                anchors.leftMargin: layout.overscanX
                anchors.rightMargin: layout.overscanX
                anchors.topMargin: layout.overscanY
                anchors.bottomMargin: layout.overscanY

                Component.onCompleted: {
                    root._layout = layout;
                    root._layout.focusHome();
                }
                shellState: root.state
                runningWindows: appLifecycle.runningWindows
                pads: inputManager.pads
                runningAppClass: appLifecycle.runningAppClass
                overlayDrawerOpen: root.overlayDrawerOpen
                avSystemOn: avController.systemOn
                onApplicationsChanged: root._applications = applications
                onStreamRequested: target => {
                    root.state = "launching";
                    avController.forceWake();
                    streamManager.launch(target);
                }
                onStreamQuitRequested: target => {
                    let argv = Components.StreamProviders.active.quitArgs(target);
                    if (argv.length > 0) {
                        streamQuitProc.command = argv;
                        streamQuitProc.running = true;
                    }
                }
                onAppLaunchRequested: app => appLifecycle.checkAndLaunchApp(app)
                onAppResumeRequested: (app, address) => appLifecycle.redeliverAndFocus(app, address)
                onAppFocusRequested: (address, windowClass) => appLifecycle.focusByAddress(address, windowClass)
                onAppCloseRequested: address => appLifecycle.closeByAddress(address)
                onReturnToShellRequested: root.returnToShell()
                onUserActivity: {
                    root._resetIdleTimer();
                    root.userActivityDetected();
                }
                onOverlayDrawerClosed: {
                    root.overlayDrawerOpen = false;
                }

                Connections {
                    target: layout.sessionDialog
                    function onResumeRequested() {
                        layout.sessionDialog.opened = false;
                        streamManager.resumeSession();
                    }
                    function onQuitRequested() {
                        layout.sessionDialog.opened = false;
                        streamManager.quitAndRelaunch();
                    }
                    function onCancelled() {
                        layout.sessionDialog.opened = false;
                        streamManager.cancelSessionCheck();
                    }
                }

                // === OLED auto-dim overlay (issue #143) ===
                // Instantiated once per screen window so it covers the full
                // panel. The dim-delay timer inside DimOverlay is independent of
                // shellIdleTimer (suspend path) and reset by any activity:
                //   - Wayland key/gamepad nav: ShellLayout.userActivity → root.userActivityDetected
                //   - Controller daemon wake:  InputManager.controllerWake → root.userActivityDetected
                //   - Gamepad/keyboard intents: InputManager.onIntent* → root.userActivityDetected
                //     (Home/Menu/Settings/Power + deep-links — so nav-only input cannot dim mid-use)
                // Using root.userActivityDetected (a signal on ShellRoot, the QML
                // document root) avoids id-scope problems with Variants delegates.
                Components.DimOverlay {
                    id: dimOverlay
                }

                Connections {
                    target: root
                    function onUserActivityDetected() {
                        dimOverlay.resetDimTimer();
                    }
                }
            }
        }
    }

    // Screenshot-flash overlay (#166). A dedicated layer-shell window on the
    // Overlay layer, so the flash shows even over a fullscreen app or stream:
    // the main shell window is hidden in those states, so a flash parented to it
    // never renders. Mapped ONLY for the duration of a flash — an always-present
    // overlay surface would block Hyprland's direct scanout and hurt game/stream
    // latency. Click-through (empty input mask) and no keyboard focus, so it
    // never intercepts input.
    Variants {
        model: Quickshell.screens

        PanelWindow {
            id: flashWindow
            required property var modelData
            screen: modelData
            visible: false

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: "transparent"
            exclusionMode: ExclusionMode.Ignore
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
            // Empty input mask -> fully click-through; input passes to the app
            // or shell beneath.
            mask: Region {}

            Components.ScreenshotFlash {
                id: screenshotFlash
                anchors.fill: parent
                onFinished: flashWindow.visible = false
            }

            Connections {
                target: inputManager
                function onScreenshotFlash() {
                    flashWindow.visible = true;
                    screenshotFlash.flash();
                }
            }
        }
    }

    // Launching overlay (#193). A dedicated Overlay-layer window, like the
    // screenshot flash: during a launch the main shell surface is unmapped
    // (`visible:false` in appRunning), so an opaque "Launching…" surface parented
    // to it would never render and the previously-open app would bleed through
    // the ~2s window-detect gap. This window is opaque (covers that app), sits on
    // the Overlay layer (above even a fullscreen lingering app), and is mapped
    // only while a launch is in flight. Click-through + no keyboard focus.
    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property var modelData
            screen: modelData
            visible: root._launchOverlayActive

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: "transparent"
            exclusionMode: ExclusionMode.Ignore
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
            mask: Region {}

            Components.LaunchOverlay {
                anchors.fill: parent
                appName: root._launchAppName
                appIcon: root._launchAppIcon
            }
        }
    }

    // Idle-inhibitor window (#195). A dedicated per-screen layer-shell surface
    // whose sole job is to host a Wayland IdleInhibitor while the shell KNOWS
    // playback is active (streaming, or an app with an MPRIS player Playing —
    // see IdleInhibitController). Three deliberate choices:
    //
    //   1. Dedicated window, NOT the main shell surface: the main shell
    //      PanelWindow is UNMAPPED during streaming/appRunning (visible:false —
    //      see its binding ~L498). A Wayland idle-inhibitor is only honored while
    //      its surface is MAPPED, so an inhibitor parented to the main surface
    //      would be inert in exactly the states we need it. This window maps
    //      independently, keyed only on shouldInhibit.
    //   2. Background layer, NOT Overlay: an Overlay-layer surface above a
    //      fullscreen game/stream forces compositing and breaks Hyprland direct
    //      scanout (hurts stream/game latency — see the flash-window warning
    //      ~L616). A Background-layer surface sits BELOW the fullscreen app, is
    //      fully occluded, yet is still a mapped surface the compositor honors
    //      for idle-inhibit — so it preserves scanout.
    //   3. Mapped only while inhibiting (visible: shouldInhibit): when not
    //      inhibiting there is zero extra surface. Kept tiny + inert (a few px in
    //      one corner, transparent, click-through, no keyboard focus) so it never
    //      covers the screen or intercepts input.
    Variants {
        model: Quickshell.screens

        PanelWindow {
            id: idleInhibitWindow
            required property var modelData
            screen: modelData
            visible: idleInhibit.shouldInhibit

            // Anchor to a single corner with a tiny explicit size — do NOT
            // anchor all four edges (that would cover the screen).
            anchors {
                top: true
                left: true
            }
            implicitWidth: 4
            implicitHeight: 4

            color: "transparent"
            exclusionMode: ExclusionMode.Ignore
            WlrLayershell.layer: WlrLayer.Background
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
            // Empty input mask -> fully click-through.
            mask: Region {}

            // Inhibitor tied to this window, active only while the window is
            // mapped (which is only while shouldInhibit — the visible binding).
            IdleInhibitor {
                window: idleInhibitWindow
                enabled: idleInhibitWindow.visible
            }
        }
    }
}

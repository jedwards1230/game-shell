import Quickshell.Io
import QtQuick
import "prewarm.js" as Prewarm
import "appQuirks.js" as AppQuirks
import "launchTrace.js" as LaunchTrace
import "resumeFocus.js" as ResumeFocus
import "windowFilter.js" as WindowFilter

Item {
    id: root

    property string runningAppClass: ""
    property var runningWindows: []
    // Signature of the last published runningWindows; gate reassignment on it
    // so an unchanged poll doesn't rebuild the home row and drop controller focus.
    property string _runningWindowsSig: ""
    property var applications: []
    property string shellState: ""

    // Prewarm key list of apps to silently prewarm at login (#238), bound from
    // SettingsStore in shell.qml. An entry is normally a StartupWMClass; for
    // desktop entries that declare none (e.g. Steam) it is the exec basename —
    // see prewarm.js keyFor().
    property var prewarmApps: []
    // One-shot guard for the login prewarm pass — set once a poll with BOTH a
    // usable window snapshot and a usable process snapshot has decided every
    // candidate. A poll missing either snapshot decides nothing and leaves this
    // false so the next poll retries.
    property bool _prewarmDone: false
    // One-shot guard for the "prewarm decided nothing" diagnostic line, so a
    // persistently failing snapshot logs once rather than on every poll.
    property bool _prewarmUndecidedLogged: false
    // { issued: {key:true} } — owned here, computed by prewarm.js. `issued` is the
    // in-flight dedup for launches WE issue: a key is marked the instant a launch
    // is dispatched, so a window/process that takes seconds to appear can never be
    // double-launched.
    property var _prewarmState: ({
            issued: ({})
        })
    // Staggered launch queue: apps resolved by _runPrewarm, dequeued one at a
    // time by prewarmStagger to avoid a thundering herd of cold starts (#238).
    property var _prewarmQueue: []

    // Last active-window class reported by the daemon's `hypr:activewindow`
    // subscribe event (empty when no window is focused). Mirrors the compositor's
    // focus state without an extra query.
    property string activeWindowClass: ""
    // Last fullscreen state reported by the daemon's `hypr:fullscreen` event.
    property bool activeWindowFullscreen: false

    property var _prelaunchClasses: []
    // Addresses of windows present at snapshot time, used for address-novelty
    // comparison when an openwindow event arrives (#203).
    property var _prelaunchAddresses: []
    property var _pendingApp: null
    property var _launchedApps: Object.create(null)
    property int _maxMisses: 3
    // Address of the window currently tracked as the foreground app; set when an
    // openwindow event confirms a launch, and cleared on appClosed / return to
    // shell.  A stale address here must not suppress future launches.
    property string _foregroundAddress: ""

    // The resumeFocus.js decision for the focus dispatch currently in flight,
    // held across the async gap so the post-dispatch verification knows what we
    // were AIMING at (#347). Null when no resume is in flight.
    property var _pendingFocusDecision: null

    // The decision waiting on the `hypr-monitors` read that precedes a resume's
    // focus dispatch (workspace consolidation). Distinct from
    // `_pendingFocusDecision` above because they cover different async gaps:
    // this one spans decision → workspace read → focus DISPATCH, that one spans
    // dispatch → verification. Collapsing them into one property would make a
    // verification arriving late for one resume clear the pending state of the
    // next, which is the class of bug this path keeps producing.
    property var _pendingResumeDecision: null

    // Monotonic id for the resume currently allowed to reach a conclusion.
    // Bumped by every focusByAddress; each decision is stamped with the value
    // live at its dispatch, and every async hop below drops its work once the
    // stamp goes stale (resumeFocus.stamp / isStale). Without it, resuming two
    // apps in quick succession lets the FIRST resume's verification judge itself
    // against compositor state the SECOND resume produced — which reads as a
    // focus miss and, since a miss now recovers, bounces the user to the shell
    // mid-resume.
    property int _resumeGeneration: 0

    // True between a launch being initiated and its window being confirmed
    // mapped — gates windowConfirmed so it fires exactly once per launch and not
    // on every subsequent poll (#193).
    property bool _awaitingWindow: false
    // wmClass of the app currently being launched, tracked while
    // `_awaitingWindow` so a live `hypr:activewindow` event can confirm the
    // launch the moment that class becomes active — the fallback for apps
    // whose window was already mapped before this launch (a single-instance
    // app re-invoked via deep-link never produces a "new" window, so the
    // window-appear detectors below can't confirm it; an activewindow event
    // naming the same class can).
    property string _pendingLaunchClass: ""

    // One-shot guard for the STARTUP idle-adoption pass in windowPoller (below).
    // A quickshell restart mid-session boots the state machine `idle` even when
    // an app is already live and focused — the daemon's presenter keeps
    // emulating gamepad input over it, so the controller is locked inside an app
    // the shell doesn't know it's hosting. The shell's escape contract only
    // works in `appRunning` (see shell.qml: overlayOverApp / onIntentHomeTap /
    // resetToHome all gate on it), so the shell MUST adopt that foreground app.
    // This flag makes the poller do that boot-under-app adoption exactly ONCE,
    // on the first idle poll — never as a steady-state re-adopt: a deliberate
    // return-to-shell (returnToShell) leaves the app running in the background
    // AND still the compositor's active window, so a repeating poll adopt would
    // bounce the user straight back into the app they just escaped. Ongoing
    // out-of-band focus adoption is instead event-driven (the hypr:activewindow
    // handler), which a deliberate escape never re-triggers.
    property bool _startupAdoptionDone: false

    signal appLaunched
    signal appClosed
    // Emitted when the launcher process exits non-zero (app failed to start).
    // shell.qml uses this for an error haptic (#99); the failure is also logged.
    signal appLaunchFailed
    // #193: emitted the moment a local app launch is initiated (carries the app
    // so the launch overlay can show its name/icon) and once the launched
    // window is confirmed mapped (so the overlay can hide).
    signal launchStarted(var app)
    signal windowConfirmed
    // A resume whose focus dispatch was VERIFIED not to have landed. Distinct
    // from `appClosed` on purpose even though shell.qml recovers from both the
    // same way: `appClosed` asserts the app is gone, and firing it here would
    // publish that claim about an app that is still very much running (recents,
    // adoption and the close path all read it). This says only "the resume did
    // not take — put the shell back up", which is all we actually know.
    signal resumeFailed

    // Fire windowConfirmed exactly once per in-flight launch.
    function _confirmWindow() {
        if (root._awaitingWindow) {
            root._awaitingWindow = false;
            root._pendingLaunchClass = "";
            root.windowConfirmed();
        }
    }

    // Shell-side ADOPTION of a focused external app (no daemon protocol change).
    //
    // The full escape contract in shell.qml is gated on `state === "appRunning"`:
    // Meta HOLD -> intent:home-hold -> resetToHome -> returnToShell (grab +
    // focusHome), and Meta TAP over an app -> the controllable overlay drawer via
    // onIntentHomeTap -> overlayOverApp -> setOverlayFocus(true). BOTH are no-ops
    // while the shell is `idle`. But the daemon's follow-focus can put a focused
    // external app (e.g. Plex, class `tv.plex.Plex`) under the input presenter —
    // emulating keyboard/mouse from the gamepad — for an app the shell never
    // launched: (a) an app focused out-of-band of the shell's own launch flow, or
    // (b) a quickshell restart mid-session under an already-running app. In both
    // the shell stays `idle`, so NEITHER tap nor hold escapes and the user is
    // locked inside the app with no controller path back.
    //
    // Adopting closes that gap WITHOUT any daemon/protocol change: set the SAME
    // `runningAppClass` the launch flow sets and fire the SAME `appLaunched()`
    // signal (shell.qml onAppLaunched: state = "appRunning"). Once appRunning +
    // runningAppClass is set, the existing escape + overlay-drawer contract just
    // works, and the existing teardown path (the poller's appClosed check below,
    // which keys off runningAppClass, plus the `_maxMisses` disappearance
    // handling) returns the shell to idle when the adopted window goes away — an
    // adopted app is indistinguishable from a launched one to those paths.
    //
    // Guardrails: only adopt while `idle` (idempotent no-op otherwise, so an
    // event-then-poll double fire can't churn), and skip the shell's own surfaces
    // (empty / "quickshell") and compositor notices, plus prelaunch/transient
    // classes — the SAME filters the launch-detect scans use, not a reinvented
    // set (windowFilter.js owns the shared half).
    function _maybeAdoptIdleApp(cls) {
        if (root.shellState !== "idle")
            return;
        if (!WindowFilter.isAppWindow(cls))
            return;
        if (root._prelaunchClasses.indexOf(cls) >= 0)
            return;
        root.runningAppClass = cls;
        // Drive the exact launch-flow signal path so state -> "appRunning".
        appLaunched();
    }

    // === The single choke point for every app-launch shell-out ===
    //
    // All three `hyprctl dispatch exec` paths (the foreground `[fullscreen]`
    // launch, the `[silent]` prewarm, and the rule-less single-instance
    // redelivery) dispatch through here, so the journal names WHICH path issued a
    // given launch and with WHICH window rule — see launchTrace.js. Every launch
    // in the shell arriving at the compositor via one function also means the next
    // such question costs one log field, not a fresh instrumentation pass.
    //
    // This is PURELY OBSERVATIONAL. It builds the exact command each call site
    // built inline, logs immediately before starting the process, and never
    // decides whether to launch — that stays with the callers.
    //
    // `rule` is the Hyprland exec-rule prefix ("[fullscreen]" / "[silent]") or ""
    // for a rule-less dispatch; `execArg` is the app's exec line.
    function _dispatchExec(proc, origin, rule, app, execArg) {
        proc.command = ["hyprctl", "dispatch", "exec", rule === "" ? execArg : rule + " " + execArg];
        LaunchTrace.logExec(origin, rule, (app && app.name) || "", (app && app.wmClass) || "", WindowMatcher.execBasename(execArg), execArg);
        proc.running = true;
    }

    function launchDesktopApp(app) {
        runningAppClass = "";
        // Clear any stale foreground address from a previous launch so it can't
        // suppress openwindow matching for this new launch (#203).
        root._foregroundAddress = "";
        // #193: this is the ONLY true fresh-launch path — show the launch overlay
        // here, not in checkAndLaunchApp, so resuming an already-running app (the
        // focus-existing-window path) never flashes the overlay.
        root._awaitingWindow = true;
        root._pendingLaunchClass = (app.wmClass || "").toLowerCase();
        root.launchStarted(app);
        snapshotClients.running = true;
        appRunner._appName = app.name || "";
        // Launch-time atomic placement: the `[fullscreen]` exec-rule prefix makes
        // Hyprland map the app's first window fullscreen from the start, before
        // any event round-trips — nothing to correct post-hoc. This is the
        // primary kiosk fullscreen guarantee for a fresh launch; the static
        // `windowrule = fullscreen` + the daemon's openwindow backstop remain as
        // defense-in-depth. Exec-rule syntax verified against Hyprland
        // src/config/supplementary/executor/Executor.cpp (`args[0] == '['`).
        _dispatchExec(appRunner, "launch", "[fullscreen]", app, app.exec || app.name);
        detectNewWindow.restart();

        // Track launched app for resilient window matching
        let key = (app.wmClass || app.name || "").toLowerCase();
        if (key !== "") {
            let tracked = _launchedApps;
            tracked[key] = {
                app: app,
                misses: 0,
                windowClass: ""
            };
            _launchedApps = tracked;
        }

        // A foreground launch satisfies any prewarm entry for the same app —
        // record it so the prewarm pass can't launch a second copy in the gap
        // before this one's process and window actually appear.
        root._markPrewarmIssued(app);

        appLaunched();
    }

    // Mark `app`'s prewarm key as already-launched for this session, so no
    // prewarm pass can dispatch a duplicate while its window is still mapping.
    function _markPrewarmIssued(app) {
        let key = Prewarm.keyFor(app, WindowMatcher);
        if (key === "")
            return;
        let st = root._prewarmState;
        st.issued[key] = true;
        root._prewarmState = st;
    }

    function checkAndLaunchApp(app) {
        _pendingApp = app;
        windowQuery.running = true;
    }

    function closeApp() {
        if (runningAppClass !== "") {
            closeAppWindow.appClass = runningAppClass;
            closeAppWindow.running = true;
        }
    }

    function closeAppByClass(windowClass) {
        if (windowClass && windowClass !== "") {
            closeAppWindow.appClass = windowClass;
            closeAppWindow.running = true;
        }
    }

    function focusApp(windowClass) {
        runningAppClass = windowClass;
        // Resuming an existing window (not a fresh launch): clear in-flight launch
        // tracking so a delayed closewindow for a PRIOR launch's address can't
        // fire appClosed() on this app (#203). No address is known here, so the
        // poll fallback handles this window's eventual close.
        root._foregroundAddress = "";
        root._awaitingWindow = false;
        // A deliberate class-targeted focus (not an address-miss fallback), so
        // no `reason` — but it still gets the same post-dispatch fullscreen
        // assertion + landing verification as every other resume (#347).
        root._pendingFocusDecision = {
            mode: ResumeFocus.MODE_CLASS,
            address: "",
            windowClass: windowClass,
            reason: ""
        };
        focusWindow.windowClass = windowClass;
        focusWindow.running = true;
        appLaunched();
    }

    // Address-based focus/close for the per-window home cards. Each running
    // card carries its Hyprland window address, so we target that exact window
    // instead of the first one matching a class.
    //
    // `windowClass` is OPTIONAL and is the resume path's SAFETY NET (#347).
    // Our `runningWindows` snapshot is a poll up to a few seconds old, so an
    // address that isn't in it usually means WE are stale — not that the app is
    // gone. This used to `return` silently on that miss: no focus, no launch, and
    // (worst of all) no log, which is a large part of why #347 took four
    // hypotheses to corner. Callers that hold the row's class now pass it so the
    // miss degrades to a class-targeted focus instead of vanishing.
    function focusByAddress(address, windowClass) {
        // The decision itself is pure and headlessly tested (resumeFocus.js) —
        // this function only carries it out.
        let decision = ResumeFocus.resolve(address, windowClass, runningWindows);

        if (decision.mode === ResumeFocus.MODE_NONE) {
            // The former silent `return`. There is genuinely nothing to focus
            // (unknown address AND no class), but that is a FINDING, not a
            // no-op — log it so the next occurrence costs a grep, not a
            // four-hypothesis investigation.
            LaunchTrace.logResume(decision.mode, decision.address, decision.windowClass, decision.reason);
            return;
        }

        // Claim this resume's generation BEFORE any state is published, so a
        // resume started while an earlier one is still in flight immediately
        // invalidates every hop the earlier one has yet to run.
        root._resumeGeneration = root._resumeGeneration + 1;
        ResumeFocus.stamp(decision, root._resumeGeneration);

        runningAppClass = decision.windowClass;
        // Only an address we actually resolved may be tracked as the foreground
        // window. On the class fallback we deliberately leave it empty (as
        // focusApp does): claiming an address we could not verify would let a
        // later closewindow for it fire appClosed() on the wrong app (#203).
        root._foregroundAddress = (decision.mode === ResumeFocus.MODE_ADDRESS) ? decision.address : "";
        root._awaitingWindow = false;
        root._pendingFocusDecision = decision;

        if (decision.mode !== ResumeFocus.MODE_ADDRESS) {
            // Falling back off the precise path is worth a line: it means the
            // window model the UI rendered from no longer matches the compositor.
            LaunchTrace.logResume(decision.mode, decision.address, decision.windowClass, decision.reason);
        }

        // CONSOLIDATE BEFORE FOCUSING. `dispatch focuswindow` does not reliably
        // follow across workspaces (hyprwm/Hyprland#1611), so if the target has
        // drifted off the displayed workspace, focusing it changes nothing that
        // is on screen — and because the shell unmaps on `appLaunched()` below,
        // "nothing on screen" is a BLACK TV, not a visible no-op. Read the
        // displayed workspace first and pull the window onto it; the decision is
        // pure and tested (resumeFocus.resolveWorkspaceMove).
        //
        // The focus dispatch is therefore deferred into the probe's reply
        // handler rather than issued here. On ANY failure to establish the
        // workspaces the handler focuses immediately anyway, so the worst case is
        // exactly the pre-change behaviour plus one local-socket round trip.
        //
        // ORDERING IS LOAD-BEARING — `appLaunched()` MUST stay ahead of the move.
        // Consolidation deliberately raises the number of toplevels sharing one
        // workspace, which is the exact condition the tiler splits on, so it
        // depends on the daemon's kiosk backstop re-fullscreening the moved
        // window (`movewindowv2` -> `enforce_active_fullscreen`, hyprland.rs).
        // That backstop is GATED: `kiosk_may_enforce` refuses to act while
        // `shell-focus` is on, because the shell is a layer-shell surface and
        // Hyprland's `activewindow` would still name a backgrounded app.
        //
        // `appLaunched()` is what closes that gate: it drives shell.qml's
        // `shellOwnsScreen` false, which pushes `shell-focus off` synchronously
        // in this same tick — whereas the move below cannot happen until an async
        // socket reply arrives. So the gate is already open when `movewindowv2`
        // fires. Move `appLaunched()` after the focus dispatch (a tempting way to
        // keep the shell up over the transition) and the gate is still CLOSED
        // when the move lands: the backstop skips, and two tiled windows sit
        // there until the verified assert 400ms later. That is a visible split
        // view — incident 1 in KIOSK_WINDOW_MODEL.md, reintroduced.
        root._pendingResumeDecision = decision;
        workspaceProbe.request("hypr-monitors");
        appLaunched();
    }

    // Carry out a resolved decision's focus dispatch. Split out of
    // focusByAddress so the workspace-consolidation step can sit between the
    // decision and the dispatch without duplicating the selector branch, and so
    // every path that gives up on consolidating still lands in ONE place.
    function _dispatchResumeFocus(decision) {
        // Stale check here too, not only at the call sites: this is the single
        // choke point every consolidation path funnels through, so guarding it
        // means a hop added later cannot reintroduce the race by forgetting to.
        if (!decision || ResumeFocus.isStale(decision, root._resumeGeneration))
            return;
        if (decision.mode === ResumeFocus.MODE_ADDRESS) {
            focusWindowAddr.addr = decision.address;
            focusWindowAddr.running = true;
        } else {
            focusWindow.windowClass = decision.windowClass;
            focusWindow.running = true;
        }
    }

    // Reads `hypr-monitors` to learn which workspace is actually on screen, then
    // consolidates the resume target onto it before focusing.
    SocketClient {
        id: workspaceProbe
        onResponseReceived: line => {
            let decision = root._pendingResumeDecision;
            root._pendingResumeDecision = null;
            // A newer resume has taken over: its own probe is already in flight
            // and will consolidate + focus ITS target. Consolidating this one's
            // would move a window the user has since navigated away from.
            if (!decision || ResumeFocus.isStale(decision, root._resumeGeneration))
                return;
            let monitors = [];
            try {
                monitors = JSON.parse(line) || [];
            } catch (e) {
                // A malformed reply establishes nothing, so it must not produce a
                // move — an empty list is the fail-safe input that makes
                // resolveWorkspaceMove return move:false.
                monitors = [];
            }
            let plan = ResumeFocus.resolveWorkspaceMove(decision, monitors, root.runningWindows);
            if (!plan.move) {
                root._dispatchResumeFocus(decision);
                return;
            }
            // Move first, focus in its onExited — sequencing them matters:
            // Hyprland applies a workspace move asynchronously, and a focus
            // dispatched alongside it can resolve against the window's OLD
            // placement. The move dispatcher also focuses what it moved, so the
            // follow-up focus is usually redundant; it is kept because "usually"
            // is not the standard the rest of this path is held to, and because
            // the verification below is gated on having dispatched a focus we can
            // name.
            LaunchTrace.logWorkspaceMove(plan.address, plan.workspace);
            moveToWorkspace.pending = decision;
            moveToWorkspace.command = ["hyprctl", "dispatch", "movetoworkspace", ResumeFocus.workspaceSelector(plan.workspace) + ",address:" + plan.address];
            moveToWorkspace.running = true;
        }
        onRequestFailed: {
            let decision = root._pendingResumeDecision;
            root._pendingResumeDecision = null;
            // Can't read the displayed workspace, so we cannot know whether a
            // move is needed OR where to move to. Focus anyway: that is the
            // behaviour this path had before consolidation existed, and a resume
            // that might work beats one that certainly does not.
            root._dispatchResumeFocus(decision);
        }
    }

    Process {
        id: moveToWorkspace
        property var pending: null
        command: ["true"]
        onExited: {
            // Deliberately NOT gated on the exit code. `hyprctl dispatch` exits 0
            // whether or not its selector matched (the same property that made
            // #347 invisible), so the code carries no information — and a move
            // that silently did nothing is precisely the case that still needs
            // the focus attempt. The `hypr-active` verification below is what
            // actually reports whether the resume landed.
            let decision = moveToWorkspace.pending;
            moveToWorkspace.pending = null;
            root._dispatchResumeFocus(decision);
        }
    }

    // Window class of a currently-known running window, or "" when the address is
    // unknown. Used to resolve a live window back to its desktop entry.
    function _windowClassForAddress(address) {
        for (let i = 0; i < runningWindows.length; i++) {
            if (runningWindows[i].address === address)
                return runningWindows[i].windowClass || "";
        }
        return "";
    }

    // Quit the app owning `address`. Closing the window is the default and is a
    // real quit for most apps — but some close to background instead (see
    // appQuirks.js), so those declare an explicit quit command there and we run it
    // rather than dispatching closewindow. No per-app branching lives here.
    //
    // `app` is OPTIONAL: callers that already hold the desktop entry pass it and
    // skip the lookup. Callers that only have an address (every UI close path
    // today — the drawer, HomeScreen, and LibraryScreen resume rows all carry a
    // window, not an app) get the app resolved from the window snapshot, so they
    // pick up quirks for free without a signal-signature change.
    function closeByAddress(address, app) {
        if (!address || address === "")
            return;
        let cmd = app ? AppQuirks.quitCommandFor(app, WindowMatcher) : AppQuirks.quitCommandForWindow(_windowClassForAddress(address), applications, WindowMatcher);
        if (cmd && cmd.length > 0) {
            // The strategy IS the quit — do NOT also dispatch closewindow. If the
            // command fails to run we fall back to the window close below (see
            // quitAppProc.onExited), so the action can never become a silent no-op.
            quitAppProc.addr = address;
            quitAppProc.command = cmd;
            quitAppProc.running = true;
            return;
        }
        closeWindowAddr.addr = address;
        closeWindowAddr.running = true;
    }

    // Resume an app that's ALREADY running at a known address while ALSO
    // re-delivering its launch command — for single-instance apps (e.g. Steam)
    // where invoking the app again is how a deep-link (steam://) navigates the
    // running instance rather than spawning a new window. Mirrors
    // focusByAddress (the "recent apps" Focus action) but additionally fires
    // the exec first, so a deep-link to an already-running instance both
    // navigates AND raises the window in one call — no waiting on a new-window
    // event that a single-instance app will never produce.
    function redeliverAndFocus(app, address) {
        if (app && app.exec) {
            // NOTE (diagnostic, behaviour unchanged): this dispatch carries NO
            // exec-rule prefix, so a window it maps is NOT placed fullscreen at
            // map time — it logs as `rule=none`, distinguishing it in the journal
            // from the `[fullscreen]` and `[silent]` paths.
            _dispatchExec(redeliverProcess, "redeliver", "", app, app.exec);
        }
        // Hand the app's class through as the resume fallback (#347): this is the
        // single-instance path (Steam), so if our window snapshot has gone stale
        // the class still resolves the live window.
        focusByAddress(address, (app && app.wmClass) || "");
    }

    onShellStateChanged: {
        if (shellState === "idle") {
            if (!windowPoller.running)
                windowPoller.running = true;
        }
    }

    Process {
        id: closeAppWindow
        property string appClass: ""
        command: ["hyprctl", "dispatch", "closewindow", "class:" + appClass]
    }

    Process {
        id: closeWindowAddr
        property string addr: ""
        command: ["hyprctl", "dispatch", "closewindow", "address:" + addr]
    }

    // Runs an app's declared quit command (appQuirks.js). `command` is assigned
    // imperatively by closeByAddress, so no binding is declared here. A non-zero
    // exit means the command could not do its job (binary missing, app already
    // gone) — fall back to the plain window close so "Quit App" still does
    // something rather than silently failing.
    Process {
        id: quitAppProc
        property string addr: ""
        onExited: exitCode => {
            if (exitCode !== 0 && quitAppProc.addr !== "") {
                console.warn("[AppLifecycle] quit command exited", exitCode, "- falling back to closewindow");
                closeWindowAddr.addr = quitAppProc.addr;
                closeWindowAddr.running = true;
            }
        }
    }

    // Fire-and-forget exec redelivery for redeliverAndFocus() above — a plain
    // one-shot dispatch, no exit-code handling needed (the focusByAddress call
    // that follows it owns the actual focus/appRunning transition).
    Process {
        id: redeliverProcess
    }

    // Fire-and-forget background prewarm launcher (#238). Uses the `[silent]`
    // exec-rule prefix so Hyprland opens the window WITHOUT focusing it — the app
    // starts in the background, never entering the foreground launch state machine
    // (no launchStarted/overlay/appLaunched/recents). A non-zero exit is logged but
    // does NOT fire the failure haptic (prewarm is silent by construction).
    Process {
        id: prewarmRunner
        property string _appName: ""
        command: ["echo"]
        onExited: exitCode => {
            if (exitCode !== 0)
                ErrorLog.log("app", "Failed to prewarm " + (_appName || "application"), "Command: " + prewarmRunner.command.join(" ") + "\nExit code: " + exitCode, _appName);
        }
    }

    // Process-table snapshot for prewarm dedup (#238 follow-up). One cheap call;
    // `-eo comm=` lists every process's NAME ONLY, with no header and, crucially,
    // no arguments — matching over a full cmdline for "steam" would also hit
    // steamwebhelper / srt-logger / pv-adverb / steam-runtime-launcher-service
    // and silently suppress a legitimate prewarm forever. prewarm.js compares
    // these names EXACTLY (and against the 15-char kernel truncation).
    Process {
        id: prewarmProcScan
        // The window snapshot this scan is being paired with, held across the
        // async gap so both halves describe the same moment.
        property var clients: []
        command: ["ps", "-eo", "comm="]
        stdout: SplitParser {
            property var collected: []
            onRead: line => {
                let name = line.trim();
                if (name !== "")
                    collected.push(name);
            }
        }
        onExited: exitCode => {
            let names = prewarmProcScan.stdout.collected;
            prewarmProcScan.stdout.collected = [];
            let clients = prewarmProcScan.clients;
            prewarmProcScan.clients = [];
            // A failed `ps` yields no usable list — hand null through so the
            // decision is skipped entirely rather than made on bad data.
            root._evaluatePrewarm(clients, exitCode === 0 ? names : null);
        }
    }

    // Launch `app` SILENTLY in the background (#238). The `[silent]` exec-rule
    // prefix is the proven production-hack incantation: it opens the window
    // UNFOCUSED so it never steals focus or enters the foreground path. This is a
    // PURE background exec — it deliberately does NOT emit launchStarted/appLaunched,
    // set _awaitingWindow/_pendingLaunchClass/runningAppClass, snapshot clients, or
    // touch _launchedApps.
    function prewarmApp(app) {
        if (!app)
            return;
        // Belt-and-braces: evaluate() already marked this key issued before it
        // reached the queue, but prewarmApp is public, so re-mark here too.
        root._markPrewarmIssued(app);
        prewarmRunner._appName = app.name || "";
        _dispatchExec(prewarmRunner, "prewarm", "[silent]", app, app.exec || app.name);
    }

    // Login prewarm trigger (#238), driven from the first idle poll AFTER the
    // startup-adoption pass (see the windowPoller wiring for the ordering
    // rationale). Mapped windows alone are NOT enough to dedup against: an app
    // launched out-of-band moments earlier has no window for 10-15s (a Plex HTPC
    // cold start), which is how prewarm used to launch a second copy. So this
    // takes a process-table snapshot to pair with the window list, and defers the
    // decision to _evaluatePrewarm below. `clients` MUST be a real array — a poll
    // error gives us no window list, so that poll decides nothing and we retry.
    function _runPrewarm(clients) {
        if (root._prewarmDone)
            return;
        let apps = root.applications || [];
        let list = root.prewarmApps || [];
        if (apps.length === 0 || list.length === 0)
            return;
        if (!Array.isArray(clients))
            return;
        // A scan from the previous poll is still in flight — let it finish rather
        // than restarting the Process and losing its half-collected output.
        if (prewarmProcScan.running)
            return;
        prewarmProcScan.clients = clients;
        prewarmProcScan.running = true;
    }

    // Second half of the prewarm trigger, invoked once the process scan returns.
    // The decision logic itself lives in the pure, headless-tested prewarm.js.
    // `procNames` is null when the scan failed — evaluate() then decides nothing,
    // because a missing process list is NOT evidence that nothing is running and
    // acting on it is exactly how a double launch happens.
    function _evaluatePrewarm(clients, procNames) {
        if (root._prewarmDone)
            return;
        let res = Prewarm.evaluate(root.prewarmApps || [], root.applications || [], clients, procNames, root._prewarmState, WindowMatcher);
        if (!res.decided) {
            // Bad snapshot — retry on the next poll. Logged ONCE per shell
            // process: a repeating `ps`/client-list failure is a real fault worth
            // seeing, but the poll retries every few seconds and this must stay
            // low-volume.
            if (!root._prewarmUndecidedLogged) {
                root._prewarmUndecidedLogged = true;
                LaunchTrace.logUndecided(Array.isArray(procNames) ? "no-window-snapshot" : "no-process-snapshot");
            }
            return;
        }
        root._prewarmState = res.state;
        // The prewarm pass decides exactly once per shell process, so this is one
        // line per boot recording what it saw and what it chose — the direct
        // answer to "was that launch prewarm, or something else?".
        LaunchTrace.logDecision((root.prewarmApps || []).length, clients.length, procNames.length, res.launch.map(a => Prewarm.keyFor(a, WindowMatcher)), res.skipped);
        if (res.launch.length > 0) {
            root._prewarmQueue = (root._prewarmQueue || []).concat(res.launch);
            prewarmStagger.start();
        }
        // Window + process dedup is a COMPLETE answer for every candidate — there
        // is nothing left to settle or re-check — so the pass is over.
        root._prewarmDone = true;
    }

    // Dequeues one prewarm app every 600ms (triggeredOnStart → the first fires
    // immediately), stopping itself when the queue drains. The stagger avoids
    // launching every prewarm app at once (a thundering herd of flatpak cold
    // starts); it is NOT a blocking sleep (#238).
    Timer {
        id: prewarmStagger
        interval: 600
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            // If the prior silent launch hasn't exited yet (plausible at login
            // under contention), skip this tick WITHOUT shifting the queue —
            // setting prewarmRunner.running = true on an already-true property is a
            // no-op that would silently drop this entry's launch. Retry next tick.
            if (prewarmRunner.running)
                return;
            let q = root._prewarmQueue || [];
            if (q.length === 0) {
                prewarmStagger.stop();
                return;
            }
            let app = q.shift();
            root._prewarmQueue = q;
            root.prewarmApp(app);
        }
    }

    Process {
        id: focusWindowAddr
        property string addr: ""
        command: ["hyprctl", "dispatch", "focuswindow", "address:" + addr]
        onExited: exitCode => {
            // A failed focuswindow (the target vanished mid-resume) means the app
            // is gone. Note this catches only a HARD failure — `hyprctl dispatch`
            // exits 0 even when its selector matched nothing, so a zero exit is
            // NOT evidence the resume landed; _afterFocusDispatch owns that.
            if (exitCode !== 0 && root.shellState === "appRunning") {
                root._pendingFocusDecision = null;
                root.appClosed();
                return;
            }
            root._afterFocusDispatch();
        }
    }

    // === Kiosk fullscreen assertion for a resume (#347) ===
    //
    // TOGGLE vs SET — the distinction this whole component got wrong once before.
    // #308 removed a client-side `hyprctl dispatch fullscreen 0` from the resume
    // path, and that removal was CORRECT: the bare form is a TOGGLE. Fired at an
    // already-fullscreen window it flips it back OUT of fullscreen, which — with
    // a second app backgrounded — let the tiler split the screen. Because it
    // raced Hyprland's own on_focus_under_fullscreen swap, whether it helped or
    // broke things depended on who won, i.e. it was non-deterministic
    // (docs/KIOSK_WINDOW_MODEL.md, incident 1).
    //
    // `fullscreen 0 set` is the IDEMPOTENT form: it SETS fullscreen state rather
    // than inverting it, so issuing it against a window that is already
    // fullscreen is a no-op instead of a regression. That is precisely why the
    // daemon's own enforcement (`force_fullscreen` / `enforce_active_fullscreen`
    // in daemon/src/hyprland.rs) uses this exact form on every openwindow,
    // closewindow, movewindowv2 and activewindowv2 — this matches that idiom
    // rather than reintroducing a competing one. Two idempotent writers of the
    // same state cannot race into a wrong result the way a toggle and a setter
    // could.
    //
    // WHY QML NEEDS IT AT ALL, given the daemon backstop: prewarmed apps map
    // TILED (the `[silent]` exec rule, #238) while foreground apps map fullscreen.
    // Focusing a tiled window that sits UNDER a fullscreen one changes focus but
    // not what is on screen, so the resume appears to do nothing. The declarative
    // on_focus_under_fullscreen swap plus the daemon's activewindowv2 backstop
    // are supposed to promote it; when they miss, the resumed window is
    // focused-but-invisible and nothing else ever corrects it. This is the
    // resume-path guarantee that closes that gap — and being idempotent, it costs
    // nothing when they did fire.
    //
    // ORDERING REQUIREMENT — THIS DISPATCH MUST NOT BE MOVED EARLIER.
    // `fullscreen 0 set` takes NO window selector: it acts on whatever is ACTIVE
    // when it runs (measured on-device — with nothing active it prints "Window
    // not found" and still exits 0, so it cannot even report having hit the wrong
    // thing). Hyprland applies focus asynchronously, so at the moment the focus
    // Process returns, the active window may still be the PREVIOUS one — firing
    // this there would fullscreen that previous window. In the #347 scenario
    // (resume a tiled Plex while a fullscreen Steam is active) that re-asserts
    // fullscreen on STEAM, i.e. reproduces the bug. Idempotence does not save it:
    // `set` is idempotent in which STATE it applies, not which WINDOW.
    //
    // It is therefore fired ONLY from the verified `hypr-active` read below,
    // where the compositor has confirmed our intended window is the active one —
    // which makes "act on the active window" provably correct rather than a race.
    // The cost is one settle interval before the window goes fullscreen. That
    // delay is the correctness mechanism, not latency to be tuned away.
    Process {
        id: assertFullscreen
        command: ["hyprctl", "dispatch", "fullscreen", "0", "set"]
    }

    // Runs after a focus dispatch returns. It does NOT act — it only schedules the
    // single read-back that both the fullscreen assertion and the landing
    // verification are gated on (see assertFullscreen above for why acting here
    // would be wrong).
    function _afterFocusDispatch() {
        // The stale case returns without arming the timer at all: a superseded
        // resume has nothing left to verify, and re-arming the SHARED timer here
        // would push the newer resume's settle interval out by another 400ms.
        if (!root._pendingFocusDecision || ResumeFocus.isStale(root._pendingFocusDecision, root._resumeGeneration))
            return;
        // Give the compositor one settle interval before reading back. Hyprland
        // applies focus + the on_focus_under_fullscreen swap asynchronously, so
        // querying immediately would report the PREVIOUS active window and cry
        // wolf on every successful resume. This is a single delayed read, NOT a
        // retry loop — one verification is enough to turn an invisible failure
        // into a greppable line, and polling the compositor to death would be a
        // worse bug than the one being fixed.
        focusVerifyTimer.restart();
    }

    Timer {
        id: focusVerifyTimer
        interval: 400
        repeat: false
        onTriggered: {
            if (root._pendingFocusDecision && !ResumeFocus.isStale(root._pendingFocusDecision, root._resumeGeneration))
                activeWindowProbe.request("hypr-active");
        }
    }

    // Reads the daemon's `hypr-active` IPC (docs/IPC_PROTOCOL.md). This one read
    // answers BOTH questions the exit code cannot: did the focus dispatch land,
    // and is it therefore safe to fullscreen "the active window"? Both decisions
    // are pure (resumeFocus.js) and headlessly tested — this handler only carries
    // them out.
    SocketClient {
        id: activeWindowProbe
        onResponseReceived: line => {
            let decision = root._pendingFocusDecision;
            root._pendingFocusDecision = null;
            // THE RACE THIS GUARD EXISTS FOR. A superseded resume must never
            // judge itself against compositor state a NEWER resume produced: the
            // window it aimed at is legitimately no longer active, so verifyFocus
            // would call a correct resume a miss and the recovery below would
            // yank the user out of the app they just switched to. Silent by
            // design — this is not a fault, it is a resume the user replaced, and
            // logging it would train us to ignore real `resume-verify` lines.
            if (!decision || ResumeFocus.isStale(decision, root._resumeGeneration))
                return;
            let active = {};
            try {
                active = JSON.parse(line) || {};
            } catch (e) {
                // A malformed reply is itself a failed verification — fall
                // through with an empty object so it logs rather than throwing.
                active = {};
            }
            // Fullscreen FIRST, gated on the read: only now is the active window
            // known to be the one we aimed at, which is what makes a selectorless
            // `fullscreen 0 set` safe (see assertFullscreen above — a miss here
            // would fullscreen somebody else's window).
            let fs = ResumeFocus.shouldAssertFullscreen(decision, active);
            if (fs.assert && !assertFullscreen.running)
                assertFullscreen.running = true;
            let res = ResumeFocus.verifyFocus(decision, active);
            if (!res.ok) {
                let wanted = decision.mode === ResumeFocus.MODE_ADDRESS ? decision.address : decision.windowClass;
                LaunchTrace.logFocusMiss(decision.mode, wanted, active["class"] || "", res.reason);
                // RECOVER, don't just record. Until now this branch logged and
                // returned — but by this point `appLaunched()` has already put
                // the shell in `appRunning` and UNMAPPED its surface, so a resume
                // that provably did not land leaves the TV showing whatever is
                // beneath: the previous app, or (the reported bug) nothing at
                // all. The user's only escape was the Home button, on a screen
                // giving no indication it would do anything.
                //
                // Consolidation above should make this rare; that is exactly why
                // it is safe to act on. Falling back to the shell is the
                // conservative move — the shell is always renderable, and the
                // resume row is still right there to try again.
                if (root.shellState === "appRunning") {
                    LaunchTrace.logResumeAbandoned(decision.mode, wanted, res.reason);
                    root.resumeFailed();
                }
            }
        }
        onRequestFailed: {
            root._pendingFocusDecision = null;
            // Can't verify — so we also do NOT assert fullscreen: with no read of
            // who is active, a selectorless `fullscreen 0 set` is a guess, and the
            // wrong guess is #347. The daemon's activewindowv2 backstop remains.
            // The resume may well have worked; what failed is our ability to
            // confirm it.
            console.warn("AppLifecycleManager: hypr-active probe failed; resume landing unverified, fullscreen not asserted");
        }
    }

    Process {
        id: appRunner
        property string _appName: ""
        command: ["echo"]
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                let cmd = appRunner.command.join(" ");
                ErrorLog.log("app", "Failed to launch " + (_appName || "application"), "Command: " + cmd + "\nExit code: " + exitCode, _appName);
                root.appLaunchFailed();
            }
        }
    }

    HyprctlClients {
        id: snapshotClients
        onClientsReceived: clients => {
            root._prelaunchClasses = clients.map(c => c["class"]);
            // Also snapshot addresses so openwindow events can check novelty by
            // address rather than by class (#203).
            root._prelaunchAddresses = clients.map(c => c["address"] || "");
        }
        onErrorOccurred: {
            root._prelaunchClasses = [];
            root._prelaunchAddresses = [];
        }
    }

    HyprctlClients {
        id: detectClient
        onClientsReceived: clients => {
            for (let i = 0; i < clients.length; i++) {
                if (root._prelaunchClasses.indexOf(clients[i]["class"]) < 0 && clients[i]["class"] !== "") {
                    root.runningAppClass = clients[i]["class"];

                    // Store discovered window class in _launchedApps
                    let tracked = root._launchedApps;
                    for (let key in tracked) {
                        if (tracked[key].windowClass === "" && WindowMatcher.matchesApp(tracked[key].app, clients[i])) {
                            tracked[key].windowClass = clients[i]["class"];
                            break;
                        }
                    }
                    root._launchedApps = tracked;

                    // New window mapped — hide the launch overlay (#193).
                    root._confirmWindow();
                    break;
                }
            }
        }
    }

    Timer {
        id: detectNewWindow
        interval: 2000
        onTriggered: {
            detectClient.running = true;
        }
    }

    HyprctlClients {
        id: windowQuery
        onClientsReceived: clients => {
            root._handleWindowQueryResult(clients);
        }
        onErrorOccurred: {
            root._handleWindowQueryResult([]);
        }
    }

    Process {
        id: focusWindow
        property string windowClass: ""
        command: ["hyprctl", "dispatch", "focuswindow", "class:" + windowClass]
        onExited: exitCode => {
            // See focusWindowAddr above. A class selector is the WEAKER of the
            // two — `class:` matching nothing still exits 0 — so the
            // _afterFocusDispatch verification matters most on this path.
            if (exitCode !== 0 && root.shellState === "appRunning") {
                root._pendingFocusDecision = null;
                root.appClosed();
                return;
            }
            root._afterFocusDispatch();
        }
    }

    function _handleWindowQueryResult(clients) {
        let app = _pendingApp;
        if (!app)
            return;
        _pendingApp = null;

        for (let i = 0; i < clients.length; i++) {
            if (WindowMatcher.matchesApp(app, clients[i])) {
                root.runningAppClass = clients[i]["class"];
                focusWindow.windowClass = clients[i]["class"];
                focusWindow.running = true;
                appLaunched();
                return;
            }
        }

        launchDesktopApp(app);
    }

    HyprctlClients {
        id: windowPoller
        onClientsReceived: clients => {
            let apps = (root.applications || []);
            let windows = [];
            // Set of window classes currently present — used by the
            // launched-app fast-path below to detect a still-running tracked app
            // without re-running the full WindowMatcher scan.
            let seenClasses = {};
            // One entry PER WINDOW (no class dedup) so the home row can show a
            // card per running window and focus/close each one individually.
            for (let i = 0; i < clients.length; i++) {
                let c = clients[i];
                let cls = c["class"] || "";
                // Not an app window (shell surface / compositor notice) — never
                // enumerate it. See windowFilter.js for the list and the why.
                if (!WindowFilter.isAppWindow(cls))
                    continue;

                seenClasses[cls] = true;

                let iconName = (c["initialClass"] || cls).toLowerCase();
                let appIcon = iconName;
                let appName = c["title"] || cls;

                // Use WindowMatcher for icon/name resolution
                for (let j = 0; j < apps.length; j++) {
                    if (WindowMatcher.matchesApp(apps[j], c)) {
                        appIcon = apps[j].icon || iconName;
                        appName = apps[j].name || appName;
                        break;
                    }
                }

                windows.push({
                    windowClass: cls,
                    address: c["address"] || "",
                    // Workspace NAME (hypr-clients' `workspace` field, which the
                    // daemon reshapes from Hyprland's `workspace.name`). The IPC
                    // has always carried it and this poller has always dropped
                    // it — which is why the resume path had no way to notice a
                    // window had drifted off the displayed workspace. It is the
                    // input to resumeFocus.resolveWorkspaceMove().
                    workspace: c["workspace"] || "",
                    title: c["title"] || cls,
                    name: appName,
                    icon: appIcon,
                    // Hyprland focus order (0 = most recently focused); used to
                    // sort the running cards most-recently-used first.
                    focusHistoryId: (c["focusHistoryId"] !== undefined) ? c["focusHistoryId"] : 9999,
                    exec: ""
                });
            }
            // Only publish when the window set actually changed (class/address/
            // name/icon/focus-order/workspace). `workspace` is in the signature
            // because a window DRIFTING to another workspace changes nothing else
            // about it — leave it out and the resume path keeps consolidating
            // against a stale workspace long after the window moved. The poll fires every few seconds; a blind
            // reassignment rebuilds the home row's delegates and can drop
            // controller focus to nothing (dead stick until the mouse re-anchors).
            let sig = windows.map(function (w) {
                return w.windowClass + "|" + w.address + "|" + w.name + "|" + w.icon + "|" + w.focusHistoryId + "|" + w.workspace;
            }).join(";");
            if (sig !== root._runningWindowsSig) {
                root._runningWindowsSig = sig;
                root.runningWindows = windows;
            }

            // Track miss counts in _launchedApps
            let tracked = root._launchedApps;
            let trackedChanged = false;
            for (let key in tracked) {
                let entry = tracked[key];
                let wc = entry.windowClass;
                let found = false;

                if (wc !== "" && seenClasses[wc]) {
                    found = true;
                } else {
                    // Try matching by app metadata
                    for (let i = 0; i < clients.length; i++) {
                        if (WindowMatcher.matchesApp(entry.app, clients[i])) {
                            found = true;
                            if (wc === "") {
                                entry.windowClass = clients[i]["class"];
                                trackedChanged = true;
                            }
                            break;
                        }
                    }
                }

                if (found) {
                    if (entry.misses > 0) {
                        entry.misses = 0;
                        trackedChanged = true;
                    }
                } else {
                    entry.misses++;
                    trackedChanged = true;
                    if (entry.misses >= root._maxMisses) {
                        delete tracked[key];
                    }
                }
            }
            if (trackedChanged)
                root._launchedApps = tracked;

            // #193: keep scanning for a freshly-launched window that hasn't mapped
            // yet. The one-shot detectNewWindow timer fires once at 2s, so an app
            // slower than that (a cold flatpak launch — Plex HTPC's first start is
            // ~10-15s — sets up the sandbox/runtime before drawing) is missed and
            // runningAppClass stays "", leaving the launch overlay to hide on the
            // fallback timeout before the app actually appears. The poller runs
            // every 2s while appRunning, so adopt the first new non-prelaunch
            // window here: set it as the foreground app and confirm the launch, so
            // the overlay stays up until the window is really on screen.
            if (root._awaitingWindow && root.runningAppClass === "" && root.shellState === "appRunning") {
                for (let i = 0; i < clients.length; i++) {
                    let cls = clients[i]["class"] || "";
                    if (!WindowFilter.isAppWindow(cls))
                        continue;
                    if (root._prelaunchClasses.indexOf(cls) < 0) {
                        root.runningAppClass = cls;
                        root._confirmWindow();
                        break;
                    }
                }
            }

            // One-shot STARTUP idle-adoption (escape contract): the event-driven
            // activewindow adoption above only fires on a focus CHANGE, which a
            // quickshell restart mid-session under an already-running app never
            // produces (the app was focused before the shell even started, so no
            // event arrives). Catch that boot-under-app case from the FIRST idle
            // poll instead — the client list is the source of truth here even
            // with no activewindow event yet. Adopt the current foreground window
            // (lowest Hyprland focusHistoryId == most-recently-focused). `windows`
            // is already stripped of non-app classes (windowFilter.js), and
            // _maybeAdoptIdleApp re-applies every filter. Guarded one-shot by
            // _startupAdoptionDone so this cannot become a steady-state re-adopt:
            // a deliberate return-to-shell leaves the app backgrounded but still
            // the compositor's active window, and a repeating poll adopt would
            // bounce the user right back into it (see _startupAdoptionDone).
            if (!root._startupAdoptionDone && root.shellState === "idle") {
                root._startupAdoptionDone = true;
                let fgClass = "";
                let fgHist = 1000000;
                for (let i = 0; i < windows.length; i++) {
                    if (windows[i].focusHistoryId < fgHist) {
                        fgHist = windows[i].focusHistoryId;
                        fgClass = windows[i].windowClass;
                    }
                }
                if (fgClass !== "")
                    root._maybeAdoptIdleApp(fgClass);
            }

            // Login prewarm (#238) — fire strictly AFTER the one-shot startup
            // adoption above. RATIONALE: on the first idle poll, _startupAdoptionDone
            // runs with NO prewarmed windows present (we haven't launched yet) → it
            // adopts nothing → sets the one-shot done. Only THEN does prewarm launch.
            // A later poll that sees a prewarmed (unfocused) window can't re-trigger
            // adoption (it's one-shot), so a silently-prewarmed background app is
            // never mis-adopted into appRunning. The poll succeeding IS the readiness
            // signal (Hyprland answering + app list loaded) — replacing the deploy
            // hack's crude fixed `sleep 10` — and hands _runPrewarm the live `clients`
            // list, which it pairs with a process-table scan before deciding. Note
            // _runPrewarm is ASYNC (it awaits that scan); the adoption above is
            // synchronous and already finished, so the ordering still holds.
            if (root.shellState === "idle" && !root._prewarmDone && root.applications.length > 0 && root.prewarmApps.length > 0)
                root._runPrewarm(clients);

            // Only fire appClosed when in appRunning state and foreground app is truly gone
            if (root.shellState === "appRunning" && root.runningAppClass !== "") {
                let found = false;
                for (let i = 0; i < windows.length; i++) {
                    if (windows[i].windowClass === root.runningAppClass) {
                        found = true;
                        break;
                    }
                }
                if (found) {
                    // Foreground window is present — confirm the launch (#193).
                    // This is the reliable path for a freshly-launched window
                    // that maps after the one-shot detect timer has fired.
                    root._confirmWindow();
                } else {
                    root._awaitingWindow = false;
                    root.appClosed();
                }
            }
        }
        onErrorOccurred: message => {
            console.warn("AppLifecycleManager: window poll error:", message);
        }
    }

    Timer {
        id: windowPollTimer
        interval: root.shellState === "appRunning" ? 2000 : 5000
        running: root.shellState === "idle" || root.shellState === "appRunning"
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (!windowPoller.running)
                windowPoller.running = true;
        }
    }

    // Subscribe to the daemon's Hyprland window events (hypr:activewindow,
    // hypr:fullscreen — see docs/IPC_PROTOCOL.md) so window open/close/focus
    // changes are reflected immediately instead of waiting for the next poll
    // tick. The periodic windowPoller above remains the source of truth for the
    // runningWindows model and appClosed detection; these events just kick an
    // extra poll on transitions, so the public behavior is unchanged.
    SocketClient {
        id: hyprEventListener
        // Subscribe stream over a native Quickshell socket (SocketClient, #97);
        // filter to `hypr:` lines (the stream also carries high-frequency
        // buttons:/intent:* events we don't want here). Auto-reconnects on drop.
        subscribe: true
        onLineReceived: line => {
            if (line.indexOf("hypr:activewindow:") === 0) {
                root.activeWindowClass = line.substring("hypr:activewindow:".length);
                root._onHyprWindowEvent();
                // Backstop for a launch waiting on a window that will NEVER be
                // "new" — e.g. a single-instance app (Steam) already running
                // before this launch, re-invoked via deep-link. The window-
                // appear detectors (detectClient/windowPoller's prelaunch-
                // novelty check) can't see it since its class predates the
                // launch; the class becoming ACTIVE is just as valid a
                // confirmation.
                if (root._awaitingWindow && root._pendingLaunchClass !== "" && root.activeWindowClass.toLowerCase() === root._pendingLaunchClass) {
                    root.runningAppClass = root.activeWindowClass;
                    root._confirmWindow();
                }
                // Idle-adoption (escape contract): if the shell believes it is
                // idle but a non-shell window just BECAME the compositor's active
                // window, the daemon's follow-focus is (or is about to be)
                // emulating gamepad input over an app the shell never launched.
                // Adopt it into appRunning so the existing escape works (see
                // _maybeAdoptIdleApp). This is the ongoing, event-driven path for
                // out-of-band focus; it is loop-safe because a deliberate return-
                // to-shell does NOT emit a fresh activewindow event naming the app
                // class, so escaping can never re-trigger adoption here.
                root._maybeAdoptIdleApp(root.activeWindowClass);
            } else if (line.indexOf("hypr:fullscreen:") === 0) {
                root.activeWindowFullscreen = line.substring("hypr:fullscreen:".length) === "1";
                root._onHyprWindowEvent();
            } else if (line.indexOf("hypr:openwindow:") === 0) {
                root._onHyprOpenWindow(line.substring("hypr:openwindow:".length));
            } else if (line.indexOf("hypr:closewindow:") === 0) {
                root._onHyprCloseWindow(line.substring("hypr:closewindow:".length));
            }
        }
    }

    function _onHyprWindowEvent() {
        // Kick an immediate poll on window transitions while the shell is the
        // active state owner; the poller itself guards against re-entry.
        if ((root.shellState === "idle" || root.shellState === "appRunning") && !windowPoller.running)
            windowPoller.running = true;
    }

    // Handle a hypr:openwindow event — deterministic ADDRESS-based launch
    // confirmation (#203). Keeps the existing poll/detectNewWindow as fallback.
    //
    // Scope note: full child-PID→window-PID correlation needs moving `exec` into
    // the daemon — out of scope for this PR. This gives deterministic
    // ADDRESS-based correlation for the common case (one in-flight launch) plus
    // keeps the poll fallback for the edge cases.
    //
    // _confirmWindow() is idempotent (no-ops once _awaitingWindow is false), so
    // an event-then-poll double fire is safe.
    function _onHyprOpenWindow(payload) {
        if (!root._awaitingWindow)
            return;
        try {
            var w = JSON.parse(payload);
            var addr = w.address || "";
            // Address-novelty check: only act if this address was not already
            // present in the pre-launch snapshot.
            if (addr === "" || root._prelaunchAddresses.indexOf(addr) >= 0)
                return;

            // Find which tracked app this window satisfies (by WindowMatcher).
            var tracked = root._launchedApps;
            var matched = false;
            for (var key in tracked) {
                if (tracked[key] && tracked[key].app && WindowMatcher.matchesApp(tracked[key].app, w)) {
                    tracked[key].windowClass = w.class || "";
                    matched = true;
                    break;
                }
            }
            // Accept even if no tracked app matched — the window is genuinely
            // new and we were awaiting one.
            root._launchedApps = tracked;
            root.runningAppClass = w.class || root.runningAppClass;
            root._foregroundAddress = addr;
            root._confirmWindow();
        } catch (e) {
            // Malformed JSON payload — log and fall through to the poll fallback.
            console.warn("AppLifecycleManager: malformed hypr:openwindow payload:", e);
        }
        // Kick an extra poll so the runningWindows model and appClosed detection
        // see the new window without waiting for the next timer tick.
        root._onHyprWindowEvent();
    }

    // Handle a hypr:closewindow event — immediate appClosed detection (#203).
    // The poll remains the source of truth for runningWindows; this just fires
    // appClosed earlier when the closed address is the tracked foreground app.
    function _onHyprCloseWindow(address) {
        // Clear a stale foreground address when any window closes so it can't
        // suppress future openwindow launches.
        if (root._foregroundAddress === address) {
            root._foregroundAddress = "";
            if (root.shellState === "appRunning" && root.runningAppClass !== "") {
                root._awaitingWindow = false;
                root.appClosed();
                return;
            }
        }
        // Still kick a poll so runningWindows and the appClosed path in the
        // poller remain consistent.
        root._onHyprWindowEvent();
    }

    Component.onCompleted: {
        hyprEventListener.start();
    }
}

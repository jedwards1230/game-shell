.pragma library

// Pure resume-focus decision logic (#347).
//
// WHY THIS IS A SEPARATE LIBRARY: the resume path is the one place in the shell
// where a wrong decision is INVISIBLE — `hyprctl dispatch` exits 0 even when its
// selector matched no window, so a resume that focuses nothing looks exactly
// like a resume that worked. That made #347 take four hypotheses to corner. The
// decision ("which selector do we focus with, and did it land?") is therefore
// pulled out of AppLifecycleManager into pure functions that can be asserted
// headlessly, so the branch that used to be a silent `return` is now pinned by
// tests rather than by a device.
//
// Mirrors the prewarm.js / resumeModel.js pattern: no QML types, no imports, all
// inputs passed in.

// How a resume should be dispatched.
//   ADDRESS — the address is a window we currently know about; focus that exact
//             window. The precise case, and the only one that may claim the
//             address as the tracked foreground window (#203).
//   CLASS   — the address is NOT in our window snapshot but the caller supplied
//             the window's class. Our snapshot is a poll a few seconds old, so
//             an address miss usually means WE are stale, not that the app is
//             gone; focusing by class still reaches the live window. Strictly
//             better than the silent `return` this replaces.
//   NONE    — nothing actionable: no known address AND no class. The caller must
//             log this rather than return quietly (that silence WAS the bug).
var MODE_ADDRESS = "address";
var MODE_CLASS = "class";
var MODE_NONE = "none";

// Reasons, logged verbatim as the `reason=` trace field.
var REASON_NO_ADDRESS = "no-address";
var REASON_UNKNOWN_ADDRESS = "unknown-address";
var REASON_NO_ACTIVE_WINDOW = "no-active-window";
var REASON_ADDRESS_MISMATCH = "active-address-mismatch";
var REASON_CLASS_MISMATCH = "active-class-mismatch";
var REASON_NO_TARGET = "no-target";
var REASON_ALREADY_FULLSCREEN = "already-fullscreen";

function _s(v) {
    return (v === undefined || v === null) ? "" : String(v);
}

// Decide how to focus a resume request.
//   address        — the Hyprland window address the UI row carried.
//   windowClass    — the row's window class, when the caller has one ("" if not).
//   runningWindows — the current window snapshot (AppLifecycleManager.runningWindows).
//
// Returns { mode, address, windowClass, reason }. `windowClass` on an ADDRESS
// hit comes from the SNAPSHOT (authoritative) rather than the caller's argument.
function resolve(address, windowClass, runningWindows) {
    var addr = _s(address);
    var cls = _s(windowClass);
    var windows = runningWindows || [];

    if (addr !== "") {
        for (var i = 0; i < windows.length; i++) {
            var w = windows[i];
            if (w && _s(w.address) === addr) {
                return {
                    mode: MODE_ADDRESS,
                    address: addr,
                    windowClass: _s(w.windowClass),
                    reason: ""
                };
            }
        }
    }

    var reason = (addr === "") ? REASON_NO_ADDRESS : REASON_UNKNOWN_ADDRESS;
    if (cls !== "") {
        return {
            mode: MODE_CLASS,
            address: addr,
            windowClass: cls,
            reason: reason
        };
    }
    return {
        mode: MODE_NONE,
        address: addr,
        windowClass: "",
        reason: reason
    };
}

// Did a focus dispatch actually land?
//
// EXIT CODE CANNOT ANSWER THIS. `hyprctl dispatch focuswindow class:nope` exits
// 0 and prints "ok" — a selector that matched nothing is indistinguishable from
// a selector that worked. The only real evidence is the compositor's own
// active-window read afterwards (the daemon's `hypr-active` IPC), which is what
// this compares against the decision we acted on.
//
//   decision — the object returned by resolve() above.
//   active   — the parsed `hypr-active` reply: {class,address,fullscreen}, or {}
//              when nothing is focused.
//
// Returns { ok, reason }. Class comparison is case-insensitive because Hyprland
// reports the window's own class casing (`tv.plex.Plex`) while our callers may
// hold a lowercased StartupWMClass.
function verifyFocus(decision, active) {
    var d = decision || {};
    var a = active || {};
    var activeAddr = _s(a.address);
    var activeCls = _s(a["class"]);

    if (d.mode === MODE_ADDRESS) {
        if (activeAddr === "")
            return {
                ok: false,
                reason: REASON_NO_ACTIVE_WINDOW
            };
        var addrOk = activeAddr === _s(d.address);
        return {
            ok: addrOk,
            reason: addrOk ? "" : REASON_ADDRESS_MISMATCH
        };
    }

    if (d.mode === MODE_CLASS) {
        if (activeCls === "")
            return {
                ok: false,
                reason: REASON_NO_ACTIVE_WINDOW
            };
        var clsOk = activeCls.toLowerCase() === _s(d.windowClass).toLowerCase();
        return {
            ok: clsOk,
            reason: clsOk ? "" : REASON_CLASS_MISMATCH
        };
    }

    // Nothing was dispatched, so nothing can have landed.
    return {
        ok: false,
        reason: REASON_NO_TARGET
    };
}

// May the resume path issue `hyprctl dispatch fullscreen 0 set`?
//
// THE ORDERING REQUIREMENT THIS FUNCTION EXISTS TO ENFORCE — do not "optimize"
// it away. Hyprland's `fullscreen` dispatcher takes NO window selector: it acts
// on whatever is ACTIVE at the instant it runs (verified on-device; the daemon's
// own force_fullscreen in daemon/src/hyprland.rs must dispatch `focuswindow
// address:<a>` FIRST for exactly this reason, and `hyprctl dispatch fullscreen 0
// set` prints "Window not found" and still exits 0 when nothing is active). The
// compositor applies focus asynchronously, so at the moment our focus Process
// returns, the active window may STILL BE THE PREVIOUS ONE. Asserting fullscreen
// there would fullscreen that previous window — in the #347 scenario (resume a
// tiled Plex while a fullscreen Steam is active) it would re-assert fullscreen on
// STEAM, which is the very bug this path is meant to fix. `set` being idempotent
// does not save us: it is idempotent in WHICH STATE it applies, not in WHICH
// WINDOW it applies to.
//
// So the assertion is gated on the SAME verified `hypr-active` read that
// verifyFocus consumes. Only once the compositor itself reports our intended
// window as active is "the active window" provably the right target. The cost is
// one settle interval before the window goes fullscreen; the alternative is a
// coin-flip on which window gets fullscreened.
//
// Mirrors the daemon's `needs_fullscreen` skip conditions (something IS focused,
// and it isn't already fullscreen) so QML and the daemon share one idiom rather
// than competing.
//
//   decision — the object returned by resolve().
//   active   — the parsed `hypr-active` reply: {class,address,fullscreen}.
//
// Returns { assert, reason }. `reason` is why we are NOT asserting, "" when we
// are. Fail-safe direction: only an EXPLICIT `fullscreen === true` suppresses the
// dispatch, so a missing/unknown field still asserts (a redundant idempotent
// `set` is harmless; a skipped needed one leaves the window invisible).
function shouldAssertFullscreen(decision, active) {
    var landed = verifyFocus(decision, active);
    if (!landed.ok)
        return {
            assert: false,
            reason: landed.reason
        };
    if ((active || {}).fullscreen === true)
        return {
            assert: false,
            reason: REASON_ALREADY_FULLSCREEN
        };
    return {
        assert: true,
        reason: ""
    };
}

// === Workspace consolidation (the resume black-screen fix) =================
//
// THE BUG THIS EXISTS TO CLOSE. `KIOSK_WINDOW_MODEL.md` states the kiosk
// contract holds "by construction ON A SINGLE WORKSPACE" — every guarantee in
// that document (the fullscreen windowrule, `on_focus_under_fullscreen`,
// `exit_window_retains_fullscreen`, the daemon's idempotent backstop) assumes
// every app window and the displayed workspace are the same workspace. Nothing
// enforced it: there is no `default_workspace`, no workspace windowrule, no
// workspace keybind, and until this change not one `dispatch workspace` /
// `movetoworkspace` call anywhere in the shell or the daemon.
//
// Observed in the field (2026-08-25): Plex on workspace 1, Steam Big Picture on
// workspace 4, and the monitor DISPLAYING workspace 2 — which held no windows at
// all. The shell is a layer-shell surface and draws regardless of workspace, so
// Home looked healthy; the instant a resume unmapped it there was genuinely
// nothing beneath to render and the TV went black. `dispatch focuswindow` could
// not save it either: it does not reliably follow across workspaces
// (hyprwm/Hyprland#1611 — the same issue KIOSK_WINDOW_MODEL.md cites when it
// defers per-app workspace isolation to Phase 2).
//
// CONSOLIDATE, DON'T FOLLOW. The fix moves the target window ONTO the workspace
// already being displayed (`movetoworkspace <active>,address:<addr>`) rather than
// switching the display to wherever the window drifted (`dispatch workspace N`).
// Following would resume correctly but leave the box permanently multi-workspace,
// keeping the active-but-empty workspace reachable forever. Consolidating drains
// stray workspaces back toward one every time the user resumes anything, which
// restores the invariant the rest of the model already depends on instead of
// teaching the shell to live without it.
//
// FAIL-SAFE DIRECTION: every branch that cannot establish BOTH the target's
// workspace and the displayed workspace returns `move: false` — i.e. it degrades
// to exactly the pre-change behaviour (focus alone). A move we are not sure about
// is strictly worse than no move: `movetoworkspace` aimed at the wrong workspace
// would relocate a live window off-screen, which is the failure we are fixing.
var REASON_NO_MONITORS = "no-monitors";
var REASON_NO_ACTIVE_WORKSPACE = "no-active-workspace";
var REASON_UNKNOWN_TARGET_WORKSPACE = "unknown-target-workspace";
var REASON_ALREADY_ON_ACTIVE = "already-on-active";

// The workspace currently on screen, read from a `hypr-monitors` reply.
//
// SINGLE-OUTPUT ASSUMPTION, STATED RATHER THAN ASSUMED: `hypr-monitors` carries
// no `focused` field (daemon/src/hyprland.rs `monitor_entry`), so on a multi-head
// setup there is no way to tell which output has focus. tv-shell is a TV kiosk
// driving one output, so the first monitor IS the display. Rather than guess on a
// multi-head box, an ambiguous read returns "" and the caller skips the move —
// the fail-safe direction above. If multi-head ever matters, add `focused` to
// `monitor_entry` and select on it here; nothing else needs to change.
function activeWorkspaceOf(monitors) {
    var list = monitors || [];
    if (list.length === 0)
        return "";
    var first = list[0] || {};
    return _s(first.activeWorkspace);
}

// Find the window a resume decision targets, so we can read its workspace.
// ADDRESS mode matches the exact window; CLASS mode (our snapshot was stale on
// address, but the class still resolves a live window) takes the first window of
// that class, case-insensitively, which is the same window `dispatch focuswindow
// class:<c>` would land on.
function _targetWindow(decision, runningWindows) {
    var d = decision || {};
    var windows = runningWindows || [];
    var i;
    if (d.mode === MODE_ADDRESS) {
        for (i = 0; i < windows.length; i++) {
            if (windows[i] && _s(windows[i].address) === _s(d.address))
                return windows[i];
        }
        return null;
    }
    if (d.mode === MODE_CLASS) {
        var want = _s(d.windowClass).toLowerCase();
        if (want === "")
            return null;
        for (i = 0; i < windows.length; i++) {
            if (windows[i] && _s(windows[i].windowClass).toLowerCase() === want)
                return windows[i];
        }
    }
    return null;
}

// Should the resume path pull its target window onto the displayed workspace
// before focusing it?
//
//   decision       — the object returned by resolve().
//   monitors       — the parsed `hypr-monitors` reply (array), or [] on failure.
//   runningWindows — the window snapshot, whose entries now carry `workspace`
//                    (the workspace NAME, matching hypr-clients' `workspace`
//                    field and hypr-monitors' `activeWorkspace`).
//
// Returns { move, workspace, address, reason }. `workspace` is the destination
// (the displayed one) and `address` the window to move — both "" when move is
// false. `reason` says why we are NOT moving, "" when we are.
function resolveWorkspaceMove(decision, monitors, runningWindows) {
    var d = decision || {};
    if (d.mode !== MODE_ADDRESS && d.mode !== MODE_CLASS)
        return {
            move: false,
            workspace: "",
            address: "",
            reason: REASON_NO_TARGET
        };

    var list = monitors || [];
    if (list.length === 0)
        return {
            move: false,
            workspace: "",
            address: "",
            reason: REASON_NO_MONITORS
        };

    var activeWs = activeWorkspaceOf(list);
    if (activeWs === "")
        return {
            move: false,
            workspace: "",
            address: "",
            reason: REASON_NO_ACTIVE_WORKSPACE
        };

    var win = _targetWindow(d, runningWindows);
    var winWs = win ? _s(win.workspace) : "";
    if (!win || winWs === "")
        return {
            move: false,
            workspace: "",
            address: "",
            reason: REASON_UNKNOWN_TARGET_WORKSPACE
        };

    if (winWs === activeWs)
        return {
            move: false,
            workspace: "",
            address: "",
            reason: REASON_ALREADY_ON_ACTIVE
        };

    // Always move by ADDRESS, never by class: `movetoworkspace <ws>,class:<c>`
    // would relocate whichever window of that class Hyprland picks first, and
    // with Steam that is routinely the Big Picture window rather than the live
    // Remote Play surface. The snapshot entry we just matched has the exact
    // address even on the CLASS path, so there is no reason to be imprecise.
    return {
        move: true,
        workspace: activeWs,
        address: _s(win.address),
        reason: ""
    };
}

// Format a workspace NAME as a Hyprland workspace selector.
//
// `hypr-clients`/`hypr-monitors` report the workspace *name*. For the ordinary
// numbered workspaces this kiosk uses, the name IS the id ("1", "4"), and
// Hyprland accepts a bare number as an id selector. A named workspace ("games")
// is not a valid bare selector and must go through `name:` — without this,
// `movetoworkspace games,address:0x…` is parsed as an id, fails to match, and
// (per `hyprctl dispatch`'s exit-code behaviour documented above) still exits 0,
// i.e. it would fail exactly as silently as the bug this file exists to fix.
function workspaceSelector(workspace) {
    var ws = _s(workspace);
    if (ws === "")
        return "";
    return /^[0-9]+$/.test(ws) ? ws : ("name:" + ws);
}

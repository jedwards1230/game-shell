.pragma library

.import "prewarm.js" as Prewarm

// Per-app behaviour overrides ("quirks"), keyed by the SAME app identity the
// prewarm engine uses — prewarm.keyFor(): StartupWMClass when the desktop entry
// declares one, else the exec basename. There is exactly one app-identity concept
// in this shell; this module reuses it rather than inventing a second one.
//
// The first quirk is `quitCommand`. Most apps genuinely exit when their window is
// closed (`hyprctl dispatch closewindow`) — Plex does. Some apps treat a window
// close as "minimise to background" and keep running: Steam does (verified on the
// device: the window disappears, but the steam PID and its dozen steamwebhelper
// children survive), and Discord/Spotify behave the same way. For those, closing
// the window makes the drawer's "Quit App" action a lie, so they declare the
// command that actually exits them.
//
// Steam is therefore the FIRST TABLE ENTRY, not a special case in the close path.
// Adding another close-to-tray app is a one-line data change here.
//
// The table is deliberately an object-of-objects rather than a bare command map so
// future per-app quirks (launch flags, resume behaviour, shutdown grace) can be
// added as sibling keys without restructuring this module or its callers.
var APP_QUIRKS = {
    "steam": {
        quitCommand: ["steam", "-shutdown"]
    }
};

// === Companion windows ====================================================
//
// Some apps put the thing the user is actually looking at in a SECOND toplevel
// with its own window class, while the original window stays mapped in the
// background. Steam Remote Play does exactly this: launching a game opens a
// `streaming_client` window carrying the live video, and "Steam Big Picture
// Mode" (class `steam`) remains mapped behind it.
//
// The window enumerator treats each as an independent app — correctly, since
// `windowFilter.js` has no reason to know better — so the drawer grew TWO resume
// rows for one session, and the Steam-looking one resumed BIG PICTURE rather
// than the running game. Observed in the field (2026-08-25), where selecting it
// produced this journal line and a TV showing nothing:
//
//   origin=resume-verify mode=address wanted=0x5642a5027310
//     active=streaming_client reason=active-address-mismatch
//
// Keyed by WINDOW CLASS, not by app identity: this resolves live compositor
// windows, which is a different question from `APP_QUIRKS`'s "what did the user
// launch" — a `streaming_client` window has no desktop entry to match against at
// all, so the app-identity path above cannot see it.
var COMPANION_WINDOWS = {
    "streaming_client": {
        owner: "steam"
    }
};

function _cls(w) {
    return ((w && w.windowClass) || "").toLowerCase();
}

// Collapse companion/owner window PAIRS into the companion alone.
//
// Returns a new array; the input is not mutated. A companion with no owner
// present is left exactly as it is (it is still a real window the user may want
// to resume), and an owner with no companion is untouched — the collapse only
// happens when keeping both would misrepresent ONE session as two.
//
// The surviving row keeps the companion's address (the live surface — the whole
// point) and title (Remote Play titles its window with the game, which is a
// better label than "Steam"), but inherits the owner's icon: the companion class
// has no desktop entry, so its icon name resolves to nothing and the row would
// render an empty tile.
function groupCompanionWindows(windows) {
    var list = windows || [];
    var i;
    var present = {};
    for (i = 0; i < list.length; i++)
        present[_cls(list[i])] = true;

    // Owner classes that are represented by a companion currently on screen.
    var supersededOwners = {};
    for (i = 0; i < list.length; i++) {
        var rule = COMPANION_WINDOWS[_cls(list[i])];
        if (rule && present[rule.owner])
            supersededOwners[rule.owner] = true;
    }
    if (Object.keys(supersededOwners).length === 0)
        return list.slice();

    // Icon donor per superseded owner, resolved before anything is dropped.
    var ownerIcons = {};
    for (i = 0; i < list.length; i++) {
        var c = _cls(list[i]);
        if (supersededOwners[c] && ownerIcons[c] === undefined)
            ownerIcons[c] = (list[i] && list[i].icon) || "";
    }

    var out = [];
    for (i = 0; i < list.length; i++) {
        var w = list[i];
        var cls = _cls(w);
        if (supersededOwners[cls])
            continue;
        var companion = COMPANION_WINDOWS[cls];
        if (companion && supersededOwners[companion.owner]) {
            var donor = ownerIcons[companion.owner] || "";
            // Shallow copy: callers hold the poller's published model and must
            // not see it mutated underneath them.
            var merged = {};
            for (var k in w)
                merged[k] = w[k];
            if (donor !== "")
                merged.icon = donor;
            out.push(merged);
            continue;
        }
        out.push(w);
    }
    return out;
}

// The whole quirk record for an app, or null when the app has no overrides.
// `matcher` is the WindowMatcher singleton at runtime, a plain object in tests —
// same contract as prewarm.js / resumeModel.js, which is what keeps this module
// QML-free and headless-testable.
function quirksFor(app, matcher) {
    if (!app || !matcher)
        return null;
    var key = Prewarm.keyFor(app, matcher);
    if (!key)
        return null;
    return APP_QUIRKS[key] || null;
}

// The command array that actually quits `app`, or null meaning "no override —
// closing the window is a real quit for this app". Callers treat null as the
// signal to keep the existing window-close behaviour.
function quitCommandFor(app, matcher) {
    var q = quirksFor(app, matcher);
    return (q && q.quitCommand && q.quitCommand.length > 0) ? q.quitCommand : null;
}

// The close path is driven from a live window (a Hyprland address + its class),
// not from a desktop entry — the drawer's and HomeScreen's resume rows carry no
// app object. Resolve the window back to its discovered app with the SAME
// WindowMatcher the rest of the shell matches windows with, then look the quirk up
// by that app's identity. Returns null when the window maps to no known app or
// that app has no quit override.
//
// A loose class can match more than one entry (WindowMatcher's last resort is a
// substring test), so we keep scanning past a match that carries no quirk rather
// than letting the first loose hit mask a real one.
function quitCommandForWindow(windowClass, apps, matcher) {
    if (!windowClass || windowClass === "" || !apps || !matcher)
        return null;
    var client = {
        "class": windowClass,
        "initialClass": windowClass
    };
    for (var i = 0; i < apps.length; i++) {
        if (!matcher.matchesApp(apps[i], client))
            continue;
        var cmd = quitCommandFor(apps[i], matcher);
        if (cmd)
            return cmd;
    }
    return null;
}

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
// `windowFilter.js` has no reason to know better — so the drawer shows a row per
// window. That is RIGHT: Big Picture and the live game surface are two genuinely
// different destinations, and the user may want either.
//
// WHAT IS ACTUALLY WRONG IS THE COMPANION'S IDENTITY. `streaming_client` has no
// desktop entry, so the enumerator's icon fallback (lowercased window class)
// resolves to nothing and the row renders the blank letter-tile — the "weird
// icon" that made the stream row unrecognisable as the game the user was looking
// for. Its title is fine ("Red Dead Redemption 2 [Streaming]"); only the icon is.
//
// THIS DELIBERATELY DOES NOT COLLAPSE THE PAIR — an earlier revision did, and it
// was wrong twice over. It removed the user's only route to the live stream
// window, and it was justified by a `resume-verify ... active-address-mismatch`
// line that is ALSO the signature of the crossed-verification race fixed by
// `_resumeGeneration` (see resumeFocus.js). That is, the evidence for "the row
// targets the wrong window" was more likely the race than a mistargeted row.
// Don't reintroduce the collapse without evidence that survives that alternative.
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

// Give companion windows a recognisable icon, leaving the window set untouched.
//
// EVERY window in, every window out, same order — this changes how one row LOOKS,
// never which rows exist or where they go. Address and title are left alone, so
// the stream row still resumes the stream and Big Picture still resumes Big
// Picture.
//
// The icon comes from the owner's live window when one is mapped; when it is not,
// it falls back to the owner's CLASS NAME as an icon name (`steam`), which the
// icon theme resolves the same way every other app row does. That fallback is the
// point of not requiring the owner to be present: a stream whose Big Picture
// window has gone is still a real, resumable window and should still look like
// one.
//
// Returns a new array and never mutates the input — callers hold the poller's
// published `runningWindows` model, which other consumers read concurrently.
function identifyCompanionWindows(windows) {
    var list = windows || [];
    var i;

    // Icon donor per owner class, taken from its live window if present.
    var ownerIcons = {};
    for (i = 0; i < list.length; i++) {
        var c = _cls(list[i]);
        if (ownerIcons[c] === undefined)
            ownerIcons[c] = (list[i] && list[i].icon) || "";
    }

    var out = [];
    for (i = 0; i < list.length; i++) {
        var w = list[i];
        var rule = COMPANION_WINDOWS[_cls(w)];
        if (!rule) {
            out.push(w);
            continue;
        }
        var donor = ownerIcons[rule.owner] || rule.owner;
        if (!donor || donor === (w && w.icon)) {
            out.push(w);
            continue;
        }
        // Shallow copy so the source model is not mutated underneath its other
        // readers.
        var merged = {};
        for (var k in w)
            merged[k] = w[k];
        merged.icon = donor;
        out.push(merged);
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

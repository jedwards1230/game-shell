.pragma library

// Membership set from a list of window classes. Blank entries are dropped — an
// empty key would match every row whose class is unknown.
function _classSet(list) {
    var set = Object.create(null);
    var items = list || [];
    for (var i = 0; i < items.length; i++) {
        var c = items[i];
        if (typeof c === "string" && c !== "")
            set[c] = true;
    }
    return set;
}

// Merge running windows (AppLifecycleManager.runningWindows) + recent apps
// (RecentsTracker.recentApps) into a deduped, ordered "resume" list for the
// nav-drawer hero zone. Running-first, sorted by focusHistoryId ascending;
// unmatched recents appended.
//
// `matcher` is an object exposing execBasename(str) and normalize(str)
// (the WindowMatcher singleton at runtime; a plain stub in tests) — passing it
// in keeps this module QML-free and headless-testable.
//
// Ported VERBATIM from HomeScreen.qml's `_recentModel` merge/dedup
// (runningMatchesRecent + resolveRecentIcon + the running/recents assembly),
// MINUS the HomeScreen-specific widget-shadowing filter (root._widgets /
// hideFromRecent) — that suppresses apps an on-screen home widget already
// represents, and the drawer hosts no home widgets. HomeScreen could later adopt
// this module to DRY the duplicated merge.
//
// Entry shape (identical to HomeScreen's, consumed by AppCard, plus the two
// audio flags below):
//   { windowClass, address, name, icon, exec, comment, running, focusHistoryId,
//     audioActive, userMuted }
//
// `audio` is optional: `{ activeClasses, mutedClasses }`, both lists of window
// classes. Passing it in (rather than reaching for a singleton) keeps this module
// QML-free, and both lists are produced upstream by `audioOwnership.js` — the
// same attribution the muting decision uses, so an indicator cannot disagree
// with what you actually hear.
//
// The two flags answer deliberately DIFFERENT questions, because the obvious
// pair would be useless:
//   * `audioActive` — this app has a live playback stream. Under the workspace
//     policy this is the informative one: it names the app making noise you
//     cannot hear.
//   * `userMuted` — the user muted this app BY HAND. Never the policy mute,
//     which is true of nearly every app at any moment and would light up every
//     row while telling you nothing.
//
// Only RUNNING rows can carry either flag. A recent-but-not-running app has no
// window and therefore no window class to attribute a stream to or key a mute
// on; both read false, which is also why the drawer offers no mute toggle there.
function build(running, recents, allApps, matcher, audio) {
    running = running || [];
    recents = recents || [];
    allApps = allApps || [];

    var audioActiveSet = _classSet((audio || {}).activeClasses);
    var userMutedSet = _classSet((audio || {}).mutedClasses);

    function runningMatchesRecent(win, recent) {
        let cls = (win.windowClass || "").toLowerCase();
        let execBase = matcher.execBasename(recent.exec || "");
        let appName = (recent.name || "").toLowerCase();
        let winName = (win.name || "").toLowerCase();
        if (winName !== "" && winName === appName)
            return true;
        if (execBase !== "") {
            if (cls === execBase || matcher.normalize(cls) === matcher.normalize(execBase))
                return true;
            if (cls !== "" && (execBase.indexOf(cls) >= 0 || cls.indexOf(execBase) >= 0))
                return true;
        }
        if (appName !== "" && (cls === appName || matcher.normalize(cls) === matcher.normalize(appName)))
            return true;
        return false;
    }
    function resolveRecentIcon(rec) {
        let rexec = matcher.execBasename(rec.exec || "");
        for (let i = 0; i < allApps.length; i++) {
            let a = allApps[i];
            if (a.name && rec.name && a.name === rec.name)
                return a.icon || "";
            if (rexec !== "" && matcher.execBasename(a.exec || "") === rexec)
                return a.icon || "";
        }
        return rec.icon || "";
    }

    let runningEntries = [];
    let matchedRecentIndices = new Set();
    for (let r = 0; r < running.length; r++) {
        let win = running[r];
        for (let j = 0; j < recents.length; j++) {
            if (runningMatchesRecent(win, recents[j]))
                matchedRecentIndices.add(j);
        }
        runningEntries.push({
            windowClass: win.windowClass,
            address: win.address || "",
            name: win.title || win.name || win.windowClass,
            icon: win.icon || "",
            exec: "",
            comment: "",
            running: true,
            focusHistoryId: (win.focusHistoryId !== undefined) ? win.focusHistoryId : 9999,
            audioActive: audioActiveSet[win.windowClass || ""] === true,
            userMuted: userMutedSet[win.windowClass || ""] === true
        });
    }
    runningEntries.sort(function (a, b) {
        return a.focusHistoryId - b.focusHistoryId;
    });

    let result = runningEntries.slice();
    for (let k = 0; k < recents.length; k++) {
        if (matchedRecentIndices.has(k))
            continue;
        let rec = recents[k];
        result.push({
            windowClass: "",
            address: "",
            name: rec.name || "",
            icon: resolveRecentIcon(rec),
            exec: rec.exec || "",
            comment: rec.comment || "",
            running: false,
            // A recent-but-not-running app has no window, so there is no class
            // to attribute a stream to or to key a mute on.
            focusHistoryId: 9999,
            audioActive: false,
            userMuted: false
        });
    }
    return result;
}

// --- nav-row focus, kept by identity ----------------------------------------
//
// The drawer's nav rows are [Home, ...one per running app], sorted by
// focusHistoryId, and BOTH their order and their membership change while the
// drawer is open. So "which row is the user on" cannot be a position.
//
// Restoring by index is worse than the problem it solves: an app exiting — or
// any new window mapping, which lands at focusHistoryId 0 and shifts everything
// down — moves a given index onto a DIFFERENT app. The cursor would sit on a
// neighbour with nothing on screen saying so, and the next activation would
// resume the wrong app. Landing on Home is at least visible.
//
// Index 0 is Home and carries no entry, so "" doubles as "Home / nothing known".

// The window address of a nav row, or "" for Home / out of range.
function addressAt(navRows, index) {
    var rows = navRows || [];
    if (index <= 0 || index >= rows.length)
        return "";
    var entry = (rows[index] || {}).entry;
    return (entry && entry.address) ? entry.address : "";
}

// Where a remembered address sits in the CURRENT rows; 0 (Home) when that app is
// gone, which is the right landing place for a row that no longer exists.
function indexForAddress(navRows, address) {
    if (!address || address === "")
        return 0;
    var rows = navRows || [];
    for (var i = 1; i < rows.length; i++) {
        var entry = (rows[i] || {}).entry;
        if (entry && entry.address === address)
            return i;
    }
    return 0;
}

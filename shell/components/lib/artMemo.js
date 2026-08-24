.pragma library

// Session-lifetime NEGATIVE memo for poster/art URLs that are permanently dead.
//
// Why it exists: SteamCard walks an ordered art candidate chain
// (`[localArt, art, headerArt]` — LAN sidecar capsule, CDN portrait, CDN header)
// and advances on `Image.Error`. The daemon emits `localArt` for EVERY library
// entry unconditionally, so for a game with no locally cached capsule candidate 0
// is a deterministic, permanent 404. Steam-library delegates are recreated
// constantly (a plain ListView with no `reuseItems`, fed a freshly built model on
// every poll), so that same 404 was re-walked from index 0 over and over — the
// `Error transferring …/art/<appid> - server replied: Not Found` lines in the log.
//
// Recording the dead URL lets the next delegate start at the first UNTRIED
// candidate instead of re-issuing the known-failing request.
//
// Keyed by full URL, not by appid: the candidates for one game are different
// hosts/paths with independent liveness. If EVERY candidate is marked bad we
// return 0 rather than an out-of-range index — the chain then behaves exactly as
// before (walk, fail, fall through to the letter placeholder) instead of silently
// skipping the whole chain.
//
// LIFETIME IS NOT REALLY "THE SESSION" — it is "until the host comes back".
// `Image.status` cannot tell a 404 apart from an unreachable host, and candidate 0
// lives on the Steam sidecar HOST, which this shell explicitly models as routinely
// asleep (SteamLibraryView has a WakeCard, a `_showWake` state, a Wake-on-LAN
// action and a 3s fast-poll for exactly that). While the host is down EVERY card's
// local capsule errors, so a naive session-lifetime memo would mark the entire
// library's local art dead and never look again.
//
// That is not cosmetic: SteamCard puts the LAN capsule first precisely because the
// CDN portrait sometimes returns a ~1.6KB placeholder stub that loads as *Ready*,
// not Error — so the error walk never advances past it and the card renders blank
// (see the rationale comment in SteamCard.qml). Permanently skipping the capsule
// would reintroduce that blank-poster bug for the rest of the session.
//
// So SteamLibraryView calls `reset()` on every host-unreachable -> reachable
// transition. Re-trying a handful of permanently missing URLs once per host recovery is
// far cheaper than the failure it prevents.
//
// Pure `.pragma library` (one shared instance per QML engine) so it is
// headless-testable — see tests/qml/tst_artmemo.qml.

var _bad = {};

// Record `url` as permanently failing for this session.
function markBad(url) {
    if (typeof url !== "string" || url === "")
        return;
    _bad[url] = true;
}

// True when `url` has already failed this session.
function isBad(url) {
    if (typeof url !== "string" || url === "")
        return false;
    return _bad[url] === true;
}

// Index of the first candidate not yet known-bad. Returns 0 for an empty list or
// when every candidate is bad (see the note above — 0 preserves the original
// walk).
function firstUntried(candidates) {
    if (!candidates || candidates.length === undefined)
        return 0;
    for (var i = 0; i < candidates.length; i++) {
        if (!isBad(candidates[i]))
            return i;
    }
    return 0;
}

// Drop every recorded URL. Called in production by SteamLibraryView whenever the
// sidecar host becomes reachable again (see the lifetime note above), and by the
// tests.
function reset() {
    _bad = {};
}

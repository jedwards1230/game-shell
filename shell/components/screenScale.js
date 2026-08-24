.pragma library

// Pure screen-geometry filter behind the `Units` singleton's `screenHeight`.
//
// The shell scales EVERY size off one number — the primary screen's height — so a
// momentary bad reading rescales the whole UI. `Quickshell.screens` goes
// transiently EMPTY during startup, DPMS off/on, a mode set and CEC/TV power
// transitions, and a ShellScreen can be PRESENT while reporting height ~0
// mid-transition. A naive live binding turned both into a full rescale, and every
// rescale re-requested every icon in the shell at a size the device never renders
// (the QSize(240,240) and QSize(2,2) `Could not load icon` floods, plus the
// matching `qt.svg.draw: The requested buffer size is too big` lines).
//
// This module is the whole decision, kept pure so it is headless-testable
// (tests/qml/tst_screenscale.qml) — Units.qml itself can't be, since it imports
// Quickshell. Same `.pragma library` convention as prewarm.js / resumeModel.js /
// settingsPayload.js.
//
// State shape: { height: int, ready: bool }
//   height — the last VALID height, or FALLBACK_HEIGHT before one is seen.
//   ready  — false until a valid height has been adopted. This is the important
//            half: `height` must always be a usable number so layout arithmetic
//            never divides by zero, but callers that must NOT act on a guess
//            (above all the icon provider) gate on `ready` instead.
//
// `ready` is one-way. Once a real height has been seen the shell keeps using the
// last good one across every later transient, so a DPMS cycle can never drop the
// UI back into "unknown scale" and re-issue the whole icon set.

// Below this a report is "no usable screen", not a resolution: far under any
// panel this shell runs on, far over the 0/1px a mid-transition ShellScreen
// hands back.
var MIN_VALID_HEIGHT = 200;

// Layout-only placeholder used before the first valid report. It is deliberately
// NOT treated as a real resolution: `ready` stays false while it is in force, so
// nothing that would bake it into a cached/served artifact acts on it. It matches
// the historical default purely so the pre-first-screen frame lays out as it
// always did.
var FALLBACK_HEIGHT = 2160;

function initial() {
    return {
        height: FALLBACK_HEIGHT,
        ready: false
    };
}

function isValid(height) {
    return typeof height === "number" && isFinite(height) && height >= MIN_VALID_HEIGHT;
}

// Fold a raw reading into the state. A degenerate reading returns the PREVIOUS
// state object unchanged (same identity, so a QML `var` property holding it emits
// no change and nothing downstream churns). A valid reading that matches the
// current height also returns the previous state — unless it flips `ready`.
function adopt(state, rawHeight) {
    var prev = state || initial();
    if (!isValid(rawHeight))
        return prev;
    if (prev.ready && prev.height === rawHeight)
        return prev;
    return {
        height: rawHeight,
        ready: true
    };
}

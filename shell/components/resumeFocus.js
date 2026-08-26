.pragma library

// Pure decision logic for resuming a backgrounded app.
//
// ============================================================================
// The switching primitive is the WORKSPACE, not focus.
// ============================================================================
//
// This module used to resolve a focus target (`hyprctl dispatch focuswindow
// address:0x…`), verify the landing by reading the active window back, and then
// assert fullscreen on it. Every one of those steps is gone, because the premise
// was wrong: focus is a request a window can decline.
//
// Observed in the field (2026-08-26): a Steam Remote Play `streaming_client`
// window sat tiled at half width behind a fullscreen `steam`, reporting
// `acceptsInput: false`. `dispatch focuswindow` returned `ok` and did nothing.
// The verification correctly noticed the miss and gave up, which surfaced to the
// user as "clicking the stream row drops me back to the home screen" — the
// stream could not be re-opened at all.
//
// Under the workspace model each app class owns a workspace (assigned by the
// daemon, see `daemon/src/workspaces.rs`), so resuming is:
//
//     find the target window -> read its workspace -> dispatch workspace N
//
// `dispatch workspace N` is a compositor-level operation. No window can refuse
// it, and it cannot half-succeed. Verification is correspondingly trivial: read
// the active workspace id back and compare one integer, instead of reading
// `activewindow` — which lies while the shell's layer surface is on screen.
//
// Fullscreen no longer participates. With one window per workspace and
// `gaps_in/gaps_out = 0`, a lone tiled window already fills the screen.
//
// Kept from the old module: `stamp`/`isStale`. Generations still matter, because
// a resume is still a chain of async hops and a second resume can still start
// mid-chain.

// --- reasons -----------------------------------------------------------------

// Resolution failures.
var REASON_NO_TARGET = "no-target";
var REASON_NO_WORKSPACE = "no-workspace";

// Verification outcomes.
var REASON_NO_MONITORS = "no-monitors";
var REASON_NO_ACTIVE_WORKSPACE = "no-active-workspace";
var REASON_WORKSPACE_MISMATCH = "workspace-mismatch";

function _s(v) {
    return (v === undefined || v === null) ? "" : ("" + v);
}

// --- target resolution -------------------------------------------------------

// Find the window a resume refers to, by address when we have one and by class
// otherwise.
//
// Address is preferred and class is the fallback because they answer different
// questions. A drawer row is a specific WINDOW (it carries an address), and with
// Steam that distinction is the whole point: `steam` and `streaming_client` are
// two rows the user switches between independently. The "resume this recent app"
// path has only a desktop entry, so it can offer nothing but a class, and takes
// the first window of that class.
//
// Returns `{ address, windowClass, workspace, reason }`. `reason` is "" on
// success; on failure `workspace` is "" and the caller must not dispatch.
function resolveTarget(address, windowClass, runningWindows) {
    var addr = _s(address);
    var cls = _s(windowClass);
    var list = runningWindows || [];
    var match = null;

    if (addr !== "") {
        for (var i = 0; i < list.length; i++) {
            if (_s(list[i].address) === addr) {
                match = list[i];
                break;
            }
        }
    }
    if (!match && cls !== "") {
        for (var j = 0; j < list.length; j++) {
            if (_s(list[j].windowClass) === cls) {
                match = list[j];
                break;
            }
        }
    }
    if (!match) {
        return {
            address: addr,
            windowClass: cls,
            workspace: "",
            reason: REASON_NO_TARGET
        };
    }

    var ws = _s(match.workspace);
    return {
        address: _s(match.address) || addr,
        windowClass: _s(match.windowClass) || cls,
        workspace: ws,
        // A mapped window always has a workspace; an empty one means the client
        // list was published before the daemon parked it. Fail rather than guess
        // — dispatching `workspace ""` would be a syntax error, and defaulting to
        // the home workspace would hide the app the user just asked for.
        reason: ws === "" ? REASON_NO_WORKSPACE : ""
    };
}

// Whether a resolved target is safe to dispatch.
function canDispatch(target) {
    return !!target && _s(target.workspace) !== "" && _s(target.reason) === "";
}

// Hyprland workspace selector. Numeric ids pass through bare; anything else is a
// name and needs the `name:` prefix.
function workspaceSelector(workspace) {
    var ws = _s(workspace);
    if (ws === "")
        return "";
    return /^[0-9]+$/.test(ws) ? ws : ("name:" + ws);
}

// --- verification ------------------------------------------------------------

// The active workspace, read from the daemon's `hypr-monitors` reply.
//
// Single-output kiosk, so the first monitor is the screen. Returns "" when the
// reply is empty or shaped unexpectedly, which callers treat as "cannot verify"
// rather than "failed" — an unreadable probe is not evidence the switch missed.
function activeWorkspaceOf(monitors) {
    var list = monitors || [];
    if (list.length === 0)
        return "";
    var m = list[0] || {};
    var ws = m.activeWorkspace;
    if (ws && typeof ws === "object")
        ws = ws.name !== undefined ? ws.name : ws.id;
    return _s(ws);
}

// Did the switch land? Compares one integer, and that is the entire improvement
// over the old address-matching verification.
//
// Returns `{ landed, reason }`. A probe that could not be read at all reports
// `landed: false` with a distinguishable reason so the caller can log it without
// treating it as a user-visible failure.
function verifyLanding(target, monitors) {
    if (!target || _s(target.workspace) === "") {
        return {
            landed: false,
            reason: REASON_NO_TARGET
        };
    }
    var list = monitors || [];
    if (list.length === 0) {
        return {
            landed: false,
            reason: REASON_NO_MONITORS
        };
    }
    var active = activeWorkspaceOf(list);
    if (active === "") {
        return {
            landed: false,
            reason: REASON_NO_ACTIVE_WORKSPACE
        };
    }
    var ok = active === _s(target.workspace);
    return {
        landed: ok,
        reason: ok ? "" : REASON_WORKSPACE_MISMATCH
    };
}

// Whether a verification failure is real evidence the resume missed, as opposed
// to the probe being unreadable. Only the former should surface to the user.
function isRealMiss(result) {
    return !!result && !result.landed && _s(result.reason) === REASON_WORKSPACE_MISMATCH;
}

// --- generations -------------------------------------------------------------

// A resume is a chain of async hops (resolve -> dispatch -> settle -> verify) and
// every hop is a place a second resume can start. The state carrying one is
// single-slot, so without a stamp a stale reply can conclude about the wrong
// resume — which is what produced spurious `resume-verify` misses.
//
// Suppression, not cancellation: a superseded chain's dispatch was already
// issued and is harmless (the newer resume's own lands after it and wins). What
// must not happen is a stale chain reaching a CONCLUSION.
function stamp(decision, generation) {
    if (decision)
        decision.generation = generation;
    return decision;
}

// An unstamped decision counts as current, so a caller that never opted into
// generations keeps working.
function isStale(decision, generation) {
    var d = decision || {};
    if (d.generation === undefined || d.generation === null)
        return false;
    return d.generation !== generation;
}

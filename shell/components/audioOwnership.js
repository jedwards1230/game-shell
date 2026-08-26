.pragma library

// Pure decision logic for "audio ownership follows workspace ownership".
//
// ============================================================================
// The rule is one sentence.
// ============================================================================
//
//     Mute every playback stream that does not belong to a window class whose
//     workspace is the one on screen.
//
// The home screen needs no special case. Workspace 1 is reserved and left EMPTY
// (`workspaces::HOME_WORKSPACE` in the daemon), so no window's workspace ever
// equals it — every stream falls on the "does not belong" side and everything
// goes quiet. That is the behaviour the feature exists for: you should not hear
// a game you cannot see.
//
// ----------------------------------------------------------------------------
// Why this replaced a class allowlist
// ----------------------------------------------------------------------------
//
// The previous module muted ONE hard-coded window class ("steam") and only while
// `shellState === "idle"`. Three things were wrong with that, and the second is
// the one that made the behaviour look random:
//
//   1. The allowlist matched the wrong window. Steam Remote Play's live game
//      window has class `streaming_client`, and "streaming_client" does not
//      contain "steam" (str-e-am vs st-e-am). So the muter engaged for Big
//      Picture and never for the window the game audio actually belongs to.
//   2. It reasoned over a SINGLE `runningAppClass` — the last-resumed app. Since
//      the one-workspace-per-app model landed, several apps run at once (Plex,
//      Steam, the stream), each on its own workspace. Nothing ever muted Plex.
//   3. `shellState === "idle"` is not "the shell is on screen". Under the
//      workspace model "what is the user looking at" is exactly the active
//      workspace id — one integer, and it cannot lie the way focus does.
//
// ----------------------------------------------------------------------------
// Attributing a PipeWire node to a window — the hard part, decided by evidence
// ----------------------------------------------------------------------------
//
// A playback stream does not announce which window owns it, and the obvious
// mappings do not survive contact with Steam. Measured on a live box with Plex,
// Big Picture and a Remote Play stream all running:
//
//   * PID matching FAILS. Plex's PipeWire client pid equals its Hyprland window
//     pid exactly — but Big Picture's window pid is a `steamwebhelper` that owns
//     no PipeWire client, and the stream's audio pid sits under
//     `streaming_client -> reaper -> IPC:CSteamEngine -> steam`, a branch that
//     never passes through the window's pid. They are siblings, not ancestors,
//     so neither an exact match nor a walk up the process tree connects them.
//
//   * `application.name` / `node.name` MISLEAD. On the Remote Play stream node
//     both are literally "Steam" — the same token as the Big Picture window
//     class. Matching on those collapses two distinct workspaces into one, which
//     is precisely the distinction the switcher depends on.
//
//   * `application.process.binary` DISCRIMINATES. It is the real executable
//     name, and it lined up with the window class for all three apps without any
//     cross-attribution:
//
//         binary              class              related?
//         Plex                tv.plex.Plex       yes (class contains binary)
//         steam               steam              yes (exact)
//         steamwebhelper      steam              yes (binary contains class)
//         streaming_client    streaming_client   yes (exact)
//         streaming_client    steam              NO   <- the one that matters
//
// So attribution runs in two passes: STRICT on the binary, and only if that
// names nothing, LOOSE on the display names. The loose pass exists for one real
// state — a stream process outliving its window, still holding a playing node.
// Strict finds no class for it (the window is gone), loose attributes it to
// `steam` via "Steam", and it is then muted everywhere except the Steam
// workspace. Running the passes in this order is what keeps the loose pass from
// stealing the live stream away from its own workspace.
//
// ----------------------------------------------------------------------------
// The failure direction, stated rather than buried
// ----------------------------------------------------------------------------
//
// A node that attributes to nothing is MUTED. That is deliberate — an orphaned
// stream must go quiet on the home screen, and "unattributable" is exactly what
// an orphan looks like. The cost is that if attribution ever failed for the app
// you are LOOKING AT, you would get video with no sound. It resolved correctly
// for every app this shell actually runs, and the loose pass widens the net
// further, but that is the direction a miss falls.

// Two tokens this short can only match by accident, and an accidental match here
// silences the wrong app. Both sides must clear it.
var MIN_TOKEN_LENGTH = 3;

// Playback the SHELL ITSELF produces, which this policy must never touch.
//
// This is not a per-app allowlist creeping back in — it is the opposite end of
// the problem. The rule above is about APPS, and an app is a thing with a
// window. The Settings ▸ Audio speaker test (`AudioSettings.qml`) execs
// `pw-play` to play a test tone; it has no window by construction, so it
// attributes to nothing and would be muted as an orphan. The user would press
// "test the centre channel" and hear silence — an intermittent, miserable bug,
// since it only bites when a cycle happens to run while the tone plays.
//
// Matched EXACTLY against `application.process.binary`, not by the fuzzy
// relatedness test, so this cannot quietly widen.
var SHELL_OWNED_BINARIES = ["pw-play"];

// Is this node the shell's own audio rather than an app's?
function isShellOwned(node) {
    var binary = _lc((node || {}).binary);
    for (var i = 0; i < SHELL_OWNED_BINARIES.length; i++) {
        if (binary === SHELL_OWNED_BINARIES[i])
            return true;
    }
    return false;
}

function _s(v) {
    return (v === undefined || v === null) ? "" : ("" + v);
}

function _lc(v) {
    return _s(v).toLowerCase();
}

// Case-insensitive relatedness between two identifier-ish tokens: equal, or one
// contains the other. Containment is needed in BOTH directions — a class can be
// a reverse-DNS name wrapping the binary (`tv.plex.Plex` / `Plex`), and a binary
// can be the class plus a suffix (`steamwebhelper` / `steam`).
function _related(a, b) {
    var x = _lc(a);
    var y = _lc(b);
    if (x.length < MIN_TOKEN_LENGTH || y.length < MIN_TOKEN_LENGTH)
        return false;
    return x === y || x.indexOf(y) >= 0 || y.indexOf(x) >= 0;
}

// The most specific class `token` relates to, or "". Longest wins, so a binary
// that relates to both `steam` and `steamwebhelper` picks the latter rather than
// whichever happened to be listed first.
function _bestMatch(token, classes) {
    var best = "";
    for (var i = 0; i < classes.length; i++) {
        var c = _s(classes[i]);
        if (_related(token, c) && c.length > best.length)
            best = c;
    }
    return best;
}

// --- node records ------------------------------------------------------------

// A node's live mute flag, which `pw-dump` reports under `info.params.Props[0]`
// rather than in `info.props`. Absent or malformed reads as "not muted", so a
// shape change degrades into doing nothing rather than into a false adoption.
function _mutedOf(info) {
    var params = (info || {}).params;
    if (!params)
        return false;
    var list = params.Props;
    if (!list || !list.length)
        return false;
    return (list[0] || {}).mute === true;
}

// Reduce a parsed `pw-dump` array to the playback streams, as compact records.
//
// Scoped to `media.class == "Stream/Output/Audio"` — an app's sink-inputs. Sinks,
// sources and devices are never candidates, so no bug in this file can mute the
// output the whole box plays through.
function nodesFrom(dump) {
    var list = dump || [];
    var out = [];
    for (var i = 0; i < list.length; i++) {
        var entry = list[i] || {};
        var info = entry.info || {};
        var props = info.props;
        if (!props)
            continue;
        if (_s(props["media.class"]) !== "Stream/Output/Audio")
            continue;
        var id = _s(entry.id);
        if (id === "")
            continue;
        out.push({
            id: id,
            binary: _s(props["application.process.binary"]),
            appName: _s(props["application.name"]),
            nodeName: _s(props["node.name"]),
            mediaName: _s(props["media.name"]),
            muted: _mutedOf(info)
        });
    }
    return out;
}

// --- attribution -------------------------------------------------------------

// Which running window class owns this stream? "" when nothing claims it.
//
// Strict pass first (see the header): the binary is the only field observed to
// tell the two Steam workspaces apart. The display names are consulted only when
// the binary names nothing at all.
function ownerClassOf(node, windowClasses) {
    var classes = windowClasses || [];
    var n = node || {};

    var strict = _bestMatch(n.binary, classes);
    if (strict !== "")
        return strict;

    var loose = _bestMatch(n.appName, classes);
    if (loose !== "")
        return loose;
    loose = _bestMatch(n.nodeName, classes);
    if (loose !== "")
        return loose;
    return _bestMatch(n.mediaName, classes);
}

// --- policy ------------------------------------------------------------------

// The node ids that SHOULD be muted while `activeWorkspace` is on screen.
//
// An unknown active workspace yields an empty set rather than a guess: with no
// idea what is on screen there is no policy to apply, and muting on a guess is
// how you silence the game the user is watching. The caller unmutes whatever it
// holds and waits for a real reading.
function desiredMutedIds(nodes, runningWindows, activeWorkspace) {
    var ws = _s(activeWorkspace);
    if (ws === "")
        return [];

    // class -> workspace, first window of a class wins (windows of one class
    // share a workspace by construction — see daemon/src/workspaces.rs).
    var workspaceOf = Object.create(null);
    var classes = [];
    var wins = runningWindows || [];
    for (var i = 0; i < wins.length; i++) {
        var cls = _s((wins[i] || {}).windowClass);
        if (cls === "" || workspaceOf[cls] !== undefined)
            continue;
        workspaceOf[cls] = _s(wins[i].workspace);
        classes.push(cls);
    }

    var muted = [];
    var list = nodes || [];
    for (var j = 0; j < list.length; j++) {
        if (isShellOwned(list[j]))
            continue;
        var owner = ownerClassOf(list[j], classes);
        var onScreen = owner !== "" && workspaceOf[owner] === ws;
        if (!onScreen)
            muted.push(_s(list[j].id));
    }
    return muted;
}

// --- reconciliation ----------------------------------------------------------

// Diff the desired muted set against what we currently hold muted.
//
// Returns `{ mute, unmute }`. Only ids WE muted are ever unmuted: audio the
// shell never touched is left exactly as the user set it, and a node the user
// muted themselves does not get helpfully turned back on.
function reconcile(desiredIds, appliedIds) {
    var desired = desiredIds || [];
    var applied = appliedIds || [];

    var inDesired = Object.create(null);
    for (var i = 0; i < desired.length; i++)
        inDesired[_s(desired[i])] = true;
    var inApplied = Object.create(null);
    for (var j = 0; j < applied.length; j++)
        inApplied[_s(applied[j])] = true;

    var mute = [];
    for (var k = 0; k < desired.length; k++) {
        var d = _s(desired[k]);
        if (!inApplied[d])
            mute.push(d);
    }
    var unmute = [];
    for (var m = 0; m < applied.length; m++) {
        var a = _s(applied[m]);
        if (!inDesired[a])
            unmute.push(a);
    }
    return {
        mute: mute,
        unmute: unmute
    };
}

// Playback streams already muted when the shell starts up.
//
// WHY THIS EXISTS. Mutes live in the PipeWire graph and outlive the shell;
// `_appliedIds` does not. "Only unmute what we muted" holds a session together,
// but across a restart it strands: a node the PREVIOUS shell instance muted is
// one the new instance will never release, so the app stays silent forever. And
// restarting the shell is not exotic — it IS the deploy loop
// (`systemctl --user restart tv-shell-quickshell.service`). Observed exactly
// that way: a live stream on the displayed workspace, playing to a muted node,
// with the new instance holding an empty applied set.
//
// So the first cycle adopts whatever it finds muted. The previous instance is
// the only plausible author (the shell's own volume control mutes the SINK, not
// individual streams), and reconciliation then releases it the moment its
// workspace is displayed. Adopting a mute we did not set is recoverable;
// stranding one is not.
function adoptableMutedIds(nodes) {
    var list = nodes || [];
    var out = [];
    for (var i = 0; i < list.length; i++) {
        if (list[i] && list[i].muted === true && !isShellOwned(list[i]))
            out.push(_s(list[i].id));
    }
    return out;
}

// Every id must be a bare integer before it reaches a command line. The ids come
// from `pw-dump`, not from a user, but the shell has no business handing an
// unvalidated string to a process — and a malformed one would be a silent no-op
// that looks like a policy bug.
function isValidNodeId(id) {
    return /^[0-9]+$/.test(_s(id));
}

// The applied set after a run that muted `mute` and unmuted `unmute`, starting
// from `appliedIds`. Kept here (rather than in the caller) so the bookkeeping
// that must never strand a muted node is testable on its own.
function nextApplied(appliedIds, mute, unmute) {
    var dropped = Object.create(null);
    var un = unmute || [];
    for (var i = 0; i < un.length; i++)
        dropped[_s(un[i])] = true;

    var next = [];
    var seen = Object.create(null);
    var applied = appliedIds || [];
    for (var j = 0; j < applied.length; j++) {
        var a = _s(applied[j]);
        if (dropped[a] || seen[a])
            continue;
        seen[a] = true;
        next.push(a);
    }
    var add = mute || [];
    for (var k = 0; k < add.length; k++) {
        var m = _s(add[k]);
        if (seen[m])
            continue;
        seen[m] = true;
        next.push(m);
    }
    return next;
}

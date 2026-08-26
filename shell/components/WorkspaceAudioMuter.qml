import QtQuick
import Quickshell.Io
import "audioOwnership.js" as AudioOwnership

// WorkspaceAudioMuter — audio ownership follows workspace ownership.
//
// You should hear the thing you are looking at, and nothing else. Since the
// one-workspace-per-app model landed (docs/KIOSK_WINDOW_MODEL.md), "the thing
// you are looking at" is exactly the displayed workspace, so this component
// mutes the PipeWire playback streams of every app that is not on it — and the
// home screen, which owns the deliberately empty workspace 1, silences
// everything.
//
// This replaced `StreamAudioMuter`, which muted one hard-coded window class
// ("steam") while `shellState === "idle"`. The decision logic and the evidence
// behind it (including why PID matching does not work and why
// `application.process.binary` does) live in `audioOwnership.js`.
//
// Inputs, all injected by shell.qml:
//   - activeWorkspace ← appLifecycle.displayedWorkspace  (workspace NAME, "1" = home)
//   - runningWindows  ← appLifecycle.runningWindows      ([{windowClass, workspace, …}])
//   - shellState      ← root.state                       (a freshness gate, NOT the predicate)
//
// Shape of a cycle: enumerate the graph -> decide (pure) -> apply the diff. The
// desired set is recomputed from scratch on every input change and a serialized
// pump reconciles the applied set toward it, so a rapid switch away and back
// cannot strand a muted node.
Item {
    id: muter

    // The workspace currently on screen. "" means "not known yet" — the policy
    // deliberately does nothing rather than guess (see desiredMutedIds).
    property string activeWorkspace: ""
    property var runningWindows: []

    // A FRESHNESS GATE, not a predicate. The predicate is the workspace, always.
    //
    // `runningWindows` is refreshed by AppLifecycleManager's `windowPollTimer`,
    // which runs only while the shell owns the state machine (`idle` or
    // `appRunning`). In `streaming`/`reconnecting` the window list is frozen and
    // Moonlight is launched as a bare Process that never goes through
    // `showWorkspace()`, so the shell's workspace model stops describing the
    // screen. Reconciling on a stale model would find the live stream's audio
    // unattributable and MUTE IT — video with no sound, in the one state where
    // that is least acceptable.
    //
    // So this mirrors `windowPollTimer.running` deliberately. That coupling used
    // to exist by accident (a frozen window list happened to mean no cycle ever
    // fired); stating it here makes it enforced instead of lucky, and widening
    // the poll gate later cannot silently silence a stream.
    property string shellState: "idle"
    readonly property bool _modelDescribesScreen: shellState === "idle" || shellState === "appRunning"

    // Node ids this component currently holds muted. The source of truth for
    // what to undo; nothing outside this list is ever unmuted.
    property var _appliedIds: []
    // Set while a cycle is in flight, so inputs changing mid-cycle queue one
    // more pass instead of interleaving two.
    property bool _cycleRunning: false
    property bool _restartQueued: false
    // Whether the first graph has been read and its existing mutes adopted.
    property bool _adoptedExisting: false

    // Re-read the graph and reconcile. Every input change lands here.
    function _evaluate() {
        if (!muter._modelDescribesScreen)
            return;
        // Guard on the RUNTIME's own state as well as our flag. Re-arming a
        // Process from inside its own `onExited` is only safe if Quickshell has
        // already cleared `running`; if it has not, the write is silently
        // dropped, and with `_cycleRunning` set below no further `exited` would
        // ever arrive — the component would be dead for the session, with no log
        // line. Treating a still-running process as "busy" makes that
        // unrepresentable rather than dependent on emission order.
        if (_cycleRunning || enumerateProc.running || applyProc.running) {
            // A snapshot taken before this change would decide on stale inputs.
            // Let the in-flight cycle finish and immediately run another.
            muter._restartQueued = true;
            return;
        }
        muter._cycleRunning = true;
        cycleWatchdog.restart();
        enumerateProc.collected = "";
        enumerateProc.running = true;
    }

    onActiveWorkspaceChanged: _evaluate()
    onRunningWindowsChanged: _evaluate()
    // Leaving streaming re-opens the gate; reconcile against whatever changed
    // while it was shut. Hooked to `shellState` rather than to the derived
    // `_modelDescribesScreen` because a change handler for an underscore-prefixed
    // property is the parse-passes-but-fails-to-load trap this repo keeps
    // stepping in — `_evaluate` re-checks the gate itself anyway.
    onShellStateChanged: _evaluate()

    // Audio can start without anything this component is bound to changing.
    //
    // The inputs are the displayed workspace and the window set, and neither
    // moves when a backgrounded app simply BEGINS playing — Plex rolling into
    // the next episode, or a Steam stream reconnecting and opening a fresh node
    // while the user sits on the home screen. Event-driven reconciliation alone
    // cannot deliver "you never hear what you cannot see"; it only reacts to the
    // screen changing, not to the graph changing.
    //
    // Cadence matches AppLifecycleManager's own window poll, so this adds no new
    // rhythm to the shell. Steady-state cost is one `pw-dump` and one parse: when
    // nothing changed the reconcile diff is empty and no `wpctl` runs at all.
    Timer {
        id: sweepTimer
        interval: 5000
        repeat: true
        running: muter._modelDescribesScreen
        onTriggered: muter._evaluate()
    }

    // `_cycleRunning` is cleared by a process EXITING. A `pw-dump` or `wpctl`
    // that hangs — or a Process that fails to start without emitting `exited` —
    // would otherwise leave it stuck true and the muter would never reconcile
    // again. The component this replaced self-healed here for free because it
    // guarded on `muteProc.running` (the runtime's own state) rather than a
    // shadow flag; this restores that property.
    Timer {
        id: cycleWatchdog
        interval: 10000
        repeat: false
        onTriggered: {
            if (!muter._cycleRunning)
                return;
            console.warn("WorkspaceAudioMuter: audio cycle did not finish within 10s; releasing the pump");
            muter._cycleRunning = false;
            if (muter._restartQueued) {
                muter._restartQueued = false;
                Qt.callLater(muter._evaluate);
            }
        }
    }

    // Enumerate the PipeWire graph.
    //
    // `pw-dump` emits pretty-printed JSON and SplitParser reads line-by-line, so
    // the newlines are folded out and the whole document arrives as ONE line for
    // `JSON.parse`. Parsing the real JSON — rather than scraping `wpctl status`
    // or shelling out to jq — is a safety decision, not a style one: this policy
    // enumerates the entire graph, and a mis-parsed id from a text scrape could
    // land on the output SINK and silence the whole box. `nodesFrom` filters to
    // `media.class == "Stream/Output/Audio"`, so only an app's own sink-inputs
    // are ever candidates. It also drops the jq dependency the old muter carried
    // (jq is not a declared dependency of this project).
    //
    // ~230 KB per cycle on a live box, and a cycle runs only when the displayed
    // workspace or the window set changes — a few times a minute, not per frame.
    Process {
        id: enumerateProc

        property string collected: ""

        // The trailing `echo` terminates that one long line. SplitParser splits on
        // newlines, and a final un-terminated chunk is not something to depend on.
        // `pipefail` is what makes the exit code mean anything: without it the
        // status would be `echo`'s, so a missing or crashed `pw-dump` would look
        // like success and the `exitCode` check below would be dead code.
        command: ["bash", "-c", "set -o pipefail; pw-dump 2>/dev/null | tr '\\n' ' '; rc=$?; echo; exit $rc"]

        stdout: SplitParser {
            onRead: line => {
                enumerateProc.collected += line;
            }
        }

        onExited: exitCode => {
            let text = enumerateProc.collected;
            enumerateProc.collected = "";
            muter._onGraph(exitCode === 0 ? text : "");
        }
    }

    // Decide and dispatch. A graph we could not read establishes nothing, so it
    // leaves the applied set alone rather than unmuting on bad data.
    function _onGraph(text) {
        let dump = null;
        if (text !== "") {
            try {
                dump = JSON.parse(text);
            } catch (e) {
                console.warn("WorkspaceAudioMuter: unparseable pw-dump output");
                dump = null;
            }
        }
        if (dump === null) {
            muter._finishCycle();
            return;
        }

        let nodes = AudioOwnership.nodesFrom(dump);

        // First graph we have ever seen: adopt whatever is already muted as ours
        // (see adoptableMutedIds). Mutes outlive the shell but `_appliedIds` does
        // not, so without this a node muted by the PREVIOUS instance is one this
        // instance will never release — and restarting the shell is the deploy
        // loop. Observed in the field as a live stream on the displayed
        // workspace playing to a muted node.
        if (!muter._adoptedExisting) {
            muter._adoptedExisting = true;
            muter._appliedIds = AudioOwnership.adoptableMutedIds(nodes);
        }

        let desired = AudioOwnership.desiredMutedIds(nodes, muter.runningWindows, muter.activeWorkspace);
        let diff = AudioOwnership.reconcile(desired, muter._appliedIds);

        let mute = diff.mute.filter(AudioOwnership.isValidNodeId);
        let unmute = diff.unmute.filter(AudioOwnership.isValidNodeId);
        if (mute.length === 0 && unmute.length === 0) {
            muter._finishCycle();
            return;
        }

        // Book the new applied set BEFORE the run, not after. `wpctl set-mute`
        // on a node that vanished between enumerate and apply fails harmlessly,
        // and a node that appeared is picked up by the next cycle — but a mute
        // we forgot we issued would never be undone, which is the one outcome
        // that strands a silent app.
        muter._appliedIds = AudioOwnership.nextApplied(muter._appliedIds, mute, unmute);

        applyProc.muteIds = mute.join(" ");
        applyProc.unmuteIds = unmute.join(" ");
        applyProc.running = true;
    }

    // Apply the diff in a single invocation. Ids are passed via env (they are
    // already validated as bare integers; env keeps them off the command line
    // regardless) and every `set-mute` tolerates a vanished node.
    Process {
        id: applyProc

        property string muteIds: ""
        property string unmuteIds: ""

        environment: ({
                "TV_SHELL_MUTE_IDS": muteIds,
                "TV_SHELL_UNMUTE_IDS": unmuteIds
            })
        command: ["bash", "-c", "for id in $TV_SHELL_MUTE_IDS; do wpctl set-mute \"$id\" 1 2>/dev/null || true; done; " + "for id in $TV_SHELL_UNMUTE_IDS; do wpctl set-mute \"$id\" 0 2>/dev/null || true; done"]

        // No notification either way. Muting a backgrounded app is a routine
        // internal consequence of switching workspaces, not an event worth
        // interrupting the screen for.
        onExited: muter._finishCycle()
    }

    // End a cycle, and run one more if inputs moved while it was in flight.
    //
    // The re-run goes through `Qt.callLater` so it never re-arms a Process from
    // inside that Process's own `onExited`. Whether that is safe depends on
    // whether Quickshell has cleared `running` by the time the handler runs;
    // deferring to the next event-loop pass means it does not have to be true.
    function _finishCycle() {
        cycleWatchdog.stop();
        muter._cycleRunning = false;
        if (muter._restartQueued) {
            muter._restartQueued = false;
            Qt.callLater(muter._evaluate);
        }
    }
}

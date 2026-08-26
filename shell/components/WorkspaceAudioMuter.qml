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
// Inputs, both injected by shell.qml:
//   - activeWorkspace ← appLifecycle.displayedWorkspace  (workspace NAME, "1" = home)
//   - runningWindows  ← appLifecycle.runningWindows      ([{windowClass, workspace, …}])
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

    // Node ids this component currently holds muted. The source of truth for
    // what to undo; nothing outside this list is ever unmuted.
    property var _appliedIds: []
    // Set while a cycle is in flight, so inputs changing mid-cycle queue one
    // more pass instead of interleaving two.
    property bool _cycleRunning: false
    property bool _restartQueued: false

    // Re-read the graph and reconcile. Every input change lands here.
    function _evaluate() {
        if (_cycleRunning) {
            // A snapshot taken before this change would decide on stale inputs.
            // Let the in-flight cycle finish and immediately run another.
            muter._restartQueued = true;
            return;
        }
        muter._cycleRunning = true;
        enumerateProc.collected = "";
        enumerateProc.running = true;
    }

    onActiveWorkspaceChanged: _evaluate()
    onRunningWindowsChanged: _evaluate()

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
        command: ["bash", "-c", "pw-dump 2>/dev/null | tr '\\n' ' '; echo"]

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
    function _finishCycle() {
        muter._cycleRunning = false;
        if (muter._restartQueued) {
            muter._restartQueued = false;
            _evaluate();
        }
    }
}

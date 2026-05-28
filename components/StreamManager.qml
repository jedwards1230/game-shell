import Quickshell.Io
import QtQuick

Item {
    id: root

    property var currentTarget: null
    property int crashCount: 0
    property string shellState: ""
    property string _lastStderr: ""
    property var _stderrLines: []

    // Active session state detected by pre-flight check
    property string activeSessionApp: ""
    property bool _sessionCheckCancelled: false
    property bool _forceKilled: false

    signal streamStarted
    signal streamEnded
    signal streamCrashed(int attempts)
    signal streamFailed(string message)
    signal requestOverlayShow(string msg)
    signal requestOverlayHide
    signal requestInputRelease
    signal requestInputGrab

    // Emitted when Sunshine reports a different app is already running
    signal sessionConflictDetected(string runningApp, string hostName)
    signal sessionCheckCancelled
    signal streamSuspended

    property bool _suspending: false

    function launch(target) {
        reconnectTimer.stop();
        errorDismissTimer.stop();
        currentTarget = target;
        crashCount = 0;
        _lastStderr = "";
        _stderrLines = [];
        activeSessionApp = "";
        _sessionCheckCancelled = false;
        _forceKilled = false;
        _suspending = false;
        ErrorLog.setCurrentTarget(target.name || target.app || "");

        if (target.sunshineUser && target.sunshinePass) {
            requestOverlayShow("Checking host...");
            _checkActiveSession();
        } else {
            requestOverlayShow("Launching " + (target.app || target.name) + "...");
            _launchMoonlight();
        }
    }

    function resumeSession() {
        _sessionCheckCancelled = false;
        requestOverlayShow("Resuming " + (currentTarget.app || currentTarget.name) + "...");
        _launchMoonlight();
    }

    function quitAndRelaunch() {
        _sessionCheckCancelled = false;
        let quitCmd = StreamProviders.active.quitArgs(currentTarget);
        if (!quitCmd || quitCmd.length === 0) {
            // Provider has nothing to quit — launch directly.
            requestOverlayShow("Launching " + (currentTarget.app || currentTarget.name) + "...");
            _launchMoonlight();
            return;
        }
        requestOverlayShow("Quitting current session...");
        sessionQuit.command = quitCmd;
        sessionQuit.running = true;
    }

    function cancelSessionCheck() {
        _sessionCheckCancelled = true;
        sessionCheckProc.running = false;
        sessionQuit.running = false;
        requestOverlayHide();
        sessionCheckCancelled();
    }

    function stop() {
        reconnectTimer.stop();
        errorDismissTimer.stop();
        moonlight.running = false;
    }

    function suspend() {
        _suspending = true;
        reconnectTimer.stop();
        errorDismissTimer.stop();
        moonlight.running = false;
    }

    function forceKill() {
        _forceKilled = true;
        stop();
        _sessionCheckCancelled = true;
        sessionCheckProc.running = false;
        sessionQuit.running = false;
        forceKillProc.running = true;
    }

    function _checkActiveSession() {
        let host = currentTarget.host;
        let port = currentTarget.sunshinePort || "47990";
        let user = currentTarget.sunshineUser;
        let pass = currentTarget.sunshinePass;
        sessionCheckProc._response = "";
        sessionCheckProc.command = ["curl", "-sk", "--connect-timeout", "3", "--max-time", "5", "--user", user + ":" + pass, "https://" + host + ":" + port + "/api/currentClient"];
        sessionCheckProc.running = true;
    }

    // Generic launch: the active provider supplies backend-specific argv; this
    // manager owns the launch/timeout/reconnect state machine.
    function _launchMoonlight() {
        let cmd = StreamProviders.active.buildLaunchArgs(currentTarget);
        if (!cmd || cmd.length === 0) {
            // No streaming backend (or it can't build args) — fail cleanly
            // instead of entering a bogus launching state with an empty command.
            requestOverlayHide();
            requestInputGrab();
            streamFailed("No streaming backend available");
            return;
        }
        moonlight.command = cmd;
        requestInputRelease();
        streamStarted();
        launchTimeout.restart();
        moonlight.running = true;
    }

    // Pre-flight session check via Sunshine API
    Process {
        id: sessionCheckProc
        property string _response: ""
        stdout: SplitParser {
            onRead: line => {
                sessionCheckProc._response += line;
            }
        }
        onExited: (exitCode, exitStatus) => {
            if (root._sessionCheckCancelled)
                return;

            let response = sessionCheckProc._response;
            sessionCheckProc._response = "";

            if (exitCode !== 0 || response === "") {
                root.requestOverlayShow("Launching " + (root.currentTarget.app || root.currentTarget.name) + "...");
                root._launchMoonlight();
                return;
            }

            try {
                let data = JSON.parse(response);
                let runningApp = data.currentApp || "";
                if (runningApp === "" || runningApp === root.currentTarget.app) {
                    root.requestOverlayShow("Launching " + (root.currentTarget.app || root.currentTarget.name) + "...");
                    root._launchMoonlight();
                } else {
                    root.activeSessionApp = runningApp;
                    root.requestOverlayHide();
                    root.sessionConflictDetected(runningApp, root.currentTarget.name || root.currentTarget.host);
                }
            } catch (e) {
                root.requestOverlayShow("Launching " + (root.currentTarget.app || root.currentTarget.name) + "...");
                root._launchMoonlight();
            }
        }
    }

    // Quit existing session then relaunch
    Process {
        id: sessionQuit
        onExited: {
            if (root._sessionCheckCancelled)
                return;
            root.requestOverlayShow("Launching " + (root.currentTarget.app || root.currentTarget.name) + "...");
            root._launchMoonlight();
        }
    }

    Process {
        id: moonlight
        stdout: SplitParser {
            onRead: line => {
                var trimmed = line.trim();
                if (trimmed === "")
                    return;
                launchTimeout.stop();
                var lines = root._stderrLines.slice();
                lines.push(trimmed);
                if (lines.length > 50)
                    lines = lines.slice(lines.length - 50);
                root._stderrLines = lines;
            }
        }
        stderr: SplitParser {
            onRead: line => {
                var trimmed = line.trim();
                if (trimmed === "")
                    return;
                launchTimeout.stop();
                root._lastStderr = trimmed;
                var lines = root._stderrLines.slice();
                lines.push(trimmed);
                if (lines.length > 50)
                    lines = lines.slice(lines.length - 50);
                root._stderrLines = lines;
            }
        }
        onExited: (exitCode, exitStatus) => {
            launchTimeout.stop();
            if (root._forceKilled)
                return;
            if (root._suspending) {
                root._suspending = false;
                root._lastStderr = "";
                root._stderrLines = [];
                root.requestOverlayHide();
                root.requestInputGrab();
                root.streamSuspended();
                return;
            }
            if (exitCode === 0) {
                root._lastStderr = "";
                root._stderrLines = [];
                root.requestOverlayHide();
                root.requestInputGrab();
                root.streamEnded();
            } else {
                root.crashCount++;
                ErrorLog.log("moonlight", "Stream exit code " + exitCode + " (attempt " + root.crashCount + ")", root._stderrLines.join("\n"));
                if (root.crashCount < 5) {
                    root.requestOverlayShow("Reconnecting... (" + root.crashCount + "/5)\n" + (root._lastStderr || ""));
                    root.streamCrashed(root.crashCount);
                    reconnectTimer.start();
                } else {
                    let msg = root._lastStderr || "Stream failed after 5 attempts";
                    root.requestOverlayShow(msg);
                    errorDismissTimer.start();
                    root.requestInputGrab();
                    root.streamFailed(msg);
                }
            }
        }
    }

    Process {
        id: forceKillProc
        command: ["bash", "-c", "pkill -f moonlight; pkill -f steam; true"]
    }

    Timer {
        id: reconnectTimer
        interval: 2000
        onTriggered: root._launchMoonlight()
    }

    Timer {
        id: errorDismissTimer
        interval: 5000
        onTriggered: root.requestOverlayHide()
    }

    Timer {
        id: launchTimeout
        interval: 30000
        onTriggered: {
            if (moonlight.running) {
                moonlight.running = false;
                forceKillProc.running = true;
                let msg = "Stream launch timed out after 30s";
                ErrorLog.log("moonlight", msg, root._stderrLines.join("\n"));
                root.requestOverlayShow(msg);
                errorDismissTimer.start();
                root.requestInputGrab();
                root.streamFailed(msg);
            }
        }
    }

    Timer {
        id: crashResetTimer
        interval: 300000
        running: root.shellState === "streaming"
        onTriggered: {
            root.crashCount = 0;
        }
    }
}

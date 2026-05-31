import QtQuick
import Quickshell.Io

BaseCard {
    id: root

    required property var target
    property string appName: ""
    property bool isOnline: false
    property string shellState: "idle"
    property bool hasActiveSession: false
    property string activeAppName: ""

    label: root.appName !== "" ? root.appName : (root.target.name || "Unknown")

    Process {
        id: pingCheck
        command: ["ping", "-c1", "-W1", root.target.host || "127.0.0.1"]
        onExited: (exitCode, exitStatus) => {
            root.isOnline = (exitCode === 0);
        }
    }

    Timer {
        interval: 10000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (!pingCheck.running)
                pingCheck.running = true;
        }
    }

    // Resolve the running app name via the daemon's `sunshine-status` IPC command
    // (see docs/IPC_PROTOCOL.md) + the local Moonlight config. The daemon owns the
    // Sunshine /serverinfo HTTPS fetch and returns {online,paired,currentApp,...},
    // where currentApp is the busy game id (or "" when idle). Phase 8 (#97) moved
    // the Unix-socket call onto SocketClient and the game-id -> friendly-name
    // resolution onto MoonlightConf, preserving the prior contract (idle / a
    // resolved name / "Unknown App").
    property bool _checkInFlight: false
    property string _pendingGameId: ""

    MoonlightConf {
        id: moonlightConf
        onLoaded: {
            // Conf (re)loaded after a busy reply we couldn't resolve yet.
            if (root._pendingGameId !== "") {
                let name = moonlightConf.nameFor(root._pendingGameId);
                root.hasActiveSession = true;
                root.activeAppName = name !== "" ? name : "Unknown App";
                root._pendingGameId = "";
            }
            root._checkInFlight = false;
        }
    }

    SocketClient {
        id: sessionCheck
        onResponseReceived: line => {
            try {
                let data = JSON.parse(line);
                let gameId = String(data.currentApp || "");
                if (gameId === "" || gameId === "0") {
                    root.hasActiveSession = false;
                    root.activeAppName = "";
                    root._checkInFlight = false;
                    return;
                }
                // Busy: resolve the id to a name. Try the cached map first; if it
                // is empty/unloaded, (re)read the conf and resolve in onLoaded.
                let name = moonlightConf.nameFor(gameId);
                if (name !== "" || moonlightConf._loaded) {
                    root.hasActiveSession = true;
                    root.activeAppName = name !== "" ? name : "Unknown App";
                    root._checkInFlight = false;
                } else {
                    root._pendingGameId = gameId;
                    moonlightConf.load();
                }
            } catch (e) {
                // Treat a malformed reply as idle (matches the old fallback).
                root.hasActiveSession = false;
                root.activeAppName = "";
                root._checkInFlight = false;
            }
        }
        onRequestFailed: {
            root.hasActiveSession = false;
            root.activeAppName = "";
            root._checkInFlight = false;
        }
    }

    Timer {
        interval: 10000
        running: root.shellState === "idle" && root.isOnline
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (root._checkInFlight)
                return;
            let host = root.target.host || "127.0.0.1";
            let port = root.target.sunshinePort || "47990";
            root._checkInFlight = true;
            root._pendingGameId = "";
            // Ask the daemon for the Sunshine session state (one compact JSON
            // line), then resolve the busy game id to a friendly name from the
            // local Moonlight.conf via MoonlightConf — exactly as before.
            sessionCheck.request("sunshine-status " + host + " " + port);
        }
    }

    Text {
        anchors.fill: parent
        text: root.appName !== "" ? root.appName.charAt(0).toUpperCase() : "\u{1F3AE}"
        font.pixelSize: 96
        font.bold: root.appName !== ""
        color: root.appName !== "" ? Theme.textSecondary : Theme.textPrimary
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }

    Rectangle {
        id: sessionBadge
        visible: root.hasActiveSession && (root.appName === "" || root.activeAppName === root.appName || root.activeAppName === (root.target.app || ""))
        height: 24
        radius: 12
        color: Theme.online
        width: badgeText.implicitWidth + 16
        anchors.top: parent.top
        anchors.right: parent.right
        anchors.topMargin: 8
        anchors.rightMargin: 8

        Text {
            id: badgeText
            anchors.centerIn: parent
            text: "LIVE"
            font.pixelSize: 12
            font.bold: true
            color: "#ffffff"
        }
    }
}

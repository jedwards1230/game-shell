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

    // Query GS serverinfo + local Moonlight config to resolve running app name.
    Process {
        id: sessionCheck
        property string _response: ""
        stdout: SplitParser {
            onRead: line => {
                sessionCheck._response = line.trim();
            }
        }
        onExited: (exitCode, exitStatus) => {
            let name = sessionCheck._response;
            sessionCheck._response = "";
            if (exitCode !== 0 || name === "" || name === "IDLE") {
                root.hasActiveSession = false;
                root.activeAppName = "";
            } else {
                root.hasActiveSession = true;
                root.activeAppName = name;
            }
        }
    }

    Timer {
        interval: 10000
        running: root.shellState === "idle" && root.isOnline
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (sessionCheck.running)
                return;
            let host = root.target.host || "127.0.0.1";
            sessionCheck._response = "";
            sessionCheck.command = ["python3", "-c", `
import urllib.request, re, configparser, os, sys
try:
    r = urllib.request.urlopen("http://${host}:47989/serverinfo", timeout=3).read().decode()
    state = re.search(r"<state>([^<]+)</state>", r)
    gid = re.search(r"<currentgame>([^<]+)</currentgame>", r)
    if not state or state.group(1) != "SUNSHINE_SERVER_BUSY" or not gid or gid.group(1) == "0":
        print("IDLE")
        sys.exit(0)
    game_id = gid.group(1)
    conf = os.path.expanduser("~/.config/Moonlight Game Streaming Project/Moonlight.conf")
    if os.path.exists(conf):
        cp = configparser.ConfigParser()
        cp.read(conf)
        for k, v in cp.items("hosts"):
            if k.endswith("\\\\id") and v == game_id:
                name_key = k.replace("\\\\id", "\\\\name")
                if cp.has_option("hosts", name_key):
                    print(cp.get("hosts", name_key))
                    sys.exit(0)
    print("Unknown App")
except Exception:
    print("IDLE")
`];
            sessionCheck.running = true;
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

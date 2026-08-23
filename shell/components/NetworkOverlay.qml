import QtQuick
import QtQuick.Layouts
import Quickshell.Io
import "lib"

// Compact network quick-popover, anchored beside the originating QuickAction
// glyph (#118). Controller-navigable, 10-foot sized. Mirrors VolumeOverlay's
// anchored-popover pattern: FocusScope + visible:opened + light scrim +
// onOpenedChanged forces focus; the panel positions itself relative to
// anchorRect (scene-root coords) and clamps fully on-screen.
// Stats sourced from the daemon's net-status + net-throughput IPC (read-only);
// surfaces the IPv4 address, live ↓/↑ speeds (sampled via net-throughput), and
// a two-step disconnect-with-warning toggle via nmcli so an accidental A on a
// couch never drops the network (and Moonlight). The disconnect toggle is only
// offered on Wi-Fi — on a wired/ethernet link the popover is status-only (no
// disable affordance), since turning networking off from the couch can strand a
// wired box with no easy recovery. B/Escape closes.
FocusScope {
    id: root

    property bool opened: false

    // Scene-root rect {x, y, w, h} of the glyph that opened this popover.
    property var anchorRect: null

    // --- Network state (from net-status daemon IPC) ---
    property string connName: ""
    property string connType: ""
    property string ipAddress: ""
    property string device: ""         // e.g. "enp3s0" — derived from activeConnections
    property bool ifaceUp: true        // reflect current link state
    property bool statusLoaded: false

    // Wired/ethernet links hide the disconnect affordance entirely — turning
    // networking off from the couch on a wired box can strand it. Derived from
    // the connType the daemon's net-status already reports (NM type string).
    readonly property bool isWired: root.connType === "802-3-ethernet"

    // --- Live speed state ---
    property real _prevRxBytes: -1
    property real _prevTxBytes: -1
    property string downSpeed: "—"
    property string upSpeed: "—"

    // --- Toggle confirm state ---
    // 0 = normal, 1 = confirm-off pending
    property int _confirmStep: 0
    // Focus row: 0 = toggle, 1 = confirm-yes (only when _confirmStep=1)
    property int _focusRow: 0

    visible: opened
    anchors.fill: parent
    focus: opened

    function openAt(rect) {
        root.anchorRect = rect;
        root.opened = true;
        Qt.callLater(function () {
            root.forceActiveFocus();
        });
    }

    onOpenedChanged: {
        if (opened) {
            Qt.callLater(function () {
                root.forceActiveFocus();
            });
            root._prevRxBytes = -1;
            root._prevTxBytes = -1;
            root.downSpeed = "—";
            root.upSpeed = "—";
            root._confirmStep = 0;
            root._focusRow = 0;
            netStatus.request("net-status");
        } else {
            speedTimer.stop();
        }
    }

    // --- net-status daemon IPC (SocketClient, read-only) ---
    SocketClient {
        id: netStatus
        onResponseReceived: line => {
            try {
                let obj = JSON.parse(line);
                root.ipAddress = obj.ipv4 || "";
                // Find the primary non-loopback active connection
                let conns = Array.isArray(obj.activeConnections) ? obj.activeConnections : [];
                let primary = null;
                for (let i = 0; i < conns.length; i++) {
                    let c = conns[i];
                    if (c.device && c.device !== "lo") {
                        primary = c;
                        break;
                    }
                }
                if (primary) {
                    root.connName = primary.name || "";
                    root.connType = primary.type || "";
                    root.device = primary.device || "";
                    root.ifaceUp = true;
                } else {
                    root.connName = "";
                    root.connType = "";
                    root.device = "";
                    root.ifaceUp = false;
                }
                root.statusLoaded = true;
                // Start speed sampling once we know the device
                if (root.device !== "")
                    speedTimer.start();
            } catch (e) {
                console.log("NetworkOverlay: failed to parse net-status:", e);
                root.statusLoaded = true;
            }
        }
    }

    // --- Live speed sampling via the daemon's net-throughput IPC ---
    // The daemon reads /sys/class/net/<iface>/statistics/{rx,tx}_bytes and returns
    // the cumulative counters; we sample on a 1 s timer and compute the delta
    // ourselves (same model as the old bash one-liner, no shell-out). Fail-soft:
    // an unknown iface comes back zeroed with an `error` field — we just leave the
    // speeds dashed rather than special-casing.
    SocketClient {
        id: readBytes
        onResponseReceived: line => {
            try {
                let obj = JSON.parse(line);
                let rx = obj.rxBytes || 0;
                let tx = obj.txBytes || 0;
                if (!obj.error && root._prevRxBytes >= 0) {
                    let downBps = Math.max(0, rx - root._prevRxBytes);
                    let upBps = Math.max(0, tx - root._prevTxBytes);
                    root.downSpeed = _formatSpeed(downBps);
                    root.upSpeed = _formatSpeed(upBps);
                }
                root._prevRxBytes = rx;
                root._prevTxBytes = tx;
            } catch (e) {
                console.log("NetworkOverlay: failed to parse net-throughput:", e);
            }
        }
    }

    Timer {
        id: speedTimer
        interval: 1000
        repeat: true
        running: false
        onTriggered: {
            if (root.device === "") {
                speedTimer.stop();
                return;
            }
            readBytes.request("net-throughput", root.device);
        }
    }

    function _formatSpeed(bytesPerSec) {
        if (bytesPerSec < 1024)
            return bytesPerSec.toFixed(0) + " B/s";
        if (bytesPerSec < 1048576)
            return (bytesPerSec / 1024).toFixed(1) + " KB/s";
        return (bytesPerSec / 1048576).toFixed(2) + " MB/s";
    }

    function _typeLabel(t) {
        if (t === "802-3-ethernet")
            return "Ethernet";
        if (t === "802-11-wireless")
            return "WiFi";
        return t || "Unknown";
    }

    // --- nmcli toggle processes ---
    Process {
        id: nmcliDisconnect
        property string dev: ""
        command: ["nmcli", "device", "disconnect", dev]
        onExited: {
            root._confirmStep = 0;
            root._focusRow = 0;
            root.ifaceUp = false;
            // Re-query status after a short delay
            reloadTimer.start();
        }
    }

    Process {
        id: nmcliConnect
        property string dev: ""
        command: ["nmcli", "device", "connect", dev]
        onExited: {
            root.ifaceUp = true;
            reloadTimer.start();
        }
    }

    Timer {
        id: reloadTimer
        interval: 1200
        onTriggered: netStatus.request("net-status")
    }

    // --- Key handling ---
    Keys.onPressed: event => {
        if (event.key === Qt.Key_Up) {
            if (root._confirmStep === 1 && root._focusRow === 1)
                root._focusRow = 0;
        } else if (event.key === Qt.Key_Down) {
            if (root._confirmStep === 1 && root._focusRow === 0)
                root._focusRow = 1;
        } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            if (root.isWired)
            // Wired link: no disable affordance — A is a no-op.
            {} else if (root._focusRow === 0) {
                // Toggle row — A activates
                if (root.ifaceUp) {
                    // First press enters confirm step
                    root._confirmStep = 1;
                    root._focusRow = 0;
                } else {
                    // Reconnect — no confirm needed, reconnect is safe
                    if (root.device !== "") {
                        nmcliConnect.dev = root.device;
                        nmcliConnect.running = true;
                    }
                }
            } else if (root._focusRow === 1 && root._confirmStep === 1) {
                // Confirm disconnect
                if (root.device !== "") {
                    nmcliDisconnect.dev = root.device;
                    nmcliDisconnect.running = true;
                }
            }
        } else if (event.key === Qt.Key_Escape || (event.key === Qt.Key_B && !event.modifiers)) {
            if (root._confirmStep === 1) {
                // Cancel the confirm step
                root._confirmStep = 0;
                root._focusRow = 0;
            } else {
                root.opened = false;
            }
        }
        // Modal: block all keys from reaching items below.
        event.accepted = true;
    }

    AnchoredPopover {
        anchorRect: root.anchorRect
        panelWidth: Units.gridUnit * 24
        onDismissed: root.opened = false

        // --- Title ---
        Text {
            Layout.alignment: Qt.AlignHCenter
            text: "Network"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        // --- Status card: connection facts + live throughput in one card ---
        // One cohesive card (name/type, IP + interface, live ↓/↑ speeds)
        // instead of two stacked cards — so the wired status-only view reads as
        // a single intentional block, not two fragments with a gap between them.
        Rectangle {
            Layout.fillWidth: true
            // Size via Layout.preferredHeight (not plain `height`) so the parent
            // ColumnLayout positions the next row (HintBar) below the card — a
            // plain `height` is not reflected in the layout, which lets the hint
            // overlap the card.
            Layout.preferredHeight: statusCol.implicitHeight + Units.spacingLG * 2
            radius: Units.radiusMD
            color: Theme.cardBackground
            border.width: Units.borderThin
            border.color: Theme.surfaceBorder

            ColumnLayout {
                id: statusCol
                anchors {
                    left: parent.left
                    right: parent.right
                    top: parent.top
                    margins: Units.spacingLG
                }
                spacing: Units.spacingSM

                // Connection name + medium (e.g. "Wired connection 1 · Ethernet").
                Text {
                    visible: root.statusLoaded && root.connName !== ""
                    text: root.connName + " · " + root._typeLabel(root.connType)
                    font.pixelSize: Theme.fontHint
                    font.bold: true
                    color: Theme.textPrimary
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                // IPv4 address + interface on one line (label · value · iface),
                // so the static facts stay compact above the live speeds.
                RowLayout {
                    visible: root.statusLoaded && root.ipAddress !== ""
                    Layout.fillWidth: true
                    spacing: Units.spacingSM

                    Text {
                        text: "IP"
                        font.pixelSize: Theme.fontHint
                        color: Theme.textMuted
                    }
                    Text {
                        Layout.fillWidth: true
                        // The daemon's ipv4 already carries the interface
                        // (e.g. "enp3s0: 192.0.2.42"), so there's no separate
                        // interface label — it would duplicate the iface.
                        text: root.ipAddress
                        font.pixelSize: Theme.fontHint
                        font.family: "monospace"
                        color: Theme.textSecondary
                        elide: Text.ElideRight
                    }
                }

                Text {
                    visible: root.statusLoaded && root.connName === ""
                    text: "No active connection"
                    font.pixelSize: Theme.fontHint
                    color: Theme.textMuted
                }

                Text {
                    visible: !root.statusLoaded
                    text: "Loading…"
                    font.pixelSize: Theme.fontHint
                    color: Theme.textMuted
                }

                // Live ↓/↑ throughput — separated from the static facts above by
                // a little extra space rather than a divider (house style: no
                // in-card rules). Only shown once we know the interface.
                RowLayout {
                    id: speedRow
                    visible: root.device !== ""
                    Layout.fillWidth: true
                    spacing: Units.spacingXL

                    Text {
                        Layout.fillWidth: true
                        text: "↓ " + root.downSpeed
                        font.pixelSize: Theme.fontHint
                        font.bold: true
                        color: Theme.online
                        horizontalAlignment: Text.AlignLeft
                    }

                    Text {
                        Layout.fillWidth: true
                        text: "↑ " + root.upSpeed
                        font.pixelSize: Theme.fontHint
                        font.bold: true
                        color: Theme.ember
                        horizontalAlignment: Text.AlignRight
                    }
                }
            }
        }

        // --- Divider (only above the actionable toggle — hidden on wired) ---
        Rectangle {
            Layout.fillWidth: true
            height: 2
            color: Theme.surfaceBorder
            visible: root.device !== "" && !root.isWired
        }

        // --- Interface toggle (Wi-Fi only — wired is status-only) ---
        Rectangle {
            id: toggleRow
            Layout.fillWidth: true
            height: Units.gridUnit * 1.6
            radius: Units.radiusMD
            visible: root.device !== "" && !root.isWired

            // Focused when _focusRow === 0
            readonly property bool rowFocused: root.activeFocus && root._focusRow === 0

            color: {
                if (rowFocused)
                    return Theme.surfaceHover;
                return Theme.cardBackground;
            }
            border.width: rowFocused ? Units.borderMedium : Units.borderThin
            border.color: rowFocused ? Theme.focusBorder : Theme.surfaceBorder

            Behavior on color {
                ColorAnimation {
                    duration: 100
                }
            }

            RowLayout {
                anchors {
                    fill: parent
                    leftMargin: Units.spacingLG
                    rightMargin: Units.spacingLG
                }
                spacing: Units.spacingMD

                // Status dot
                Rectangle {
                    width: Units.gridUnit * 0.44
                    height: Units.gridUnit * 0.44
                    radius: width / 2
                    color: root.ifaceUp ? Theme.online : Theme.offline
                }

                Text {
                    Layout.fillWidth: true
                    text: "Network: " + (root.ifaceUp ? "On" : "Off")
                    font.pixelSize: Theme.fontHint
                    font.bold: true
                    color: Theme.textPrimary
                }

                Text {
                    visible: toggleRow.rowFocused
                    text: root.ifaceUp ? "A: Turn off" : "A: Turn on"
                    font.pixelSize: Theme.fontHint
                    color: root.ifaceUp ? Theme.warning : Theme.online
                }
            }

            MouseArea {
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onEntered: {
                    root._focusRow = 0;
                }
                onClicked: {
                    root._focusRow = 0;
                    if (root.ifaceUp) {
                        root._confirmStep = 1;
                    } else if (root.device !== "") {
                        nmcliConnect.dev = root.device;
                        nmcliConnect.running = true;
                    }
                }
            }
        }

        // --- Confirm-disconnect row (only when _confirmStep === 1) ---
        Rectangle {
            id: confirmRow
            Layout.fillWidth: true
            height: confirmCol.implicitHeight + Units.spacingLG * 2
            radius: Units.radiusMD
            visible: root._confirmStep === 1

            // Focused when _focusRow === 1 (in confirm mode)
            readonly property bool rowFocused: root.activeFocus && root._focusRow === 1 && root._confirmStep === 1

            color: rowFocused ? Theme.crimson : Qt.darker(Theme.crimson, 1.5)
            border.width: rowFocused ? Units.borderMedium : Units.borderThin
            border.color: rowFocused ? Theme.textOnDark : Theme.crimson

            Behavior on color {
                ColorAnimation {
                    duration: 100
                }
            }

            ColumnLayout {
                id: confirmCol
                anchors {
                    left: parent.left
                    right: parent.right
                    top: parent.top
                    margins: Units.spacingLG
                }
                spacing: Units.spacingXS

                Text {
                    Layout.fillWidth: true
                    text: "⚠  This will disconnect the network"
                    font.pixelSize: Theme.fontHint
                    font.bold: true
                    color: Theme.textOnDark
                    wrapMode: Text.Wrap
                }

                Text {
                    Layout.fillWidth: true
                    text: "Moonlight streaming and remote access will stop."
                    font.pixelSize: Theme.fontHint
                    color: Theme.textOnDarkMuted
                    wrapMode: Text.Wrap
                }

                Text {
                    visible: confirmRow.rowFocused
                    text: "A: Confirm disconnect"
                    font.pixelSize: Theme.fontHint
                    font.bold: true
                    color: Theme.textOnDark
                }

                Text {
                    visible: !confirmRow.rowFocused
                    text: "↓ navigate here, then A to confirm"
                    font.pixelSize: Theme.fontHint
                    color: Theme.textOnDarkMuted
                }
            }

            MouseArea {
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onEntered: {
                    root._focusRow = 1;
                }
                onClicked: {
                    root._focusRow = 1;
                    if (root.device !== "") {
                        nmcliDisconnect.dev = root.device;
                        nmcliDisconnect.running = true;
                    }
                }
            }
        }

        // --- Hint bar ---
        HintBar {
            muted: true
            text: {
                if (root._confirmStep === 1)
                    return "▲▼ Navigate    A: Confirm    B: Cancel";
                // Wired link has no toggle — close-only.
                return root.isWired ? "B: Close" : "A: Toggle    B: Close";
            }
        }
    }
}

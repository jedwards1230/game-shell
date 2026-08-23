import QtQuick
import QtQuick.Layouts
import "../components"
import "../components/lib"

// Server surface for the Steam widget's config page (Widgets ▸ Steam) — the
// counterpart to Moonlight's inlined server management, but for OUR custom
// helper: the `tv-shell-host` sidecar that enumerates the host's Steam library.
// It shows WHICH configured sidecar the daemon is actively checking and lets
// you switch between the `[[steam.hosts]]` entries from the couch — no SSH.
//
// Roster comes from the `steam-hosts` IPC ({status,active,hosts:[{name,host}]});
// a selection fires `steam-set-host <name>` (persisted daemon-side as
// `steamServer` in settings.json), so the next `steam-library` poll, health
// probe, and the Wake card (which targets the reply's `host`) all follow the
// new server. ALWAYS rendered on the Steam config (mirroring Moonlight) — with
// no hosts it shows an explanatory empty state rather than collapsing, so the
// helper's configuration is always discoverable here.
FocusScope {
    id: root

    // The control above this section (the last manifest control row) — Up from
    // the picker returns focus there. Wired by WidgetConfig's Loader, mirroring
    // MoonlightSettings' embedded upTarget.
    property Item upTarget: null

    // {name, host} roster + active name from the latest `steam-hosts` reply.
    property var hosts: []
    property string active: ""
    // Whether there's a server to pick — gates the focus drop-in from the page
    // (an empty-state has nothing focusable). Distinct from `visible`: the
    // section is always shown, but only focusable when it has hosts.
    readonly property bool available: root.hosts.length > 0

    implicitHeight: col.implicitHeight

    function refresh() {
        hostsReq.request("steam-hosts");
    }
    Component.onCompleted: root.refresh()

    // The active entry's host part (IP/hostname), for the caption.
    function _activeHostAddr() {
        for (var i = 0; i < root.hosts.length; i++) {
            if (root.hosts[i].name === root.active)
                return root.hosts[i].host || "";
        }
        return "";
    }

    Keys.onUpPressed: if (root.upTarget)
        root.upTarget.forceActiveFocus()

    SocketClient {
        id: hostsReq
        onResponseReceived: response => {
            let d = null;
            try {
                d = JSON.parse(response);
            } catch (e) {
                console.warn("[SteamServerSettings] unparseable steam-hosts reply");
                return;
            }
            if (!d || (d.status !== "ok" && d.status !== "disabled")) {
                // A transient failure — keep the last-good roster.
                return;
            }
            root.hosts = d.hosts || [];
            root.active = d.active || "";
        }
        onRequestFailed: console.warn("[SteamServerSettings] steam-hosts request failed")
    }

    SocketClient {
        id: setReq
        // Re-read the roster so `active` reflects what the daemon persisted
        // (an `error:*` reply — e.g. a raced config reload — reverts the
        // optimistic selection below).
        onResponseReceived: () => root.refresh()
        onRequestFailed: console.warn("[SteamServerSettings] steam-set-host request failed")
    }

    ColumnLayout {
        id: col
        anchors.left: parent.left
        anchors.right: parent.right
        spacing: Units.spacingSM

        // Section header — names the surface so the Steam config reads as
        // "this is about the Steam host helper", parallel to Moonlight's.
        Text {
            text: "Steam Host"
            font.pixelSize: Theme.fontTitle
            font.bold: true
            color: Theme.textPrimary
        }

        // --- Empty state: no [steam] host configured ---
        Text {
            visible: !root.available
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            text: "No Steam host configured. Add a [steam] url or [[steam.hosts]] entries in config.toml (see the tv-shell-host helper on the gaming PC)."
            font.pixelSize: Theme.fontBody
            color: Theme.textMuted
        }

        // --- Server picker (one or more hosts) ---
        RowLayout {
            visible: root.available
            Layout.fillWidth: true
            spacing: Units.spacingLG

            Text {
                text: "Server"
                font.pixelSize: Theme.fontBody
                color: Theme.textSecondary
                Layout.alignment: Qt.AlignVCenter
                Layout.preferredWidth: Units.gridUnit * 8
            }

            SettingsButtonGroup {
                id: chips
                focus: root.available
                Layout.alignment: Qt.AlignVCenter
                options: {
                    let out = [];
                    for (var i = 0; i < root.hosts.length; i++)
                        out.push({
                            "label": root.hosts[i].name || root.hosts[i].host || "?",
                            "value": root.hosts[i].name
                        });
                    return out;
                }
                isCurrentOption: opt => opt.value === root.active
                onValueSelected: opt => {
                    // Optimistic: reflect the choice immediately; the set reply
                    // triggers a roster re-read that confirms (or reverts) it.
                    root.active = opt.value;
                    setReq.request("steam-set-host", opt.value);
                }
            }

            Item {
                Layout.fillWidth: true
            }
        }

        // e.g. "Checking gaming-pc (192.0.2.10)" — the address the Wake card
        // targets while this server is down.
        Text {
            visible: root.available && root.active !== ""
            text: "Checking " + root.active + (root._activeHostAddr() !== "" ? " (" + root._activeHostAddr() + ")" : "")
            font.pixelSize: Theme.fontSmall
            color: Theme.textMuted
            Layout.leftMargin: Units.gridUnit * 8 + Units.spacingLG
        }
    }
}

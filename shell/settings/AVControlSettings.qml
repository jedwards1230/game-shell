import QtQuick
import QtQuick.Layouts
import Quickshell.Io
import "../components"
import "../components/lib"

// AV Control settings — rewritten (#16) to consume the daemon's `cec-*` IPC
// instead of shelling out to cec-ctl / cec-client / living-room-cec. The daemon
// owns one persistent in-process libcec connection (cec-rs), which fixes the
// intermittent detection the per-call subprocess approach suffered from.
//
// IPC commands used (see docs/IPC_PROTOCOL.md):
//   cec-scan              -> JSON array of {logicalAddress, powerStatus}
//   cec-power-on <addr>   -> ok | error:*
//   cec-power-off <addr>  -> ok | error:*
//   cec-active-source     -> ok | error:*  ("Switch Input" = set active source)
//
// Subscribe events (prefix-matched on the subscribe stream):
//   cec:device:<json>  -> {logicalAddress, powerStatus}; merge into root.devices
//   cec:power:<json>   -> {addr, power}; update powerStatus for that device
//
// NOTE: cec-rs 12.0.1 wraps no per-device metadata query (OSD name, physical
// address, vendor, type), so the daemon emits only logicalAddress + powerStatus.
// Friendly names are derived locally from the logical address. The former
// living-room-cec / cec-ctl / cec-client QML fallback chain is removed — when
// the daemon is built without `--features cec` (or libcec is absent at runtime)
// every cec-* command replies error:* and this panel shows an unavailable card.
//
// Root MUST be a FocusScope (not Item) so SettingsApp's
// contentLoader.item.forceActiveFocus() delegates focus into the focus:true
// child (wakeScope). A plain Item swallows focus and the page becomes
// unnavigable when entered via the Right d-pad.
SettingsPageBase {
    id: root
    hintText: root.cecAvailable ? "A: Set as default input  |  Auto-refresh every 30s" : root.cecUnavailableHint()

    property bool cecAvailable: false
    property var devices: []
    property string statusText: "Checking CEC availability..."
    property string actionFeedback: ""

    // CEC transmit-link health (#19). The adapter periodically "wedges": it
    // still OPENS and can RECEIVE (so cec-scan returns [] and cecAvailable stays
    // true) but every TRANSMIT fails, leaving the user unable to reclaim the AVR
    // input from the couch. The daemon now tracks transmit health and exposes it
    // over IPC; this property drives the CEC link status line below.
    //   "ok"      — transmits are succeeding
    //   "failing" — adapter open + receiving but transmits fail (wedged)
    //   "unknown" — not yet probed / old daemon / parse error
    property string cecTransmitHealth: "unknown"
    property string cecHealthLastError: ""

    // Why the adapter is unavailable, when it is (#22). The daemon distinguishes
    // three cases via the `reason` field on a `transmit:"unavailable"` health
    // reply — the page surfaces an accurate, actionable card per reason instead
    // of one misleading "no CEC adapter" message:
    //   "no_libcec"           — daemon built without libcec support
    //   "no_adapter"          — built with libcec but no USB adapter present
    //   "adapter_open_failed" — adapter present but won't open (hardware-wedged)
    //   "adapter_busy"        — adapter present + healthy, but ANOTHER owner holds
    //                           its exclusive serial lock (kernel pulse8_cec driver,
    //                           or another CEC app such as Plex HTPC). Split out of
    //                           adapter_open_failed, which told the operator to
    //                           re-seat hardware that was working fine.
    //   ""                    — not yet known / adapter is available (cleared)
    property string cecUnavailableReason: ""

    // Parse a `cec-health` / `cec-test` reply or a `cec:health:` event payload
    // (compact JSON: {"transmit":"ok"|"failing"|"unknown"|"unavailable",
    // "reason":<null|"no_libcec"|"no_adapter"|"adapter_open_failed"|"adapter_busy">,
    // "since":<ms>,"lastError":<string|null>}). Defensive: any non-object reply
    // (error:* from an old/unavailable daemon) or parse failure resolves to
    // "unknown" and clears the unavailable reason.
    function applyHealth(jsonText) {
        var t = (jsonText || "").trim();
        if (t.length === 0 || t[0] !== "{") {
            root.cecTransmitHealth = "unknown";
            root.cecHealthLastError = "";
            root.cecUnavailableReason = "";
            return;
        }
        try {
            var obj = JSON.parse(t);
            var tx = obj.transmit;
            root.cecTransmitHealth = (tx === "ok" || tx === "failing") ? tx : "unknown";
            root.cecHealthLastError = (obj.lastError !== undefined && obj.lastError !== null) ? String(obj.lastError) : "";
            // Only an "unavailable" reply carries a meaningful reason; for any
            // open/available state (ok/failing/unknown) the adapter works, so
            // clear it. Unknown reason strings collapse to "" (generic copy).
            if (tx === "unavailable") {
                var r = obj.reason;
                root.cecUnavailableReason = (r === "no_libcec" || r === "no_adapter" || r === "adapter_open_failed" || r === "adapter_busy") ? r : "";
            } else {
                root.cecUnavailableReason = "";
            }
        } catch (e) {
            console.log("AVControlSettings: failed to parse cec-health:", e);
            root.cecTransmitHealth = "unknown";
            root.cecHealthLastError = "";
            root.cecUnavailableReason = "";
        }
    }

    // --- Per-reason copy for the "HDMI-CEC unavailable" card + footer hint ---
    // The card shows via the existing `!cecAvailable` gate; these drive its
    // title/body (and the footer line) off `cecUnavailableReason` so a
    // hardware-wedged adapter no longer reads as a missing one.
    function cecUnavailableTitle() {
        switch (root.cecUnavailableReason) {
        case "no_adapter":
            return "No CEC Adapter";
        case "adapter_open_failed":
            return "CEC Adapter Not Responding";
        case "adapter_busy":
            return "CEC Adapter In Use";
        default:
            // "no_libcec" and the not-yet-known fallback share the generic copy.
            return "HDMI-CEC Not Available";
        }
    }

    function cecUnavailableBody() {
        switch (root.cecUnavailableReason) {
        case "no_adapter":
            return "No CEC adapter detected — plug in the USB CEC adapter.";
        case "adapter_open_failed":
            return "CEC adapter detected but not responding — re-seat the USB adapter or power-cycle the AVR (pull mains, not standby), then retry.";
        case "adapter_busy":
            return "The CEC adapter is working, but another program or driver holds it — the adapter allows only one owner at a time. Quit the other CEC app (or stop the kernel pulse8-cec service), then restart the input daemon. The daemon log names the exact claimant. Do NOT re-seat the adapter; the hardware is fine.";
        default:
            return "CEC requires the daemon built with libcec support.";
        }
    }

    function cecUnavailableHint() {
        switch (root.cecUnavailableReason) {
        case "no_libcec":
            return "HDMI-CEC unavailable — daemon built without libcec support";
        case "no_adapter":
            return "HDMI-CEC unavailable — no CEC adapter detected";
        case "adapter_open_failed":
            return "CEC adapter detected but not responding — re-seat it";
        case "adapter_busy":
            return "CEC adapter held by another program — see the daemon log";
        default:
            return "HDMI-CEC unavailable";
        }
    }

    // Friendly label for a CEC logical address (no OSD name in cec-rs 12.0.1).
    function nameForAddress(addr) {
        // Prefer a local name override (#16, set via the config file — a freeform
        // on-screen editor is deferred to #20). Keyed by the stringified address.
        var o = SettingsStore.cecDeviceNames[String(addr)];
        if (o && o.length)
            return o;
        // CEC logical-address table (HDMI-CEC spec), consistent with the
        // daemon's cec-rs CecLogicalAddress semantics.
        switch (addr) {
        case 0:
            return "TV";
        case 1:
            return "Recorder 1";
        case 2:
            return "Recorder 2";
        case 9:
            return "Recorder 3";
        case 3:
            return "Tuner 1";
        case 6:
            return "Tuner 2";
        case 7:
            return "Tuner 3";
        case 10:
            return "Tuner 4";
        case 4:
            return "Playback 1";
        case 8:
            return "Playback 2";
        case 11:
            return "Playback 3";
        case 5:
            return "Audio System";
        case 14:
            return "Free Use";
        case 15:
            return "Broadcast";
        default:
            return "Device " + addr;  // 12, 13 reserved
        }
    }

    // --- Daemon IPC via SocketClient (mirrors NetworkSettings.qml) ---

    // Round-trip scan: request `cec-scan`, receive a JSON array of devices.
    SocketClient {
        id: scanClient
        onResponseReceived: line => {
            var trimmed = line.trim();
            if (trimmed.length === 0 || (trimmed[0] !== "[" && trimmed[0] !== "{")) {
                // error:* reply or empty — daemon reports no usable CEC adapter.
                root.cecAvailable = false;
                root.statusText = "CEC unavailable";
            } else {
                try {
                    var arr = JSON.parse(trimmed);
                    root.devices = Array.isArray(arr) ? arr : [];
                    root.cecAvailable = true;
                    if (root.devices.length > 0) {
                        root.statusText = root.devices.length + " device" + (root.devices.length !== 1 ? "s" : "") + " detected";
                    } else {
                        root.statusText = "No CEC devices found";
                    }
                } catch (e) {
                    console.log("AVControlSettings: failed to parse cec-scan:", e);
                    root.cecAvailable = false;
                    root.statusText = "CEC unavailable";
                }
            }
        }
        onRequestFailed: {
            root.cecAvailable = false;
            root.statusText = "CEC unavailable";
        }
    }

    // Subscribe stream: receive live cec:device:* and cec:power:* events so
    // device rows update without re-polling — the reliability win behind #16.
    SocketClient {
        id: cecEvents
        subscribe: true
        onLineReceived: line => {
            if (line.startsWith("cec:device:")) {
                try {
                    var obj = JSON.parse(line.substring(11));
                    // Merge/update the device into root.devices by logicalAddress.
                    var updated = false;
                    var copy = root.devices.slice();
                    for (var i = 0; i < copy.length; i++) {
                        if (copy[i].logicalAddress === obj.logicalAddress) {
                            copy[i] = obj;
                            updated = true;
                            break;
                        }
                    }
                    if (!updated)
                        copy.push(obj);
                    root.devices = copy;
                    root.cecAvailable = true;
                    if (root.devices.length > 0) {
                        root.statusText = root.devices.length + " device" + (root.devices.length !== 1 ? "s" : "") + " detected";
                    }
                } catch (e) {
                    console.log("AVControlSettings: failed to parse cec:device event:", e);
                }
            } else if (line.startsWith("cec:power:")) {
                try {
                    var pwr = JSON.parse(line.substring(10));
                    // The daemon sends addr as a wire string ("0"); logicalAddress
                    // is numeric, so coerce before matching.
                    var pwrAddr = Number(pwr.addr);
                    var pwrCopy = root.devices.slice();
                    for (var j = 0; j < pwrCopy.length; j++) {
                        if (pwrCopy[j].logicalAddress === pwrAddr) {
                            pwrCopy[j] = Object.assign({}, pwrCopy[j], {
                                powerStatus: pwr.power
                            });
                            break;
                        }
                    }
                    root.devices = pwrCopy;
                } catch (e) {
                    console.log("AVControlSettings: failed to parse cec:power event:", e);
                }
            } else if (line.startsWith("cec:health:")) {
                // Live transmit-health change — same JSON shape as cec-health.
                // Slice by the known prefix length (the value contains colons in
                // a lastError string), mirroring the cec:device: handling above.
                root.applyHealth(line.substring("cec:health:".length));
            }
        }
    }

    // Action clients — each is a one-shot request/response socket.
    SocketClient {
        id: wakeClient
        onResponseReceived: response => {
            root.actionFeedback = response === "ok" ? "Wake command sent" : "Wake command failed";
            feedbackTimer.restart();
            if (response === "ok")
                refreshAfterAction.restart();
        }
        onRequestFailed: {
            root.actionFeedback = "Wake command failed";
            feedbackTimer.restart();
        }
    }

    SocketClient {
        id: sleepClient
        onResponseReceived: response => {
            root.actionFeedback = response === "ok" ? "Sleep command sent" : "Sleep command failed";
            feedbackTimer.restart();
            if (response === "ok")
                refreshAfterAction.restart();
        }
        onRequestFailed: {
            root.actionFeedback = "Sleep command failed";
            feedbackTimer.restart();
        }
    }

    SocketClient {
        id: switchClient
        onResponseReceived: response => {
            root.actionFeedback = response === "ok" ? "Input switch command sent" : "Input switch command failed";
            feedbackTimer.restart();
        }
        onRequestFailed: {
            root.actionFeedback = "Input switch command failed";
            feedbackTimer.restart();
        }
    }

    // Transmit-health poll: request `cec-health`, receive the compact health
    // JSON object (or error:* → "unknown"). Polled on the same cadence as
    // cec-scan so the status line stays fresh; live changes also arrive via the
    // cec:health:* subscribe event below.
    SocketClient {
        id: healthClient
        onResponseReceived: line => root.applyHealth(line)
        onRequestFailed: {
            root.cecTransmitHealth = "unknown";
            root.cecHealthLastError = "";
        }
    }

    // On-demand transmit probe behind the "Test CEC" button. `cec-test` runs a
    // probe and replies the same health JSON — feed it to applyHealth so the
    // status line updates immediately, and surface a one-line result via the
    // shared actionFeedback mechanism.
    SocketClient {
        id: testClient
        onResponseReceived: line => {
            root.applyHealth(line);
            var t = (line || "").trim();
            if (t.length === 0 || t[0] !== "{")
                root.actionFeedback = "CEC test failed";
            else if (root.cecTransmitHealth === "ok")
                root.actionFeedback = "CEC link OK";
            else if (root.cecTransmitHealth === "failing")
                root.actionFeedback = "CEC transmit failing";
            else
                root.actionFeedback = "CEC status unknown";
            feedbackTimer.restart();
        }
        onRequestFailed: {
            root.actionFeedback = "CEC test failed";
            feedbackTimer.restart();
        }
    }

    // --- Timers ---
    Timer {
        id: autoRefresh
        interval: 30000
        running: root.visible && root.cecAvailable
        repeat: true
        onTriggered: {
            scanClient.request("cec-scan");
            healthClient.request("cec-health");
        }
    }

    Timer {
        id: feedbackTimer
        interval: 3000
        onTriggered: {
            root.actionFeedback = "";
        }
    }

    Timer {
        id: refreshAfterAction
        interval: 5000
        onTriggered: {
            scanClient.request("cec-scan");
            healthClient.request("cec-health");
        }
    }

    Component.onCompleted: {
        scanClient.request("cec-scan");
        healthClient.request("cec-health");
        cecEvents.start();
    }

    onVisibleChanged: {
        if (visible) {
            scanClient.request("cec-scan");
            healthClient.request("cec-health");
            cecEvents.start();
        } else {
            cecEvents.stop();
        }
    }

    // Focus the first toggle (always present) so the page is navigable even
    // when CEC is unavailable.
    function focusFirst() {
        focusStartupScope.forceActiveFocus();
    }

    function doWake() {
        wakeClient.request("cec-power-on 0");
    }

    function doSleep() {
        sleepClient.request("cec-power-off 0");
    }

    function doSwitchInput() {
        switchClient.request("cec-active-source");
    }

    // --- UI ---
    // Single content column (child of the base content slot). NOT anchors-filled
    // — SettingsPageBase supplies the page padding + trailing spacer + HintBar.
    ColumnLayout {
        id: avMainCol
        Layout.fillWidth: true
        spacing: Units.spacingLG

        // Header with status
        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Item {
                Layout.fillWidth: true
            }

            // Status indicator
            StatusPill {
                pillState: root.cecAvailable ? "good" : "neutral"
                text: root.statusText
            }

            // Refresh button
            FocusButton {
                id: refreshScope
                visible: root.cecAvailable
                KeyNavigation.right: testScope
                KeyNavigation.down: focusStartupScope
                text: "Refresh"
                onActivated: {
                    root.statusText = "Scanning...";
                    scanClient.request("cec-scan");
                    healthClient.request("cec-health");
                }
            }

            // Test CEC button — on-demand transmit probe (cec-test). Sits beside
            // Refresh; both are only visible/focusable when CEC is available.
            FocusButton {
                id: testScope
                visible: root.cecAvailable
                KeyNavigation.left: refreshScope
                KeyNavigation.down: focusStartupScope
                text: "Test CEC"
                onActivated: {
                    root.actionFeedback = "Testing CEC…";
                    feedbackTimer.stop();
                    testClient.request("cec-test");
                }
            }
        }

        // CEC link status line (#19). Surfaces the transmit-wedge state the
        // device list can't: the adapter opens + receives (so cecAvailable is
        // true and devices may even be listed) while every transmit fails.
        // Hidden when CEC is unavailable — the "HDMI-CEC Not Available" card
        // below owns that state, so the two never show together.
        Text {
            Layout.fillWidth: true
            visible: root.cecAvailable
            wrapMode: Text.WordWrap
            font.pixelSize: Theme.fontBody
            font.bold: root.cecTransmitHealth === "failing"
            text: {
                if (root.cecTransmitHealth === "ok")
                    return "CEC link: OK";
                if (root.cecTransmitHealth === "failing")
                    return "CEC transmit failing — the adapter may be wedged. Re-seat the USB adapter or power-cycle the AVR (pull mains, not standby), then retry.";
                return "CEC link: checking…";
            }
            color: {
                if (root.cecTransmitHealth === "ok")
                    return Theme.online;
                if (root.cecTransmitHealth === "failing")
                    return Theme.warning;
                return Theme.textSecondary;
            }
        }

        // Focus preference toggles — always visible (not gated on cecAvailable).
        // The daemon reads these at CEC startup, at resume, and on an observed
        // device power-on to decide whether to focus the display — and reads the
        // per-device default below to decide which input to focus.
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 16

            SectionHeader {
                text: "Focus Preferences"
            }

            // Focus TV on startup toggle
            PreferenceRow {
                label: "Focus TV on startup"
                description: "Switch the TV/AVR to this input when the shell starts (off keeps your current input on restart)."

                SettingsToggle {
                    id: focusStartupScope
                    // Refresh is hidden when CEC is unavailable — wrap Up to the
                    // last (always-visible) toggle so focus can't vanish.
                    KeyNavigation.up: root.cecAvailable ? refreshScope : autoSwitchScope
                    KeyNavigation.down: focusWakeScope
                    checked: SettingsStore.cecFocusOnStartup
                    onToggled: SettingsStore.setCecFocusOnStartup(!SettingsStore.cecFocusOnStartup)
                }
            }

            // Focus TV on wake toggle
            PreferenceRow {
                label: "Focus TV on wake from sleep"
                description: "Switch to this input when the box wakes from sleep."

                SettingsToggle {
                    id: focusWakeScope
                    KeyNavigation.up: focusStartupScope
                    KeyNavigation.down: autoSwitchScope
                    checked: SettingsStore.cecFocusOnWake
                    onToggled: SettingsStore.setCecFocusOnWake(!SettingsStore.cecFocusOnWake)
                }
            }

            // Auto-switch input on power-on toggle. The daemon's CEC actor folds
            // every bus sweep into a per-address power history and, on a power-on
            // EDGE of the display chain (TV or AVR), applies the default input
            // below (#415). Gated by the same lifecycle master flag as the focus
            // toggles, and skipped when another device demonstrably holds the
            // screen.
            PreferenceRow {
                label: "Auto-switch input on power-on"
                description: "Switch the TV/AVR to this input automatically when a device powers on."

                SettingsToggle {
                    id: autoSwitchScope
                    KeyNavigation.up: focusWakeScope
                    // When CEC is unavailable the action row below is hidden —
                    // wrap Down back to the first toggle instead of self-looping
                    // (which would trap focus).
                    KeyNavigation.down: root.cecAvailable ? wakeScope : focusStartupScope
                    checked: SettingsStore.cecAutoSwitchOnPowerOn
                    onToggled: SettingsStore.setCecAutoSwitchOnPowerOn(!SettingsStore.cecAutoSwitchOnPowerOn)
                }
            }
        }

        // CEC unavailable message — reason-driven (#22). Still gated on the
        // existing `!cecAvailable` state (cec-scan returns error:* in every
        // unavailable case, including the wedged-at-open one), but the title +
        // body now reflect WHY via cecUnavailableReason. adapter_open_failed and
        // adapter_busy are both treated as WARNINGS (ember title + border) because
        // both are actionable with the adapter physically present — one is wedged
        // hardware, the other is a live adapter held by another owner.
        Rectangle {
            id: unavailableCard
            readonly property bool cecWedged: root.cecUnavailableReason === "adapter_open_failed" || root.cecUnavailableReason === "adapter_busy"

            Layout.fillWidth: true
            // Adapt to the wrapped body height so the longer wedged-adapter copy
            // never clips; floor at the original 200 so the short cases are unchanged.
            Layout.preferredHeight: Math.max(200, unavailableCol.implicitHeight + 64)
            radius: Units.radiusLG
            color: Theme.surface
            border.width: 2
            border.color: cecWedged ? Theme.warning : Theme.surfaceBorder
            visible: !root.cecAvailable

            ColumnLayout {
                id: unavailableCol
                anchors.centerIn: parent
                width: parent.width - 64
                spacing: 16

                Text {
                    text: root.cecUnavailableTitle()
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: unavailableCard.cecWedged ? Theme.warning : Theme.textPrimary
                    Layout.fillWidth: true
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                }

                Text {
                    text: root.cecUnavailableBody()
                    font.pixelSize: Theme.fontSmall
                    color: Theme.textSecondary
                    Layout.fillWidth: true
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                }
            }
        }

        // Quick actions — always visible when CEC available
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 16
            visible: root.cecAvailable

            SectionHeader {
                text: "Quick Actions"
            }

            // Feedback text
            Text {
                text: root.actionFeedback
                font.pixelSize: Theme.fontSmall
                color: Theme.online
                visible: root.actionFeedback !== ""
                Layout.alignment: Qt.AlignHCenter
            }

            RowLayout {
                Layout.alignment: Qt.AlignHCenter
                spacing: Units.spacingLG

                // Wake AV
                ActionCard {
                    id: wakeScope
                    // Only claim the root scope's focus when actually visible
                    // (CEC available). Otherwise the root FocusScope holds focus
                    // itself so the read-only state stays dismissable.
                    focus: root.cecAvailable
                    accentColor: Theme.online
                    title: "Wake AV"
                    subtitle: "CEC power on"
                    KeyNavigation.up: autoSwitchScope
                    KeyNavigation.right: sleepScope
                    KeyNavigation.down: deviceRepeater.count > 0 ? deviceRepeater.itemAt(0) : wakeScope
                    onActivated: root.doWake()
                }

                // Sleep AV
                ActionCard {
                    id: sleepScope
                    accentColor: Theme.gold
                    title: "Sleep AV"
                    subtitle: "CEC standby"
                    KeyNavigation.left: wakeScope
                    KeyNavigation.right: switchScope
                    KeyNavigation.down: deviceRepeater.count > 0 ? deviceRepeater.itemAt(0) : sleepScope
                    onActivated: root.doSleep()
                }

                // Switch Input
                ActionCard {
                    id: switchScope
                    accentColor: Theme.ember
                    title: "Switch Input"
                    subtitle: "Set active source"
                    KeyNavigation.left: sleepScope
                    KeyNavigation.down: deviceRepeater.count > 0 ? deviceRepeater.itemAt(0) : switchScope
                    onActivated: root.doSwitchInput()
                }
            }
        }

        // Device list
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 16
            visible: root.cecAvailable

            SectionHeader {
                text: "Detected Devices"
            }

            // No devices placeholder
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 120
                radius: Units.radiusMD
                color: Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder
                visible: root.devices.length === 0

                Text {
                    anchors.centerIn: parent
                    text: "No CEC devices detected. Try refreshing."
                    font.pixelSize: Theme.fontSmall
                    color: Theme.textSecondary
                }
            }

            // Device rows as a Repeater of per-row FocusScopes (not a ListView):
            // each row becomes its own activeFocusItem so SettingsApp's outer
            // contentFlick scrolls the WHOLE page to follow focus down the rows
            // (the Focus Preferences section slides up out of view). A ListView
            // holds focus as one tall item, so moving currentIndex never changed
            // Window.activeFocusItem and the page never scrolled per-row.
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 16
                visible: root.devices.length > 0

                Repeater {
                    id: deviceRepeater
                    model: root.devices

                    delegate: FocusScope {
                        id: deviceRow
                        required property int index
                        required property var modelData
                        Layout.fillWidth: true
                        implicitHeight: 160
                        activeFocusOnTab: true

                        // Measure the longer "Set as default" label so every row's
                        // action button shares one stable width — otherwise the
                        // default row's narrower "Default ✓" button would jut left
                        // out of the right-aligned column.
                        TextMetrics {
                            id: defaultBtnMetrics
                            font.pixelSize: Theme.fontBody
                            text: "Set as default"
                        }

                        // First row goes back up to the Wake-AV action button
                        // (matches the old deviceListView.KeyNavigation.up).
                        // itemAt() can be transiently null during model rebuilds
                        // (CEC rescans reassign root.devices) — acceptable, the
                        // KeyNavigation binding re-evaluates.
                        KeyNavigation.up: index > 0 ? deviceRepeater.itemAt(index - 1) : wakeScope
                        KeyNavigation.down: index < deviceRepeater.count - 1 ? deviceRepeater.itemAt(index + 1) : focusStartupScope

                        // Controller path for "Set as default": Return/Enter on the
                        // focused row toggles that device as the preferred default
                        // input — re-selecting the current default clears it (#16).
                        Keys.onReturnPressed: {
                            var a = modelData.logicalAddress;
                            SettingsStore.setCecDefaultInput(a === SettingsStore.cecDefaultInput ? -1 : a);
                        }
                        Keys.onEnterPressed: {
                            var a = modelData.logicalAddress;
                            SettingsStore.setCecDefaultInput(a === SettingsStore.cecDefaultInput ? -1 : a);
                        }

                        Rectangle {
                            anchors.fill: parent
                            radius: Units.radiusMD
                            color: deviceRow.activeFocus ? Theme.surfaceHover : Theme.surface
                            border.width: 2
                            border.color: Theme.surfaceBorder

                            Behavior on color {
                                ColorAnimation {
                                    duration: 150
                                }
                            }

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 32
                                anchors.rightMargin: 32
                                spacing: 24

                                // Device name + power badge + address detail.
                                ColumnLayout {
                                    spacing: 8
                                    Layout.fillWidth: true

                                    RowLayout {
                                        spacing: 16

                                        Text {
                                            // cec-rs 12.0.1 exposes no OSD name; derive a
                                            // friendly label from the logical address.
                                            text: root.nameForAddress(deviceRow.modelData.logicalAddress)
                                            font.pixelSize: Theme.fontBody
                                            font.bold: true
                                            color: Theme.textPrimary
                                        }

                                        // Power status badge
                                        Rectangle {
                                            Layout.preferredWidth: powerLabel.implicitWidth + 24
                                            Layout.preferredHeight: 40
                                            radius: 20
                                            color: {
                                                if (deviceRow.modelData.powerStatus === "on")
                                                    return Qt.rgba(0.176, 0.541, 0.306, 0.2);
                                                if (deviceRow.modelData.powerStatus === "standby")
                                                    return Qt.rgba(0.843, 0.651, 0.294, 0.2);
                                                return Theme.surfaceHover;
                                            }

                                            Text {
                                                id: powerLabel
                                                anchors.centerIn: parent
                                                text: {
                                                    if (deviceRow.modelData.powerStatus === "on")
                                                        return "On";
                                                    if (deviceRow.modelData.powerStatus === "standby")
                                                        return "Standby";
                                                    if (deviceRow.modelData.powerStatus === "waking")
                                                        return "Waking";
                                                    if (deviceRow.modelData.powerStatus === "sleeping")
                                                        return "Sleeping";
                                                    return "Unknown";
                                                }
                                                font.pixelSize: Theme.fontCaption
                                                color: {
                                                    if (deviceRow.modelData.powerStatus === "on")
                                                        return Theme.online;
                                                    if (deviceRow.modelData.powerStatus === "standby")
                                                        return Theme.gold;
                                                    return Theme.textSecondary;
                                                }
                                            }
                                        }

                                        // Default-input badge — shown when this device is
                                        // the persisted preferred default (#16). Reuses the
                                        // power-badge styling (crimson/sidebarActive tint).
                                        Rectangle {
                                            visible: deviceRow.modelData.logicalAddress === SettingsStore.cecDefaultInput
                                            Layout.preferredWidth: defaultLabel.implicitWidth + 24
                                            Layout.preferredHeight: 40
                                            radius: 20
                                            color: Theme.sidebarActive

                                            Text {
                                                id: defaultLabel
                                                anchors.centerIn: parent
                                                text: "Default"
                                                font.pixelSize: Theme.fontCaption
                                                color: Theme.textOnDark
                                            }
                                        }
                                    }

                                    Text {
                                        text: "Logical address: " + deviceRow.modelData.logicalAddress
                                        font.pixelSize: Theme.fontSmall
                                        color: Theme.textSecondary
                                    }
                                }

                                // Set-as-default action — picks which input the
                                // daemon focuses (#415): unset means "claim the
                                // display for this box", any other device means
                                // "hand the display to that device instead".
                                // z lifts it above the row-selection MouseArea so its own
                                // click handler wins; Return on the focused row is the
                                // controller path.
                                SettingsButton {
                                    z: 1
                                    Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
                                    Layout.preferredWidth: defaultBtnMetrics.width + 80
                                    text: deviceRow.modelData.logicalAddress === SettingsStore.cecDefaultInput ? "Default ✓" : "Set as default"
                                    highlighted: deviceRow.modelData.logicalAddress === SettingsStore.cecDefaultInput
                                    onActivated: SettingsStore.setCecDefaultInput(deviceRow.modelData.logicalAddress === SettingsStore.cecDefaultInput ? -1 : deviceRow.modelData.logicalAddress)
                                }
                            }

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    deviceRow.forceActiveFocus();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

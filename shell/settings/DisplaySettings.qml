import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell.Io
import "../components"
import "../components/lib"

SettingsPageBase {
    id: root
    hintText: "A: Open/apply  |  B: Close dropdown  |  HDR note: applies live via hyprctl"

    property var monitors: []
    property int selectedMonitor: 0

    // --- Daemon read: hypr-monitors (replaces the hyprctl monitors -j Process) ---

    SocketClient {
        id: getMonitors
        onResponseReceived: line => {
            try {
                let data = JSON.parse(line);
                let mons = [];
                for (let i = 0; i < data.length; i++) {
                    let m = data[i];
                    mons.push({
                        name: m.name || "Unknown",
                        description: m.description || "",
                        width: m.width || 0,
                        height: m.height || 0,
                        refreshRate: m.refreshRate || 0,
                        scale: m.scale || 1.0,
                        x: m.x || 0,
                        y: m.y || 0,
                        activeWorkspace: m.activeWorkspace || "",
                        dpmsStatus: m.dpmsStatus !== false,
                        vrr: m.vrr || false,
                        availableModes: m.availableModes || [],
                        currentFormat: m.currentFormat || "",
                        hdr: m.hdr || false
                    });
                }
                root.monitors = mons;
            } catch (e) {
                console.log("Failed to parse monitor data:", e);
            }
        }
    }

    // --- One-shot APPLY paths (hyprctl keyword monitor) ---

    Process {
        id: setScale
        property string monName: ""
        property real scaleVal: 1.0
        command: ["hyprctl", "keyword", "monitor", monName + ",preferred,auto," + scaleVal]
        onExited: {
            getMonitors.request("hypr-monitors");
        }
    }

    // Apply a full monitor mode line (resolution + refresh). #233: always built
    // from an EXACT entry in availableModes + the monitor's current scale/position,
    // so resolution and refresh changes never lose scale and never form an invalid
    // resolution@refresh pair.
    Process {
        id: setMode
        property string monKeyword: ""
        command: ["hyprctl", "keyword", "monitor", monKeyword]
        onExited: {
            getMonitors.request("hypr-monitors");
        }
    }

    // Apply HDR on: include bitdepth,10,cm,hdr,sdrbrightness,sdrsaturation
    // Apply HDR off: standard mode line only
    Process {
        id: setHdr
        property string monKeyword: ""
        command: ["hyprctl", "keyword", "monitor", monKeyword]
        onExited: {
            getMonitors.request("hypr-monitors");
        }
    }

    // Night-light: enable via hyprsunset -t <temp>, disable via pkill hyprsunset
    Process {
        id: applyNightLight
        property var pendingCmd: []
        command: pendingCmd
    }

    function applyHdr(enabled) {
        if (root.monitors.length <= root.selectedMonitor)
            return;
        let mon = root.monitors[root.selectedMonitor];
        let name = mon.name;
        let w = mon.width;
        let h = mon.height;
        let rr = mon.refreshRate.toFixed(6);
        let scale = mon.scale.toFixed(1);
        let x = mon.x;
        let y = mon.y;
        let keyword;
        if (enabled) {
            keyword = name + "," + w + "x" + h + "@" + rr + "," + x + "x" + y + "," + scale + ",vrr,1,bitdepth,10,cm,hdr,sdrbrightness,1.2,sdrsaturation,1.05";
        } else {
            keyword = name + "," + w + "x" + h + "@" + rr + "," + x + "x" + y + "," + scale;
        }
        setHdr.monKeyword = keyword;
        setHdr.running = true;
    }

    // #233: apply an EXACT availableModes entry ("WxH@refresh") while preserving
    // the monitor's current scale and position. Single funnel for both the
    // resolution and refresh-rate dropdowns so combos are always valid.
    function applyMode(modeStr) {
        if (root.monitors.length <= root.selectedMonitor)
            return;
        let mon = root.monitors[root.selectedMonitor];
        let scale = mon.scale.toFixed(1);
        let keyword = mon.name + "," + modeStr + "," + mon.x + "x" + mon.y + "," + scale;
        setMode.monKeyword = keyword;
        setMode.running = true;
    }

    // #233: switch resolution. Picks a valid refresh for the target resolution —
    // keeps the current rate if that resolution supports it, otherwise the
    // highest available — and applies the exact mode string for that pair.
    function applyResolution(resStr) {
        if (root.monitors.length <= root.selectedMonitor)
            return;
        let mon = root.monitors[root.selectedMonitor];
        let best = null;
        let bestHz = -1;
        let exactCurrent = null;
        for (let i = 0; i < mon.availableModes.length; i++) {
            let m = mon.availableModes[i];
            let p = m.split("@");
            if (p.length < 2 || p[0] !== resStr)
                continue;
            let hz = parseFloat(p[1]);
            if (hz > bestHz) {
                bestHz = hz;
                best = m;
            }
            if (Math.abs(hz - mon.refreshRate) < 0.5)
                exactCurrent = m;
        }
        // Guard: reject a resolution with no known mode (invalid pair).
        if (!best)
            return;
        root.applyMode(exactCurrent || best);
    }

    function applyNightLightSetting(enabled, temp) {
        if (enabled) {
            applyNightLight.pendingCmd = ["hyprsunset", "-t", temp.toString()];
        } else {
            applyNightLight.pendingCmd = ["pkill", "hyprsunset"];
        }
        applyNightLight.running = true;
    }

    Component.onCompleted: {
        getMonitors.request("hypr-monitors");
    }

    onVisibleChanged: {
        if (visible) {
            getMonitors.request("hypr-monitors");
        }
    }

    function focusFirst() {
        monitorList.forceActiveFocus();
    }

    // #231: hours 0..23 for the auto-theme schedule dropdowns, plus a 12-hour
    // "7:00 AM" / "8:00 PM" formatter for couch readability.
    property var hoursOfDay: {
        let a = [];
        for (let i = 0; i < 24; i++)
            a.push(i);
        return a;
    }
    function formatHour(h) {
        let ampm = h < 12 ? "AM" : "PM";
        let hr = h % 12;
        if (hr === 0)
            hr = 12;
        return hr + ":00 " + ampm;
    }

    // Single content column (child of the base content slot). NOT anchors-filled
    // — SettingsPageBase supplies the page padding + trailing spacer + HintBar.
    ColumnLayout {
        id: contentColumn
        Layout.fillWidth: true
        spacing: Units.spacingLG

        SectionHeader {
            text: "Displays"
        }

        // HDR live-state status line (read-only, driven by daemon-read hdr field)
        Text {
            visible: root.monitors.length > root.selectedMonitor
            text: {
                if (root.monitors.length <= root.selectedMonitor)
                    return "";
                let mon = root.monitors[root.selectedMonitor];
                return "HDR: " + (mon.hdr ? "Active (" + mon.currentFormat + ")" : "Inactive");
            }
            font.pixelSize: Theme.fontSmall
            color: Theme.textSecondary
        }

        // Monitor list
        SettingsList {
            id: monitorList
            // rowStride:200 preserves the original Math.min(count*200,400) formula
            // exactly; true geometric stride is delegate 180 + spacing 16 = 196.
            rowStride: 200
            maxHeight: 400
            spacing: 16
            model: root.monitors
            focus: true

            KeyNavigation.down: hdrToggleScope

            delegate: SettingsListRow {
                required property int index
                required property var modelData
                width: monitorList.width
                itemHeight: 180
                selected: monitorList.currentIndex === index && monitorList.activeFocus

                property bool isSel: selected

                ColumnLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 20
                    anchors.bottomMargin: 20
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 16

                        Text {
                            text: modelData.name
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                        }

                        Text {
                            text: modelData.description
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }

                    RowLayout {
                        spacing: Units.spacingLG

                        Text {
                            text: "Resolution: " + modelData.width + "x" + modelData.height
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "Refresh: " + modelData.refreshRate.toFixed(1) + " Hz"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "Scale: " + modelData.scale.toFixed(1) + "x"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }

                    RowLayout {
                        spacing: Units.spacingLG

                        Text {
                            text: "Position: " + modelData.x + "," + modelData.y
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "DPMS: " + (modelData.dpmsStatus ? "On" : "Off")
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "VRR: " + (modelData.vrr ? "On" : "Off")
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        monitorList.currentIndex = index;
                        monitorList.forceActiveFocus();
                        root.selectedMonitor = index;
                    }
                }
            }

            Keys.onReturnPressed: {
                root.selectedMonitor = currentIndex;
                hdrToggleScope.forceActiveFocus();
            }
        }

        // HDR toggle
        SectionHeader {
            text: "HDR"
            visible: root.monitors.length > 0
        }

        SettingsToggle {
            id: hdrToggleScope
            visible: root.monitors.length > 0
            KeyNavigation.up: monitorList
            KeyNavigation.down: scaleRow
            checked: SettingsStore.hdrEnabled
            onToggled: {
                SettingsStore.setHdrEnabled(!SettingsStore.hdrEnabled);
                root.applyHdr(SettingsStore.hdrEnabled);
            }
        }

        // Scale controls
        SectionHeader {
            text: "Scale"
            visible: root.monitors.length > 0
        }

        SettingsButtonGroup {
            id: scaleRow
            visible: root.monitors.length > 0
            options: [
                {
                    label: "0.5x",
                    value: 0.5
                },
                {
                    label: "1x",
                    value: 1.0
                },
                {
                    label: "1.25x",
                    value: 1.25
                },
                {
                    label: "1.5x",
                    value: 1.5
                },
                {
                    label: "1.75x",
                    value: 1.75
                },
                {
                    label: "2x",
                    value: 2.0
                }
            ]
            isCurrentOption: function (opt) {
                return root.monitors.length > root.selectedMonitor && Math.abs(root.monitors[root.selectedMonitor].scale - opt.value) < 0.05;
            }
            onValueSelected: function (opt) {
                if (root.monitors.length > root.selectedMonitor) {
                    setScale.monName = root.monitors[root.selectedMonitor].name;
                    setScale.scaleVal = opt.value;
                    setScale.running = true;
                }
            }

            KeyNavigation.up: hdrToggleScope
            KeyNavigation.down: modeDropdownScope
        }

        // Resolution / Mode dropdown
        SectionHeader {
            text: "Resolution"
            visible: root.monitors.length > 0
        }

        SettingsDropdown {
            id: modeDropdownScope
            visible: root.monitors.length > 0
            maxHeight: 600

            // #233: resolutions only (deduped, "WxH") — no refresh rate here. The
            // separate Refresh Rate dropdown below owns the rate for the current
            // resolution.
            property var resolutions: {
                if (root.monitors.length <= root.selectedMonitor)
                    return [];
                let allModes = root.monitors[root.selectedMonitor].availableModes;
                let seen = {};
                let result = [];
                for (let i = 0; i < allModes.length; i++) {
                    let res = allModes[i].split("@")[0];
                    if (res && !seen[res]) {
                        seen[res] = true;
                        result.push(res);
                    }
                }
                return result;
            }
            property string currentRes: {
                if (root.monitors.length <= root.selectedMonitor)
                    return "";
                let mon = root.monitors[root.selectedMonitor];
                return mon.width + "x" + mon.height;
            }

            // Present "3840x2160" as "3840 × 2160" for couch readability.
            function formatRes(res) {
                return res.replace("x", " × ");
            }

            model: resolutions
            displayText: formatRes(currentRes)
            isCurrentItem: function (item) {
                return item === currentRes;
            }
            itemLabel: function (item) {
                return formatRes(item);
            }
            onItemSelected: function (item) {
                root.applyResolution(item);
            }

            KeyNavigation.up: scaleRow
            KeyNavigation.down: refreshDropdownScope
        }

        // Refresh Rate dropdown (separate from resolution)
        SectionHeader {
            text: "Refresh Rate"
            visible: root.monitors.length > 0
        }

        SettingsDropdown {
            id: refreshDropdownScope
            visible: root.monitors.length > 0

            property var refreshRates: {
                if (root.monitors.length <= root.selectedMonitor)
                    return [];
                let mon = root.monitors[root.selectedMonitor];
                let currentRes = mon.width + "x" + mon.height;
                let seen = {};
                let result = [];
                for (let i = 0; i < mon.availableModes.length; i++) {
                    let m = mon.availableModes[i];
                    let parts = m.split("@");
                    if (parts.length < 2)
                        continue;
                    let res = parts[0];
                    if (res !== currentRes)
                        continue;
                    let hz = parseFloat(parts[1]).toFixed(2);
                    if (!seen[hz]) {
                        seen[hz] = true;
                        result.push({
                            hz: hz,
                            mode: m
                        });
                    }
                }
                return result;
            }
            property real currentHz: root.monitors.length > root.selectedMonitor ? root.monitors[root.selectedMonitor].refreshRate : 0

            model: refreshRates
            displayText: currentHz.toFixed(2) + " Hz"
            isCurrentItem: function (item) {
                return Math.abs(parseFloat(item.hz) - currentHz) < 0.5;
            }
            itemLabel: function (item) {
                return item.hz + " Hz";
            }
            onItemSelected: function (item) {
                // #233: apply the exact availableModes string for this rate at the
                // current resolution — guaranteed-valid pair.
                root.applyMode(item.mode);
            }

            KeyNavigation.up: modeDropdownScope
            KeyNavigation.down: nightLightToggleScope
        }

        // Night-light / Color temperature
        SectionHeader {
            text: "Night Light"
        }

        SettingsToggle {
            id: nightLightToggleScope
            KeyNavigation.up: refreshDropdownScope
            KeyNavigation.down: nightLightTempScope
            checked: SettingsStore.nightLightEnabled
            onToggled: {
                SettingsStore.setNightLightEnabled(!SettingsStore.nightLightEnabled);
                root.applyNightLightSetting(SettingsStore.nightLightEnabled, SettingsStore.nightLightTemp);
            }
        }

        // Night-light color temperature dropdown
        SettingsDropdown {
            id: nightLightTempScope

            property var tempPresets: [
                {
                    label: "Off (disable)",
                    value: 0
                },
                {
                    label: "Very Warm (3500K)",
                    value: 3500
                },
                {
                    label: "Warm (4000K)",
                    value: 4000
                },
                {
                    label: "Neutral (4500K)",
                    value: 4500
                },
                {
                    label: "Cool (5500K)",
                    value: 5500
                },
                {
                    label: "Daylight (6500K)",
                    value: 6500
                }
            ]

            model: tempPresets
            displayText: {
                let t = SettingsStore.nightLightTemp;
                for (let i = 0; i < tempPresets.length; i++) {
                    if (tempPresets[i].value === t)
                        return tempPresets[i].label;
                }
                return t + "K";
            }
            isCurrentItem: function (item) {
                return item.value === SettingsStore.nightLightTemp;
            }
            itemLabel: function (item) {
                return item.label;
            }
            onItemSelected: function (item) {
                if (item.value === 0) {
                    SettingsStore.setNightLightEnabled(false);
                    root.applyNightLightSetting(false, SettingsStore.nightLightTemp);
                } else {
                    SettingsStore.setNightLightTemp(item.value);
                    if (SettingsStore.nightLightEnabled)
                        root.applyNightLightSetting(true, item.value);
                }
            }

            KeyNavigation.up: nightLightToggleScope
            KeyNavigation.down: overscanScope
        }

        Text {
            text: "Night light requires hyprsunset to be installed."
            font.pixelSize: Theme.fontHint
            color: Theme.textMuted
        }

        // Overscan / safe-area
        SectionHeader {
            text: "Overscan"
        }

        SettingsButtonGroup {
            id: overscanScope
            options: [
                {
                    label: "0%",
                    value: 0
                },
                {
                    label: "2%",
                    value: 2
                },
                {
                    label: "4%",
                    value: 4
                },
                {
                    label: "6%",
                    value: 6
                },
                {
                    label: "8%",
                    value: 8
                },
                {
                    label: "10%",
                    value: 10
                }
            ]
            isCurrentOption: function (opt) {
                return SettingsStore.overscan === opt.value;
            }
            onValueSelected: function (opt) {
                SettingsStore.setOverscan(opt.value);
            }

            KeyNavigation.up: nightLightTempScope
            KeyNavigation.down: autoDimScope
        }

        Text {
            text: "Insets the shell by this percent so nothing lands in the area a TV crops. Applies immediately."
            font.pixelSize: Theme.fontHint
            color: Theme.textMuted
        }

        // Auto-Dim (OLED burn-in protection, #143). #232: one combined row —
        // `Off` disables auto-dim; any time enables it at that delay.
        SectionHeader {
            text: "Auto-Dim"
        }

        SettingsButtonGroup {
            id: autoDimScope
            options: [
                {
                    label: "Off",
                    value: 0
                },
                {
                    label: "1 min",
                    value: 1
                },
                {
                    label: "2 min",
                    value: 2
                },
                {
                    label: "5 min",
                    value: 5
                },
                {
                    label: "10 min",
                    value: 10
                }
            ]
            isCurrentOption: function (opt) {
                // `Off` is current when disabled; otherwise the matching delay.
                if (opt.value === 0)
                    return !SettingsStore.autoDimEnabled;
                return SettingsStore.autoDimEnabled && SettingsStore.autoDimDelayMinutes === opt.value;
            }
            onValueSelected: function (opt) {
                if (opt.value === 0) {
                    SettingsStore.setAutoDimEnabled(false);
                } else {
                    SettingsStore.setAutoDimDelayMinutes(opt.value);
                    SettingsStore.setAutoDimEnabled(true);
                }
            }

            KeyNavigation.up: overscanScope
            KeyNavigation.down: modeList
        }

        Text {
            text: "Dims the display after inactivity. Any input restores full brightness immediately."
            font.pixelSize: Theme.fontHint
            color: Theme.textMuted
        }

        // Appearance — Theme Mode
        SectionHeader {
            text: "Appearance"
        }

        RowLayout {
            id: modeList
            Layout.alignment: Qt.AlignHCenter
            spacing: 40
            focus: false

            property var modes: [
                {
                    id: "auto",
                    icon: "◐",
                    label: "Auto",
                    desc: "Follows time of day"
                },
                {
                    id: "light",
                    icon: "☀",
                    label: "Light",
                    desc: "Light background"
                },
                {
                    id: "dark",
                    icon: "☽",
                    label: "Dark",
                    desc: "OLED optimized"
                }
            ]

            property int currentIndex: {
                for (var i = 0; i < modeList.modes.length; i++) {
                    if (modeList.modes[i].id === Theme.themeMode)
                        return i;
                }
                return 0;
            }

            property int focusIndex: 0

            Keys.onLeftPressed: event => {
                if (focusIndex > 0)
                    focusIndex--;
                else
                    event.accepted = false;
            }
            Keys.onRightPressed: {
                if (focusIndex < modeList.modes.length - 1)
                    focusIndex++;
            }
            Keys.onReturnPressed: {
                SettingsStore.setThemeMode(modeList.modes[focusIndex].id);
            }
            Keys.onUpPressed: {
                autoDimScope.forceActiveFocus();
            }
            // #231: drop into the auto-theme schedule when in Auto mode.
            Keys.onDownPressed: event => {
                if (Theme.themeMode === "auto")
                    lightStartScope.forceActiveFocus();
                else
                    event.accepted = false;
            }

            Repeater {
                model: modeList.modes

                Rectangle {
                    required property var modelData
                    required property int index
                    readonly property bool isSelected: Theme.themeMode === modelData.id
                    width: 400
                    height: 280
                    radius: Theme.cardRadius
                    color: isSelected ? Theme.sidebarActive : Theme.surface
                    clip: true
                    border.width: (modeList.focusIndex === index && modeList.activeFocus) || isSelected ? 3 : 2
                    border.color: isSelected ? Theme.focusBorder : (modeList.focusIndex === index && modeList.activeFocus ? Theme.focusBorder : Theme.surfaceBorder)

                    Behavior on border.color {
                        ColorAnimation {
                            duration: 150
                        }
                    }
                    Behavior on color {
                        ColorAnimation {
                            duration: 150
                        }
                    }

                    Rectangle {
                        anchors.fill: parent
                        radius: parent.radius
                        color: Theme.surfaceHover
                        visible: modeList.focusIndex === index && modeList.activeFocus && !parent.isSelected
                    }

                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: 32
                        spacing: 12

                        Item {
                            Layout.fillHeight: true
                        }

                        Text {
                            text: modelData.icon
                            font.pixelSize: Theme.fontTitle
                            color: Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: modelData.label
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: modelData.desc
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                            Layout.alignment: Qt.AlignHCenter
                            Layout.maximumWidth: parent.width
                            horizontalAlignment: Text.AlignHCenter
                            wrapMode: Text.WordWrap
                        }

                        Item {
                            Layout.fillHeight: true
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            modeList.focusIndex = index;
                            modeList.forceActiveFocus();
                            SettingsStore.setThemeMode(modelData.id);
                        }
                    }
                }
            }
        }

        // Auto-theme schedule (#231) — only relevant in Auto mode.
        SectionHeader {
            text: "Auto Schedule"
            visible: Theme.themeMode === "auto"
        }

        Text {
            visible: Theme.themeMode === "auto"
            text: "When Auto mode switches to light and dark each day."
            font.pixelSize: Theme.fontHint
            color: Theme.textMuted
        }

        SettingsDropdown {
            id: lightStartScope
            visible: Theme.themeMode === "auto"
            maxHeight: 600
            model: root.hoursOfDay
            displayText: "Light starts:  " + root.formatHour(SettingsStore.autoThemeLightStart)
            isCurrentItem: function (item) {
                return item === SettingsStore.autoThemeLightStart;
            }
            itemLabel: function (item) {
                return root.formatHour(item);
            }
            onItemSelected: function (item) {
                SettingsStore.setAutoThemeLightStart(item);
            }

            KeyNavigation.up: modeList
            KeyNavigation.down: darkStartScope
        }

        SettingsDropdown {
            id: darkStartScope
            visible: Theme.themeMode === "auto"
            maxHeight: 600
            model: root.hoursOfDay
            displayText: "Dark starts:  " + root.formatHour(SettingsStore.autoThemeDarkStart)
            isCurrentItem: function (item) {
                return item === SettingsStore.autoThemeDarkStart;
            }
            itemLabel: function (item) {
                return root.formatHour(item);
            }
            onItemSelected: function (item) {
                SettingsStore.setAutoThemeDarkStart(item);
            }

            KeyNavigation.up: lightStartScope
        }
    }
}

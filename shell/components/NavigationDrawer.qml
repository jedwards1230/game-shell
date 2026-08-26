import QtQuick
import QtQuick.Layouts
import "lib"
import "resumeModel.js" as ResumeModel
import "appQuirks.js" as AppQuirks

// Left navigation drawer (#216, epic #133 Phase 1). Top→bottom:
//   1. Clock/date header
//   2. Nav list — the Home row, followed by one row per RUNNING app to resume.
//      Resume rows share the Home row's styling (icon + label + accent bar); the
//      list simply grows/shrinks with the running-window set.
//   3. Quick Actions row
//   4. Now-Playing mini-strip — a MediaWidget, hidden when idle (no MPRIS player)
//   5. Status glyph line — a non-focusable readout pinned to the bottom
//      (online · volume · controller battery · notifications)
//
// Focus chain (down-flow): navList (Home + resume rows) → drawerActions →
// MediaWidget. Up reverses. The status line is NOT in the focus chain.
Drawer {
    id: root
    edge: "left"
    drawerWidth: Units.gridUnit * 18

    property bool overlayMode: false

    // Running windows (Hyprland clients) — wired from ShellLayout
    // (AppLifecycleManager.runningWindows). Feeds the resume list.
    property var runningWindows: []
    // Gamepad fleet model (InputManager.pads) — wired from ShellLayout. Drives the
    // controller-battery status glyph; empty ⇒ that glyph is hidden.
    property var pads: []

    signal settingsRequested
    signal widgetsSelected
    signal notificationCenterRequested
    signal powerRequested
    signal networkRequested(var anchorRect)
    signal volumeRequested(var anchorRect)
    signal homeSelected

    // Resume-row activation contract (mirrors HomeScreen; ShellLayout forwards
    // each to shell.qml → AppLifecycle).
    signal appLaunchRequested(var app)
    signal appResumeRequested(var app, string address)
    // `windowClass` lets AppLifecycleManager fall back to a class-targeted
    // focus when our window snapshot has gone stale (#347).
    signal appFocusRequested(string address, string windowClass)
    signal appCloseRequested(string address)

    // === Resume list — RUNNING windows only ===
    // resumeModel.build merges running windows + recents and resolves each entry's
    // name/icon (via the WindowMatcher singleton + AppDiscoveryManager). We keep
    // only the running entries here: the drawer's resume rows are for jumping back
    // into a live app, one row each. (Recents live on the Home screen.)
    // Companion windows are re-iconed BEFORE the merge so the icon the merge
    // resolves against is the corrected one. This changes appearance only —
    // every running window still gets its own row (a Remote Play
    // `streaming_client` and the Steam Big Picture window behind it are two
    // different destinations, and collapsing them removed the only route to the
    // live stream). See appQuirks.identifyCompanionWindows.
    readonly property var resumeModel: ResumeModel.build(AppQuirks.identifyCompanionWindows(root.runningWindows), RecentsTracker.recentApps, AppDiscoveryManager.applications, WindowMatcher, {
        // The user's OWN mutes only. The workspace policy mutes nearly
        // everything at any moment, so showing that would light up every row and
        // say nothing — which is also why there is no "producing audio"
        // indicator: the policy already guarantees the app on screen is the only
        // one you can hear, so the question has been made unaskable.
        mutedClasses: SettingsStore.mutedApps
    })
    readonly property var resumeApps: root.resumeModel.filter(function (e) {
        return e.running === true;
    })

    // Nav list model: the Home row, then one "resume" row per running app.
    readonly property var navModel: {
        var rows = [
            {
                label: "Home",
                icon: "\u{1F3E0}",
                kind: "home"
            }
        ];
        for (var i = 0; i < root.resumeApps.length; i++) {
            var app = root.resumeApps[i];
            rows.push({
                label: app.name || "App",
                kind: "resume",
                entry: app
            });
        }
        return rows;
    }

    // Best wireless pad reporting a battery level (mirrors HomeScreen._batteryPad).
    // null when only wired pads / none are connected — the glyph then hides.
    readonly property var _batteryPad: {
        var best = null;
        for (var i = 0; i < root.pads.length; i++) {
            var p = root.pads[i];
            if (p.batteryLevel < 0)
                continue;
            if (best === null || p.batteryLevel < best.batteryLevel)
                best = p;
        }
        return best;
    }

    // The row the user is on, held as the app's window ADDRESS — an identity, not
    // a position. "" means Home (or nothing known).
    //
    // `navModel` is a binding that now depends on the audio flags, so muting an
    // app (or an app starting to play) REBUILDS the model, and a JS-array model
    // reset drops currentIndex back to 0. Without restoring, muting an app from
    // its own row would bounce drawer focus to Home.
    //
    // Restoring by INDEX would be worse than the bounce it fixes. The resume rows
    // are sorted by focusHistoryId and their membership changes while the drawer
    // is open, so an app exiting — or any new window mapping, which lands at
    // focusHistoryId 0 and shifts everything down — moves a given index onto a
    // DIFFERENT app. The cursor would sit on a neighbour with nothing on screen
    // saying so, and the next A press would resume the wrong app. Bouncing to
    // Home is at least visible; silently retargeting is not.
    //
    // Recorded at the points the user actually moves, never from
    // currentIndexChanged — that fires during the reset itself and would record
    // the very 0 this exists to undo.
    property string focusedAddress: ""

    // Both lookups are pure and headlessly tested in resumeModel.js — this is
    // the logic an index-keyed restore got wrong, so it does not live in a QML
    // file where nothing can assert on it.
    function _addressAt(index) {
        return ResumeModel.addressAt(root.navModel, index);
    }

    function _indexForAddress(address) {
        return ResumeModel.indexForAddress(root.navModel, address);
    }

    onOpenedChanged: {
        if (opened) {
            root.focusedAddress = "";
            navList.currentIndex = 0;
            navFocusTimer.restart();
        }
    }

    Timer {
        id: navFocusTimer
        interval: 50
        // Land on the nav list (Home row) whenever the drawer opens.
        onTriggered: navList.forceActiveFocus()
    }

    // Return controller focus to the QuickActions row — called when an anchored
    // popover (Volume/Network) closes while the drawer is still open, so focus
    // lands back on the glyph the user activated rather than jumping to home.
    function focusQuickActions() {
        drawerActions.forceActiveFocus();
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // === Clock + Date Header ===
        Item {
            Layout.fillWidth: true
            Layout.preferredHeight: Units.gridUnit * 5

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 12

                ClockText {
                    kind: "time"
                    running: root.opened
                    font.pixelSize: Theme.fontHero * 0.7
                    font.bold: true
                    color: Theme.textPrimary
                    Layout.alignment: Qt.AlignHCenter
                }

                ClockText {
                    kind: "date"
                    running: root.opened
                    font.pixelSize: Theme.fontBody
                    color: Theme.textSecondary
                    Layout.alignment: Qt.AlignHCenter
                }
            }
        }

        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 2
            color: Theme.surfaceBorder
        }

        // === Nav list: Home row + one resume row per running app ===
        ListView {
            id: navList
            Layout.fillWidth: true
            Layout.topMargin: Units.spacingSM
            Layout.preferredHeight: contentHeight
            model: root.navModel
            // Put the user back on the SAME APP they were on. The model rebuilds
            // whenever the audio flags change — a mute toggle, or an app
            // starting to play — and a reset would otherwise drop them at the
            // top mid-interaction. Resolving the address against the fresh model
            // means a row that moved is followed and a row that vanished lands
            // on Home, rather than the index landing on whoever took its place.
            onModelChanged: {
                let idx = root._indexForAddress(root.focusedAddress);
                currentIndex = (idx > 0 && idx < count) ? idx : 0;
                // The app is gone — forget it, so a later rebuild cannot revive
                // a stale target and jump the cursor with no user input.
                if (idx === 0)
                    root.focusedAddress = "";
            }
            // Hold default focus the instant the drawer opens so there is never a
            // handler-less window before navFocusTimer runs.
            focus: true
            interactive: false
            currentIndex: 0

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: navList.width
                height: Math.round(Units.gridUnit * 2.2)
                // The row stays lit while its context popover is open — the
                // popover takes activeFocus off navList, and gating on
                // activeFocus alone would flatten the row mid-interaction.
                color: navList.currentIndex === index && (navList.activeFocus || rowContextMenu.opened) ? Theme.surfaceHover : "transparent"
                Accessible.role: Accessible.Button
                Accessible.name: modelData.label
                Accessible.focusable: true
                Accessible.onPressAction: root._activateNav(index)
                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                FocusAccentBar {
                    active: navList.currentIndex === index && (navList.activeFocus || rowContextMenu.opened)
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: Units.gridUnit
                    anchors.rightMargin: Units.gridUnit
                    spacing: Units.spacingLG

                    // Icon column: the 🏠 glyph for Home, the app icon (with
                    // letter-initial fallback) for a resume row.
                    Item {
                        Layout.preferredWidth: Units.gridUnit
                        Layout.preferredHeight: Units.gridUnit
                        Layout.alignment: Qt.AlignVCenter

                        Text {
                            anchors.centerIn: parent
                            visible: modelData.kind === "home"
                            text: modelData.icon || ""
                            font.pixelSize: Theme.fontTitle
                            horizontalAlignment: Text.AlignHCenter
                        }

                        AppIcon {
                            anchors.centerIn: parent
                            visible: modelData.kind === "resume"
                            iconSize: Math.round(Units.gridUnit * 1.2)
                            iconSource: modelData.kind === "resume" && modelData.entry ? (modelData.entry.icon || "") : ""
                            fallbackText: modelData.kind === "resume" && modelData.entry ? (modelData.entry.name || modelData.label) : ""
                        }
                    }

                    Text {
                        text: modelData.label
                        font.pixelSize: Theme.fontTitle
                        color: Theme.textPrimary
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    // "You muted this app by hand." NEVER the policy mute, which
                    // is true of nearly every app at any moment and would light
                    // up every row while telling the user nothing.
                    //
                    // Conditionally rendered, so a row you have not muted looks
                    // exactly as it did before this existed. Glyph-only on
                    // purpose: the box has no system icon theme (see AppIcon's
                    // letter-initial fallback), so an `image://icon/` name would
                    // render nothing.
                    Text {
                        visible: modelData.kind === "resume" && modelData.entry && modelData.entry.userMuted === true
                        Layout.alignment: Qt.AlignVCenter
                        text: "\u{1F507}"
                        font.pixelSize: Theme.fontBody
                        color: Theme.crimson
                        Accessible.role: Accessible.Indicator
                        Accessible.name: "Muted by you"
                    }

                    // Running-app cue: a small ember dot on resume rows (every row
                    // below Home is a live app you can jump back into).
                    Rectangle {
                        visible: modelData.kind === "resume"
                        Layout.alignment: Qt.AlignVCenter
                        implicitWidth: 8
                        implicitHeight: 8
                        radius: 4
                        color: Theme.ember
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        root.focusedAddress = root._addressAt(index);
                        root._activateNav(index);
                    }
                }
            }

            Keys.onDownPressed: {
                if (currentIndex < count - 1)
                    currentIndex++;
                else
                    drawerActions.forceActiveFocus();
                root.focusedAddress = root._addressAt(currentIndex);
            }
            Keys.onUpPressed: {
                if (currentIndex > 0)
                    currentIndex--;
                root.focusedAddress = root._addressAt(currentIndex);
            }
            Keys.onReturnPressed: root._activateNav(currentIndex)

            // Context popover trigger = the X face (daemon altAction → KEY_X),
            // NOT the Y face (altSelect → KEY_TAB). Standardized across every
            // surface — mirrors NavigableRow's handler. The event is accepted
            // only when a menu actually opened, so X on the Home row stays inert
            // rather than silently swallowing the key.
            Keys.onPressed: event => {
                if (event.key === Qt.Key_X) {
                    InputMode.exitMouseMode();
                    if (root._openRowContext(navList.currentIndex))
                        event.accepted = true;
                }
            }
        }

        // X-face affordance for the resume rows — shown only when there is at
        // least one, since X on the Home row is a no-op.
        HintBar {
            visible: root.resumeApps.length > 0
            muted: true
            Layout.topMargin: Units.spacingXS
            text: "A: Resume   X: Actions (quit, mute)"
        }

        // Absorbs the slack so the QuickActions row (and the Now-Playing strip +
        // status line below it) stay pinned to the BOTTOM of the drawer. The nav
        // list sizes to its content (Layout.preferredHeight: contentHeight), so
        // without this every child stacks at the top and QuickActions rides up
        // under the last resume row — the taller the list, the lower it floats.
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
        }

        // === Quick Actions row ===
        Item {
            Layout.fillWidth: true
            Layout.topMargin: Units.spacingSM
            // Reserve full label height plus a safe-margin so the floating
            // focus label never bleeds into the row below (#142) — now the
            // Now-Playing strip rather than the viewport bottom.
            Layout.preferredHeight: drawerActions.implicitHeight + Units.spacingLG
            Layout.bottomMargin: Units.spacingLG

            QuickActions {
                id: drawerActions
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: Units.gridUnit
                anchors.rightMargin: Units.gridUnit
                anchors.top: parent.top
                // Bubble Escape/B up to the Drawer so it closes instead of
                // opening Settings.
                escapeRequestsSettings: false
                // Cap width to the drawer so overflowing actions scroll.
                maxContentWidth: width

                onFocusUpRequested: navList.forceActiveFocus()
                // Down → the Now-Playing strip when a player is present; a no-op
                // (focus stays) when idle, since focusFirstChild returns false.
                onFocusDownRequested: mediaWidget.focusFirstChild()
                onSettingsRequested: {
                    root.closed();
                    root.settingsRequested();
                }
                onWidgetsRequested: {
                    root.closed();
                    root.widgetsSelected();
                }
                onNotificationCenterRequested: {
                    root.closed();
                    root.notificationCenterRequested();
                }
                onPowerRequested: {
                    root.closed();
                    root.powerRequested();
                }
                onNetworkRequested: anchorRect => {
                    // Keep the drawer open underneath; the popover paints on
                    // top (higher z) anchored to this glyph (#118).
                    root.networkRequested(anchorRect);
                }
                onVolumeRequested: anchorRect => {
                    root.volumeRequested(anchorRect);
                }
            }
        }

        // === Now-Playing mini-strip (hides when idle) ===
        // Reuses MediaWidget (NowPlayingCard + MPRIS); collapses to zero height and
        // out of the focus chain when there is no player (hasPlayer false). Up →
        // the QuickActions row via the shared focus chain (previousRow). Its
        // Left/Right/Return transport nav is internal.
        MediaWidget {
            id: mediaWidget
            Layout.fillWidth: true
            Layout.leftMargin: Units.gridUnit
            Layout.rightMargin: Units.gridUnit
            Layout.topMargin: hasPlayer ? Units.spacingMD : 0
            Layout.preferredHeight: hasPlayer ? implicitHeight : 0
            // visible is inherited from the Widget base (`visible: wantVisible`,
            // and MprisPlayerBase sets wantVisible = hasPlayer) — no override here.
            previousRow: drawerActions
            onEscaped: root.closed()
        }

        // === Spacer — absorbs slack so the status line pins to the bottom ===
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
        }

        // === Status glyph line (non-focusable readout, bottom-pinned) ===
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 2
            color: Theme.surfaceBorder
        }

        Flow {
            Layout.fillWidth: true
            Layout.leftMargin: Units.gridUnit
            Layout.rightMargin: Units.gridUnit
            Layout.topMargin: Units.spacingMD
            Layout.bottomMargin: Units.spacingLG
            spacing: Units.spacingSM

            // Online — always available (NetworkManager singleton).
            StatusPill {
                pillState: NetworkManager.connected ? "good" : "bad"
                text: NetworkManager.connected ? "Online" : "Offline"
            }

            // Volume — always available (AudioController singleton).
            StatusPill {
                pillState: "neutral"
                showDot: false
                text: AudioController.muted ? "\u{1F507} Muted" : "\u{1F509} " + AudioController.volume + "%"
            }

            // Controller battery — only when a wireless pad reports a level.
            StatusPill {
                visible: root._batteryPad !== null
                showDot: false
                pillState: (root._batteryPad !== null && root._batteryPad.batteryLevel <= 15) ? "bad" : "good"
                text: root._batteryPad !== null ? "\u{1F50B} " + root._batteryPad.batteryLevel + "%" : ""
            }

            // Notifications — only when there is something unread (explicit empty
            // state: no pill at zero, mirroring the QuickActions CountBadge).
            // "neutral" (not "warn") — a warn pill renders gold text, and the
            // palette rule forbids gold for text; this is always-on chrome.
            StatusPill {
                visible: NotificationManager.unreadCount > 0
                pillState: "neutral"
                showDot: false
                text: "\u{1F514} " + NotificationManager.unreadCount
            }
        }
    }

    // Resume-row context menu (Resume / Quit App). Declared as a child of the
    // drawer ROOT, so it lands in Drawer's contentContainer and paints inside the
    // panel (z: 20, above the ColumnLayout) — its scrim therefore dims the drawer
    // only, not the whole screen, which is the intended read. Hosting it here
    // rather than bubbling an anchorRect up to ShellLayout (the Volume/Network
    // pattern) keeps the drawer self-contained and gives BOTH instances — the
    // idle navDrawer and the appRunning overlayNavDrawer — the feature for free.
    PopoverMenu {
        id: rowContextMenu
        onClosed: {
            rowContextMenu.opened = false;
            // Return focus to the row we came from — but only if the drawer is
            // still open. "Resume" fires root.closed() before this handler runs;
            // ShellLayout's onClosed then drives its home-focus timer, and
            // grabbing focus into a closing drawer would fight it.
            if (root.opened)
                navList.forceActiveFocus();
        }
    }

    // X-face context popover on a resume row: Resume (focus the live window) or
    // Quit App (close it). Restores the menu the hero rail used to provide for
    // free; the actions mirror HomeScreen._recentContext's running branch.
    // Returns true only when a menu opened, so the key handler leaves the event
    // unaccepted on the Home row.
    function _openRowContext(index) {
        let items = root.navModel;
        if (index < 0 || index >= items.length)
            return false;
        let row = items[index];
        if (row.kind !== "resume" || !row.entry)
            return false;
        let addr = row.entry.address;
        if (!addr || addr === "")
            return false;
        let cls = row.entry.windowClass || "";
        // Anchor over the focused row. PopoverMenu draws ABOVE targetY and clamps
        // to its own bounds, so the row TOP is the right anchor (same as the card
        // popovers). Map to rowContextMenu, NOT to root: this popover lives inside
        // drawerPanel, which is translated by `x` during the slide animation, so
        // mapping to the full-screen Drawer FocusScope would be off by that much.
        // A delegate that hasn't been created yet yields null — the popover then
        // falls back to its clamped top-left rather than throwing.
        let item = navList.itemAtIndex(index);
        if (item) {
            let pos = item.mapToItem(rowContextMenu, item.width / 2, 0);
            rowContextMenu.targetX = pos.x;
            rowContextMenu.targetY = pos.y;
        }
        rowContextMenu.actions = [
            {
                label: "Resume",
                hint: "A: Resume",
                action: function () {
                    root.appFocusRequested(addr, row.entry.windowClass || "");
                    root.closed();
                }
            },
            {
                label: "Quit App",
                hint: "A: Quit App",
                action: function () {
                    root.appCloseRequested(addr);
                }
            },
            {
                // Label reflects CURRENT state, so the row says what pressing it
                // will do rather than what the app is. Keyed on the window class,
                // not the address: the mute must survive the app closing and
                // reopening, and a per-window mute would silently lapse.
                //
                // Guarded on a non-empty class — a window with no class has
                // nothing to key a mute on, and an empty key would match every
                // unattributed stream.
                label: SettingsStore.isAppMuted(cls) ? "Unmute App" : "Mute App",
                // A disabled item stays visible and selectable precisely so its
                // hint can explain itself (see PopoverMenu), so say why rather
                // than leaving it blank.
                hint: cls === "" ? "Unavailable: this window reports no app class" : (SettingsStore.isAppMuted(cls) ? "A: Unmute App" : "A: Mute App"),
                enabled: cls !== "",
                action: function () {
                    if (cls === "")
                        return;
                    SettingsStore.setAppMuted(cls, !SettingsStore.isAppMuted(cls));
                }
            }
        ];
        rowContextMenu.opened = true;
        rowContextMenu.forceActiveFocus();
        return true;
    }

    function _activateNav(index) {
        let items = root.navModel;
        if (index < 0 || index >= items.length)
            return;
        var row = items[index];
        switch (row.kind) {
        case "home":
            root.homeSelected();
            break;
        case "resume":
            // Jump back into the running window, then close the drawer.
            root.appFocusRequested(row.entry.address, row.entry.windowClass || "");
            root.closed();
            break;
        }
    }
}

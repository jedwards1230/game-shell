import QtQuick
import QtQuick.Effects
import QtQuick.Layouts
import "../../components"
import "../../components/lib/artMemo.js" as ArtMemo

// Portrait poster card for the Steam widget (Recently Played / Library). Renders
// the Steam CDN library poster (`library_600x900.jpg`) with an automatic
// fallback to the 16:9 `header.jpg` when the portrait art 404s (older titles
// often lack the portrait asset). Controller-focusable like PlexCard/AppCard: a
// crimson FocusFrame ring on focus, `activated` on A/Return (the host launches
// the game over the stream).
Item {
    id: root

    property string title: ""
    // Portrait library poster URL on Steam's CDN (built daemon-side off the appid).
    property string art: ""
    // Host-served local Steam library art (`{host}/art/{appid}`) — the daemon's
    // fallback for newer titles whose CDN art 404s (the CDN simply lacks
    // `library_600x900`/`header` for some unreleased/just-released games, but the
    // gaming PC has the portrait capsule cached locally).
    property string localArt: ""
    // 16:9 header.jpg on the CDN — last image fallback before the letter initial.
    property string headerArt: ""

    // Poster-only mode (small size): hide the title caption band so the card
    // collapses to just the poster. The host row sizes itself to match.
    property bool showCaption: true

    // True when this game is the one currently running on the host (from the
    // daemon's `runningAppid`, NOT a locally-tracked tap). Shows an ember running
    // dot inline with the title (matching the recent-apps row / BaseCard) so the
    // user sees what's live regardless of which client started it.
    property bool playing: false

    // Running-game lockdown: true when a game is running on the host AND this card
    // is NOT that game. A locked card is dimmed and fully non-interactive (no
    // hover/click, no A/Return activation, not an activatable Accessibility
    // target) so the user can only act on the running card. False (the default,
    // and always when nothing runs) renders exactly as before.
    property bool locked: false

    // Poster geometry (set by the host row so all cards match).
    property int posterWidth: Math.round(Theme.cardWidth * 0.62)
    property int posterHeight: Math.round(posterWidth * 1.5)

    readonly property bool isFocused: (activeFocus && !InputMode.mouseMode) || (mouseArea.containsMouse && InputMode.mouseMode)

    // Art fallback chain: host-local capsule → CDN portrait → CDN header → letter
    // initial. `_artCandidates` is the ordered, non-empty URL list; on each load
    // error we advance `_artIdx` to the next, and when the last also fails the
    // Image isn't Ready so the letter placeholder shows through. Resets to the top
    // whenever any source URL changes.
    //
    // Local art is FIRST (ahead of the CDN portrait) on purpose: for an installed
    // game the host sidecar serves the real high-res library capsule off Steam's
    // own librarycache, whereas Steam's CDN sometimes returns a ~1.6KB placeholder
    // stub for `library_600x900.jpg` (e.g. Battlefield 6) that loads as a
    // "successful" near-empty image — Image.status is Ready, not Error, so the
    // error-driven fallback below never advances past it and the card renders
    // blank. Trying the LAN sidecar first sidesteps the CDN-stub trap entirely;
    // the CDN portrait/header remain fallbacks for not-installed games (empty
    // localArt, filtered out) and for when the sidecar host is unreachable
    // (localArt errors → advance to CDN).
    readonly property var _artCandidates: [root.localArt, root.art, root.headerArt].filter(u => u !== "")
    // Start at the first candidate not ALREADY known dead, not blindly at 0. The
    // daemon emits `localArt` for every entry whether or not the host has a local
    // capsule, so for those games candidate 0 is a deterministic 404 — and these
    // delegates are recreated on every library poll (plain ListView, no
    // reuseItems, freshly built model), so without the memo the same dead request
    // was re-issued over and over. (ArtMemo is cleared whenever the sidecar host
    // becomes reachable again — see SteamLibraryView.hostReachable — so a sleeping
    // host cannot permanently demote the LAN capsule.)
    //
    // This is a BINDING, and the three former `onArtChanged` / `onLocalArtChanged`
    // / `onHeaderArtChanged` reset handlers are gone on purpose. They read the
    // `_artCandidates` sibling binding during the same notify cascade that had
    // just changed one of its inputs, so which value they saw depended on
    // evaluation order: on the `createObject` path the list lagged by one
    // assignment and the reset landed on the known-dead candidate, making the memo
    // a no-op. The binding here has no such ordering hazard — it re-evaluates
    // whenever `_artCandidates` genuinely changes and stays live until the first
    // `Image.Error` writes `_artIdx` imperatively, which is exactly the intended
    // lifecycle: a JS-array model reassignment destroys and recreates the delegate
    // rather than mutating these URLs in place.
    property int _artIdx: ArtMemo.firstUntried(root._artCandidates)
    readonly property string _artSource: root._artIdx < root._artCandidates.length ? root._artCandidates[root._artIdx] : ""

    signal activated
    // Context popover trigger — emitted on the X face (daemon altAction → KEY_X)
    // ONLY for the running game's card (`playing`). Locked cards (a different game
    // is live) stay fully non-interactive and never emit this; non-running cards
    // with nothing running also don't (there's no active session to act on).
    signal contextRequested

    width: posterWidth
    height: posterHeight + (root.showCaption ? captionCol.implicitHeight + Units.spacingSM : 0)

    // Dimming a locked card is done with a dark SCRIM over the poster (below),
    // NOT card `opacity`: lowering opacity bleeds the theme background through, so
    // in light mode a disabled card washed out to white. A scrim darkens the
    // poster the same way in both light and dark mode.

    // A locked card is not a pressable button — expose it as a static image so
    // accessibility tooling doesn't advertise an activation that's disabled.
    Accessible.role: root.locked ? Accessible.Graphic : Accessible.Button
    Accessible.name: root.title
    Accessible.description: root.playing ? "Running" : ""
    Accessible.focusable: !root.locked
    Accessible.onPressAction: if (!root.locked)
        root.activated()

    // Mirror BaseCard: when leaving mouse-mode with the pointer still over this
    // card, claim controller focus so the crimson ring lands where the cursor was.
    Connections {
        target: Theme
        function onMouseModeChanged() {
            if (!InputMode.mouseMode && mouseArea.containsMouse) {
                if (root.ListView.view)
                    root.ListView.view.currentIndex = root.ListView.view.indexAt(root.x, root.y);
                root.forceActiveFocus();
            }
        }
    }

    z: root.isFocused ? 10 : 0

    ColumnLayout {
        anchors.fill: parent
        spacing: Units.spacingSM

        // === Poster ===
        FocusFrame {
            id: frame
            focused: root.isFocused
            radius: Units.radiusMD
            Layout.preferredWidth: root.posterWidth
            Layout.preferredHeight: root.posterHeight
            Layout.alignment: Qt.AlignHCenter

            // Poster surface, corner-rounded via a MultiEffect mask. A Rectangle's
            // `clip` only clips the bounding BOX (its `radius` never reached the
            // child Image), so the art kept square corners that poked past the
            // rounded scrim as bright pixels. Masking the whole surface to a rounded
            // rect rounds the art, the letter fallback, AND the scrim together —
            // clean rounded corners, no bright-corner gap.
            Item {
                id: posterSurface
                anchors.fill: parent
                layer.enabled: true
                layer.smooth: true
                layer.effect: MultiEffect {
                    maskEnabled: true
                    maskSource: posterMask
                    // Soft mask edge so the rounded corners stay anti-aliased.
                    maskThresholdMin: 0.5
                    maskSpreadAtMin: 1.0
                }

                Rectangle {
                    anchors.fill: parent
                    color: Theme.surface
                }

                Image {
                    id: poster
                    anchors.fill: parent
                    source: root._artSource
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                    cache: true
                    visible: status === Image.Ready
                    // On a load error record the dead URL, then advance to the
                    // next candidate in the real chain order (local → portrait →
                    // header, matching _artCandidates at :49-50/:65 — the old
                    // comment here claimed "portrait → local → header", which
                    // contradicted the code). Stop at the last so the letter
                    // placeholder shows through. No loop: index only moves
                    // forward. The memo is recorded BEFORE advancing so the next
                    // delegate for this game starts past the failure.
                    onStatusChanged: {
                        if (status === Image.Error) {
                            ArtMemo.markBad(root._artSource);
                            if (root._artIdx < root._artCandidates.length - 1)
                                root._artIdx += 1;
                        }
                    }
                }

                // Fallback while loading / when art is missing: the title initial
                // (or a controller glyph when there's no title yet).
                Text {
                    anchors.centerIn: parent
                    visible: poster.status !== Image.Ready
                    text: root.title !== "" ? root.title.charAt(0).toUpperCase() : "\u{1F3AE}"
                    font.pixelSize: Units.iconSizeLG
                    font.bold: true
                    color: Theme.textMuted
                }

                // Locked-game scrim — a different game is running, so this card is
                // disabled. A dark overlay (theme-independent) darkens the poster so
                // it reads as inactive in both light and dark mode; using card
                // opacity instead would let the light theme bg show as washed white.
                Rectangle {
                    anchors.fill: parent
                    visible: root.locked
                    color: Qt.rgba(0, 0, 0, 0.66)
                }
            }

            // Rounded-rect alpha mask for `posterSurface` (rendered to a layer, not
            // drawn directly); its rounded corners become the poster's corners.
            Item {
                id: posterMask
                anchors.fill: parent
                visible: false
                layer.enabled: true

                Rectangle {
                    anchors.fill: parent
                    radius: Units.radiusMD
                }
            }
        }

        // === Caption ===
        ColumnLayout {
            id: captionCol
            visible: root.showCaption
            Layout.preferredWidth: root.posterWidth
            Layout.alignment: Qt.AlignHCenter
            spacing: 2

            RowLayout {
                Layout.fillWidth: true
                spacing: 6

                // Per-game running indicator — ember dot beside the title,
                // matching the recent-apps row (BaseCard). Ember (#e06236) is
                // distinct from the crimson focus ring, so "running" is never
                // confused with "focused"; dual cue (color + circular shape) is
                // colourblind-safe. Collapses out of the row when this game isn't
                // the one live on the host. This is the per-game signal, separate
                // from the header's session indicator (any stream active at all).
                Rectangle {
                    visible: root.playing
                    Layout.alignment: Qt.AlignVCenter
                    implicitWidth: 8
                    implicitHeight: 8
                    radius: 4
                    color: Theme.ember
                    Accessible.ignored: true
                }

                MarqueeText {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Theme.fontSmall * 1.3
                    animate: root.isFocused
                    text: root.title
                    font.pixelSize: Theme.fontSmall
                    font.bold: true
                    // Mute a locked card's title so it reads disabled, matching the
                    // poster scrim above.
                    color: root.locked ? Theme.textMuted : Theme.textPrimary
                }
            }
        }
    }

    MouseArea {
        id: mouseArea
        anchors.fill: parent
        // Locked cards take no hover or click — only the running card is live.
        enabled: !root.locked
        hoverEnabled: !root.locked
        cursorShape: Qt.PointingHandCursor
        onPositionChanged: mouse => {
            let p = mapToItem(null, mouse.x, mouse.y);
            InputMode.pointerMoved(p.x, p.y);
        }
        onClicked: {
            InputMode.enterMouseMode();
            root.forceActiveFocus();
            root.activated();
        }
    }

    // A/Return activation is suppressed while locked (belt-and-braces — a locked
    // card also can't hold focus, since the host row skips it).
    Keys.onReturnPressed: if (!root.locked)
        root.activated()
    Keys.onEnterPressed: if (!root.locked)
        root.activated()

    // X face → context popover (Resume / Quit) — ONLY on the running game's card.
    // A locked card (a different game running) is non-interactive; a card with
    // nothing running has no live session to act on, so neither emits.
    Keys.onPressed: event => {
        if (event.key === Qt.Key_X && root.playing && !root.locked) {
            root.contextRequested();
            event.accepted = true;
        }
    }
}

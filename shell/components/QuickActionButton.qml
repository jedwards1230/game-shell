import QtQuick
import "lib/glyphMetrics.js" as GlyphMetrics

// One circular quick-action glyph in the QuickActions row. Renders an SVG icon
// (from IconTheme) with a Unicode fallback glyph, a hover/focus background, an
// optional extra-overlay slot (e.g. CountBadge), and genuine-move-only
// mouse-mode handling (#45).
//
// The parent QuickActions FocusScope drives focus — this component is NOT a
// FocusScope itself. Focus ring is painted when `rowActiveFocus && !mouseMode
// && currentIndex === index`.
Rectangle {
    id: glyph

    // Geometry (driven by the parent QuickActions row).
    property int iconSize: 100
    property int imgSize: 62

    // This glyph's index in the row + the row's current selection.
    property int index: 0
    property int currentIndex: -1
    property bool rowActiveFocus: false

    // Visual content.
    property string iconPath: ""        // full file:// path or "" → fallback
    property string fallbackGlyph: ""
    property color fallbackColor: Theme.textMuted

    // Accessibility label.
    property string a11yName: ""

    // Extra overlay content (e.g. CountBadge) hosted in an anchors.fill Item.
    default property alias extra: extraHolder.data

    // Exposes the inner MouseArea's containsMouse so the parent's
    // onMouseModeChanged block can read per-glyph hover state.
    readonly property bool hovered: mouseArea.containsMouse

    signal activated

    width: iconSize
    height: iconSize
    radius: iconSize / 2
    color: mouseArea.containsMouse && InputMode.mouseMode ? Theme.surfaceHover : "transparent"
    border.width: glyph.rowActiveFocus && !InputMode.mouseMode && glyph.currentIndex === glyph.index ? 3 : 0
    border.color: Theme.focusBorder

    Accessible.role: Accessible.Button
    Accessible.name: glyph.a11yName
    Accessible.focusable: true
    Accessible.onPressAction: glyph.activated()

    Behavior on color {
        ColorAnimation {
            duration: 150
        }
    }

    Image {
        id: iconImg
        anchors.centerIn: parent
        source: glyph.iconPath
        sourceSize: Qt.size(glyph.imgSize, glyph.imgSize)
        width: glyph.imgSize
        height: glyph.imgSize
        fillMode: Image.PreserveAspectFit
        visible: glyph.iconPath !== "" && status === Image.Ready
    }

    // Fallback glyph, centered on its INK rather than on its font line box. The
    // row mixes glyphs from several Unicode blocks, whose ink sits at different
    // heights inside the one line box they all share, so line-box centering
    // leaves them visibly unaligned with each other and with the SVG icons
    // beside them. See lib/glyphMetrics.js for the rule, and for why the
    // per-glyph hand-tuned nudges this replaced kept drifting.
    Text {
        id: fallbackText
        anchors.centerIn: parent
        anchors.verticalCenterOffset: GlyphMetrics.centerOffset(glyphFont.ascent, glyphFont.descent, glyphInk.tightBoundingRect.y, glyphInk.tightBoundingRect.height)
        text: glyph.fallbackGlyph
        font.pixelSize: glyph.imgSize
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        color: glyph.fallbackColor
        visible: !(glyph.iconPath !== "" && iconImg.status === Image.Ready)
    }

    FontMetrics {
        id: glyphFont
        font: fallbackText.font
    }

    TextMetrics {
        id: glyphInk
        font: fallbackText.font
        text: glyph.fallbackGlyph
    }

    Item {
        id: extraHolder
        anchors.fill: parent
    }

    MouseArea {
        id: mouseArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        // Genuine-move-only mouse-mode flip (#45): scene-root coords delta
        // filtered by InputMode.pointerMoved. No onEntered (content-scroll false
        // trigger). mapToItem(null,...) maps to scene root.
        onPositionChanged: mouse => {
            let p = mapToItem(null, mouse.x, mouse.y);
            InputMode.pointerMoved(p.x, p.y);
        }
        onClicked: glyph.activated()
    }
}

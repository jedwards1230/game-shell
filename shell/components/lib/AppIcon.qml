import QtQuick
import "../"
import "iconMemo.js" as IconMemo

// App icon resolver: Freedesktop `image://icon/…` with a letter-initial
// fallback when the icon isn't in the theme. Used in AppCard and LaunchOverlay.
//
// NOTE: AppCard uses an imperative _refreshIcon() to clear the source on
// `app` change and avoid ListView stale-texture bugs — that logic stays in
// AppCard.qml on top of this component. AppIcon itself is purely declarative.
//
// Usage:
//   AppIcon {
//       iconSource: root.app.icon    // may be "" or undefined
//       fallbackText: root.app.name  // first char is uppercased
//       iconSize: Units.iconSizeXL   // optional, defaults to Units.iconSizeXL
//   }
Item {
    id: root

    property string iconSource: ""
    property string fallbackText: "?"
    property int iconSize: Units.iconSizeXL

    // "Is the size we would request at trustworthy?" — TWO conditions, both
    // required before any `image://icon/` request is issued:
    //
    //  * `Units.screenReady` — before the compositor has reported a real screen
    //    height the entire scale is a placeholder, and on this device the
    //    placeholder is double the real one. Requesting during that window means
    //    every icon is fetched at a size that is immediately thrown away and
    //    re-fetched (`cache: false` below guarantees the second fetch), and it
    //    would let the negative memo learn from failures observed at the wrong
    //    scale. Waiting costs nothing visible: the letter fallback already renders.
    //  * `iconSize >= 8` — a 0/near-0 size reaches the provider as
    //    `Qt.size(0, 0)`, which it clamps to 1px × DPR, i.e. the QSize(2, 2)
    //    requests that fail for EVERY icon whether or not it is in the theme.
    //
    // Because the source binding below is gated on this, nothing can be requested
    // — and therefore nothing can be memoised — at an untrustworthy size. That is
    // what makes the memo's "only record a real failure" invariant structural
    // rather than a convention the caller has to remember.
    readonly property bool _sizeValid: Units.screenReady && iconSize >= 8

    implicitWidth: iconSize
    implicitHeight: iconSize

    Image {
        id: iconImage
        anchors.fill: parent
        // Ask the provider only when there is a name to ask for, the size is
        // real, and the name is not already known-absent from the theme.
        //
        // The IconMemo lookup is a plain JS call, so it is NOT reactive — this
        // binding does not re-evaluate when a name is later marked missing. That
        // is intentional: the memo is read at binding evaluation (i.e. per
        // delegate construction / per iconSource change), which is exactly the
        // moment that matters, and the shell recreates these delegates
        // constantly. Do NOT "fix" this by making the memo a QML singleton — a
        // reactive memo would invalidate every AppIcon binding in the shell on
        // each newly discovered missing name, which is the churn this change
        // exists to remove.
        source: (root.iconSource !== "" && root._sizeValid && !IconMemo.isMissing(root.iconSource)) ? "image://icon/" + root.iconSource : ""
        sourceSize: Qt.size(root.iconSize, root.iconSize)
        fillMode: Image.PreserveAspectFit
        // cache: false is LOAD-BEARING — it fixes the #194 stale-neighbour-
        // texture bug on ListView delegate recycling (aee043c / e371e8e). Do not
        // flip it to true to "save" the reloads; the memo above is what removes
        // the repeat requests.
        cache: false
        visible: status === Image.Ready && source != ""

        // Teach the memo, but only from a failure we can trust. The `_sizeValid`
        // re-check here is defensive redundancy, not the actual guard: the source
        // binding above already refuses to issue an untrustworthy request, and
        // dropping `source` back to "" moves the Image to Null rather than Error,
        // so this branch cannot currently be reached with `_sizeValid` false. It
        // is kept so the invariant survives a future edit to that binding.
        //
        // KNOWN GAP — this only catches names that genuinely reach Image.Error.
        // `image://icon/` can instead return a *Ready* magenta placeholder for
        // some theme-missing names (documented in aee043c); those never error,
        // so they are not memoised and keep being requested. Solving the
        // placeholder case needs provider-side (or pixel-level) detection and is
        // deliberately out of scope here.
        onStatusChanged: {
            if (status === Image.Error && root._sizeValid && root.iconSource !== "")
                IconMemo.markMissing(root.iconSource);
        }
    }

    Text {
        visible: !iconImage.visible
        anchors.fill: parent
        text: (root.fallbackText || "?").charAt(0).toUpperCase()
        font.pixelSize: Math.round(root.iconSize * 0.75)
        font.bold: true
        color: Theme.textSecondary
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }
}

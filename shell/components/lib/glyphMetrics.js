.pragma library

// Optical vertical centering for a single fallback glyph.
//
// THE PROBLEM. `anchors.centerIn` centers a Text item's LINE BOX — ascent plus
// descent, a constant for the whole font — not the ink the glyph actually
// paints. Every glyph in a font shares that box no matter how much of it the
// glyph fills, so where the ink lands inside it varies per character. A glyph
// whose ink sits low in the box (☾ ☀ ◐ ⊞) renders visibly below the middle of
// its circular button; one that sits high renders above it. The QuickActions
// row draws its fallbacks from several Unicode blocks at once — Miscellaneous
// Symbols, Mathematical Operators — whose ink boxes disagree, so no single
// line-box centering can align them all.
//
// WHY IT KEEPS COMING BACK. The previous fix was a per-glyph `glyphOffsetY`
// tuned by eye against one font at one size. That is unfalsifiable (nothing
// fails when it drifts), it silently mis-centers at any other `imgSize`
// because it was a fixed fraction of one, and every newly added glyph starts
// wrong until someone notices. It had already been re-tuned twice, and the
// second pass over-corrected the theme toggle into sitting ABOVE the row's
// center line.
//
// THE RULE. Center the INK, not the box. Both are measured against the same
// origin — the baseline — so the result is a property of the font and the
// character, not of anyone's judgement:
//
//     line box spans  -ascent .. +descent   → center at (descent - ascent) / 2
//     ink spans       tightY  .. tightY + h → center at tightY + h / 2
//
// and the offset that moves the ink's center onto the box's center is the
// difference. Sign matches `verticalCenterOffset`: negative lifts the glyph.
//
// Everything here is pure so it can be tested headlessly; the caller supplies
// the metrics (QML `FontMetrics` + `TextMetrics.tightBoundingRect`).

// Offset to add to a Text's `anchors.verticalCenterOffset` so the glyph's ink
// is centered in its parent rather than its line box.
//
//   ascent, descent — font metrics, both positive, in pixels
//   tightY, tightHeight — the glyph's ink rect RELATIVE TO THE BASELINE, i.e.
//     QML's `TextMetrics.tightBoundingRect` (tightY is negative above baseline)
//
// Returns 0 for a glyph with no ink (empty string, or a character the font has
// no outline for) — there is nothing to center, and guessing would move a
// tofu box off-center for no reason.
function centerOffset(ascent, descent, tightY, tightHeight) {
    if (!_finite(ascent) || !_finite(descent) || !_finite(tightY) || !_finite(tightHeight))
        return 0;
    if (tightHeight <= 0)
        return 0;
    var boxCenter = (descent - ascent) / 2;
    var inkCenter = tightY + tightHeight / 2;
    return boxCenter - inkCenter;
}

function _finite(v) {
    return typeof v === "number" && isFinite(v);
}

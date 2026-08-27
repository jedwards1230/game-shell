import QtQuick
import QtTest
import "../../shell/components/lib/glyphMetrics.js" as GlyphMetrics

// Headless tests for fallback-glyph optical centering. glyphMetrics.js is a
// pure `.pragma library` imported by its real source path (zero drift) — no
// Quickshell, no stubs, no font.
//
// WHAT THESE PIN. The QuickActions row centers each fallback glyph with
// `anchors.centerIn`, which centers the font's LINE BOX — a constant shared by
// every character — not the ink a particular glyph paints inside it. Symbols
// drawn from different Unicode blocks (☾ ☀ ◐ from Miscellaneous Symbols, ⊞ from
// Mathematical Operators) fill that box at different heights, so line-box
// centering leaves them visibly unaligned with each other and with the SVG
// icons beside them.
//
// This replaced per-glyph offsets tuned by eye. Those were unfalsifiable —
// nothing failed when they drifted — and they were expressed as a fraction of
// one `imgSize`, so they mis-centered at every other size. They had been
// re-tuned twice, and the second pass over-corrected the theme toggle into
// sitting ABOVE the row's center line, which is the bug that prompted this.
// The cases below are the properties a hand-tuned constant cannot promise.
TestCase {
    id: testCase
    name: "GlyphMetrics"

    // A glyph that exactly fills the box needs no correction: ink from the
    // baseline up to `ascent`, nothing below.
    function test_glyph_filling_the_box_is_already_centered() {
        // box spans -40..+10 (center -15); ink spans -40..+10 (center -15)
        compare(GlyphMetrics.centerOffset(40, 10, -40, 50), 0);
    }

    // THE REGRESSION. A glyph sitting low in the box must be lifted, and by an
    // amount that lands its ink dead-center — not by whatever looked right.
    function test_low_sitting_glyph_is_lifted_exactly_onto_center() {
        // ascent 40, descent 10 → box center at (10-40)/2 = -15
        // ink -20..+5 → ink center -7.5 ; offset = -15 - (-7.5) = -7.5 (up)
        compare(GlyphMetrics.centerOffset(40, 10, -20, 25), -7.5);
    }

    // ...and the converse, which the old hand-tuned nudge could not express:
    // every offset it applied was negative, so a HIGH-sitting glyph could only
    // ever be pushed further up. Over-correction had no way to self-report.
    function test_high_sitting_glyph_is_pushed_down() {
        // ink -40..-20 → ink center -30 ; offset = -15 - (-30) = +15 (down)
        var offset = GlyphMetrics.centerOffset(40, 10, -40, 20);
        compare(offset, 15);
        verify(offset > 0);
    }

    // Scale-invariance. The replaced nudges were a fixed fraction of one
    // `imgSize`; doubling the row's icon size silently doubled the error.
    // Doubling the metrics must double the offset — nothing else.
    function test_offset_scales_with_font_size() {
        var small = GlyphMetrics.centerOffset(40, 10, -20, 25);
        var large = GlyphMetrics.centerOffset(80, 20, -40, 50);
        compare(large, small * 2);
    }

    // A glyph the font has no outline for (tofu, or an empty string) has no ink
    // to center. Return 0 rather than inventing a shift for a box that is
    // already where it belongs.
    function test_glyph_without_ink_is_left_alone() {
        compare(GlyphMetrics.centerOffset(40, 10, 0, 0), 0);
        compare(GlyphMetrics.centerOffset(40, 10, -20, -5), 0);
    }

    // Metrics can arrive unset before the font resolves; a NaN must not become
    // a NaN anchor offset, which silently un-positions the glyph entirely.
    function test_missing_metrics_do_not_produce_a_nan_offset() {
        compare(GlyphMetrics.centerOffset(NaN, 10, -20, 25), 0);
        compare(GlyphMetrics.centerOffset(40, 10, undefined, 25), 0);
        compare(GlyphMetrics.centerOffset(40, 10, -20, undefined), 0);
    }
}

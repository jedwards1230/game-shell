import QtQuick
import QtTest
import components
import components.lib
import "../../shell/components/lib/iconMemo.js" as IconMemo

// Headless tests for the negative icon memo (shell/components/lib/iconMemo.js)
// and for AppIcon's use of it.
//
// Two halves, deliberately kept apart:
//
//  1. The PURE API is exercised against the real source file, imported by path
//     (`../../shell/components/lib/iconMemo.js`) exactly like tst_appquirks /
//     tst_prewarm do — zero drift, no stubs.
//
//  2. The AppIcon integration is exercised through the REAL AppIcon component
//     (copied into the assembled module by run.sh) and asserts only on what the
//     component OBSERVABLY does — whether its Image actually requests
//     `image://icon/…`. It deliberately does NOT poke the memo directly: a
//     `.pragma library` is shared per ENGINE per URL, and AppIcon imports the
//     copy under tests/qml/.build, which is a different URL from the source-path
//     import above. Asserting through behaviour keeps the two halves honest and
//     independent (the AppIcon half therefore uses unique icon names and never
//     calls reset()).
//
// Offscreen there is no `image://icon` provider at all, so every request errors —
// which is precisely the condition the memo is built to stop repeating.
//
// Scope note, so the claims here are not read as broader than they are: the
// AppIcon half pins that an untrustworthy request is never ISSUED (degenerate
// size, and screen scale not yet known), and that a trustworthy failure is
// memoised and suppresses the next request. It does NOT exercise a `markMissing`
// call made at a bad size — that path is unreachable through the component, since
// dropping `source` to "" moves the Image to Null rather than Error. The
// `_sizeValid` re-check inside `onStatusChanged` is therefore defensive
// redundancy, not a behaviour these tests can trigger.
TestCase {
    id: testCase
    name: "IconMemo"
    when: windowShown
    visible: true
    width: 400
    height: 400

    Item {
        id: holder
        anchors.fill: parent
    }

    Component {
        id: appIconComp
        AppIcon {}
    }

    // The inner Image is AppIcon's first declared child; the letter fallback is
    // the second. Reading it directly keeps AppIcon free of test-only hooks.
    function _img(icon) {
        return icon.children[0];
    }

    function init() {
        IconMemo.reset();
        Units.screenReady = true;
    }

    function cleanup() {
        Units.screenReady = true;
    }

    // === 1. Pure API ========================================================

    function test_unknown_name_is_not_missing() {
        verify(!IconMemo.isMissing("firefox"));
    }

    function test_mark_then_is_missing() {
        IconMemo.markMissing("firefox");
        verify(IconMemo.isMissing("firefox"));
        // Keyed by NAME only — Freedesktop lookup is name-based, so one record
        // covers the name at every size/DPR. Other names are unaffected.
        verify(!IconMemo.isMissing("chromium"));
    }

    function test_mark_is_idempotent() {
        IconMemo.markMissing("firefox");
        IconMemo.markMissing("firefox");
        verify(IconMemo.isMissing("firefox"));
    }

    function test_reset_clears() {
        IconMemo.markMissing("firefox");
        IconMemo.reset();
        verify(!IconMemo.isMissing("firefox"));
    }

    function test_empty_and_non_string_are_safe() {
        // The call sites run inside a binding and a status handler; a throw there
        // would take the delegate down. Both directions must no-op quietly.
        IconMemo.markMissing("");
        IconMemo.markMissing(null);
        IconMemo.markMissing(undefined);
        verify(!IconMemo.isMissing(""));
        verify(!IconMemo.isMissing(null));
        verify(!IconMemo.isMissing(undefined));
    }

    // === 2. AppIcon integration ============================================

    // A valid-size failure is recorded, so the NEXT AppIcon built for the same
    // name never issues the request again — it goes straight to the letter
    // fallback. This is the whole point of the memo.
    function test_valid_size_failure_suppresses_next_request() {
        var name = "tst-memo-valid-" + Date.now();
        var first = appIconComp.createObject(holder, {
            "iconSource": name,
            "iconSize": 120
        });
        verify(first._sizeValid);
        compare(_img(first).source.toString(), "image://icon/" + name);
        tryVerify(function () {
            return _img(first).status === Image.Error;
        }, 2000, "the missing icon errors headless");

        var second = appIconComp.createObject(holder, {
            "iconSource": name,
            "iconSize": 120
        });
        compare(_img(second).source.toString(), "", "a known-missing name is not requested again");
        verify(!_img(second).visible);

        first.destroy();
        second.destroy();
    }

    // THE INVARIANT, half 1 of 2: a failure observed at a DEGENERATE size must
    // never be recorded. A 0/2px request fails for icons that are genuinely
    // present too (that is the QSize(2,2) flood), so memoising it would
    // permanently blank a working icon. Below the floor AppIcon must not even
    // issue the request — which is what makes the invariant structural rather
    // than a convention: nothing is requested, so nothing can be learned.
    function test_degenerate_size_never_memoises() {
        var name = "tst-memo-degenerate-" + Date.now();
        var tiny = appIconComp.createObject(holder, {
            "iconSource": name,
            "iconSize": 0
        });
        verify(!tiny._sizeValid);
        compare(_img(tiny).source.toString(), "", "a degenerate size issues no request at all");
        wait(50);

        // Same name, now at a real size: it must still be requested, i.e. the
        // degenerate pass taught the memo nothing.
        var normal = appIconComp.createObject(holder, {
            "iconSource": name,
            "iconSize": 120
        });
        compare(_img(normal).source.toString(), "image://icon/" + name, "the degenerate failure must not have been memoised");

        tiny.destroy();
        normal.destroy();
    }

    // THE INVARIANT, half 2 of 2: nothing is requested — and therefore nothing can
    // be memoised — before the compositor has reported a real screen height.
    //
    // This is the startup half of the icon flood. `screenHeight` is seeded with a
    // layout-only placeholder so first paint has a usable number, but on the target
    // device that placeholder is DOUBLE the real scale: every icon requested during
    // that window is fetched at a size that is then thrown away and re-fetched
    // (`cache: false` guarantees the second fetch), and any failure seen at the
    // wrong scale would teach the memo something it has no business learning.
    function test_no_request_before_the_screen_scale_is_known() {
        Units.screenReady = false;
        var name = "tst-memo-prescale-" + Date.now();
        var early = appIconComp.createObject(holder, {
            "iconSource": name,
            "iconSize": 120
        });
        verify(!early._sizeValid, "a real size is not enough — the scale must be known");
        compare(_img(early).source.toString(), "", "no request may be issued pre-ready");
        wait(50);
        compare(_img(early).status, Image.Null, "no request means no Error, so nothing to memoise");

        // The screen settles: the request is now issued. This is also the proof
        // that nothing was learned in the dark — the `source` binding re-evaluates
        // on `_sizeValid` and consults the memo, so had the name been recorded
        // during the pre-ready window it would stay "" forever instead.
        Units.screenReady = true;
        wait(50);
        compare(_img(early).source.toString(), "image://icon/" + name, "the request is issued once the scale is known, and the memo did not learn pre-ready");
        early.destroy();
    }

    // An empty icon name is not a "missing icon" — it is "no icon", which has
    // always rendered the letter fallback with no request.
    function test_empty_icon_source_requests_nothing() {
        var icon = appIconComp.createObject(holder, {
            "iconSource": "",
            "iconSize": 120
        });
        compare(_img(icon).source.toString(), "");
        icon.destroy();
    }
}

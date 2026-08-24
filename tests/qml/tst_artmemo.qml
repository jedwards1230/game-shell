import QtQuick
import QtTest
import "../../shell/components/lib/artMemo.js" as ArtMemo

// Headless tests for the negative art-URL memo (shell/components/lib/artMemo.js),
// imported by its real source path like the other pure `.pragma library` helpers
// (tst_appquirks / tst_prewarm / tst_resumemodel) — zero drift, no stubs.
//
// What these pin:
//  - a URL marked bad stays bad for the session, keyed by full URL (the three
//    candidates for one game are independent hosts/paths);
//  - firstUntried() skips known-dead candidates so a recreated SteamCard starts
//    past the daemon's unconditional `localArt` 404 instead of re-issuing it;
//  - firstUntried() returns 0 — NOT an out-of-range index — when every candidate
//    is bad, so the card's walk degrades to exactly the pre-memo behaviour
//    (walk, fail, fall through to the letter placeholder) rather than silently
//    skipping the whole chain after a transient network blip;
//  - empty/malformed input is safe: these run inside a delegate's property
//    binding and its Image.onStatusChanged, where a throw would take the card
//    down mid-scroll.
TestCase {
    id: testCase
    name: "ArtMemo"

    readonly property string localUrl: "http://host.example:47995/art/413150"
    readonly property string cdnUrl: "https://cdn.example/413150/library_600x900.jpg"
    readonly property string headerUrl: "https://cdn.example/413150/header.jpg"

    function init() {
        ArtMemo.reset();
    }

    function test_unknown_url_is_not_bad() {
        verify(!ArtMemo.isBad(localUrl));
    }

    function test_mark_then_is_bad() {
        ArtMemo.markBad(localUrl);
        verify(ArtMemo.isBad(localUrl));
        // Keyed by full URL — marking the sidecar capsule dead says nothing about
        // the CDN candidates for the same game.
        verify(!ArtMemo.isBad(cdnUrl));
        verify(!ArtMemo.isBad(headerUrl));
    }

    function test_reset_clears() {
        ArtMemo.markBad(localUrl);
        ArtMemo.reset();
        verify(!ArtMemo.isBad(localUrl));
    }

    function test_first_untried_fresh_chain_is_zero() {
        compare(ArtMemo.firstUntried([localUrl, cdnUrl, headerUrl]), 0);
    }

    // The real-world case: the daemon emits localArt for every entry, the host
    // sidecar 404s it, and the delegate is rebuilt on every poll.
    function test_first_untried_skips_the_dead_local_capsule() {
        ArtMemo.markBad(localUrl);
        compare(ArtMemo.firstUntried([localUrl, cdnUrl, headerUrl]), 1);
    }

    function test_first_untried_skips_a_run_of_dead_candidates() {
        ArtMemo.markBad(localUrl);
        ArtMemo.markBad(cdnUrl);
        compare(ArtMemo.firstUntried([localUrl, cdnUrl, headerUrl]), 2);
    }

    // All dead → 0, preserving the original walk instead of returning an
    // out-of-range index (which would make _artSource "" and skip the chain).
    function test_first_untried_all_bad_returns_zero() {
        ArtMemo.markBad(localUrl);
        ArtMemo.markBad(cdnUrl);
        ArtMemo.markBad(headerUrl);
        compare(ArtMemo.firstUntried([localUrl, cdnUrl, headerUrl]), 0);
    }

    // A game with no local capsule has it filtered out of _artCandidates, so a
    // shorter chain must behave identically.
    function test_first_untried_short_chain() {
        ArtMemo.markBad(cdnUrl);
        compare(ArtMemo.firstUntried([cdnUrl, headerUrl]), 1);
    }

    function test_empty_and_malformed_inputs_are_safe() {
        compare(ArtMemo.firstUntried([]), 0);
        compare(ArtMemo.firstUntried(null), 0);
        compare(ArtMemo.firstUntried(undefined), 0);
        ArtMemo.markBad("");
        ArtMemo.markBad(null);
        verify(!ArtMemo.isBad(""));
        verify(!ArtMemo.isBad(null));
    }
}

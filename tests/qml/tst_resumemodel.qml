import QtQuick
import QtTest
import "../../shell/components/resumeModel.js" as ResumeModel

// Headless tests for the nav-drawer resume merge (#216). resumeModel.js is a pure
// `.pragma library` module imported by its real source path (zero drift) — no
// Quickshell, no stubs. These lock the merge/dedup invariants the drawer's hero
// zone depends on: running-first ordering by focusHistoryId, recent dedup against
// a matching running window, unmatched-recents appending, and empty-in/empty-out.
TestCase {
    id: testCase
    name: "ResumeModel"

    // Minimal matcher stub mirroring WindowMatcher.execBasename/normalize — the
    // two QML-free helpers resumeModel.js relies on.
    readonly property var matcher: ({
            execBasename: function (exec) {
                if (!exec)
                    return "";
                var cmd = exec.split(/\s/)[0];
                return cmd.split("/").pop().toLowerCase();
            },
            normalize: function (s) {
                return (s || "").toLowerCase().replace(/[-_.]/g, "");
            }
        })

    // --- audio indicators (#445) -------------------------------------------

    readonly property var audioFleet: [
        {
            windowClass: "streaming_client",
            address: "0x1",
            title: "Red Dead Redemption 2",
            focusHistoryId: 0
        },
        {
            windowClass: "tv.plex.Plex",
            address: "0x2",
            title: "Plex HTPC",
            focusHistoryId: 1
        }
    ]

    // The two flags answer deliberately different questions. `audioActive` is the
    // informative one under the workspace policy: it names the app making noise
    // you cannot hear. `userMuted` is the user's OWN mute, never the policy mute
    // — which is true of nearly every app at any moment and would light up every
    // row while saying nothing.
    function test_audio_flags_are_set_per_class() {
        var r = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher, {
            activeClasses: ["streaming_client"],
            mutedClasses: ["tv.plex.Plex"]
        });
        compare(r.length, 2);
        compare(r[0].windowClass, "streaming_client");
        compare(r[0].audioActive, true);
        compare(r[0].userMuted, false);
        compare(r[1].windowClass, "tv.plex.Plex");
        compare(r[1].audioActive, false);
        compare(r[1].userMuted, true);
    }

    // One app can be both: making noise AND muted by the user. That pairing is
    // the whole point of showing two indicators rather than one.
    function test_an_app_can_be_both_active_and_user_muted() {
        var r = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher, {
            activeClasses: ["streaming_client"],
            mutedClasses: ["streaming_client"]
        });
        compare(r[0].audioActive, true);
        compare(r[0].userMuted, true);
    }

    // Omitting the audio argument must keep every existing caller working and
    // render exactly as the drawer did before this existed — both icons hidden.
    function test_audio_is_optional_and_defaults_to_quiet() {
        var r = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher);
        compare(r[0].audioActive, false);
        compare(r[0].userMuted, false);
        var r2 = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher, {});
        compare(r2[0].audioActive, false);
        compare(r2[0].userMuted, false);
    }

    // A recent-but-not-running app has no window, so no class to attribute a
    // stream to or key a mute on. Both flags stay false — which is also why the
    // drawer offers no mute toggle on such a row.
    function test_a_recent_that_is_not_running_is_never_flagged() {
        var recents = [
            {
                name: "Firefox",
                exec: "/usr/bin/firefox"
            }
        ];
        var r = ResumeModel.build([], recents, [], testCase.matcher, {
            activeClasses: [""],
            mutedClasses: [""]
        });
        compare(r.length, 1);
        compare(r[0].running, false);
        compare(r[0].audioActive, false);
        compare(r[0].userMuted, false);
    }

    // A blank class in either list must not match a row whose class is unknown —
    // an empty key would otherwise light up every unattributed row.
    function test_blank_classes_match_nothing() {
        var running = [
            {
                windowClass: "",
                address: "0x9",
                title: "Classless",
                focusHistoryId: 0
            }
        ];
        var r = ResumeModel.build(running, [], [], testCase.matcher, {
            activeClasses: ["", "steam"],
            mutedClasses: ["", "steam"]
        });
        compare(r[0].audioActive, false);
        compare(r[0].userMuted, false);
    }

    // --- (e) empty inputs → empty array -----------------------------------
    function test_empty_inputs() {
        compare(ResumeModel.build([], [], [], testCase.matcher).length, 0);
        // Null/undefined inputs are guarded → empty, not a throw.
        compare(ResumeModel.build(null, undefined, null, testCase.matcher).length, 0);
    }

    // --- (a) running-only --------------------------------------------------
    function test_running_only() {
        var running = [
            {
                windowClass: "firefox",
                address: "0x1",
                title: "Mozilla Firefox",
                focusHistoryId: 0
            }
        ];
        var r = ResumeModel.build(running, [], [], testCase.matcher);
        compare(r.length, 1);
        compare(r[0].running, true);
        compare(r[0].name, "Mozilla Firefox");
        compare(r[0].address, "0x1");
        compare(r[0].windowClass, "firefox");
    }

    // --- (b) recents-only --------------------------------------------------
    function test_recents_only() {
        var recents = [
            {
                name: "Steam",
                exec: "/usr/bin/steam %U",
                comment: "Games",
                icon: "steam"
            }
        ];
        var allApps = [
            {
                name: "Steam",
                exec: "steam",
                icon: "steam-icon"
            }
        ];
        var r = ResumeModel.build([], recents, allApps, testCase.matcher);
        compare(r.length, 1);
        compare(r[0].running, false);
        compare(r[0].name, "Steam");
        compare(r[0].exec, "/usr/bin/steam %U");
        // Icon resolved from allApps (name match) over the recent's own icon.
        compare(r[0].icon, "steam-icon");
        compare(r[0].focusHistoryId, 9999);
    }

    // --- (c) running window dedups a matching recent -----------------------
    function test_running_dedups_recent() {
        var running = [
            {
                windowClass: "firefox",
                address: "0xff",
                title: "Firefox",
                focusHistoryId: 0
            }
        ];
        // Two recents: Firefox (should be deduped by the running window via exec
        // basename → class), and a distinct Steam (should survive).
        var recents = [
            {
                name: "Firefox",
                exec: "/usr/bin/firefox",
                icon: "firefox"
            },
            {
                name: "Steam",
                exec: "/usr/bin/steam",
                icon: "steam"
            }
        ];
        var r = ResumeModel.build(running, recents, [], testCase.matcher);
        compare(r.length, 2, "firefox recent deduped, steam kept");
        compare(r[0].running, true);
        compare(r[0].name, "Firefox");
        compare(r[1].running, false);
        compare(r[1].name, "Steam");
    }

    // --- (d) ordering: running by focusHistoryId asc, then unmatched recents
    function test_ordering() {
        var running = [
            {
                windowClass: "alpha",
                address: "0xa",
                title: "Alpha",
                focusHistoryId: 2
            },
            {
                windowClass: "bravo",
                address: "0xb",
                title: "Bravo",
                focusHistoryId: 0
            },
            {
                windowClass: "charlie",
                address: "0xc",
                title: "Charlie",
                focusHistoryId: 1
            }
        ];
        var recents = [
            {
                name: "Delta",
                exec: "delta",
                icon: "delta"
            }
        ];
        var r = ResumeModel.build(running, recents, [], testCase.matcher);
        compare(r.length, 4);
        // Running sorted by focusHistoryId ascending: Bravo(0), Charlie(1), Alpha(2).
        compare(r[0].name, "Bravo");
        compare(r[1].name, "Charlie");
        compare(r[2].name, "Alpha");
        // Unmatched recent appended last.
        compare(r[3].name, "Delta");
        compare(r[3].running, false);
    }

    // --- name fallback: title → name → windowClass -------------------------
    function test_running_name_fallback() {
        var running = [
            {
                windowClass: "someclass",
                address: "0x9",
                focusHistoryId: 0
            }
        ];
        var r = ResumeModel.build(running, [], [], testCase.matcher);
        compare(r[0].name, "someclass", "falls back to windowClass when no title/name");
    }
}

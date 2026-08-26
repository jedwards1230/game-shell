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

    // `userMuted` is the user's OWN mute, never the policy mute — which is true
    // of nearly every app at any moment and would light up every row while
    // saying nothing. There is deliberately no companion "producing audio" flag:
    // the policy already guarantees the app on screen is the only one you can
    // hear, so that question has been made unaskable.
    function test_user_mute_is_set_per_class() {
        var r = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher, {
            mutedClasses: ["tv.plex.Plex"]
        });
        compare(r.length, 2);
        compare(r[0].windowClass, "streaming_client");
        compare(r[0].userMuted, false);
        compare(r[1].windowClass, "tv.plex.Plex");
        compare(r[1].userMuted, true);
    }

    // Omitting the audio argument must keep every existing caller working and
    // render exactly as the drawer did before this existed — no icon.
    function test_audio_is_optional_and_defaults_to_unmuted() {
        var r = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher);
        compare(r[0].userMuted, false);
        var r2 = ResumeModel.build(testCase.audioFleet, [], [], testCase.matcher, {});
        compare(r2[0].userMuted, false);
    }

    // A recent-but-not-running app has no window, so no class to key a mute on.
    // That is also why the drawer offers no mute toggle on such a row.
    function test_a_recent_that_is_not_running_is_never_flagged() {
        var recents = [
            {
                name: "Firefox",
                exec: "/usr/bin/firefox"
            }
        ];
        var r = ResumeModel.build([], recents, [], testCase.matcher, {
            mutedClasses: [""]
        });
        compare(r.length, 1);
        compare(r[0].running, false);
        compare(r[0].userMuted, false);
    }

    // A blank class must not match a row whose class is unknown —
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
            mutedClasses: ["", "steam"]
        });
        compare(r[0].userMuted, false);
    }

    // --- nav-row focus kept by identity (#445) ------------------------------

    // The drawer's nav rows: Home first, then one per running app.
    function _navRows(addresses) {
        var rows = [
            {
                label: "Home",
                kind: "home"
            }
        ];
        for (var i = 0; i < addresses.length; i++) {
            rows.push({
                label: "App " + addresses[i],
                kind: "resume",
                entry: {
                    address: addresses[i]
                }
            });
        }
        return rows;
    }

    function test_addressAt_reads_a_rows_identity() {
        var rows = _navRows(["0xa", "0xb"]);
        compare(ResumeModel.addressAt(rows, 1), "0xa");
        compare(ResumeModel.addressAt(rows, 2), "0xb");
    }

    // Index 0 is Home and carries no entry, so "" doubles as "Home / unknown".
    function test_addressAt_is_blank_for_home_and_out_of_range() {
        var rows = _navRows(["0xa"]);
        compare(ResumeModel.addressAt(rows, 0), "");
        compare(ResumeModel.addressAt(rows, 5), "");
        compare(ResumeModel.addressAt(rows, -1), "");
        compare(ResumeModel.addressAt(null, 1), "");
    }

    // THE regression this replaced. Rows are sorted by focusHistoryId and their
    // membership changes while the drawer is open, so restoring a bare INDEX can
    // land the cursor on a different app — and the next activation would resume
    // the wrong one. Identity follows the app instead.
    function test_focus_follows_the_app_when_a_row_above_it_disappears() {
        var before = _navRows(["0xa", "0xb", "0xc"]);
        var focused = ResumeModel.addressAt(before, 2);      // the user is on 0xb
        compare(focused, "0xb");
        var after = _navRows(["0xb", "0xc"]);                // 0xa exited
        // An index-keyed restore would have replayed 2 and landed on 0xc.
        compare(ResumeModel.indexForAddress(after, focused), 1);
    }

    // The same hazard in the other direction: a newly mapped window lands first
    // and pushes every existing row down one.
    function test_focus_follows_the_app_when_a_new_window_appears_above_it() {
        var before = _navRows(["0xa", "0xb"]);
        var focused = ResumeModel.addressAt(before, 1);      // the user is on 0xa
        var after = _navRows(["0xnew", "0xa", "0xb"]);
        compare(ResumeModel.indexForAddress(after, focused), 2);
    }

    // A row that vanished lands on Home — visible, and the caller then clears the
    // remembered address so a later rebuild cannot revive a stale target.
    function test_a_vanished_app_falls_back_to_home() {
        var after = _navRows(["0xb"]);
        compare(ResumeModel.indexForAddress(after, "0xa"), 0);
    }

    function test_indexForAddress_treats_blank_as_home() {
        var rows = _navRows(["0xa"]);
        compare(ResumeModel.indexForAddress(rows, ""), 0);
        compare(ResumeModel.indexForAddress(rows, null), 0);
        compare(ResumeModel.indexForAddress(null, "0xa"), 0);
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

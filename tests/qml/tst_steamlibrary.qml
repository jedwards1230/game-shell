import QtQuick
import QtTest
import components
import components.lib
import widgets.steamlib

// Headless tests for SteamLibraryView's content-signature adoption guard — the
// change that stopped a 10s poll from tearing down every poster delegate.
//
// The real view is already assembled into the test module (run.sh copies it, and
// now the REAL SteamCard beside it), so these run against production code. The
// stub SocketClient is inert, so `onUpdated` never fires headless — which is why
// the adoption was extracted into the callable `_adoptRails(d)` seam that these
// drive directly.
//
// What these pin:
//  - an identical repoll does NOT reassign the rail. That mattered because the
//    payload is a freshly built list every poll, so a blind assignment changed the
//    property IDENTITY even when the content was byte-identical: `_activeItems`
//    changed, the ListView model reset, EVERY delegate was destroyed and
//    recreated, and every art chain re-walked from candidate 0;
//  - a real content change (including one only visible in an art URL) still gets
//    through;
//  - the signature cannot be spoofed by a delimiter inside a game title;
//  - the `disabled` clear resets the signatures so a later good poll repopulates.
TestCase {
    id: testCase
    name: "SteamLibrary"
    when: windowShown
    visible: true
    width: 900
    height: 600

    Item {
        id: holder
        anchors.fill: parent
    }

    Component {
        id: viewComp
        SteamLibraryView {}
    }

    SignalSpy {
        id: allSpy
        signalName: "allItemsChanged"
    }

    SignalSpy {
        id: recentSpy
        signalName: "recentItemsChanged"
    }

    function _game(appid, name, art) {
        return {
            "appid": appid,
            "name": name,
            "art": art || ("https://cdn.example/" + appid + "/library_600x900.jpg"),
            "localArt": "http://host.example:47995/art/" + appid,
            "headerArt": "https://cdn.example/" + appid + "/header.jpg"
        };
    }

    function _payload(games) {
        return {
            "status": "ok",
            "recentlyPlayed": games,
            "allGames": games
        };
    }

    function _mk() {
        var v = viewComp.createObject(holder);
        verify(v, "the REAL SteamLibraryView instantiates headless");
        allSpy.target = v;
        recentSpy.target = v;
        allSpy.clear();
        recentSpy.clear();
        return v;
    }

    // The whole point: poll, poll again with the identical payload, and the rail
    // must not churn.
    function test_identical_repoll_does_not_churn() {
        var v = _mk();
        v._adoptRails(_payload([_game(1, "Alpha"), _game(2, "Beta")]));
        compare(v.allItems.length, 2);
        compare(allSpy.count, 1, "the first payload is adopted");

        // A *different array object* with identical content — exactly what the
        // daemon reply produces every 10s.
        v._adoptRails(_payload([_game(1, "Alpha"), _game(2, "Beta")]));
        compare(allSpy.count, 1, "an identical repoll must not reassign the rail");
        compare(recentSpy.count, 1);
        compare(v.allItems.length, 2);
        v.destroy();
    }

    function test_content_change_is_adopted() {
        var v = _mk();
        v._adoptRails(_payload([_game(1, "Alpha")]));
        compare(allSpy.count, 1);
        v._adoptRails(_payload([_game(1, "Alpha"), _game(2, "Beta")]));
        compare(allSpy.count, 2, "a new game must get through");
        compare(v.allItems.length, 2);
        v.destroy();
    }

    // The cards render off the art URLs, so a change confined to one must not be
    // swallowed by the signature.
    function test_art_url_change_is_adopted() {
        var v = _mk();
        v._adoptRails(_payload([_game(1, "Alpha", "https://cdn.example/1/a.jpg")]));
        compare(allSpy.count, 1);
        v._adoptRails(_payload([_game(1, "Alpha", "https://cdn.example/1/b.jpg")]));
        compare(allSpy.count, 2, "a changed poster URL must get through");
        v.destroy();
    }

    // Regression: a join on a delimiter could make two different payloads
    // signature-identical and silently freeze the rail. JSON.stringify cannot.
    function test_delimiter_in_a_title_cannot_spoof_the_signature() {
        var v = _mk();
        var sigA = v._librarySig([
            {
                "appid": 1,
                "name": "A|B",
                "art": "",
                "localArt": "",
                "headerArt": ""
            }
        ]);
        var sigB = v._librarySig([
            {
                "appid": 1,
                "name": "A",
                "art": "B",
                "localArt": "",
                "headerArt": ""
            }
        ]);
        verify(sigA !== sigB, "a '|' inside a title must not collide with the field break");

        var sigC = v._librarySig([
            {
                "appid": 1,
                "name": "A;B",
                "art": "",
                "localArt": "",
                "headerArt": ""
            }
        ]);
        var sigD = v._librarySig([
            {
                "appid": 1,
                "name": "A",
                "art": "",
                "localArt": "",
                "headerArt": ""
            },
            {
                "appid": undefined,
                "name": "B",
                "art": "",
                "localArt": "",
                "headerArt": ""
            }
        ]);
        verify(sigC !== sigD, "a ';' inside a title must not collide with the record break");
        v.destroy();
    }

    // A partial reply must not blank a good rail (pre-existing contract, kept).
    function test_partial_reply_leaves_the_other_rail_alone() {
        var v = _mk();
        v._adoptRails(_payload([_game(1, "Alpha")]));
        compare(v.allItems.length, 1);
        v._adoptRails({
            "status": "ok",
            "recentlyPlayed": [_game(2, "Beta")]
        });
        compare(v.allItems.length, 1, "allGames absent -> allItems untouched");
        compare(v.recentItems.length, 1);
        compare(v.recentItems[0].name, "Beta");
        v.destroy();
    }

    // The `disabled` clear zeroes the signatures, so an identical payload arriving
    // afterwards is seen as a change and repopulates rather than being suppressed.
    function test_disabled_clear_allows_repopulation() {
        var v = _mk();
        var games = [_game(1, "Alpha")];
        v._adoptRails(_payload(games));
        compare(v.allItems.length, 1);

        // Mirror the `disabled` branch in onUpdated.
        v._recentSig = "";
        v._allSig = "";
        v.recentItems = [];
        v.allItems = [];
        compare(v.allItems.length, 0);

        v._adoptRails(_payload([_game(1, "Alpha")]));
        compare(v.allItems.length, 1, "the same library must come back after a disabled clear");
        v.destroy();
    }

    function test_empty_payload_is_safe() {
        var v = _mk();
        compare(v._librarySig(null), "");
        compare(v._librarySig(undefined), "");
        compare(v._librarySig([]), "[]");
        v._adoptRails({
            "status": "ok"
        });
        compare(v.allItems.length, 0);
        v.destroy();
    }
}

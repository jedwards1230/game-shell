// The focus model, headless.
//
// Two halves:
//   * the PURE half — focusGraph.js called directly with literal cell arrays.
//     No Items, no windows, no compositor. This is where R1..R5 are pinned.
//   * the WIRED half — a real FocusRouter over real FocusSlots, to prove the
//     glue actually calls the pure half (a perfect pure module wired to nothing
//     defends nothing).
//
// docs/V2_SHELL.md records, per assertion, which mutation of focusGraph.js it
// was confirmed to catch.
import QtQuick
import QtTest
import TvShell
import "qrc:/qt/qml/TvShell/focusGraph.js" as FocusGraph

Item {
    id: harness
    width: 400
    height: 400

    // A 3x3 grid. `off` names cells that are not focusable.
    function grid(off) {
        const cells = [];
        for (let i = 0; i < 9; ++i) {
            const id = "c" + i;
            cells.push({
                "id": id,
                "row": Math.floor(i / 3),
                "column": i % 3,
                "focusable": off.indexOf(id) === -1
            });
        }
        return cells;
    }

    // NOT `id: router`. FocusSlot has a PROPERTY of that name, and inside a
    // delegate the property shadows the outer id, so `router: router` would bind
    // every slot to its own null. A real ergonomic trap in this API, and the
    // reason Main.qml always writes the qualified `root.router`.
    FocusRouter {
        id: theRouter
    }

    Item {
        id: slotHost
        Repeater {
            id: rep
            model: 9
            FocusSlot {
                required property int index
                slotId: "s" + index
                row: Math.floor(index / 3)
                column: index % 3
                router: theRouter
                // Row 1 (cells 3,4,5) is switchable as a unit so the wired half
                // can empty a whole row and watch traversal skip it.
                slotEnabled: row !== 1 || harness.middleRowEnabled
            }
        }
    }

    property bool middleRowEnabled: true

    TestCase {
        name: "FocusGraph"
        when: windowShown

        // --- R5 / basics ---------------------------------------------------

        function test_initial_is_reading_order() {
            compare(FocusGraph.initial(harness.grid([])), "c0");
            // With the top-left disabled, the next in reading order wins — not
            // some registration-order artefact.
            compare(FocusGraph.initial(harness.grid(["c0"])), "c1");
        }

        function test_initial_of_empty_graph_is_null() {
            compare(FocusGraph.initial([]), null);
            compare(FocusGraph.initial(harness.grid(["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"])), null);
        }

        function test_horizontal_moves_within_the_row() {
            const g = harness.grid([]);
            compare(FocusGraph.neighbour(g, "c0", "right"), "c1");
            compare(FocusGraph.neighbour(g, "c1", "left"), "c0");
        }

        // --- R2: no wrap, in any direction ---------------------------------

        function test_no_horizontal_wrap() {
            const g = harness.grid([]);
            compare(FocusGraph.neighbour(g, "c2", "right"), null);
            compare(FocusGraph.neighbour(g, "c0", "left"), null);
        }

        function test_no_vertical_wrap() {
            const g = harness.grid([]);
            compare(FocusGraph.neighbour(g, "c1", "up"), null);
            compare(FocusGraph.neighbour(g, "c7", "down"), null);
        }

        // --- R1: vertical movement skips rows with nothing focusable -------

        function test_vertical_skips_an_empty_row() {
            // Whole middle row gone — down from the top row must land on the
            // BOTTOM row, not stop and not land on a disabled cell.
            const g = harness.grid(["c3", "c4", "c5"]);
            compare(FocusGraph.neighbour(g, "c1", "down"), "c7");
            compare(FocusGraph.neighbour(g, "c7", "up"), "c1");
        }

        function test_vertical_skips_a_partially_empty_row_by_column() {
            // Only the cell directly below is gone: the move must still land in
            // the NEAREST row, on the nearest column — row 1 is not empty.
            const g = harness.grid(["c4"]);
            compare(FocusGraph.neighbour(g, "c1", "down"), "c3");
        }

        // --- R3: a non-focusable cell is never returned --------------------

        function test_never_returns_a_disabled_cell() {
            const g = harness.grid(["c1"]);
            compare(FocusGraph.neighbour(g, "c0", "right"), "c2");
            compare(FocusGraph.neighbour(g, "c4", "up"), "c0");
            verify(["c1"].indexOf(FocusGraph.initial(g)) === -1);
            verify(["c1"].indexOf(FocusGraph.rehome(g, 0, 1)) === -1);
        }

        // --- R4: rehome always lands somewhere if anything is focusable ----

        function test_rehome_lands_next_to_the_lost_cell() {
            // The user was on c4 (row 1, col 1) and c4 was disabled. Nearest
            // survivors at row-distance 0 are c3 and c5, both at column
            // distance 1; the documented tie-break prefers the lower column.
            const g = harness.grid(["c4"]);
            compare(FocusGraph.rehome(g, 1, 1), "c3");
        }

        function test_rehome_crosses_rows_when_the_row_is_gone() {
            const g = harness.grid(["c3", "c4", "c5"]);
            const landed = FocusGraph.rehome(g, 1, 1);
            // Row 0 and row 2 are both at distance 1; the tie-break prefers the
            // lower row. What matters more than which: it is never null.
            verify(landed !== null);
            compare(landed, "c1");
        }

        function test_rehome_is_null_only_for_a_totally_empty_graph() {
            const all = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"];
            compare(FocusGraph.rehome(harness.grid(all), 1, 1), null);
            // One survivor anywhere is enough — this is R4.
            const one = all.filter(id => id !== "c8");
            compare(FocusGraph.rehome(harness.grid(one), 0, 0), "c8");
        }

        // --- structural defects the router logs ----------------------------

        function test_problems_flags_duplicate_ids() {
            const dup = [
                {
                    "id": "a",
                    "row": 0,
                    "column": 0,
                    "focusable": true
                },
                {
                    "id": "a",
                    "row": 0,
                    "column": 1,
                    "focusable": true
                }
            ];
            verify(FocusGraph.problems(dup).length > 0);
            compare(FocusGraph.problems(harness.grid([])).length, 0);
        }

        // --- the wired half ------------------------------------------------

        // Initial placement goes through the router, not around it. Asserted by
        // ASKING for it rather than by reading state left over from
        // registration: QtTest runs slots in alphabetical order, so a test that
        // depended on "nothing has moved focus yet" would turn into a false
        // green the moment another test was added above it alphabetically.
        function test_router_places_initial_focus() {
            theRouter.focusInitial();
            compare(theRouter.currentId, "s0");
        }

        function test_router_moves_through_the_real_slots() {
            theRouter.setCurrent("s0");
            verify(theRouter.move("right"));
            compare(theRouter.currentId, "s1");
            verify(theRouter.move("down"));
            compare(theRouter.currentId, "s4");
            verify(theRouter.move("left")); // s4 -> s3
            compare(theRouter.currentId, "s3");
        }

        // The reason the whole model exists: disabling the cell under focus
        // must re-home, not strand. Nothing in the test re-wires a neighbour,
        // because there is no neighbour to re-wire.
        function test_disabling_the_focused_row_rehomes_instead_of_stranding() {
            harness.middleRowEnabled = true;
            theRouter.setCurrent("s4");
            compare(theRouter.currentId, "s4");

            harness.middleRowEnabled = false;
            verify(theRouter.currentId !== "");
            verify(theRouter.currentId !== "s4");
            // And traversal now skips the empty row entirely.
            theRouter.setCurrent("s1");
            verify(theRouter.move("down"));
            compare(theRouter.currentId, "s7");

            harness.middleRowEnabled = true;
        }
    }
}

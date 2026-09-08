// The v2 focus model, as pure functions.
//
// WHAT THIS REPLACES
//
// v1's focus contract was eight duck-typed members -- previousRow, nextRow,
// firstRow, lastRow, canFocus, regionFocused, focusFirstChild,
// ensureVisibleRequested -- hand-wired across ~17 files with no compile-time
// check. Every "focus stranded" bug in that codebase was a mis-wired neighbour,
// and a mis-wire was invisible until it was on a television.
//
// THE INVERSION
//
// A component no longer declares WHO its neighbours are. It declares WHERE IT
// SITS -- a (row, column) cell and whether it is currently focusable -- and this
// module computes the neighbour from the live set of cells, on every move.
//
// Three consequences follow, and they are the whole point:
//
//   * There is no neighbour to get wrong. `up` from a cell is derived, so it
//     cannot point at a widget that was removed, disabled, or reordered.
//   * Disabling a widget cannot strand focus. Its cell simply leaves the set;
//     traversal skips whole empty rows (R1), and if the CURRENT cell is the one
//     that left, `rehome` picks the nearest survivor (R4). Neither path can
//     return a cell that is not focusable (R3).
//   * The logic is testable with no QML, no window and no compositor, which is
//     v1's best idea (`.pragma library` decision modules) applied to the part of
//     v1 that most needed it.
//
// THE RULES, stated so the tests can break them
//
//   R1  Vertical movement skips rows containing no focusable cell.
//   R2  No wrap, in any direction. Movement off the edge is a no-op (null).
//   R3  No function ever returns a cell whose `focusable` is false.
//   R4  `rehome` returns a cell whenever ANY focusable cell exists. It returns
//       null only for a genuinely empty graph -- so "focus is stranded" is
//       reachable only when there is nothing to focus.
//   R5  Neighbours are derived from coordinates alone. Nothing in a cell names
//       another cell.
//
// tests/qml/tst_focusgraph.qml asserts each of these, and docs/V2_SHELL.md
// records which mutation of this file each assertion was confirmed to catch.
.pragma library

// A cell: { id: string, row: int, column: int, focusable: bool }.

function _focusable(cells) {
    return cells.filter(function (c) {
        return !!c && c.focusable === true;
    });
}

function _byId(cells, id) {
    for (var i = 0; i < cells.length; ++i) {
        if (cells[i] && cells[i].id === id)
            return cells[i];
    }
    return null;
}

// Reading order: top-to-bottom, then left-to-right. Ties on both fall back to
// the id so the ordering is total and the result never depends on array order.
function _readingOrder(a, b) {
    if (a.row !== b.row)
        return a.row - b.row;
    if (a.column !== b.column)
        return a.column - b.column;
    return a.id < b.id ? -1 : (a.id > b.id ? 1 : 0);
}

// The first focusable cell in reading order, or null when there is none.
function initial(cells) {
    var live = _focusable(cells);
    if (live.length === 0)
        return null;
    live.sort(_readingOrder);
    return live[0].id;
}

// The cell in `row` nearest to `column`. Ties prefer the lower column, so a move
// down a column boundary lands leftward rather than unpredictably.
function _nearestInRow(live, row, column) {
    var best = null;
    var bestDist = Infinity;
    for (var i = 0; i < live.length; ++i) {
        var c = live[i];
        if (c.row !== row)
            continue;
        var dist = Math.abs(c.column - column);
        if (dist < bestDist || (dist === bestDist && best !== null && c.column < best.column)) {
            best = c;
            bestDist = dist;
        }
    }
    return best;
}

// The next id in `direction` from `fromId`, or null when the move leaves the
// graph (R2: no wrap). `direction` is "left" | "right" | "up" | "down".
function neighbour(cells, fromId, direction) {
    var live = _focusable(cells);
    // `from` is looked up in the FULL set: the current cell may itself have just
    // become unfocusable, and moving off it must still work rather than trapping
    // the user. It is never returned, only used for its coordinates.
    var from = _byId(cells, fromId);
    if (!from || live.length === 0)
        return null;

    if (direction === "left" || direction === "right") {
        var wantLeft = direction === "left";
        var best = null;
        for (var i = 0; i < live.length; ++i) {
            var c = live[i];
            if (c.row !== from.row || c.id === fromId)
                continue;
            if (wantLeft ? c.column >= from.column : c.column <= from.column)
                continue;
            // Nearest in the direction of travel; a column collision (a
            // malformed graph) breaks on the id so the result never depends on
            // the order the cells happened to register in.
            if (best === null
                    || (wantLeft ? c.column > best.column : c.column < best.column)
                    || (c.column === best.column && c.id < best.id))
                best = c;
        }
        return best ? best.id : null;
    }

    if (direction === "up" || direction === "down") {
        var step = direction === "down" ? 1 : -1;
        // Collect the candidate rows on that side, nearest first. Rows with no
        // focusable cell never enter this list, which IS R1: an entire disabled
        // widget is skipped without anybody rewiring anything.
        var rows = [];
        for (var j = 0; j < live.length; ++j) {
            var r = live[j].row;
            var beyond = step > 0 ? r > from.row : r < from.row;
            if (beyond && rows.indexOf(r) === -1)
                rows.push(r);
        }
        if (rows.length === 0)
            return null;
        rows.sort(function (a, b) {
            return step > 0 ? a - b : b - a;
        });
        var landed = _nearestInRow(live, rows[0], from.column);
        return landed ? landed.id : null;
    }

    return null;
}

// The nearest focusable cell to a (row, column) that is no longer focusable --
// the cell the user was on when a widget was disabled, hidden, or destroyed.
//
// Distance is lexicographic: row distance first, then column distance. Ties
// prefer the LOWER row and then the LOWER column, so the landing is deterministic
// and a test can assert it. Returns null only when the graph has no focusable
// cell at all (R4).
function rehome(cells, row, column) {
    var live = _focusable(cells);
    if (live.length === 0)
        return null;
    var best = null;
    var bestRow = Infinity;
    var bestCol = Infinity;
    for (var i = 0; i < live.length; ++i) {
        var c = live[i];
        var dr = Math.abs(c.row - row);
        var dc = Math.abs(c.column - column);
        var better = dr < bestRow
                || (dr === bestRow && dc < bestCol)
                || (dr === bestRow && dc === bestCol && best !== null
                    && (c.row < best.row || (c.row === best.row && c.column < best.column)));
        if (better) {
            best = c;
            bestRow = dr;
            bestCol = dc;
        }
    }
    return best.id;
}

// Structural problems a caller should never ship: a duplicate id makes
// `neighbour` ambiguous, and a missing id makes a cell unreachable. Returns an
// array of human-readable strings, empty when the graph is well-formed. The
// router logs these once at startup -- the compile-time check QML cannot give us,
// moved to the earliest runtime moment that can have one.
function problems(cells) {
    var out = [];
    var seen = {};
    for (var i = 0; i < cells.length; ++i) {
        var c = cells[i];
        if (!c || typeof c.id !== "string" || c.id.length === 0) {
            out.push("cell " + i + " has no id");
            continue;
        }
        if (seen[c.id])
            out.push("duplicate id '" + c.id + "'");
        seen[c.id] = true;
        if (typeof c.row !== "number" || typeof c.column !== "number")
            out.push("cell '" + c.id + "' has a non-numeric row/column");
    }
    return out;
}

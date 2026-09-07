# shell-v2 — the tv-shell v2 shell

A spike. It is built beside v1's `shell/`, wired into no session, and it exists to
prove three things under gamescope: **it maps, it self-tags its X11 atoms before
it maps, and it takes controller focus.**

Every pixel is placeholder. The structure is not — this tree sets the patterns
every later screen follows, so the parts worth reviewing are the shapes.

Full rationale, the two repo rules it reverses, the mutation record, and (at
equal length) what it does **not** prove: [`../docs/V2_SHELL.md`](../docs/V2_SHELL.md).
The compositor contract it implements: [`../docs/V2_DESIGN.md`](../docs/V2_DESIGN.md) §5, §7, §13 Q1.

## Layout

```
CMakeLists.txt          The build step v1 deliberately lacks. ONE QML module, URI TvShell.
src/
  surfacetags.{h,cpp}   PURE role -> STEAM_* atoms. No Qt GUI, no X in its link line.
  x11tagger.{h,cpp}     The ONLY place the shell speaks X (mirrors core/src/atoms.rs).
  surface.{h,cpp}       Surface: a toplevel that declares its ROLE and tags before map.
  main.cpp              Entry point; warns loudly on a non-xcb platform plugin.
qml/TvShell/            The single QML module — no qmldir, no relative-dir imports.
  Main.qml              Placeholder screen: three SEPARATE toplevels + a D-pad grid.
  FocusRouter.qml       Owns currentId; every decision delegates to focusGraph.js.
  FocusSlot.qml         One cell: declares WHERE it sits, never who its neighbours are.
  focusGraph.js         Pure .pragma library: neighbour / rehome / initial / problems.
tests/                  Three lanes — see below.
```

## Three things to know before editing

1. **`Surface` tags before map, and Qt gives no virtual hook to do it.**
   `QWindow::setVisible` is not virtual in Qt 6 and neither is `create()`. The
   ordering is enforced by redeclaring the `visible` property, hiding the base
   `setVisible`, **deleting** `show()`/`showNormal()`/`showFullScreen()`/
   `showMaximized()`, and deferring visibility to `componentComplete()`. If you
   find yourself adding a way to show a window, you are about to map an untagged
   one. Read `src/surface.h` first.

2. **Nothing sets an atom directly.** A caller sets `role`; the role decides the
   atoms. That is what makes an overlay drawn inside the base window
   unrepresentable rather than merely discouraged.

3. **Focus is computed, never wired.** A `FocusSlot` declares `row`, `column` and
   `slotEnabled`. It does not name a neighbour, and neither does anything else.
   Disabling a slot cannot strand focus: traversal skips empty rows and the router
   re-homes off a cell that stops being focusable. Put new decision logic in
   `focusGraph.js` (pure, headlessly tested), not in a binding.

## Build and test

```bash
cmake -S shell-v2 -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug
cmake --build build
cmake --build build --target all_qmllint
ctest --test-dir build --output-on-failure
```

| Lane | Needs | Asserts |
|---|---|---|
| `surfacetags` | nothing | the role → atoms mapping, including the two negative rules |
| `qml` | nothing (offscreen) | `focusGraph.js` directly, plus a real router over real slots |
| `premap` | a real X server | `PropertyNotify` before `MapNotify`, per role |

`premap` is opt-in behind `TV_SHELL_TEST_XVFB`, read at **configure** time:

```bash
Xvfb :99 -screen 0 1280x800x24 &
TV_SHELL_TEST_XVFB=:99 cmake -S shell-v2 -B build -G Ninja
cmake --build build && ctest --test-dir build --output-on-failure
```

Without it `ctest` reports two lanes; with it, three. CI sets it.

## Running it

```bash
QT_QPA_PLATFORM=xcb ./build/tv-shell-v2
```

Arrows move focus, **space** toggles the middle cell (watch focus re-home rather
than strand), **menu** opens the overlay toplevel, **tab** shows the toast. The
header line reports the current cell id and whether the base window was tagged.

On a Wayland platform plugin it starts, warns, and maps untagged — under gamescope
that means it is never a focus candidate, which looks like a black screen. The
warning is the diagnosis.

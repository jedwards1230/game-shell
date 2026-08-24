# QML unit tests (QtQuickTest, headless)

Fast, deterministic layout + navigation tests for the shell's QML components.
They run with the stock Qt `qmltestrunner` under `QT_QPA_PLATFORM=offscreen`:
**no Quickshell, no compositor, no GPU.** They answer two questions the
controller-first design depends on:

1. **Is every interactive element reachable by D-pad** and does it clamp at the
   ends? (`Left`/`Right`/`Up`/`Down`/`Escape`)
2. **Does activation (`A`/Return) fire the right behaviour** on each focus stop?

This is layer 2 of the testing strategy (the daemon's Rust unit/integration
tests are layer 1; full-shell pixel screenshots through a headless Wayland
compositor would be layer 3 — not done here).

## Running

```bash
./tests/qml/run.sh
```

Requires Qt's `qmltestrunner` (ships with `qtdeclarative`) on `PATH`. Override
the binary with `QMLTESTRUNNER=/path/to/qmltestrunner`. CI runs this in
`.github/workflows/qml-test.yml` on any change under `shell/**/*.qml` or
`tests/qml/**`.

## How it works — the shim layer

Production components reference singletons (`Theme`, `Units`, `SettingsStore`,
`InputMode`, `IconTheme`, …) that import Quickshell modules (`Quickshell`,
`Quickshell.Io`). `qmltestrunner` has no Quickshell runtime, so it can't load
that import graph directly.

`run.sh` sidesteps this by **assembling a throwaway `components` module** in
`tests/qml/.build/` (gitignored) from two sources:

- **Stub singletons** (`tests/qml/stubs/*.qml`) — plain-QtQuick re-implementations
  of *only* the singleton surface the tested components touch. This is the
  intentional shim: edit these when a tested component starts using a new
  `Theme`/`Units`/etc. property.
- **The real components under test** (e.g. `shell/components/QuickActions.qml`),
  copied verbatim — so the tests exercise production code with **zero drift**.

The "zero drift" guarantee applies only to the **tested components** (copied
verbatim). The **stub singletons are hand-maintained shims** — they reproduce a
subset of the real singleton API and can fall behind it. Keep them in sync as
the tested component set grows (a missing property surfaces as a binding error
at test time, not a silent pass).

The assembled dir declares `module components` (and `components.lib`), so the
real files' bare singleton references and `import "lib"` resolve exactly as they
do in the shell — just against stubs.

## Adding a component to the suite

1. Confirm it's Quickshell-free (`import QtQuick` / `import "lib"` only). If it
   imports `Quickshell*` directly it needs decoupling first.
2. In `run.sh`, copy the real file (and any real leaf components it uses) into
   the assembled module.
3. Add it to `tests/qml/stubs/qmldir` (or `stubs/lib/qmldir`).
4. Add any singleton properties it newly relies on to the matching stub.
5. Write `tst_<component>.qml` and run `./tests/qml/run.sh`.

The copy list in `run.sh` is a manual checklist of each tested component's real
dependencies. **If a test fails with a type/import-not-found error, a real
dependency is probably missing from that list** (or its qmldir entry / stub
property is absent).

Currently covered: `QuickActions` (+ real `QuickActionButton`, `CountBadge`),
`WidgetHost` (against the stub `WidgetRegistry` + stub widgets), `PopoverMenu`
(`tst_popovermenu.qml` — the disabled-item contract), the widget config
migrator `widgetConfig.js` (`tst_widgetmigrate.qml`), the two log-noise negative
memos `iconMemo.js` / `artMemo.js` (`tst_iconmemo.qml` — which also drives the
REAL `AppIcon`, since offscreen has no `image://icon` provider so every request
errors — and `tst_artmemo.qml`), the screen-scale filter `screenScale.js`
(`tst_screenscale.qml`), `SteamLibraryView`'s poll-adoption signature guard
(`tst_steamlibrary.qml`, against the real view + the real `SteamCard`), and
`AppsWidget`'s segment derivation + hint-bar contract (`tst_appssegment.qml`, the
two `_activeModel` binding-loop regressions).

**The run also fails on any QML binding loop.** `run.sh` scans the combined
output for `Binding loop detected` and exits non-zero if it appears, for every
test at once. This is not decoration: Qt *abandons* a re-entered binding update,
so the symptom is a stale property value that usually still looks plausible —
asserting settled values cannot catch the class. QML `TestCase` has no
`failOnWarning` (that is a C++-only `QTest` API), hence the harness-level gate.

The migrator test imports the **real** `widgetConfig.js` by source path
(`../../shell/widgets/lib/widgetConfig.js`, a pure `.pragma library` module) and
the **real** `WidgetManifests` singleton (copied into the assembled `components.lib`
module by `run.sh`, since it is pure-data with no Quickshell imports). That keeps
the migration invariants (fresh-install defaults, legacy flat-key preservation,
idempotency) pinned to production code with zero drift.

## Widget-contract conformance (`tst_widgetcontract`)

`tst_widgetcontract.qml` loads the **four REAL home widgets** — `AppsWidget`,
`PlexWidget`, `MoonlightWidget`, `NowPlayingWidget` — and asserts each honours the
duck-typed home-tile contract (`focusFirstChild()`/`canFocus`/`wantVisible` +
no-focus-trap Up/Down hand-off + the optional `ensureVisibleRequested` signal).
Assertions are **behavioral and refactor-agnostic**: they check behaviour (focus
leaves the widget onto its wired neighbour; the signal fires *iff* the widget
exposes it), never "function X exists on the base".

To make the real widgets' own relative imports resolve, `run.sh` mirrors the
widget subtree into `.build/widgets/{lib,apps,plex,moonlight,nowplaying}` so
`../lib` → the real `Widget`/`SegmentedHeader`/`FilterChips`, `../../components`
→ the SAME flat stub module the other tests use, and `../../components/lib` →
`ServiceMonitor`/`ServiceStatusNotice`/`MprisPlayerBase` (all real). The widget
ROOTS + their focus/segment framework + `SteamLibraryView` are copied **verbatim**
(the contract under test has zero drift). Only two classes are stubbed
(`tests/qml/widgetstubs/`):

- **Quickshell-backed clients** — `SocketClient` / `AppDiscoveryManager` become
  inert no-ops, and a minimal `Quickshell.Services.Mpris` `Mpris` singleton is
  put on a second import path (`-import .build/qml`) so `MprisPlayerBase` loads.
- **Pure-visual leaf cards** — `AppCard` / `StreamCard` / `PlexCard` / `SteamCard`
  / `WakeCard` / `SessionIndicator` / `NowPlayingCard` / `NowPlayingStripView`
  (the `QtQuick.Effects`/`image://icon` renderers) become minimal stubs exposing
  only the delegate/child surface the real widgets assign. They never render in
  the contract test, so a stub is faithful enough for load.

`PlexWidget`'s visibility is gated on the live `ServiceMonitor` health bus, which
can't be faked headlessly — its row asserts load + contract shape + the
*structural* no-trap guarantee (a non-focusable widget is skipped by the chain)
rather than a synthetic key hand-off. The other three inject minimal content
(a recent-model row / a Moonlight target / a fake MPRIS player) to exercise the
full visible→focusable→hand-off path.

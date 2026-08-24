#!/usr/bin/env bash
# Headless QML unit tests (QtQuickTest) for the shell's components.
#
# Quickshell-free components can be exercised with the standard Qt qmltestrunner
# under QT_QPA_PLATFORM=offscreen — no compositor, no GPU. The trick is the
# import graph: production components reference singletons (Theme, Units, …) that
# pull in Quickshell, which qmltestrunner can't load. So we assemble a throwaway
# `components` module from:
#   - hand-written STUB singletons (tests/qml/stubs)  — the shim layer
#   - the REAL components under test, copied verbatim  — zero drift
# and point qmltestrunner's import path at it. See tests/qml/README.md.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
build="$here/.build"
shellc="$repo/shell/components"
shellw="$repo/shell/widgets"

runner="${QMLTESTRUNNER:-qmltestrunner}"
if ! command -v "$runner" >/dev/null 2>&1; then
    echo "error: '$runner' not found on PATH (install Qt qtdeclarative tools, or set QMLTESTRUNNER)" >&2
    exit 127
fi

# 1. Assemble the stub `components` module (singleton shims + qmldirs).
rm -rf "$build"
mkdir -p "$build/components/lib"
cp "$here/stubs/"*.qml "$build/components/"
cp "$here/stubs/qmldir" "$build/components/qmldir"
cp "$here/stubs/lib/qmldir" "$build/components/lib/qmldir"

# 2. Overlay the REAL components under test (copied, not symlinked — avoids
#    Qt resolving relative imports through the symlink back into shell/).
cp "$shellc/QuickActions.qml" "$build/components/"
cp "$shellc/QuickActionButton.qml" "$build/components/"
# NavigableGrid is pure QtQuick (references Theme/InputMode only) — runs headless
# against the stub singletons; tst_navigablegrid exercises its key-nav + handoff.
cp "$shellc/NavigableGrid.qml" "$build/components/"
# IdleInhibitController is pure QtQuick + Quickshell.Services.Mpris (the stub Mpris
# loads via the -import "$build/qml" path) — headless-testable; tst_idleinhibit-
# controller exercises its streaming/appRunning truth table.
cp "$shellc/IdleInhibitController.qml" "$build/components/"
# Drawer is pure QtQuick and touches only Theme.surface, so it loads headless.
# tst_drawer pins the clip contract that keeps an overflowing child (the Resume
# rail's third tile) from painting outside a closed drawer.
cp "$shellc/Drawer.qml" "$build/components/"
# PopoverMenu is pure QtQuick over Theme/Units only — headless-testable;
# tst_popovermenu pins its disabled-item contract (an unavailable action stays
# rendered + inert instead of vanishing from the menu).
cp "$shellc/PopoverMenu.qml" "$build/components/"
cat >>"$build/components/qmldir" <<'EOF'
IdleInhibitController 1.0 IdleInhibitController.qml
Drawer 1.0 Drawer.qml
PopoverMenu 1.0 PopoverMenu.qml
EOF
cp "$shellc/lib/CountBadge.qml" "$build/components/lib/"
# AppIcon + its negative icon memo. AppIcon is pure QtQuick over Theme/Units and
# an `image://icon/` request that offscreen has no provider for — so every request
# ERRORS, which is exactly what tst_iconmemo needs to pin the "only memoise a
# failure seen at a valid size" invariant against production code.
cp "$shellc/lib/AppIcon.qml" "$build/components/lib/"
cp "$shellc/lib/iconMemo.js" "$build/components/lib/"
cat >>"$build/components/lib/qmldir" <<'EOF'
AppIcon 1.0 AppIcon.qml
EOF
# focusChain.js — the shared vertical-traversal helper imported by Widget.qml and
# NavigableGrid via a relative path; copy it in so those imports resolve headless.
cp "$shellc/lib/focusChain.js" "$build/components/lib/"
# Widget base + WidgetHost are pure QtQuick (no Quickshell), so they run headless
# against the stub WidgetRegistry + Stub*Widget shims under stubs/lib. They now
# live in shell/widgets/lib/ but are copied into the test's assembled
# `components.lib` module (the test module name is a build-time fiction).
cp "$shellw/lib/Widget.qml" "$build/components/lib/"
cp "$shellw/lib/WidgetHost.qml" "$build/components/lib/"
# WidgetManifests is a pure-data singleton (no Quickshell) — the real one runs
# headless so tst_widgetmigrate exercises the production manifest defaults.
cp "$shellw/lib/WidgetManifests.qml" "$build/components/lib/"
cp "$here/stubs/lib/"*.qml "$build/components/lib/"

# 3. Widget-contract harness (tst_widgetcontract): mirror the REAL home-widget
#    subtree into .build so each widget's own relative imports resolve naturally
#    (`../lib` → widgets/lib, `../../components` → the SAME flat stub module the
#    existing tests use, `../../components/lib` → components.lib). The five widget
#    ROOT files + their focus/segment framework are the REAL production files
#    (zero drift for the contract under test); only Quickshell-backed clients and
#    the pure-visual leaf CARDS are stubbed (widgetstubs/). See tests/qml/README.md.
wstub="$here/widgetstubs"

# 3a. Extra flat `components` leaves the widgets pull in. NavigableRow is the one
#     REAL pure-QtQuick leaf; the rest are inert stubs (SocketClient/AppDiscovery
#     are Quickshell.Io-backed; the cards are QtQuick.Effects visuals that never
#     render in the contract test — see widgetstubs/components/*.qml).
cp "$shellc/NavigableRow.qml" "$build/components/"
# MarqueeText is pure QtQuick over Theme.reduceMotion — the REAL one, needed by the
# real SteamCard's caption row (see the steamlib block below).
cp "$shellc/MarqueeText.qml" "$build/components/"
cp "$wstub/components/"*.qml "$build/components/"
cat >>"$build/components/qmldir" <<'EOF'
NavigableRow 1.0 NavigableRow.qml
MarqueeText 1.0 MarqueeText.qml
SocketClient 1.0 SocketClient.qml
singleton AppDiscoveryManager 1.0 AppDiscoveryManager.qml
AppCard 1.0 AppCard.qml
StreamCard 1.0 StreamCard.qml
WakeCard 1.0 WakeCard.qml
SessionIndicator 1.0 SessionIndicator.qml
NowPlayingCard 1.0 NowPlayingCard.qml
FocusFrame 1.0 FocusFrame.qml
EOF

# 3b. REAL components.lib types the widgets instantiate (ServiceMonitor drives the
#     health bus off the stub SocketClient; MprisPlayerBase reads the stub Mpris).
cp "$shellc/lib/ServiceMonitor.qml" "$build/components/lib/"
cp "$shellc/lib/ServiceStatusNotice.qml" "$build/components/lib/"
cp "$shellc/lib/MprisPlayerBase.qml" "$build/components/lib/"
cat >>"$build/components/lib/qmldir" <<'EOF'
ServiceMonitor 1.0 ServiceMonitor.qml
ServiceStatusNotice 1.0 ServiceStatusNotice.qml
MprisPlayerBase 1.0 MprisPlayerBase.qml
EOF

# 3c. Widget framework (widgets/lib) — REAL Widget base + shared segment header.
mkdir -p "$build/widgets/lib"
cp "$shellw/lib/Widget.qml" "$build/widgets/lib/"
cp "$shellw/lib/FilterChips.qml" "$build/widgets/lib/"
cp "$shellw/lib/SegmentedHeader.qml" "$build/widgets/lib/"
cp "$wstub/widgets/lib/qmldir" "$build/widgets/lib/qmldir"

# 3d. The five REAL home widgets + their same-dir leaves (real view / stub cards).
mkdir -p "$build/widgets/apps"
cp "$shellw/apps/AppsWidget.qml" "$build/widgets/apps/"
cp "$wstub/widgets/apps/qmldir" "$build/widgets/apps/qmldir"

mkdir -p "$build/widgets/plex"
cp "$shellw/plex/PlexWidget.qml" "$build/widgets/plex/"
cp "$wstub/widgets/plex/PlexCard.qml" "$build/widgets/plex/"
cp "$wstub/widgets/plex/qmldir" "$build/widgets/plex/qmldir"

mkdir -p "$build/widgets/moonlight"
cp "$shellw/moonlight/MoonlightWidget.qml" "$build/widgets/moonlight/"
cp "$wstub/widgets/moonlight/qmldir" "$build/widgets/moonlight/qmldir"

# steamlib: REAL SteamLibraryView AND REAL SteamCard (shared by moonlight +
# steam). MoonlightWidget now imports "../steamlib", so this module must exist for
# it to load headless.
#
# SteamCard used to be stubbed here as "a QtQuick.Effects visual that never
# renders in the contract test". It is now the real file, because it grew a
# `.pragma library` import (artMemo.js) and a memo-driven `_artIdx` binding — the
# exact parse-passes-but-fails-to-load class this suite exists to catch. Stubbing
# it meant the real file was never instantiated headless at all. Its MultiEffect /
# MarqueeText / FocusFrame dependencies resolve against the stub singletons the
# same way the other real leaves do.
mkdir -p "$build/widgets/steamlib"
cp "$shellw/steamlib/SteamLibraryView.qml" "$build/widgets/steamlib/"
cp "$shellw/steamlib/SteamCard.qml" "$build/widgets/steamlib/"
cp "$wstub/widgets/steamlib/qmldir" "$build/widgets/steamlib/qmldir"
# SteamCard's negative art memo (a `.pragma library` it imports by relative path).
cp "$shellc/lib/artMemo.js" "$build/components/lib/"

mkdir -p "$build/widgets/nowplaying"
cp "$shellw/nowplaying/NowPlayingWidget.qml" "$build/widgets/nowplaying/"
cp "$wstub/widgets/nowplaying/NowPlayingStripView.qml" "$build/widgets/nowplaying/"
cp "$wstub/widgets/nowplaying/qmldir" "$build/widgets/nowplaying/qmldir"

# steam: the REAL SteamWidget (hosts the shared SteamLibraryView above; no
# Quickshell.Io deps of its own). Covers the cross-PR drift that bricked the
# shell — a leaf re-declaring a base signal is now a headless load failure.
mkdir -p "$build/widgets/steam"
cp "$shellw/steam/SteamWidget.qml" "$build/widgets/steam/"
cp "$wstub/widgets/steam/qmldir" "$build/widgets/steam/qmldir"

# 3e. Stub Quickshell modules on a second import path so real leaves that
#     `import Quickshell.Services.Mpris` load headless (no Quickshell runtime).
mkdir -p "$build/qml"
cp -R "$wstub/qml/Quickshell" "$build/qml/Quickshell"

# 4. Run every tst_*.qml in tests/qml headless.
#
# The run is ALSO gated on the absence of QML binding loops. qmltestrunner does
# not fail a test on an unexpected warning (QML TestCase has no `failOnWarning` —
# that is a C++-only QTest API), and a binding loop is not a cosmetic warning: Qt
# ABANDONS a re-entered binding update, leaving the property stale at a value that
# usually still looks plausible. Asserting settled values therefore cannot catch
# the whole class. Scanning the combined output does, for every test at once.
echo "Running QML tests with: $runner (offscreen)"
log="$build/run.log"
set +e
QT_QPA_PLATFORM=offscreen "$runner" -import "$build" -import "$build/qml" -input "$here" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
set -e

if grep -q "Binding loop detected" "$log"; then
    echo >&2
    echo "error: QML binding loop(s) reported during the test run:" >&2
    grep "Binding loop detected" "$log" | sort -u >&2
    exit 1
fi

exit "$status"

// Offscreen QML test runner for the TvShell module.
//
// Unlike v1's tests/qml/run.sh — which assembles a throwaway `components` module
// out of copied production files and hand-written stub singletons — this binary
// LINKS the real module. There is no copy step and therefore no drift: the
// FocusRouter under test is the FocusRouter that ships.
#include <QtQuickTest>

QUICK_TEST_MAIN(tvshell_qml)

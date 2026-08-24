.pragma library

// Session-lifetime NEGATIVE memo for Freedesktop icon names that are not in the
// icon theme.
//
// Why it exists: `image://icon/<name>` is asked for a name once per delegate, and
// the shell recreates delegates constantly (ListView model reassignment, screen
// settle passes, widget show/hide). A name the theme simply does not have was
// therefore re-requested hundreds of times per session — the bulk of the
// `WARN: Could not load icon "<name>" at size QSize(W,H) from request` flood in
// the shell log. Freedesktop icon lookup is NAME-based, so a name missing from
// the theme fails at every size and every DPR; keying the memo by name alone is
// correct and complete.
//
// Once a name is recorded here, AppIcon never asks the provider for it again and
// drops straight to its existing letter-initial fallback — the same pixels the
// user already saw, minus the request and the warning.
//
// INVARIANT — only record a failure observed at a TRUSTWORTHY size.
// Two things make an icon fail regardless of whether the theme has it: a
// degenerate size (0, or the 1px×DPR=2px clamp the provider applies to
// `Qt.size(0, 0)`), and a size derived from a screen scale that has not settled
// yet — on the target device the pre-settle placeholder is double the real scale.
// Recording either would blank a working icon for the rest of the session.
//
// This module cannot see the size, so it deliberately does not guess: the caller
// owns the gate. AppIcon enforces it structurally rather than by convention — its
// `_sizeValid` gates the `source` binding itself (`Units.screenReady &&
// iconSize >= 8`), so an untrustworthy request is never ISSUED and therefore can
// never produce a failure to record. tst_iconmemo pins both halves of that gate
// through the real component.
//
// Pure `.pragma library` (one shared instance per QML engine, no imports, no QML
// types) so it is headless-testable — see tests/qml/tst_iconmemo.qml.

var _missing = {};

// Record `name` as known-absent from the icon theme. No-op for an empty/invalid
// name so a caller never has to pre-check.
function markMissing(name) {
    if (typeof name !== "string" || name === "")
        return;
    _missing[name] = true;
}

// True when `name` has already been observed failing at a valid size.
function isMissing(name) {
    if (typeof name !== "string" || name === "")
        return false;
    return _missing[name] === true;
}

// Drop every recorded name. Exists for tests; the shell never calls it (the memo
// is intentionally session-lifetime — an icon theme does not change under a
// running kiosk shell).
function reset() {
    _missing = {};
}

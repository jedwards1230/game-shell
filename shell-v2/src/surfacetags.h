// Pure role -> X11 property mapping. NO X11 HEADERS HERE, deliberately.
//
// This is the v2 shell's equivalent of v1's `.pragma library` JS: the decision
// is a pure function of (role, appId), so it is unit-testable with no display
// server, no Qt GUI, and no compositor. `x11tagger` is the only translation
// unit that speaks X, exactly as `core/src/atoms.rs` is the only place the core
// crate does.
//
// The contract encoded here is docs/V2_DESIGN.md §5, "Shell windows self-tag":
//
//   Base     STEAM_GAME=<appid>                      the shell's own base window
//   Overlay  STEAM_OVERLAY=1, STEAM_INPUT_FOCUS=1    drawer / QAM over a running app
//   Toast    STEAM_OVERLAY=1, STEAM_NOTIFICATION=1   notifications, never take input
//
// Two things about that table are load-bearing and easy to "tidy" wrongly:
//
//   * Overlays carry NO STEAM_GAME. Under `--steam` gamescope selects an overlay
//     by the overlay atom, not by app id; tagging an overlay with the shell's id
//     would make it a base-layer candidate, which is precisely the bug the
//     separate-toplevel design exists to prevent. `dev/gamescope/launch.sh`
//     (the measured prototype) tags its overlay the same way -- overlay atoms
//     only.
//   * A Toast gets NO STEAM_INPUT_FOCUS. A notification that takes keyboard
//     focus is a notification that steals the pad mid-game.
//
// UNVERIFIED ON HARDWARE: the Toast pair (STEAM_OVERLAY + STEAM_NOTIFICATION
// together) is this file's reading of §5, not a measured result -- the prototype
// exercised the Overlay pair only. See docs/V2_SHELL.md, "What the spike does
// not prove".
#pragma once

#include <cstdint>
#include <vector>

namespace tvshell {

// The role a toplevel plays in the shell. This is the ONLY input that decides
// how a window is tagged, which is what makes "an overlay drawn inside the base
// window" unrepresentable rather than merely discouraged.
enum class SurfaceRole {
    Base = 0,
    Overlay = 1,
    Toast = 2,
};

// One X11 CARDINAL property. `name` is an atom NAME to be interned later, not an
// xcb_atom_t: keeping it a string is what lets this layer stay X-free.
struct SurfaceTag {
    const char *name;
    std::uint32_t value;
};

// The full set of properties for a role. Order is stable so tests can assert on
// it and so the wire order is reproducible.
std::vector<SurfaceTag> tagsForRole(SurfaceRole role, std::uint32_t appId);

// True when the role participates in the base-layer list at all. Only a Base
// surface does; used to reject an appId set on a role that has no use for one.
bool roleCarriesAppId(SurfaceRole role);

} // namespace tvshell

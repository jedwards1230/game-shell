// The ONLY translation unit in the shell that speaks X.
//
// Mirrors `core/src/atoms.rs`'s rule for the same reason: X access spread across
// a UI tree is how v1's compositor boundary rotted. Everything above this file
// deals in `SurfaceRole`; everything below it deals in xcb.
#pragma once

#include "surfacetags.h"

#include <QtGlobal>

#include <cstdint>
#include <vector>

namespace tvshell {

// Writes `tags` onto window `xid` as 32-bit CARDINAL properties, using the
// xcb connection Qt's xcb platform plugin already owns.
//
// Sharing Qt's connection is not an optimisation, it is the correctness
// requirement: X processes one connection's requests in order, so a property
// written here is guaranteed to reach the server BEFORE a map request Qt issues
// afterwards on that same connection. On a second connection there would be no
// such ordering and "tagged before map" would be a race.
//
// Returns false (with a warning logged) when there is no X connection -- an
// offscreen/software or Wayland platform plugin. It never asserts: the same
// binary must stay runnable under QT_QPA_PLATFORM=offscreen for headless QML
// tests.
bool applyTags(std::uint32_t xid, const std::vector<SurfaceTag> &tags);

// True when running on a platform plugin that can actually tag. Callers use it
// to log once at startup rather than per window.
bool x11Available();

} // namespace tvshell

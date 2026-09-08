#include "surfacetags.h"

namespace tvshell {

bool roleCarriesAppId(SurfaceRole role)
{
    return role == SurfaceRole::Base;
}

std::vector<SurfaceTag> tagsForRole(SurfaceRole role, std::uint32_t appId)
{
    switch (role) {
    case SurfaceRole::Base:
        // The base window is the shell as an app: gamescope resolves it to an
        // app id and can put it on the base layer. Nothing else about it is
        // special -- the shell holds no privileged surface type (V2_DESIGN §4).
        return {{"STEAM_GAME", appId}};

    case SurfaceRole::Overlay:
        // Takes keyboard and mouse WITHOUT changing the base layer, so the app
        // underneath keeps the screen. No STEAM_GAME: see surfacetags.h.
        return {{"STEAM_OVERLAY", 1}, {"STEAM_INPUT_FOCUS", 1}};

    case SurfaceRole::Toast:
        // Composited over whatever is on screen and deliberately input-inert.
        return {{"STEAM_OVERLAY", 1}, {"STEAM_NOTIFICATION", 1}};
    }

    // Unreachable for the enum's own values. Returning empty rather than
    // defaulting to Base is the fail-closed choice: an untagged window is
    // visible and diagnosable, whereas a wrongly-Base-tagged one silently
    // becomes a base-layer candidate and takes the screen from a running game.
    return {};
}

} // namespace tvshell

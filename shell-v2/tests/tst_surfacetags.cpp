// The pure role -> atoms mapping. No display, no Qt GUI, no X.
//
// Every assertion here pins a sentence from docs/V2_DESIGN.md §5, so a future
// "simplification" of surfacetags.cpp has to argue with the design doc rather
// than with a green suite.
#include "surfacetags.h"

#include <QtTest>

using namespace tvshell;

// Comparison support for the set assertions below. Must live in namespace
// tvshell: std::vector's operator== finds the element comparison by ADL.
namespace tvshell {
inline bool operator==(const SurfaceTag &a, const SurfaceTag &b)
{
    return qstrcmp(a.name, b.name) == 0 && a.value == b.value;
}
} // namespace tvshell

namespace {

// Convenience: does the tag list contain name=value, exactly once?
bool has(const std::vector<SurfaceTag> &tags, const char *name, std::uint32_t value)
{
    int found = 0;
    for (const auto &t : tags) {
        if (qstrcmp(t.name, name) == 0) {
            ++found;
            if (t.value != value)
                return false;
        }
    }
    return found == 1;
}

bool mentions(const std::vector<SurfaceTag> &tags, const char *name)
{
    for (const auto &t : tags) {
        if (qstrcmp(t.name, name) == 0)
            return true;
    }
    return false;
}

} // namespace

class TestSurfaceTags : public QObject
{
    Q_OBJECT

private slots:
    // §5: "the shell process sets STEAM_GAME on its base window".
    void baseCarriesTheAppId()
    {
        const auto tags = tagsForRole(SurfaceRole::Base, 9001);
        QVERIFY(has(tags, "STEAM_GAME", 9001));
        QCOMPARE(tags.size(), size_t(1));
    }

    // §5: an overlay "takes keyboard and mouse without changing the base layer".
    void overlayTakesInputWithoutTheBaseLayer()
    {
        const auto tags = tagsForRole(SurfaceRole::Overlay, 9001);
        QVERIFY(has(tags, "STEAM_OVERLAY", 1));
        QVERIFY(has(tags, "STEAM_INPUT_FOCUS", 1));
    }

    // The rule most likely to be "tidied" wrongly: an overlay tagged with the
    // shell's app id becomes a base-layer candidate and can take the screen from
    // a running game. The prototype (dev/gamescope/launch.sh) tags overlays with
    // the overlay atoms alone, and so do we.
    void overlayNeverCarriesAnAppId()
    {
        QVERIFY(!mentions(tagsForRole(SurfaceRole::Overlay, 9001), "STEAM_GAME"));
        QVERIFY(!roleCarriesAppId(SurfaceRole::Overlay));
    }

    // A notification that takes keyboard focus is a notification that steals the
    // pad mid-game.
    void toastIsInputInert()
    {
        const auto tags = tagsForRole(SurfaceRole::Toast, 9001);
        QVERIFY(has(tags, "STEAM_NOTIFICATION", 1));
        QVERIFY(has(tags, "STEAM_OVERLAY", 1));
        QVERIFY(!mentions(tags, "STEAM_INPUT_FOCUS"));
        QVERIFY(!mentions(tags, "STEAM_GAME"));
    }

    // The three roles must not collapse into each other. Asserted as a set
    // comparison rather than per-role so adding a fourth role that duplicates an
    // existing one is caught here.
    void rolesAreDistinct()
    {
        const auto base = tagsForRole(SurfaceRole::Base, 9001);
        const auto overlay = tagsForRole(SurfaceRole::Overlay, 9001);
        const auto toast = tagsForRole(SurfaceRole::Toast, 9001);
        QVERIFY(base != overlay);
        QVERIFY(overlay != toast);
        QVERIFY(base != toast);
    }

    // The app id is data, not a constant baked into the mapping.
    void appIdFlowsThrough()
    {
        QVERIFY(has(tagsForRole(SurfaceRole::Base, 4242), "STEAM_GAME", 4242));
    }

    // Nothing here may ever emit 769: under `--steam` that is the Steam client's
    // own id and carries gamescope's window_is_steam behaviour (§5).
    void neverClaimsTheSteamClientId()
    {
        for (auto role : {SurfaceRole::Base, SurfaceRole::Overlay, SurfaceRole::Toast}) {
            for (const auto &t : tagsForRole(role, 9001))
                QVERIFY(t.value != 769);
        }
    }
};

QTEST_APPLESS_MAIN(TestSurfaceTags)
#include "tst_surfacetags.moc"

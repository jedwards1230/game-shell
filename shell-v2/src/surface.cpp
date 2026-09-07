#include "surface.h"
#include "x11tagger.h"

#include <QLoggingCategory>
#include <QVariantMap>

Q_DECLARE_LOGGING_CATEGORY(lcTag)

namespace tvshell {
namespace {

// The shell's app id is private to the deployment (docs/V2_DESIGN.md §5): under
// `--steam`, 769 is the Steam client's own id and carries gamescope's
// `window_is_steam` behaviour, so the shell must not use it. 9001 is the id the
// measured prototype used (`dev/gamescope/launch.sh`), kept here so the shim and
// the bench agree. The env var name is the prototype's, for the same reason.
constexpr uint kDefaultShellAppId = 9001;

uint defaultShellAppId()
{
    static const uint value = [] {
        bool ok = false;
        const uint fromEnv = qEnvironmentVariableIntValue("TV_SHELL_GS_SHELL_APPID", &ok);
        return (ok && fromEnv != 0) ? fromEnv : kDefaultShellAppId;
    }();
    return value;
}

} // namespace

// m_complete starts true: a Surface built from C++ has no parser status to wait
// for, so setVisible() acts immediately. classBegin() clears it for the QML path.
Surface::Surface(QWindow *parent) : QQuickWindow(parent), m_appId(defaultShellAppId()) { }

bool Surface::frozen(const char *what) const
{
    if (!handle())
        return false;
    qCWarning(lcTag, "ignoring %s change: surface already created (role decides tagging at map)",
              what);
    return true;
}

void Surface::setRole(Role role)
{
    if (m_role == role || frozen("role"))
        return;
    m_role = role;
    Q_EMIT roleChanged();
}

void Surface::setAppId(uint appId)
{
    if (m_appId == appId || frozen("appId"))
        return;
    m_appId = appId;
    Q_EMIT appIdChanged();
}

QVariantMap Surface::plannedTags() const
{
    QVariantMap out;
    for (const SurfaceTag &tag : tagsForRole(pureRole(), m_appId))
        out.insert(QString::fromLatin1(tag.name), tag.value);
    return out;
}

void Surface::setVisible(bool visible)
{
    m_wantVisible = visible;
    // Before componentComplete() the role may not have been assigned yet, so
    // showing now could tag with a stale role. Hold the request; componentComplete
    // replays it. Outside QML (a C++-constructed Surface, as in the tests) there
    // is no parser status to wait for, so m_complete is set in the constructor.
    if (m_complete)
        applyVisibility();
}

void Surface::componentComplete()
{
    m_complete = true;
    applyVisibility();
}

void Surface::applyVisibility()
{
    if (!m_wantVisible) {
        QQuickWindow::setVisible(false);
        return;
    }

    // The three-step ordering this whole class exists for. `create()` issues X
    // CreateWindow without mapping; the base `setVisible(true)` below issues
    // MapWindow. Everything between the two is guaranteed to reach the server
    // first, because it goes out on the same connection.
    if (!handle())
        create();

    // MUTATION (throwaway branch, never merged): tag AFTER the map instead of
    // before it. If tst_premap is defending anything, this must turn it red.
    QQuickWindow::setVisible(true);

    const bool ok = applyTags(static_cast<std::uint32_t>(winId()), tagsForRole(pureRole(), m_appId));
    if (ok != m_tagged) {
        m_tagged = ok;
        Q_EMIT taggedChanged();
    }
}

} // namespace tvshell

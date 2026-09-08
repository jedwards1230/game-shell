#include "x11tagger.h"

#include <QGuiApplication>
#include <QHash>
#include <QLoggingCategory>
// Public since Qt 6.2 — QNativeInterface::QX11Application hands out the xcb
// connection Qt's platform plugin owns. Deliberately NOT the QPA private
// headers: nothing in this shell needs a private API.
#include <QtGui/qguiapplication_platform.h>

#include <xcb/xcb.h>

Q_LOGGING_CATEGORY(lcTag, "tvshell.tag")

namespace tvshell {
namespace {

// Qt exposes its xcb connection through the platform native interface. Using
// Qt's connection (rather than opening our own) is what makes the write/map
// ordering a server-side guarantee -- see x11tagger.h.
xcb_connection_t *qtConnection()
{
    auto *app = qGuiApp;
    if (!app)
        return nullptr;
    auto *x11 = app->nativeInterface<QNativeInterface::QX11Application>();
    return x11 ? x11->connection() : nullptr;
}

// Interning is per-name and cheap after the first call; xcb caches nothing for
// us, so we do one round-trip per distinct atom per process via a small cache.
xcb_atom_t internAtom(xcb_connection_t *conn, const char *name)
{
    static QHash<QByteArray, xcb_atom_t> cache;
    const QByteArray key(name);
    const auto it = cache.constFind(key);
    if (it != cache.constEnd())
        return *it;

    xcb_intern_atom_cookie_t cookie =
            xcb_intern_atom(conn, /*only_if_exists=*/0, static_cast<uint16_t>(key.size()), name);
    xcb_generic_error_t *err = nullptr;
    xcb_intern_atom_reply_t *reply = xcb_intern_atom_reply(conn, cookie, &err);
    xcb_atom_t atom = XCB_ATOM_NONE;
    if (reply) {
        atom = reply->atom;
        free(reply);
    }
    if (err)
        free(err);
    cache.insert(key, atom);
    return atom;
}

} // namespace

bool x11Available()
{
    return qtConnection() != nullptr;
}

bool applyTags(std::uint32_t xid, const std::vector<SurfaceTag> &tags)
{
    xcb_connection_t *conn = qtConnection();
    if (!conn) {
        // Expected under QT_QPA_PLATFORM=offscreen (the headless QML tests) and
        // on a Wayland session. Not fatal: the window still works, it is simply
        // untagged, which under gamescope means it is not a focus candidate.
        qCWarning(lcTag, "no X connection (platform=%s); window 0x%x left untagged",
                  qPrintable(QGuiApplication::platformName()), xid);
        return false;
    }
    if (xid == 0) {
        qCWarning(lcTag, "refusing to tag window id 0");
        return false;
    }

    for (const SurfaceTag &tag : tags) {
        const xcb_atom_t atom = internAtom(conn, tag.name);
        if (atom == XCB_ATOM_NONE) {
            qCWarning(lcTag, "could not intern atom %s", tag.name);
            return false;
        }
        const std::uint32_t value = tag.value;
        xcb_change_property(conn, XCB_PROP_MODE_REPLACE, xid, atom, XCB_ATOM_CARDINAL,
                            /*format=*/32, /*data_len=*/1, &value);
        qCDebug(lcTag, "0x%x %s=%u", xid, tag.name, value);
    }

    // Push the requests out now. Ordering relative to Qt's later map request is
    // already guaranteed by the shared connection; the flush just means the
    // server sees them without waiting for Qt's next flush, which keeps the
    // window between create and map as short as possible.
    xcb_flush(conn);
    return true;
}

} // namespace tvshell

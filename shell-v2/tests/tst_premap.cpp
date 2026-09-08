// Does the shim actually tag BEFORE the window maps?
//
// This is the assertion the whole PR turns on, and it is deliberately not a
// read-back: reading STEAM_OVERLAY after the window is up proves the property
// exists, not that it existed in time. gamescope evaluates a window at creation,
// so "eventually tagged" is indistinguishable from "untagged" as far as the
// compositor's decision goes.
//
// So we assert the ORDER OF EVENTS, from a second X client:
//
//   1. surface.create()            — X CreateWindow, no map. The window id now
//                                    exists, so a watcher can subscribe to it.
//   2. watcher selects PropertyChangeMask | StructureNotifyMask on that id.
//   3. surface.setVisible(true)    — the shim's tag-then-map sequence.
//   4. assert the watcher saw PropertyNotify(STEAM_OVERLAY) BEFORE MapNotify.
//
// Step 1 is what makes this deterministic rather than a race: the tagging
// deliberately does NOT happen in create(), so the watcher is guaranteed to be
// listening before any of it happens. Move applyTags() after the base
// setVisible() and this test fails every time, not sometimes.
#include "surface.h"

#include <QtTest>
#include <QGuiApplication>

#include <xcb/xcb.h>

using namespace tvshell;

namespace {

xcb_atom_t intern(xcb_connection_t *c, const char *name)
{
    xcb_intern_atom_reply_t *r = xcb_intern_atom_reply(
            c, xcb_intern_atom(c, 0, static_cast<uint16_t>(qstrlen(name)), name), nullptr);
    if (!r)
        return XCB_ATOM_NONE;
    const xcb_atom_t a = r->atom;
    free(r);
    return a;
}

} // namespace

class TestPreMap : public QObject
{
    Q_OBJECT

private slots:
    void initTestCase()
    {
        QVERIFY2(QGuiApplication::platformName().startsWith(QLatin1String("xcb")),
                 "this lane requires QT_QPA_PLATFORM=xcb against a real X server");
        m_watch = xcb_connect(nullptr, nullptr);
        QVERIFY2(m_watch && !xcb_connection_has_error(m_watch), "watcher could not connect to X");
    }

    void cleanupTestCase()
    {
        if (m_watch)
            xcb_disconnect(m_watch);
    }

    void tagsLandBeforeTheWindowMaps_data()
    {
        QTest::addColumn<int>("role");
        QTest::addColumn<QByteArray>("atom");
        // One row per role, each naming an atom unique to that role, so a
        // regression in any single role's mapping is caught here too.
        QTest::newRow("base") << int(Surface::Base) << QByteArray("STEAM_GAME");
        QTest::newRow("overlay") << int(Surface::Overlay) << QByteArray("STEAM_INPUT_FOCUS");
        QTest::newRow("toast") << int(Surface::Toast) << QByteArray("STEAM_NOTIFICATION");
    }

    void tagsLandBeforeTheWindowMaps()
    {
        QFETCH(int, role);
        QFETCH(QByteArray, atom);

        const xcb_atom_t watched = intern(m_watch, atom.constData());
        QVERIFY(watched != XCB_ATOM_NONE);

        Surface surface;
        surface.setRole(static_cast<Surface::Role>(role));
        surface.resize(120, 80);

        // Step 1: platform window without a map.
        surface.create();
        const xcb_window_t xid = static_cast<xcb_window_t>(surface.winId());
        QVERIFY(xid != 0);
        QVERIFY2(!surface.isVisible(), "create() must not map the window");

        // Step 2: subscribe, and round-trip so the server has processed the
        // subscription before anything else is sent.
        const uint32_t mask = XCB_EVENT_MASK_PROPERTY_CHANGE | XCB_EVENT_MASK_STRUCTURE_NOTIFY;
        xcb_change_window_attributes(m_watch, xid, XCB_CW_EVENT_MASK, &mask);
        free(xcb_get_input_focus_reply(m_watch, xcb_get_input_focus(m_watch), nullptr));

        // Step 3: the sequence under test.
        surface.setVisible(true);
        QVERIFY2(surface.tagged(), "applyTags() reported failure on an xcb platform");
        // Let the server process Qt's map request, then drain.
        QTest::qWait(200);

        // Step 4: read the watcher's queue in order.
        int propertyAt = -1;
        int mapAt = -1;
        int seen = 0;
        while (xcb_generic_event_t *ev = xcb_poll_for_event(m_watch)) {
            const uint8_t type = ev->response_type & ~0x80;
            if (type == XCB_PROPERTY_NOTIFY) {
                auto *pn = reinterpret_cast<xcb_property_notify_event_t *>(ev);
                if (pn->window == xid && pn->atom == watched && propertyAt < 0)
                    propertyAt = seen;
            } else if (type == XCB_MAP_NOTIFY) {
                auto *mn = reinterpret_cast<xcb_map_notify_event_t *>(ev);
                if (mn->window == xid && mapAt < 0)
                    mapAt = seen;
            }
            ++seen;
            free(ev);
        }

        QVERIFY2(propertyAt >= 0,
                 qPrintable(QStringLiteral("no PropertyNotify for %1").arg(QString(atom))));
        QVERIFY2(mapAt >= 0, "no MapNotify — the window never mapped");
        QVERIFY2(propertyAt < mapAt,
                 qPrintable(QStringLiteral("%1 was set AFTER the window mapped (property at %2, "
                                           "map at %3) — gamescope had already decided")
                                    .arg(QString(atom))
                                    .arg(propertyAt)
                                    .arg(mapAt)));
    }

    // The companion assertion: an overlay must NOT be carrying the shell's app
    // id on the wire. Checked against the server rather than the pure mapping,
    // so a shim that helpfully added it later would still be caught.
    void overlayHasNoAppIdOnTheServer()
    {
        const xcb_atom_t steamGame = intern(m_watch, "STEAM_GAME");
        QVERIFY(steamGame != XCB_ATOM_NONE);

        Surface surface;
        surface.setRole(Surface::Overlay);
        surface.resize(120, 80);
        surface.setVisible(true);
        QTest::qWait(200);

        const xcb_window_t xid = static_cast<xcb_window_t>(surface.winId());
        xcb_get_property_reply_t *r = xcb_get_property_reply(
                m_watch,
                xcb_get_property(m_watch, 0, xid, steamGame, XCB_ATOM_CARDINAL, 0, 1),
                nullptr);
        QVERIFY(r);
        const int len = xcb_get_property_value_length(r);
        free(r);
        QCOMPARE(len, 0);
    }

private:
    xcb_connection_t *m_watch = nullptr;
};

QTEST_MAIN(TestPreMap)
#include "tst_premap.moc"

// tv-shell v2 shell -- entry point.
//
// A plain Qt Quick application, not the `qml` runtime and not Quickshell. The
// reason is docs/V2_DESIGN.md §13 Q1: the shell must set X11 properties on its
// own toplevels before they map, which needs C++, which needs a build step. Once
// there is a build step, owning `main()` costs nothing further and buys the
// startup checks below -- so this is the smaller of the two reversals recorded
// in docs/V2_SHELL.md, not an extra one.
#include <QGuiApplication>
#include <QLoggingCategory>
#include <QQmlApplicationEngine>
#include <QQuickWindow>

Q_DECLARE_LOGGING_CATEGORY(lcTag)

int main(int argc, char *argv[])
{
    // Qt Quick's default backend needs a GL context. The headless test lane and
    // any software-rendered bring-up set QT_QUICK_BACKEND=software themselves;
    // nothing is forced here, so the couch keeps hardware rendering.
    QGuiApplication app(argc, argv);
    app.setApplicationName(QStringLiteral("tv-shell"));
    app.setDesktopFileName(QStringLiteral("tv-shell-v2"));

    // A native-Wayland toplevel has no STEAM_GAME selector at all -- gamescope's
    // focus control is X11-atom driven (V2_DESIGN §1 non-goals, and the same
    // warning in dev/gamescope/launch.sh). Under a Wayland platform plugin every
    // Surface below will map untagged and the shell will simply never become a
    // focus candidate: a black screen with no error. Say so at startup, loudly,
    // rather than letting it present as "the shell did not come up".
    const QString platform = QGuiApplication::platformName();
    if (!platform.startsWith(QLatin1String("xcb"))) {
        qCWarning(lcTag,
                  "platform plugin is '%s', not xcb: window tagging is unavailable and this shell "
                  "will not be a gamescope focus candidate. Set QT_QPA_PLATFORM=xcb.",
                  qPrintable(platform));
    }

    QQmlApplicationEngine engine;
    engine.loadFromModule("TvShell", "Main");
    if (engine.rootObjects().isEmpty())
        return 1;

    return app.exec();
}

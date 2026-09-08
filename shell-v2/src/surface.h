// `Surface` -- a typed toplevel that declares its ROLE, and tags itself from
// that role before the window maps.
//
// WHY THIS IS A C++ TYPE AND NOT A QML ONE
//
// docs/V2_DESIGN.md §5 requires the shell to set its X11 properties "before
// mapping". Post-map is too late: gamescope evaluates a window at creation and
// has already decided where it belongs. A QML `Window` cannot express that
// ordering -- `visible: true` is a binding, and QML offers no hook between "the
// platform window exists" and "the platform window is mapped".
//
// HOW THE ORDERING IS ENFORCED, AND WHY IT LOOKS LIKE THIS
//
// The obvious implementation -- override `QWindow::setVisible` -- does not
// exist: in Qt 6 `setVisible` is a slot but is NOT virtual, and neither is
// `create()`. There is no virtual hook anywhere between window creation and map.
// (This is a finding, not a workaround; it is recorded in docs/V2_SHELL.md
// because it constrains any future shell runtime, Quickshell included.)
//
// So the ordering is enforced at the two entry points that exist, and the others
// are removed:
//
//   * `visible` is REDECLARED here as a Q_PROPERTY. QML resolves properties
//     through the meta-object and finds the derived one, so a `visible: true`
//     binding goes through this class, not through QWindow.
//   * `Surface::setVisible` hides the base slot, so C++ calls through a
//     `Surface*` go through this class too.
//   * `show()`, `showNormal()`, `showFullScreen()` and `showMaximized()` are
//     DELETED. They call the base `setVisible` non-virtually and would map an
//     untagged window; deleting them turns that from a silent bug into a
//     compile error.
//
// and the sequence itself is:
//
//     setVisible(true)  ->  create()      X CreateWindow
//                       ->  applyTags()   X ChangeProperty  (x11tagger)
//                       ->  base impl     X MapWindow
//
// Both X requests travel on the connection Qt already owns, so the server
// processes them in that order by protocol guarantee, not by timing. The
// X-backed test asserts the resulting PropertyNotify/MapNotify order rather than
// trusting this comment (tests/tst_premap.cpp).
//
// Visibility is additionally DEFERRED until `componentComplete()`, the way
// QQuickWindowQmlImpl defers it for `Window`. Without that, a Surface whose
// `visible: true` appeared above its `role:` in the QML body would map before
// the role was assigned -- correctness by declaration order, which is exactly
// the class of fragility this design exists to remove.
//
// WHY ROLE-DRIVEN AND NOT PROPERTY-DRIVEN
//
// A caller sets `role`, never atoms. That is what makes "an overlay accidentally
// drawn inside the base window" unrepresentable: an overlay is not a flag on a
// surface, it IS a separate Surface with role Overlay, and §7's requirement that
// drawer/QAM/toasts be separate toplevels is therefore structural. There is no
// API here for tagging an arbitrary window.
#pragma once

#include "surfacetags.h"

#include <QQmlParserStatus>
#include <QQuickWindow>
#include <QtQml/qqmlregistration.h>

namespace tvshell {

class Surface : public QQuickWindow, public QQmlParserStatus
{
    Q_OBJECT
    Q_INTERFACES(QQmlParserStatus)
    QML_ELEMENT

    Q_PROPERTY(Role role READ role WRITE setRole NOTIFY roleChanged)
    Q_PROPERTY(uint appId READ appId WRITE setAppId NOTIFY appIdChanged)
    Q_PROPERTY(bool tagged READ tagged NOTIFY taggedChanged)
    // Shadows QWindow::visible on purpose -- see the header comment. Same name,
    // same NOTIFY signal, so every existing binding keeps working; only the
    // write path changes.
    //
    // FINAL is not decoration. Without it Qt's property cache logs
    // "Member visible of the object tvshell::Surface overrides a member of the
    // base object. Consider renaming it or adding final or override specifier"
    // on every run -- a real warning about a real shadowing, which we want
    // DECLARED rather than accidental. FINAL says the shadowing is deliberate
    // and stops a further subclass from shadowing it again.
    Q_PROPERTY(bool visible READ isVisible WRITE setVisible NOTIFY visibleChanged FINAL)

public:
    // Mirrors tvshell::SurfaceRole. Duplicated as a Q_ENUM rather than exposing
    // the pure enum directly so the pure header stays free of Qt's moc.
    enum Role {
        Base = 0,
        Overlay = 1,
        Toast = 2,
    };
    Q_ENUM(Role)

    explicit Surface(QWindow *parent = nullptr);

    Role role() const { return m_role; }
    void setRole(Role role);

    uint appId() const { return m_appId; }
    void setAppId(uint appId);

    // True once the tags for this surface reached the X server. False on a
    // platform with no X connection, and false before the first show. Read-only
    // from QML on purpose -- it reports what happened, it does not request it.
    bool tagged() const { return m_tagged; }

    // The tag-then-map sequence. Hides QWindow::setVisible (not an override --
    // the base is not virtual).
    void setVisible(bool visible);

    // Deleted so they cannot silently map an untagged window; see the header
    // comment. Use `visible`.
    void show() = delete;
    void showNormal() = delete;
    void showFullScreen() = delete;
    void showMaximized() = delete;

    // The atoms this surface WOULD write, without writing them. Exposed so a
    // headless test (and the panel, later) can assert the mapping on a platform
    // that has no X server at all.
    Q_INVOKABLE QVariantMap plannedTags() const;

    // QQmlParserStatus
    // The engine calls this BEFORE any property assignment, so it is where a
    // QML-constructed Surface opts into deferral. A C++-constructed one never
    // gets here and stays "complete" from construction (see the constructor),
    // which is what lets the tests drive setVisible() directly.
    void classBegin() override { m_complete = false; }
    void componentComplete() override;

Q_SIGNALS:
    void roleChanged();
    void appIdChanged();
    void taggedChanged();

private:
    SurfaceRole pureRole() const { return static_cast<SurfaceRole>(m_role); }
    // Refuse a configuration change once the platform window exists: after
    // `create()` the role has already decided how this window was tagged, so a
    // later change would leave the object and the server disagreeing. Logs and
    // ignores rather than asserting, because a crash on the couch is worse than
    // a stale drawer.
    bool frozen(const char *what) const;
    void applyVisibility();

    Role m_role = Base;
    uint m_appId = 0;
    bool m_tagged = false;
    bool m_wantVisible = false;
    bool m_complete = true;
};

} // namespace tvshell

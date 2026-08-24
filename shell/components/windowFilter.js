.pragma library

// Which Hyprland window classes count as a running APPLICATION.
//
// Pure `.pragma library` so the rule is one list, shared by every scan in
// AppLifecycleManager (window enumeration, idle adoption, launch detection) and
// directly testable headless — AppLifecycleManager.qml imports Quickshell.Io and
// cannot load under qmltestrunner (tests/qml/tst_windowfilter.qml).

// The shell's OWN surfaces. Quickshell names its layer surfaces with a
// `quickshell`-containing class, hence a substring match rather than an exact
// one: there is no single fixed class to compare against.
var SHELL_CLASS_SUBSTRING = "quickshell";

// Compositor-owned transient NOTICES — not applications.
//
// Hyprland maps its own donate/update nag at session start (and again after a
// compositor upgrade). It has a real window class, so without this list it is
// enumerated like any app: it gets a running card on Home and a Resume row in the
// nav drawer for a window the user cannot meaningfully resume, and adopting it
// can flip the shell into `appRunning` over a notice. The 12x
// `Could not load icon "hyprland-donate-screen"` warnings per cold start are a
// downstream CONSEQUENCE of that misclassification (the class becomes an icon
// name at AppLifecycleManager's `iconName` fallback, and no `.desktop` matches
// it), not the defect itself — so this is fixed at the enumerator, not with an
// icon-name filter.
//
// Kept in sync BY HAND with the `windowrule` block in `config/hyprland.conf`
// (search that file for `hyprland-donate-screen`), which floats/centers the same
// pair so the nag reads as a dismissable overlay instead of blanketing the TV.
// Two lists, two jobs: that one governs how the compositor MAPS the window, this
// one governs whether the shell treats it as an app. Add a class to both.
//
// EXACT matches only. A substring/prefix test on "hyprland" would swallow real
// Hyprland-based application windows.
var NOTICE_CLASSES = ["hyprland-donate-screen", "hyprland-update-screen"];

// True when `cls` is a window the shell should treat as a running application.
// False for an absent/empty class, the shell's own surfaces, and the compositor
// notices above. This is the single filter every client-list scan applies; a
// caller may add its own extra conditions (e.g. the prelaunch-class list) on top.
function isAppWindow(cls) {
    if (!cls || cls === "")
        return false;
    if (cls.indexOf(SHELL_CLASS_SUBSTRING) >= 0)
        return false;
    // Case-insensitive: `class` casing is set by the toolkit, not by us, and the
    // comparison targets two exact literal names so folding case cannot widen the
    // match onto an unrelated app.
    var lower = cls.toLowerCase();
    for (var i = 0; i < NOTICE_CLASSES.length; i++) {
        if (lower === NOTICE_CLASSES[i])
            return false;
    }
    return true;
}

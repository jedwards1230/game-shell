import QtQuick
import QtTest
import "../../shell/components/windowFilter.js" as WindowFilter
import "../../shell/components/resumeModel.js" as ResumeModel

// Headless tests for the running-application filter
// (shell/components/windowFilter.js), imported by its real source path like the
// other pure `.pragma library` helpers — zero drift, no stubs.
//
// WHICH LAYER THIS COVERS — read before trusting it. `AppLifecycleManager.qml`
// imports `Quickshell.Io` and cannot be loaded by qmltestrunner at all, so the
// client-list scan itself is NOT exercised here. What is:
//   1. the filter predicate the three scans in that file now call, directly; and
//   2. the DOWNSTREAM consequence, by pushing a filtered client list through the
//      real `resumeModel.build()` and asserting the excluded window produces no
//      Resume entry (and a normal one still does).
// The uncovered gap is therefore exactly one line per scan site — that the loop
// calls `WindowFilter.isAppWindow(cls)` — which is visible in review.
//
// WHY THE FILTER EXISTS: Hyprland maps its own donate/update nag at session
// start; it has a real window class, so it used to be enumerated as a running
// app (a Home running card + a nav-drawer Resume row for a window nobody can
// resume). The 12x `Could not load icon "hyprland-donate-screen"` warnings per
// cold start were a downstream symptom of that, not the defect. Confirmed live on
// htpc-1 via `hyprctl clients -j`; `config/hyprland.conf` already floats/centers
// the same class pair.
TestCase {
    id: testCase
    name: "WindowFilter"

    // A minimal stand-in for the WindowMatcher singleton (resumeModel.build takes
    // it injected precisely so this stays QML-free). Same shape the drawer passes.
    property var matcher: ({
            execBasename: function (s) {
                s = s || "";
                var parts = s.split(" ")[0].split("/");
                return parts[parts.length - 1].toLowerCase();
            },
            normalize: function (s) {
                return (s || "").toLowerCase().replace(/[^a-z0-9]/g, "");
            }
        })

    function _win(cls) {
        return {
            windowClass: cls,
            address: "0x" + cls.length,
            title: cls,
            name: cls,
            icon: cls,
            focusHistoryId: 0,
            exec: ""
        };
    }

    // What AppLifecycleManager's enumeration loop does with the predicate.
    function _enumerate(classes) {
        var out = [];
        for (var i = 0; i < classes.length; i++) {
            if (!WindowFilter.isAppWindow(classes[i]))
                continue;
            out.push(_win(classes[i]));
        }
        return out;
    }

    // === The predicate ===============================================

    function test_a_normal_app_window_is_an_app() {
        verify(WindowFilter.isAppWindow("firefox"));
        verify(WindowFilter.isAppWindow("steam"));
        verify(WindowFilter.isAppWindow("plexhtpc"));
    }

    function test_absent_or_empty_class_is_not_an_app() {
        verify(!WindowFilter.isAppWindow(""));
        verify(!WindowFilter.isAppWindow(undefined));
        verify(!WindowFilter.isAppWindow(null));
    }

    // Pre-existing behaviour, kept verbatim: the shell's own layer surfaces are
    // excluded by SUBSTRING (there is no one fixed class to compare against).
    function test_shell_surfaces_are_excluded_by_substring() {
        verify(!WindowFilter.isAppWindow("quickshell"));
        verify(!WindowFilter.isAppWindow("org.quickshell.tv-shell"));
        verify(!WindowFilter.isAppWindow("quickshell-overlay"));
    }

    // The change: Hyprland's own announcement windows are not applications.
    function test_compositor_notices_are_excluded() {
        verify(!WindowFilter.isAppWindow("hyprland-donate-screen"));
        verify(!WindowFilter.isAppWindow("hyprland-update-screen"));
    }

    function test_notice_match_is_case_insensitive() {
        verify(!WindowFilter.isAppWindow("Hyprland-Donate-Screen"));
        verify(!WindowFilter.isAppWindow("HYPRLAND-UPDATE-SCREEN"));
    }

    // The guardrail that keeps the exclusion from swallowing real software: the
    // notice list is matched EXACTLY, never as a substring or prefix on
    // "hyprland". A Hyprland-based app must still enumerate.
    function test_the_notice_match_is_exact_not_a_hyprland_prefix() {
        verify(WindowFilter.isAppWindow("hyprland"));
        verify(WindowFilter.isAppWindow("Hyprland"));
        verify(WindowFilter.isAppWindow("hyprland-share-picker"));
        verify(WindowFilter.isAppWindow("hyprland-donate-screen-viewer"), "a longer class is a different window");
        verify(WindowFilter.isAppWindow("my-hyprland-donate-screen"), "a prefixed class is a different window");
    }

    // The list is the discoverable constant the call sites share, and it must stay
    // in step with the `windowrule` pair in config/hyprland.conf.
    function test_the_notice_list_is_exported_and_holds_both_classes() {
        compare(WindowFilter.NOTICE_CLASSES.length, 2);
        verify(WindowFilter.NOTICE_CLASSES.indexOf("hyprland-donate-screen") >= 0);
        verify(WindowFilter.NOTICE_CLASSES.indexOf("hyprland-update-screen") >= 0);
    }

    // === The downstream consequence (real resumeModel.build) =========

    function test_an_excluded_notice_never_reaches_the_resume_model() {
        var running = _enumerate(["firefox", "hyprland-donate-screen", "hyprland-update-screen", "quickshell"]);
        compare(running.length, 1, "only the real app is enumerated");
        compare(running[0].windowClass, "firefox");

        var model = ResumeModel.build(running, [], [], testCase.matcher);
        compare(model.length, 1, "the nag produces no Resume entry");
        compare(model[0].windowClass, "firefox");
        verify(model[0].running);
        for (var i = 0; i < model.length; i++) {
            verify(model[i].icon.indexOf("hyprland-donate-screen") < 0, "no entry carries the nag's class as an icon name");
        }
    }

    // Negative control: WITHOUT the filter the nag does reach the drawer and
    // carries its class as an icon name — i.e. the assertion above is load-bearing
    // rather than trivially true.
    function test_without_the_filter_the_nag_would_reach_the_resume_model() {
        var unfiltered = [_win("firefox"), _win("hyprland-donate-screen")];
        var model = ResumeModel.build(unfiltered, [], [], testCase.matcher);
        compare(model.length, 2, "unfiltered, the nag becomes a Resume entry");
        var icons = model.map(function (e) {
            return e.icon;
        });
        verify(icons.indexOf("hyprland-donate-screen") >= 0, "and its class is used as an icon name");
    }

    // Recents still merge normally alongside a filtered client list.
    function test_recents_still_merge_after_filtering() {
        var running = _enumerate(["hyprland-donate-screen", "firefox"]);
        var model = ResumeModel.build(running, [
            {
                name: "Plex",
                exec: "plexhtpc",
                icon: "plexhtpc",
                comment: ""
            }
        ], [], testCase.matcher);
        compare(model.length, 2, "one running app + one unmatched recent");
        compare(model[0].windowClass, "firefox");
        verify(model[0].running);
        compare(model[1].name, "Plex");
        verify(!model[1].running);
    }
}

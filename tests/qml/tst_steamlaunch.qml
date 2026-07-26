import QtQuick
import QtTest
import "../../shell/components/steamLaunch.js" as SteamLaunch

// Headless tests for the stream-target selection in steamLaunch.js — a pure
// `.pragma library` imported by its real source path (zero drift), like
// appQuirks.js / prewarm.js.
//
// What these pin:
//  - the Steam host and the Moonlight stream target are two DIFFERENT addresses.
//    `steam-launch`/`steam-quit` go to whichever sidecar the daemon has active;
//    the stream used to go to a hardcoded targets[0]. On a dual-boot gaming PC
//    (Linux boot = one IP + one target entry, Windows boot = another) that sent
//    the video request to the powered-off boot: Resume half-fired, the host
//    navigated, and no picture ever arrived — the "Resume does nothing" report;
//  - the fallback to targets[0] when nothing matches, so single-host setups and
//    daemons too old to report a host behave exactly as before;
//  - canStream(), which drives the popover's disabled-with-a-reason Resume row:
//    true when there's a target to stream to OR this client is already streaming
//    (the host-side navigate alone moves the live session).
TestCase {
    id: testCase
    name: "SteamLaunch"

    readonly property var linuxBoot: ({
            "name": "desktop-1",
            "host": "192.168.8.10"
        })
    readonly property var windowsBoot: ({
            "name": "desktop-2",
            "host": "192.168.8.153"
        })

    function rootWith(targets, activeSteamHost, shellState) {
        return {
            "targets": targets,
            "activeSteamHost": activeSteamHost,
            "shellState": shellState || "idle"
        };
    }

    // The regression: with both boots configured, the active Steam host decides.
    function test_matches_active_steam_host() {
        var r = rootWith([linuxBoot, windowsBoot], "192.168.8.153");
        compare(SteamLaunch.streamTargetFor(r).name, "desktop-2", "streams the host actually serving the library, not targets[0]");
    }

    function test_matches_active_steam_host_when_it_is_first() {
        var r = rootWith([linuxBoot, windowsBoot], "192.168.8.10");
        compare(SteamLaunch.streamTargetFor(r).name, "desktop-1");
    }

    // No match (host not in targets.json) → previous behaviour: primary target.
    function test_falls_back_to_first_target_on_no_match() {
        var r = rootWith([linuxBoot, windowsBoot], "192.168.8.99");
        compare(SteamLaunch.streamTargetFor(r).name, "desktop-1", "unknown active host falls back to targets[0]");
    }

    // Daemon too old to report a host (or first poll not back yet).
    function test_falls_back_to_first_target_with_no_reported_host() {
        var r = rootWith([linuxBoot, windowsBoot], "");
        compare(SteamLaunch.streamTargetFor(r).name, "desktop-1");
    }

    function test_no_targets_returns_null() {
        compare(SteamLaunch.streamTargetFor(rootWith([], "192.168.8.153")), null);
        compare(SteamLaunch.streamTargetFor(rootWith(undefined, "")), null, "an absent targets list must not throw");
    }

    // canStream — the Resume gate.
    function test_can_stream_with_a_target() {
        verify(SteamLaunch.canStream(rootWith([windowsBoot], "192.168.8.153")));
    }

    function test_cannot_stream_with_no_target() {
        verify(!SteamLaunch.canStream(rootWith([], "192.168.8.153")), "no target and not streaming ⇒ Resume is unavailable");
    }

    // Already in the stream: the host-side navigate alone moves the live session,
    // so Resume is meaningful even with nothing to stream to.
    function test_can_stream_while_already_streaming() {
        verify(SteamLaunch.canStream(rootWith([], "", "streaming")));
    }
}

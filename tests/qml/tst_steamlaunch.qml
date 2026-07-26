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

    // === steam-quit reply classification ===
    // The incident: a crashed Steam client left a stale RunningAppID, the shell
    // badged a phantom as "Playing", the host correctly refused the quit — and the
    // daemon flattened the refusal into {"status":"ok"}, so the UI showed nothing.
    // The daemon now answers {"status":"error","reason":...,"refused":true}; these
    // pin that the shell branches on the FLAG, not on the reason text, and that
    // every non-ok shape is distinguishable from success.

    function test_quit_reply_ok_says_nothing() {
        compare(SteamLaunch.classifyQuitReply('{"status":"ok"}').kind, "ok");
    }

    function test_quit_reply_refused_is_its_own_kind() {
        var r = SteamLaunch.classifyQuitReply('{"status":"error","reason":"not running","refused":true}');
        compare(r.kind, "refused", "a refusal must not read as success");
        compare(r.reason, "not running", "the host's reason is surfaced verbatim");
    }

    // The flag decides, not the wording — the host is free to reword the reason.
    function test_quit_reply_refusal_does_not_depend_on_the_reason_text() {
        var r = SteamLaunch.classifyQuitReply('{"status":"error","reason":"no matching process for appid 252950","refused":true}');
        compare(r.kind, "refused");
    }

    function test_quit_reply_plain_error_is_not_a_refusal() {
        var r = SteamLaunch.classifyQuitReply('{"status":"error","reason":"sidecar unreachable"}');
        compare(r.kind, "error");
        compare(r.reason, "sidecar unreachable");
    }

    // An older daemon answers a bare, non-JSON "ok" — still success, still silent.
    function test_quit_reply_legacy_bare_ok() {
        compare(SteamLaunch.classifyQuitReply("ok").kind, "ok");
    }

    // Garbage must not throw: this runs off a socket reply on a user keypress.
    function test_quit_reply_garbage_is_unknown_not_a_throw() {
        compare(SteamLaunch.classifyQuitReply("<html>502</html>").kind, "unknown");
        compare(SteamLaunch.classifyQuitReply("").kind, "unknown");
        compare(SteamLaunch.classifyQuitReply("null").kind, "unknown");
    }
}

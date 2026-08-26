import QtQuick
import QtTest
import "../../shell/components/audioOwnership.js" as AudioOwnership

// Headless tests for "audio ownership follows workspace ownership".
// audioOwnership.js is a pure `.pragma library` imported by its real source path
// (zero drift) — no Quickshell, no stubs.
//
// WHAT THESE PIN. The old muter's bug was invisible from the outside: it engaged
// for the wrong window, so the user experienced "sometimes I hear the game on the
// home screen and sometimes I don't" with nothing in a log to distinguish the
// cases. The fixtures below are the REAL shapes read off a live box on
// 2026-08-26 — Plex, Steam Big Picture and a Steam Remote Play stream, each on
// its own workspace — so the discriminations that matter are asserted rather
// than assumed.
TestCase {
    id: testCase
    name: "AudioOwnership"

    function _win(windowClass, workspace) {
        return {
            windowClass: windowClass,
            workspace: workspace
        };
    }

    // Verbatim from `hyprctl -j clients` on the live box.
    readonly property var fleet: [_win("tv.plex.Plex", "2"), _win("streaming_client", "3"), _win("steam", "4")]

    // Verbatim from `pw-dump`: the live Remote Play stream. Note that BOTH
    // `application.name` and `node.name` are "Steam" while the binary is
    // `streaming_client` — the whole reason the binary is consulted first.
    function _streamNode(id) {
        return {
            id: id || "65",
            binary: "streaming_client",
            appName: "Steam",
            nodeName: "Steam",
            mediaName: "Game streaming client (playback)"
        };
    }

    function _plexNode(id) {
        return {
            id: id || "70",
            binary: "Plex",
            appName: "Plex HTPC",
            nodeName: "Plex HTPC",
            mediaName: "Playback"
        };
    }

    function _steamNode(id) {
        return {
            id: id || "71",
            binary: "steamwebhelper",
            appName: "Steam",
            nodeName: "Steam",
            mediaName: "Chromium input"
        };
    }

    // --- attribution ---------------------------------------------------------

    // The headline discrimination. "streaming_client" does not contain "steam"
    // (str-e-am vs st-e-am), which is what broke the old allowlist — and here it
    // is exactly the property that keeps the live game's audio from being
    // credited to the Big Picture workspace.
    function test_stream_node_attributes_to_streaming_client_not_steam() {
        compare(AudioOwnership.ownerClassOf(_streamNode(), ["tv.plex.Plex", "streaming_client", "steam"]), "streaming_client");
    }

    // Reverse-DNS class wrapping the binary.
    function test_plex_binary_matches_reverse_dns_class() {
        compare(AudioOwnership.ownerClassOf(_plexNode(), ["tv.plex.Plex", "steam"]), "tv.plex.Plex");
    }

    // Binary is the class plus a suffix — containment must work both ways.
    function test_steam_helper_binary_matches_bare_class() {
        compare(AudioOwnership.ownerClassOf(_steamNode(), ["tv.plex.Plex", "steam"]), "steam");
    }

    // The loose pass exists for ONE real state, observed live: a stream process
    // outliving its window, still holding a playing node. With no
    // `streaming_client` window left, the binary names nothing, so the display
    // name "Steam" credits it to Steam — which is enough to mute it anywhere but
    // the Steam workspace.
    function test_orphaned_stream_falls_back_to_display_name() {
        compare(AudioOwnership.ownerClassOf(_streamNode(), ["tv.plex.Plex", "steam"]), "steam");
    }

    // ...but the loose pass must never PRE-EMPT the strict one. Both classes are
    // present here, and the binary has to win or the two Steam workspaces
    // collapse into one.
    function test_strict_pass_beats_loose_pass() {
        compare(AudioOwnership.ownerClassOf(_streamNode(), ["steam", "streaming_client"]), "streaming_client");
    }

    function test_unrelated_node_attributes_to_nothing() {
        let node = {
            id: "99",
            binary: "firefox",
            appName: "Firefox",
            nodeName: "Firefox",
            mediaName: "AudioStream"
        };
        compare(AudioOwnership.ownerClassOf(node, ["tv.plex.Plex", "steam"]), "");
    }

    // Two tokens this short can only match by accident, and an accidental match
    // silences the wrong app.
    function test_short_tokens_never_match() {
        let node = {
            id: "1",
            binary: "vl",
            appName: "vl",
            nodeName: "vl",
            mediaName: "vl"
        };
        compare(AudioOwnership.ownerClassOf(node, ["vlc"]), "");
        compare(AudioOwnership.ownerClassOf(_plexNode(), ["ab"]), "");
    }

    // Most specific wins, so listing order can't decide the answer.
    function test_longest_matching_class_wins() {
        compare(AudioOwnership.ownerClassOf(_steamNode(), ["steam", "steamwebhelper"]), "steamwebhelper");
        compare(AudioOwnership.ownerClassOf(_steamNode(), ["steamwebhelper", "steam"]), "steamwebhelper");
    }

    // --- policy --------------------------------------------------------------

    // The whole feature, in one assertion: on the stream's workspace you hear the
    // stream and nothing else.
    function test_only_the_displayed_workspace_stays_audible() {
        let nodes = [_streamNode("65"), _plexNode("70"), _steamNode("71")];
        let muted = AudioOwnership.desiredMutedIds(nodes, fleet, "3");
        compare(muted.length, 2);
        compare(muted.indexOf("65"), -1, "the displayed workspace's stream stays audible");
        verify(muted.indexOf("70") >= 0);
        verify(muted.indexOf("71") >= 0);
    }

    // The user's actual complaint: no game audio behind the home screen. Home
    // owns the reserved, deliberately EMPTY workspace 1, so nothing can match it
    // and everything mutes — no special case in the code, and none here either.
    function test_home_workspace_mutes_everything() {
        let nodes = [_streamNode("65"), _plexNode("70"), _steamNode("71")];
        let muted = AudioOwnership.desiredMutedIds(nodes, fleet, "1");
        compare(muted.length, 3);
    }

    // The live pathology this design had to handle: audio with no window at all.
    // On home it must go quiet even though nothing claims it.
    function test_orphaned_node_is_muted_on_home() {
        // No `streaming_client` window — only Plex and Steam are mapped.
        let windows = [_win("tv.plex.Plex", "2"), _win("steam", "4")];
        let muted = AudioOwnership.desiredMutedIds([_streamNode("65")], windows, "1");
        compare(muted, ["65"]);
    }

    // ...and while a DIFFERENT app is displayed. This is the state read off the
    // box: sitting on Plex with an orphaned game stream still playing.
    function test_orphaned_node_is_muted_while_another_app_is_displayed() {
        let windows = [_win("tv.plex.Plex", "2"), _win("steam", "4")];
        let muted = AudioOwnership.desiredMutedIds([_streamNode("65")], windows, "2");
        compare(muted, ["65"]);
    }

    // A node nothing claims is muted — the failure direction is documented, so
    // pin it rather than let it drift silently.
    function test_unattributable_node_is_muted() {
        let node = {
            id: "99",
            binary: "firefox",
            appName: "Firefox",
            nodeName: "Firefox",
            mediaName: "AudioStream"
        };
        compare(AudioOwnership.desiredMutedIds([node], fleet, "3"), ["99"]);
    }

    // No idea what is on screen means no policy. Muting on a guess is how you
    // silence the game the user is watching.
    function test_unknown_active_workspace_mutes_nothing() {
        let nodes = [_streamNode("65"), _plexNode("70")];
        compare(AudioOwnership.desiredMutedIds(nodes, fleet, ""), []);
    }

    function test_empty_graph_is_a_clean_no_op() {
        compare(AudioOwnership.desiredMutedIds([], fleet, "1"), []);
        compare(AudioOwnership.desiredMutedIds(null, null, "1"), []);
    }

    // With no windows mapped at all, every stream is unattributable — and every
    // stream mutes, which is right: nothing is on screen to own them.
    function test_no_windows_mutes_every_stream() {
        let nodes = [_streamNode("65"), _plexNode("70")];
        compare(AudioOwnership.desiredMutedIds(nodes, [], "1").length, 2);
    }

    // --- shell-owned audio ---------------------------------------------------

    // The Settings ▸ Audio speaker test execs `pw-play`, which has no window and
    // so attributes to nothing. Without the exemption it would be muted as an
    // orphan and the user would press "test the centre channel" and hear
    // silence — intermittently, since it only bites when a cycle happens to run
    // while the tone plays.
    function _tone() {
        return {
            id: "120",
            binary: "pw-play",
            appName: "pw-play",
            nodeName: "pw-play",
            mediaName: "tv-shell-tone.wav"
        };
    }

    function test_shell_speaker_test_is_never_muted() {
        verify(AudioOwnership.isShellOwned(_tone()));
        // On home, where everything else goes quiet.
        compare(AudioOwnership.desiredMutedIds([_tone()], fleet, "1"), []);
        // ...and while an app is displayed.
        compare(AudioOwnership.desiredMutedIds([_tone()], fleet, "3"), []);
    }

    // The exemption must not survive into the app streams around it.
    function test_shell_exemption_does_not_spare_app_audio() {
        let muted = AudioOwnership.desiredMutedIds([_tone(), _plexNode("70")], fleet, "1");
        compare(muted, ["70"]);
    }

    // Matched EXACTLY, so the exemption cannot quietly widen the way the fuzzy
    // relatedness test would let it.
    function test_shell_exemption_is_an_exact_binary_match() {
        let lookalike = {
            id: "121",
            binary: "pw-playback-helper",
            appName: "x",
            nodeName: "x",
            mediaName: "x"
        };
        verify(!AudioOwnership.isShellOwned(lookalike));
        verify(!AudioOwnership.isShellOwned(_plexNode()));
        verify(!AudioOwnership.isShellOwned(null));
    }

    // --- nodesFrom -----------------------------------------------------------

    // Only an app's own sink-inputs are ever candidates. A bug that let a SINK
    // through would mute the output the whole box plays through, so the filter is
    // asserted against the real `pw-dump` shape.
    function test_nodesFrom_keeps_only_playback_streams() {
        let dump = [
            {
                id: 80,
                info: {
                    props: {
                        "media.class": "Audio/Sink",
                        "node.name": "alsa_output.pci-0000_c4_00.1.hdmi-surround-extra1"
                    }
                }
            },
            {
                id: 53,
                info: {
                    props: {
                        "media.class": "Audio/Source",
                        "node.name": "alsa_input.usb-mic"
                    }
                }
            },
            {
                id: 65,
                info: {
                    props: {
                        "media.class": "Stream/Output/Audio",
                        "application.process.binary": "streaming_client",
                        "application.name": "Steam",
                        "node.name": "Steam",
                        "media.name": "Game streaming client (playback)"
                    }
                }
            },
            // A Client object, not a Node — no media.class at all.
            {
                id: 95,
                info: {
                    props: {
                        "application.process.binary": "streaming_client"
                    }
                }
            }
        ];
        let nodes = AudioOwnership.nodesFrom(dump);
        compare(nodes.length, 1);
        compare(nodes[0].id, "65");
        compare(nodes[0].binary, "streaming_client");
        compare(nodes[0].appName, "Steam");
    }

    function test_nodesFrom_tolerates_junk() {
        compare(AudioOwnership.nodesFrom(null), []);
        compare(AudioOwnership.nodesFrom([]), []);
        compare(AudioOwnership.nodesFrom([
            {},
            {
                info: {}
            }
        ]), []);
    }

    // --- the user's own manual mutes -----------------------------------------

    // The precedence that makes the feature worth having. A manual mute the
    // policy stomps on the next workspace switch is worse than no manual mute at
    // all, so a user-muted app stays muted even while its workspace is DISPLAYED.
    function test_user_mute_wins_over_the_policy_on_the_displayed_workspace() {
        let nodes = [_streamNode("65"), _plexNode("70")];
        let muted = AudioOwnership.desiredMutedIds(nodes, fleet, "3", ["streaming_client"]);
        verify(muted.indexOf("65") >= 0, "the displayed app stays muted because the user said so");
        verify(muted.indexOf("70") >= 0, "and the policy still owns everything else");
    }

    // Structural, not a check somewhere: because a user-muted class is always in
    // the desired set, reconcile can never place it in the unmute list. The
    // release path cannot revoke the user's choice as a side effect.
    function test_the_release_path_can_never_unmute_a_user_muted_app() {
        let nodes = [_streamNode("65")];
        // Applied holds it muted, and its workspace comes on screen — the exact
        // moment the policy would otherwise release it.
        let desired = AudioOwnership.desiredMutedIds(nodes, fleet, "3", ["streaming_client"]);
        let diff = AudioOwnership.reconcile(desired, ["65"]);
        compare(diff.unmute, [], "no user mute may be released by policy");
        compare(diff.mute, []);
    }

    // Clearing the manual mute hands the app straight back to the policy.
    function test_clearing_a_user_mute_returns_the_app_to_the_policy() {
        let nodes = [_streamNode("65")];
        let desired = AudioOwnership.desiredMutedIds(nodes, fleet, "3", []);
        compare(AudioOwnership.reconcile(desired, ["65"]).unmute, ["65"]);
        // ...and on a workspace that is NOT displayed, policy mutes it again.
        compare(AudioOwnership.desiredMutedIds(nodes, fleet, "2", []), ["65"]);
    }

    // A user mute does not depend on knowing what is on screen, and a shell
    // restart must not silently unmute what the user muted. Policy alone is
    // gated on a known workspace.
    function test_user_mutes_apply_even_when_the_workspace_is_unknown() {
        let nodes = [_streamNode("65"), _plexNode("70")];
        compare(AudioOwnership.desiredMutedIds(nodes, fleet, "", ["streaming_client"]), ["65"]);
    }

    // The user set is keyed by CLASS, so it survives the app closing and
    // reopening with a new node id — which a per-node mute would not.
    function test_user_mute_follows_the_class_not_the_node_id() {
        let reopened = _streamNode("999");
        compare(AudioOwnership.desiredMutedIds([reopened], fleet, "3", ["streaming_client"]), ["999"]);
    }

    // THE trap the design has to avoid: an adopted mute is one whose AUTHOR IS
    // UNKNOWN. It must never be mistaken for a user mute, or the user ends up
    // with an app they never muted and cannot unmute. Adoption populates the
    // applied set (node ids); the user set is classes and is written only by the
    // drawer. They are different data, and adoption reads nothing back into it.
    function test_adoption_never_manufactures_a_user_mute() {
        let stranded = _streamNode("107");
        stranded.muted = true;
        // Adoption sees it muted...
        compare(AudioOwnership.adoptableMutedIds([stranded]), ["107"]);
        // ...but with no user mute recorded, the policy still releases it on its
        // own workspace. An adopted mute is recoverable; a fabricated user mute
        // would not be.
        let desired = AudioOwnership.desiredMutedIds([stranded], fleet, "3", []);
        compare(AudioOwnership.reconcile(desired, ["107"]).unmute, ["107"]);
    }

    // The other half of that interaction: a real user mute that survived a
    // restart must stay put once adopted, not be released by the policy.
    function test_an_adopted_mute_that_is_also_a_user_mute_stays() {
        let stranded = _streamNode("107");
        stranded.muted = true;
        let applied = AudioOwnership.adoptableMutedIds([stranded]);
        let desired = AudioOwnership.desiredMutedIds([stranded], fleet, "3", ["streaming_client"]);
        compare(AudioOwnership.reconcile(desired, applied).unmute, []);
    }

    // --- startup adoption of pre-existing mutes ------------------------------

    // The field failure this exists for: mutes live in the PipeWire graph and
    // outlive the shell, but `_appliedIds` does not. Without adoption, a node the
    // PREVIOUS shell instance muted is never released — and restarting the shell
    // is the deploy loop. Observed as a live stream on the DISPLAYED workspace
    // playing to a muted node.
    function test_startup_adopts_existing_mutes() {
        let muted = _streamNode("107");
        muted.muted = true;
        compare(AudioOwnership.adoptableMutedIds([muted, _plexNode("70")]), ["107"]);
    }

    // Having adopted it, the very next reconcile must RELEASE it, because its
    // workspace is the one on screen. This is the assertion that would have
    // caught the live failure.
    function test_adopted_mute_is_released_when_its_workspace_is_displayed() {
        let stranded = _streamNode("107");
        stranded.muted = true;
        let applied = AudioOwnership.adoptableMutedIds([stranded]);
        let desired = AudioOwnership.desiredMutedIds([stranded], fleet, "3");
        let diff = AudioOwnership.reconcile(desired, applied);
        compare(diff.unmute, ["107"]);
        compare(diff.mute, []);
    }

    // An adopted mute that SHOULD stay muted produces no churn.
    function test_adopted_mute_that_is_still_correct_is_left_alone() {
        let stranded = _streamNode("107");
        stranded.muted = true;
        let applied = AudioOwnership.adoptableMutedIds([stranded]);
        let desired = AudioOwnership.desiredMutedIds([stranded], fleet, "1");
        let diff = AudioOwnership.reconcile(desired, applied);
        compare(diff.mute, []);
        compare(diff.unmute, []);
    }

    // Never adopt the shell's own test tone — adopting it would mean unmuting it
    // later, i.e. touching audio this policy has no business touching.
    function test_adoption_skips_shell_owned_audio() {
        let tone = _tone();
        tone.muted = true;
        compare(AudioOwnership.adoptableMutedIds([tone]), []);
    }

    function test_adoption_ignores_unmuted_and_junk() {
        compare(AudioOwnership.adoptableMutedIds([_plexNode("70")]), []);
        compare(AudioOwnership.adoptableMutedIds([]), []);
        compare(AudioOwnership.adoptableMutedIds(null), []);
    }

    // The mute flag lives under `info.params.Props[0]`, not in `info.props`.
    function test_nodesFrom_reads_the_mute_flag() {
        let dump = [
            {
                id: 107,
                info: {
                    props: {
                        "media.class": "Stream/Output/Audio",
                        "application.process.binary": "streaming_client"
                    },
                    params: {
                        Props: [
                            {
                                mute: true,
                                volume: 1.0
                            }
                        ]
                    }
                }
            },
            {
                id: 70,
                info: {
                    props: {
                        "media.class": "Stream/Output/Audio",
                        "application.process.binary": "Plex"
                    }
                }
            }
        ];
        let nodes = AudioOwnership.nodesFrom(dump);
        compare(nodes.length, 2);
        compare(nodes[0].muted, true);
        // Absent params must read as "not muted", so a pw-dump shape change
        // degrades into doing nothing rather than a false adoption.
        compare(nodes[1].muted, false);
    }

    // --- reconciliation ------------------------------------------------------

    function test_reconcile_computes_both_directions() {
        let diff = AudioOwnership.reconcile(["70", "71"], ["65", "70"]);
        compare(diff.mute, ["71"]);
        compare(diff.unmute, ["65"]);
    }

    function test_reconcile_in_sync_is_a_no_op() {
        let diff = AudioOwnership.reconcile(["70"], ["70"]);
        compare(diff.mute, []);
        compare(diff.unmute, []);
    }

    // Only ids WE muted are ever unmuted — audio the shell never touched stays
    // exactly as the user set it.
    function test_reconcile_never_unmutes_what_it_did_not_mute() {
        let diff = AudioOwnership.reconcile([], []);
        compare(diff.unmute, []);
        diff = AudioOwnership.reconcile(["70"], []);
        compare(diff.unmute, []);
        compare(diff.mute, ["70"]);
    }

    // Returning home from an app must undo every mute the app's workspace held.
    function test_reconcile_releases_everything_when_nothing_should_be_muted() {
        let diff = AudioOwnership.reconcile([], ["65", "70", "71"]);
        compare(diff.mute, []);
        compare(diff.unmute, ["65", "70", "71"]);
    }

    // --- applied-set bookkeeping ---------------------------------------------

    function test_nextApplied_adds_and_removes() {
        compare(AudioOwnership.nextApplied(["65"], ["70"], ["65"]), ["70"]);
        compare(AudioOwnership.nextApplied([], ["70", "71"], []), ["70", "71"]);
        compare(AudioOwnership.nextApplied(["70"], [], ["70"]), []);
    }

    // Forgetting a mute we issued is the one outcome that strands a silent app,
    // so a repeated mute must not duplicate and a repeated unmute must not
    // resurrect.
    function test_nextApplied_is_idempotent() {
        compare(AudioOwnership.nextApplied(["70"], ["70"], []), ["70"]);
        compare(AudioOwnership.nextApplied(["70", "70"], [], []), ["70"]);
        compare(AudioOwnership.nextApplied(["70"], [], ["70", "70"]), []);
    }

    // A full switch-away-and-back round trip leaves nothing muted.
    function test_round_trip_strands_nothing() {
        let nodes = [_streamNode("65"), _plexNode("70")];
        let applied = [];

        // Go home: everything mutes.
        let desired = AudioOwnership.desiredMutedIds(nodes, fleet, "1");
        let diff = AudioOwnership.reconcile(desired, applied);
        applied = AudioOwnership.nextApplied(applied, diff.mute, diff.unmute);
        compare(applied.length, 2);

        // Back to the stream: only Plex stays muted.
        desired = AudioOwnership.desiredMutedIds(nodes, fleet, "3");
        diff = AudioOwnership.reconcile(desired, applied);
        applied = AudioOwnership.nextApplied(applied, diff.mute, diff.unmute);
        compare(applied, ["70"]);

        // Over to Plex: the stream mutes, Plex is released.
        desired = AudioOwnership.desiredMutedIds(nodes, fleet, "2");
        diff = AudioOwnership.reconcile(desired, applied);
        applied = AudioOwnership.nextApplied(applied, diff.mute, diff.unmute);
        compare(applied, ["65"]);
    }

    // --- id validation -------------------------------------------------------

    // Ids come from `pw-dump`, not from a user, but nothing unvalidated reaches a
    // command line — and a malformed id would be a silent no-op that reads as a
    // policy bug.
    function test_only_bare_integers_are_valid_ids() {
        verify(AudioOwnership.isValidNodeId("65"));
        verify(AudioOwnership.isValidNodeId(65));
        verify(!AudioOwnership.isValidNodeId(""));
        verify(!AudioOwnership.isValidNodeId("65 1"));
        verify(!AudioOwnership.isValidNodeId("65;reboot"));
        verify(!AudioOwnership.isValidNodeId("-1"));
        verify(!AudioOwnership.isValidNodeId(null));
    }
}

import QtQuick
import QtTest
import components

// Headless test for PopoverMenu's disabled-item contract.
//
// The bug this pins: an action that is temporarily unavailable used to be
// dropped from the menu entirely (the caller just didn't push it), so the menu
// shape shifted under the user and nothing said why. `enabled: false` keeps the
// row on screen, muted and inert: A/Return does NOT fire it and does NOT close
// the menu, so the row's `hint` (the reason) stays readable. The Steam
// active-game menu uses this for Resume when there's no stream target — Quit,
// which only talks to the Steam host, stays live.
TestCase {
    id: testCase
    name: "PopoverMenu"
    when: windowShown
    visible: true
    width: 800
    height: 600

    property int firedEnabled: 0
    property int firedDisabled: 0

    Component {
        id: rigComp
        Item {
            width: 800
            height: 600
            property alias menu: m

            PopoverMenu {
                id: m
                anchors.fill: parent
                targetX: 400
                targetY: 300
                actions: [
                    {
                        label: "Resume",
                        hint: "No stream target configured",
                        enabled: false,
                        action: function () {
                            testCase.firedDisabled++;
                        }
                    },
                    {
                        label: "Quit",
                        hint: "A: Quit",
                        action: function () {
                            testCase.firedEnabled++;
                        }
                    }
                ]
            }
        }
    }

    function init() {
        firedEnabled = 0;
        firedDisabled = 0;
    }

    function openMenu() {
        var rig = createTemporaryObject(rigComp, testCase);
        verify(rig, "rig created");
        rig.menu.opened = true;
        rig.menu.forceActiveFocus();
        wait(50);
        verify(rig.menu.activeFocus, "an opened menu takes focus");
        return rig;
    }

    // Both defects in one: the menu opens with BOTH rows present (Resume is not
    // dropped just because it's unavailable) and Quit stays actionable.
    function test_disabled_item_is_rendered_not_dropped() {
        var rig = openMenu();
        compare(rig.menu.actions.length, 2, "both rows stay in the menu");
        compare(rig.menu.actions[0].enabled, false, "Resume is the disabled row");
        verify(rig.menu.actions[1].enabled === undefined, "Quit is enabled by default (no flag needed)");
    }

    // A on a disabled row: no action, and the menu STAYS OPEN so the hint
    // explaining why can still be read.
    function test_activating_disabled_item_is_a_no_op() {
        var rig = openMenu();
        var spy = createTemporaryObject(spyComp, testCase, {
            "target": rig.menu
        });
        keyClick(Qt.Key_Return);
        wait(50);
        compare(testCase.firedDisabled, 0, "a disabled action must not fire");
        compare(spy.count, 0, "a disabled item must not close the menu");
        verify(rig.menu.opened, "the menu stays open so the reason stays visible");
    }

    // The enabled sibling is unaffected: it fires and closes as before.
    function test_enabled_item_still_fires_and_closes() {
        var rig = openMenu();
        var spy = createTemporaryObject(spyComp, testCase, {
            "target": rig.menu
        });
        keyClick(Qt.Key_Down);
        keyClick(Qt.Key_Return);
        wait(50);
        compare(testCase.firedEnabled, 1, "the enabled action fires");
        compare(spy.count, 1, "the enabled action closes the menu");
    }

    // Walk the scene for the delegate label carrying `text`. The delegates are
    // built by a Repeater inside a Column, so there's no id to reach for.
    function findLabel(item, label) {
        if (item.text === label && item.color !== undefined)
            return item;
        for (var i = 0; i < item.children.length; i++) {
            var hit = findLabel(item.children[i], label);
            if (hit)
                return hit;
        }
        return null;
    }

    // "Disabled" has to be VISIBLE, not just inert — the muted label is the
    // affordance. Also proves `modelData.enabled` resolves in delegate scope.
    function test_disabled_row_renders_muted() {
        var rig = openMenu();
        var off = findLabel(rig.menu, "Resume");
        var on = findLabel(rig.menu, "Quit");
        verify(off && on, "both delegate labels exist");
        compare(off.color, Theme.textMuted, "the disabled row is muted");
        compare(on.color, Theme.textPrimary, "the enabled row is not");
    }

    // A disabled row must stay SELECTABLE — the footer hint is per-selected-item,
    // so it's the only place the reason can be shown.
    function test_disabled_item_stays_selectable() {
        var rig = openMenu();
        keyClick(Qt.Key_Down);
        keyClick(Qt.Key_Up);
        wait(50);
        compare(rig.menu.actions[0].hint, "No stream target configured", "the reason rides on the disabled row's hint");
    }

    Component {
        id: spyComp
        SignalSpy {
            signalName: "closed"
        }
    }
}

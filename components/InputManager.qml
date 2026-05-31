import Quickshell.Io
import QtQuick

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: grab, release, subscribe, get-pads, rumble
// Events handled: combo:force-quit, combo:end-session, combo:suspend-stream,
//   input-mode:*, controller-wake, controller-disconnected, intent:* (the
//   control-surface stream — the SOLE shell-intent vocabulary; the legacy
//   gamepad home-press / combo:home-hold bridge was deleted in Phase 5), and
//   the fleet per-pad events pad:connected / pad:disconnected / pad:index /
//   pad:battery (#98/#100/#101) — folded into the `pads` model below.
//
// Transport: SocketClient (native Quickshell.Io socket, #97) — the python3
// socket shims were retired in Phase 8.
Item {
    id: root

    signal forceQuitRequested
    signal endSessionRequested
    signal suspendStreamRequested
    signal inputModeChanged(string mode)
    signal controllerWake
    signal controllerDisconnected

    // --- Control-surface intents (Channel B: global, focus-independent) ---
    // Mapped from the daemon `intent:*` broadcast stream. QML owns the focus,
    // so it decides what each gamepad-neutral (home-tap/home-hold) means.
    signal intentHome          // global return-to-shell escape (always leaves app)
    signal intentHomeTap        // gamepad Home neutral — short press
    signal intentHomeHold       // gamepad Home neutral — long press (reset)
    signal intentMenu           // toggle nav drawer
    signal intentNav(string direction)  // up | down | left | right
    signal intentSelect
    signal intentBack
    signal intentSettings
    signal intentPower

    // --- Gamepad fleet model (#98/#100/#101) ---
    //
    // `pads` is the per-pad model the controller UI renders: an array of
    //   { id, index, name, batteryLevel, batteryCharging }
    // kept in ascending player-slot order. It is seeded from `get-pads` on
    // (re)connect and then kept live from the pad:* broadcast events:
    //   • pad:connected:{id,index,name}  → add/update (index,name)
    //   • pad:index:{id,index}           → update index
    //   • pad:battery:{id,level,charging}→ update battery (level 0..100, charging)
    //   • pad:disconnected:<id>          → remove
    // batteryLevel is -1 when a pad reports no battery (wired) so the UI can
    // distinguish "no battery" from "0%".
    property var pads: []

    // Wire-id strings of every connected pad (CONTRACT — consumed by the
    // rumble-trigger cluster). Kept in sync from pad:connected/pad:disconnected
    // (mirrored off `pads`).
    property var connectedPadIds: []

    // Fire a short rumble pulse on every connected pad. The daemon gates this on
    // the `rumbleEnabled` setting (and pad FF support), so QML fires it
    // unconditionally. See docs/IPC_PROTOCOL.md `rumble <id> <ms>`.
    //
    // rumbleCmd is a single one-shot request socket, so the per-pad commands are
    // QUEUED and sent strictly serially (the next only after the prior round-trip
    // completes) — issuing them back-to-back would have the reconnect for command
    // N+1 clobber command N's in-flight socket, so on a multi-pad fleet only the
    // last pad would rumble.
    property var _rumbleQueue: []
    property bool _rumbleBusy: false

    function rumblePulse(ms) {
        for (var i = 0; i < root.connectedPadIds.length; i++)
            root._rumbleQueue.push("rumble " + root.connectedPadIds[i] + " " + ms);
        root._pumpRumble();
    }

    function _pumpRumble() {
        if (root._rumbleBusy || root._rumbleQueue.length === 0)
            return;
        root._rumbleBusy = true;
        rumbleCmd.request(root._rumbleQueue.shift());
    }

    // --- pads model maintenance ---------------------------------------------

    function _syncConnectedPadIds() {
        var ids = [];
        for (var i = 0; i < root.pads.length; i++)
            ids.push(root.pads[i].id);
        root.connectedPadIds = ids;
    }

    function _padIndexById(id) {
        for (var i = 0; i < root.pads.length; i++) {
            if (root.pads[i].id === id)
                return i;
        }
        return -1;
    }

    // Replace the whole fleet from a `get-pads` JSON array
    // ([{id,index,name,grabbed}, …]). Preserves any battery info already known
    // for a still-present pad (get-pads carries no battery field).
    function _setPadsFromList(list) {
        var next = [];
        for (var i = 0; i < list.length; i++) {
            var d = list[i];
            var prev = root._padIndexById(d.id);
            next.push({
                "id": d.id,
                "index": d.index,
                "name": d.name,
                "batteryLevel": prev >= 0 ? root.pads[prev].batteryLevel : -1,
                "batteryCharging": prev >= 0 ? root.pads[prev].batteryCharging : false
            });
        }
        next.sort((a, b) => a.index - b.index);
        root.pads = next;
        root._syncConnectedPadIds();
    }

    function _padConnected(obj) {
        var next = root.pads.slice();
        var i = root._padIndexById(obj.id);
        if (i >= 0) {
            next[i] = Object.assign({}, next[i], {
                "index": obj.index,
                "name": obj.name
            });
        } else {
            next.push({
                "id": obj.id,
                "index": obj.index,
                "name": obj.name,
                "batteryLevel": -1,
                "batteryCharging": false
            });
        }
        next.sort((a, b) => a.index - b.index);
        root.pads = next;
        root._syncConnectedPadIds();
    }

    function _padIndex(obj) {
        var i = root._padIndexById(obj.id);
        if (i < 0)
            return;
        var next = root.pads.slice();
        next[i] = Object.assign({}, next[i], {
            "index": obj.index
        });
        next.sort((a, b) => a.index - b.index);
        root.pads = next;
        root._syncConnectedPadIds();
    }

    function _padBattery(obj) {
        var i = root._padIndexById(obj.id);
        if (i < 0)
            return;
        var next = root.pads.slice();
        next[i] = Object.assign({}, next[i], {
            "batteryLevel": obj.level,
            "batteryCharging": obj.charging
        });
        root.pads = next;
    }

    function _padDisconnected(id) {
        var next = [];
        for (var i = 0; i < root.pads.length; i++) {
            if (root.pads[i].id !== id)
                next.push(root.pads[i]);
        }
        root.pads = next;
        root._syncConnectedPadIds();
    }

    function refreshPads() {
        getPadsCmd.request("get-pads");
    }

    function grab() {
        inputGrab.request("grab");
    }
    function release() {
        inputRelease.request("release");
    }
    function startListening() {
        comboListener.start();
        // Seed the fleet snapshot once the subscriber is up; pad:* deltas keep
        // it live afterwards, and controller-wake re-seeds on each join.
        root.refreshPads();
    }
    function endSession() {
        endSessionProc.running = true;
    }

    SocketClient {
        id: inputGrab
    }

    SocketClient {
        id: inputRelease
    }

    // Seeds the `pads` model with the current fleet (id,index,name,grabbed) on
    // connect / wake. Battery info is layered in from pad:battery events.
    SocketClient {
        id: getPadsCmd
        onResponseReceived: line => {
            try {
                var list = JSON.parse(line);
                if (Array.isArray(list))
                    root._setPadsFromList(list);
            } catch (e) {
                console.log("InputManager: failed to parse get-pads:", e);
            }
        }
    }

    // Fire-and-forget rumble command (one request per connected pad).
    SocketClient {
        id: rumbleCmd
        // Advance the queue once each command's round-trip settles (either way),
        // so multi-pad pulses are delivered to every controller, not just the last.
        onResponseReceived: () => {
            root._rumbleBusy = false;
            root._pumpRumble();
        }
        onRequestFailed: () => {
            root._rumbleBusy = false;
            root._pumpRumble();
        }
    }

    Process {
        id: endSessionProc
        command: ["/usr/local/bin/end-game-session"]
    }

    SocketClient {
        id: comboListener
        subscribe: true
        onLineReceived: line => {
            if (line === "combo:force-quit")
                root.forceQuitRequested();
            else if (line === "combo:end-session")
                root.endSessionRequested();
            else if (line === "combo:suspend-stream")
                root.suspendStreamRequested();
            else if (line === "input-mode:mouse") {
                // Daemon right-stick->cursor hint. Post-#45 this is just ONE
                // source for mouse-mode among QML's own pointer/key events;
                // route it through the same helper so it can't fight them.
                Theme.enterMouseMode();
                root.inputModeChanged("mouse");
            } else if (line === "input-mode:controller") {
                Theme.exitMouseMode();
                root.inputModeChanged("controller");
            } else if (line === "controller-wake") {
                root.controllerWake();
                NotificationManager.info("controller", "Controller Connected");
                // Re-seed the fleet snapshot (slots/names) on every join; the
                // pad:* deltas keep it live afterwards.
                root.refreshPads();
            } else if (line === "controller-disconnected") {
                root.controllerDisconnected();
                NotificationManager.warn("controller", "Controller Disconnected");
            } else if (line.startsWith("pad:connected:")) {
                root._handlePadJson(line.substring(14), root._padConnected);
            } else if (line.startsWith("pad:index:")) {
                root._handlePadJson(line.substring(10), root._padIndex);
            } else if (line.startsWith("pad:battery:")) {
                root._handlePadJson(line.substring(12), root._padBattery);
            } else if (line.startsWith("pad:disconnected:")) {
                root._padDisconnected(line.substring(17));
            } else if (line.startsWith("intent:")) {
                root._handleIntent(line.substring(7));
            }
        }
    }

    // Parse a compact-JSON pad:* payload and dispatch it to `apply`. A bad
    // payload is logged and dropped (the model is left untouched).
    function _handlePadJson(json, apply) {
        try {
            apply(JSON.parse(json));
        } catch (e) {
            console.log("InputManager: failed to parse pad event:", e);
        }
    }

    // Map a closed-vocabulary intent name to its QML signal. `home` is the
    // global return-to-shell escape (keyboard Super+Escape / automation); the
    // rest fan out 1:1. `menu` (bare Super) is the nav drawer; `home-hold`
    // (Super+Backspace / gamepad Home-hold) is the reset.
    function _handleIntent(name) {
        switch (name) {
        case "home":
            root.intentHome();
            break;
        case "home-tap":
            root.intentHomeTap();
            break;
        case "home-hold":
            root.intentHomeHold();
            break;
        case "menu":
            root.intentMenu();
            break;
        case "nav-up":
            root.intentNav("up");
            break;
        case "nav-down":
            root.intentNav("down");
            break;
        case "nav-left":
            root.intentNav("left");
            break;
        case "nav-right":
            root.intentNav("right");
            break;
        case "select":
            root.intentSelect();
            break;
        case "back":
            root.intentBack();
            break;
        case "settings":
            root.intentSettings();
            break;
        case "power":
            root.intentPower();
            break;
        }
    }
}

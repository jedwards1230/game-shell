pragma Singleton
import Quickshell.Io
import QtQuick
import "../widgets/lib"
import "../widgets/lib/widgetConfig.js" as WidgetConfig
import "settingsPayload.js" as SettingsPayload

// Centralized settings I/O for tv-shell — the QML-side façade over
// ~/.config/tv-shell/settings.json.
//
// IMPORTANT: root MUST be Item (not QtObject). Quickshell 0.3.0 cannot host
// Process/Timer children inside a QtObject singleton, and this store needs
// Process children to do socket I/O.
//
// Ownership: the input daemon is the SOLE writer of settings.json. This store
// holds the QML-relevant keys (themeMode, controllerDebug,
// rumbleEnabled) and hands them to the daemon via `set-config`; the daemon does
// the
// read-modify-write, preserving foreign keys it owns (notably keyBindings).
// `keyBindings` here is a read-through mirror kept in sync over IPC.
//
// Binding IPC lives here (rather than in InputManager) because keyBindings is a
// setting and this keeps InputManager focused on the core input-grab/combo path.
// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: get-config, set-config, get-bindings, set-binding,
//                capture-next, capture-cancel
// Events subscribed: config:changed (live-reload — daemon broadcasts when an
//                    external writer modifies settings.json; QML re-fetches via
//                    get-config so all keys re-apply live without a restart)
// QML-owned display keys: hdrEnabled, nightLightEnabled, nightLightTemp, overscan,
//                         sleepTimerMinutes, wakeOnController, defaultSink,
//                         cecFocusOnStartup, cecFocusOnWake, cecAutoSwitchOnPowerOn,
//                         cecDefaultInput, cecDeviceNames, wallpaperPath
Item {
    id: store

    // === Persisted settings (QML-owned) ===
    property string themeMode: "dark"             // "auto" | "light" | "dark"
    property int autoThemeDarkStart: 20           // hour (0-23) "auto" flips to dark (#231)
    property int autoThemeLightStart: 7           // hour (0-23) "auto" flips to light (#231)
    property bool controllerDebug: false
    property bool rumbleEnabled: true             // gates daemon-fired rumble (#99)
    property bool reduceMotion: false             // suppress animations (#109)
    // Home-screen widget config — namespaced per widget (#249 Phase 3). The new
    // QML SSOT for per-widget enabled/order/size/prefs, replacing the flat
    // widget<Name>* keys. Shape: { <id>: {enabled, order, size, prefs:{...}} }.
    // Populated by the migrator in loadProc (legacy flat keys are folded in once,
    // then live under this subtree). Read via widget(id); write via setWidget /
    // setWidgetPref / setWidgetOrder — each persists just this whole subtree as
    // the single `widgets` key (the daemon can't merge below a top-level key), so
    // the shallow-merge replaces it wholesale without touching other keys.
    property var widgets: ({})
    property real textScale: 1.0                  // font-size multiplier: 1.0/1.15/1.3 (#110)
    property bool hdrEnabled: true               // mirrors config/hyprland.conf cm,hdr default
    property bool nightLightEnabled: false       // drives hyprsunset
    property int nightLightTemp: 4500            // color temperature in Kelvin
    property int overscan: 0                     // safe-area overscan percent (0-10)
    property string wallpaperPath: ""             // absolute path to home-screen wallpaper ("" = solid color)
    property int sleepTimerMinutes: 0            // 0 = disabled; cycle: 0/5/10/15/30/60
    property bool wakeOnController: true         // declarative preference (no suspend wiring)
    property bool autoDimEnabled: false          // auto-dim OLED protection (#143)
    property int autoDimDelayMinutes: 2          // idle minutes before dimming (#143)
    property string defaultSink: ""              // WirePlumber sink node.name (stable across reboots)
    property string audioCardProfile: ""         // "card|profile" surround profile to reapply on boot (#234)
    property bool cecFocusOnStartup: false      // claim active source when daemon starts (default off)
    property bool cecFocusOnWake: true          // claim active source on resume from sleep (default on)
    property bool cecAutoSwitchOnPowerOn: false // focus the display when the TV/AVR powers on (default off; daemon: cec::auto_switch_on_power_on)
    property int cecDefaultInput: -1            // logical address to focus (-1 = unset, i.e. claim for this box; daemon: cec::apply_focus)
    property var cecDeviceNames: ({})           // local label overrides keyed by logical address, e.g. {"0":"Living Room TV"}
    property var prewarmApps: []                 // wmClass list of apps to silently prewarm at login (#238)

    // Web-app registry (#187) — DAEMON-OWNED, read-only mirror here. Each entry:
    // { id, name, url, wmClass }. The daemon writes .desktop launchers +
    // this registry key (P1 webapp-* IPC); QML only reads/lists it (P0). Hence
    // noSave in the schema — never sent in a set-config payload.
    property var webApps: []

    // === Daemon-owned mirror (authoritative copy lives in the daemon) ===
    property var keyBindings: ({})

    // === Settings schema (drives parse / serialize / setters, #8 M6) ===
    // One row per persisted key replaces the parse / serialize / setter
    // triplication. Fields:
    //   key       — property name on `store` (also the JSON key)
    //   t         — JSON typeof guard: "string" | "number" | "boolean" | "object"
    //   validate  — optional extra predicate (v) => bool, applied on BOTH parse and
    //               setter (enum / range checks); a failing value is rejected.
    //   noSave    — true for keys this store reads but does NOT write back
    //               (daemon-owned: keyBindings). Parsed in, excluded from any
    //               set-config payload (buildSavePayload drops noSave keys).
    // Keep the per-key validators identical to the old inline guards so behavior
    // is unchanged.
    readonly property var _schema: [
        {
            key: "themeMode",
            t: "string",
            validate: v => v === "auto" || v === "light" || v === "dark"
        },
        {
            key: "autoThemeDarkStart",
            t: "number",
            validate: v => v >= 0 && v <= 23
        },
        {
            key: "autoThemeLightStart",
            t: "number",
            validate: v => v >= 0 && v <= 23
        },
        {
            key: "controllerDebug",
            t: "boolean"
        },
        {
            key: "rumbleEnabled",
            t: "boolean"
        },
        {
            key: "reduceMotion",
            t: "boolean"
        },
        {
            key: "widgets",
            t: "object"
        },
        {
            key: "textScale",
            t: "number"
        },
        {
            key: "hdrEnabled",
            t: "boolean"
        },
        {
            key: "nightLightEnabled",
            t: "boolean"
        },
        {
            key: "nightLightTemp",
            t: "number"
        },
        {
            key: "overscan",
            t: "number"
        },
        {
            key: "wallpaperPath",
            t: "string"
        },
        {
            key: "sleepTimerMinutes",
            t: "number"
        },
        {
            key: "wakeOnController",
            t: "boolean"
        },
        {
            key: "autoDimEnabled",
            t: "boolean"
        },
        {
            key: "autoDimDelayMinutes",
            t: "number"
        },
        {
            key: "defaultSink",
            t: "string"
        },
        {
            key: "audioCardProfile",
            t: "string"
        },
        {
            key: "cecFocusOnStartup",
            t: "boolean"
        },
        {
            key: "cecFocusOnWake",
            t: "boolean"
        },
        {
            key: "cecAutoSwitchOnPowerOn",
            t: "boolean"
        },
        {
            key: "cecDefaultInput",
            t: "number"
        },
        {
            key: "cecDeviceNames",
            t: "object"
        },
        {
            key: "webApps",
            t: "object",
            noSave: true,
            validate: v => Array.isArray(v)
        },
        {
            key: "prewarmApps",
            t: "object",
            validate: v => Array.isArray(v)
        },
        {
            key: "keyBindings",
            t: "object",
            noSave: true
        }
    ]

    // Does a parsed JSON value satisfy a schema row's type + validator? The
    // "object" type also rejects null (typeof null === "object"), matching the
    // old `obj.x && typeof obj.x === "object"` guards.
    function _valueOk(row, v) {
        if (row.t === "object") {
            if (!v || typeof v !== "object")
                return false;
        } else if (typeof v !== row.t) {
            return false;
        }
        return row.validate ? row.validate(v) : true;
    }

    // Emitted once each time a get-config response has been parsed, so root-level
    // consumers can (re)apply system state that the daemon doesn't itself restore
    // on boot — e.g. the audio surround card profile (#234).
    signal configLoaded

    // === Binding IPC signals ===
    signal bindingsReceived(var bindings)
    signal bindingCaptured(string button)
    signal captureCancelled

    // --- Load: read settings via the daemon's `get-config` IPC (no write) ---
    // The daemon is the sole settings.json writer; this asks it for the current
    // document as a compact single-line JSON object.
    SocketClient {
        id: loadProc
        onResponseReceived: line => {
            try {
                var obj = JSON.parse(line);
                // Schema-driven parse: apply each known key whose value passes its
                // type + validator guard. Unknown / malformed keys are left at the
                // current value (same as the old per-key `if` guards).
                for (var i = 0; i < store._schema.length; i++) {
                    var row = store._schema[i];
                    if (row.key in obj && store._valueOk(row, obj[row.key]))
                        store[row.key] = obj[row.key];
                }
                // Namespaced widget config migration (#249 Phase 3): fold the
                // legacy flat widget* keys into the widgets.<id>.* subtree on
                // first load (and fill any missing keys when the subtree already
                // exists), so accessors always see a complete, defaulted subtree.
                // When the subtree was absent we persist it once (changed=true).
                var r = WidgetConfig.migrate(obj, WidgetManifests.manifests);
                store.widgets = r.widgets;
                if (r.changed)
                    store._saveKeys(["widgets"]);
                store.configLoaded();
            } catch (e) {
                console.log("SettingsStore: failed to parse settings:", e);
            }
        }
    }

    // --- Save: hand the QML-owned keys to the daemon via `set-config`. ---
    // The daemon does the read-modify-write, preserving foreign keys (notably
    // daemon-owned keyBindings). The JSON body is built with JSON.stringify and
    // passed as argv to python (not interpolated into the source), so app/enum
    // values can never break the command string.
    SocketClient {
        id: saveProc
    }

    function load() {
        loadProc.request("get-config");
    }

    // Persist ONLY the given changed keys via `set-config`. The daemon
    // shallow-merges the payload into settings.json (read-modify-write),
    // preserving every key we don't send — including daemon-owned keyBindings and
    // any key an external editor touched concurrently. Sending the whole store on
    // every change (the old save()) round-tripped a stale snapshot over unrelated
    // keys; buildSavePayload keeps the write minimal (and drops noSave/unknown
    // keys). A no-op payload (nothing writable) skips the socket round-trip.
    function _saveKeys(keys) {
        var payload = SettingsPayload.buildSavePayload(store._schema, keys, store);
        if (Object.keys(payload).length > 0)
            saveProc.request("set-config", JSON.stringify(payload));
    }

    // Generic setter backing the trivial set+save setters. Looks up the key's
    // schema row and applies its validator (if any) as a gate, so a validated key
    // (themeMode enum, autoTheme*Start range) rejects a bad value exactly as the
    // old hand-written guards did. Returns true if the value was accepted.
    function _setKey(key, value) {
        for (var i = 0; i < store._schema.length; i++) {
            var row = store._schema[i];
            if (row.key === key) {
                if (row.validate && !row.validate(value))
                    return false;
                store[key] = value;
                store._saveKeys([key]);
                return true;
            }
        }
        return false;
    }

    function setThemeMode(mode) {
        store._setKey("themeMode", mode);
    }

    function setAutoThemeDarkStart(hour) {
        store._setKey("autoThemeDarkStart", hour);
    }

    function setAutoThemeLightStart(hour) {
        store._setKey("autoThemeLightStart", hour);
    }

    function setControllerDebug(enabled) {
        store._setKey("controllerDebug", enabled);
    }

    function setRumbleEnabled(enabled) {
        store._setKey("rumbleEnabled", enabled);
    }

    function setReduceMotion(enabled) {
        store._setKey("reduceMotion", enabled);
    }

    // === Namespaced widget config accessors (#249 Phase 3) ===
    // widget(id) → the per-widget subtree, ALWAYS fully defaulted (merged over the
    // manifest defaults) so callers get {enabled, order, size, prefs:{...}} even
    // before/without an on-disk value. Reactive: it reads store.widgets, so a
    // binding using widget(id) re-evaluates whenever store.widgets reassigns.
    function widget(id) {
        var base = WidgetConfig.defaultSubtree(WidgetManifests.manifests)[id];
        if (!base)
            base = {
                "enabled": true,
                "order": 0,
                "size": "medium",
                "prefs": {}
            };
        var w = (store.widgets && store.widgets[id]) ? store.widgets[id] : null;
        if (!w)
            return base;
        var out = {
            "enabled": (typeof w.enabled === "boolean") ? w.enabled : base.enabled,
            "order": (typeof w.order === "number") ? w.order : base.order,
            "size": (typeof w.size === "string") ? w.size : base.size,
            "prefs": {}
        };
        for (var bk in base.prefs)
            out.prefs[bk] = base.prefs[bk];
        if (w.prefs && typeof w.prefs === "object") {
            for (var pk in w.prefs)
                out.prefs[pk] = w.prefs[pk];
        }
        return out;
    }

    // Default subtree for the setters' "materialise a missing entry" fallback.
    function _widgetDefaults() {
        return WidgetConfig.defaultSubtree(WidgetManifests.manifests);
    }

    // The widget-config setters delegate the immutable update to widgetConfig.js
    // (one tested place — see its "Immutable per-widget config mutators" block),
    // then REASSIGN store.widgets to the returned NEW object. Reassignment is what
    // fires widgetsChanged so every binding reading widget(id) re-evaluates; an
    // in-place mutation would notify nothing. tst_widgetreact.qml guards this.

    // Set a top-level per-widget key (enabled / order / size), then persist. Only
    // the `widgets` subtree is sent — the daemon can't merge below a top-level
    // key, so the whole (fully-defaulted) subtree goes as one key; every non-widget
    // key is left untouched.
    function setWidget(id, key, value) {
        store.widgets = WidgetConfig.setWidget(store.widgets, id, key, value, store._widgetDefaults());
        store._saveKeys(["widgets"]);
    }

    // Set a per-widget pref (under widgets.<id>.prefs), then persist.
    function setWidgetPref(id, prefKey, value) {
        store.widgets = WidgetConfig.setPref(store.widgets, id, prefKey, value, store._widgetDefaults());
        store._saveKeys(["widgets"]);
    }

    // Reorder: assign widgets.<id>.order = position for each id in orderedIds,
    // then persist once. Used by the Widgets page reorder UI.
    function setWidgetOrder(orderedIds) {
        store.widgets = WidgetConfig.setOrder(store.widgets, orderedIds, store._widgetDefaults());
        store._saveKeys(["widgets"]);
    }

    function setTextScale(scale) {
        store._setKey("textScale", scale);
    }

    function setHdrEnabled(enabled) {
        store._setKey("hdrEnabled", enabled);
    }

    function setNightLightEnabled(enabled) {
        store._setKey("nightLightEnabled", enabled);
    }

    function setNightLightTemp(temp) {
        store._setKey("nightLightTemp", temp);
    }

    function setOverscan(pct) {
        store._setKey("overscan", pct);
    }

    function setWallpaperPath(path) {
        store._setKey("wallpaperPath", path);
    }

    function setSleepTimerMinutes(m) {
        store._setKey("sleepTimerMinutes", m);
    }

    function setWakeOnController(enabled) {
        store._setKey("wakeOnController", enabled);
    }

    function setAutoDimEnabled(enabled) {
        store._setKey("autoDimEnabled", enabled);
    }

    function setAutoDimDelayMinutes(minutes) {
        store._setKey("autoDimDelayMinutes", minutes);
    }

    function setDefaultSink(name) {
        store._setKey("defaultSink", name);
    }

    // Persist the chosen surround card profile as "card|profile" so it can be
    // re-applied on boot (PipeWire otherwise reverts to the stereo default).
    function setAudioCardProfile(value) {
        store._setKey("audioCardProfile", value);
    }

    function setCecFocusOnStartup(enabled) {
        store._setKey("cecFocusOnStartup", enabled);
    }

    function setCecFocusOnWake(enabled) {
        store._setKey("cecFocusOnWake", enabled);
    }

    function setCecAutoSwitchOnPowerOn(enabled) {
        store._setKey("cecAutoSwitchOnPowerOn", enabled);
    }

    function setCecDefaultInput(addr) {
        store._setKey("cecDefaultInput", addr);
    }

    // Set or clear a local name override for a CEC logical address. An empty/blank
    // name removes the override (falls back to the derived friendly name). The
    // value is stored keyed by the address stringified, matching the on-disk shape.
    function setCecDeviceName(addr, name) {
        var key = String(addr);
        var copy = Object.assign({}, cecDeviceNames);
        if (name && name.length > 0)
            copy[key] = name;
        else
            delete copy[key];
        cecDeviceNames = copy;
        store._saveKeys(["cecDeviceNames"]);
    }

    // Is wmClass marked for login prewarm? (#238) Exact string membership; an
    // empty/undefined list or blank wmClass → false. Normalizes nothing.
    function isPrewarm(wmClass) {
        if (!wmClass || !store.prewarmApps)
            return false;
        return store.prewarmApps.indexOf(wmClass) >= 0;
    }

    // Add or remove a wmClass from the prewarm list, then persist. Builds a NEW
    // array (copy-then-assign, mirroring setCecDeviceName above) so the
    // reassignment fires prewarmAppsChanged — an in-place push would notify
    // nothing. A blank wmClass is a no-op: an app with no StartupWMClass can't be
    // targeted for prewarm.
    function setPrewarm(wmClass, enabled) {
        if (!wmClass || wmClass === "")
            return;
        var next = (store.prewarmApps || []).slice();
        var idx = next.indexOf(wmClass);
        if (enabled) {
            if (idx < 0)
                next.push(wmClass);
        } else {
            next = next.filter(function (c) {
                return c !== wmClass;
            });
        }
        store.prewarmApps = next;
        store._saveKeys(["prewarmApps"]);
    }

    // === Binding IPC (respects TV_SHELL_SOCK; no hardcoded socket path) ===
    function getBindings() {
        getBindingsProc.request("get-bindings");
    }

    function setBinding(action, button) {
        setBindingProc.request("set-binding " + action + " " + button);
    }

    function captureNext() {
        captureProc.request("capture-next");
    }

    function cancelCapture() {
        cancelCaptureProc.request("capture-cancel");
    }

    // All daemon IPC goes over native Quickshell sockets (SocketClient, #97).
    // The set-config body (a JSON string) is passed as the request body so
    // arbitrary JSON never needs shell/argv quoting.

    SocketClient {
        id: getBindingsProc
        onResponseReceived: line => {
            try {
                var b = JSON.parse(line);
                store.keyBindings = b;
                store.bindingsReceived(b);
            } catch (e) {
                console.log("SettingsStore: failed to parse bindings:", e);
            }
        }
    }

    SocketClient {
        id: setBindingProc
        // command issued dynamically in setBinding(); re-fetch so the mirror
        // stays current once the daemon acknowledges.
        onResponseReceived: response => store.getBindings()
        onRequestFailed: store.getBindings()
    }

    SocketClient {
        id: captureProc
        onResponseReceived: line => {
            if (line.startsWith("captured:"))
                store.bindingCaptured(line.substring(9));
            else if (line === "timeout" || line === "cancelled")
                store.captureCancelled();
        }
    }

    SocketClient {
        id: cancelCaptureProc
    }

    // --- Live-reload: subscribe to config:changed events from the daemon. ---
    // The daemon inotify-watches settings.json and broadcasts config:changed
    // when an external writer (SSH/Ansible/web UI) modifies it. The daemon
    // suppresses its own set-config/set-binding writes via a self-write
    // generation guard, so this fires only for foreign edits. We re-fetch the
    // full document via get-config (the same path as startup load()), so every
    // QML-owned key re-applies live and keyBindings propagates to consumers.
    SocketClient {
        id: configWatch
        subscribe: true
        onLineReceived: line => {
            if (line === "config:changed")
                store.load();
        }
    }

    // Re-apply persisted default audio sink once at shell startup, after
    // settings have been loaded from the daemon. The AudioSettings page only
    // re-applies when that page is opened; this ensures the correct sink is
    // active even if the user never visits Audio Settings. (#131)
    //
    // The sink name is passed via the TV_SHELL_SINK environment variable so
    // it cannot inject shell commands regardless of its content. (#131 injection fix)
    Process {
        id: startupSinkApply
        environment: ({
                "TV_SHELL_SINK": store.defaultSink
            })
        command: ["bash", "-c", "[ -z \"$TV_SHELL_SINK\" ] && exit 0; " + "if command -v jq >/dev/null 2>&1; then " + "  id=$(pw-dump 2>/dev/null | jq -r --arg n \"$TV_SHELL_SINK\" " + "    '.[] | select(.info.props[\"node.name\"]==$n) | .id' | head -1); " + "else " + "  id=$(wpctl status 2>/dev/null | grep -F \"$TV_SHELL_SINK\" | grep -oE '[0-9]+' | head -1); " + "fi; " + "[ -n \"$id\" ] && wpctl set-default \"$id\" || true"]
    }

    onDefaultSinkChanged: {
        if (defaultSink !== "" && !startupSinkApply.running)
            startupSinkApply.running = true;
    }

    // Re-apply persisted surround card profile once at boot after settings load.
    // PipeWire reverts non-default card profiles (e.g. Digital Surround 5.1) to
    // stereo across reboots. The profile string is "card|profile"; card and profile
    // are passed via env so their content can never inject shell commands. (#234)
    property bool _audioProfileApplied: false

    Process {
        id: startupCardProfileApply
        property string cardName: ""
        property string profileName: ""
        environment: ({
                "GS_CARD": cardName,
                "GS_PROFILE": profileName
            })
        command: ["bash", "-c", "[ -z \"$GS_CARD\" ] && exit 0; " + "pactl set-card-profile \"$GS_CARD\" \"$GS_PROFILE\" || exit 0; " + "sink=$(pactl list sinks | awk -v c=\"$GS_CARD\" '/^[ \\t]*Name:/{n=$2} /device\\.name = /{ gsub(/\"/,\"\"); if($3==c) print n }' | head -1); " + "[ -n \"$sink\" ] && pactl set-default-sink \"$sink\" || true"]
    }

    onConfigLoaded: {
        if (!store._audioProfileApplied) {
            var v = store.audioCardProfile;
            var sep = v ? v.indexOf("|") : -1;
            if (sep >= 0) {
                store._audioProfileApplied = true;
                startupCardProfileApply.cardName = v.substring(0, sep);
                startupCardProfileApply.profileName = v.substring(sep + 1);
                startupCardProfileApply.running = true;
            }
        }
    }

    Component.onCompleted: {
        load();
        configWatch.start();
    }
}

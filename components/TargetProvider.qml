import QtQuick

// Base contract for a streaming backend (Moonlight, Sunshine-direct, etc.).
//
// This is a duck-typed "interface": consumers (HomeScreen, StreamManager,
// SettingsPanel) talk to whatever StreamProviders.active points at via these
// members. The base itself is a valid NO-STREAMING provider — an empty
// targets list, no settings section, and no-op launch/quit — so selecting it
// turns game-shell into a pure app launcher with no null-guards at call sites.
//
// `id` is reserved in QML, so the backend identity property is `providerId`.
Item {
    id: provider

    property string providerId: "none"
    property string displayName: ""

    // Available streaming hosts: [{name, host, app, resolution, fps, hdr, ...}]
    property var targets: []
    // Discovered apps per host: { "host": ["App1", "App2", ...] }
    property var hostApps: ({})
    property bool discovering: false

    // QML Component for the backend's settings panel (null = no section).
    property Component settingsComponent: null

    // Refresh the targets list (e.g. re-read targets.json).
    function loadTargets() {
    }

    // Discover available apps for every known host (populates hostApps).
    function discoverApps() {
    }

    // Build the argv the launcher Process should run for a given target.
    // StreamManager owns the generic launch state machine; the provider only
    // supplies backend-specific arguments. Empty array = nothing to launch.
    function buildLaunchArgs(target) {
        return [];
    }

    // Build the argv that quits an active session for a target. Empty = no-op.
    function quitArgs(target) {
        return [];
    }

    // Initiate pairing for a host.
    function pair(host) {
    }

    // Refresh online/paired status for known hosts.
    function checkStatus() {
    }
}

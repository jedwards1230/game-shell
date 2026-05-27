pragma Singleton
import QtQuick

Item {
    id: root
    readonly property int maxEntries: 50
    property var entries: []
    property int count: 0
    signal errorAdded(var entry)

    property int _nextId: 1
    property string _currentTarget: ""

    function log(source, message, details, target) {
        var entry = Object.create(null);
        entry.id = _nextId++;
        entry.timestamp = new Date();
        entry.source = source;
        entry.message = message;
        entry.details = details || "";
        entry.target = target !== undefined ? target : _currentTarget;

        var list = root.entries.slice()
        list.push(entry)
        if (list.length > root.maxEntries)
            list = list.slice(list.length - root.maxEntries)
        root.entries = list
        root.count = list.length
        errorAdded(entry)

        NotificationManager.notify(
            _sourceLabel(source) + " Error",
            message,
            { level: "error", duration: 0, source: source }
        )
    }

    function clear() {
        root.entries = []
        root.count = 0
    }

    function setCurrentTarget(t) { _currentTarget = t }

    function _sourceLabel(source) {
        if (source === "moonlight") return "Stream"
        if (source === "app") return "App"
        if (source === "input") return "Input"
        if (source === "av") return "AV System"
        return "System"
    }
}

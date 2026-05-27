pragma Singleton
import QtQuick

Item {
    function matchesApp(app, client) {
        let cls = (client["class"] || "").toLowerCase()
        let initCls = (client["initialClass"] || "").toLowerCase()
        if (cls === "" && initCls === "") return false

        // 1. StartupWMClass match (most reliable)
        let wmClass = (app.wmClass || "").toLowerCase()
        if (wmClass !== "") {
            if (cls === wmClass || initCls === wmClass) return true
            let normWm = _normalize(wmClass)
            if (_normalize(cls) === normWm || _normalize(initCls) === normWm) return true
        }

        // 2. Exec basename match
        let execBase = _execBasename(app.exec || "")
        if (execBase !== "") {
            if (cls === execBase || initCls === execBase) return true
            if (_normalize(cls) === _normalize(execBase) || _normalize(initCls) === _normalize(execBase)) return true
        }

        // 3. App name match (exact only)
        let appName = (app.name || "").toLowerCase()
        if (appName !== "" && (cls === appName || initCls === appName)) return true

        // 4. Substring: class within exec or exec within class
        if (execBase !== "" && cls !== "") {
            if (execBase.indexOf(cls) >= 0 || cls.indexOf(execBase) >= 0) return true
        }

        return false
    }

    function _execBasename(exec) {
        let cmd = exec.split(/\s/)[0]
        let base = cmd.split("/").pop()
        return base.toLowerCase()
    }

    function _normalize(s) {
        return s.toLowerCase().replace(/[-_\.]/g, "")
    }
}

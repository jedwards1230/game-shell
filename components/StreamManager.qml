import Quickshell.Io
import QtQuick

Item {
    id: root

    property var currentTarget: null
    property int crashCount: 0
    property string shellState: ""

    signal streamStarted()
    signal streamEnded()
    signal streamCrashed(int attempts)
    signal streamFailed(string message)
    signal requestOverlayShow(string msg)
    signal requestOverlayHide()
    signal requestInputRelease()
    signal requestInputGrab()

    function launch(target) {
        reconnectTimer.stop()
        errorDismissTimer.stop()
        currentTarget = target
        crashCount = 0
        requestOverlayShow("Launching " + (target.app || target.name) + "...")
        _launchMoonlight()
    }

    function stop() {
        reconnectTimer.stop()
        errorDismissTimer.stop()
        moonlight.running = false
    }

    function forceKill() {
        stop()
        forceKillProc.running = true
    }

    function _launchMoonlight() {
        let args = ["moonlight", "stream", currentTarget.host, currentTarget.app]
        if (currentTarget.resolution === "3840x2160") args.push("--4k")
        if (currentTarget.fps) { args.push("--fps"); args.push(String(currentTarget.fps)) }
        if (currentTarget.hdr) args.push("--hdr")
        if (currentTarget.codec) { args.push("--video-codec"); args.push(currentTarget.codec) }
        args.push("--display-mode", "fullscreen")
        args.push("--no-quit-after")
        args.push("--no-frame-pacing")
        moonlight.command = args
        requestInputRelease()
        streamStarted()
        moonlight.running = true
    }

    Process {
        id: moonlight
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0) {
                root.requestOverlayHide()
                root.requestInputGrab()
                root.streamEnded()
            } else {
                root.crashCount++
                if (root.crashCount < 5) {
                    root.requestOverlayShow("Reconnecting... (" + root.crashCount + "/5)")
                    root.streamCrashed(root.crashCount)
                    reconnectTimer.start()
                } else {
                    let msg = "Stream failed after 5 attempts"
                    root.requestOverlayShow(msg)
                    errorDismissTimer.start()
                    root.requestInputGrab()
                    root.streamFailed(msg)
                }
            }
        }
    }

    Process {
        id: forceKillProc
        command: ["bash", "-c", "pkill -f moonlight; pkill -f steam; true"]
    }

    Timer {
        id: reconnectTimer
        interval: 2000
        onTriggered: root._launchMoonlight()
    }

    Timer {
        id: errorDismissTimer
        interval: 5000
        onTriggered: root.requestOverlayHide()
    }

    Timer {
        id: crashResetTimer
        interval: 300000
        running: root.shellState === "streaming"
        onTriggered: { root.crashCount = 0 }
    }
}

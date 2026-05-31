import Quickshell
import Quickshell.Io
import QtQuick

// Native Quickshell Unix-socket client for the input daemon (#97).
//
// Replaces the ~29 `python3 -c "import socket…"` shims that previously spoke
// the daemon's newline-delimited wire protocol (see docs/IPC_PROTOCOL.md) by
// spawning a python process per call. This wraps `Quickshell.Io.Socket`
// directly — no subprocess, no python dependency — speaking the *unchanged*
// wire protocol (one command per line, newline-terminated; the daemon holds the
// connection open after replying).
//
// Two usage modes, selected by `subscribe`:
//
//   • Request/response (subscribe: false, the default) — connect, send one
//     command, read the FIRST reply line, emit responseReceived(line), then
//     disconnect. The daemon keeps the connection open after replying, so we
//     stop at the first newline rather than waiting for EOF. Call request(cmd)
//     or request(cmd, body) (body is appended after a space, matching the old
//     `_ipcArg` argv form — JSON bodies pass through verbatim, no shell quoting).
//
//   • Subscribe stream (subscribe: true) — connect, send `subscribe`, and emit
//     lineReceived(line) for every event line for the lifetime of the
//     connection. Auto-reconnects after `reconnectMs` if the connection drops
//     (mirroring the old per-listener reconnect timers). Start with start();
//     stop with stop().
//
// Socket path: GAME_SHELL_SOCK if set, else $XDG_RUNTIME_DIR/game-shell-input.sock
// (== /run/user/$UID/…), matching the python shims' resolution.
Item {
    id: client

    // --- Configuration ---

    // true  → persistent `subscribe` stream (use start()/stop(), lineReceived)
    // false → one-shot request/response (use request(), responseReceived)
    property bool subscribe: false

    // The command word sent for a subscribe stream (always "subscribe" in
    // practice; exposed so a future stream command could reuse this).
    property string subscribeCommand: "subscribe"

    // Reconnect delay (ms) for subscribe streams after a dropped connection.
    property int reconnectMs: 2000

    // --- Signals ---

    // Request/response: the first reply line (trailing newline stripped).
    signal responseReceived(string response)
    // Request/response: the underlying socket closed without yielding a reply
    // line (daemon down / connect failure). responseReceived never fired.
    signal requestFailed

    // Subscribe stream: one event line (trailing newline stripped).
    signal lineReceived(string line)

    // --- Internal state ---
    property bool _running: false      // subscribe: should stay connected
    property bool _gotResponse: false  // request: a reply line was delivered
    property string _pendingCommand: ""
    property bool _reconnecting: false  // request: closing to replace an in-flight request, not a failure

    function _socketPath() {
        let override = Quickshell.env("GAME_SHELL_SOCK");
        if (override && override !== "")
            return override;
        let runtime = Quickshell.env("XDG_RUNTIME_DIR");
        if (runtime && runtime !== "")
            return runtime + "/game-shell-input.sock";
        // Last-ditch fallback; XDG_RUNTIME_DIR is always set in a real session.
        return "/run/user/1000/game-shell-input.sock";
    }

    // --- Request/response API ---
    // request(cmd)         → sends "cmd\n"
    // request(cmd, body)   → sends "cmd body\n" (body verbatim, e.g. a JSON arg)
    function request(cmd, body) {
        if (client.subscribe) {
            console.log("SocketClient: request() called on a subscribe client");
            return;
        }
        let line = (body !== undefined && body !== null && String(body).length > 0) ? (cmd + " " + body) : cmd;
        client._pendingCommand = line;
        client._gotResponse = false;
        // Reconnect cleanly if a previous request socket is still open. Flag the
        // close as intentional so the disconnect handler does NOT report it as a
        // failure for this new request (it would otherwise emit a spurious
        // requestFailed() before the reconnect sends _pendingCommand).
        if (sock.connected) {
            client._reconnecting = true;
            sock.connected = false;
        }
        sock.connected = true;
    }

    // --- Subscribe API ---
    function start() {
        if (!client.subscribe) {
            console.log("SocketClient: start() called on a request client");
            return;
        }
        client._running = true;
        if (!sock.connected)
            sock.connected = true;
    }

    function stop() {
        client._running = false;
        reconnectTimer.stop();
        sock.connected = false;
    }

    Socket {
        id: sock
        path: client._socketPath()

        onConnectionStateChanged: {
            if (connected) {
                // On connect, send the opening command:
                //  • subscribe stream → the subscribe verb
                //  • request/response → the queued command
                if (client.subscribe) {
                    write(client.subscribeCommand + "\n");
                    flush();
                } else if (client._pendingCommand !== "") {
                    write(client._pendingCommand + "\n");
                    flush();
                    client._pendingCommand = "";
                }
            } else {
                // Disconnected.
                if (client.subscribe) {
                    if (client._running)
                        reconnectTimer.restart();
                } else if (client._reconnecting) {
                    // Intentional close to replace an in-flight request — the
                    // immediately-following reconnect will send the new command.
                    client._reconnecting = false;
                } else if (!client._gotResponse) {
                    // Request socket closed before any reply line.
                    client.requestFailed();
                }
            }
        }

        parser: SplitParser {
            onRead: line => {
                if (client.subscribe) {
                    client.lineReceived(line);
                    return;
                }
                // Request/response: deliver only the FIRST reply line, then close.
                if (client._gotResponse)
                    return;
                client._gotResponse = true;
                client.responseReceived(line);
                sock.connected = false;
            }
        }
    }

    Timer {
        id: reconnectTimer
        interval: client.reconnectMs
        onTriggered: {
            if (client.subscribe && client._running)
                sock.connected = true;
        }
    }
}

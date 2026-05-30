//! IPC wire protocol: parsing commands and rendering responses/events.
//!
//! The wire format is **newline-delimited bare UTF-8 text** in both directions
//! (see `docs/IPC_PROTOCOL.md`), NOT JSON — only the `get-bindings` *response*
//! body is a compact JSON object. The QML client talks to this exact format,
//! so every string here is byte-for-byte compatible with `gamepad-input.py`.
//!
//! `Command`/`Event` are typed enums so the daemon's `match` arms are
//! compiler-checked exhaustive; the (de)serialization to/from legacy text lives
//! here rather than via `#[serde]` (serde-tagged JSON would change the wire and
//! break QML).

use std::fmt;

/// A command parsed from one inbound line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Grab,
    Release,
    Status,
    Subscribe,
    GetBindings,
    SetBinding {
        action: String,
        button: String,
    },
    /// `set-binding` with the wrong number of arguments.
    SetBindingUsage,
    CaptureNext,
    CaptureCancel,
    /// `get-pads` — return the gamepad fleet as a compact JSON array, one object
    /// per connected pad (`{id,index,name,grabbed}`). Stable player indices
    /// (#101). Served by the input runtime (fleet state), so it round-trips the
    /// control channel like `status`.
    GetPads,
    /// `intent <name>` — inject a shell intent into the broadcast bus. `<name>`
    /// is validated against the closed vocabulary by the input runtime: a valid
    /// name re-broadcasts as `intent:<name>` and replies `ok`; an unknown name
    /// replies `error:unknown intent '<name>'`. This is the headless control
    /// surface for keyboard global-escape and automation (see
    /// `docs/IPC_PROTOCOL.md`). Pure broadcast — touches no device.
    Intent(String),
    /// `intent` with a missing/empty `<name>` body.
    IntentUsage,
    /// Scan installed `.desktop` apps; reply is a compact JSON array.
    /// Stateless (no input-runtime round-trip).
    ListApps,
    /// Return the full settings document as a compact JSON object.
    GetConfig,
    /// Merge a compact-JSON object of settings updates (read-modify-write,
    /// preserving foreign keys). The body is the raw JSON text after the
    /// command word.
    SetConfig(String),
    /// `set-config` with a missing/empty body.
    SetConfigUsage,
    /// Record an app launch into recents.json. The body is the raw JSON text
    /// (a `{name,exec,comment}` object) after the command word.
    RecordLaunch(String),
    /// `record-launch` with a missing/empty body.
    RecordLaunchUsage,
    /// Return recent launches as a compact JSON array.
    GetRecents,

    // --- Phase 3: Bluetooth (bluer / BlueZ) ---
    /// Adapter power state -> `bt:on` / `bt:off` / `error:*`.
    BtPowerStatus,
    /// Power the default adapter on.
    BtPowerOn,
    /// Power the default adapter off.
    BtPowerOff,
    /// Start discovery; results arrive asynchronously as `bt:device` events.
    BtScanOn,
    /// Stop discovery.
    BtScanOff,
    /// List known devices as a compact JSON array.
    BtList,
    /// Connect to a device by MAC address.
    BtConnect(String),
    /// Disconnect a device by MAC address.
    BtDisconnect(String),
    /// Pair a device by MAC address (just-works via BlueZ default agent).
    BtPair(String),
    /// Trust a device by MAC address.
    BtTrust(String),
    /// A `bt-connect`/`bt-disconnect`/`bt-pair`/`bt-trust` with a missing MAC.
    /// `which` is the bare command word (e.g. `bt-connect`) for the usage line.
    BtMacUsage(&'static str),

    // --- Phase 3: Network READ (zbus / NetworkManager) ---
    /// Connectivity + primary connection as a compact JSON object.
    NetStatus,
    /// Visible Wi-Fi access points as a compact JSON array.
    NetWifiList,
    /// Trigger a Wi-Fi rescan (`RequestScan`).
    NetWifiRescan,

    // --- Phase 3: Power / idle (zbus / logind + UPower) ---
    /// Whether the system can suspend -> `yes` / `no` / `error:*`.
    PowerCanSuspend,
    /// Suspend the system (logind `Suspend(false)`).
    PowerSuspend,
    /// Battery state as a compact JSON object (`{"present":false}` on a desktop).
    PowerBattery,

    // --- Phase 4: Hyprland (direct IPC sockets) ---
    /// Active window as a compact JSON object `{class,title,address}` (`{}` if
    /// none). Replaces the `hyprctl`-based active-window read in QML.
    HyprActive,
    /// All Hyprland clients as a compact JSON array (mirrors `hyprctl clients -j`:
    /// at least `class,title,address,workspace`). Replaces the QML
    /// `hyprctl clients -j` shell-out.
    HyprClients,

    // --- Phase 4: Sunshine session detection (reqwest) ---
    /// `sunshine-status <host> <port>` -> compact JSON object
    /// `{online,paired,currentApp,httpsPort}` parsed from Sunshine's
    /// `/serverinfo` response. The body is `<host> <port>` (two whitespace-
    /// separated tokens) after the command word. Stateless (cross-platform).
    SunshineStatus {
        host: String,
        port: String,
    },
    /// `sunshine-status` with a missing/incomplete `<host> <port>` body.
    SunshineStatusUsage,

    /// Anything unrecognized -> the daemon replies `unknown`.
    Unknown,
}

/// The closed vocabulary of shell intents accepted by the `intent <name>`
/// command and re-broadcast as `intent:<name>` events. Single source of truth
/// shared by the input runtime's validator and the IPC tests.
///
/// Semantics (mapped to shell actions by QML):
/// - `home` — global return-to-shell escape (keyboard Super, automation);
///   distinct from the gamepad neutrals below.
/// - `home-tap` / `home-hold` — gamepad Home neutrals (QML routes tap->menu,
///   hold->home/reset from the focus it owns).
/// - `menu` — nav-drawer toggle.
/// - `nav-up` / `nav-down` / `nav-left` / `nav-right` — directional navigation.
/// - `select` / `back` — confirm / cancel.
/// - `settings` — open settings.
/// - `power` — power menu.
pub const INTENT_VOCAB: &[&str] = &[
    "home",
    "home-tap",
    "home-hold",
    "menu",
    "nav-up",
    "nav-down",
    "nav-left",
    "nav-right",
    "select",
    "back",
    "settings",
    "power",
];

/// True if `name` is in the closed intent vocabulary.
pub fn is_known_intent(name: &str) -> bool {
    INTENT_VOCAB.contains(&name)
}

/// If `cmd` is `word` (exact) or `word` followed by whitespace, return the
/// trimmed remainder (the body). `Some("")` means the bare command with no body;
/// `None` means `cmd` isn't this command at all (e.g. `set-configX`).
fn command_body<'a>(cmd: &'a str, word: &str) -> Option<&'a str> {
    let rest = cmd.strip_prefix(word)?;
    if rest.is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

impl Command {
    /// Parse one line (the trailing newline is already stripped by the codec).
    /// Surrounding whitespace is trimmed to mirror Python's `data.decode().strip()`.
    pub fn parse(line: &str) -> Command {
        let cmd = line.trim();
        match cmd {
            "grab" => Command::Grab,
            "release" => Command::Release,
            "status" => Command::Status,
            "subscribe" => Command::Subscribe,
            "get-bindings" => Command::GetBindings,
            "capture-next" => Command::CaptureNext,
            "capture-cancel" => Command::CaptureCancel,
            "get-pads" => Command::GetPads,
            "list-apps" => Command::ListApps,
            "get-config" => Command::GetConfig,
            "get-recents" => Command::GetRecents,
            // Phase 3 bare commands (no body).
            "bt-power-status" => Command::BtPowerStatus,
            "bt-power-on" => Command::BtPowerOn,
            "bt-power-off" => Command::BtPowerOff,
            "bt-scan-on" => Command::BtScanOn,
            "bt-scan-off" => Command::BtScanOff,
            "bt-list" => Command::BtList,
            "net-status" => Command::NetStatus,
            "net-wifi-list" => Command::NetWifiList,
            "net-wifi-rescan" => Command::NetWifiRescan,
            "power-can-suspend" => Command::PowerCanSuspend,
            "power-suspend" => Command::PowerSuspend,
            "power-battery" => Command::PowerBattery,
            // Phase 4 bare commands (no body).
            "hypr-active" => Command::HyprActive,
            "hypr-clients" => Command::HyprClients,
            _ => {
                // `set-config <json>` / `record-launch <json>`: the rest of the
                // line is a compact single-line JSON body. The command word must
                // be followed by whitespace (or be bare); a bare command with no
                // body is a usage error. `command_body` enforces the word
                // boundary so e.g. `set-configX` is not mistaken for set-config.
                if let Some(body) = command_body(cmd, "set-config") {
                    return if body.is_empty() {
                        Command::SetConfigUsage
                    } else {
                        Command::SetConfig(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "record-launch") {
                    return if body.is_empty() {
                        Command::RecordLaunchUsage
                    } else {
                        Command::RecordLaunch(body.to_string())
                    };
                }
                // `intent <name>`: a single intent-name token (whitespace-
                // trimmed). A missing body is a usage error. `command_body`
                // enforces the word boundary so e.g. `intentX` is not mistaken
                // for `intent`. The closed-vocabulary check happens in the
                // input runtime, not here (parsing stays validation-free).
                if let Some(body) = command_body(cmd, "intent") {
                    return if body.is_empty() {
                        Command::IntentUsage
                    } else {
                        Command::Intent(body.to_string())
                    };
                }
                // Phase 3 MAC-argument commands: `bt-connect <mac>` etc. The body
                // is a single MAC token (whitespace-trimmed); a missing body is a
                // usage error. `command_body` enforces the word boundary so e.g.
                // `bt-connectX` is not mistaken for `bt-connect`. Order matters:
                // `bt-connect` is a prefix of nothing else here, but check the
                // longer-or-equal words explicitly to avoid surprises.
                for word in ["bt-connect", "bt-disconnect", "bt-pair", "bt-trust"] {
                    if let Some(body) = command_body(cmd, word) {
                        if body.is_empty() {
                            return Command::BtMacUsage(word);
                        }
                        let mac = body.to_string();
                        return match word {
                            "bt-connect" => Command::BtConnect(mac),
                            "bt-disconnect" => Command::BtDisconnect(mac),
                            "bt-pair" => Command::BtPair(mac),
                            "bt-trust" => Command::BtTrust(mac),
                            _ => unreachable!("word came from the literal list above"),
                        };
                    }
                }
                // Phase 4 `sunshine-status <host> <port>`: the body is two
                // whitespace-separated tokens. A missing/incomplete body is a
                // usage error. `command_body` enforces the word boundary so
                // e.g. `sunshine-statusX` is not mistaken for the command.
                if let Some(body) = command_body(cmd, "sunshine-status") {
                    let mut toks = body.split_whitespace();
                    return match (toks.next(), toks.next()) {
                        (Some(host), Some(port)) => Command::SunshineStatus {
                            host: host.to_string(),
                            port: port.to_string(),
                        },
                        _ => Command::SunshineStatusUsage,
                    };
                }
                // Python keys `set-binding` off the `"set-binding "` prefix
                // (with trailing space), so a bare `set-binding` is `unknown`.
                if let Some(rest) = cmd.strip_prefix("set-binding ") {
                    // Mirror Python `cmd.split(None, 2)`: at most two splits, so
                    // the button is everything after the action (e.g.
                    // "select BTN_SOUTH EXTRA" -> button "BTN_SOUTH EXTRA",
                    // which then fails as an invalid button — matching Python,
                    // not a usage error).
                    match rest.trim_start().split_once(char::is_whitespace) {
                        Some((action, button)) => {
                            let button = button.trim_start();
                            if action.is_empty() || button.is_empty() {
                                return Command::SetBindingUsage;
                            }
                            return Command::SetBinding {
                                action: action.to_string(),
                                button: button.to_string(),
                            };
                        }
                        // Only one token after the prefix -> wrong arg count.
                        None => return Command::SetBindingUsage,
                    }
                }
                Command::Unknown
            }
        }
    }
}

/// Events streamed to `subscribe` clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    ControllerWake,
    ControllerDisconnected,
    /// A physical pad joined the fleet and was assigned a stable player slot
    /// (#101). Payload is a compact JSON object `{id,index,name}`: `id` is the
    /// stable wire id, `index` the player slot (0 = P1), `name` the device name.
    /// Wire: `pad:connected:<json>`. Complements `controller-wake` (the
    /// single-pad legacy signal still fires for the first pad).
    PadConnected(String),
    /// A physical pad left the fleet; its slot is freed for reuse. Payload is the
    /// pad's stable wire id. Wire: `pad:disconnected:<id>`. Complements
    /// `controller-disconnected`.
    PadDisconnected(String),
    HomePress,
    ComboHomeHold,
    ComboEndSession,
    ComboForceQuit,
    ComboSuspendStream,
    /// A shell intent broadcast (`intent:<name>`), re-emitted from an accepted
    /// `intent <name>` command. `<name>` is one of the closed vocabulary
    /// (see `docs/IPC_PROTOCOL.md`). The control surface for keyboard
    /// global-escape and automation; QML maps each name to a shell action.
    Intent(String),
    InputMode(InputMode),
    /// Space-and-plus joined held controller inputs (may be empty).
    Buttons(String),

    // --- Phase 3 events ---
    /// Bluetooth adapter power changed (`bt:powered:on` / `bt:powered:off`).
    BtPowered(bool),
    /// A device was discovered/updated; payload is a compact JSON object
    /// (same shape as a `bt-list` element). Wire: `bt:device:<json>`.
    BtDevice(String),
    /// A device was removed from discovery; payload is the MAC.
    /// Wire: `bt:device-removed:<mac>`.
    BtDeviceRemoved(String),
    /// Discovery (scan) started/stopped (`bt:scanning:on` / `bt:scanning:off`).
    BtScanning(bool),
    /// NetworkManager connectivity changed; payload is a state word
    /// (`none`/`portal`/`limited`/`full`/`unknown`). Wire: `net:connectivity:<state>`.
    NetConnectivity(String),
    /// Wi-Fi state changed; payload is a compact JSON object (same shape as a
    /// `net-status` body). Wire: `net:wifi:<json>`.
    NetWifi(String),
    /// Primary connection changed; payload is its id/name (may be empty).
    /// Wire: `net:primary:<id>`.
    NetPrimary(String),
    /// Battery state changed; payload is a compact JSON object (same shape as a
    /// `power-battery` body). Wire: `power:battery:<json>`.
    PowerBattery(String),

    // --- Phase 4 events (Hyprland) ---
    /// Active window changed; payload is the active window's class (may be empty
    /// when no window is focused). Wire: `hypr:activewindow:<class>`.
    HyprActiveWindow(String),
    /// Active window fullscreen state changed. Wire: `hypr:fullscreen:<0|1>`.
    HyprFullscreen(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Controller,
    Mouse,
}

impl InputMode {
    pub fn as_str(self) -> &'static str {
        match self {
            InputMode::Controller => "controller",
            InputMode::Mouse => "mouse",
        }
    }
}

impl fmt::Display for Event {
    /// Render the event as its exact wire string (no trailing newline; the
    /// codec adds it).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::ControllerWake => f.write_str("controller-wake"),
            Event::ControllerDisconnected => f.write_str("controller-disconnected"),
            Event::PadConnected(json) => write!(f, "pad:connected:{json}"),
            Event::PadDisconnected(id) => write!(f, "pad:disconnected:{id}"),
            Event::HomePress => f.write_str("home-press"),
            Event::ComboHomeHold => f.write_str("combo:home-hold"),
            Event::ComboEndSession => f.write_str("combo:end-session"),
            Event::ComboForceQuit => f.write_str("combo:force-quit"),
            Event::ComboSuspendStream => f.write_str("combo:suspend-stream"),
            Event::Intent(name) => write!(f, "intent:{name}"),
            Event::InputMode(m) => write!(f, "input-mode:{}", m.as_str()),
            Event::Buttons(s) => write!(f, "buttons:{s}"),
            Event::BtPowered(on) => write!(f, "bt:powered:{}", on_off(*on)),
            Event::BtDevice(json) => write!(f, "bt:device:{json}"),
            Event::BtDeviceRemoved(mac) => write!(f, "bt:device-removed:{mac}"),
            Event::BtScanning(on) => write!(f, "bt:scanning:{}", on_off(*on)),
            Event::NetConnectivity(state) => write!(f, "net:connectivity:{state}"),
            Event::NetWifi(json) => write!(f, "net:wifi:{json}"),
            Event::NetPrimary(id) => write!(f, "net:primary:{id}"),
            Event::PowerBattery(json) => write!(f, "power:battery:{json}"),
            Event::HyprActiveWindow(class) => write!(f, "hypr:activewindow:{class}"),
            Event::HyprFullscreen(fs) => write!(f, "hypr:fullscreen:{}", if *fs { 1 } else { 0 }),
        }
    }
}

/// `on`/`off` for boolean wire payloads (bt:powered, bt:scanning).
fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

// ---------------------------------------------------------------------------
// Response builders (the exact reply strings, sans trailing newline).
// ---------------------------------------------------------------------------

pub fn resp_ok() -> String {
    "ok".to_string()
}

pub fn resp_unknown() -> String {
    "unknown".to_string()
}

pub fn resp_subscribed() -> String {
    "subscribed".to_string()
}

pub fn resp_status(connected: bool, grabbed: bool) -> String {
    let c = if connected {
        "connected"
    } else {
        "disconnected"
    };
    let g = if grabbed { "grabbed" } else { "released" };
    format!("{c}:{g}")
}

pub fn resp_set_binding_usage() -> String {
    "error:usage: set-binding <action> <button_name>".to_string()
}

pub fn resp_unknown_action(action: &str) -> String {
    format!("error:unknown action '{action}'")
}

pub fn resp_invalid_button(button: &str) -> String {
    format!("error:invalid button '{button}'")
}

pub fn resp_captured(button_name: &str) -> String {
    format!("captured:{button_name}")
}

pub fn resp_set_config_usage() -> String {
    "error:usage: set-config <json-object>".to_string()
}

pub fn resp_record_launch_usage() -> String {
    "error:usage: record-launch <json-object>".to_string()
}

pub fn resp_intent_usage() -> String {
    "error:usage: intent <name>".to_string()
}

/// Error reply for an `intent <name>` whose name is outside the closed
/// vocabulary.
pub fn resp_unknown_intent(name: &str) -> String {
    format!("error:unknown intent '{name}'")
}

/// Generic error reply for a malformed config/recents body.
pub fn resp_error(msg: &str) -> String {
    format!("error:{msg}")
}

pub fn resp_timeout() -> String {
    "timeout".to_string()
}

// ---------------------------------------------------------------------------
// Phase 3 response builders (D-Bus query replies).
// ---------------------------------------------------------------------------

/// Returned when a Phase 3 command is issued on a host where the D-Bus backbone
/// isn't wired (e.g. non-Linux builds where the module is `cfg`-excluded). The
/// IPC layer substitutes this for the `Some(tx)` round-trip when the channel is
/// `None`.
pub fn resp_unsupported() -> String {
    "error:unsupported on this platform".to_string()
}

/// Bluetooth adapter power status reply (`bt-power-status`).
pub fn resp_bt_power(on: bool) -> String {
    if on {
        "bt:on".to_string()
    } else {
        "bt:off".to_string()
    }
}

/// Usage line for a `bt-connect`/`bt-disconnect`/`bt-pair`/`bt-trust` issued
/// without a MAC. `which` is the bare command word.
pub fn resp_bt_mac_usage(which: &str) -> String {
    format!("error:usage: {which} <mac>")
}

/// Usage line for `sunshine-status` issued without a `<host> <port>` body.
pub fn resp_sunshine_status_usage() -> String {
    "error:usage: sunshine-status <host> <port>".to_string()
}

/// `power-can-suspend` reply: `yes` / `no`.
pub fn resp_yes_no(yes: bool) -> String {
    if yes {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

pub fn resp_cancelled() -> String {
    "cancelled".to_string()
}

/// Compact single-line JSON object mapping action -> button code name, in the
/// given order. Mirrors Python `json.dumps(result, separators=(",", ":"))`.
pub fn resp_bindings(ordered: &[(String, String)]) -> String {
    let mut map = serde_json::Map::new();
    for (action, name) in ordered {
        map.insert(action.clone(), serde_json::Value::String(name.clone()));
    }
    serde_json::to_string(&serde_json::Value::Object(map)).expect("bindings serialize")
}

/// One pad's compact-JSON object `{id,index,name,grabbed}`. Shared shape for the
/// `pad:connected:<json>` event body (sans `grabbed`) and the `get-pads` array
/// elements. `grabbed` is omitted from the connect event (the slot is always
/// grabbed at connect) and present in `get-pads` so the UI can show per-pad grab
/// state.
fn pad_value(id: &str, index: u8, name: &str, grabbed: Option<bool>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("index".into(), serde_json::Value::Number(index.into()));
    obj.insert("name".into(), serde_json::Value::String(name.to_string()));
    if let Some(g) = grabbed {
        obj.insert("grabbed".into(), serde_json::Value::Bool(g));
    }
    serde_json::Value::Object(obj)
}

/// Compact single-line JSON object for a `pad:connected:<json>` event body.
pub fn pad_connected_json(id: &str, index: u8, name: &str) -> String {
    serde_json::to_string(&pad_value(id, index, name, None)).expect("pad serialize")
}

/// Compact single-line JSON array body for the `get-pads` reply. `pads` is
/// `(id, index, name, grabbed)` per connected pad; the caller should pass them
/// in ascending player-index order for a stable wire.
pub fn resp_pads(pads: &[(String, u8, String, bool)]) -> String {
    let arr: Vec<serde_json::Value> = pads
        .iter()
        .map(|(id, index, name, grabbed)| pad_value(id, *index, name, Some(*grabbed)))
        .collect();
    serde_json::to_string(&serde_json::Value::Array(arr)).expect("pads serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_commands() {
        assert_eq!(Command::parse("grab"), Command::Grab);
        assert_eq!(Command::parse("release"), Command::Release);
        assert_eq!(Command::parse("status"), Command::Status);
        assert_eq!(Command::parse("subscribe"), Command::Subscribe);
        assert_eq!(Command::parse("get-bindings"), Command::GetBindings);
        assert_eq!(Command::parse("capture-next"), Command::CaptureNext);
        assert_eq!(Command::parse("capture-cancel"), Command::CaptureCancel);
        assert_eq!(Command::parse("get-pads"), Command::GetPads);
        assert_eq!(Command::parse("  get-pads  "), Command::GetPads);
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(Command::parse("  grab  "), Command::Grab);
        assert_eq!(Command::parse("grab\r"), Command::Grab);
    }

    #[test]
    fn parses_set_binding() {
        assert_eq!(
            Command::parse("set-binding select BTN_SOUTH"),
            Command::SetBinding {
                action: "select".into(),
                button: "BTN_SOUTH".into()
            }
        );
    }

    #[test]
    fn set_binding_arg_errors() {
        // One token after the prefix -> usage.
        assert_eq!(
            Command::parse("set-binding select"),
            Command::SetBindingUsage
        );
        // Bare `set-binding` (no trailing space/args) -> unknown, matching Python.
        assert_eq!(Command::parse("set-binding"), Command::Unknown);
    }

    #[test]
    fn set_binding_extra_tokens_match_python_split() {
        // Python `split(None, 2)` keeps the remainder as the button name, so
        // extra tokens become part of an (invalid) button, not a usage error.
        // Leading/internal whitespace runs collapse like Python's split.
        assert_eq!(
            Command::parse("set-binding select BTN_SOUTH EXTRA"),
            Command::SetBinding {
                action: "select".into(),
                button: "BTN_SOUTH EXTRA".into()
            }
        );
        assert_eq!(
            Command::parse("set-binding   select    BTN_SOUTH"),
            Command::SetBinding {
                action: "select".into(),
                button: "BTN_SOUTH".into()
            }
        );
    }

    #[test]
    fn unrecognized_is_unknown() {
        assert_eq!(Command::parse("frobnicate"), Command::Unknown);
        // `kbd-log` is gone entirely (keyboard snoop removed in Phase 2).
        assert_eq!(Command::parse("kbd-log on"), Command::Unknown);
        assert_eq!(Command::parse("kbd-log maybe"), Command::Unknown);
        assert_eq!(Command::parse(""), Command::Unknown);
    }

    #[test]
    fn parses_phase2_simple_commands() {
        assert_eq!(Command::parse("list-apps"), Command::ListApps);
        assert_eq!(Command::parse("get-config"), Command::GetConfig);
        assert_eq!(Command::parse("get-recents"), Command::GetRecents);
        assert_eq!(Command::parse("  list-apps  "), Command::ListApps);
    }

    #[test]
    fn parses_set_config_body() {
        assert_eq!(
            Command::parse(r#"set-config {"themeMode":"dark"}"#),
            Command::SetConfig(r#"{"themeMode":"dark"}"#.into())
        );
        // Body is trimmed of surrounding whitespace.
        assert_eq!(
            Command::parse("set-config   {\"a\":1}  "),
            Command::SetConfig(r#"{"a":1}"#.into())
        );
        // Bare command (no body) -> usage.
        assert_eq!(Command::parse("set-config"), Command::SetConfigUsage);
        assert_eq!(Command::parse("set-config   "), Command::SetConfigUsage);
        // Word boundary: `set-configX` is NOT set-config.
        assert_eq!(Command::parse("set-configX"), Command::Unknown);
    }

    #[test]
    fn parses_record_launch_body() {
        assert_eq!(
            Command::parse(r#"record-launch {"name":"Firefox","exec":"firefox"}"#),
            Command::RecordLaunch(r#"{"name":"Firefox","exec":"firefox"}"#.into())
        );
        assert_eq!(Command::parse("record-launch"), Command::RecordLaunchUsage);
        assert_eq!(Command::parse("record-launchX"), Command::Unknown);
    }

    #[test]
    fn parses_intent_command() {
        // Valid name token -> Intent (vocabulary is NOT checked at parse time).
        assert_eq!(
            Command::parse("intent home-tap"),
            Command::Intent("home-tap".into())
        );
        assert_eq!(
            Command::parse("intent home"),
            Command::Intent("home".into())
        );
        // An unknown-but-well-formed token still parses; the runtime rejects it.
        assert_eq!(
            Command::parse("intent frobnicate"),
            Command::Intent("frobnicate".into())
        );
        // Body is whitespace-trimmed.
        assert_eq!(
            Command::parse("  intent   nav-up  "),
            Command::Intent("nav-up".into())
        );
        // Bare command (no name) -> usage.
        assert_eq!(Command::parse("intent"), Command::IntentUsage);
        assert_eq!(Command::parse("intent   "), Command::IntentUsage);
        // Word boundary: `intentX` is NOT intent.
        assert_eq!(Command::parse("intentX"), Command::Unknown);
    }

    #[test]
    fn intent_event_wire_strings_round_trip() {
        // Each closed-vocabulary intent renders as `intent:<name>`.
        for name in [
            "home",
            "home-tap",
            "home-hold",
            "menu",
            "nav-up",
            "nav-down",
            "nav-left",
            "nav-right",
            "select",
            "back",
            "settings",
            "power",
        ] {
            assert_eq!(
                Event::Intent(name.to_string()).to_string(),
                format!("intent:{name}")
            );
        }
    }

    #[test]
    fn phase2_usage_strings() {
        assert_eq!(
            resp_set_config_usage(),
            "error:usage: set-config <json-object>"
        );
        assert_eq!(
            resp_record_launch_usage(),
            "error:usage: record-launch <json-object>"
        );
        assert_eq!(resp_error("bad body"), "error:bad body");
        assert_eq!(resp_intent_usage(), "error:usage: intent <name>");
        assert_eq!(
            resp_unknown_intent("frobnicate"),
            "error:unknown intent 'frobnicate'"
        );
    }

    #[test]
    fn event_wire_strings() {
        assert_eq!(Event::ControllerWake.to_string(), "controller-wake");
        assert_eq!(
            Event::ControllerDisconnected.to_string(),
            "controller-disconnected"
        );
        assert_eq!(Event::HomePress.to_string(), "home-press");
        assert_eq!(Event::ComboHomeHold.to_string(), "combo:home-hold");
        assert_eq!(Event::ComboEndSession.to_string(), "combo:end-session");
        assert_eq!(Event::ComboForceQuit.to_string(), "combo:force-quit");
        assert_eq!(
            Event::ComboSuspendStream.to_string(),
            "combo:suspend-stream"
        );
        assert_eq!(
            Event::InputMode(InputMode::Controller).to_string(),
            "input-mode:controller"
        );
        assert_eq!(
            Event::InputMode(InputMode::Mouse).to_string(),
            "input-mode:mouse"
        );
        assert_eq!(
            Event::Buttons("Home + B".into()).to_string(),
            "buttons:Home + B"
        );
        assert_eq!(Event::Buttons(String::new()).to_string(), "buttons:");
    }

    #[test]
    fn response_strings() {
        assert_eq!(resp_ok(), "ok");
        assert_eq!(resp_unknown(), "unknown");
        assert_eq!(resp_subscribed(), "subscribed");
        assert_eq!(resp_status(true, true), "connected:grabbed");
        assert_eq!(resp_status(false, false), "disconnected:released");
        assert_eq!(
            resp_set_binding_usage(),
            "error:usage: set-binding <action> <button_name>"
        );
        assert_eq!(
            resp_unknown_action("drawer"),
            "error:unknown action 'drawer'"
        );
        assert_eq!(
            resp_invalid_button("BTN_LEFT"),
            "error:invalid button 'BTN_LEFT'"
        );
        assert_eq!(resp_captured("BTN_SOUTH"), "captured:BTN_SOUTH");
        assert_eq!(resp_timeout(), "timeout");
        assert_eq!(resp_cancelled(), "cancelled");
    }

    #[test]
    fn parses_phase3_bare_commands() {
        assert_eq!(Command::parse("bt-power-status"), Command::BtPowerStatus);
        assert_eq!(Command::parse("bt-power-on"), Command::BtPowerOn);
        assert_eq!(Command::parse("bt-power-off"), Command::BtPowerOff);
        assert_eq!(Command::parse("bt-scan-on"), Command::BtScanOn);
        assert_eq!(Command::parse("bt-scan-off"), Command::BtScanOff);
        assert_eq!(Command::parse("bt-list"), Command::BtList);
        assert_eq!(Command::parse("net-status"), Command::NetStatus);
        assert_eq!(Command::parse("net-wifi-list"), Command::NetWifiList);
        assert_eq!(Command::parse("net-wifi-rescan"), Command::NetWifiRescan);
        assert_eq!(
            Command::parse("power-can-suspend"),
            Command::PowerCanSuspend
        );
        assert_eq!(Command::parse("power-suspend"), Command::PowerSuspend);
        assert_eq!(Command::parse("power-battery"), Command::PowerBattery);
        assert_eq!(Command::parse("  bt-list  "), Command::BtList);
    }

    #[test]
    fn parses_phase3_mac_commands() {
        assert_eq!(
            Command::parse("bt-connect AA:BB:CC:DD:EE:FF"),
            Command::BtConnect("AA:BB:CC:DD:EE:FF".into())
        );
        assert_eq!(
            Command::parse("bt-disconnect AA:BB:CC:DD:EE:FF"),
            Command::BtDisconnect("AA:BB:CC:DD:EE:FF".into())
        );
        assert_eq!(
            Command::parse("bt-pair AA:BB:CC:DD:EE:FF"),
            Command::BtPair("AA:BB:CC:DD:EE:FF".into())
        );
        assert_eq!(
            Command::parse("bt-trust AA:BB:CC:DD:EE:FF"),
            Command::BtTrust("AA:BB:CC:DD:EE:FF".into())
        );
        // Body is trimmed.
        assert_eq!(
            Command::parse("bt-connect   AA:BB:CC:DD:EE:FF  "),
            Command::BtConnect("AA:BB:CC:DD:EE:FF".into())
        );
    }

    #[test]
    fn phase3_mac_usage_and_word_boundary() {
        assert_eq!(
            Command::parse("bt-connect"),
            Command::BtMacUsage("bt-connect")
        );
        assert_eq!(
            Command::parse("bt-disconnect   "),
            Command::BtMacUsage("bt-disconnect")
        );
        assert_eq!(Command::parse("bt-pair"), Command::BtMacUsage("bt-pair"));
        assert_eq!(Command::parse("bt-trust"), Command::BtMacUsage("bt-trust"));
        // Word boundary: a longer word is NOT a MAC command.
        assert_eq!(Command::parse("bt-connectX"), Command::Unknown);
        assert_eq!(Command::parse("bt-listX"), Command::Unknown);
    }

    #[test]
    fn phase3_event_wire_strings() {
        assert_eq!(Event::BtPowered(true).to_string(), "bt:powered:on");
        assert_eq!(Event::BtPowered(false).to_string(), "bt:powered:off");
        assert_eq!(
            Event::BtDevice(r#"{"mac":"AA","name":"Pad"}"#.into()).to_string(),
            r#"bt:device:{"mac":"AA","name":"Pad"}"#
        );
        assert_eq!(
            Event::BtDeviceRemoved("AA:BB:CC:DD:EE:FF".into()).to_string(),
            "bt:device-removed:AA:BB:CC:DD:EE:FF"
        );
        assert_eq!(Event::BtScanning(true).to_string(), "bt:scanning:on");
        assert_eq!(Event::BtScanning(false).to_string(), "bt:scanning:off");
        assert_eq!(
            Event::NetConnectivity("full".into()).to_string(),
            "net:connectivity:full"
        );
        assert_eq!(
            Event::NetWifi(r#"{"connectivity":"full"}"#.into()).to_string(),
            r#"net:wifi:{"connectivity":"full"}"#
        );
        assert_eq!(
            Event::NetPrimary("Wired connection 1".into()).to_string(),
            "net:primary:Wired connection 1"
        );
        assert_eq!(Event::NetPrimary(String::new()).to_string(), "net:primary:");
        assert_eq!(
            Event::PowerBattery(r#"{"present":false}"#.into()).to_string(),
            r#"power:battery:{"present":false}"#
        );
    }

    #[test]
    fn parses_phase4_bare_commands() {
        assert_eq!(Command::parse("hypr-active"), Command::HyprActive);
        assert_eq!(Command::parse("hypr-clients"), Command::HyprClients);
        assert_eq!(Command::parse("  hypr-active  "), Command::HyprActive);
        // Word boundary: a longer word is NOT a hypr command.
        assert_eq!(Command::parse("hypr-activeX"), Command::Unknown);
        assert_eq!(Command::parse("hypr-clientsX"), Command::Unknown);
    }

    #[test]
    fn parses_sunshine_status_body() {
        assert_eq!(
            Command::parse("sunshine-status 192.168.8.10 47990"),
            Command::SunshineStatus {
                host: "192.168.8.10".into(),
                port: "47990".into()
            }
        );
        // Surrounding/internal whitespace collapses like split_whitespace.
        assert_eq!(
            Command::parse("sunshine-status   host   1234  "),
            Command::SunshineStatus {
                host: "host".into(),
                port: "1234".into()
            }
        );
        // Missing port -> usage; bare command -> usage.
        assert_eq!(
            Command::parse("sunshine-status host"),
            Command::SunshineStatusUsage
        );
        assert_eq!(
            Command::parse("sunshine-status"),
            Command::SunshineStatusUsage
        );
        // Word boundary: `sunshine-statusX` is NOT sunshine-status.
        assert_eq!(Command::parse("sunshine-statusX"), Command::Unknown);
    }

    #[test]
    fn phase4_event_wire_strings() {
        assert_eq!(
            Event::HyprActiveWindow("firefox".into()).to_string(),
            "hypr:activewindow:firefox"
        );
        // Empty class is allowed (no focused window).
        assert_eq!(
            Event::HyprActiveWindow(String::new()).to_string(),
            "hypr:activewindow:"
        );
        assert_eq!(Event::HyprFullscreen(true).to_string(), "hypr:fullscreen:1");
        assert_eq!(
            Event::HyprFullscreen(false).to_string(),
            "hypr:fullscreen:0"
        );
    }

    #[test]
    fn phase4_usage_string() {
        assert_eq!(
            resp_sunshine_status_usage(),
            "error:usage: sunshine-status <host> <port>"
        );
    }

    #[test]
    fn phase3_response_strings() {
        assert_eq!(resp_unsupported(), "error:unsupported on this platform");
        assert_eq!(resp_bt_power(true), "bt:on");
        assert_eq!(resp_bt_power(false), "bt:off");
        assert_eq!(resp_yes_no(true), "yes");
        assert_eq!(resp_yes_no(false), "no");
        assert_eq!(
            resp_bt_mac_usage("bt-connect"),
            "error:usage: bt-connect <mac>"
        );
        assert_eq!(resp_bt_mac_usage("bt-pair"), "error:usage: bt-pair <mac>");
    }

    #[test]
    fn pad_event_wire_strings() {
        // `pad:connected:<json>` carries a compact `{id,index,name}` object.
        assert_eq!(
            Event::PadConnected(r#"{"id":"uniq:aa","index":0,"name":"Xbox"}"#.into()).to_string(),
            r#"pad:connected:{"id":"uniq:aa","index":0,"name":"Xbox"}"#
        );
        // `pad:disconnected:<id>` carries the bare wire id.
        assert_eq!(
            Event::PadDisconnected("uniq:aa".into()).to_string(),
            "pad:disconnected:uniq:aa"
        );
    }

    #[test]
    fn pad_connected_json_is_compact_object() {
        assert_eq!(
            pad_connected_json("uniq:aa:bb", 1, "DualSense"),
            r#"{"id":"uniq:aa:bb","index":1,"name":"DualSense"}"#
        );
    }

    #[test]
    fn resp_pads_is_compact_array_with_grabbed() {
        let pads = vec![
            ("uniq:p1".to_string(), 0u8, "Xbox".to_string(), true),
            ("uniq:p2".to_string(), 1u8, "PS5".to_string(), false),
        ];
        assert_eq!(
            resp_pads(&pads),
            r#"[{"id":"uniq:p1","index":0,"name":"Xbox","grabbed":true},{"id":"uniq:p2","index":1,"name":"PS5","grabbed":false}]"#
        );
        // Empty fleet -> empty array.
        assert_eq!(resp_pads(&[]), "[]");
    }

    #[test]
    fn bindings_response_is_ordered_compact_json() {
        let ordered = vec![
            ("select".to_string(), "BTN_SOUTH".to_string()),
            ("back".to_string(), "BTN_EAST".to_string()),
            ("altSelect".to_string(), "BTN_NORTH".to_string()),
            ("confirm".to_string(), "BTN_START".to_string()),
        ];
        assert_eq!(
            resp_bindings(&ordered),
            r#"{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START"}"#
        );
    }
}

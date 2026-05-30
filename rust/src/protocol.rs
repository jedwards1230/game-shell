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
    KbdLog(bool),
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
    /// Anything unrecognized -> the daemon replies `unknown`.
    Unknown,
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
            "kbd-log on" => Command::KbdLog(true),
            "kbd-log off" => Command::KbdLog(false),
            "list-apps" => Command::ListApps,
            "get-config" => Command::GetConfig,
            "get-recents" => Command::GetRecents,
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
    HomePress,
    ComboHomeHold,
    ComboEndSession,
    ComboForceQuit,
    ComboSuspendStream,
    InputMode(InputMode),
    /// Space-and-plus joined held controller inputs (may be empty).
    Buttons(String),
    /// Space-and-plus joined held keyboard keys (may be empty).
    Keys(String),
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
            Event::HomePress => f.write_str("home-press"),
            Event::ComboHomeHold => f.write_str("combo:home-hold"),
            Event::ComboEndSession => f.write_str("combo:end-session"),
            Event::ComboForceQuit => f.write_str("combo:force-quit"),
            Event::ComboSuspendStream => f.write_str("combo:suspend-stream"),
            Event::InputMode(m) => write!(f, "input-mode:{}", m.as_str()),
            Event::Buttons(s) => write!(f, "buttons:{s}"),
            Event::Keys(s) => write!(f, "keys:{s}"),
        }
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

/// Generic error reply for a malformed config/recents body.
pub fn resp_error(msg: &str) -> String {
    format!("error:{msg}")
}

pub fn resp_timeout() -> String {
    "timeout".to_string()
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
        assert_eq!(Command::parse("kbd-log on"), Command::KbdLog(true));
        assert_eq!(Command::parse("kbd-log off"), Command::KbdLog(false));
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
        assert_eq!(
            Event::Keys("Ctrl + Shift + A".into()).to_string(),
            "keys:Ctrl + Shift + A"
        );
        assert_eq!(Event::Keys(String::new()).to_string(), "keys:");
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

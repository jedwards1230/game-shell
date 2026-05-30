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
    /// Anything unrecognized -> the daemon replies `unknown`.
    Unknown,
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
            _ => {
                // Python keys `set-binding` off the `"set-binding "` prefix
                // (with trailing space), so a bare `set-binding` is `unknown`.
                if let Some(rest) = cmd.strip_prefix("set-binding ") {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.len() == 2 {
                        return Command::SetBinding {
                            action: parts[0].to_string(),
                            button: parts[1].to_string(),
                        };
                    }
                    return Command::SetBindingUsage;
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
        // Wrong arg count -> usage.
        assert_eq!(
            Command::parse("set-binding select"),
            Command::SetBindingUsage
        );
        assert_eq!(
            Command::parse("set-binding a b c"),
            Command::SetBindingUsage
        );
        // Bare `set-binding` (no trailing space/args) -> unknown, matching Python.
        assert_eq!(Command::parse("set-binding"), Command::Unknown);
    }

    #[test]
    fn unrecognized_is_unknown() {
        assert_eq!(Command::parse("frobnicate"), Command::Unknown);
        assert_eq!(Command::parse("kbd-log maybe"), Command::Unknown);
        assert_eq!(Command::parse(""), Command::Unknown);
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

//! Static input tables, key bindings, and settings persistence.
//!
//! This module is deliberately platform-independent: evdev/uinput codes are
//! represented as plain `u16` kernel constants (not `evdev::Key`) so the
//! wire-critical naming logic compiles and is unit-testable on any host. The
//! Linux input layer (`input.rs`) translates between these `u16` codes and
//! `evdev` types.
//!
//! Behavior here mirrors `input/gamepad-input.py` exactly so the QML client
//! sees an identical daemon. Where the legacy IPC doc and the Python code
//! disagree, the Python code wins (it is what the live shell talks to).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Linux input-event-codes (kernel `input-event-codes.h`). Kept as plain u16 so
// this module never depends on the (Linux-only) evdev crate.
// ---------------------------------------------------------------------------

// EV types (EV_ABS is matched via evdev's typed `EventType::ABSOLUTE`).
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;

// Gamepad buttons
pub const BTN_SOUTH: u16 = 0x130; // BTN_A
pub const BTN_EAST: u16 = 0x131; // BTN_B
pub const BTN_NORTH: u16 = 0x133; // BTN_X (kernel) -> "Y" face
pub const BTN_WEST: u16 = 0x134; // BTN_Y (kernel) -> "X" face
pub const BTN_TL: u16 = 0x136;
pub const BTN_TR: u16 = 0x137;
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_MODE: u16 = 0x13c;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;

// Mouse buttons
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

// Keyboard keys
pub const KEY_ESC: u16 = 1;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_A: u16 = 30;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_UP: u16 = 103;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_DOWN: u16 = 108;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_HOMEPAGE: u16 = 172;

// Relative axes
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;

// Absolute axes
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

// ---------------------------------------------------------------------------
// Combos and tuning constants (mirror gamepad-input.py)
// ---------------------------------------------------------------------------

/// Home + B held for COMBO_HOLD_SECS -> `combo:end-session`.
pub const COMBO_KEYS: [u16; 2] = [BTN_MODE, BTN_EAST];
pub const COMBO_HOLD_SECS: f64 = 3.0;

/// Back + Home + LB + RB, instant -> `combo:force-quit`.
pub const QUIT_COMBO_KEYS: [u16; 4] = [BTN_SELECT, BTN_MODE, BTN_TL, BTN_TR];

/// LB + RB + Start, instant -> `combo:suspend-stream` (unless also force-quit).
pub const SUSPEND_COMBO_KEYS: [u16; 3] = [BTN_START, BTN_TL, BTN_TR];

pub const HOME_HOLD_SECS: f64 = 2.0;

pub const STICK_DEADZONE: f64 = 0.30;
pub const STICK_INITIAL_DELAY_MS: u64 = 300;
pub const STICK_REPEAT_INTERVAL_MS: u64 = 150;

pub const MOUSE_SPEED_MIN: f64 = 2.0;
pub const MOUSE_SPEED_MAX: f64 = 25.0;
pub const MOUSE_POLL_MS: u64 = 16;

pub const CAPTURE_TIMEOUT_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

/// A single action binding: which gamepad button triggers which keyboard key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub action: &'static str,
    pub button: u16,
    pub key: u16,
}

/// Default bindings, in canonical order. `BTN_MODE` (Home) is intentionally
/// absent: it is handled directly to broadcast `intent:home-tap`/`intent:home-hold`
/// rather than mapped to a key (mapping `KEY_HOMEPAGE` would leak to focused
/// apps). The legacy IPC doc lists a `drawer` action — that is stale; the
/// Python daemon and the QML `remappableActions` both use exactly these four.
pub fn default_bindings() -> Vec<Binding> {
    vec![
        Binding {
            action: "select",
            button: BTN_SOUTH,
            key: KEY_ENTER,
        },
        Binding {
            action: "back",
            button: BTN_EAST,
            key: KEY_ESC,
        },
        Binding {
            action: "altSelect",
            button: BTN_NORTH,
            key: KEY_TAB,
        },
        Binding {
            action: "confirm",
            button: BTN_START,
            key: KEY_ENTER,
        },
    ]
}

pub fn is_default_action(action: &str) -> bool {
    matches!(action, "select" | "back" | "altSelect" | "confirm")
}

/// Buttons that may be assigned to an action via `set-binding`.
pub const REMAPPABLE_BUTTONS: [u16; 11] = [
    BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE,
    BTN_THUMBL, BTN_THUMBR,
];

pub fn is_remappable(code: u16) -> bool {
    REMAPPABLE_BUTTONS.contains(&code)
}

// ---------------------------------------------------------------------------
// Name <-> code conversion
// ---------------------------------------------------------------------------

/// Resolve an evdev button name (e.g. `BTN_SOUTH`) to its code. Accepts all
/// kernel aliases (`BTN_A`/`BTN_SOUTH`/`BTN_GAMEPAD` all resolve to 0x130) so
/// a `settings.json` written by either the Python daemon (which may emit the
/// `BTN_A` alias) or this daemon loads correctly. Mirrors Python's
/// `getattr(ecodes, name)`.
pub fn button_name_to_code(name: &str) -> Option<u16> {
    Some(match name {
        "BTN_SOUTH" | "BTN_A" | "BTN_GAMEPAD" => BTN_SOUTH,
        "BTN_EAST" | "BTN_B" => BTN_EAST,
        "BTN_NORTH" | "BTN_X" => BTN_NORTH,
        "BTN_WEST" | "BTN_Y" => BTN_WEST,
        "BTN_TL" => BTN_TL,
        "BTN_TR" => BTN_TR,
        "BTN_TL2" => BTN_TL2,
        "BTN_TR2" => BTN_TR2,
        "BTN_SELECT" => BTN_SELECT,
        "BTN_START" => BTN_START,
        "BTN_MODE" => BTN_MODE,
        "BTN_THUMBL" => BTN_THUMBL,
        "BTN_THUMBR" => BTN_THUMBR,
        _ => return None,
    })
}

/// Canonical evdev name for a button code. We emit the semantic gamepad names
/// (`BTN_SOUTH`, not `BTN_A`) which the QML display map handles either way.
pub fn button_code_to_name(code: u16) -> String {
    let name = match code {
        BTN_SOUTH => "BTN_SOUTH",
        BTN_EAST => "BTN_EAST",
        BTN_NORTH => "BTN_NORTH",
        BTN_WEST => "BTN_WEST",
        BTN_TL => "BTN_TL",
        BTN_TR => "BTN_TR",
        BTN_TL2 => "BTN_TL2",
        BTN_TR2 => "BTN_TR2",
        BTN_SELECT => "BTN_SELECT",
        BTN_START => "BTN_START",
        BTN_MODE => "BTN_MODE",
        BTN_THUMBL => "BTN_THUMBL",
        BTN_THUMBR => "BTN_THUMBR",
        _ => return format!("0x{code:x}"),
    };
    name.to_string()
}

/// Friendly display name used in the `buttons:` debug event.
pub fn button_display_name(code: u16) -> Option<&'static str> {
    Some(match code {
        BTN_SOUTH => "A",
        BTN_EAST => "B",
        BTN_NORTH => "Y",
        BTN_WEST => "X",
        BTN_TL => "LB",
        BTN_TR => "RB",
        BTN_TL2 => "LT",
        BTN_TR2 => "RT",
        BTN_SELECT => "Back",
        BTN_START => "Start",
        BTN_MODE => "Home",
        BTN_THUMBL => "L3",
        BTN_THUMBR => "R3",
        _ => return None,
    })
}

/// D-pad arrow-key display names used in the `buttons:` debug event.
pub fn dpad_display_name(code: u16) -> Option<&'static str> {
    Some(match code {
        KEY_UP => "D-Up",
        KEY_DOWN => "D-Down",
        KEY_LEFT => "D-Left",
        KEY_RIGHT => "D-Right",
        _ => return None,
    })
}

/// Keyboard source classification used when resolving a keyboard key's
/// friendly display name from its kernel code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdSource {
    Mapped,
    Fallback,
    Unknown,
}

impl KbdSource {
    pub fn as_str(self) -> &'static str {
        match self {
            KbdSource::Mapped => "mapped",
            KbdSource::Fallback => "fallback",
            KbdSource::Unknown => "unknown",
        }
    }
}

/// Explicit friendly names for keyboard keys (the `mapped` source).
pub fn kbd_mapped_name(code: u16) -> Option<&'static str> {
    Some(match code {
        KEY_LEFTMETA | KEY_RIGHTMETA => "Meta",
        KEY_LEFTCTRL | KEY_RIGHTCTRL => "Ctrl",
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => "Shift",
        KEY_LEFTALT | KEY_RIGHTALT => "Alt",
        KEY_UP => "↑",
        KEY_DOWN => "↓",
        KEY_LEFT => "←",
        KEY_RIGHT => "→",
        KEY_ENTER => "Enter",
        KEY_ESC => "Esc",
        KEY_BACKSPACE => "Backspace",
        KEY_SPACE => "Space",
        KEY_TAB => "Tab",
        KEY_HOMEPAGE => "Home",
        KEY_CAPSLOCK => "Caps",
        _ => return None,
    })
}

/// Mimics Python `str.title()`: each maximal run of ASCII letters is
/// capitalized (first letter upper, rest lower); non-letters reset the run and
/// pass through. e.g. `LEFTMETA` -> `Leftmeta`, `F1` -> `F1`.
pub fn python_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alpha = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() {
            if prev_alpha {
                out.extend(ch.to_lowercase());
            } else {
                out.extend(ch.to_uppercase());
            }
            prev_alpha = true;
        } else {
            out.push(ch);
            prev_alpha = false;
        }
    }
    out
}

/// Classify a keyboard code into `(raw_kernel_name, display, source)`.
///
/// `raw_kernel_name` is the kernel name as known to evdev (e.g. `KEY_LEFTMETA`),
/// supplied by the Linux layer; `None` means evdev did not recognize the code.
/// Mirrors Python `_kbd_key_info`.
pub fn kbd_key_info(code: u16, raw_kernel_name: Option<&str>) -> (String, String, KbdSource) {
    let raw = match raw_kernel_name {
        Some(n) => n.to_string(),
        None => format!("0x{code:x}"),
    };
    if let Some(mapped) = kbd_mapped_name(code) {
        return (raw, mapped.to_string(), KbdSource::Mapped);
    }
    if let Some(stripped) = raw.strip_prefix("KEY_") {
        return (raw.clone(), python_title(stripped), KbdSource::Fallback);
    }
    let display = raw.clone();
    (raw, display, KbdSource::Unknown)
}

// ---------------------------------------------------------------------------
// Settings persistence (~/.config/game-shell/settings.json)
// ---------------------------------------------------------------------------

/// Default settings path: `~/.config/game-shell/settings.json`.
pub fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/game-shell/settings.json")
}

/// Apply `keyBindings` overrides from a parsed settings document onto a set of
/// bindings. Mirrors Python `_load_bindings`: unknown actions and
/// non-remappable / unknown buttons are silently skipped; an array value uses
/// its last element.
pub fn apply_binding_overrides(bindings: &mut [Binding], settings: &serde_json::Value) {
    let Some(kb) = settings.get("keyBindings").and_then(|v| v.as_object()) else {
        return;
    };
    // action -> resolved button code
    let mut overrides: HashMap<&str, u16> = HashMap::new();
    for (action, value) in kb {
        if !is_default_action(action) {
            continue;
        }
        let name = match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => match arr.last().and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            },
            _ => continue,
        };
        let Some(code) = button_name_to_code(&name) else {
            continue;
        };
        if !is_remappable(code) {
            continue;
        }
        overrides.insert(action.as_str(), code);
    }
    for b in bindings.iter_mut() {
        if let Some(&code) = overrides.get(b.action) {
            b.button = code;
        }
    }
}

/// Load bindings: start from defaults, overlay `settings.json` overrides.
pub fn load_bindings(path: &Path) -> Vec<Binding> {
    let mut bindings = default_bindings();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            apply_binding_overrides(&mut bindings, &value);
        }
    }
    bindings
}

/// Serialize bindings into the document under `keyBindings`, preserving any
/// other keys already present (read-modify-write). Returns the compact,
/// single-line JSON string that must be written to disk (QML's `SplitParser`
/// requires single-line). Mirrors Python `_save_bindings`.
pub fn build_settings_json(existing: Option<&str>, bindings: &[Binding]) -> String {
    let mut doc: serde_json::Value = existing
        .and_then(|t| serde_json::from_str(t).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

    let mut kb = serde_json::Map::new();
    for b in bindings {
        kb.insert(
            b.action.to_string(),
            serde_json::Value::String(button_code_to_name(b.button)),
        );
    }
    doc.as_object_mut()
        .expect("doc is an object")
        .insert("keyBindings".to_string(), serde_json::Value::Object(kb));

    // serde_json (with preserve_order) produces compact output via to_string,
    // matching Python's json.dumps(separators=(",", ":")).
    serde_json::to_string(&doc).expect("settings serialize")
}

/// Write bindings to disk (read existing, modify, write single-line).
pub fn save_bindings(path: &Path, bindings: &[Binding]) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).ok();
    let json = build_settings_json(existing.as_deref(), bindings);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)
}

// ---------------------------------------------------------------------------
// Generic config read-modify-write (the `get-config` / `set-config` commands).
//
// These let the QML shell stop shelling out to inline python for its own
// settings (themeMode, streamingViewMode, controllerDebug, ...). The daemon is
// the sole writer of settings.json; QML sends the keys it owns and the daemon
// merges them in, preserving foreign keys (notably the daemon-owned
// `keyBindings`). The wire body is a compact single-line JSON object in both
// directions; see docs/IPC_PROTOCOL.md.
// ---------------------------------------------------------------------------

/// Merge a compact-JSON object of updates into the existing settings document,
/// preserving every other key, and return the new compact single-line JSON.
///
/// - `existing` is the current settings.json text (or `None`/garbage -> treated
///   as an empty object), matching `build_settings_json`'s tolerance.
/// - `updates` must be a JSON object; any key whose value is JSON `null` is
///   *removed* from the document (this is how the shell drops the legacy
///   `moonlightViewMode` key). Non-null values overwrite/insert.
/// - Insertion order: existing keys keep their position; brand-new keys append
///   in `updates` order (serde_json `preserve_order`).
///
/// Returns `None` if `updates` is not a JSON object.
pub fn merge_config(existing: Option<&str>, updates: &serde_json::Value) -> Option<String> {
    let updates = updates.as_object()?;
    let mut doc: serde_json::Value = existing
        .and_then(|t| serde_json::from_str(t).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let obj = doc.as_object_mut().expect("doc is an object");
    for (k, v) in updates {
        if v.is_null() {
            obj.shift_remove(k);
        } else {
            obj.insert(k.clone(), v.clone());
        }
    }
    Some(serde_json::to_string(&doc).expect("settings serialize"))
}

/// Read the settings document and return it as a compact single-line JSON
/// object (the `get-config` response body). A missing or unparseable file
/// yields `{}` (empty object), so the QML client always gets valid JSON.
pub fn load_config_json(path: &Path) -> String {
    let doc: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    serde_json::to_string(&doc).expect("settings serialize")
}

/// Apply a `set-config` update to disk: read existing, merge, write single-line.
/// Returns the new document text on success. `updates` must be a JSON object.
pub fn set_config(path: &Path, updates: &serde_json::Value) -> std::io::Result<String> {
    let existing = std::fs::read_to_string(path).ok();
    let merged = merge_config(existing.as_deref(), updates).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "set-config body must be a JSON object",
        )
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &merged)?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_are_the_four_canonical_actions() {
        let b = default_bindings();
        let actions: Vec<&str> = b.iter().map(|x| x.action).collect();
        assert_eq!(actions, ["select", "back", "altSelect", "confirm"]);
        // No `drawer`, BTN_MODE not bound.
        assert!(!b.iter().any(|x| x.button == BTN_MODE));
    }

    #[test]
    fn button_name_aliases_resolve() {
        assert_eq!(button_name_to_code("BTN_SOUTH"), Some(BTN_SOUTH));
        assert_eq!(button_name_to_code("BTN_A"), Some(BTN_SOUTH));
        assert_eq!(button_name_to_code("BTN_GAMEPAD"), Some(BTN_SOUTH));
        assert_eq!(button_name_to_code("BTN_B"), Some(BTN_EAST));
        assert_eq!(button_name_to_code("BTN_Y"), Some(BTN_WEST));
        assert_eq!(button_name_to_code("BTN_X"), Some(BTN_NORTH));
        assert_eq!(button_name_to_code("NONSENSE"), None);
    }

    #[test]
    fn button_code_to_name_is_canonical() {
        assert_eq!(button_code_to_name(BTN_SOUTH), "BTN_SOUTH");
        assert_eq!(button_code_to_name(BTN_START), "BTN_START");
        assert_eq!(button_code_to_name(0x999), "0x999");
    }

    #[test]
    fn python_title_matches_cpython() {
        assert_eq!(python_title("LEFTMETA"), "Leftmeta");
        assert_eq!(python_title("A"), "A");
        assert_eq!(python_title("F1"), "F1");
        assert_eq!(python_title("NUMLOCK"), "Numlock");
        assert_eq!(python_title("abc1def"), "Abc1Def");
    }

    #[test]
    fn kbd_key_info_sources() {
        // mapped
        let (raw, disp, src) = kbd_key_info(KEY_LEFTMETA, Some("KEY_LEFTMETA"));
        assert_eq!(
            (raw.as_str(), disp.as_str(), src),
            ("KEY_LEFTMETA", "Meta", KbdSource::Mapped)
        );
        // fallback
        let (raw, disp, src) = kbd_key_info(KEY_CAPSLOCK + 1, Some("KEY_NUMLOCK"));
        assert_eq!(
            (raw.as_str(), disp.as_str(), src),
            ("KEY_NUMLOCK", "Numlock", KbdSource::Fallback)
        );
        // unknown (no kernel name)
        let (raw, _disp, src) = kbd_key_info(0xfff, None);
        assert_eq!((raw.as_str(), src), ("0xfff", KbdSource::Unknown));
    }

    #[test]
    fn load_applies_overrides_and_skips_invalid() {
        let settings = serde_json::json!({
            "themeMode": "dark",
            "keyBindings": {
                "select": "BTN_NORTH",        // remap select -> Y
                "back": ["BTN_WEST", "BTN_TR"], // array -> last element (BTN_TR)
                "confirm": "BTN_LEFT",        // not remappable -> skipped
                "bogus": "BTN_SOUTH"          // unknown action -> skipped
            }
        });
        let mut b = default_bindings();
        apply_binding_overrides(&mut b, &settings);
        let by = |a: &str| b.iter().find(|x| x.action == a).unwrap().button;
        assert_eq!(by("select"), BTN_NORTH);
        assert_eq!(by("back"), BTN_TR);
        assert_eq!(by("confirm"), BTN_START); // unchanged (BTN_LEFT not remappable)
        assert_eq!(by("altSelect"), BTN_NORTH); // default unchanged
    }

    #[test]
    fn save_preserves_other_keys_and_is_single_line() {
        let existing = r#"{"themeMode":"dark","moonlightViewMode":"grid"}"#;
        let bindings = default_bindings();
        let out = build_settings_json(Some(existing), &bindings);
        assert!(!out.contains('\n'));
        assert!(!out.contains(": ")); // compact separators
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["themeMode"], "dark");
        assert_eq!(v["moonlightViewMode"], "grid");
        assert_eq!(v["keyBindings"]["select"], "BTN_SOUTH");
        assert_eq!(v["keyBindings"]["confirm"], "BTN_START");
        // Insertion order preserved: keyBindings appended after existing keys.
        let prefix = r#"{"themeMode":"dark","moonlightViewMode":"grid","keyBindings":"#;
        assert!(out.starts_with(prefix), "got: {out}");
    }

    #[test]
    fn save_with_no_existing_file() {
        let out = build_settings_json(None, &default_bindings());
        assert_eq!(
            out,
            r#"{"keyBindings":{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START"}}"#
        );
    }

    #[test]
    fn merge_config_preserves_foreign_keys_and_is_single_line() {
        // Existing has a daemon-owned keyBindings the QML client must not clobber.
        let existing = r#"{"keyBindings":{"select":"BTN_SOUTH"},"themeMode":"light"}"#;
        let updates = serde_json::json!({
            "themeMode": "dark",
            "streamingViewMode": "apps",
            "controllerDebug": true
        });
        let out = merge_config(Some(existing), &updates).unwrap();
        assert!(!out.contains('\n'));
        assert!(!out.contains(": ")); // compact separators
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Foreign key preserved untouched.
        assert_eq!(v["keyBindings"]["select"], "BTN_SOUTH");
        // Existing key overwritten in place.
        assert_eq!(v["themeMode"], "dark");
        // New keys added (bool preserved as bool).
        assert_eq!(v["streamingViewMode"], "apps");
        assert_eq!(v["controllerDebug"], true);
        // themeMode kept its original position (it existed before the update).
        assert!(out.starts_with(r#"{"keyBindings":{"select":"BTN_SOUTH"},"themeMode":"dark""#));
    }

    #[test]
    fn merge_config_null_removes_key() {
        // Mirrors the shell dropping the legacy moonlightViewMode key.
        let existing = r#"{"themeMode":"dark","moonlightViewMode":"grid"}"#;
        let updates = serde_json::json!({ "moonlightViewMode": null });
        let out = merge_config(Some(existing), &updates).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("moonlightViewMode").is_none());
        assert_eq!(v["themeMode"], "dark");
    }

    #[test]
    fn merge_config_no_existing_creates_object() {
        let out = merge_config(None, &serde_json::json!({"themeMode":"auto"})).unwrap();
        assert_eq!(out, r#"{"themeMode":"auto"}"#);
        // Garbage existing is treated as empty.
        let out = merge_config(Some("not json"), &serde_json::json!({"a":1})).unwrap();
        assert_eq!(out, r#"{"a":1}"#);
    }

    #[test]
    fn set_config_then_load_round_trips_on_disk() {
        // Unique temp file (no global env) so this is parallel-safe.
        let path = std::env::temp_dir().join(format!(
            "gs-cfg-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);

        // Seed a foreign daemon-owned key.
        std::fs::write(&path, r#"{"keyBindings":{"select":"BTN_SOUTH"}}"#).unwrap();

        // QML-style update.
        let written = set_config(
            &path,
            &serde_json::json!({"themeMode":"dark","controllerDebug":true}),
        )
        .unwrap();
        assert!(!written.contains('\n'));

        // load_config_json reads it back as a compact object.
        let loaded = load_config_json(&path);
        let v: serde_json::Value = serde_json::from_str(&loaded).unwrap();
        assert_eq!(v["keyBindings"]["select"], "BTN_SOUTH"); // preserved
        assert_eq!(v["themeMode"], "dark");
        assert_eq!(v["controllerDebug"], true);

        // Missing file -> {}.
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_config_json(&path), "{}");
    }

    #[test]
    fn merge_config_rejects_non_object_body() {
        assert!(merge_config(None, &serde_json::json!([1, 2, 3])).is_none());
        assert!(merge_config(None, &serde_json::json!("string")).is_none());
    }
}

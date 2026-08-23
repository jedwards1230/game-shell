//! Static input tables, key bindings, and settings persistence.
//!
//! This module is deliberately platform-independent: evdev/uinput codes are
//! represented as plain `u16` kernel constants (not `evdev::Key`) so the
//! wire-critical naming logic compiles and is unit-testable on any host. The
//! Linux input layer (`input.rs`) translates between these `u16` codes and
//! `evdev` types.
//!
//! Behavior here was ported from the former `input/gamepad-input.py` (since
//! deleted) so the QML client sees an identical daemon. Where the legacy IPC
//! doc and that Python code disagreed, the Python code won (it was what the
//! live shell talked to).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// settings.json read-modify-write serialization.
// ---------------------------------------------------------------------------

/// Serializes the read→modify→write of `settings.json`. The file has two
/// concurrent writers that share no other synchronization: `save_bindings`
/// (driven from the input thread when a binding is captured, input.rs) and
/// `set_config` (driven from a `spawn_blocking` IPC handler, ipc.rs). Both
/// read the existing document, merge their change, and write the whole file
/// back. Without a shared lock those two RMW cycles can interleave
/// (read, read, write, write) and silently drop the loser's keys — e.g. a
/// captured binding clobbered by a concurrent `themeMode` flip. Holding this
/// lock across the entire read→modify→write makes each cycle atomic with
/// respect to the other regardless of thread/task scheduling.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Self-write generation guard (used by watch.rs to suppress daemon-own writes).
// ---------------------------------------------------------------------------

/// Monotonically increasing counter bumped before each `std::fs::write` to
/// `settings.json`. The file-watch task compares the generation before and after
/// a debounced inotify batch: if the counter advanced, the write was (at least
/// partly) daemon-originated and the `config:changed` event is suppressed. Pure
/// atomics — no lock, no async, always safe to call from any context.
static SELF_WRITE_GEN: AtomicU64 = AtomicU64::new(0);

/// Bump the self-write generation counter. Call this immediately BEFORE every
/// `std::fs::write` to `settings.json` (not for other files such as recents).
pub fn note_self_write() {
    SELF_WRITE_GEN.fetch_add(1, Ordering::Release);
}

/// Read the current self-write generation. The file-watch task calls this to
/// detect whether the daemon wrote `settings.json` during a debounce window.
pub fn self_write_gen() -> u64 {
    SELF_WRITE_GEN.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Atomic file write (shared crash-safe write-then-rename helper).
// ---------------------------------------------------------------------------

/// Atomically write `contents` to `path`: create the parent directory, write a
/// sibling temp file in the same directory, fsync-free `rename` it over the
/// target. A crash mid-write can then only leave a stray `*.tmp`, never a torn
/// or truncated target file (rename is atomic within a filesystem).
///
/// The temp file lives beside the target (same directory) so the rename stays on
/// one filesystem, and its name is salted with the process id so two concurrent
/// writers to the same path don't clobber each other's temp before renaming.
///
/// NOTE: this does NOT bump the self-write generation counter — callers writing
/// `settings.json` must still call [`note_self_write`] immediately before, so the
/// file-watch task can suppress the daemon's own writes.
pub fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let tmp_name = format!(".{file_name}.{}.tmp", std::process::id());
    let tmp = match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    };
    std::fs::write(&tmp, contents.as_ref())?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup so a failed rename doesn't strand the temp file.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

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
pub const BTN_NORTH: u16 = 0x133; // BTN_X (kernel) -> "X" face
pub const BTN_WEST: u16 = 0x134; // BTN_Y (kernel) -> "Y" face
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
pub const KEY_X: u16 = 45;
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

/// Map a `key <name>` IPC token to the keycode it taps on the shared virtual
/// keyboard. The closed vocabulary is the directions + confirm/cancel the shell
/// navigates by — the *same* keys the gamepad d-pad/A/B already synthesize — so
/// headless automation can drive focus over the one socket (parity with the
/// pad, without the dead `intent:nav-*` broadcast). `select` -> Enter, `back`
/// -> Esc. Returns `None` for an unknown token (the runtime replies
/// `error:unknown key '<name>'`).
pub fn key_for_action(name: &str) -> Option<u16> {
    Some(match name {
        "up" => KEY_UP,
        "down" => KEY_DOWN,
        "left" => KEY_LEFT,
        "right" => KEY_RIGHT,
        "select" => KEY_ENTER,
        "back" => KEY_ESC,
        _ => return None,
    })
}

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
// Combos and tuning constants (ported from the former gamepad-input.py)
// ---------------------------------------------------------------------------

/// Home + B held for COMBO_HOLD_SECS -> `combo:end-session`.
pub const COMBO_KEYS: [u16; 2] = [BTN_MODE, BTN_EAST];
pub const COMBO_HOLD_SECS: f64 = 3.0;

/// Back + Home + LB + RB, instant -> `combo:force-quit`.
pub const QUIT_COMBO_KEYS: [u16; 4] = [BTN_SELECT, BTN_MODE, BTN_TL, BTN_TR];

/// LB + RB + Start, instant -> `combo:suspend-stream` (unless also force-quit).
pub const SUSPEND_COMBO_KEYS: [u16; 3] = [BTN_START, BTN_TL, BTN_TR];

/// Union of every safety combo's key set — the "combo participant" buttons whose
/// *partial* presses must not leak into a focused app while a combo is being
/// discriminated (the combo-buffer safety, #escape-contract). Deduped union of
/// [`QUIT_COMBO_KEYS`], [`SUSPEND_COMBO_KEYS`], and [`COMBO_KEYS`]:
/// {Back, Home, LB, RB, Start, B}. `BTN_MODE` (Home) is a member — its own
/// app-delivery is governed by the Meta tap/hold split, but it still counts
/// toward the per-presenter participant-held arming (see
/// `state::participant_held_count`). A compile-time check below keeps this in
/// sync with the three source arrays.
pub const COMBO_PARTICIPANTS: [u16; 6] =
    [BTN_SELECT, BTN_MODE, BTN_TL, BTN_TR, BTN_START, BTN_EAST];

/// True if `code` is a member of any safety combo (see [`COMBO_PARTICIPANTS`]).
pub fn is_combo_participant(code: u16) -> bool {
    COMBO_PARTICIPANTS.contains(&code)
}

// Compile-time guard: every button in the three combo arrays must appear in
// COMBO_PARTICIPANTS, so adding a key to a combo can't silently bypass the
// buffer. (A `const fn` contains check keeps this a zero-cost invariant.)
const _: () = {
    const fn in_participants(code: u16) -> bool {
        let mut i = 0;
        while i < COMBO_PARTICIPANTS.len() {
            if COMBO_PARTICIPANTS[i] == code {
                return true;
            }
            i += 1;
        }
        false
    }
    const fn all_in(combo: &[u16]) -> bool {
        let mut i = 0;
        while i < combo.len() {
            if !in_participants(combo[i]) {
                return false;
            }
            i += 1;
        }
        true
    }
    assert!(all_in(&QUIT_COMBO_KEYS));
    assert!(all_in(&SUSPEND_COMBO_KEYS));
    assert!(all_in(&COMBO_KEYS));
};

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
/// apps). The legacy IPC doc lists a `drawer` action — that is stale. `altSelect`
/// is the "Y" face (BTN_WEST) and `altAction` the "X" face (BTN_NORTH); the QML
/// `remappableActions` mirrors this set.
///
/// `menu` (Start, `BTN_START`) emits `KEY_TAB` rather than `KEY_ENTER`. Start
/// used to be bound to a `confirm` action that synthesized the *same*
/// `KEY_ENTER` as `select` — a second A button in disguise. `ShellLayout.qml`'s
/// `Keys.onTabPressed` already toggles the nav drawer, so wiring Start to
/// `KEY_TAB` makes it a dedicated, self-toggling menu button for free, with no
/// QML change. `confirm` is retired rather than kept as a bindable-but-unbound
/// action: nothing else uses it, and a dangling default-less action would only
/// invite a confusing remap target (see [`is_default_action`]).
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
            button: BTN_WEST,
            key: KEY_TAB,
        },
        Binding {
            action: "menu",
            button: BTN_START,
            key: KEY_TAB,
        },
        // Secondary face button ("X" face). The shell uses KEY_X as a per-context
        // secondary action (e.g. the home Moonlight Y-menu "set default profile").
        Binding {
            action: "altAction",
            button: BTN_NORTH,
            key: KEY_X,
        },
    ]
}

pub fn is_default_action(action: &str) -> bool {
    matches!(
        action,
        "select" | "back" | "altSelect" | "menu" | "altAction"
    )
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
        BTN_NORTH => "X",
        BTN_WEST => "Y",
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
// Settings persistence (~/.config/tv-shell/settings.json)
// ---------------------------------------------------------------------------

/// Default settings path: `~/.config/tv-shell/settings.json` (legacy
/// `~/.config/game-shell/` honored as a read-fallback via
/// [`tv_shell_protocol::brand::config_dir`], shared with the daemon's
/// `config.toml` resolution so both live in the same directory).
pub fn settings_path() -> PathBuf {
    tv_shell_protocol::brand::config_dir().join("settings.json")
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

/// Write bindings to disk (read existing, modify, write single-line). The
/// read→modify→write is serialized by [`SETTINGS_LOCK`] against a concurrent
/// [`set_config`] so neither loses the other's keys.
pub fn save_bindings(path: &Path, bindings: &[Binding]) -> std::io::Result<()> {
    let _guard = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let existing = std::fs::read_to_string(path).ok();
    let json = build_settings_json(existing.as_deref(), bindings);
    // Bump the self-write generation only AFTER the write succeeds: if the write
    // fails, the file is unchanged, so the watcher must NOT treat a later external
    // edit as daemon-originated (which would suppress a legitimate reload).
    atomic_write(path, json)?;
    note_self_write();
    Ok(())
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

// ---------------------------------------------------------------------------
// Per-player / per-game binding override parsers (#104)
// ---------------------------------------------------------------------------

/// Parse the `perPlayerBindings` override layer from a settings document.
///
/// Reads `settings["perPlayerBindings"]` as an object whose keys are player-slot
/// strings (`"0"`..`"3"`). Each value is an `{action: button_name}` object
/// using the same validation as `apply_binding_overrides`: only the four
/// canonical default actions are accepted, button names are resolved via
/// `button_name_to_code`, and buttons must be remappable. Array values use the
/// last element; unknown/invalid entries are silently skipped.
///
/// Returns an empty map when the key is absent or not an object.
pub fn parse_per_player_bindings(
    settings: &serde_json::Value,
) -> std::collections::HashMap<u8, std::collections::HashMap<&'static str, u16>> {
    let mut out: std::collections::HashMap<u8, std::collections::HashMap<&'static str, u16>> =
        std::collections::HashMap::new();
    let Some(obj) = settings
        .get("perPlayerBindings")
        .and_then(|v| v.as_object())
    else {
        return out;
    };
    for (slot_str, slot_val) in obj {
        let Ok(slot) = slot_str.parse::<u8>() else {
            continue;
        };
        if slot > 3 {
            continue;
        }
        let Some(actions) = slot_val.as_object() else {
            continue;
        };
        let mut slot_map: std::collections::HashMap<&'static str, u16> =
            std::collections::HashMap::new();
        for (action, value) in actions {
            if !is_default_action(action) {
                continue;
            }
            // Resolve the canonical &'static str action key from default_bindings.
            let static_action: &'static str = default_bindings()
                .iter()
                .find(|b| b.action == action.as_str())
                .map(|b| b.action)
                .unwrap_or("");
            if static_action.is_empty() {
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
            slot_map.insert(static_action, code);
        }
        if !slot_map.is_empty() {
            out.insert(slot, slot_map);
        }
    }
    out
}

/// Parse the `perGameBindings` override layer from a settings document.
///
/// Reads `settings["perGameBindings"]` as an object whose keys are arbitrary
/// non-empty game-id strings. Each value is an `{action: button_name}` object
/// using the same validation as `apply_binding_overrides`: only the four
/// canonical default actions are accepted, button names are resolved via
/// `button_name_to_code`, and buttons must be remappable. Array values use the
/// last element; unknown/invalid entries are silently skipped.
///
/// Returns an empty map when the key is absent or not an object.
pub fn parse_per_game_bindings(
    settings: &serde_json::Value,
) -> std::collections::HashMap<String, std::collections::HashMap<&'static str, u16>> {
    let mut out: std::collections::HashMap<String, std::collections::HashMap<&'static str, u16>> =
        std::collections::HashMap::new();
    let Some(obj) = settings.get("perGameBindings").and_then(|v| v.as_object()) else {
        return out;
    };
    for (game_id, game_val) in obj {
        if game_id.is_empty() {
            continue;
        }
        let Some(actions) = game_val.as_object() else {
            continue;
        };
        let mut game_map: std::collections::HashMap<&'static str, u16> =
            std::collections::HashMap::new();
        for (action, value) in actions {
            if !is_default_action(action) {
                continue;
            }
            let static_action: &'static str = default_bindings()
                .iter()
                .find(|b| b.action == action.as_str())
                .map(|b| b.action)
                .unwrap_or("");
            if static_action.is_empty() {
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
            game_map.insert(static_action, code);
        }
        if !game_map.is_empty() {
            out.insert(game_id.clone(), game_map);
        }
    }
    out
}

/// Resolve which keyboard key a `button` press should emit, given the layered
/// binding overrides.
///
/// Resolution order: game override → player override → global (the merged
/// `bindings` vec). The first layer that assigns an *action* to `button` wins.
/// Returns `None` if `button` is not mapped to any action after overlay.
///
/// This is the pure, allocation-light core used by `Shared::resolved_key` in
/// `input.rs`. Keeping it here makes it unit-testable on any host without a
/// uinput device.
pub fn resolve_button_key(
    global: &[Binding],
    player: Option<&std::collections::HashMap<&'static str, u16>>,
    game: Option<&std::collections::HashMap<&'static str, u16>>,
    button: u16,
) -> Option<u16> {
    // At most 4 actions, so compute each action's effective button inline
    // (game → player → global) instead of allocating an overlay map. The first
    // action whose effective button matches `button` wins; its key comes from
    // the global bindings (the key never changes per-player/game — only the
    // button assignment does).
    for b in global {
        let effective = game
            .and_then(|g| g.get(b.action).copied())
            .or_else(|| player.and_then(|p| p.get(b.action).copied()))
            .unwrap_or(b.button);
        if effective == button {
            return Some(b.key);
        }
    }
    None
}

/// Default for the `rumbleEnabled` setting when the key is absent or malformed.
/// Rumble is tasteful and on by default (#99); the user can disable it.
pub const RUMBLE_ENABLED_DEFAULT: bool = true;

/// Default for `cecFocusOnStartup`: off by default so a daemon start/restart
/// never steals the TV input unexpectedly.
pub const CEC_FOCUS_ON_STARTUP_DEFAULT: bool = false;
/// Default for `cecFocusOnWake`: on by default so resume from sleep switches the
/// TV to our input (the expected behaviour when waking the box).
pub const CEC_FOCUS_ON_WAKE_DEFAULT: bool = true;
// Compile-time invariant: the unexpected-steal default is off, the expected
// wake default is on.
const _: () = assert!(!CEC_FOCUS_ON_STARTUP_DEFAULT);
const _: () = assert!(CEC_FOCUS_ON_WAKE_DEFAULT);

/// Default for `cecAutoSwitchOnPowerOn`: off by default so a device powering on
/// never yanks the TV/AVR input unexpectedly. Consumed by the CEC actor's
/// power-on edge detector (`cec::auto_switch_on_power_on`), which applies
/// [`CecBehavior::default_input`] when the display chain comes out of standby.
pub const CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT: bool = false;
const _: () = assert!(!CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT);

/// Value of `cecDefaultInput` meaning "no preferred default input" — the shell
/// writes `-1` when the user clears the per-device "Default ✓" badge. Anything
/// outside the 16 CEC logical addresses (`0..=15`) normalizes to this, so a
/// stale or garbage value degrades to "claim the display for ourselves" rather
/// than addressing a device that cannot exist.
pub const CEC_DEFAULT_INPUT_UNSET: i32 = -1;

/// Read the `cecFocusOnStartup` setting from a parsed settings document. Returns
/// [`CEC_FOCUS_ON_STARTUP_DEFAULT`] when the key is absent or not a JSON bool.
/// Pure (no I/O) — unit-testable on any host.
pub fn cec_focus_on_startup_from(settings: &serde_json::Value) -> bool {
    settings
        .get("cecFocusOnStartup")
        .and_then(|v| v.as_bool())
        .unwrap_or(CEC_FOCUS_ON_STARTUP_DEFAULT)
}

/// Read the `cecFocusOnWake` setting from a parsed settings document. Returns
/// [`CEC_FOCUS_ON_WAKE_DEFAULT`] when the key is absent or not a JSON bool.
/// Pure (no I/O) — unit-testable on any host.
pub fn cec_focus_on_wake_from(settings: &serde_json::Value) -> bool {
    settings
        .get("cecFocusOnWake")
        .and_then(|v| v.as_bool())
        .unwrap_or(CEC_FOCUS_ON_WAKE_DEFAULT)
}

/// Read the `cecFocusOnStartup` setting from `settings.json` on disk. A missing
/// or unparseable file (or a non-bool value) yields [`CEC_FOCUS_ON_STARTUP_DEFAULT`].
pub fn cec_focus_on_startup(path: &Path) -> bool {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(v) => cec_focus_on_startup_from(&v),
        None => CEC_FOCUS_ON_STARTUP_DEFAULT,
    }
}

/// Read the `cecFocusOnWake` setting from `settings.json` on disk. A missing or
/// unparseable file (or a non-bool value) yields [`CEC_FOCUS_ON_WAKE_DEFAULT`].
pub fn cec_focus_on_wake(path: &Path) -> bool {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(v) => cec_focus_on_wake_from(&v),
        None => CEC_FOCUS_ON_WAKE_DEFAULT,
    }
}

/// Read the `cecAutoSwitchOnPowerOn` setting from a parsed settings document.
/// Returns [`CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT`] when the key is absent or not a
/// JSON bool. Pure (no I/O) — unit-testable on any host.
pub fn cec_auto_switch_on_power_on_from(settings: &serde_json::Value) -> bool {
    settings
        .get("cecAutoSwitchOnPowerOn")
        .and_then(|v| v.as_bool())
        .unwrap_or(CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT)
}

/// Read the `cecAutoSwitchOnPowerOn` setting from `settings.json` on disk. A
/// missing or unparseable file (or a non-bool value) yields
/// [`CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT`].
pub fn cec_auto_switch_on_power_on(path: &Path) -> bool {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(v) => cec_auto_switch_on_power_on_from(&v),
        None => CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT,
    }
}

/// Read the `cecDefaultInput` setting from a parsed settings document.
///
/// Returns a CEC logical address in `0..=15`, or [`CEC_DEFAULT_INPUT_UNSET`]
/// when the key is absent, is not a JSON integer, or names an address outside
/// the 16 addressable slots. Pure (no I/O) — unit-testable on any host.
pub fn cec_default_input_from(settings: &serde_json::Value) -> i32 {
    settings
        .get("cecDefaultInput")
        .and_then(|v| v.as_i64())
        .filter(|a| (0..=15).contains(a))
        .map(|a| a as i32)
        .unwrap_or(CEC_DEFAULT_INPUT_UNSET)
}

/// Read the `cecDefaultInput` setting from `settings.json` on disk. A missing or
/// unparseable file (or an out-of-range value) yields [`CEC_DEFAULT_INPUT_UNSET`].
pub fn cec_default_input(path: &Path) -> i32 {
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(v) => cec_default_input_from(&v),
        None => CEC_DEFAULT_INPUT_UNSET,
    }
}

/// The CEC *behaviour* settings the actor consumes, read as one unit so a single
/// `settings.json` parse serves every gate.
///
/// Bundling them is not cosmetic: it is what lets a test go from a settings file
/// on disk all the way to an asserted CEC transmit in one hop. A reader tested in
/// isolation proves only that a getter parses — the failure mode #415 exists to
/// fix — so the consuming decisions ([`should_auto_switch_target`],
/// [`focus_target`]) take this struct rather than loose bools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CecBehavior {
    /// `cecAutoSwitchOnPowerOn` — apply the default input when the display chain
    /// transitions out of standby.
    pub auto_switch_on_power_on: bool,
    /// `cecDefaultInput` — logical address to make the active source, or
    /// [`CEC_DEFAULT_INPUT_UNSET`].
    pub default_input: i32,
}

impl Default for CecBehavior {
    fn default() -> Self {
        Self {
            auto_switch_on_power_on: CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT,
            default_input: CEC_DEFAULT_INPUT_UNSET,
        }
    }
}

impl CecBehavior {
    /// Fold both keys out of an already-parsed settings document. Pure.
    pub fn from_settings(settings: &serde_json::Value) -> Self {
        Self {
            auto_switch_on_power_on: cec_auto_switch_on_power_on_from(settings),
            default_input: cec_default_input_from(settings),
        }
    }

    /// Read both keys from `settings.json` on disk. A missing or unparseable
    /// file yields [`CecBehavior::default`] (auto-switch off, no default input),
    /// so a broken settings file can never *start* driving the CEC bus.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        {
            Some(v) => Self::from_settings(&v),
            None => Self::default(),
        }
    }
}

/// Where a focus action should point the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    /// Claim active source for ourselves — today's behaviour, and what an unset
    /// (or self-addressed) `cecDefaultInput` means.
    Ours,
    /// Hand the display to the device at this logical address instead.
    Device(i32),
}

/// Resolve `cecDefaultInput` against our own logical address.
///
/// `ours` is our own address in `0..=15`, or [`CEC_DEFAULT_INPUT_UNSET`] when
/// libcec cannot tell us. Unset, out-of-range, and self-addressed defaults all
/// collapse to [`FocusTarget::Ours`], so the *default* configuration reproduces
/// the pre-#415 behaviour byte for byte. Pure.
pub fn focus_target(default_input: i32, ours: i32) -> FocusTarget {
    if !(0..=15).contains(&default_input) || default_input == ours {
        FocusTarget::Ours
    } else {
        FocusTarget::Device(default_input)
    }
}

/// Whether an observed power word is a transition INTO powered-on.
///
/// `previous` is the last word observed for that address, or `None` when it has
/// never been seen. A first observation is deliberately NOT an edge: the daemon
/// starting up next to an already-on TV must not read that as "the TV just came
/// on" and steal the input. Only a known non-on -> `on` move counts. Pure.
pub fn powered_on_edge(previous: Option<&str>, current: &str) -> bool {
    current == "on"
        && matches!(
            previous,
            Some("standby") | Some("sleeping") | Some("waking") | Some("unknown")
        )
}

/// Whether a power-on edge on `addr` should trigger the auto-switch.
///
/// `display_chain` names the addresses whose power-on means "the display came
/// up" (the TV and the AVR). A *source* device powering on is claiming the
/// display for itself, not offering it, so it is not a trigger — the caller
/// supplies the list because the CEC module owns those constants. Pure.
pub fn should_auto_switch_target(
    lifecycle_enabled: bool,
    behavior: &CecBehavior,
    display_chain: &[i32],
    addr: i32,
    previous: Option<&str>,
    current: &str,
) -> bool {
    lifecycle_enabled
        && behavior.auto_switch_on_power_on
        && display_chain.contains(&addr)
        && powered_on_edge(previous, current)
}

/// Whether to claim CEC active source: only when the lifecycle master is on AND
/// the relevant focus setting is enabled.
pub fn should_focus(lifecycle_enabled: bool, focus_setting: bool) -> bool {
    lifecycle_enabled && focus_setting
}

/// Read the `rumbleEnabled` setting from a parsed settings document. Returns
/// [`RUMBLE_ENABLED_DEFAULT`] when the key is absent or not a JSON bool, so a
/// stale/garbage value never silently disables rumble. Pure (no I/O) — the
/// caller supplies the parsed document — so it unit-tests on any host.
pub fn rumble_enabled_from(settings: &serde_json::Value) -> bool {
    settings
        .get("rumbleEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(RUMBLE_ENABLED_DEFAULT)
}

/// Read the `rumbleEnabled` setting from `settings.json` on disk. A missing or
/// unparseable file (or a non-bool value) yields [`RUMBLE_ENABLED_DEFAULT`].
pub fn rumble_enabled(path: &Path) -> bool {
    let parsed = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    match parsed {
        Some(v) => rumble_enabled_from(&v),
        None => RUMBLE_ENABLED_DEFAULT,
    }
}

/// Read the `steamServer` setting — the selected `[[steam.hosts]]` name, written
/// by the `steam-set-host` IPC (daemon-owned, like `keyBindings`) — from a
/// parsed settings document. `None` when absent or not a string; the caller
/// falls back to the first configured host, so a stale name never breaks the
/// widget. Pure (no I/O) so it unit-tests on any host.
pub fn steam_server_from(settings: &serde_json::Value) -> Option<String> {
    settings
        .get("steamServer")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Read the `steamServer` setting from `settings.json` on disk. A missing or
/// unparseable file yields `None` (⇒ first configured host).
pub fn steam_server(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    steam_server_from(&parsed)
}

/// Apply a `set-config` update to disk: read existing, merge, write single-line.
/// Returns the new document text on success. `updates` must be a JSON object.
/// The read→modify→write is serialized by [`SETTINGS_LOCK`] against a concurrent
/// [`save_bindings`] so neither loses the other's keys.
pub fn set_config(path: &Path, updates: &serde_json::Value) -> std::io::Result<String> {
    let _guard = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let existing = std::fs::read_to_string(path).ok();
    let merged = merge_config(existing.as_deref(), updates).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "set-config body must be a JSON object",
        )
    })?;
    // Bump the self-write generation only AFTER the write succeeds (see
    // save_bindings): a failed write leaves the file unchanged, so a later
    // external edit must still fire a config:changed reload.
    atomic_write(path, &merged)?;
    note_self_write();
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_are_the_canonical_actions() {
        let b = default_bindings();
        let actions: Vec<&str> = b.iter().map(|x| x.action).collect();
        assert_eq!(
            actions,
            ["select", "back", "altSelect", "menu", "altAction"]
        );
        // No `drawer`, BTN_MODE not bound.
        assert!(!b.iter().any(|x| x.button == BTN_MODE));
    }

    #[test]
    fn start_button_is_menu_not_a_second_confirm() {
        // Regression guard: Start (BTN_START) must NOT emit KEY_ENTER — that
        // would make it a second `select`/A button. It emits KEY_TAB (the nav
        // drawer's existing keyboard gesture), giving it toggle-open/close
        // behavior for free via ShellLayout's Keys.onTabPressed.
        let b = default_bindings();
        let start = b
            .iter()
            .find(|x| x.button == BTN_START)
            .expect("BTN_START must be bound by default");
        assert_eq!(start.action, "menu");
        assert_eq!(start.key, KEY_TAB);
        assert_ne!(start.key, KEY_ENTER);

        // `confirm` is retired: not a recognized action, and not remap-able.
        assert!(!is_default_action("confirm"));
        assert!(is_default_action("menu"));

        // Coherence: every default binding's action is a known default action,
        // and every default action is claimed by exactly one binding.
        for binding in &b {
            assert!(
                is_default_action(binding.action),
                "binding action {:?} not recognized by is_default_action",
                binding.action
            );
        }
        let mut actions: Vec<&str> = b.iter().map(|x| x.action).collect();
        actions.sort_unstable();
        actions.dedup();
        assert_eq!(
            actions.len(),
            b.len(),
            "duplicate action in default_bindings"
        );
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
                "menu": "BTN_LEFT",           // not remappable -> skipped
                "bogus": "BTN_SOUTH"          // unknown action -> skipped
            }
        });
        let mut b = default_bindings();
        apply_binding_overrides(&mut b, &settings);
        let by = |a: &str| b.iter().find(|x| x.action == a).unwrap().button;
        assert_eq!(by("select"), BTN_NORTH);
        assert_eq!(by("back"), BTN_TR);
        assert_eq!(by("menu"), BTN_START); // unchanged (BTN_LEFT not remappable)
        assert_eq!(by("altSelect"), BTN_WEST); // default unchanged (Y face)
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
        assert_eq!(v["keyBindings"]["menu"], "BTN_START");
        // Insertion order preserved: keyBindings appended after existing keys.
        let prefix = r#"{"themeMode":"dark","moonlightViewMode":"grid","keyBindings":"#;
        assert!(out.starts_with(prefix), "got: {out}");
    }

    #[test]
    fn save_with_no_existing_file() {
        let out = build_settings_json(None, &default_bindings());
        assert_eq!(
            out,
            r#"{"keyBindings":{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_WEST","menu":"BTN_START","altAction":"BTN_NORTH"}}"#
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
    fn atomic_write_creates_parent_and_replaces_atomically() {
        // Nested path under a fresh dir: atomic_write must mkdir -p the parent.
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir. Note this is one of the sites
        // `crate::testutil`'s doc calls out as NOT fully hardened: the
        // `mkdir -p` under test here is `atomic_write`'s own (production
        // code), so we can't pre-chmod what it creates without defeating the
        // point of the test.
        let dir = crate::testutil::scratch_path("gs-atomic", "");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("data.json");

        atomic_write(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");

        // Overwrite leaves the new content and no stray temp files behind.
        atomic_write(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_config_then_load_round_trips_on_disk() {
        // Unique temp file (no global env) so this is parallel-safe. See
        // `crate::testutil` for why this is based on `current_exe()` rather
        // than the system temp dir.
        let path = crate::testutil::scratch_path("gs-cfg", ".json");
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

    #[test]
    fn concurrent_set_config_and_save_bindings_lose_no_keys() {
        // M2 regression guard: set_config (IPC handler) and save_bindings (input
        // thread) both read→modify→write settings.json. Without SETTINGS_LOCK their
        // RMW cycles interleave and silently drop the loser's keys. Here many
        // threads each set a *distinct* key while another writes bindings; with the
        // lock, every key and the keyBindings block must survive. See
        // `crate::testutil` for why this is based on `current_exe()` rather
        // than the system temp dir.
        let path = crate::testutil::scratch_path("gs-concurrent", ".json");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "{}").unwrap();

        const WRITERS: usize = 16;
        let mut handles = Vec::new();
        for i in 0..WRITERS {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                let key = format!("key{i}");
                set_config(&p, &serde_json::json!({ key: i })).unwrap();
            }));
        }
        // Interleave a couple of binding writes (the other RMW writer).
        for _ in 0..2 {
            let p = path.clone();
            let bindings = default_bindings();
            handles.push(std::thread::spawn(move || {
                save_bindings(&p, &bindings).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        // Every distinct set_config key survived (none clobbered by a racing write).
        for i in 0..WRITERS {
            assert_eq!(
                v.get(format!("key{i}")).and_then(|n| n.as_u64()),
                Some(i as u64),
                "key{i} lost or wrong; doc = {text}"
            );
        }
        // The bindings block survived too.
        assert!(
            v.get("keyBindings").and_then(|b| b.as_object()).is_some(),
            "keyBindings lost; doc = {text}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rumble_enabled_reads_bool_with_default() {
        // Explicit values pass through.
        assert!(rumble_enabled_from(
            &serde_json::json!({ "rumbleEnabled": true })
        ));
        assert!(!rumble_enabled_from(
            &serde_json::json!({ "rumbleEnabled": false })
        ));
        // Absent key -> default (on).
        assert_eq!(
            rumble_enabled_from(&serde_json::json!({ "themeMode": "dark" })),
            RUMBLE_ENABLED_DEFAULT
        );
        // Non-bool value -> default (never silently disables).
        assert_eq!(
            rumble_enabled_from(&serde_json::json!({ "rumbleEnabled": "yes" })),
            RUMBLE_ENABLED_DEFAULT
        );
        // The default is on (#99): an absent setting should never silence rumble.
        assert!(
            rumble_enabled_from(&serde_json::json!({})),
            "rumble defaults on"
        );
    }

    #[test]
    fn rumble_enabled_from_disk_round_trips() {
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir.
        let path = crate::testutil::scratch_path("gs-rumble", ".json");
        let _ = std::fs::remove_file(&path);
        // Missing file -> default.
        assert_eq!(rumble_enabled(&path), RUMBLE_ENABLED_DEFAULT);
        // Explicit off.
        std::fs::write(&path, r#"{"rumbleEnabled":false}"#).unwrap();
        assert!(!rumble_enabled(&path));
        // Garbage file -> default.
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(rumble_enabled(&path), RUMBLE_ENABLED_DEFAULT);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cec_focus_from_reads_bool_with_default() {
        // cecFocusOnStartup: explicit true/false pass through.
        assert!(cec_focus_on_startup_from(
            &serde_json::json!({ "cecFocusOnStartup": true })
        ));
        assert!(!cec_focus_on_startup_from(
            &serde_json::json!({ "cecFocusOnStartup": false })
        ));
        // Absent key -> startup default (off).
        assert_eq!(
            cec_focus_on_startup_from(&serde_json::json!({ "themeMode": "dark" })),
            CEC_FOCUS_ON_STARTUP_DEFAULT
        );
        // Non-bool -> startup default.
        assert_eq!(
            cec_focus_on_startup_from(&serde_json::json!({ "cecFocusOnStartup": "yes" })),
            CEC_FOCUS_ON_STARTUP_DEFAULT
        );

        // cecFocusOnWake: explicit true/false pass through.
        assert!(cec_focus_on_wake_from(
            &serde_json::json!({ "cecFocusOnWake": true })
        ));
        assert!(!cec_focus_on_wake_from(
            &serde_json::json!({ "cecFocusOnWake": false })
        ));
        // Absent key -> wake default (on).
        assert_eq!(
            cec_focus_on_wake_from(&serde_json::json!({ "themeMode": "dark" })),
            CEC_FOCUS_ON_WAKE_DEFAULT
        );
        // Non-bool -> wake default.
        assert_eq!(
            cec_focus_on_wake_from(&serde_json::json!({ "cecFocusOnWake": "yes" })),
            CEC_FOCUS_ON_WAKE_DEFAULT
        );
    }

    #[test]
    fn cec_focus_from_disk_round_trips() {
        // See `crate::testutil` for why these are based on `current_exe()`
        // rather than the system temp dir.
        let startup_path = crate::testutil::scratch_path("gs-cec-startup", ".json");
        let wake_path = crate::testutil::scratch_path("gs-cec-wake", ".json");
        let _ = std::fs::remove_file(&startup_path);
        let _ = std::fs::remove_file(&wake_path);

        // Missing file -> defaults.
        assert_eq!(
            cec_focus_on_startup(&startup_path),
            CEC_FOCUS_ON_STARTUP_DEFAULT
        );
        assert_eq!(cec_focus_on_wake(&wake_path), CEC_FOCUS_ON_WAKE_DEFAULT);

        // Explicit values honored.
        std::fs::write(&startup_path, r#"{"cecFocusOnStartup":true}"#).unwrap();
        assert!(cec_focus_on_startup(&startup_path));
        std::fs::write(&wake_path, r#"{"cecFocusOnWake":false}"#).unwrap();
        assert!(!cec_focus_on_wake(&wake_path));

        // Garbage file -> defaults.
        std::fs::write(&startup_path, "not json").unwrap();
        assert_eq!(
            cec_focus_on_startup(&startup_path),
            CEC_FOCUS_ON_STARTUP_DEFAULT
        );
        std::fs::write(&wake_path, "not json").unwrap();
        assert_eq!(cec_focus_on_wake(&wake_path), CEC_FOCUS_ON_WAKE_DEFAULT);

        let _ = std::fs::remove_file(&startup_path);
        let _ = std::fs::remove_file(&wake_path);
    }

    #[test]
    fn cec_auto_switch_from_reads_bool_with_default() {
        // Explicit true/false pass through.
        assert!(cec_auto_switch_on_power_on_from(
            &serde_json::json!({ "cecAutoSwitchOnPowerOn": true })
        ));
        assert!(!cec_auto_switch_on_power_on_from(
            &serde_json::json!({ "cecAutoSwitchOnPowerOn": false })
        ));
        // Absent key -> default (off).
        assert_eq!(
            cec_auto_switch_on_power_on_from(&serde_json::json!({ "themeMode": "dark" })),
            CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT
        );
        // Non-bool -> default (off).
        assert_eq!(
            cec_auto_switch_on_power_on_from(
                &serde_json::json!({ "cecAutoSwitchOnPowerOn": "yes" })
            ),
            CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT
        );
    }

    #[test]
    fn cec_auto_switch_from_disk_round_trips() {
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir.
        let path = crate::testutil::scratch_path("gs-cec-autoswitch", ".json");
        let _ = std::fs::remove_file(&path);

        // Missing file -> default (off).
        assert_eq!(
            cec_auto_switch_on_power_on(&path),
            CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT
        );

        // Explicit value honored.
        std::fs::write(&path, r#"{"cecAutoSwitchOnPowerOn":true}"#).unwrap();
        assert!(cec_auto_switch_on_power_on(&path));

        // Garbage file -> default (off).
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            cec_auto_switch_on_power_on(&path),
            CEC_AUTO_SWITCH_ON_POWER_ON_DEFAULT
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cec_default_input_from_reads_addresses_and_rejects_the_rest() {
        // Every addressable slot round-trips.
        for addr in 0..=15i64 {
            assert_eq!(
                cec_default_input_from(&serde_json::json!({ "cecDefaultInput": addr })),
                addr as i32
            );
        }
        // The shell's explicit "cleared" sentinel, an out-of-range address, a
        // non-integer, and an absent key all mean "unset".
        for doc in [
            serde_json::json!({ "cecDefaultInput": -1 }),
            serde_json::json!({ "cecDefaultInput": 16 }),
            serde_json::json!({ "cecDefaultInput": "4" }),
            serde_json::json!({ "themeMode": "dark" }),
        ] {
            assert_eq!(
                cec_default_input_from(&doc),
                CEC_DEFAULT_INPUT_UNSET,
                "doc {doc}"
            );
        }
    }

    #[test]
    fn cec_behavior_loads_both_keys_from_disk() {
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir.
        let path = crate::testutil::scratch_path("gs-cec-behavior", ".json");
        let _ = std::fs::remove_file(&path);

        // Missing file -> the inert default: no auto-switch, no default input.
        assert_eq!(CecBehavior::load(&path), CecBehavior::default());

        std::fs::write(
            &path,
            r#"{"cecAutoSwitchOnPowerOn":true,"cecDefaultInput":4}"#,
        )
        .unwrap();
        assert_eq!(
            CecBehavior::load(&path),
            CecBehavior {
                auto_switch_on_power_on: true,
                default_input: 4,
            }
        );

        // Garbage file -> inert default, never a half-read that drives the bus.
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(CecBehavior::load(&path), CecBehavior::default());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn focus_target_collapses_unset_and_self_to_ours() {
        let cases = [
            // (default_input, ours, expected)
            (CEC_DEFAULT_INPUT_UNSET, 4, FocusTarget::Ours),
            (99, 4, FocusTarget::Ours),
            (4, 4, FocusTarget::Ours),
            // A real other device is the only case that redirects.
            (0, 4, FocusTarget::Device(0)),
            (5, 4, FocusTarget::Device(5)),
            // Our own address undeterminable: a configured device still wins,
            // because "we don't know who we are" is not "the user meant us".
            (5, CEC_DEFAULT_INPUT_UNSET, FocusTarget::Device(5)),
        ];
        for (default_input, ours, expected) in cases {
            assert_eq!(
                focus_target(default_input, ours),
                expected,
                "focus_target({default_input}, {ours})"
            );
        }
    }

    #[test]
    fn powered_on_edge_needs_a_known_non_on_predecessor() {
        let cases = [
            // (previous, current, expected)
            (Some("standby"), "on", true),
            (Some("sleeping"), "on", true),
            (Some("waking"), "on", true),
            (Some("unknown"), "on", true),
            // Already on -> not an edge (a repeated poll must not re-fire).
            (Some("on"), "on", false),
            // Never observed -> not an edge: a daemon starting beside an
            // already-on TV must not read that as "it just came on".
            (None, "on", false),
            // Any non-on current state is never an edge.
            (Some("on"), "standby", false),
            (None, "standby", false),
        ];
        for (previous, current, expected) in cases {
            assert_eq!(
                powered_on_edge(previous, current),
                expected,
                "powered_on_edge({previous:?}, {current})"
            );
        }
    }

    #[test]
    fn should_auto_switch_target_requires_every_gate() {
        let on = CecBehavior {
            auto_switch_on_power_on: true,
            default_input: CEC_DEFAULT_INPUT_UNSET,
        };
        let off = CecBehavior::default();
        let chain = [0, 5];
        let cases = [
            // (lifecycle, behavior, addr, previous, current, expected)
            (true, on, 0, Some("standby"), "on", true),
            (true, on, 5, Some("standby"), "on", true),
            // The setting is the gate this issue exists to install.
            (true, off, 0, Some("standby"), "on", false),
            // The lifecycle master flag still outranks it.
            (false, on, 0, Some("standby"), "on", false),
            // A source device powering on is claiming the display, not offering it.
            (true, on, 4, Some("standby"), "on", false),
            // No edge -> nothing to react to.
            (true, on, 0, Some("on"), "on", false),
            (true, on, 0, None, "on", false),
        ];
        for (lifecycle, behavior, addr, previous, current, expected) in cases {
            assert_eq!(
                should_auto_switch_target(lifecycle, &behavior, &chain, addr, previous, current),
                expected,
                "should_auto_switch_target({lifecycle}, {behavior:?}, {addr}, {previous:?}, {current})"
            );
        }
    }

    #[test]
    fn should_focus_truth_table() {
        // Table-driven so the inputs aren't compile-time constants (a literal
        // `assert!(should_focus(true, true))` trips clippy's assertions_on_constants).
        let cases = [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ];
        for (lifecycle, setting, want) in cases {
            assert_eq!(
                should_focus(lifecycle, setting),
                want,
                "should_focus({lifecycle}, {setting})"
            );
        }
    }

    #[test]
    fn parse_per_player_bindings_valid_and_skips_invalid() {
        let settings = serde_json::json!({
            "perPlayerBindings": {
                "0": {"select": "BTN_NORTH"},        // slot 0: remap select -> Y
                "1": {"back": "BTN_WEST"},            // slot 1: remap back -> X
                "bogus": {"select": "BTN_SOUTH"},     // non-numeric key -> skipped
                "9": {"select": "BTN_SOUTH"},         // out-of-range slot -> skipped
                "2": {
                    "select": "BTN_LEFT",             // BTN_LEFT not remappable -> skipped
                    "bogusAction": "BTN_SOUTH"        // unknown action -> skipped
                }
            }
        });
        let result = parse_per_player_bindings(&settings);
        // Slot 0: select -> BTN_NORTH
        assert_eq!(
            result.get(&0).and_then(|m| m.get("select")).copied(),
            Some(BTN_NORTH)
        );
        // Slot 1: back -> BTN_WEST
        assert_eq!(
            result.get(&1).and_then(|m| m.get("back")).copied(),
            Some(BTN_WEST)
        );
        // Slot 2: both entries invalid -> not present (or empty, not inserted)
        assert!(!result.contains_key(&2));
        // Bogus/out-of-range slots not present
        assert!(!result.contains_key(&9));

        // Absent key -> empty map
        let empty = parse_per_player_bindings(&serde_json::json!({}));
        assert!(empty.is_empty());
    }

    #[test]
    fn parse_per_game_bindings_valid_and_skips_invalid() {
        let settings = serde_json::json!({
            "perGameBindings": {
                "steam_12345": {"select": "BTN_NORTH"},
                "steam_99999": {"back": ["BTN_WEST", "BTN_TR"]},  // array -> last (BTN_TR)
                "": {"select": "BTN_SOUTH"},                       // empty key -> skipped
                "bad_game": {"select": "BTN_LEFT"}                 // not remappable -> skipped (slot not inserted)
            }
        });
        let result = parse_per_game_bindings(&settings);
        assert_eq!(
            result
                .get("steam_12345")
                .and_then(|m| m.get("select"))
                .copied(),
            Some(BTN_NORTH)
        );
        assert_eq!(
            result
                .get("steam_99999")
                .and_then(|m| m.get("back"))
                .copied(),
            Some(BTN_TR)
        );
        assert!(!result.contains_key(""));
        assert!(!result.contains_key("bad_game"));

        // Absent key -> empty map
        let empty = parse_per_game_bindings(&serde_json::json!({}));
        assert!(empty.is_empty());
    }

    #[test]
    fn resolve_button_key_layering() {
        use std::collections::HashMap;

        let global = default_bindings();
        // Default: select=BTN_SOUTH, back=BTN_EAST, altSelect=BTN_WEST, menu=BTN_START, altAction=BTN_NORTH

        // No overrides: global mapping
        assert_eq!(
            resolve_button_key(&global, None, None, BTN_SOUTH),
            Some(KEY_ENTER) // select -> Enter
        );
        assert_eq!(
            resolve_button_key(&global, None, None, BTN_EAST),
            Some(KEY_ESC) // back -> Esc
        );
        // altSelect lives on the "Y" face (BTN_WEST) -> Tab
        assert_eq!(
            resolve_button_key(&global, None, None, BTN_WEST),
            Some(KEY_TAB)
        );
        // altAction lives on the "X" face (BTN_NORTH) -> KEY_X
        assert_eq!(
            resolve_button_key(&global, None, None, BTN_NORTH),
            Some(KEY_X)
        );
        // A button not assigned to any action -> None
        assert_eq!(resolve_button_key(&global, None, None, BTN_THUMBL), None);

        // Player override: remap select from BTN_SOUTH to BTN_TL (LB).
        // BTN_TL is remappable and not used by any default action.
        let mut player: HashMap<&'static str, u16> = HashMap::new();
        player.insert("select", BTN_TL);
        // BTN_TL now triggers select -> KEY_ENTER
        assert_eq!(
            resolve_button_key(&global, Some(&player), None, BTN_TL),
            Some(KEY_ENTER)
        );
        // Original BTN_SOUTH no longer triggers select (displaced by player)
        assert_eq!(
            resolve_button_key(&global, Some(&player), None, BTN_SOUTH),
            None
        );

        // Game override: wins over player. Game remaps select to BTN_TR (RB).
        let mut game: HashMap<&'static str, u16> = HashMap::new();
        game.insert("select", BTN_TR);
        // Game wins: BTN_TR triggers select -> KEY_ENTER
        assert_eq!(
            resolve_button_key(&global, Some(&player), Some(&game), BTN_TR),
            Some(KEY_ENTER)
        );
        // Player's BTN_TL is displaced by the game layer
        assert_eq!(
            resolve_button_key(&global, Some(&player), Some(&game), BTN_TL),
            None
        );
        // BTN_SOUTH is still displaced (no layer reassigns it to select)
        assert_eq!(
            resolve_button_key(&global, Some(&player), Some(&game), BTN_SOUTH),
            None
        );

        // Fall through: game layer absent -> player wins
        assert_eq!(
            resolve_button_key(&global, Some(&player), None, BTN_TL),
            Some(KEY_ENTER)
        );

        // No layers -> global default. Start (menu) emits KEY_TAB, NOT
        // KEY_ENTER — it must not be a second `select`/A button.
        assert_eq!(
            resolve_button_key(&global, None, None, BTN_START),
            Some(KEY_TAB) // menu -> Tab (toggles the nav drawer)
        );

        // Button not in any action after all layers -> None
        assert_eq!(resolve_button_key(&global, None, None, BTN_THUMBL), None);
    }

    #[test]
    fn note_self_write_advances_generation() {
        // note_self_write() must strictly advance the counter each call.
        // Uses a local snapshot so parallel tests don't interfere.
        let before = self_write_gen();
        note_self_write();
        let after = self_write_gen();
        assert!(
            after > before,
            "generation must advance after note_self_write"
        );
        // A second call advances again.
        note_self_write();
        assert!(self_write_gen() > after, "each note_self_write increments");
    }
}

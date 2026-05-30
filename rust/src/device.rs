//! Gamepad / keyboard discovery.
//!
//! Replaces the Python daemon's hardcoded Xbox `0x045e:0x028e` vendor/product
//! match with SDL-style controller identification: compute the SDL joystick
//! GUID from the device's `input_id` and check membership in the bundled
//! `SDL_GameControllerDB`. This lets the daemon grab an *arbitrary* known
//! controller, not just the Xbox pad.
//!
//! The GUID math and DB parsing are platform-independent and unit-tested; the
//! actual `/dev/input/event*` enumeration and grab live behind `cfg(linux)`.

use std::collections::HashSet;

/// Compute the 16-byte SDL joystick GUID for a Linux device.
///
/// Layout (matches SDL's `SDL_CreateJoystickGUID` on Linux):
/// `bus(LE16) | crc(LE16) | vendor(LE16) | 0 0 | product(LE16) | 0 0 | version(LE16) | 0 0`.
/// We leave the CRC field zero (we don't hash the name); DB matching ignores it.
pub fn sdl_guid(bus: u16, vendor: u16, product: u16, version: u16) -> [u8; 16] {
    let mut g = [0u8; 16];
    g[0..2].copy_from_slice(&bus.to_le_bytes());
    // g[2..4] crc -> left zero
    g[4..6].copy_from_slice(&vendor.to_le_bytes());
    g[8..10].copy_from_slice(&product.to_le_bytes());
    g[12..14].copy_from_slice(&version.to_le_bytes());
    g
}

/// Lowercase 32-char hex rendering of a GUID, as used in `gamecontrollerdb.txt`.
pub fn guid_to_string(guid: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in guid {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse a 32-char hex GUID string into bytes. Returns `None` if malformed.
fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut g = [0u8; 16];
    for (i, byte) in g.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(g)
}

fn vendor_of(guid: &[u8; 16]) -> u16 {
    u16::from_le_bytes([guid[4], guid[5]])
}

fn product_of(guid: &[u8; 16]) -> u16 {
    u16::from_le_bytes([guid[8], guid[9]])
}

/// A parsed controller database: the set of known `(vendor, product)` pairs.
///
/// We match on vendor/product rather than full-GUID equality: the bus and
/// version fields and the optional CRC vary between how a device presents and
/// how the DB recorded it, but vendor/product reliably identifies a controller
/// model. Entries with a zero vendor (SDL name-encoded GUIDs) are ignored.
#[derive(Debug, Default, Clone)]
pub struct ControllerDb {
    known: HashSet<(u16, u16)>,
}

impl ControllerDb {
    pub fn parse(text: &str) -> ControllerDb {
        let mut known = HashSet::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(first) = line.split(',').next() else {
                continue;
            };
            let Some(guid) = parse_guid(first) else {
                continue;
            };
            let (v, p) = (vendor_of(&guid), product_of(&guid));
            if v != 0 {
                known.insert((v, p));
            }
        }
        ControllerDb { known }
    }

    pub fn is_known(&self, vendor: u16, product: u16) -> bool {
        self.known.contains(&(vendor, product))
    }

    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// The bundled baseline DB (common controllers). A fuller upstream
/// `SDL_GameControllerDB` can be supplied at runtime via the
/// `GAME_SHELL_GAMECONTROLLERDB` env var (see `load_db`).
const BUNDLED_DB: &str = include_str!("../assets/gamecontrollerdb.txt");

/// Load the controller DB: an operator-supplied file (env override) layered
/// over the bundled baseline.
pub fn load_db() -> ControllerDb {
    let mut db = ControllerDb::parse(BUNDLED_DB);
    if let Some(path) = std::env::var_os("GAME_SHELL_GAMECONTROLLERDB") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let extra = ControllerDb::parse(&text);
            db.known.extend(extra.known);
        }
    }
    db
}

/// Parse a `GAMEPAD_VENDOR`/`GAMEPAD_PRODUCT`-style override (supports `0x`
/// hex, like Python's `int(x, 0)`). Returns `None` if unset/unparseable.
pub fn parse_id_env(var: &str) -> Option<u16> {
    let raw = std::env::var(var).ok()?;
    let raw = raw.trim();
    let parsed = if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16)
    } else {
        raw.parse::<u16>()
    };
    parsed.ok()
}

// ---------------------------------------------------------------------------
// Linux device enumeration / grab
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub use linux::{find_gamepad, find_keyboards, GamepadHandle};

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::config;
    use evdev::{Device, KeyCode};
    use std::path::PathBuf;

    /// A discovered gamepad: its evdev device plus its display name/path.
    pub struct GamepadHandle {
        pub device: Device,
        pub name: String,
        pub path: PathBuf,
    }

    fn has_btn_south(dev: &Device) -> bool {
        dev.supported_keys()
            .is_some_and(|keys| keys.contains(KeyCode::BTN_SOUTH))
    }

    /// Find a gamepad. Selection order:
    /// 1. If both `GAMEPAD_VENDOR` and `GAMEPAD_PRODUCT` are set -> exact match
    ///    (legacy operator pin).
    /// 2. Else prefer the first BTN_SOUTH device whose vendor/product is in the
    ///    controller DB.
    /// 3. Else fall back to the first BTN_SOUTH device (arbitrary controller).
    pub fn find_gamepad(db: &ControllerDb) -> Option<GamepadHandle> {
        let pin_vendor = parse_id_env("GAMEPAD_VENDOR");
        let pin_product = parse_id_env("GAMEPAD_PRODUCT");
        let pinned = matches!((pin_vendor, pin_product), (Some(_), Some(_)));

        let mut devices: Vec<(PathBuf, Device)> = evdev::enumerate().collect();
        // Deterministic order (mirrors Python's sorted(list_devices())).
        devices.sort_by(|a, b| a.0.cmp(&b.0));

        let mut fallback: Option<GamepadHandle> = None;
        for (path, dev) in devices {
            if !has_btn_south(&dev) {
                continue;
            }
            let id = dev.input_id();
            let (vendor, product) = (id.vendor(), id.product());

            if pinned {
                if Some(vendor) == pin_vendor && Some(product) == pin_product {
                    return Some(make_handle(dev, path));
                }
                continue;
            }

            if db.is_known(vendor, product) {
                return Some(make_handle(dev, path));
            }
            if fallback.is_none() {
                fallback = Some(make_handle(dev, path));
            }
        }
        fallback
    }

    fn make_handle(dev: Device, path: PathBuf) -> GamepadHandle {
        let name = dev.name().unwrap_or("unknown").to_string();
        GamepadHandle {
            device: dev,
            name,
            path,
        }
    }

    /// Keyboard-like devices for read-only snooping: have `KEY_A`, lack
    /// `BTN_SOUTH`, and are not our own virtual devices or ydotoold's.
    pub fn find_keyboards() -> Vec<GamepadHandle> {
        let mut devices: Vec<(PathBuf, Device)> = evdev::enumerate().collect();
        devices.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::new();
        for (path, dev) in devices {
            let name = dev.name().unwrap_or("").to_string();
            if name.starts_with("game-shell-virtual-") || name.contains("ydotoold") {
                continue;
            }
            let Some(keys) = dev.supported_keys() else {
                continue;
            };
            // `KeyCode::new(KEY_A)` — config::KEY_A is the kernel code.
            if keys.contains(KeyCode::new(config::KEY_A)) && !keys.contains(KeyCode::BTN_SOUTH) {
                out.push(make_handle(dev, path));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbox360_guid_matches_known_sdl_string() {
        // Wired Xbox 360 controller on Linux (bus=USB(3), vendor=0x045e,
        // product=0x028e, version=0x0110). This is the canonical SDL GUID.
        let guid = sdl_guid(0x0003, 0x045e, 0x028e, 0x0110);
        assert_eq!(guid_to_string(&guid), "030000005e0400008e02000010010000");
    }

    #[test]
    fn guid_roundtrip_extracts_vendor_product() {
        let guid = sdl_guid(0x0003, 0x045e, 0x028e, 0x0110);
        let s = guid_to_string(&guid);
        let back = parse_guid(&s).unwrap();
        assert_eq!(vendor_of(&back), 0x045e);
        assert_eq!(product_of(&back), 0x028e);
    }

    #[test]
    fn db_parse_and_match() {
        let db_text = "\
# comment line
030000005e0400008e02000010010000,Xbox 360 Controller,a:b0,b:b1,platform:Linux,
030000004c050000c405000011810000,PS4 Controller,a:b1,platform:Linux,
00000000000000000000000000000000,Zero vendor (name-encoded),platform:Linux,
";
        let db = ControllerDb::parse(db_text);
        assert_eq!(db.len(), 2); // zero-vendor entry skipped
        assert!(db.is_known(0x045e, 0x028e)); // Xbox 360
        assert!(db.is_known(0x054c, 0x05c4)); // PS4 (vendor 4c05 LE -> 0x054c)
        assert!(!db.is_known(0x1234, 0x5678));
    }

    #[test]
    fn bundled_db_loads() {
        let db = ControllerDb::parse(BUNDLED_DB);
        // Baseline should contain the common Xbox 360 pad.
        assert!(db.is_known(0x045e, 0x028e));
    }

    #[test]
    fn load_db_includes_baseline() {
        // No env override set -> just the bundled baseline.
        std::env::remove_var("GAME_SHELL_GAMECONTROLLERDB");
        let db = load_db();
        assert!(db.is_known(0x045e, 0x028e));
        assert!(db.is_known(0x054c, 0x09cc)); // DualShock 4 v2
    }

    #[test]
    fn id_env_parsing() {
        std::env::set_var("TEST_GP_VENDOR_HEX", "0x045e");
        std::env::set_var("TEST_GP_VENDOR_DEC", "1118");
        assert_eq!(parse_id_env("TEST_GP_VENDOR_HEX"), Some(0x045e));
        assert_eq!(parse_id_env("TEST_GP_VENDOR_DEC"), Some(1118));
        assert_eq!(parse_id_env("TEST_GP_UNSET_XYZ"), None);
        std::env::remove_var("TEST_GP_VENDOR_HEX");
        std::env::remove_var("TEST_GP_VENDOR_DEC");
    }
}

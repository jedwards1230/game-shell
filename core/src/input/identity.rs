//! Device identity: the SDL GUID math, the controller database, the stable wire
//! id, and the player-slot allocator.
//!
//! Ported from `daemon/src/device.rs`, whose pure half has been debugged against
//! this exact hardware. It is **copied rather than shared** for the same reason
//! `core.toml` is a separate file from v1's `config.toml` (V2_DESIGN §11:
//! "beside, not instead, at every shared layer") — a crate-crossing
//! `include_str!` or `pub use` would make an edit made for v1 silently change
//! what v2 claims off the couch, which is the coupling the v2 split exists to
//! avoid.
//!
//! Everything in this file is pure: no `/dev`, no evdev, no syscall. It compiles
//! and its tests run on any host, including in CI where there is no seat.

use std::collections::HashSet;

/// Compute the 16-byte SDL joystick GUID for a Linux device.
///
/// Layout (matches SDL's `SDL_CreateJoystickGUID` on Linux):
/// `bus(LE16) | crc(LE16) | vendor(LE16) | 0 0 | product(LE16) | 0 0 | version(LE16) | 0 0`.
/// The CRC field is left zero (we do not hash the name); DB matching ignores it.
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
///
/// This is a diagnostic: an operator copies it out of a log line straight into a
/// database entry to teach the core a controller the bundled baseline does not
/// know.
pub fn guid_to_string(guid: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(32);
    for b in guid {
        // `write!` to a String cannot fail; the result is discarded rather than
        // unwrapped so this stays panic-free on a purely infallible path.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parse a 32-char hex GUID string into bytes. `None` if malformed.
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
/// Matching is on vendor/product rather than full-GUID equality: the bus and
/// version fields and the optional CRC vary between how a device presents and
/// how the DB recorded it, but vendor/product reliably identifies a controller
/// model. Entries with a zero vendor (SDL name-encoded GUIDs) are ignored — they
/// identify a device by hashed name, which this crate never computes, so
/// admitting them would make every zero-vendor device match every other one.
#[derive(Debug, Default, Clone)]
pub struct ControllerDb {
    known: HashSet<(u16, u16)>,
}

impl ControllerDb {
    /// Parse `gamecontrollerdb.txt` text. Malformed lines are skipped, not
    /// fatal: an operator-supplied database with one bad row must still teach
    /// the core about its good ones.
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

    /// Union another database into this one.
    pub fn merge(&mut self, other: &ControllerDb) {
        self.known.extend(other.known.iter().copied());
    }
}

/// The bundled baseline database (common controllers).
///
/// A fuller upstream `SDL_GameControllerDB` is layered over it at runtime via
/// `[input].controller_db`.
const BUNDLED_DB: &str = include_str!("../../assets/gamecontrollerdb.txt");

/// The baseline database, with no file I/O.
pub fn bundled_db() -> ControllerDb {
    ControllerDb::parse(BUNDLED_DB)
}

/// Derive a stable wire id for a pad from its evdev identity.
///
/// Preference order, most-to-least stable:
///   1. `uniq` (evdev "unique name" — a controller serial / BT MAC) when present;
///   2. `phys` (the physical port/path the device hangs off) when present;
///   3. `vp:<vendor>:<product>:<path>` as a last resort — the path keeps two
///      identical pads on different ports distinct even when neither exposes
///      `uniq` or `phys`.
///
/// Empty `uniq`/`phys` strings (the kernel reports `""` rather than absent for
/// some devices) are treated as missing. The id exists so a UI can follow one
/// physical pad across reconnects; the core's own in-process key is the devnode
/// path, never this.
pub fn derive_wire_id(
    uniq: Option<&str>,
    phys: Option<&str>,
    vendor: u16,
    product: u16,
    path: &str,
) -> String {
    if let Some(u) = uniq.map(str::trim).filter(|s| !s.is_empty()) {
        return format!("uniq:{u}");
    }
    if let Some(p) = phys.map(str::trim).filter(|s| !s.is_empty()) {
        return format!("phys:{p}");
    }
    format!("vp:{vendor:04x}:{product:04x}:{path}")
}

/// Stable player-slot allocator.
///
/// Each claimed physical pad gets a small stable player index (`0` = P1, `1` =
/// P2, …). The allocator hands out the **lowest free index** on join and returns
/// it to the free pool on leave, so a freed index is reused by the next
/// connecting pad. This is what keeps P1 = P1 across a P2 reconnect: with slots
/// 0 and 1 taken, P2 unplugging frees 1, and the replug finds 0 still held and
/// takes 1 again.
///
/// The allocator is bounded by `capacity`: [`alloc`](Self::alloc) returns `None`
/// once every slot is held. The bound is the presenter count, because a pad with
/// no presenter has nothing to be re-presented on — v1's allocator was unbounded
/// and its `checked_add` panic was the only thing between it and a `u8`
/// overflow.
#[derive(Debug, Clone)]
pub struct SlotAllocator {
    used: HashSet<u8>,
    capacity: u8,
}

impl SlotAllocator {
    pub fn new(capacity: u8) -> SlotAllocator {
        SlotAllocator {
            used: HashSet::new(),
            capacity,
        }
    }

    /// Allocate the lowest free slot, or `None` when the fleet is full.
    pub fn alloc(&mut self) -> Option<u8> {
        (0..self.capacity)
            .find(|i| !self.used.contains(i))
            .inspect(|i| {
                self.used.insert(*i);
            })
    }

    /// Return a slot to the free pool. Idempotent.
    pub fn free(&mut self, idx: u8) {
        self.used.remove(&idx);
    }

    pub fn is_used(&self, idx: u8) -> bool {
        self.used.contains(&idx)
    }

    pub fn capacity(&self) -> u8 {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.used.len()
    }

    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Xbox 360 wired pad, as SDL renders it: bus 3 (USB), 045e:028e.
    const XBOX360_GUID: &str = "030000005e0400008e02000010010000";

    #[test]
    fn guid_layout_matches_sdl() {
        let g = sdl_guid(3, 0x045e, 0x028e, 0x0110);
        assert_eq!(guid_to_string(&g), XBOX360_GUID);
        assert_eq!(vendor_of(&g), 0x045e);
        assert_eq!(product_of(&g), 0x028e);
    }

    #[test]
    fn guid_round_trips_through_its_string_form() {
        let g = sdl_guid(3, 0x054c, 0x09cc, 0x8111);
        assert_eq!(parse_guid(&guid_to_string(&g)), Some(g));
    }

    #[test]
    fn malformed_guids_are_rejected() {
        assert_eq!(parse_guid(""), None);
        assert_eq!(parse_guid("0300"), None);
        // 32 chars but not hex.
        assert_eq!(parse_guid(&"z".repeat(32)), None);
        // 33 chars.
        assert_eq!(parse_guid(&"0".repeat(33)), None);
    }

    /// **Rule: a zero-vendor (SDL name-hashed) entry is never admitted.**
    ///
    /// Such a GUID identifies a device by a hash of its name, which this crate
    /// never computes. Admitting it would put `(0, 0)` in the set, and then
    /// every device whose vendor and product both read zero — which is what an
    /// unidentified virtual device commonly reports — would match.
    #[test]
    fn zero_vendor_entries_are_ignored() {
        let db = ControllerDb::parse("00000000000000000000000000000000,Some Pad,a:b1,\n");
        assert!(db.is_empty());
        assert!(!db.is_known(0, 0));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped_and_a_bad_row_is_not_fatal() {
        let db = ControllerDb::parse(&format!(
            "# a comment\n\nnot-a-guid,Bad Row,\n{XBOX360_GUID},X360,a:b1,\n"
        ));
        assert_eq!(db.len(), 1);
        assert!(db.is_known(0x045e, 0x028e));
    }

    #[test]
    fn the_bundled_db_is_non_empty_and_knows_the_xbox_pad() {
        let db = bundled_db();
        assert!(!db.is_empty(), "the bundled baseline must parse");
        assert!(db.is_known(0x045e, 0x028e));
    }

    #[test]
    fn merge_is_a_union() {
        let mut a = ControllerDb::parse(&format!("{XBOX360_GUID},X360,\n"));
        let b = ControllerDb::parse("03000000c82d00000631000014010000,8BitDo,\n");
        a.merge(&b);
        assert_eq!(a.len(), 2);
        assert!(a.is_known(0x045e, 0x028e));
        assert!(a.is_known(0x2dc8, 0x3106));
    }

    /// **Rule: the wire id prefers `uniq`, then `phys`, then vendor/product+path.**
    #[test]
    fn wire_id_preference_order() {
        assert_eq!(
            derive_wire_id(Some("AA:BB"), Some("usb-1"), 1, 2, "/dev/input/event3"),
            "uniq:AA:BB"
        );
        assert_eq!(
            derive_wire_id(None, Some("usb-1"), 1, 2, "/dev/input/event3"),
            "phys:usb-1"
        );
        assert_eq!(
            derive_wire_id(None, None, 0x045e, 0x028e, "/dev/input/event3"),
            "vp:045e:028e:/dev/input/event3"
        );
    }

    /// The kernel reports `""` rather than absent for some devices, so an empty
    /// string must fall through as if it were missing — otherwise every such pad
    /// gets the id `uniq:` and two of them collide.
    #[test]
    fn empty_uniq_and_phys_fall_through() {
        assert_eq!(
            derive_wire_id(Some("  "), Some(""), 1, 2, "/dev/input/event3"),
            "vp:0001:0002:/dev/input/event3"
        );
    }

    #[test]
    fn slots_are_handed_out_lowest_first() {
        let mut s = SlotAllocator::new(4);
        assert_eq!(s.alloc(), Some(0));
        assert_eq!(s.alloc(), Some(1));
        assert_eq!(s.alloc(), Some(2));
        assert_eq!(s.len(), 3);
    }

    /// **Rule: P1 keeps slot 0 across a P2 unplug/replug.**
    ///
    /// The whole point of the allocator. If `free` did not return the index to
    /// the pool, or if `alloc` did not scan from zero, the replugging pad would
    /// take a fresh index and every player would drift a seat over on each
    /// reconnect.
    #[test]
    fn a_reconnecting_pad_does_not_displace_the_players_below_it() {
        let mut s = SlotAllocator::new(4);
        let p1 = s.alloc().unwrap();
        let p2 = s.alloc().unwrap();
        assert_eq!((p1, p2), (0, 1));
        s.free(p2);
        assert!(s.is_used(0), "P1 must keep its slot");
        assert_eq!(s.alloc(), Some(1), "the replug takes the freed slot back");
    }

    /// **Rule: LOWEST free, not next-after-the-highest.**
    ///
    /// Freeing the top slot cannot tell these apart — both hand it straight
    /// back. The distinguishing case is a hole in the MIDDLE: P1 leaves while
    /// P2 and P3 stay, and the next pad must take seat 0, not seat 3. Without
    /// this case an allocator that simply counted upward from the high-water
    /// mark passed every other test here, and would have silently stranded a
    /// presenter per departed player.
    #[test]
    fn a_freed_slot_below_the_high_water_mark_is_reused_first() {
        let mut s = SlotAllocator::new(4);
        assert_eq!(s.alloc(), Some(0));
        assert_eq!(s.alloc(), Some(1));
        assert_eq!(s.alloc(), Some(2));

        s.free(0);
        assert_eq!(s.alloc(), Some(0), "the hole below the top must be filled");
        assert_eq!(s.alloc(), Some(3), "and only then does it grow");

        // And with two holes, still lowest-first.
        s.free(2);
        s.free(1);
        assert_eq!(s.alloc(), Some(1));
        assert_eq!(s.alloc(), Some(2));
    }

    /// **Rule: the allocator is bounded by capacity and reports exhaustion.**
    ///
    /// A pad with no slot has no presenter to be re-presented on, so the fleet
    /// must be told rather than handed a slot that indexes past the presenters.
    #[test]
    fn allocation_is_bounded_by_capacity() {
        let mut s = SlotAllocator::new(2);
        assert_eq!(s.alloc(), Some(0));
        assert_eq!(s.alloc(), Some(1));
        assert_eq!(
            s.alloc(),
            None,
            "a full fleet must refuse, not wrap or panic"
        );
        s.free(0);
        assert_eq!(s.alloc(), Some(0));
    }

    #[test]
    fn freeing_an_unallocated_slot_is_a_no_op() {
        let mut s = SlotAllocator::new(4);
        s.free(3);
        assert!(s.is_empty());
        assert_eq!(s.alloc(), Some(0));
    }

    #[test]
    fn a_zero_capacity_allocator_hands_out_nothing() {
        let mut s = SlotAllocator::new(0);
        assert_eq!(s.alloc(), None);
    }
}

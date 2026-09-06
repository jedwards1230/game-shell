//! Typed X11 atom layer for gamescope's published state.
//!
//! Under gamescope's SteamControlled focus policy the whole kiosk contract is a
//! set of root-window properties plus a few per-window ones (V2_DESIGN §5 table).
//! This module is the ONLY place in the core that speaks X: everything above it
//! sees typed values, never `u32` blobs and never atom names.
//!
//! Three rules are baked into the API rather than left to callers:
//!
//! 1. **A missing atom is a normal absent value, never an error.** gamescope
//!    creates most of these lazily — `GAMESCOPE_VRR_FEEDBACK` simply does not
//!    exist before the first VRR evaluation, and an unset
//!    `GAMESCOPECTRL_BASELAYER_APPID` is the ordinary boot state. Every read
//!    returns `Ok(None)` / `Ok(vec![])` for absent, and reserves `Err` for a
//!    connection or protocol failure. This is the direct lesson of v1's
//!    "silent success" class inverted: absence must be *representable*, so it is
//!    never confused with failure and never papered over.
//! 2. **Every property is a 32-bit id array.** gamescope writes these as 32-bit
//!    arrays (`steamcompmgr.cpp`), including the booleans, which are `0`/`1`.
//!    The **width is the invariant**; the accepted types are `CARDINAL` and
//!    `WINDOW` (see [`decode_cardinals`] for why both). A read that finds any
//!    other width or type is a typed error, not a coerced value — a shape we did
//!    not expect is exactly the #448 failure (unit fixtures asserting the shape
//!    the code wanted rather than the bytes the compositor sends). A reply that
//!    is only *part* of the property is the same class of failure and is
//!    likewise an error ([`AtomError::Truncated`]).
//! 3. **Atoms are interned once**, at connect, so a read is one round trip and
//!    a name typo fails at startup rather than at the first switch.

use std::collections::HashMap;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, PropMode, Window, WindowClass};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// A gamescope app id.
///
/// Newtype rather than a bare `u32` because §5 turns on *which* id is meant: the
/// id of the focused **window** is the one every rule keys on, and a bare
/// integer makes it trivially easy to hand it the wrong one. See
/// [`crate::screen::ScreenState`].
/// The field is **private**. A `pub` field made
/// `AppId(state.focused_app_atom_diagnostic().unwrap())` compile — laundering the
/// diagnostic-only `GAMESCOPE_FOCUSED_APP` value, which §5 records as reading
/// empty under an input-focus overlay, into a first-class app id. Nothing did
/// that, but nothing stopped it either. With [`AppId::new`] the launder still
/// exists as a possibility, but it has to be *written*, which is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct AppId(u32);

impl AppId {
    /// Wrap a raw gamescope app id.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The raw id, for the wire and for formatting.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for AppId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.parse::<u32>().map(AppId::new)
    }
}

/// Everything that can go wrong talking to X, as typed variants.
///
/// `Missing` is deliberately absent: a missing property is `Ok(None)`, not an
/// error (see the module docs).
#[derive(Debug, thiserror::Error)]
pub enum AtomError {
    #[error("connecting to the X display: {0}")]
    Connect(String),
    #[error("X protocol error reading {atom}: {source}")]
    Protocol {
        atom: &'static str,
        #[source]
        source: x11rb::errors::ReplyError,
    },
    #[error("X connection error on {atom}: {source}")]
    Connection {
        atom: &'static str,
        #[source]
        source: x11rb::errors::ConnectionError,
    },
    /// The property exists but is not the 32-bit id array every one of these
    /// atoms is documented (and measured) to be. Never coerced — see rule 2 in
    /// the module docs.
    #[error(
        "{atom} has unexpected format: {width}-bit, type atom {type_atom} \
         (expected a 32-bit CARDINAL or WINDOW array)"
    )]
    BadFormat {
        atom: &'static str,
        width: u8,
        type_atom: Atom,
    },
    /// The server had more of this property than the reply carried.
    ///
    /// Every read here asks for `long_length = u32::MAX`, so this is practically
    /// unreachable — but "practically unreachable" is how silent truncation gets
    /// shipped, and this is the one module whose whole thesis is that a
    /// truncated read must be an error rather than a shorter-but-plausible
    /// answer. Named explicitly so a future partial read fails loudly.
    #[error("{atom} was truncated: {bytes_after} bytes remained unread after the reply")]
    Truncated {
        atom: &'static str,
        bytes_after: u32,
    },
    /// A `(xid, appid, pid)` triplet array whose length is not a multiple of 3.
    #[error(
        "{atom} holds {len} values, which is not a whole number of (xid, appid, pid) triplets"
    )]
    BadTripletLen { atom: &'static str, len: usize },
}

impl AtomError {
    /// Is this a `BadWindow` for a window that no longer exists?
    ///
    /// The one legitimate race in a two-round-trip read: `screen::read` learns a
    /// window id from `GAMESCOPE_FOCUSED_WINDOW` and then asks that window for
    /// its `STEAM_GAME`, and the window can be destroyed in between. Every OTHER
    /// error on that second trip (a `BadFormat`, a `Truncated`) is a shape
    /// mismatch, and swallowing those is how #448 happened — so only this one is
    /// treated as absence.
    pub fn is_bad_window(&self) -> bool {
        use x11rb::errors::ReplyError;
        use x11rb::protocol::ErrorKind;
        matches!(
            self,
            AtomError::Protocol {
                source: ReplyError::X11Error(e),
                ..
            } if e.error_kind == ErrorKind::Window
        )
    }
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, AtomError>;

/// The atom names the core reads or writes.
///
/// Kept as one list so [`Atoms::intern`] interns them in a single round trip and
/// a rename upstream fails loudly at startup instead of silently reading absent.
pub mod names {
    // --- root: base-layer policy (core writes) ---
    /// Ordered app-id list; the first id with a mapped window is on screen.
    pub const BASELAYER_APPID: &str = "GAMESCOPECTRL_BASELAYER_APPID";
    /// Window-id list pinned across known transient unmaps (§5 hysteresis).
    pub const BASELAYER_WINDOW: &str = "GAMESCOPECTRL_BASELAYER_WINDOW";

    // --- root: published focus result (gamescope writes) ---
    /// **The** base window. Every focus rule keys on this window's app id.
    pub const FOCUSED_WINDOW: &str = "GAMESCOPE_FOCUSED_WINDOW";
    /// Diagnostic only — reads empty under an input-focus overlay (measured).
    pub const FOCUSED_APP: &str = "GAMESCOPE_FOCUSED_APP";
    /// Flat `(xid, appid, pid)` triplets for every focus candidate.
    pub const FOCUSABLE_WINDOWS: &str = "GAMESCOPE_FOCUSABLE_WINDOWS";
    /// Flat app-id list of the focus candidates.
    pub const FOCUSABLE_APPS: &str = "GAMESCOPE_FOCUSABLE_APPS";

    // --- root: display feedback (gamescope writes) ---
    /// `EDID HDR10 && hdr_enabled`. Zeroes for ~1 s across an HDMI hotplug (§6).
    pub const HDR_OUTPUT_FEEDBACK: &str = "GAMESCOPE_HDR_OUTPUT_FEEDBACK";
    /// 1 while VRR is actually active on the output.
    pub const VRR_FEEDBACK: &str = "GAMESCOPE_VRR_FEEDBACK";
    /// 1 when the connected display advertises HDR at all.
    pub const DISPLAY_SUPPORTS_HDR: &str = "GAMESCOPE_DISPLAY_SUPPORTS_HDR";

    // --- per Xwayland-server root ---
    /// Identifies which Xwayland server a given root window belongs to.
    pub const XWAYLAND_SERVER_ID: &str = "GAMESCOPE_XWAYLAND_SERVER_ID";

    // --- per window ---
    /// App id override for a window whose cgroup scope did not resolve (§5
    /// "scope first, tag as repair"). Authoritative when present.
    pub const STEAM_GAME: &str = "STEAM_GAME";
    /// Marks a toplevel as an overlay: drawn over the base window without
    /// changing the base layer.
    pub const STEAM_OVERLAY: &str = "STEAM_OVERLAY";
    /// An overlay that also takes keyboard and mouse focus.
    pub const STEAM_INPUT_FOCUS: &str = "STEAM_INPUT_FOCUS";
    /// A transient toast: drawn, never focused.
    pub const STEAM_NOTIFICATION: &str = "STEAM_NOTIFICATION";

    /// Every name above, in one array, for interning and for tests that assert
    /// the list has not silently lost a member.
    pub const ALL: &[&str] = &[
        BASELAYER_APPID,
        BASELAYER_WINDOW,
        FOCUSED_WINDOW,
        FOCUSED_APP,
        FOCUSABLE_WINDOWS,
        FOCUSABLE_APPS,
        HDR_OUTPUT_FEEDBACK,
        VRR_FEEDBACK,
        DISPLAY_SUPPORTS_HDR,
        XWAYLAND_SERVER_ID,
        STEAM_GAME,
        STEAM_OVERLAY,
        STEAM_INPUT_FOCUS,
        STEAM_NOTIFICATION,
    ];
}

/// One focus candidate as gamescope publishes it.
///
/// `GAMESCOPE_FOCUSABLE_WINDOWS` is a flat CARDINAL array of `(xid, appid, pid)`
/// triplets. That shape is not guessed: it is the live reading recorded in
/// `dev/gamescope/lib.sh` from a scoped launch — `8388625, 9003, 2998` for a
/// process launched into `app-steam-app9003-2970.scope`. The fixtures in this
/// crate's tests use those exact bytes (the #448 lesson: assert the compositor's
/// shape, not the one the code would prefer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FocusableWindow {
    pub window: Window,
    pub app_id: AppId,
    pub pid: u32,
}

/// Interned atom ids, resolved once at connect.
#[derive(Debug, Clone)]
pub struct Atoms(HashMap<&'static str, Atom>);

impl Atoms {
    /// Intern every name in [`names::ALL`] in one round trip.
    ///
    /// `only_if_exists = false`: the core WRITES several of these before
    /// gamescope has ever created them, so they must be minted rather than
    /// resolved. An atom that exists only because we interned it still reads as
    /// absent on a window until something sets it, which is exactly rule 1.
    pub fn intern(conn: &RustConnection) -> Result<Self> {
        let cookies: Vec<_> = names::ALL
            .iter()
            .map(|name| {
                conn.intern_atom(false, name.as_bytes())
                    .map_err(|source| AtomError::Connection {
                        atom: "intern",
                        source,
                    })
            })
            .collect::<Result<_>>()?;
        let mut map = HashMap::with_capacity(names::ALL.len());
        for (name, cookie) in names::ALL.iter().zip(cookies) {
            let reply = cookie.reply().map_err(|source| AtomError::Protocol {
                atom: "intern",
                source,
            })?;
            map.insert(*name, reply.atom);
        }
        Ok(Self(map))
    }

    /// The interned id for a name from [`names`].
    ///
    /// Panics only on a name that is not in [`names::ALL`], which is a
    /// programming error this crate's `every_name_is_in_all_so_get_can_never_panic`
    /// test rules out (it fails if a `pub const` in [`names`] is missing from
    /// `ALL`, which is the only way that panic becomes reachable).
    pub fn get(&self, name: &'static str) -> Atom {
        *self
            .0
            .get(name)
            .unwrap_or_else(|| panic!("atom {name} was never interned; add it to names::ALL"))
    }
}

/// A live X connection with the gamescope atoms interned.
pub struct AtomConn {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
}

impl AtomConn {
    /// Connect to `$DISPLAY` (or `display` when given) and intern the atoms.
    pub fn connect(display: Option<&str>) -> Result<Self> {
        let (conn, screen_num) =
            RustConnection::connect(display).map_err(|e| AtomError::Connect(e.to_string()))?;
        let root = conn.setup().roots[screen_num].root;
        let atoms = Atoms::intern(&conn)?;
        Ok(Self { conn, root, atoms })
    }

    /// The default screen's root window — where every root atom in §5 lives.
    pub fn root(&self) -> Window {
        self.root
    }

    /// The interned atom table, for callers batching their own requests.
    pub fn atoms(&self) -> &Atoms {
        &self.atoms
    }

    /// The underlying connection, for [`crate::screen`]'s batched read.
    pub fn conn(&self) -> &RustConnection {
        &self.conn
    }

    // -- reads ---------------------------------------------------------------

    /// Read a 32-bit id array (`CARDINAL` or `WINDOW`). Absent ⇒ `Ok(vec![])`.
    ///
    /// `long_length = u32::MAX` asks for the whole property in one reply, which
    /// is what makes [`AtomError::Truncated`] unreachable in practice rather
    /// than merely unlikely.
    pub fn read_cardinals(&self, window: Window, name: &'static str) -> Result<Vec<u32>> {
        let cookie = self
            .conn
            .get_property(
                false,
                window,
                self.atoms.get(name),
                AtomEnum::ANY,
                0,
                u32::MAX,
            )
            .map_err(|source| AtomError::Connection { atom: name, source })?;
        let reply = cookie
            .reply()
            .map_err(|source| AtomError::Protocol { atom: name, source })?;
        decode_cardinals(
            name,
            reply.format,
            reply.type_,
            reply.bytes_after,
            &reply.value,
        )
    }

    /// Read a single 32-bit CARDINAL. Absent ⇒ `Ok(None)`.
    ///
    /// A property holding several values yields its first: gamescope writes
    /// scalars as one-element arrays, and a longer array here means the caller
    /// asked a list atom for a scalar, which is its bug, not a wire error.
    pub fn read_cardinal(&self, window: Window, name: &'static str) -> Result<Option<u32>> {
        Ok(self.read_cardinals(window, name)?.first().copied())
    }

    /// Read a CARDINAL that gamescope uses as a boolean (`0`/`1`).
    ///
    /// Absent ⇒ `Ok(None)` — "gamescope has not published this yet" is a
    /// genuinely different state from "published, and false". §6's hotplug
    /// window turns on that distinction.
    pub fn read_flag(&self, window: Window, name: &'static str) -> Result<Option<bool>> {
        Ok(self.read_cardinal(window, name)?.map(|v| v != 0))
    }

    /// Read an app-id list. Absent ⇒ empty.
    pub fn read_app_ids(&self, window: Window, name: &'static str) -> Result<Vec<AppId>> {
        Ok(self
            .read_cardinals(window, name)?
            .into_iter()
            .map(AppId::new)
            .collect())
    }

    /// Read the `(xid, appid, pid)` triplet array. Absent ⇒ empty.
    pub fn read_focusable_windows(&self) -> Result<Vec<FocusableWindow>> {
        decode_focusable_windows(
            names::FOCUSABLE_WINDOWS,
            &self.read_cardinals(self.root, names::FOCUSABLE_WINDOWS)?,
        )
    }

    // -- writes --------------------------------------------------------------

    /// Replace a 32-bit CARDINAL array, then flush.
    ///
    /// The flush is not optional: a base-layer write that sits in the output
    /// buffer while the caller reads back `GAMESCOPE_FOCUSED_WINDOW` would time
    /// out against a switch that was never sent — a silent-success shape by
    /// construction.
    pub fn write_cardinals(
        &self,
        window: Window,
        name: &'static str,
        values: &[u32],
    ) -> Result<()> {
        self.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms.get(name),
                AtomEnum::CARDINAL,
                values,
            )
            .map_err(|source| AtomError::Connection { atom: name, source })?
            .check()
            .map_err(|source| AtomError::Protocol { atom: name, source })?;
        self.conn
            .flush()
            .map_err(|source| AtomError::Connection { atom: name, source })
    }

    /// Write an app-id list.
    pub fn write_app_ids(&self, window: Window, name: &'static str, ids: &[AppId]) -> Result<()> {
        let raw: Vec<u32> = ids.iter().map(|a| a.0).collect();
        self.write_cardinals(window, name, &raw)
    }

    /// Delete a property. Deleting an absent property is a no-op, not an error.
    pub fn delete(&self, window: Window, name: &'static str) -> Result<()> {
        self.conn
            .delete_property(window, self.atoms.get(name))
            .map_err(|source| AtomError::Connection { atom: name, source })?
            .check()
            .map_err(|source| AtomError::Protocol { atom: name, source })?;
        self.conn
            .flush()
            .map_err(|source| AtomError::Connection { atom: name, source })
    }

    // -- typed root accessors ------------------------------------------------

    /// The ordered base-layer app-id list the core last wrote (or Steam did).
    pub fn base_layer(&self) -> Result<Vec<AppId>> {
        self.read_app_ids(self.root, names::BASELAYER_APPID)
    }

    /// Replace the base-layer app-id list. One write — see
    /// [`crate::baselayer`] for the verify half.
    pub fn set_base_layer(&self, ids: &[AppId]) -> Result<()> {
        self.write_app_ids(self.root, names::BASELAYER_APPID, ids)
    }

    /// The pinned base-layer window list (§5 transient-unmap hysteresis).
    pub fn base_layer_windows(&self) -> Result<Vec<Window>> {
        self.read_cardinals(self.root, names::BASELAYER_WINDOW)
    }

    /// **The** base window. Absent ⇒ nothing is on screen yet.
    pub fn focused_window(&self) -> Result<Option<Window>> {
        self.read_cardinal(self.root, names::FOCUSED_WINDOW)
    }

    // -- typed per-window accessors -----------------------------------------

    /// A window's `STEAM_GAME` tag, if it carries one.
    pub fn window_app_id(&self, window: Window) -> Result<Option<AppId>> {
        Ok(self
            .read_cardinal(window, names::STEAM_GAME)?
            .map(AppId::new))
    }

    /// Write `STEAM_GAME` on a window. This is the §5 REPAIR path, only for a
    /// window whose cgroup scope did not resolve — never the primary mechanism.
    pub fn tag_window(&self, window: Window, app_id: AppId) -> Result<()> {
        self.write_cardinals(window, names::STEAM_GAME, &[app_id.0])
    }

    /// Create an unmapped 1x1 window on the root, used by tests (and later by
    /// the forced-paint heartbeat) as somewhere to put per-window properties.
    pub fn create_probe_window(&self) -> Result<Window> {
        let win = self
            .conn
            .generate_id()
            .map_err(|e| AtomError::Connect(e.to_string()))?;
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                win,
                self.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &Default::default(),
            )
            .map_err(|source| AtomError::Connection {
                atom: "create_window",
                source,
            })?
            .check()
            .map_err(|source| AtomError::Protocol {
                atom: "create_window",
                source,
            })?;
        Ok(win)
    }
}

/// Decode a `get_property` reply body into 32-bit values.
///
/// Split out from the connection so it is unit-testable against real reply
/// bytes with no X server. Absent is `format == 0` (X's own encoding for "no
/// such property"), which maps to an empty vector, never an error. A non-zero
/// `bytes_after` — the server holding more of the property than the reply
/// carried — is [`AtomError::Truncated`], never a short answer.
///
/// # Why `WINDOW` is accepted alongside `CARDINAL`
///
/// The **width is the invariant here; the type is not measured per-atom yet.**
/// Everything this crate has actually observed is 32-bit, and every value in
/// these properties is a 32-bit id either way — `XA_WINDOW` and `XA_CARDINAL`
/// differ in what the id *means*, not in how it is encoded. But
/// `GAMESCOPE_FOCUSED_WINDOW` and `GAMESCOPECTRL_BASELAYER_WINDOW` hold window
/// ids, and `WINDOW` is an entirely plausible type for a compositor to publish
/// them as. Rejecting it would make every [`crate::screen::read`] hard-fail on a
/// perfectly healthy compositor — a strictness that turns into an outage
/// instead of a diagnosis.
///
/// So this accepts both 32-bit id types and still rejects every other type and
/// every width but 32. What settles it properly is the §10 headless-gamescope
/// job: that is where the real per-atom types get measured, and this list gets
/// narrowed (or widened) against bytes rather than against reasoning.
pub fn decode_cardinals(
    atom: &'static str,
    format: u8,
    type_: Atom,
    bytes_after: u32,
    value: &[u8],
) -> Result<Vec<u32>> {
    if format == 0 {
        // No such property. Rule 1: absent is a value.
        return Ok(Vec::new());
    }
    let accepted_type =
        type_ == u32::from(AtomEnum::CARDINAL) || type_ == u32::from(AtomEnum::WINDOW);
    if format != 32 || !accepted_type {
        return Err(AtomError::BadFormat {
            atom,
            width: format,
            type_atom: type_,
        });
    }
    if bytes_after != 0 {
        return Err(AtomError::Truncated { atom, bytes_after });
    }
    Ok(value
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Split a flat CARDINAL array into `(xid, appid, pid)` triplets.
///
/// A length that is not a multiple of 3 is an error, not a truncation: it means
/// the compositor's shape is not the one measured, and quietly dropping the
/// remainder is how a wrong shape becomes an invisible wrong answer (#448).
pub fn decode_focusable_windows(atom: &'static str, raw: &[u32]) -> Result<Vec<FocusableWindow>> {
    if raw.len() % 3 != 0 {
        return Err(AtomError::BadTripletLen {
            atom,
            len: raw.len(),
        });
    }
    Ok(raw
        .chunks_exact(3)
        .map(|c| FocusableWindow {
            window: c[0],
            app_id: AppId(c[1]),
            pid: c[2],
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live reading recorded in `dev/gamescope/lib.sh` for a process
    /// launched into `app-steam-app9003-2970.scope`. Real compositor bytes.
    const MEASURED_TRIPLET: [u32; 3] = [8388625, 9003, 2998];

    fn x11_error(kind: x11rb::protocol::ErrorKind) -> AtomError {
        AtomError::Protocol {
            atom: names::STEAM_GAME,
            source: x11rb::errors::ReplyError::X11Error(x11rb::x11_utils::X11Error {
                error_kind: kind,
                error_code: 3,
                sequence: 1,
                bad_value: 0x800011,
                minor_opcode: 0,
                major_opcode: 20,
                extension_name: None,
                request_name: None,
            }),
        }
    }

    #[test]
    fn only_a_bad_window_counts_as_a_window_that_went_away() {
        assert!(x11_error(x11rb::protocol::ErrorKind::Window).is_bad_window());
        // Everything else on that round trip is a shape mismatch, and swallowing
        // one is how #448 happened.
        for other in [
            x11rb::protocol::ErrorKind::Value,
            x11rb::protocol::ErrorKind::Atom,
            x11rb::protocol::ErrorKind::Access,
        ] {
            assert!(!x11_error(other).is_bad_window(), "{other:?}");
        }
        assert!(!AtomError::Truncated {
            atom: names::STEAM_GAME,
            bytes_after: 4
        }
        .is_bad_window());
        assert!(!AtomError::BadFormat {
            atom: names::STEAM_GAME,
            width: 8,
            type_atom: 6
        }
        .is_bad_window());
    }

    fn cardinal_bytes(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    #[test]
    fn all_names_are_unique_and_non_empty() {
        let mut seen = std::collections::HashSet::new();
        for name in names::ALL {
            assert!(!name.is_empty());
            assert!(seen.insert(*name), "duplicate atom name {name}");
        }
        assert_eq!(seen.len(), names::ALL.len());
    }

    #[test]
    fn every_name_is_in_all_so_get_can_never_panic() {
        // The failure this test exists to catch is adding a `pub const` to
        // `names` and forgetting to add it to `names::ALL`: `Atoms::intern`
        // would never intern it and `Atoms::get` would panic at the first read.
        // Listing the constants explicitly here is the point — a new name has to
        // be written down twice, and the second time is this assertion.
        let declared = [
            names::BASELAYER_APPID,
            names::BASELAYER_WINDOW,
            names::FOCUSED_WINDOW,
            names::FOCUSED_APP,
            names::FOCUSABLE_WINDOWS,
            names::FOCUSABLE_APPS,
            names::HDR_OUTPUT_FEEDBACK,
            names::VRR_FEEDBACK,
            names::DISPLAY_SUPPORTS_HDR,
            names::XWAYLAND_SERVER_ID,
            names::STEAM_GAME,
            names::STEAM_OVERLAY,
            names::STEAM_INPUT_FOCUS,
            names::STEAM_NOTIFICATION,
        ];
        let declared: std::collections::HashSet<&str> = declared.into_iter().collect();
        let interned: std::collections::HashSet<&str> = names::ALL.iter().copied().collect();
        assert_eq!(
            declared, interned,
            "every atom name must be in names::ALL, and ALL must hold nothing else"
        );
    }

    #[test]
    fn absent_property_decodes_to_empty_not_error() {
        // X encodes "no such property" as format 0 with an empty value.
        let got = decode_cardinals(names::VRR_FEEDBACK, 0, 0, 0, &[]).unwrap();
        assert!(got.is_empty(), "a missing atom must be an absent value");
    }

    #[test]
    fn cardinal_array_round_trips() {
        let values = [1u32, 9001, 769];
        let got = decode_cardinals(
            names::BASELAYER_APPID,
            32,
            u32::from(AtomEnum::CARDINAL),
            0,
            &cardinal_bytes(&values),
        )
        .unwrap();
        assert_eq!(got, values);
    }

    #[test]
    fn a_window_typed_property_is_accepted_like_a_cardinal_one() {
        // GAMESCOPE_FOCUSED_WINDOW holds window ids, and WINDOW is a plausible
        // type for a compositor to publish them as. Both are 32-bit ids; the
        // width is the invariant. Rejecting WINDOW would hard-fail every
        // screen::read on a healthy compositor.
        let got = decode_cardinals(
            names::FOCUSED_WINDOW,
            32,
            u32::from(AtomEnum::WINDOW),
            0,
            &cardinal_bytes(&[8_388_625]),
        )
        .unwrap();
        assert_eq!(got, vec![8_388_625]);
    }

    #[test]
    fn wrong_width_is_an_error_not_a_coercion() {
        let err = decode_cardinals(
            names::FOCUSED_WINDOW,
            16,
            u32::from(AtomEnum::CARDINAL),
            0,
            &[0, 0],
        )
        .unwrap_err();
        assert!(
            matches!(err, AtomError::BadFormat { width: 16, .. }),
            "{err}"
        );
    }

    #[test]
    fn wrong_type_is_an_error_not_a_coercion() {
        // Still exactly two accepted types: anything else is refused.
        for bad in [AtomEnum::STRING, AtomEnum::ATOM, AtomEnum::PIXMAP] {
            let err = decode_cardinals(
                names::FOCUSED_WINDOW,
                32,
                u32::from(bad),
                0,
                &cardinal_bytes(&[1]),
            )
            .unwrap_err();
            assert!(matches!(err, AtomError::BadFormat { .. }), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_truncated_reply_is_an_error_not_a_short_answer() {
        // bytes_after != 0 means the server has more of this property than the
        // reply carried. Returning the prefix would be silent truncation inside
        // the one module whose thesis is that truncation must be an error.
        let err = decode_cardinals(
            names::FOCUSABLE_WINDOWS,
            32,
            u32::from(AtomEnum::CARDINAL),
            12,
            &cardinal_bytes(&[8_388_625, 9003, 2998]),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                AtomError::Truncated {
                    bytes_after: 12,
                    ..
                }
            ),
            "{err}"
        );
        assert!(
            err.to_string().contains("GAMESCOPE_FOCUSABLE_WINDOWS"),
            "{err}"
        );
    }

    #[test]
    fn focusable_windows_decode_the_measured_triplet() {
        let got = decode_focusable_windows(names::FOCUSABLE_WINDOWS, &MEASURED_TRIPLET).unwrap();
        assert_eq!(
            got,
            vec![FocusableWindow {
                window: 8388625,
                app_id: AppId(9003),
                pid: 2998
            }]
        );
    }

    #[test]
    fn focusable_windows_decode_several_triplets() {
        let raw = [8388625u32, 9003, 2998, 8390000, 769, 4242];
        let got = decode_focusable_windows(names::FOCUSABLE_WINDOWS, &raw).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].app_id, AppId(769));
        assert_eq!(got[1].pid, 4242);
    }

    #[test]
    fn focusable_windows_empty_is_empty_not_an_error() {
        assert!(decode_focusable_windows(names::FOCUSABLE_WINDOWS, &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn ragged_triplet_array_is_an_error_not_a_truncation() {
        // Four values is one triplet plus a stray — silently dropping the stray
        // is how a shape mismatch becomes an invisible wrong answer (#448).
        let err = decode_focusable_windows(names::FOCUSABLE_WINDOWS, &[1, 2, 3, 4]).unwrap_err();
        assert!(
            matches!(err, AtomError::BadTripletLen { len: 4, .. }),
            "{err}"
        );
    }

    #[test]
    fn app_id_display_and_parse_round_trip() {
        let id: AppId = "9001".parse().unwrap();
        assert_eq!(id, AppId(9001));
        assert_eq!(id.to_string(), "9001");
        assert!("nine thousand".parse::<AppId>().is_err());
        assert!("-1".parse::<AppId>().is_err());
    }
}

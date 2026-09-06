//! `ScreenState` — one snapshot of what the compositor says is on screen.
//!
//! This replaces v1's `hypr-active` / `hypr-clients` / `hypr-monitors` with a
//! single read of gamescope's published atoms, and it is the "compositor-agnostic
//! focus source" the rest of the v2 plan sits on (V2_DESIGN §5).
//!
//! # The one rule this type exists to make unbreakable
//!
//! **The base window is `GAMESCOPE_FOCUSED_WINDOW`, and every rule keys on that
//! window's app id — never on `GAMESCOPE_FOCUSED_APP`.** `_APP` was *measured*
//! to read empty while an input-focus overlay (a drawer, the QAM) is mapped, so
//! a rule keyed on it silently decides "nothing is running" precisely when the
//! user has opened the menu over a live game. That is v1's failure class exactly
//! — the compositor answering about a different object than the one you asked
//! about — and it must not be reachable by accident.
//!
//! So the type does not expose `_APP` as an app id at all. [`ScreenState::on_screen`]
//! is the only accessor that returns "what is on screen", and it is derived from
//! the focused *window*. `_APP` survives only as
//! [`ScreenState::focused_app_atom_diagnostic`], whose name and doc say it is for
//! logs and metrics and is never a decision input. A caller cannot pick the wrong
//! one, because the wrong one is not an [`AppId`]-shaped thing in the public API.

use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Window};

use crate::atoms::{
    decode_cardinals, decode_focusable_windows, names, AppId, AtomConn, AtomError, FocusableWindow,
    Result,
};

/// How the app id of the focused window was resolved.
///
/// Recorded because §5's "scope first, tag as repair" is an invariant the field
/// assertions check: a core-launched window resolving via `SteamGame` means its
/// cgroup scope did not resolve, which is a repair, not the normal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppIdSource {
    /// gamescope itself published the id in its `GAMESCOPE_FOCUSABLE_WINDOWS`
    /// triplet for this window — i.e. it resolved from the cgroup scope or from
    /// a tag it already accepted. The normal path.
    Focusable,
    /// Read from the window's own `STEAM_GAME` property. The repair path.
    SteamGame,
}

/// The app id of the base window, with its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct OnScreen {
    pub window: Window,
    pub app_id: AppId,
    pub source: AppIdSource,
}

/// HDR/VRR feedback as gamescope publishes it.
///
/// Each field is `Option<bool>` on purpose: absent means gamescope has not
/// published the atom yet, which §6 distinguishes from a published `false`
/// (an HDMI hotplug zeroes these for ~1 s, and a Vulkan surface created inside
/// that window stays SDR for its life).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct DisplayFeedback {
    /// `GAMESCOPE_HDR_OUTPUT_FEEDBACK` — EDID HDR10 && hdr_enabled.
    pub hdr_output: Option<bool>,
    /// `GAMESCOPE_DISPLAY_SUPPORTS_HDR` — the display advertises HDR at all.
    pub supports_hdr: Option<bool>,
    /// `GAMESCOPE_VRR_FEEDBACK` — VRR is active on the output.
    pub vrr: Option<bool>,
}

/// One consistent snapshot of the compositor's published screen state.
///
/// Read with [`read`], which issues every `GetProperty` before awaiting any
/// reply, so the whole snapshot costs one round trip rather than one per atom.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreenState {
    /// `GAMESCOPE_FOCUSED_WINDOW` — the base window. `None` ⇒ nothing mapped.
    pub focused_window: Option<Window>,
    /// The base-layer app-id list as it currently reads back. Under Steam this
    /// is Steam's list, not ours (§9): the core reconciles after Steam's writes.
    pub base_layer: Vec<AppId>,
    /// `GAMESCOPECTRL_BASELAYER_WINDOW` — windows pinned across transient unmaps.
    pub base_layer_windows: Vec<Window>,
    /// Every focus candidate gamescope knows, as `(xid, appid, pid)`.
    pub focusable_windows: Vec<FocusableWindow>,
    /// `GAMESCOPE_FOCUSABLE_APPS`.
    pub focusable_apps: Vec<AppId>,
    /// HDR/VRR feedback (§6).
    pub display: DisplayFeedback,
    /// `GAMESCOPE_XWAYLAND_SERVER_ID` of the server this snapshot was read from.
    pub xwayland_server_id: Option<u32>,

    /// Resolved base-window app id. `None` ⇒ a window is focused but no id
    /// resolved for it, which is itself an assertable fault (§10's "no untagged
    /// core-launched toplevel").
    on_screen: Option<OnScreen>,

    /// `GAMESCOPE_FOCUSED_APP`, kept as a RAW number, never as an [`AppId`].
    ///
    /// See the module docs: this reads empty under an input-focus overlay, so it
    /// is a diagnostic signal only. It is serialized under a name that says so.
    #[serde(rename = "focused_app_atom_diagnostic")]
    focused_app_atom: Option<u32>,
}

impl ScreenState {
    /// What is on screen, resolved from the focused **window**.
    ///
    /// This is the only "what is running" accessor, and the only correct input
    /// to a focus, audio-ownership, or input-contract decision.
    pub fn on_screen(&self) -> Option<OnScreen> {
        self.on_screen
    }

    /// The app id of the base window, if one resolved.
    pub fn on_screen_app(&self) -> Option<AppId> {
        self.on_screen.map(|o| o.app_id)
    }

    /// `GAMESCOPE_FOCUSED_APP` as a raw number, **for logs and metrics only**.
    ///
    /// Never branch on this. It reads empty while an input-focus overlay is
    /// mapped (measured), so a rule keyed on it decides "nothing is running"
    /// exactly when a menu is open over a live game. Use [`Self::on_screen`].
    /// Its value is worth *recording*: a disagreement with [`Self::on_screen_app`]
    /// outside an open overlay is a real signal that something changed upstream.
    pub fn focused_app_atom_diagnostic(&self) -> Option<u32> {
        self.focused_app_atom
    }

    /// True when `_APP` and the focused window's id disagree.
    ///
    /// Expected, and benign, while an overlay is up. Outside that it is worth an
    /// assertion line — hence exposed as a predicate rather than leaving callers
    /// to compare the two themselves (which is how `_APP` gets used as a rule).
    pub fn focused_app_atom_disagrees(&self) -> bool {
        match (self.focused_app_atom, self.on_screen_app()) {
            (Some(raw), Some(app)) => raw != app.0,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        }
    }

    /// The app id gamescope published for a specific window, if it is a
    /// candidate. Used by the base-layer verify and by launch resolution.
    pub fn app_id_of(&self, window: Window) -> Option<AppId> {
        self.focusable_windows
            .iter()
            .find(|f| f.window == window)
            .map(|f| f.app_id)
    }

    /// Every candidate window belonging to a pid — how a freshly launched
    /// process's window is found (§5 derives pids as gamescope does, and this is
    /// the read side of that).
    pub fn windows_of_pid(&self, pid: u32) -> Vec<FocusableWindow> {
        self.focusable_windows
            .iter()
            .copied()
            .filter(|f| f.pid == pid)
            .collect()
    }

    /// Build a snapshot from already-decoded parts.
    ///
    /// Public so tests (and, later, the headless-CI harness) can construct a
    /// state from real captured bytes without an X server.
    ///
    /// Takes [`ScreenParts`] rather than a positional argument list, and that is
    /// this type's central safety property expressed structurally. The two
    /// leading parts are the focused **window** and the `_APP` atom — and
    /// `Window` IS `u32`, so a positional signature let a caller swap them and
    /// still compile, resolving `on_screen` against the `_APP` value. That is
    /// precisely the bug this module exists to make unreachable (see the module
    /// docs), so it may not be a matter of getting the argument order right.
    /// Named fields make the swap not typecheck as a mistake at all: you have to
    /// write `focused_app_atom:` to put a value there.
    pub fn assemble(parts: ScreenParts) -> Self {
        let ScreenParts {
            focused_window,
            focused_app_atom,
            base_layer,
            base_layer_windows,
            focusable_windows,
            focusable_apps,
            display,
            xwayland_server_id,
            steam_game,
        } = parts;
        // Resolution order is §5's: gamescope's own published id first (it is
        // what the compositor actually acts on), the window's STEAM_GAME tag
        // only as the repair fallback.
        let on_screen = focused_window.and_then(|window| {
            let published = focusable_windows
                .iter()
                .find(|f| f.window == window)
                .map(|f| (f.app_id, AppIdSource::Focusable));
            let fallback = steam_game.map(|a| (a, AppIdSource::SteamGame));
            published.or(fallback).map(|(app_id, source)| OnScreen {
                window,
                app_id,
                source,
            })
        });
        Self {
            focused_window,
            base_layer,
            base_layer_windows,
            focusable_windows,
            focusable_apps,
            display,
            xwayland_server_id,
            on_screen,
            focused_app_atom,
        }
    }
}

/// The decoded parts [`ScreenState::assemble`] is built from, as named fields.
///
/// One struct instead of nine positional arguments, because two of those
/// arguments are `Option<Window>` and `Option<u32>` over the same underlying
/// `u32`: positionally they are interchangeable to the compiler and catastrophic
/// to the semantics. See [`ScreenState::assemble`].
#[derive(Debug, Clone, Default)]
pub struct ScreenParts {
    /// `GAMESCOPE_FOCUSED_WINDOW` — **the** base window.
    pub focused_window: Option<Window>,
    /// `GAMESCOPE_FOCUSED_APP`, raw. Diagnostic only; never a decision input.
    pub focused_app_atom: Option<u32>,
    /// `GAMESCOPECTRL_BASELAYER_APPID`, as it currently reads back.
    pub base_layer: Vec<AppId>,
    /// `GAMESCOPECTRL_BASELAYER_WINDOW`.
    pub base_layer_windows: Vec<Window>,
    /// `GAMESCOPE_FOCUSABLE_WINDOWS`, as `(xid, appid, pid)` triplets.
    pub focusable_windows: Vec<FocusableWindow>,
    /// `GAMESCOPE_FOCUSABLE_APPS`.
    pub focusable_apps: Vec<AppId>,
    /// HDR/VRR feedback (§6).
    pub display: DisplayFeedback,
    /// `GAMESCOPE_XWAYLAND_SERVER_ID` of the server this was read from.
    pub xwayland_server_id: Option<u32>,
    /// The focused window's own `STEAM_GAME`, used ONLY as the repair fallback
    /// when gamescope published no triplet for that window (§5).
    pub steam_game: Option<AppId>,
}

/// Read one `ScreenState` from the compositor.
///
/// Every root property is requested before any reply is awaited, so the whole
/// snapshot is a single round trip. The focused window's `STEAM_GAME` cannot join
/// that batch — its target window id is only known once `GAMESCOPE_FOCUSED_WINDOW`
/// has come back — so it costs a second round trip, and only when a window is
/// focused *and* gamescope published no triplet for it (the repair case).
pub fn read(conn: &AtomConn) -> Result<ScreenState> {
    let root = conn.root();
    let x = conn.conn();

    // Fire every root request first; await afterwards.
    macro_rules! ask {
        ($name:expr) => {
            x.get_property(
                false,
                root,
                conn.atoms().get($name),
                AtomEnum::ANY,
                0,
                u32::MAX,
            )
            .map_err(|source| AtomError::Connection {
                atom: $name,
                source,
            })?
        };
    }
    let c_focused_window = ask!(names::FOCUSED_WINDOW);
    let c_focused_app = ask!(names::FOCUSED_APP);
    let c_base_layer = ask!(names::BASELAYER_APPID);
    let c_base_windows = ask!(names::BASELAYER_WINDOW);
    let c_focusable_windows = ask!(names::FOCUSABLE_WINDOWS);
    let c_focusable_apps = ask!(names::FOCUSABLE_APPS);
    let c_hdr = ask!(names::HDR_OUTPUT_FEEDBACK);
    let c_supports_hdr = ask!(names::DISPLAY_SUPPORTS_HDR);
    let c_vrr = ask!(names::VRR_FEEDBACK);
    let c_server_id = ask!(names::XWAYLAND_SERVER_ID);

    macro_rules! take {
        ($cookie:expr, $name:expr) => {{
            let reply = $cookie.reply().map_err(|source| AtomError::Protocol {
                atom: $name,
                source,
            })?;
            decode_cardinals(
                $name,
                reply.format,
                reply.type_,
                reply.bytes_after,
                &reply.value,
            )?
        }};
    }
    let focused_window = take!(c_focused_window, names::FOCUSED_WINDOW)
        .first()
        .copied();
    let focused_app_atom = take!(c_focused_app, names::FOCUSED_APP).first().copied();
    let base_layer: Vec<AppId> = take!(c_base_layer, names::BASELAYER_APPID)
        .into_iter()
        .map(AppId)
        .collect();
    let base_layer_windows = take!(c_base_windows, names::BASELAYER_WINDOW);
    let focusable_windows = decode_focusable_windows(
        names::FOCUSABLE_WINDOWS,
        &take!(c_focusable_windows, names::FOCUSABLE_WINDOWS),
    )?;
    let focusable_apps: Vec<AppId> = take!(c_focusable_apps, names::FOCUSABLE_APPS)
        .into_iter()
        .map(AppId)
        .collect();
    let display = DisplayFeedback {
        hdr_output: flag(take!(c_hdr, names::HDR_OUTPUT_FEEDBACK)),
        supports_hdr: flag(take!(c_supports_hdr, names::DISPLAY_SUPPORTS_HDR)),
        vrr: flag(take!(c_vrr, names::VRR_FEEDBACK)),
    };
    let xwayland_server_id = take!(c_server_id, names::XWAYLAND_SERVER_ID)
        .first()
        .copied();

    // Second round trip, and only in the repair case: gamescope already told us
    // the id for every window it knows, so reading STEAM_GAME on a window it
    // published a triplet for would just re-ask a question we have answered.
    let needs_repair_lookup =
        focused_window.is_some_and(|w| !focusable_windows.iter().any(|f| f.window == w));
    let steam_game = match (focused_window, needs_repair_lookup) {
        (Some(w), true) => conn.window_app_id(w).unwrap_or(None),
        _ => None,
    };

    Ok(ScreenState::assemble(ScreenParts {
        focused_window,
        focused_app_atom,
        base_layer,
        base_layer_windows,
        focusable_windows,
        focusable_apps,
        display,
        xwayland_server_id,
        steam_game,
    }))
}

fn flag(values: Vec<u32>) -> Option<bool> {
    values.first().map(|v| *v != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELL: AppId = AppId(9001);
    const GAME: AppId = AppId(9003);
    /// The live triplet from `dev/gamescope/lib.sh` (real compositor bytes).
    const GAME_WIN: Window = 8388625;
    const GAME_PID: u32 = 2998;

    fn triplet() -> FocusableWindow {
        FocusableWindow {
            window: GAME_WIN,
            app_id: GAME,
            pid: GAME_PID,
        }
    }

    fn state_with(
        focused: Option<Window>,
        focused_app: Option<u32>,
        focusable: Vec<FocusableWindow>,
        steam_game: Option<AppId>,
    ) -> ScreenState {
        ScreenState::assemble(ScreenParts {
            focused_window: focused,
            focused_app_atom: focused_app,
            base_layer: vec![GAME, SHELL],
            focusable_windows: focusable,
            focusable_apps: vec![GAME, SHELL],
            xwayland_server_id: Some(0),
            steam_game,
            ..Default::default()
        })
    }

    #[test]
    fn on_screen_comes_from_the_focused_window_not_the_app_atom() {
        // The measured overlay case: _APP is empty while the base window is a
        // live game. A rule keyed on _APP would read "nothing running".
        let s = state_with(Some(GAME_WIN), None, vec![triplet()], None);
        assert_eq!(s.on_screen_app(), Some(GAME));
        assert_eq!(s.on_screen().unwrap().source, AppIdSource::Focusable);
        assert_eq!(s.focused_app_atom_diagnostic(), None);
        assert!(s.focused_app_atom_disagrees(), "overlay up: the two differ");
    }

    #[test]
    fn app_atom_is_not_used_even_when_it_disagrees_with_the_window() {
        // If _APP ever names a different id, the window still wins.
        let s = state_with(Some(GAME_WIN), Some(769), vec![triplet()], None);
        assert_eq!(s.on_screen_app(), Some(GAME));
        assert_eq!(s.focused_app_atom_diagnostic(), Some(769));
        assert!(s.focused_app_atom_disagrees());
    }

    #[test]
    fn agreement_is_reported_as_agreement() {
        let s = state_with(Some(GAME_WIN), Some(GAME.0), vec![triplet()], None);
        assert!(!s.focused_app_atom_disagrees());
    }

    #[test]
    fn steam_game_is_the_repair_fallback_only() {
        // gamescope published no triplet for this window (scope did not
        // resolve), so the window's own tag answers — and says so.
        let s = state_with(Some(GAME_WIN), None, vec![], Some(GAME));
        let on = s.on_screen().unwrap();
        assert_eq!(on.app_id, GAME);
        assert_eq!(on.source, AppIdSource::SteamGame);
    }

    #[test]
    fn published_id_beats_the_repair_tag() {
        let s = state_with(Some(GAME_WIN), None, vec![triplet()], Some(AppId(1)));
        assert_eq!(s.on_screen().unwrap().source, AppIdSource::Focusable);
        assert_eq!(s.on_screen_app(), Some(GAME));
    }

    #[test]
    fn a_focused_window_with_no_resolvable_id_is_none_not_a_guess() {
        let s = state_with(Some(GAME_WIN), Some(GAME.0), vec![], None);
        assert_eq!(s.on_screen(), None, "an unresolved id must not be invented");
        assert!(s.focused_app_atom_disagrees());
    }

    #[test]
    fn nothing_focused_is_nothing_on_screen() {
        let s = state_with(None, None, vec![], None);
        assert_eq!(s.on_screen(), None);
        assert!(!s.focused_app_atom_disagrees());
    }

    #[test]
    fn lookups_by_window_and_pid() {
        let other = FocusableWindow {
            window: 42,
            app_id: SHELL,
            pid: 7,
        };
        let s = state_with(Some(GAME_WIN), None, vec![triplet(), other], None);
        assert_eq!(s.app_id_of(GAME_WIN), Some(GAME));
        assert_eq!(s.app_id_of(999), None);
        assert_eq!(s.windows_of_pid(GAME_PID), vec![triplet()]);
        assert!(s.windows_of_pid(1234).is_empty());
    }

    #[test]
    fn serialized_shape_names_the_app_atom_as_diagnostic() {
        let s = state_with(Some(GAME_WIN), Some(769), vec![triplet()], None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains("focused_app_atom_diagnostic"),
            "the wire name must warn readers off it: {json}"
        );
        assert!(!json.contains("\"focused_app\":"), "{json}");
        assert!(json.contains("\"on_screen\""), "{json}");
    }

    #[test]
    fn display_feedback_distinguishes_absent_from_false() {
        let absent = DisplayFeedback::default();
        assert_eq!(absent.hdr_output, None);
        let published_off = DisplayFeedback {
            hdr_output: Some(false),
            ..Default::default()
        };
        assert_ne!(
            absent, published_off,
            "the hotplug window (§6) turns on this distinction"
        );
    }
}

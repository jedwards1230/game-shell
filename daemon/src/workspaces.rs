//! App -> workspace assignment for the kiosk window model.
//!
//! The kiosk invariant is "exactly one app fills the screen". This module holds
//! it **structurally**: every app class gets its own Hyprland workspace, so two
//! apps physically cannot share the screen. That replaces the previous model,
//! which stacked every window on one workspace and maintained a per-window
//! fullscreen bit through four cooperating mechanisms (`windowrule = fullscreen`,
//! `misc:on_focus_under_fullscreen`, `misc:exit_window_retains_fullscreen`, and a
//! daemon backstop that re-fullscreened the active window).
//!
//! Why the change: in the stacked model the *switching primitive was focus*, and
//! focus can silently fail. Observed in the field (2026-08-26): a Steam Remote
//! Play `streaming_client` window sat tiled at half width behind a fullscreen
//! `steam`, reporting `acceptsInput: false`. `dispatch focuswindow address:...`
//! returned `ok` and did nothing, so the stream could not be brought back at all.
//! `dispatch workspace N` is a compositor-level operation that no window can
//! refuse, and it cannot half-succeed the way focus can.
//!
//! Keyed by window CLASS, not by address, and that distinction is load-bearing in
//! both directions:
//!   * two windows of the same class (an app plus its modal dialog) share a
//!     workspace, so a dialog still appears over the app that raised it;
//!   * two windows of different classes are separate destinations, which is
//!     exactly the Steam Big Picture (`steam`) vs live game (`streaming_client`)
//!     case the drawer needs to switch between.
//!
//! Assignment is sticky for the daemon's lifetime: an app that closes and
//! relaunches returns to the same workspace, so the switcher doesn't shuffle
//! under the user. Slots are never recycled — with a `u32` counter and a handful
//! of apps per session, exhaustion is not a real condition.

use std::collections::BTreeMap;

/// The workspace the shell shows when no app is on screen.
///
/// Deliberately left EMPTY. In the stacked model the home screen was a layer
/// surface drawn over whatever app happened to be fullscreen behind it; here
/// "go home" is a switch to a workspace with nothing on it.
pub const HOME_WORKSPACE: u32 = 1;

/// First workspace handed out to an app. `HOME_WORKSPACE` is reserved.
pub const FIRST_APP_WORKSPACE: u32 = 2;

/// Sticky window-class -> workspace map.
#[derive(Debug)]
pub struct Registry {
    map: BTreeMap<String, u32>,
    next: u32,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            next: FIRST_APP_WORKSPACE,
        }
    }

    /// The workspace already assigned to `class`, if any. Does not allocate.
    pub fn get(&self, class: &str) -> Option<u32> {
        self.map.get(class).copied()
    }

    /// The workspace for `class`, allocating a fresh one on first sight.
    ///
    /// Returns `None` for an empty class rather than burning a slot: Hyprland
    /// emits `openwindow` for surfaces that never report a class, and stranding
    /// one on its own workspace would make it unreachable.
    pub fn assign(&mut self, class: &str) -> Option<u32> {
        if class.is_empty() {
            return None;
        }
        if let Some(ws) = self.map.get(class) {
            return Some(*ws);
        }
        let ws = self.next;
        self.next = self.next.saturating_add(1);
        self.map.insert(class.to_string(), ws);
        Some(ws)
    }
}

/// The dispatch that parks a freshly-mapped window on its workspace.
///
/// **`movetoworkspacesilent`, never `movetoworkspace`.** The silent form moves
/// the window without following it, so assignment can never yank the screen away
/// from what the user is looking at — a prewarmed (`[silent]`) app that maps in
/// the background stays in the background. Bringing an app to the front is a
/// separate, explicit decision the shell makes via [`switch_command`], which is
/// why launch and resume are now the same one-line operation.
pub fn move_command(workspace: u32, address: &str) -> String {
    format!("dispatch movetoworkspacesilent {workspace},address:{address}")
}

/// The dispatch that puts `workspace` on screen.
pub fn switch_command(workspace: u32) -> String {
    format!("dispatch workspace {workspace}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_workspace_is_never_handed_to_an_app() {
        let mut r = Registry::new();
        for class in ["steam", "streaming_client", "tv.plex.Plex"] {
            assert_ne!(r.assign(class), Some(HOME_WORKSPACE));
        }
    }

    #[test]
    fn each_class_gets_its_own_workspace() {
        let mut r = Registry::new();
        let steam = r.assign("steam").unwrap();
        let stream = r.assign("streaming_client").unwrap();
        // The Steam/stream pair is the whole reason this is per-class: they are
        // two destinations the switcher must reach independently.
        assert_ne!(steam, stream);
    }

    #[test]
    fn assignment_is_sticky_across_relaunch() {
        let mut r = Registry::new();
        let first = r.assign("steam").unwrap();
        r.assign("tv.plex.Plex").unwrap();
        // Steam closed and came back: same slot, so the switcher doesn't reshuffle.
        assert_eq!(r.assign("steam"), Some(first));
    }

    #[test]
    fn same_class_shares_a_workspace_so_dialogs_land_on_their_parent() {
        let mut r = Registry::new();
        assert_eq!(r.assign("steam"), r.assign("steam"));
    }

    #[test]
    fn classless_window_is_not_assigned() {
        let mut r = Registry::new();
        assert_eq!(r.assign(""), None);
        // ...and it burned no slot.
        assert_eq!(r.assign("steam"), Some(FIRST_APP_WORKSPACE));
    }

    #[test]
    fn get_does_not_allocate() {
        let mut r = Registry::new();
        assert_eq!(r.get("steam"), None);
        assert_eq!(r.assign("steam"), Some(FIRST_APP_WORKSPACE));
        assert_eq!(r.get("steam"), Some(FIRST_APP_WORKSPACE));
    }

    #[test]
    fn move_command_is_the_silent_form() {
        // A non-silent move would follow the window and steal the screen from
        // whatever the user is looking at. Pin the form.
        let cmd = move_command(3, "0x55f51a9aedc0");
        assert_eq!(
            cmd,
            "dispatch movetoworkspacesilent 3,address:0x55f51a9aedc0"
        );
    }

    #[test]
    fn switch_command_shape() {
        assert_eq!(switch_command(HOME_WORKSPACE), "dispatch workspace 1");
        assert_eq!(switch_command(7), "dispatch workspace 7");
    }
}

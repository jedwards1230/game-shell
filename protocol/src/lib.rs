//! Shared wire types for the tv-shell Steam-library feature.
//!
//! These types are the single source of truth for the JSON shape exchanged
//! between `tv-shell-host` (the cross-platform sidecar that enumerates and
//! launches Steam games on the gaming PC) and `tv-shell-input` (the daemon on
//! the TV client that proxies `GET /library` / `POST /launch` / `GET /status` /
//! `POST /quit` / `POST /sleep` for the QML shell). The daemon reaches the host over **HTTP on the LAN** — the
//! host runs on a separate machine (the gaming PC; see `docs/HOST_SETUP.md`), so
//! the daemon is an HTTP *client*, not a process supervisor: it does not spawn,
//! health-restart, or otherwise manage the host's lifecycle. Both sides
//! (de)serialize through these types so the wire shape can't drift — the host
//! serializes them in its axum handlers (`host/src/main.rs`), and the daemon
//! deserializes the responses in `daemon/src/steam.rs`.
//!
//! Pure serde, no I/O — so both crates depend on it without dragging in either
//! one's heavier graph (axum on the host, evdev/cec on the daemon).
//!
//! The [`brand`] module carries the product identity (slug, env prefix, metric
//! prefix, config-dir resolution) shared by the daemon and host, with the
//! game-shell → tv-shell backward-compat shims in one place.

/// Central brand identity + backward-compat shims (see module docs).
pub mod brand;

use serde::{Deserialize, Serialize};

/// One installed Steam game, derived from an `appmanifest_*.acf` file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Steam application id (the number in `appmanifest_<appid>.acf` and the
    /// `steam://rungameid/<appid>` launch URL).
    pub appid: u32,
    /// Display name (`name` field in the manifest).
    pub name: String,
    /// Last-played unix timestamp (`LastPlayed`), or `None` when never played /
    /// absent. Used to build the "Recently Played" rail.
    pub last_played: Option<u64>,
    /// On-disk size in bytes (`SizeOnDisk`), or `None` when absent.
    pub size_on_disk: Option<u64>,
    /// Fully-installed bit (`StateFlags & 4`). Only fully-installed games are
    /// launchable; partially-downloaded ones are reported but flagged.
    pub installed: bool,
}

/// Response body for `GET /library`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryResponse {
    /// Installed games, real titles only (runtime junk filtered out host-side).
    /// `#[serde(default)]` keeps a body that omits `games` entirely deserializing
    /// to an empty library rather than failing — matching the daemon's previous
    /// lenient `Value`-based parse (a missing array was treated as "empty, ok").
    #[serde(default)]
    pub games: Vec<LibraryEntry>,
}

/// Request body for `POST /launch` (and `POST /quit`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    /// The Steam appid to launch via `steam://rungameid/<appid>`.
    pub appid: u32,
}

/// Response body for `GET /status` — the host's liveness + foreground-game probe.
///
/// All fields default so the daemon's parse stays resilient to a host that omits
/// one (matching the previous best-effort `Value` parse: a missing `running_appid`
/// is "nothing running", a missing `streaming` is `false`). The daemon treats a
/// successful `GET /status` as the reachability signal; this body carries the
/// foreground-game id and stream state it reads alongside that.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusResponse {
    /// `tv-shell-host` package version (`CARGO_PKG_VERSION`). Informational; the
    /// daemon does not act on it today.
    #[serde(default)]
    pub version: String,
    /// The foreground Steam appid on the host, or `None` when nothing is running
    /// (or detection found no match). Serialized as JSON `null` when absent.
    /// `u32` to match [`LibraryEntry::appid`] / [`LaunchRequest::appid`].
    #[serde(default)]
    pub running_appid: Option<u32>,
    /// Whether a Moonlight/Sunshine stream is active on the host.
    #[serde(default)]
    pub streaming: bool,
}

/// Response body for `POST /quit` — the host's answer to a quit request.
///
/// Shaped like [`SleepResponse`] and for the same reason: the host answers HTTP
/// **200** in both branches, so the body — not the status code — carries the
/// decision. `ok: true` means a matching game process was found and signalled;
/// `ok: false` means nothing was quit and `reason` says why (`"not running"`).
/// Without this the daemon can't tell a refusal from a success, and a `steam-quit`
/// against an already-dead game reports as if it worked.
///
/// `reason` is always serialized (as JSON `null` when the quit succeeded) so the
/// shape is identical in both branches. All fields default so the daemon's parse
/// survives a host that omits one — note the default is `ok: false`, i.e. an
/// unreadable/partial body degrades to "nothing was quit", never to a false
/// success.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct QuitResponse {
    /// Whether a running game was actually found and signalled.
    #[serde(default)]
    pub ok: bool,
    /// The appid the quit was requested for, echoed back.
    #[serde(default)]
    pub appid: u32,
    /// Human-readable reason when `ok` is false (e.g. `"not running"`); `None`
    /// (JSON `null`) when the game was signalled.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Response body for `POST /sleep` — the host's answer to a suspend request.
///
/// Two-valued on purpose: `ok: true` means the suspend was accepted and
/// dispatched to the OS; `ok: false` means the host REFUSED it and `reason`
/// says why (a game is running, a stream is live). A refusal is an HTTP **200**
/// with `ok: false`, not an error status — "I decided not to" is a normal
/// answer, so the daemon can surface the reason instead of a transport failure.
///
/// `reason` is always serialized (as JSON `null` on success) so the shape is
/// identical in both branches and a consumer can bind one field unconditionally.
/// Both fields default so the daemon's parse survives a host that omits one.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SleepResponse {
    /// Whether the suspend was accepted (`true`) or refused (`false`).
    #[serde(default)]
    pub ok: bool,
    /// Human-readable refusal reason when `ok` is false; `None` (JSON `null`)
    /// when the suspend was accepted.
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_response_roundtrips() {
        let resp = LibraryResponse {
            games: vec![LibraryEntry {
                appid: 730,
                name: "Counter-Strike 2".to_string(),
                last_played: Some(1_700_000_000),
                size_on_disk: Some(35_000_000_000),
                installed: true,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: LibraryResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn launch_request_roundtrips() {
        let req = LaunchRequest { appid: 220 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"appid":220}"#);
        let back: LaunchRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn empty_library_default() {
        assert!(LibraryResponse::default().games.is_empty());
    }

    #[test]
    fn library_response_missing_games_is_empty() {
        // A body that omits `games` entirely must deserialize to an empty library
        // (not error) — preserving the daemon's previous lenient parse where a
        // missing array meant "empty, ok".
        let back: LibraryResponse = serde_json::from_str("{}").unwrap();
        assert!(back.games.is_empty());
    }

    #[test]
    fn launch_request_serializes_appid_only() {
        // The POST body the daemon sends for /launch and /quit must stay exactly
        // `{"appid":N}` — the host parses this same type.
        assert_eq!(
            serde_json::to_string(&LaunchRequest { appid: 730 }).unwrap(),
            r#"{"appid":730}"#
        );
    }

    #[test]
    fn status_response_roundtrips() {
        let s = StatusResponse {
            version: "1.2.3".to_string(),
            running_appid: Some(730),
            streaming: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        // Field order is declaration order — keep it byte-stable with the host's
        // previous hand-rolled `json!({version, running_appid, streaming})`.
        assert_eq!(
            json,
            r#"{"version":"1.2.3","running_appid":730,"streaming":true}"#
        );
        let back: StatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn status_response_running_null_when_idle() {
        let s = StatusResponse {
            version: "0.1.0".to_string(),
            running_appid: None,
            streaming: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            json,
            r#"{"version":"0.1.0","running_appid":null,"streaming":false}"#
        );
    }

    #[test]
    fn sleep_response_accepted_serializes_reason_null() {
        // `reason` must be PRESENT as JSON null on the accepted path — a consumer
        // binds one field in both branches rather than probing for existence.
        let s = SleepResponse {
            ok: true,
            reason: None,
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"ok":true,"reason":null}"#
        );
    }

    #[test]
    fn sleep_response_refusal_roundtrips() {
        let s = SleepResponse {
            ok: false,
            reason: Some("a game is running".to_string()),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"ok":false,"reason":"a game is running"}"#);
        let back: SleepResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn sleep_response_defaults_on_missing_fields() {
        // A `{}` body must not fail the parse; it degrades to "refused, no reason
        // given" (ok defaults false), which is the safe reading of a host that
        // answered but told us nothing.
        let back: SleepResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(back, SleepResponse::default());
        assert!(!back.ok);
        assert!(back.reason.is_none());
    }

    #[test]
    fn quit_response_signalled_serializes_reason_null() {
        // Like SleepResponse: `reason` is PRESENT as JSON null on the success
        // path so a consumer binds one field in both branches.
        let q = QuitResponse {
            ok: true,
            appid: 730,
            reason: None,
        };
        assert_eq!(
            serde_json::to_string(&q).unwrap(),
            r#"{"ok":true,"appid":730,"reason":null}"#
        );
    }

    #[test]
    fn quit_response_refusal_roundtrips() {
        let q = QuitResponse {
            ok: false,
            appid: 252950,
            reason: Some("not running".to_string()),
        };
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(
            json,
            r#"{"ok":false,"appid":252950,"reason":"not running"}"#
        );
        let back: QuitResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn quit_response_defaults_on_missing_fields() {
        // A `{}` body degrades to "nothing was quit" (ok defaults false) — a
        // partial/unreadable body must never be read as a successful quit.
        let back: QuitResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(back, QuitResponse::default());
        assert!(!back.ok);
        assert!(back.reason.is_none());
    }

    #[test]
    fn status_response_defaults_on_missing_fields() {
        // A partial body must not fail the parse: missing version → "", missing
        // running_appid → None, missing streaming → false.
        let back: StatusResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(back, StatusResponse::default());
        assert_eq!(back.version, "");
        assert!(back.running_appid.is_none());
        assert!(!back.streaming);
    }
}

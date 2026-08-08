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

/// Shared MQTT state envelope, device identity, and Home Assistant discovery
/// types (see module docs).
pub mod mqtt;

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// What a node *is*, structurally — which of the two tv-shell binaries answers
/// for it. A shell node runs `tv-shell-input` beside the QML shell; a sidecar
/// node runs `tv-shell-host` and has no UI of its own.
///
/// Informational: route gating is done on [`Capabilities::features`], never on
/// the kind. A node with no shell surface simply doesn't declare the shell
/// features, which is a finer-grained (and non-lying) statement than its kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    /// Runs `tv-shell-input` + the QML shell (e.g. the TV client).
    Shell,
    /// Runs `tv-shell-host` only (e.g. the gaming PC).
    Sidecar,
}

/// The OS a node runs on, resolved at **compile time** by [`Platform::current`].
///
/// Shaped like [`mqtt::DeviceOs`] (same `lowercase` wire form) but deliberately
/// a separate type: `DeviceOs` is a Home Assistant device attribute with an
/// `Unknown` escape hatch, while this one enumerates exactly the three targets
/// the workspace builds for.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Linux — the TV client and the Linux boot of the gaming PC.
    Linux,
    /// Windows — the Windows boot of the gaming PC.
    Windows,
    /// macOS — CI and developer machines.
    MacOS,
}

impl Platform {
    /// The platform this binary was compiled for. Total; never panics.
    ///
    /// The workspace releases for exactly linux / macOS / Windows (see
    /// `.github/workflows/release-*.yml`), so any other target — a BSD, say —
    /// falls through to `Linux` rather than growing an `Unknown` variant a
    /// consumer would have to handle for a build that is never produced.
    pub fn current() -> Platform {
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Platform::MacOS
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Platform::Linux
        }
    }
}

/// One thing a node declares it can do. The panel builds its nav **and
/// registers its routes** from the set a node reports — it never probes,
/// sniffs, or guesses (see `docs/MULTI_NODE_PANEL.md`).
///
/// ## Wire form and forward compatibility
///
/// Serialized as a plain snake_case string (`"web_apps"`, `"steam_library"`),
/// matching the crate's other wire types. Anything this build does not know
/// deserializes into [`Feature::Unknown`] **holding the original string**, which
/// re-serializes byte-identically. That is the whole point: in a mixed-version
/// fleet a newer node reports a feature an older panel has never heard of, and
/// the older panel must round-trip it rather than failing the entire
/// [`Capabilities`] parse (which would ungate *everything* on that node).
/// `#[serde(other)]` is not usable here — it collapses every unknown into one
/// valueless variant, so two different unknown features would compare equal and
/// neither would survive a round-trip.
///
/// Ordering is the derived `Ord` (declaration order, `Unknown` last), so a
/// `BTreeSet<Feature>` serializes byte-stably.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Feature {
    // --- shell node ---
    /// HDMI-CEC control (`cec-*` IPC).
    Cec,
    /// Gamepad fleet: grab/release, per-pad battery/rumble, bindings.
    Controllers,
    /// Home-screen widget registry + per-widget config.
    Widgets,
    /// Wallpaper library.
    Wallpapers,
    /// The daemon-owned web-app registry (`webapp-*` IPC).
    WebApps,
    /// Read/write of the shared settings store (`get-config` / `set-config`).
    SettingsStore,
    /// Screen-ownership push (`shell-focus on|off`). Deliberately NOT the
    /// shell *restart* path: that is `POST /dev/restart-shell` / the MCP
    /// `restart_shell` tool, which ride the optional network bridges rather
    /// than the always-present IPC, so promising it here would have a client
    /// register a route a socket-only node can't serve.
    ShellLifecycle,
    /// Capture a frame of the live session.
    Screenshot,

    // --- sidecar ---
    /// Enumerate the installed Steam library.
    SteamLibrary,
    /// Launch / quit a game on this node.
    GameLaunch,
    /// Suspend **this** node to RAM.
    Sleep,

    // --- shared / platform-dependent ---
    /// Pull a git ref and rebuild on-device.
    DevDeploy,
    /// Read this node's logs.
    Logs,
    /// Enumerate / manage processes on this node.
    Processes,
    /// Query and apply OS package updates.
    SystemUpdates,

    /// A feature this build does not know. Preserved verbatim so an older panel
    /// round-trips a newer node's set without data loss and without erroring.
    ///
    /// `#[non_exhaustive]` so **no other crate can construct it directly** —
    /// everyone outside this one must go through [`From<String>`], which
    /// canonicalizes. Without that, `Feature::Unknown("cec".into())` would be a
    /// second, `Eq`/`Ord`-distinct value that serializes to the same `"cec"`
    /// string: a `BTreeSet` could hold both, emit a duplicate in the JSON array,
    /// and stop round-tripping. The panel builds its sets from strings, so this
    /// is exactly the mistake it is positioned to make.
    #[non_exhaustive]
    Unknown(String),
}

impl Feature {
    /// The snake_case wire name. For [`Feature::Unknown`] this is the original
    /// string, which is what makes the round-trip lossless.
    pub fn as_str(&self) -> &str {
        match self {
            Feature::Cec => "cec",
            Feature::Controllers => "controllers",
            Feature::Widgets => "widgets",
            Feature::Wallpapers => "wallpapers",
            Feature::WebApps => "web_apps",
            Feature::SettingsStore => "settings_store",
            Feature::ShellLifecycle => "shell_lifecycle",
            Feature::Screenshot => "screenshot",
            Feature::SteamLibrary => "steam_library",
            Feature::GameLaunch => "game_launch",
            Feature::Sleep => "sleep",
            Feature::DevDeploy => "dev_deploy",
            Feature::Logs => "logs",
            Feature::Processes => "processes",
            Feature::SystemUpdates => "system_updates",
            Feature::Unknown(s) => s,
        }
    }

    /// The known variant for a wire name, or `None` when this build doesn't
    /// know it. Split out of `From<String>` so the string can be *moved* into
    /// [`Feature::Unknown`] instead of cloned.
    fn known(s: &str) -> Option<Feature> {
        Some(match s {
            "cec" => Feature::Cec,
            "controllers" => Feature::Controllers,
            "widgets" => Feature::Widgets,
            "wallpapers" => Feature::Wallpapers,
            "web_apps" => Feature::WebApps,
            "settings_store" => Feature::SettingsStore,
            "shell_lifecycle" => Feature::ShellLifecycle,
            "screenshot" => Feature::Screenshot,
            "steam_library" => Feature::SteamLibrary,
            "game_launch" => Feature::GameLaunch,
            "sleep" => Feature::Sleep,
            "dev_deploy" => Feature::DevDeploy,
            "logs" => Feature::Logs,
            "processes" => Feature::Processes,
            "system_updates" => Feature::SystemUpdates,
            _ => return None,
        })
    }
}

impl From<String> for Feature {
    fn from(s: String) -> Feature {
        Feature::known(&s).unwrap_or(Feature::Unknown(s))
    }
}

impl From<Feature> for String {
    fn from(f: Feature) -> String {
        match f {
            // Move the original string out rather than re-allocating it.
            Feature::Unknown(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

/// What a node declares it can do — served as the `capabilities` IPC command by
/// the daemon (shell node) and `GET /capabilities` by the sidecar.
///
/// **Capability is declared by the node, never inferred by the panel.** Same
/// principle as `shell-focus` ("screen ownership is declared, never inferred"),
/// and the same failure mode if violated: a probe answers a question adjacent to
/// the one you asked, and is confidently wrong.
///
/// `#[serde(default)]` is applied per field by which direction is safe when a
/// node **omits** it. It covers **absence only** — an explicit `null` still
/// fails the whole parse (`"node_id":null` → `invalid type: null, expected a
/// string`), which is fine because every producer in this workspace is Rust and
/// emits a real string or a real array:
///
/// - `features` — an absent array defaults to **empty**, i.e. "this node can do
///   nothing". The panel gates routes on this set, so a set the node didn't
///   state must ungate nothing.
/// - `node_id` / `agent_version` — display-only, so an absent one degrades to
///   `""` instead of failing the parse.
/// - `kind` and `platform` have **no default on purpose**. Both are structural
///   claims about what the node *is*; inventing one would make a sidecar render
///   as a shell node. A body that can't state them is not a capability
///   handshake, and failing the parse is the fail-closed direction.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Stable identity of this node (`"htpc-1"`, `"desktop-2"`). Resolution
    /// order is the serving binary's business — see `docs/IPC_PROTOCOL.md`
    /// (daemon) and `docs/HOST_SETUP.md` (sidecar).
    #[serde(default)]
    pub node_id: String,
    /// Which binary answers for this node.
    pub kind: NodeKind,
    /// The serving binary's release version (`CARGO_PKG_VERSION`, stamped from
    /// the tag by the release workflow).
    #[serde(default)]
    pub agent_version: String,
    /// The OS this node's binary was compiled for.
    pub platform: Platform,
    /// Everything this node can do. Ordered (`BTreeSet`) so the serialized JSON
    /// is byte-stable across calls.
    #[serde(default)]
    pub features: BTreeSet<Feature>,
}

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

    /// Every known [`Feature`] variant, in declaration order — the widest set a
    /// node could report. Used by the round-trip and line-length tests.
    fn all_known_features() -> Vec<Feature> {
        vec![
            Feature::Cec,
            Feature::Controllers,
            Feature::Widgets,
            Feature::Wallpapers,
            Feature::WebApps,
            Feature::SettingsStore,
            Feature::ShellLifecycle,
            Feature::Screenshot,
            Feature::SteamLibrary,
            Feature::GameLaunch,
            Feature::Sleep,
            Feature::DevDeploy,
            Feature::Logs,
            Feature::Processes,
            Feature::SystemUpdates,
        ]
    }

    #[test]
    fn every_known_feature_roundtrips_through_its_wire_name() {
        for f in all_known_features() {
            let json = serde_json::to_string(&f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.as_str()));
            let back: Feature = serde_json::from_str(&json).unwrap();
            assert_eq!(f, back);
        }
    }

    #[test]
    fn feature_wire_names_are_snake_case_and_unique() {
        let features = all_known_features();
        let names: Vec<&str> = features.iter().map(|f| f.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate feature wire name");
        for n in names {
            assert!(
                n.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{n} is not snake_case"
            );
        }
    }

    #[test]
    fn unknown_feature_roundtrips_verbatim() {
        // A feature a NEWER node reports must survive an older build untouched:
        // same string in, same string out, and no parse error.
        let json = r#""quantum_teleport""#;
        let f: Feature = serde_json::from_str(json).unwrap();
        assert_eq!(f, Feature::Unknown("quantum_teleport".to_string()));
        assert_eq!(serde_json::to_string(&f).unwrap(), json);
    }

    #[test]
    fn from_string_canonicalizes_known_names() {
        // The ONLY way another crate can build a `Feature` from a string
        // (`Unknown` is `#[non_exhaustive]`), so this is what keeps a
        // string-built set from carrying a second, Eq-distinct `Unknown("cec")`
        // beside `Feature::Cec` that serializes to the same `"cec"`.
        assert_eq!(Feature::from("cec".to_string()), Feature::Cec);
        for f in all_known_features() {
            assert_eq!(Feature::from(f.as_str().to_string()), f);
        }
        // A round-trip through a set therefore can't grow a duplicate.
        let set: BTreeSet<Feature> = ["cec", "cec", "web_apps"]
            .into_iter()
            .map(|s| Feature::from(s.to_string()))
            .collect();
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"["cec","web_apps"]"#
        );
    }

    #[test]
    fn two_distinct_unknown_features_stay_distinct() {
        // The reason `#[serde(other)]` is unusable: it would collapse these two
        // into one valueless variant, so a set would silently lose one.
        let a: Feature = serde_json::from_str(r#""future_a""#).unwrap();
        let b: Feature = serde_json::from_str(r#""future_b""#).unwrap();
        assert_ne!(a, b);
        let set: BTreeSet<Feature> = [a, b].into_iter().collect();
        assert_eq!(set.len(), 2);
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"["future_a","future_b"]"#
        );
    }

    #[test]
    fn capabilities_roundtrips() {
        let c = Capabilities {
            node_id: "htpc-1".to_string(),
            kind: NodeKind::Shell,
            agent_version: "0.2.2".to_string(),
            platform: Platform::Linux,
            features: [Feature::Cec, Feature::Controllers].into_iter().collect(),
        };
        let json = serde_json::to_string(&c).unwrap();
        // Field order is declaration order; the feature array is BTreeSet order
        // (declaration order), so the whole body is byte-stable.
        assert_eq!(
            json,
            r#"{"node_id":"htpc-1","kind":"shell","agent_version":"0.2.2","platform":"linux","features":["cec","controllers"]}"#
        );
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn capabilities_mixing_known_and_unknown_features_parses() {
        // The mixed-version case: an older panel reading a newer node.
        let json = r#"{"node_id":"desktop-2","kind":"sidecar","agent_version":"9.9.9","platform":"windows","features":["steam_library","holodeck"]}"#;
        let c: Capabilities = serde_json::from_str(json).unwrap();
        assert_eq!(c.kind, NodeKind::Sidecar);
        assert_eq!(c.platform, Platform::Windows);
        assert!(c.features.contains(&Feature::SteamLibrary));
        assert!(c
            .features
            .contains(&Feature::Unknown("holodeck".to_string())));
        // Unknown sorts last (declaration order), so the re-serialized body is
        // NOT byte-identical to the input here — but no feature is lost.
        let back: Capabilities = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn capabilities_defaults_absent_optional_fields() {
        // node_id / agent_version / features degrade; an empty feature set means
        // "this node can do nothing", which ungates nothing panel-side.
        let c: Capabilities =
            serde_json::from_str(r#"{"kind":"sidecar","platform":"macos"}"#).unwrap();
        assert_eq!(c.node_id, "");
        assert_eq!(c.agent_version, "");
        assert!(c.features.is_empty());
        assert_eq!(c.platform, Platform::MacOS);
    }

    #[test]
    fn capabilities_explicit_null_is_not_a_default() {
        // `#[serde(default)]` covers ABSENCE only — an explicit null fails the
        // whole parse. Pinned so the documented boundary is explicit rather than
        // assumed: every in-tree producer is Rust and emits real values, so this
        // is a statement about the contract, not a live failure mode. If a
        // JSON-emitting node ever joins the fleet, this test is the tripwire.
        assert!(serde_json::from_str::<Capabilities>(
            r#"{"node_id":null,"kind":"shell","platform":"linux"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Capabilities>(
            r#"{"features":null,"kind":"shell","platform":"linux"}"#
        )
        .is_err());
    }

    #[test]
    fn capabilities_without_kind_or_platform_fails_closed() {
        // Structural claims have no default: a body that can't say what the node
        // IS is not a capability handshake.
        assert!(serde_json::from_str::<Capabilities>(r#"{"platform":"linux"}"#).is_err());
        assert!(serde_json::from_str::<Capabilities>(r#"{"kind":"shell"}"#).is_err());
        assert!(serde_json::from_str::<Capabilities>("{}").is_err());
    }

    #[test]
    fn capabilities_unknown_fields_are_ignored() {
        // Forward compat the other way: a NEWER node adding a field must not
        // break an older reader.
        let c: Capabilities = serde_json::from_str(
            r#"{"node_id":"n","kind":"shell","platform":"linux","uptime_secs":42}"#,
        )
        .unwrap();
        assert_eq!(c.node_id, "n");
    }

    #[test]
    fn full_capabilities_fit_the_ipc_line_limit() {
        // A deliberate REPLY-SIZE BUDGET, not a codec limit. The daemon's
        // `LinesCodec::new_with_max_length(4096)` (daemon/src/ipc.rs) constrains
        // the INBOUND command line only — `LinesCodec`'s `Encoder` ignores
        // `max_length`, and the one in-tree reader (panel/src/ipc.rs) uses an
        // unbounded `BufReader::read_line` — so nothing truncates an oversized
        // reply today. The assertion is here to keep one newline-framed
        // capability reply small enough to stay comfortably inside that budget
        // as features accrue.
        //
        // This is a wide-but-not-maximal sample: every known feature plus a
        // generous node_id/version. It is not an upper bound — `node_id` comes
        // from `[mqtt].device_id`, an unbounded config string.
        let c = Capabilities {
            node_id: "a-deliberately-long-node-identifier-for-headroom".to_string(),
            kind: NodeKind::Shell,
            agent_version: "123.456.789-rc.1+build.20260807".to_string(),
            platform: Platform::Linux,
            features: all_known_features().into_iter().collect(),
        };
        let len = serde_json::to_string(&c).unwrap().len();
        assert!(len < 1024, "capabilities JSON is {len} bytes, want < 1024");
    }

    #[test]
    fn platform_current_matches_the_build_target() {
        let p = Platform::current();
        if cfg!(target_os = "windows") {
            assert_eq!(p, Platform::Windows);
        } else if cfg!(target_os = "macos") {
            assert_eq!(p, Platform::MacOS);
        } else {
            assert_eq!(p, Platform::Linux);
        }
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

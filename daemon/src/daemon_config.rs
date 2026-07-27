//! Typed daemon configuration: `~/.config/tv-shell/config.toml`.
//!
//! Replaces the old `daemon.env` `KEY=VALUE` file that `tv-shell-session.sh`
//! sourced into the environment. The daemon now reads a typed TOML document
//! directly, so the config surface is parsed once, validated at startup, and
//! never leaks the bearer token into the process environment (where any child
//! subprocess — grim, quickshell — would inherit it).
//!
//! ## Layout
//!
//! ```toml
//! [panel]                     # tv-shell-panel web control panel (its own binary)
//! enabled = true              #   the panel serves when its systemd unit runs
//! bind = "127.0.0.1:8091"     #   panel listen address (LAN-only, no auth in v1)
//!
//! [http]                      # LAN HTTP control bridge (opt-in)
//! bind = "127.0.0.1:8089"     #   absent ⇒ bridge off
//! auth_enabled = true         #   default true; only disable on a trusted LAN
//! token_file = "~/.config/tv-shell/http-token"  # 0600; the shared bearer token
//!
//! [mcp]                       # MCP server (opt-in; needs --features mcp)
//! bind = "127.0.0.1:8090"     #   absent ⇒ off; shares [http].token_file + auth
//! dev = false                 #   dev tools (build/restart/deploy) — keep off in prod
//! allowed_hosts = ["my-host.local"]   # Host-header allowlist (DNS-rebind guard)
//!
//! [cec]                       # HDMI-CEC lifecycle (needs --features cec)
//! lifecycle = false
//! osd_name = "living-room"    #   input label TVs/AVRs display; absent ⇒ hostname
//!
//! [plex]                      # Plex home-screen widget (optional)
//! url = "http://plex:32400"
//! token_file = "~/.config/tv-shell/plex-token"   # or: token = "…"
//!
//! [steam]                     # Steam library row (optional)
//! url = "http://gaming-pc:47995"
//! token_file = "~/.config/tv-shell/steam-token"  # or: token = "…"
//! wake_active_host_on_start = false   # WoL the ACTIVE host on start / host switch
//!
//! [[steam.hosts]]             # …or named sidecars instead of the single `url`
//! name = "desktop-1"
//! url = "http://192.0.2.10:47995"
//! mac = "aa:bb:cc:dd:ee:ff"   # static WoL MAC (preferred over `ip neigh`/cache)
//!
//! [mqtt]                      # MQTT state publisher + command surface (opt-in)
//! broker = "mqtts://mqtt.example:8883"   # absent ⇒ MQTT off entirely
//! device_id = "htpc-1"        #   EXPLICIT identity; required when broker is set
//! username = "tv-shell-htpc-1"
//! password_file = "~/.config/tv-shell/mqtt-password"   # 0600
//! ca_file = "~/.config/tv-shell/mqtt-ca.pem"           # PEM CA bundle (public)
//! heartbeat_secs = 30         #   floor republish interval
//! keepalive_secs = 60         #   MQTT keepalive
//!
//! [dev]                       # operator escape hatch
//! allow_insecure_lan = false  # see validate(): permit LAN + dev + no-auth on purpose
//! ```
//!
//! ## Security
//!
//! The shared bearer token is **by reference only** (`[http].token_file`), never
//! inline, and the referenced file must be private (mode `0600`). A
//! missing/empty/world-readable token file means "no token", which — with auth
//! enabled — fails closed (all requests 401).
//!
//! [`DaemonConfig::validate`] REFUSES to run with the dangerous combination of a
//! non-loopback bind + dev tools + effectively-disabled auth (for BOTH the HTTP
//! bridge and the MCP server), unless the operator has explicitly opted in with
//! `[dev].allow_insecure_lan = true`.
//!
//! Cross-platform: pure parsing/validation, unit-tested on every host.

use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Process-global parsed config, populated once at startup by [`init_global`].
/// The standalone command handlers (plex/steam/cec/http) read their settings
/// from here instead of `std::env::var`, so the typed config is the single
/// source of truth without threading a `&DaemonConfig` through every call site.
static GLOBAL: OnceLock<DaemonConfig> = OnceLock::new();

/// Install the process-global config (call once, at startup, after `validate`).
/// A second call is ignored (the first wins) — tests that don't init see the
/// default via [`global`].
pub fn init_global(config: DaemonConfig) {
    let _ = GLOBAL.set(config);
}

/// Borrow the process-global config. Before [`init_global`] runs (e.g. in unit
/// tests of the standalone modules) this returns a shared all-default config, so
/// callers never panic and behave as "everything off / not configured".
pub fn global() -> &'static DaemonConfig {
    GLOBAL.get_or_init(DaemonConfig::default)
}

/// The full typed daemon configuration. Every section is optional; an empty or
/// missing `config.toml` yields all-default (everything off), matching the old
/// "no daemon.env ⇒ shell still boots, no control surface" behavior.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// `[panel]` — the tv-shell-panel web control panel. The daemon does NOT use
    /// these values (the separate `tv-shell-panel` binary reads them); the field
    /// exists only so the shared `config.toml`'s `[panel]` section parses under
    /// this struct's `deny_unknown_fields` instead of aborting daemon startup.
    pub panel: PanelConfig,
    pub http: HttpConfig,
    pub mcp: McpConfig,
    /// `[mqtt]` — the MQTT state publisher + command surface (see [`MqttConfig`]).
    pub mqtt: MqttConfig,
    pub cec: CecConfig,
    pub plex: PlexConfig,
    pub steam: SteamConfig,
    pub observability: ObservabilityConfig,
    pub input: InputConfig,
    pub dev: DevConfig,
}

/// `[panel]` — the tv-shell-panel web control panel (a separate binary from the
/// daemon). The daemon parses-and-ignores this section; the `tv-shell-panel`
/// binary is the real owner of these fields. Intentionally does NOT set
/// `deny_unknown_fields` (unlike the daemon's own sections) so the panel can add
/// its own keys later without forcing a matching daemon-struct change — the
/// daemon only needs the `[panel]` table to not abort its `deny_unknown_fields`
/// top-level parse.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    /// Whether the panel serves when its unit runs. Default true.
    pub enabled: bool,
    /// Panel listen address, e.g. `127.0.0.1:8091`. LAN-only, no auth in v1.
    pub bind: Option<String>,
    /// Reserved for future panel auth; unused in v1 (LAN-only).
    pub token_file: Option<String>,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: None,
            token_file: None,
        }
    }
}

/// `[http]` — the LAN HTTP control bridge.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    /// Bind address, e.g. `127.0.0.1:8089`. `None` ⇒ bridge off.
    pub bind: Option<String>,
    /// Require a bearer token. Default `true` (secure by default).
    pub auth_enabled: bool,
    /// Path to a 0600 file holding the shared bearer token. Never inline.
    pub token_file: Option<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: None,
            auth_enabled: true,
            token_file: None,
        }
    }
}

/// `[mcp]` — the MCP server (shares the HTTP bridge's token + auth).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Bind address, e.g. `127.0.0.1:8090`. `None` ⇒ off.
    pub bind: Option<String>,
    /// Enable the dev tool surface (build/restart/deploy). Default `false`.
    pub dev: bool,
    /// Host-header allowlist (DNS-rebinding guard). Empty ⇒ allow-all (token-gated).
    pub allowed_hosts: Vec<String>,
}

/// `[mqtt]` — the MQTT state publisher + command surface (`crate::mqtt`).
///
/// `deny_unknown_fields` **is** set here, matching `[http]` and `[mcp]`.
/// `[cec]` and `[panel]` deliberately omit it because a *second binary*
/// (tv-shell-panel) writes those sections and is released independently; nothing
/// but the daemon ever writes `[mqtt]`, so it inherits the daemon's strict
/// posture and a typo fails loudly at startup.
///
/// **The asymmetry that bites:** `DaemonConfig` is `deny_unknown_fields` at the
/// top level, but `panel/src/config.rs` parses the *same file* leniently. A typo
/// under `[mqtt]` therefore aborts the daemon while the panel keeps running — so
/// the symptom presents as "the daemon is broken", not "the config has a typo".
/// Read the daemon's startup log before believing anything else.
///
/// There is **no config-reload path**: `DaemonConfig::load()` runs once into a
/// `OnceLock` and `watch.rs` watches `settings.json` only. Every key here —
/// including a credential rotation — needs a **daemon restart**, which hands the
/// CEC adapter to whatever grabs it next. See the `crate::mqtt` module docs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttConfig {
    /// `mqtts://host:8883` or `mqtt://host:1883`. `None` ⇒ MQTT off entirely;
    /// no connection is attempted and no discovery is published.
    pub broker: Option<String>,
    /// The device identity. EXPLICIT ONLY — never derived from hostname, OS, or
    /// IP. Required whenever `broker` is set; startup FAILS if it is missing.
    pub device_id: Option<String>,
    /// MQTT username (e.g. `tv-shell-htpc-1`). Must be set together with
    /// `password_file`, or neither.
    pub username: Option<String>,
    /// 0600 file under the config dir, resolved like every other token file.
    pub password_file: Option<String>,
    /// PEM CA bundle. A PUBLIC certificate — path-expanded but NOT
    /// permission-checked and NOT confined to the config dir.
    pub ca_file: Option<String>,
    /// Floor heartbeat: republish at least this often so `published_at` always
    /// advances.
    pub heartbeat_secs: u64,
    /// MQTT keepalive. Generous by default — the Windows sidecar's watchdog
    /// makes reconnect churn a real risk, and this daemon shares the same broker.
    pub keepalive_secs: u64,
}

impl Default for MqttConfig {
    /// Hand-written, not derived: `#[derive(Default)]` would give `0` for both
    /// interval fields, and a `0`-second heartbeat busy-loops the publisher.
    fn default() -> Self {
        Self {
            broker: None,
            device_id: None,
            username: None,
            password_file: None,
            ca_file: None,
            heartbeat_secs: 30,
            keepalive_secs: 60,
        }
    }
}

/// A parsed `[mqtt].broker` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttEndpoint {
    /// Hostname or IP literal (IPv6 without its brackets).
    pub host: String,
    /// TCP port — the scheme default (1883/8883) unless explicitly given.
    pub port: u16,
    /// `true` for `mqtts://`.
    pub tls: bool,
}

/// Everything the MQTT actor needs, resolved from `[mqtt]`.
///
/// Produced by [`DaemonConfig::mqtt_settings`]. `ca_path` is deliberately a path
/// rather than the certificate bytes: a CA file is public, and an unreadable one
/// must degrade to the platform trust store rather than abort — so the read
/// happens at the spawn site, where that fallback lives.
#[derive(Debug, Clone)]
pub struct ResolvedMqtt {
    /// Validated device identity — explicitly configured, never derived.
    pub device_id: tv_shell_protocol::mqtt::DeviceId,
    /// Broker host/port/TLS.
    pub endpoint: MqttEndpoint,
    /// MQTT username, when credentials are configured.
    pub username: Option<String>,
    /// MQTT password read from `password_file`, when configured.
    pub password: Option<String>,
    /// Optional CA bundle path. Unset ⇒ the platform trust store, which is the
    /// normal path now that the broker presents a publicly-trusted certificate.
    pub ca_path: Option<PathBuf>,
    /// Floor-heartbeat interval in seconds.
    pub heartbeat_secs: u64,
    /// MQTT keepalive in seconds.
    pub keepalive_secs: u64,
}

/// Parse a `[mqtt].broker` URL **by hand**.
///
/// The daemon has no `url` crate and must not gain one for two schemes. Only
/// `mqtt://` (default port 1883) and `mqtts://` (default port 8883) are accepted;
/// anything else is an error naming both, so a `http://`/`tcp://` paste fails at
/// startup rather than at first connect.
fn parse_mqtt_endpoint(raw: &str) -> anyhow::Result<MqttEndpoint> {
    let (tls, default_port, rest) = if let Some(rest) = raw.strip_prefix("mqtts://") {
        (true, 8883u16, rest)
    } else if let Some(rest) = raw.strip_prefix("mqtt://") {
        (false, 1883u16, rest)
    } else {
        anyhow::bail!(
            "[mqtt].broker {raw:?} has no recognised scheme — use \"mqtts://host[:port]\" \
             (TLS, default port 8883) or \"mqtt://host[:port]\" (cleartext, default port 1883)"
        );
    };

    // A single trailing slash is a common paste artifact; anything else is a
    // path, which an MQTT broker URL has no place for.
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') {
        anyhow::bail!("[mqtt].broker {raw:?} must not contain a path");
    }

    // Bracketed IPv6 (`[::1]:8883`) is split explicitly so the address's own
    // colons are never mistaken for the port separator.
    let (host, port) = if let Some(after_bracket) = rest.strip_prefix('[') {
        let (host, tail) = after_bracket.split_once(']').ok_or_else(|| {
            anyhow::anyhow!("[mqtt].broker {raw:?} has an unterminated IPv6 literal")
        })?;
        let port = match tail {
            "" => default_port,
            t => parse_mqtt_port(
                t.strip_prefix(':').ok_or_else(|| {
                    anyhow::anyhow!(
                        "[mqtt].broker {raw:?} has trailing junk after the IPv6 literal"
                    )
                })?,
                raw,
            )?,
        };
        (host, port)
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) => (host, parse_mqtt_port(port, raw)?),
            None => (rest, default_port),
        }
    };

    if host.is_empty() {
        anyhow::bail!("[mqtt].broker {raw:?} has an empty host");
    }
    Ok(MqttEndpoint {
        host: host.to_string(),
        port,
        tls,
    })
}

/// Parse the `:port` half of a broker URL. Rejects non-numeric, out-of-range,
/// and `0` (which would otherwise mean "any port" to the OS and never connect).
fn parse_mqtt_port(port: &str, raw: &str) -> anyhow::Result<u16> {
    let parsed: u16 = port.parse().map_err(|_| {
        anyhow::anyhow!("[mqtt].broker {raw:?} has a non-numeric or out-of-range port {port:?}")
    })?;
    if parsed == 0 {
        anyhow::bail!("[mqtt].broker {raw:?} has port 0, which is not a valid broker port");
    }
    Ok(parsed)
}

/// `[cec]` — HDMI-CEC lifecycle.
///
/// Deliberately does NOT set `deny_unknown_fields` (same rationale as
/// [`PanelConfig`]): the tv-shell-panel binary writes `[cec].osd_name` into
/// the shared config.toml, and panel and daemon are released independently
/// (`input-v*` tags) — a daemon predating a panel-written key must ignore it
/// and keep starting, not abort the whole shell backend on "unknown field".
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct CecConfig {
    /// Wake the AV chain on start/resume and standby on suspend. Default `false`.
    pub lifecycle: bool,
    /// OSD device name announced on the CEC bus — the input label TVs/AVRs
    /// display for this machine. Absent ⇒ the machine hostname, so the AV
    /// chain shows the same name as SSH/the network. Resolved by
    /// [`resolve_osd_name`].
    pub osd_name: Option<String>,
}

/// Maximum OSD name length: libcec's `libcec_configuration.strDeviceName` is a
/// 13-char buffer, so anything longer is truncated on the bus anyway — truncate
/// deterministically here instead.
pub const CEC_OSD_NAME_MAX: usize = 13;

/// Resolve the CEC OSD device name from the `[cec].osd_name` override and the
/// machine hostname: the override wins when non-blank, else the hostname, else
/// the historical `"tv-shell"` fallback. The result is reduced to printable
/// ASCII (CEC OSD strings are ASCII) and truncated to [`CEC_OSD_NAME_MAX`].
pub fn resolve_osd_name(configured: Option<&str>, hostname: Option<&str>) -> String {
    let picked = [configured, hostname]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("tv-shell");
    let cleaned: String = picked
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(CEC_OSD_NAME_MAX)
        .collect();
    if cleaned.trim().is_empty() {
        "tv-shell".to_string()
    } else {
        cleaned
    }
}

/// `[plex]` — Plex home-screen widget. Both a URL and a token are required for
/// the widget to function (the QML collapses it otherwise).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlexConfig {
    pub url: Option<String>,
    /// Path to a `0600` file holding the Plex token. Inline tokens are NOT
    /// supported (matching the HTTP bearer-token policy) — a secret pasted into
    /// config.toml leaks via backups/CI/config-management/shared-host reads.
    pub token_file: Option<String>,
}

/// `[steam]` — Steam library row, pointing at one or more `tv-shell-host`
/// sidecars. Either the legacy single `url` (+ `token_file`) or a list of named
/// `[[steam.hosts]]` entries; [`Config::steam_hosts`] normalizes both forms.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SteamConfig {
    pub url: Option<String>,
    /// Path to a `0600` file holding the Steam/host token. Inline tokens are NOT
    /// supported (same rationale as Plex / the HTTP bearer token). With
    /// `[[steam.hosts]]` entries this is the shared fallback token for hosts
    /// that don't set their own `token_file`.
    pub token_file: Option<String>,
    /// Named sidecar entries (`[[steam.hosts]]`). When non-empty these REPLACE
    /// the legacy single `url`; the active one is selected at runtime via the
    /// `steam-set-host` IPC (persisted as `steamServer` in settings.json).
    pub hosts: Vec<SteamHostConfig>,
    /// Fire a Wake-on-LAN magic packet at the **active** host (only that one,
    /// never the whole `hosts` list) at daemon startup and whenever
    /// `steam-set-host` changes the selection. Default **false** — waking a
    /// machine is a side effect on someone else's hardware, so it stays opt-in.
    ///
    /// Fail-soft: a failed proactive wake is logged and otherwise ignored; it
    /// never blocks startup or the `steam-set-host` reply.
    pub wake_active_host_on_start: bool,
}

/// One named `[[steam.hosts]]` sidecar entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteamHostConfig {
    /// Stable selector name, shown in the widget's server picker and used by
    /// `steam-set-host <name>` (e.g. "desktop-1"). Must be unique + non-empty
    /// (checked by [`Config::validate`]).
    pub name: String,
    /// tv-shell-host base URL, e.g. `http://192.0.2.1:47995`.
    pub url: String,
    /// Per-host token file; absent ⇒ the shared `[steam].token_file` applies.
    pub token_file: Option<String>,
    /// Static Ethernet MAC (`aa:bb:cc:dd:ee:ff`, `-` separators also accepted)
    /// for Wake-on-LAN. Optional, and **preferred over discovery when present**.
    ///
    /// Without it the daemon can only learn the MAC from the live neighbor table
    /// (`ip neigh`) or its persisted `host-macs.json` cache — and a host that has
    /// slept long enough to age out of the neighbor table with a cold cache
    /// simply cannot be woken (`wol` replies `{"status":"error",
    /// "reason":"no-mac"}`). Pinning the MAC here removes that failure mode
    /// entirely. Validated at startup by [`DaemonConfig::validate`], so a typo
    /// fails loudly instead of silently at wake time.
    pub mac: Option<String>,
}

/// `[observability]` — logs + metrics emission (#268).
///
/// `RUST_LOG` is deliberately NOT modelled here: it's the standard
/// `tracing-subscriber` EnvFilter variable, read directly at logging init, and
/// kept as an env var so the usual `RUST_LOG=debug tv-shell-input` workflow
/// still works. Everything else that used to be a `TV_SHELL_*` env var is
/// typed config now.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObservabilityConfig {
    /// Logging backend: `Some(true)` forces the systemd journal
    /// (tracing-journald), `Some(false)` forces plain stdout, `None` = auto
    /// (journald when `JOURNAL_STREAM` indicates a systemd-spawned service).
    /// Was `TV_SHELL_LOG_JOURNAL`.
    pub log_journal: Option<bool>,
    /// node_exporter textfile-collector output path (the PRIMARY metrics path).
    /// `None` ⇒ the textfile writer is disabled (the `/metrics` HTTP route, when
    /// the bridge is bound, is unaffected). Was `TV_SHELL_METRICS_TEXTFILE`.
    pub metrics_textfile: Option<String>,
    /// Textfile render/write interval in seconds. Was `TV_SHELL_METRICS_INTERVAL`.
    pub metrics_interval: u64,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_journal: None,
            metrics_textfile: None,
            // Mirrors metrics.rs DEFAULT_INTERVAL_SECS (15).
            metrics_interval: 15,
        }
    }
}

/// A per-app **input contract**: how the daemon presents the controller fleet
/// to a focused external window, keyed by its Hyprland window class. Selected by
/// the input runtime's follow-focus (the focused window's contract *is* the
/// presenter it drives — see `input.rs`).
///
/// * `Gamepad` — forward each pad to a clean per-player virtual Xbox pad (the
///   Game presenter). The app reads a real gamepad. This is the default for
///   unknown classes, preserving the pre-contract "any app focused ⇒ virtual
///   pad" behavior.
/// * `Keyboard` — emulate keyboard/mouse from the pad, targeted at the focused
///   app (the shell key-map: d-pad→arrows, A→Enter, B→Esc, sticks→arrows/mouse).
///   **No virtual pad exists**, so a key-driven HTPC app like Plex is drivable —
///   and, the point of the fix, Steam has no always-alive virtual pad to take an
///   exclusive `EVIOCGRAB` on out from under the focused app.
/// * `Handoff` — ungrab the physical pad entirely so the app reads the raw evdev
///   node directly (SDL/Moonlight-style). No virtual pad, no key emulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputContract {
    Gamepad,
    Keyboard,
    Handoff,
}

/// Default for [`InputConfig::meta_hold_ms`] — the Meta (BTN_MODE / Guide)
/// tap-vs-hold threshold in milliseconds. 500 ms is a comfortable press-and-hold
/// that a deliberate long-press clears but a quick tap does not. This knob is the
/// tap/hold split point for the Meta gesture in *every* grabbed presenter and
/// replaces the former hard-coded 2 s `HOME_HOLD_SECS`; a shorter default makes
/// the reserved shell escape (the hold) feel responsive.
pub const DEFAULT_META_HOLD_MS: u64 = 500;

/// Default for [`InputConfig::combo_guard_ms`] — the combo settle window in
/// milliseconds. When a focused app owns the screen the daemon briefly buffers a
/// combo-participant press instead of forwarding it, so a partial safety-combo
/// chord (e.g. the first two of Back+Home+LB+RB) never leaks into the app as a
/// stray media key. 120 ms is long enough for a human to complete a chorded combo
/// yet short enough that a genuine single-button press replays with barely
/// perceptible latency.
pub const DEFAULT_COMBO_GUARD_MS: u64 = 120;

/// `[input]` — per-app input contracts and Meta/combo timing. Maps a focused
/// window class to the contract the daemon honors while that window is focused;
/// entries **override** the built-in defaults (see [`builtin_contract`]), an
/// unlisted class falls back to those defaults. Also carries the two input-timing
/// knobs (`meta_hold_ms`, `combo_guard_ms`). Like the other `config.toml`
/// sections this is read once at startup — a change needs a daemon restart.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    /// window-class → contract, e.g. `"tv.plex.Plex" = "keyboard"`. TOML:
    /// `[input.contracts]` with one `"<class>" = "<gamepad|keyboard|handoff>"`
    /// line per app.
    pub contracts: HashMap<String, InputContract>,
    /// Meta (BTN_MODE / Guide) tap-vs-hold threshold in milliseconds
    /// ([`DEFAULT_META_HOLD_MS`]). Held past this ⇒ HOLD (the reserved shell
    /// escape); released before ⇒ TAP (delivered to the focused app per its
    /// contract). Threaded into the input runtime's `Shared` at startup.
    pub meta_hold_ms: u64,
    /// Combo settle window in milliseconds ([`DEFAULT_COMBO_GUARD_MS`]) — how long
    /// a buffered combo-participant press waits for the rest of a combo before it
    /// is replayed to the focused app as a normal press. Threaded into `Shared` at
    /// startup.
    pub combo_guard_ms: u64,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            contracts: HashMap::new(),
            meta_hold_ms: DEFAULT_META_HOLD_MS,
            combo_guard_ms: DEFAULT_COMBO_GUARD_MS,
        }
    }
}

/// Resolver for per-app input contracts: user `[input.contracts]` overrides
/// layered over the daemon's built-in defaults. Cloned into the input runtime at
/// startup ([`DaemonConfig::input_contracts`]) so contract resolution on the hot
/// focus path is a plain map lookup with no global/`OnceLock` access, and so the
/// resolution logic is unit-tested here (cross-platform) rather than in the
/// Linux-only input module.
#[derive(Debug, Clone, Default)]
pub struct InputContracts {
    overrides: HashMap<String, InputContract>,
}

impl InputContracts {
    /// Build from the parsed `[input]` config (clones the override map).
    pub fn from_config(cfg: &InputConfig) -> Self {
        Self {
            overrides: cfg.contracts.clone(),
        }
    }

    /// Construct directly from an override map (tests / explicit callers).
    pub fn new(overrides: HashMap<String, InputContract>) -> Self {
        Self { overrides }
    }

    /// Resolve the contract for a focused window `class`: a user override wins,
    /// else the built-in default. An unknown class defaults to
    /// [`InputContract::Gamepad`] — preserving the pre-contract behavior where
    /// any focused app got a virtual pad. `class` is never empty here: an empty
    /// focused-window class means "no toplevel focused → the shell itself owns
    /// input", which the caller maps to the shell presenter before consulting a
    /// contract.
    pub fn resolve(&self, class: &str) -> InputContract {
        self.overrides
            .get(class)
            .copied()
            .unwrap_or_else(|| builtin_contract(class))
    }
}

/// Built-in per-app contract defaults, overridable via `[input.contracts]`.
/// `tv.plex.Plex` is keyboard-driven (the Plex HTPC UI reads keys, not a pad);
/// `steam` takes a real gamepad; every other class defaults to gamepad (the
/// pre-contract behavior). `steam` is listed explicitly even though it matches
/// the fallback — it documents the intended contract and survives a future change
/// to the default.
fn builtin_contract(class: &str) -> InputContract {
    match class {
        "tv.plex.Plex" => InputContract::Keyboard,
        "steam" => InputContract::Gamepad,
        _ => InputContract::Gamepad,
    }
}

/// `[dev]` — operator escape hatches.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevConfig {
    /// Explicitly permit the otherwise-refused LAN + dev-tools + no-auth combo.
    /// This is how a trusted single-user host keeps its intentional insecure dev loop.
    pub allow_insecure_lan: bool,
}

/// Default config path: `~/.config/tv-shell/config.toml`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// `${XDG_CONFIG_HOME:-$HOME/.config}/tv-shell` (legacy `…/game-shell` honored
/// as a read-fallback via [`tv_shell_protocol::brand::config_dir`]).
fn config_dir() -> PathBuf {
    tv_shell_protocol::brand::config_dir()
}

/// Expand a leading `~/` (or bare `~`) in a config path to `$HOME`. Other paths
/// pass through unchanged. Used for non-secret output paths (e.g.
/// `metrics_textfile`) where the operator may legitimately point outside the
/// config dir; secret token files go through [`resolve_token_path`] instead.
fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if p == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(p)
}

/// Resolve a **token file** path and confine it to the config dir (CWE-22 guard).
///
/// A token file holds a secret the daemon reads with its own privileges, so a
/// config writer must not be able to point it at arbitrary paths
/// (`../../../etc/shadow`, `/tmp/attacker`). After tilde-expansion the path is
/// canonicalized and required to live within `~/.config/tv-shell/`; anything
/// escaping the config dir is a hard error (refuse startup). Canonicalizing also
/// resolves `..`/symlinks, so a symlink inside the config dir pointing out is
/// caught too.
///
/// NOTE: this is intentionally NOT applied to `metrics_textfile` — that is an
/// OUTPUT the operator legitimately points outside the config dir (e.g.
/// node_exporter's `/var/lib/node_exporter/textfile/`), not a secret the daemon
/// reads. Output-path safety there is bounded by filesystem permissions, not by
/// confinement to the config dir.
fn resolve_token_path(p: &str, field: &str) -> anyhow::Result<PathBuf> {
    let expanded = expand_tilde(p);
    let canonical = expanded.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "{field} {}: cannot resolve token file path: {e}",
            expanded.display()
        )
    })?;
    let config_dir = config_dir()
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot resolve config dir for {field} validation: {e}"))?;
    if !canonical.starts_with(&config_dir) {
        return Err(anyhow::anyhow!(
            "{field} {} escapes the config directory {} — a token file must live \
             under ~/.config/tv-shell/ (refusing to read a secret from an \
             arbitrary path)",
            canonical.display(),
            config_dir.display()
        ));
    }
    Ok(canonical)
}

impl DaemonConfig {
    /// Load and parse `config.toml`. A missing file is not an error — it yields
    /// the all-default config (everything off), so a fresh install still boots.
    /// A present-but-malformed file IS an error (the operator should know their
    /// config was ignored rather than silently running with defaults).
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&config_path())
    }

    /// Load from an explicit path (testable; no env/global state).
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            // Absent ⇒ defaults. Any other read error (perms, etc.) surfaces.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("reading {}: {e}", path.display())),
        }
    }

    /// Parse a TOML document (no I/O).
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        toml::from_str(text).map_err(|e| anyhow::anyhow!("parsing config.toml: {e}"))
    }

    /// Parse the HTTP bind address, if any. `Err` on a malformed address so the
    /// operator gets a clear startup failure rather than a silently-off bridge.
    pub fn http_bind(&self) -> anyhow::Result<Option<SocketAddr>> {
        parse_bind("http.bind", self.http.bind.as_deref())
    }

    /// Parse the MCP bind address, if any.
    pub fn mcp_bind(&self) -> anyhow::Result<Option<SocketAddr>> {
        parse_bind("mcp.bind", self.mcp.bind.as_deref())
    }

    /// Resolve the shared bearer token from `[http].token_file`. `Ok(None)` when
    /// no token file is configured or it is empty (both mean "no token" →
    /// fail-closed when auth is enabled). `Err` when the path escapes the config
    /// dir (CWE-22) or the file is group/other-accessible (fail-closed: a leaked
    /// token must abort startup, not run with a compromised secret).
    pub fn http_token(&self) -> anyhow::Result<Option<String>> {
        match self.http.token_file.as_deref() {
            Some(p) => read_token_file(
                &resolve_token_path(p, "http.token_file")?,
                "http.token_file",
            ),
            None => Ok(None),
        }
    }

    /// The validated MQTT device identity.
    ///
    /// `Ok(None)` when `[mqtt].broker` is unset (MQTT is off, so there is no
    /// identity to resolve). When `broker` IS set, a missing `device_id` is a
    /// hard error: **fail closed**, matching the daemon's posture on token files.
    /// Deriving the id from hostname/OS/IP is what the error message forbids —
    /// the desktop is one physical machine that dual-boots, and a derived id
    /// would split it into two Home Assistant devices that alternate.
    pub fn mqtt_device_id(&self) -> anyhow::Result<Option<tv_shell_protocol::mqtt::DeviceId>> {
        if self.mqtt.broker.is_none() {
            return Ok(None);
        }
        let raw = self.mqtt.device_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "[mqtt].device_id is required when [mqtt].broker is set; it must be set \
                 explicitly and identically on every boot of the machine — deriving it from \
                 hostname or OS produces two Home Assistant devices for one dual-boot machine"
            )
        })?;
        let id = tv_shell_protocol::mqtt::DeviceId::new(raw)
            .map_err(|e| anyhow::anyhow!("[mqtt].device_id {raw:?} is invalid: {e}"))?;
        Ok(Some(id))
    }

    /// Resolve the MQTT password from `[mqtt].password_file`. Same fail-closed
    /// semantics as [`DaemonConfig::http_token`]: config-dir-confined, 0600, and
    /// an empty file reads as "no password".
    pub fn mqtt_password(&self) -> anyhow::Result<Option<String>> {
        match self.mqtt.password_file.as_deref() {
            Some(p) => read_token_file(
                &resolve_token_path(p, "mqtt.password_file")?,
                "mqtt.password_file",
            ),
            None => Ok(None),
        }
    }

    /// The `[mqtt].ca_file` path, tilde-expanded only.
    ///
    /// A CA **certificate is public** — it is what the broker presents to every
    /// client — so unlike a token file it is deliberately NOT mode-checked and
    /// NOT confined to the config dir: an operator may legitimately point at a
    /// system trust bundle (`/etc/ssl/certs/…`) or an Ansible-managed path.
    pub fn mqtt_ca_path(&self) -> Option<PathBuf> {
        self.mqtt.ca_file.as_deref().map(expand_tilde)
    }

    /// Parse `[mqtt].broker`. `Ok(None)` ⇒ MQTT is off; `Err` on a malformed URL
    /// so the operator gets a clear startup failure rather than a silently-off
    /// publisher.
    pub fn mqtt_endpoint(&self) -> anyhow::Result<Option<MqttEndpoint>> {
        match self.mqtt.broker.as_deref() {
            None => Ok(None),
            Some(raw) => parse_mqtt_endpoint(raw).map(Some),
        }
    }

    /// Resolve the Plex token from `[plex].token_file` (token-file only; inline
    /// tokens are not supported — see PlexConfig). Same fail-closed semantics as
    /// [`http_token`].
    pub fn plex_token(&self) -> anyhow::Result<Option<String>> {
        match self.plex.token_file.as_deref() {
            Some(p) => read_token_file(
                &resolve_token_path(p, "plex.token_file")?,
                "plex.token_file",
            ),
            None => Ok(None),
        }
    }

    /// The configured Steam sidecar hosts, normalized to the named-host form:
    /// explicit `[[steam.hosts]]` entries when present (the legacy single
    /// `[steam].url` is then ignored), else the legacy `url` as one host named
    /// by the URL's host part. Empty ⇒ the Steam widget is unconfigured.
    pub fn steam_hosts(&self) -> Vec<SteamHostConfig> {
        if !self.steam.hosts.is_empty() {
            return self.steam.hosts.clone();
        }
        match self.steam.url.as_deref() {
            Some(url) => vec![SteamHostConfig {
                name: crate::sidecar::url_host(url).unwrap_or_else(|| "default".to_string()),
                url: url.to_string(),
                token_file: None,
                // The legacy single-`url` form has no place to declare a MAC;
                // static-MAC config is a `[[steam.hosts]]` feature. WoL still
                // works here via neighbor-table/cache discovery, as before.
                mac: None,
            }],
            None => Vec::new(),
        }
    }

    /// Resolve the bearer token for one Steam host: its own `token_file` when
    /// set, else the shared `[steam].token_file` (token-file only, like Plex).
    pub fn steam_token_for(&self, host: &SteamHostConfig) -> anyhow::Result<Option<String>> {
        match host
            .token_file
            .as_deref()
            .or(self.steam.token_file.as_deref())
        {
            Some(p) => read_token_file(
                &resolve_token_path(p, "steam.token_file")?,
                "steam.token_file",
            ),
            None => Ok(None),
        }
    }

    /// Metrics textfile write interval in seconds, clamped to ≥1 so a `0` (which
    /// would busy-loop the writer) falls back to the 15s default — mirroring the
    /// old `interval_secs()` env parser's `filter(|&n| n > 0)`.
    pub fn metrics_interval_secs(&self) -> u64 {
        let n = self.observability.metrics_interval;
        if n == 0 {
            15
        } else {
            n
        }
    }

    /// The per-app input-contract resolver (built-in defaults layered under the
    /// `[input.contracts]` overrides). Cloned by the input runtime at startup.
    pub fn input_contracts(&self) -> InputContracts {
        InputContracts::from_config(&self.input)
    }

    /// Validate cross-field invariants, refusing to run in a configuration that
    /// would expose an unauthenticated remote-control / RCE surface on the LAN.
    ///
    /// For BOTH the HTTP bridge and the MCP server, the dangerous combination is:
    /// a **non-loopback** bind + **dev tools** enabled + auth **effectively
    /// disabled** (auth off, or no token resolvable). The HTTP bridge's dev tools
    /// are its `/dev/*` endpoints (build/deploy/restart); the MCP server's are
    /// gated by `[mcp].dev`. Returning `Err` here aborts startup.
    ///
    /// `[dev].allow_insecure_lan = true` is an explicit operator opt-in that
    /// downgrades the refusal to a loud warning — this is how a trusted single-user host keeps
    /// its intentional LAN + dev + no-auth dev loop.
    pub fn validate(&self) -> anyhow::Result<()> {
        // Resolve the token eagerly so a path-traversal / world-readable token
        // file aborts startup here (fail-closed), not silently as "no token".
        let token = self.http_token()?;
        let auth_effectively_disabled = !self.http.auth_enabled || token.is_none();

        // `[[steam.hosts]]` names are the `steam-set-host` selectors — an empty
        // or duplicate name would make selection ambiguous, so refuse at startup.
        let mut steam_names = std::collections::HashSet::new();
        for h in &self.steam.hosts {
            if h.name.trim().is_empty() {
                anyhow::bail!(
                    "config: [[steam.hosts]] entry with an empty name (url = {:?})",
                    h.url
                );
            }
            if !steam_names.insert(h.name.as_str()) {
                anyhow::bail!("config: duplicate [[steam.hosts]] name {:?}", h.name);
            }
            // A configured `mac` is the PREFERRED Wake-on-LAN source, so a typo
            // must fail loudly at startup rather than silently degrade to
            // neighbor-table/cache discovery on the one wake that needed it.
            if let Some(mac) = &h.mac {
                if crate::wol::Mac::parse(mac).is_none() {
                    anyhow::bail!(
                        "config: [[steam.hosts]] {:?} has an unparseable mac {:?} \
                         (expected six hex octets, e.g. \"aa:bb:cc:dd:ee:ff\")",
                        h.name,
                        mac
                    );
                }
            }
        }

        // NOTE: `[mqtt]` is deliberately NOT validated here. See
        // [`DaemonConfig::mqtt_settings`] — an optional subsystem's config must
        // not be able to stop the daemon that owns the shell, CEC and input from
        // starting.

        // The HTTP bridge always exposes its /dev/* tools, so a non-loopback
        // bridge with no auth is an unauthenticated RCE surface regardless of MCP.
        if let Some(addr) = self.http_bind()? {
            if !addr.ip().is_loopback() && auth_effectively_disabled {
                self.refuse_or_warn(
                    "HTTP control bridge",
                    addr,
                    "its /dev/* endpoints (build/deploy/restart) are an unauthenticated RCE surface",
                )?;
            }
        }

        // The MCP server only exposes dev tools when [mcp].dev is set; without
        // dev tools an unauthenticated MCP surface is still a remote-control leak
        // but not RCE — match the existing mcp.rs refusal which gated on dev.
        if let Some(addr) = self.mcp_bind()? {
            if self.mcp.dev && !addr.ip().is_loopback() && auth_effectively_disabled {
                self.refuse_or_warn(
                    "MCP server",
                    addr,
                    "tv-shell dev tools over MCP are an unauthenticated RCE surface",
                )?;
            }
        }

        Ok(())
    }

    /// Resolve everything the MQTT actor needs, or explain why it cannot run.
    ///
    /// - `Ok(None)`   — `[mqtt].broker` is unset: MQTT is off, which is normal.
    /// - `Ok(Some(_))` — fully resolved and safe to spawn.
    /// - `Err(_)`     — the `[mqtt]` section is misconfigured.
    ///
    /// **`Err` must NOT abort the daemon.** This is deliberately not called from
    /// [`DaemonConfig::validate`]. `validate()` aborts on the daemon's own
    /// mandatory configuration; `[mqtt]` is an *optional* subsystem, and letting
    /// it stop a daemon that owns the shell, CEC and the input fleet would make
    /// MQTT subtractive — able to break features that have nothing to do with it.
    /// The caller logs the error loudly and skips the actor; everything else
    /// starts normally. A misconfigured `[mqtt]` costs you an MQTT device in Home
    /// Assistant, nothing more.
    ///
    /// (A *malformed* `[mqtt]` table still aborts at parse time, because
    /// `DaemonConfig` is `deny_unknown_fields`. That is a separate mechanism —
    /// the point here is not to add a second abort path on top of it.)
    ///
    /// Resolution is still **fail-closed in what it publishes**: a missing
    /// `device_id` refuses to invent one, and a world-readable password file
    /// refuses to be read. It just fails the subsystem, not the process.
    pub fn mqtt_settings(&self) -> anyhow::Result<Option<ResolvedMqtt>> {
        // Both parse even when `broker` is unset (they answer Ok(None) then), so
        // calling them unconditionally is the cheapest way to make a malformed
        // URL / invalid device_id abort startup.
        let endpoint = self.mqtt_endpoint()?;
        self.mqtt_device_id()?;

        let Some(endpoint) = endpoint else {
            // MQTT off: nothing else in this section can matter.
            return Ok(None);
        };

        // Half-configured credentials silently authenticate as anonymous against
        // a broker that expects a user — refuse rather than let that look like a
        // broker-side ACL problem.
        match (
            self.mqtt.username.is_some(),
            self.mqtt.password_file.is_some(),
        ) {
            (true, false) => anyhow::bail!(
                "config: [mqtt].username is set without [mqtt].password_file — set both or neither"
            ),
            (false, true) => anyhow::bail!(
                "config: [mqtt].password_file is set without [mqtt].username — set both or neither"
            ),
            _ => {}
        }

        // Resolve eagerly: a path-escaping or group/other-readable password file
        // must abort startup, not degrade to "no password".
        self.mqtt_password()?;

        if self.mqtt.heartbeat_secs == 0 {
            anyhow::bail!(
                "config: [mqtt].heartbeat_secs must be > 0 — a zero floor heartbeat would \
                 publish on every tick and flood the broker"
            );
        }
        if self.mqtt.keepalive_secs == 0 {
            anyhow::bail!("config: [mqtt].keepalive_secs must be > 0");
        }

        if !endpoint.tls {
            // A local test broker over cleartext is legitimate, so warn rather
            // than refuse — but the credentials and every state payload cross the
            // LAN in the clear, which the operator should know about.
            tracing::warn!(
                "config: [mqtt].broker uses the cleartext mqtt:// scheme ({}:{}) — the MQTT \
                 password and all published state cross the LAN unencrypted. Use mqtts:// \
                 unless this is a local test broker.",
                endpoint.host,
                endpoint.port
            );
        }

        Ok(Some(ResolvedMqtt {
            device_id: self
                .mqtt_device_id()?
                .expect("device_id resolves to Some whenever broker is Some"),
            endpoint,
            username: self.mqtt.username.clone(),
            password: self.mqtt_password()?,
            ca_path: self.mqtt_ca_path(),
            heartbeat_secs: self.mqtt.heartbeat_secs,
            keepalive_secs: self.mqtt.keepalive_secs,
        }))
    }

    /// Either return an error (refuse to start) or, when the operator has opted
    /// into `[dev].allow_insecure_lan`, log a loud warning and continue.
    fn refuse_or_warn(&self, surface: &str, addr: SocketAddr, why: &str) -> anyhow::Result<()> {
        if self.dev.allow_insecure_lan {
            // error!, not warn!: the escape hatch is a deliberate hole, and a
            // forgotten `allow_insecure_lan = true` (e.g. a copy-pasted dev
            // config) silently opens an unauthenticated RCE surface to the LAN.
            // Logging at error level makes that impossible to miss at startup.
            tracing::error!(
                "config: {surface} bound to non-loopback {addr} with auth effectively \
                 disabled — {why}. PERMITTED ONLY because [dev].allow_insecure_lan = true; \
                 remove it unless this box intentionally runs an unauthenticated LAN dev loop."
            );
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "refusing to start: {surface} is bound to non-loopback {addr} with auth \
                 effectively disabled (no token / auth off) — {why}. Set [http].token_file \
                 (0600) and [http].auth_enabled = true, bind to 127.0.0.1, or explicitly \
                 opt in with [dev].allow_insecure_lan = true."
            ))
        }
    }
}

/// Parse an optional `host:port` bind string into a `SocketAddr`.
fn parse_bind(field: &str, value: Option<&str>) -> anyhow::Result<Option<SocketAddr>> {
    match value {
        None => Ok(None),
        Some(s) => s
            .parse::<SocketAddr>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{field} = {s:?} is not a valid host:port address: {e}")),
    }
}

/// Read a bearer/API token from a (config-dir-confined) file: trim trailing
/// whitespace/newline, treat empty as `Ok(None)` ("no token" → fail-closed when
/// auth is on). A group/other-accessible file is a hard `Err` (fail-closed): a
/// world-readable secret lets any local user / co-hosted service assume daemon
/// privileges, so the daemon refuses to start rather than run with a leaked token.
fn read_token_file(path: &Path, field: &str) -> anyhow::Result<Option<String>> {
    ensure_owner_only(path, field)?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("config: {field} {} unreadable: {e}", path.display()))?;
    let token = raw.trim();
    if token.is_empty() {
        tracing::warn!(
            "config: {field} {} is empty — treating as no token",
            path.display()
        );
        Ok(None)
    } else {
        Ok(Some(token.to_string()))
    }
}

/// Fail-closed if a token file is readable by group/other (mode & 0o077 != 0).
/// Unix-only check; a no-op elsewhere (non-Unix has no POSIX mode bits).
#[cfg(unix)]
fn ensure_owner_only(path: &Path, field: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("config: {field} {} stat failed: {e}", path.display()))?;
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(anyhow::anyhow!(
            "config: {field} {} is group/other-accessible (mode {:o}); refusing to \
             start — a bearer/API token must be private. Fix: chmod 600 {}",
            path.display(),
            mode & 0o7777,
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only(_path: &Path, _field: &str) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osd_name_prefers_config_then_hostname_then_fallback() {
        assert_eq!(
            resolve_osd_name(Some("living-room"), Some("htpc-1")),
            "living-room"
        );
        assert_eq!(resolve_osd_name(None, Some("htpc-1")), "htpc-1");
        assert_eq!(resolve_osd_name(Some("  "), Some("htpc-1")), "htpc-1");
        assert_eq!(resolve_osd_name(None, None), "tv-shell");
        assert_eq!(resolve_osd_name(Some(""), Some(" ")), "tv-shell");
    }

    #[test]
    fn osd_name_is_ascii_and_truncated() {
        // 13-char libcec strDeviceName limit.
        assert_eq!(
            resolve_osd_name(Some("a-very-long-device-name"), None),
            "a-very-long-d"
        );
        // Non-ASCII is dropped, not mangled.
        assert_eq!(resolve_osd_name(Some("héllo tv"), None), "hllo tv");
        // All-non-ASCII collapses to the fallback rather than an empty name.
        assert_eq!(resolve_osd_name(Some("телевизор"), None), "tv-shell");
    }

    fn loopback_http(dev_allow: bool, bind: &str, auth: bool) -> DaemonConfig {
        let mut c = DaemonConfig::default();
        c.http.bind = Some(bind.to_string());
        c.http.auth_enabled = auth;
        c.dev.allow_insecure_lan = dev_allow;
        c
    }

    #[test]
    fn empty_config_is_all_default_off() {
        let c = DaemonConfig::parse("").unwrap();
        assert!(c.http.bind.is_none());
        assert!(c.http.auth_enabled); // secure by default
        assert!(c.mcp.bind.is_none());
        assert!(!c.mcp.dev);
        assert!(!c.cec.lifecycle);
        assert!(!c.dev.allow_insecure_lan);
        // Observability defaults: auto log backend, no textfile, 15s interval.
        assert_eq!(c.observability.log_journal, None);
        assert!(c.observability.metrics_textfile.is_none());
        assert_eq!(c.metrics_interval_secs(), 15);
        c.validate().unwrap(); // nothing bound ⇒ trivially valid
    }

    #[test]
    fn observability_section_parses_and_interval_clamps() {
        let c = DaemonConfig::parse(
            r#"
            [observability]
            log_journal = true
            metrics_textfile = "/var/lib/node_exporter/textfile/tv-shell.prom"
            metrics_interval = 30
        "#,
        )
        .unwrap();
        assert_eq!(c.observability.log_journal, Some(true));
        assert_eq!(
            c.observability.metrics_textfile.as_deref(),
            Some("/var/lib/node_exporter/textfile/tv-shell.prom")
        );
        assert_eq!(c.metrics_interval_secs(), 30);

        // A zero interval (busy-loop) clamps back to the 15s default.
        let z = DaemonConfig::parse("[observability]\nmetrics_interval = 0\n").unwrap();
        assert_eq!(z.metrics_interval_secs(), 15);

        // log_journal = false forces stdout.
        let f = DaemonConfig::parse("[observability]\nlog_journal = false\n").unwrap();
        assert_eq!(f.observability.log_journal, Some(false));
    }

    #[test]
    fn full_config_parses() {
        let toml = r#"
            [http]
            bind = "127.0.0.1:8089"
            auth_enabled = true
            token_file = "/run/secrets/http-token"

            [mcp]
            bind = "127.0.0.1:8090"
            dev = true
            allowed_hosts = ["localhost", "my-host.local"]

            [cec]
            lifecycle = true

            [plex]
            url = "http://plex:32400"
            token_file = "/run/secrets/plex-token"

            [steam]
            url = "http://gaming-pc:47995"
            token_file = "/run/secrets/steam-token"

            [dev]
            allow_insecure_lan = false
        "#;
        let c = DaemonConfig::parse(toml).unwrap();
        assert_eq!(c.http.bind.as_deref(), Some("127.0.0.1:8089"));
        assert_eq!(c.mcp.allowed_hosts, vec!["localhost", "my-host.local"]);
        assert!(c.mcp.dev);
        assert!(c.cec.lifecycle);
        assert_eq!(c.plex.url.as_deref(), Some("http://plex:32400"));
        // Plex/Steam are token-file only now (inline `token` is a rejected
        // unknown field — verified in inline_token_is_rejected below).
        assert_eq!(
            c.plex.token_file.as_deref(),
            Some("/run/secrets/plex-token")
        );
        assert_eq!(c.http_bind().unwrap().unwrap().port(), 8089);
    }

    #[test]
    fn input_contract_builtin_defaults() {
        // No overrides: the built-ins apply. Plex is keyboard-driven; Steam and
        // any unknown class take a gamepad (the pre-contract default).
        let c = DaemonConfig::parse("").unwrap();
        let contracts = c.input_contracts();
        assert_eq!(contracts.resolve("tv.plex.Plex"), InputContract::Keyboard);
        assert_eq!(contracts.resolve("steam"), InputContract::Gamepad);
        assert_eq!(
            contracts.resolve("org.some.UnknownApp"),
            InputContract::Gamepad
        );
    }

    #[test]
    fn input_contract_overrides_win_and_parse() {
        // A user can both override a built-in (force Plex to gamepad) and add a
        // new class (VLC → keyboard, handoff for a raw-node app).
        let c = DaemonConfig::parse(
            r#"
            [input.contracts]
            "tv.plex.Plex" = "gamepad"
            "org.videolan.VLC" = "keyboard"
            "com.example.RawPad" = "handoff"
        "#,
        )
        .unwrap();
        let contracts = c.input_contracts();
        // Override beats the built-in keyboard default.
        assert_eq!(contracts.resolve("tv.plex.Plex"), InputContract::Gamepad);
        assert_eq!(
            contracts.resolve("org.videolan.VLC"),
            InputContract::Keyboard
        );
        assert_eq!(
            contracts.resolve("com.example.RawPad"),
            InputContract::Handoff
        );
        // An unlisted class still falls through to the built-in default.
        assert_eq!(contracts.resolve("steam"), InputContract::Gamepad);
    }

    #[test]
    fn input_contract_invalid_value_rejected() {
        // A typo'd contract value is a hard parse error (not silently ignored).
        assert!(DaemonConfig::parse("[input.contracts]\n\"steam\" = \"joystick\"\n").is_err());
        // An unknown key under [input] is rejected too (deny_unknown_fields).
        assert!(DaemonConfig::parse("[input]\nbogus = 1\n").is_err());
    }

    #[test]
    fn input_timing_defaults_and_parse() {
        // Absent [input] ⇒ the timing knobs take their defaults.
        let c = DaemonConfig::parse("").unwrap();
        assert_eq!(c.input.meta_hold_ms, DEFAULT_META_HOLD_MS);
        assert_eq!(c.input.combo_guard_ms, DEFAULT_COMBO_GUARD_MS);

        // A [input] table with only contracts still defaults the timing knobs
        // (container-level serde(default) fills missing fields from Default).
        let c = DaemonConfig::parse("[input.contracts]\n\"steam\" = \"gamepad\"\n").unwrap();
        assert_eq!(c.input.meta_hold_ms, DEFAULT_META_HOLD_MS);
        assert_eq!(c.input.combo_guard_ms, DEFAULT_COMBO_GUARD_MS);

        // Explicit values parse and override the defaults.
        let c = DaemonConfig::parse(
            r#"
            [input]
            meta_hold_ms = 750
            combo_guard_ms = 60
        "#,
        )
        .unwrap();
        assert_eq!(c.input.meta_hold_ms, 750);
        assert_eq!(c.input.combo_guard_ms, 60);

        // A non-integer timing value is a hard parse error (not silently ignored).
        assert!(DaemonConfig::parse("[input]\nmeta_hold_ms = \"soon\"\n").is_err());
    }

    #[test]
    fn steam_host_mac_parses_and_normalizes_into_hosts() {
        let c = DaemonConfig::parse(
            r#"
            [[steam.hosts]]
            name = "desktop-1"
            url = "http://192.0.2.10:47995"
            mac = "AA-BB-CC-DD-EE-FF"

            [[steam.hosts]]
            name = "desktop-1-windows"
            url = "http://192.0.2.20:47995"
        "#,
        )
        .unwrap();
        c.validate().unwrap();
        let hosts = c.steam_hosts();
        assert_eq!(hosts[0].mac.as_deref(), Some("AA-BB-CC-DD-EE-FF"));
        // `mac` is optional — an entry without one keeps the discovery behavior.
        assert_eq!(hosts[1].mac, None);
    }

    #[test]
    fn steam_host_mac_absent_is_the_unchanged_default() {
        // The legacy single-`url` form has nowhere to declare a MAC, and
        // normalizes to a host with `mac: None` (WoL still discovers as before).
        let c = DaemonConfig::parse("[steam]\nurl = \"http://gaming-pc:47995\"\n").unwrap();
        c.validate().unwrap();
        let hosts = c.steam_hosts();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].mac, None);
    }

    #[test]
    fn unparseable_steam_host_mac_is_refused_at_startup() {
        // A configured MAC is the PREFERRED WoL source, so a typo must abort
        // startup rather than silently degrade on the one wake that needed it.
        for bad in ["not-a-mac", "aa:bb:cc:dd:ee", "zz:bb:cc:dd:ee:ff", ""] {
            let c = DaemonConfig::parse(&format!(
                "[[steam.hosts]]\nname = \"d1\"\nurl = \"http://h:1\"\nmac = \"{bad}\"\n"
            ))
            .unwrap();
            let err = c
                .validate()
                .expect_err("an unparseable mac must fail validation");
            let msg = err.to_string();
            assert!(msg.contains("mac"), "{msg}");
            assert!(msg.contains("d1"), "{msg}");
        }
    }

    #[test]
    fn proactive_wake_flag_defaults_off() {
        // Waking someone else's machine is a side effect — it stays opt-in.
        assert!(!DaemonConfig::default().steam.wake_active_host_on_start);
        assert!(
            !DaemonConfig::parse("[steam]\nurl = \"http://h:1\"\n")
                .unwrap()
                .steam
                .wake_active_host_on_start
        );
        assert!(
            DaemonConfig::parse("[steam]\nwake_active_host_on_start = true\n")
                .unwrap()
                .steam
                .wake_active_host_on_start
        );
    }

    #[test]
    fn inline_plex_steam_token_is_rejected() {
        // #5: inline tokens are not supported; deny_unknown_fields rejects them so
        // an operator can't paste a raw secret into config.toml.
        assert!(DaemonConfig::parse("[plex]\ntoken = \"x\"\n").is_err());
        assert!(DaemonConfig::parse("[steam]\ntoken = \"x\"\n").is_err());
    }

    #[test]
    fn panel_section_parses_and_is_ignored_by_daemon() {
        // The shared config.toml carries a [panel] section for the tv-shell-panel
        // binary. The daemon's top-level deny_unknown_fields must NOT reject it —
        // otherwise adding [panel] would abort daemon startup. The daemon parses
        // and ignores it; the panel binary is the real consumer.
        let c = DaemonConfig::parse(
            r#"
            [panel]
            enabled = true
            bind = "127.0.0.1:8091"

            [http]
            bind = "127.0.0.1:8089"
        "#,
        )
        .unwrap();
        assert!(c.panel.enabled);
        assert_eq!(c.panel.bind.as_deref(), Some("127.0.0.1:8091"));
        // The daemon's own sections still parse alongside it.
        assert_eq!(c.http.bind.as_deref(), Some("127.0.0.1:8089"));
        // An absent [panel] defaults to enabled = true (panel serves by default).
        let d = DaemonConfig::parse("").unwrap();
        assert!(d.panel.enabled);
        // A panel-only key the daemon doesn't model is tolerated (no
        // deny_unknown_fields on PanelConfig), so the panel can grow its config
        // without a matching daemon change.
        assert!(DaemonConfig::parse("[panel]\nfuture_panel_key = 42\n").is_ok());
    }

    // ── [mqtt] ──────────────────────────────────────────────────────────────

    #[test]
    fn mqtt_section_parses_with_every_key_and_with_none() {
        let c = DaemonConfig::parse(
            r#"
            [mqtt]
            broker = "mqtts://mqtt.example:8883"
            device_id = "htpc-1"
            username = "tv-shell-htpc-1"
            password_file = "~/.config/tv-shell/mqtt-password"
            ca_file = "~/.config/tv-shell/mqtt-ca.pem"
            heartbeat_secs = 15
            keepalive_secs = 45
        "#,
        )
        .unwrap();
        assert_eq!(c.mqtt.broker.as_deref(), Some("mqtts://mqtt.example:8883"));
        assert_eq!(c.mqtt.device_id.as_deref(), Some("htpc-1"));
        assert_eq!(c.mqtt.username.as_deref(), Some("tv-shell-htpc-1"));
        assert_eq!(
            c.mqtt.password_file.as_deref(),
            Some("~/.config/tv-shell/mqtt-password")
        );
        assert_eq!(c.mqtt.heartbeat_secs, 15);
        assert_eq!(c.mqtt.keepalive_secs, 45);
        assert_eq!(
            c.mqtt_device_id()
                .unwrap()
                .map(|d| d.to_string())
                .as_deref(),
            Some("htpc-1")
        );

        // An empty [mqtt] table is all-default (MQTT off).
        let empty = DaemonConfig::parse("[mqtt]\n").unwrap();
        assert!(empty.mqtt.broker.is_none());
        assert!(empty.mqtt.device_id.is_none());
        assert!(empty.mqtt_ca_path().is_none());
        assert!(empty.mqtt_endpoint().unwrap().is_none());
    }

    #[test]
    fn mqtt_unknown_key_is_rejected() {
        // Proves deny_unknown_fields is live on [mqtt] (unlike [cec]/[panel]).
        assert!(DaemonConfig::parse("[mqtt]\nbrokr = \"mqtt://h\"\n").is_err());
    }

    #[test]
    fn mqtt_interval_defaults_are_nonzero() {
        // A derived Default would give 0 for both, which busy-loops the publisher.
        let c = DaemonConfig::parse("").unwrap();
        assert_eq!(c.mqtt.heartbeat_secs, 30);
        assert_eq!(c.mqtt.keepalive_secs, 60);
        assert_eq!(MqttConfig::default().heartbeat_secs, 30);
        assert_eq!(MqttConfig::default().keepalive_secs, 60);
    }

    #[test]
    fn mqtt_endpoint_parse_table() {
        let ok: &[(&str, &str, u16, bool)] = &[
            ("mqtt://h", "h", 1883, false),
            ("mqtts://h", "h", 8883, true),
            ("mqtts://h:1234", "h", 1234, true),
            ("mqtt://h:1883", "h", 1883, false),
            // A single trailing slash is tolerated (paste artifact).
            ("mqtts://h/", "h", 8883, true),
            // Bracketed IPv6: the address's own colons are not the port split.
            ("mqtts://[::1]", "::1", 8883, true),
            ("mqtts://[::1]:1234", "::1", 1234, true),
        ];
        for (raw, host, port, tls) in ok {
            let mut c = DaemonConfig::default();
            c.mqtt.broker = Some((*raw).to_string());
            let got = c
                .mqtt_endpoint()
                .unwrap_or_else(|e| panic!("{raw} should parse: {e}"))
                .unwrap();
            assert_eq!(
                got,
                MqttEndpoint {
                    host: (*host).to_string(),
                    port: *port,
                    tls: *tls
                },
                "for {raw}"
            );
        }

        let bad = [
            "http://h",        // wrong scheme
            "h:1883",          // no scheme at all
            "mqtts://",        // empty host
            "mqtt://",         // empty host
            "mqtts://h:0",     // port 0
            "mqtts://h:abc",   // non-numeric port
            "mqtts://h:99999", // out of u16 range
            "mqtts://h/path",  // a path
        ];
        for raw in bad {
            let mut c = DaemonConfig::default();
            c.mqtt.broker = Some(raw.to_string());
            assert!(c.mqtt_endpoint().is_err(), "{raw} should be rejected");
            // …and it must disable the MQTT actor, not fail later at connect
            // time. It must NOT abort the daemon — see
            // `a_broken_mqtt_section_never_blocks_daemon_startup`.
            assert!(c.mqtt_settings().is_err(), "{raw} should fail resolution");
        }
    }

    /// A misconfigured `[mqtt]` must NOT stop the daemon starting.
    ///
    /// The daemon owns the shell, CEC and the input fleet. `[mqtt]` is an
    /// optional subsystem, and letting its config abort startup would make MQTT
    /// *subtractive* — able to break features that have nothing to do with it.
    /// `validate()` therefore does not look at `[mqtt]` at all; the caller
    /// resolves it separately, logs loudly, and skips only the MQTT actor.
    ///
    /// Every case below is a genuine misconfiguration that `mqtt_settings()`
    /// rejects — the point is that `validate()` still says the daemon may start.
    #[test]
    fn a_broken_mqtt_section_never_blocks_daemon_startup() {
        let broken = [
            // Broker set, identity missing — cannot publish, must not abort.
            "[mqtt]\nbroker = \"mqtts://h:8883\"\n",
            // Identity present but invalid (topic wildcard).
            "[mqtt]\nbroker = \"mqtts://h\"\ndevice_id = \"a/b\"\n",
            // Unparseable broker URL.
            "[mqtt]\nbroker = \"http://h\"\ndevice_id = \"htpc-1\"\n",
            // Half-configured credentials.
            "[mqtt]\nbroker = \"mqtts://h\"\ndevice_id = \"htpc-1\"\nusername = \"u\"\n",
            // Zero intervals.
            "[mqtt]\nbroker = \"mqtts://h\"\ndevice_id = \"htpc-1\"\nheartbeat_secs = 0\n",
        ];
        for raw in broken {
            let cfg = DaemonConfig::parse(raw).expect("parses");
            assert!(
                cfg.mqtt_settings().is_err(),
                "expected a config error for {raw:?}"
            );
            assert!(
                cfg.validate().is_ok(),
                "a broken [mqtt] must not abort daemon startup: {raw:?}"
            );
        }
    }

    #[test]
    fn mqtt_broker_without_device_id_fails_closed() {
        // THE fail-closed guarantee: a broker with no explicit device_id must
        // abort startup rather than derive an id (which would split the
        // dual-boot desktop into two Home Assistant devices).
        let mut c = DaemonConfig::default();
        c.mqtt.broker = Some("mqtts://h".to_string());
        let err = c.mqtt_device_id().unwrap_err().to_string();
        assert!(err.contains("[mqtt].device_id is required"), "got: {err}");
        assert!(err.contains("dual-boot"), "got: {err}");
        assert!(c.mqtt_settings().is_err());
    }

    #[test]
    fn mqtt_invalid_device_id_is_rejected() {
        // A `/` would inject extra topic levels; the protocol newtype rejects it
        // and the config surfaces that as a startup error.
        for bad in ["a/b", "a+b", "a#b", ""] {
            let mut c = DaemonConfig::default();
            c.mqtt.broker = Some("mqtts://h".to_string());
            c.mqtt.device_id = Some(bad.to_string());
            let err = c.mqtt_device_id().unwrap_err().to_string();
            assert!(err.contains("[mqtt].device_id"), "for {bad:?}: {err}");
            assert!(c.mqtt_settings().is_err(), "for {bad:?}");
        }
    }

    #[test]
    fn mqtt_username_and_password_must_be_both_or_neither() {
        let base = || {
            let mut c = DaemonConfig::default();
            c.mqtt.broker = Some("mqtts://h".to_string());
            c.mqtt.device_id = Some("htpc-1".to_string());
            c
        };

        let mut user_only = base();
        user_only.mqtt.username = Some("tv-shell-htpc-1".to_string());
        let err = user_only.mqtt_settings().unwrap_err().to_string();
        assert!(err.contains("without [mqtt].password_file"), "got: {err}");

        let mut pass_only = base();
        pass_only.mqtt.password_file = Some("mqtt-password".to_string());
        let err = pass_only.mqtt_settings().unwrap_err().to_string();
        assert!(err.contains("without [mqtt].username"), "got: {err}");

        // Neither set is fine (anonymous broker).
        assert!(base().mqtt_settings().unwrap().is_some());
    }

    #[test]
    fn mqtt_zero_intervals_are_refused() {
        for (heartbeat, keepalive, needle) in
            [(0u64, 60u64, "heartbeat_secs"), (30, 0, "keepalive_secs")]
        {
            let mut c = DaemonConfig::default();
            c.mqtt.broker = Some("mqtts://h".to_string());
            c.mqtt.device_id = Some("htpc-1".to_string());
            c.mqtt.heartbeat_secs = heartbeat;
            c.mqtt.keepalive_secs = keepalive;
            let err = c.mqtt_settings().unwrap_err().to_string();
            assert!(err.contains(needle), "got: {err}");
        }
    }

    #[test]
    fn mqtt_off_ignores_every_other_key() {
        // No broker ⇒ no identity to resolve and nothing to validate, even with
        // a deliberately broken device_id / half-set credentials present.
        let c = DaemonConfig::parse(
            r#"
            [mqtt]
            device_id = "not/valid"
            username = "someone"
            heartbeat_secs = 0
            keepalive_secs = 0
        "#,
        )
        .unwrap();
        assert!(c.mqtt_device_id().unwrap().is_none());
        assert!(c.mqtt_endpoint().unwrap().is_none());
        c.validate().unwrap();
    }

    #[test]
    fn mqtt_ca_path_expands_tilde_and_is_not_confined() {
        // A CA certificate is public: expand `~`, but do NOT require 0600 and do
        // NOT confine it to the config dir (a system trust bundle is legitimate).
        let mut c = DaemonConfig::default();
        c.mqtt.ca_file = Some("/etc/ssl/certs/broker-ca.pem".to_string());
        assert_eq!(
            c.mqtt_ca_path().unwrap(),
            PathBuf::from("/etc/ssl/certs/broker-ca.pem")
        );
        if std::env::var_os("HOME").is_some() {
            c.mqtt.ca_file = Some("~/mqtt-ca.pem".to_string());
            let p = c.mqtt_ca_path().unwrap();
            assert!(p.is_absolute(), "{}", p.display());
            assert!(p.ends_with("mqtt-ca.pem"), "{}", p.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn mqtt_world_readable_password_file_disables_mqtt() {
        with_temp_config_dir(|gs| {
            let pw = write_token(gs, "mqtt-password", "hunter2\n", 0o644);
            let mut c = DaemonConfig::default();
            c.mqtt.broker = Some("mqtts://h".to_string());
            c.mqtt.device_id = Some("htpc-1".to_string());
            c.mqtt.username = Some("tv-shell-htpc-1".to_string());
            c.mqtt.password_file = Some(pw.to_string_lossy().into_owned());
            let err = c.mqtt_password().unwrap_err().to_string();
            assert!(err.contains("group/other-accessible"), "got: {err}");
            // Fail-closed in what it PUBLISHES: the actor does not start with a
            // leaked secret. But the daemon itself still starts.
            assert!(c.mqtt_settings().is_err());
            assert!(c.validate().is_ok());

            // The same file at 0600 resolves cleanly.
            let pw = write_token(gs, "mqtt-password", "hunter2\n", 0o600);
            c.mqtt.password_file = Some(pw.to_string_lossy().into_owned());
            assert_eq!(c.mqtt_password().unwrap().as_deref(), Some("hunter2"));
            assert!(c.mqtt_settings().unwrap().is_some());
            c.validate().unwrap();
        });
    }

    #[test]
    fn unknown_field_is_rejected() {
        // deny_unknown_fields catches typos (e.g. a stale daemon.env-era key).
        assert!(DaemonConfig::parse("[http]\nbnid = \"x\"\n").is_err());
        assert!(DaemonConfig::parse("[bogus]\nx = 1\n").is_err());
    }

    #[test]
    fn malformed_bind_is_an_error_not_silently_off() {
        let mut c = DaemonConfig::default();
        c.http.bind = Some("not-an-addr".to_string());
        assert!(c.http_bind().is_err());
    }

    #[test]
    fn validate_refuses_lan_http_without_auth() {
        // Non-loopback HTTP bridge + auth off + no opt-in ⇒ refuse.
        let c = loopback_http(false, "0.0.0.0:8089", false);
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("refusing to start"), "got: {err}");
        assert!(err.contains("HTTP control bridge"), "got: {err}");
    }

    #[test]
    fn validate_allows_lan_http_loopback() {
        // Loopback bind is always fine even with auth off.
        let c = loopback_http(false, "127.0.0.1:8089", false);
        c.validate().unwrap();
    }

    #[test]
    fn validate_escape_hatch_downgrades_to_warning() {
        // allow_insecure_lan = true ⇒ the same dangerous combo is permitted.
        let c = loopback_http(true, "0.0.0.0:8089", false);
        c.validate().unwrap();
    }

    #[test]
    fn validate_mcp_dev_lan_no_auth_refused() {
        let mut c = DaemonConfig::default();
        c.mcp.bind = Some("0.0.0.0:8090".to_string());
        c.mcp.dev = true;
        c.http.auth_enabled = false; // shared auth disabled
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("MCP server"), "got: {err}");
    }

    #[test]
    fn validate_mcp_lan_no_dev_no_auth_is_allowed() {
        // Without dev tools, an unauthenticated MCP bind is a remote-control leak
        // but not RCE; matches the prior mcp.rs refusal which gated on dev.
        let mut c = DaemonConfig::default();
        c.mcp.bind = Some("0.0.0.0:8090".to_string());
        c.mcp.dev = false;
        c.http.auth_enabled = false;
        c.validate().unwrap();
    }

    // Token-file tests mutate XDG_CONFIG_HOME (process-global, since config_dir()
    // reads it), so they serialize on this guard to stay parallel-safe.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with XDG_CONFIG_HOME pointed at a fresh temp dir whose
    /// `tv-shell/` subdir exists; cleans up after. Serialized via ENV_GUARD.
    #[cfg(unix)]
    fn with_temp_config_dir(f: impl FnOnce(&std::path::Path)) {
        let _g = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir.
        let base = crate::testutil::scratch_path("tv-cfgdir", "");
        let gs = base.join("tv-shell");
        std::fs::create_dir_all(&gs).unwrap();
        // `create_dir_all` freshly mints both `base` and `gs` — harden both.
        crate::testutil::harden_dir(&base);
        crate::testutil::harden_dir(&gs);
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: serialized by ENV_GUARD; restored before returning.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        f(&gs);
        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Write a token file at `<config-dir>/<name>` with mode `mode`.
    #[cfg(unix)]
    fn write_token(dir: &std::path::Path, name: &str, body: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn validate_lan_http_with_0600_token_ok() {
        // A resolvable 0600 token inside the config dir + auth on ⇒ not
        // "effectively disabled" ⇒ allowed.
        with_temp_config_dir(|gs| {
            let tok = write_token(gs, "http-token", "a-long-secret\n", 0o600);
            let mut c = DaemonConfig::default();
            c.http.bind = Some("0.0.0.0:8089".to_string());
            c.http.auth_enabled = true;
            c.http.token_file = Some(tok.to_string_lossy().into_owned());
            assert_eq!(c.http_token().unwrap().as_deref(), Some("a-long-secret"));
            c.validate().unwrap();
        });
    }

    #[cfg(unix)]
    #[test]
    fn token_file_world_readable_is_rejected() {
        // #4: a group/other-accessible token file fails closed (hard error).
        with_temp_config_dir(|gs| {
            let tok = write_token(gs, "http-token", "secret\n", 0o644);
            let mut c = DaemonConfig::default();
            c.http.token_file = Some(tok.to_string_lossy().into_owned());
            let err = c.http_token().unwrap_err().to_string();
            assert!(err.contains("group/other-accessible"), "got: {err}");
            // And validate() refuses to start because of it.
            c.http.bind = Some("0.0.0.0:8089".to_string());
            assert!(c.validate().is_err());
        });
    }

    #[cfg(unix)]
    #[test]
    fn token_file_outside_config_dir_is_rejected() {
        // #1 (CWE-22): a token path escaping the config dir is a hard error even
        // when the target exists and is 0600.
        with_temp_config_dir(|_gs| {
            // /etc/hostname exists on Linux CI; any readable file outside the
            // config dir works to prove the confinement check fires.
            let mut c = DaemonConfig::default();
            c.http.token_file = Some("/etc/hostname".to_string());
            let err = c.http_token().unwrap_err().to_string();
            assert!(
                err.contains("escapes the config directory") || err.contains("cannot resolve"),
                "got: {err}"
            );
        });
    }
}

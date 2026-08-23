//! Panel configuration: reads the daemon's `~/.config/tv-shell/config.toml`
//! for the `[panel]`, `[http]` and `[dev]` sections only.
//!
//! The panel cannot depend on the `tv-shell-input` daemon crate (it pulls a
//! Linux-only evdev/zbus/bluer/cec graph), so it parses the shared
//! `config.toml` itself with a PERMISSIVE deserializer: only the sections it
//! needs are declared, every other section (`[mcp]`, `[cec]`, `[plex]`,
//! `[steam]`, `[observability]`, `[input]`, ...) is silently ignored by
//! serde's default "unknown fields are OK" behavior (no `deny_unknown_fields`
//! anywhere in this module).
//!
//! ```toml
//! [panel]
//! enabled = true
//! bind = "127.0.0.1:8091"
//! token_file = "~/.config/tv-shell/panel-token"  # 0600; enables panel auth
//! allow_dangerous = false                        # deploy/build/reboot/pacman
//!
//! [http]
//! bind = "127.0.0.1:8089"
//! token_file = "~/.config/tv-shell/http-token"
//!
//! [dev]
//! allow_insecure_lan = false  # shared with the daemon — one flag to audit
//! ```
//!
//! Parsing never panics and never blocks boot: a missing file yields all
//! defaults and a malformed file logs a warning and falls back to defaults —
//! the panel must always come up so an operator can reach the Dev recovery
//! page even when config.toml is broken.
//!
//! **Resolution, however, can refuse to start** (mirroring the daemon's
//! `DaemonConfig::validate`): a `[panel].token_file` that escapes the config
//! dir, is group/other-accessible, or is unreadable aborts startup rather than
//! silently degrading to "no auth", and a non-loopback `[panel].bind` with
//! auth effectively disabled aborts unless `[dev].allow_insecure_lan = true`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default panel bind address (loopback-only; LAN exposure is the operator's
/// choice via `[panel].bind` in config.toml).
const DEFAULT_PANEL_BIND: &str = "127.0.0.1:8091";

/// `[panel]` section of `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    pub enabled: bool,
    pub bind: String,
    pub token_file: Option<String>,
    /// Gate for the root-equivalent action set (deploy, build, reboot,
    /// suspend, `pacman -Syu`, raw IPC). `false` by default: a fresh node
    /// gets the read-only + recovery surface until an operator opts in.
    pub allow_dangerous: bool,
    /// `[[panel.nodes]]` — remote sidecar nodes this panel may serve
    /// (`docs/MULTI_NODE_PANEL.md` §4). Absent on every node deployed today.
    ///
    /// # Why this is nested under `[panel]` and not a top-level `[[nodes]]`
    ///
    /// `config.toml` is **shared with the daemon**, and `DaemonConfig` is
    /// `#[serde(default, deny_unknown_fields)]` at the top level with a hard
    /// `Err` on a parse failure — so a top-level `[[nodes]]` table would abort
    /// **daemon** startup, taking the TV down over a key the daemon has no
    /// interest in. That is not theoretical: the daemon carries a
    /// parse-and-ignore `panel: PanelConfig` field for exactly this reason,
    /// and its own comment says `PanelConfig` deliberately omits
    /// `deny_unknown_fields` "so the panel can add its own keys later without
    /// forcing a matching daemon-struct change". This is that extension point,
    /// used as designed.
    ///
    /// `docs/MULTI_NODE_PANEL.md` §4 sketched a top-level `[nodes]`; nesting
    /// is the same idea correctly scoped, and it also reads truer — these are
    /// the *panel's* nodes, not the node's.
    ///
    /// Pinned from the daemon side by
    /// `daemon_config::tests::panel_nodes_parse_and_are_ignored_by_daemon`.
    pub nodes: Vec<RawRemoteNode>,
    /// `[panel].managed_units` — the units System ▸ Services may RESTART,
    /// beyond the three built-in tv-shell ones. Read is unrestricted; restart
    /// is not (`docs/PANEL_IA.md` § Services). Resolved and validated at load
    /// into [`AppConfig::managed_units`]; a malformed entry aborts startup
    /// rather than silently dropping a unit the operator believes is
    /// restartable.
    pub managed_units: Vec<RawManagedUnit>,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: DEFAULT_PANEL_BIND.to_string(),
            token_file: None,
            allow_dangerous: false,
            nodes: Vec::new(),
            managed_units: Vec::new(),
        }
    }
}

/// `[dev]` section of `config.toml` — shared with the daemon. The panel reads
/// only `allow_insecure_lan`, deliberately reusing the daemon's flag so there
/// is ONE insecure-LAN opt-in on a node to audit, not two.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DevSection {
    pub allow_insecure_lan: bool,
}

/// `[http]` section of `config.toml` (the daemon's opt-in LAN HTTP bridge).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HttpSection {
    pub bind: Option<String>,
    pub token_file: Option<String>,
}

// ── systemd units the Services page may read and restart ──────────────────

/// The three tv-shell unit keys that are **built in** rather than configured.
///
/// They stay hardcoded so a typo in `[panel].managed_units` can never cost the
/// recovery path (`docs/PANEL_IA.md` § Services). A `managed_units` entry
/// whose key collides with one of these is a load error, not a shadow — see
/// [`resolve_managed_units`].
pub const BUILT_IN_UNIT_KEYS: [&str; 3] = ["daemon", "shell", "panel"];

/// systemd's own unit-name length ceiling (`UNIT_NAME_MAX`, 256 including the
/// NUL). Anything longer is not a unit name, it is a payload.
const MAX_UNIT_NAME_LEN: usize = 255;

/// The unit-type suffixes a name may carry. A name with no `.` at all is
/// accepted too — `systemctl` appends `.service` itself — but a name ending in
/// an unknown suffix (`foo.sh`, `foo.`) is rejected: it is far more likely to
/// be a mistyped path than a unit.
const UNIT_SUFFIXES: [&str; 11] = [
    "service",
    "socket",
    "target",
    "timer",
    "mount",
    "automount",
    "path",
    "slice",
    "scope",
    "swap",
    "device",
];

/// Which systemd manager a unit lives under.
///
/// Load-bearing for privilege, not decoration: a `User` unit is restarted with
/// `systemctl --user` and needs no elevation (so it keeps working with the
/// daemon down, which is what makes Services a recovery surface); a `System`
/// unit goes through `sudo -n` and fails closed without a per-unit sudoers
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitScope {
    System,
    User,
}

impl UnitScope {
    /// Parse the `scope = "..."` field. Exactly two spellings are accepted —
    /// anything else is a load error rather than a silent default, because
    /// defaulting either way is wrong: guessing `user` breaks a working
    /// restart, guessing `system` quietly routes a user unit through `sudo`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "system" => Ok(UnitScope::System),
            "user" => Ok(UnitScope::User),
            other => Err(format!(
                "scope {other:?} is not valid; it must be exactly \"system\" or \"user\""
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            UnitScope::System => "system",
            UnitScope::User => "user",
        }
    }

    pub fn is_user(self) -> bool {
        matches!(self, UnitScope::User)
    }
}

impl std::fmt::Display for UnitScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A **syntactically validated** systemd unit name.
///
/// The only way to get one is [`UnitName::parse`], and every `systemctl`
/// invocation in [`crate::exec`] takes either string literals or one of these
/// — so an operator-typed name (System ▸ Services' "inspect any unit" form)
/// cannot reach `systemctl` unvalidated, on the read path or any other.
///
/// The accepted alphabet is deliberately narrower than systemd's own. Nothing
/// here is ever handed to a shell (`tokio::process::Command` takes an argv
/// array), so a metacharacter could not execute anything anyway; the alphabet
/// is narrow so the strings that reach an exec, a log line and an HTML
/// attribute cannot be *mistaken* for syntax by any of them. The two rules
/// that are load-bearing rather than tidy are the leading-`-` rejection —
/// `systemctl` would parse `--foo` as an option, the one real injection this
/// interface has — and the length cap.
///
/// Known narrowing: escaped names (`dev-disk-by\x2duuid-….device`) carry a
/// backslash and so are not addressable from the panel. Mount and device units
/// are not what this surface is for, and admitting `\` to buy them back is not
/// a trade worth making.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitName(String);

impl UnitName {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let t = raw.trim();
        if t.is_empty() {
            return Err("unit name must not be empty".to_string());
        }
        if t.len() > MAX_UNIT_NAME_LEN {
            return Err(format!(
                "unit name is {} bytes; systemd's own limit is {MAX_UNIT_NAME_LEN}",
                t.len()
            ));
        }
        if t.starts_with('-') {
            return Err(format!(
                "unit name {t:?} must not start with '-' — systemctl would read it as an option"
            ));
        }
        if t.starts_with('.') {
            return Err(format!("unit name {t:?} must not start with '.'"));
        }
        if t.contains("..") {
            return Err(format!("unit name {t:?} must not contain '..'"));
        }
        if let Some(bad) = t
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | ':')))
        {
            return Err(format!(
                "unit name {t:?} contains {bad:?}; only ASCII letters, digits and - _ . @ : \
                 are accepted"
            ));
        }
        if let Some((_, suffix)) = t.rsplit_once('.') {
            if !UNIT_SUFFIXES.contains(&suffix) {
                return Err(format!(
                    "unit name {t:?} ends in .{suffix}, which is not a systemd unit type ({})",
                    UNIT_SUFFIXES.join(", ")
                ));
            }
        }
        Ok(UnitName(t.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UnitName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A unit the panel is permitted to **restart**, with the scope it must be
/// restarted in.
///
/// The fields are private and this module exposes exactly three constructors —
/// [`builtin_target`], [`resolve_managed_units`] and
/// [`AppConfig::restart_target`] — every one of which resolves a *key* against
/// a server-side table. There is no `From<&str>`, no public field and no
/// `Deserialize`, so a client-supplied unit name has no path into this type,
/// and [`crate::exec::Recovery::restart`] accepts nothing else. That is
/// `docs/PANEL_IA.md`'s no-arbitrary-unit property, held by the type system
/// rather than by a `match` a later edit could widen.
#[derive(Debug, Clone)]
pub struct RestartTarget {
    key: String,
    unit: UnitName,
    scope: UnitScope,
}

impl RestartTarget {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn unit(&self) -> &UnitName {
        &self.unit
    }

    pub fn scope(&self) -> UnitScope {
        self.scope
    }

    /// Whether losing this unit can end the operator's remaining path to the
    /// box — what earns the extra sentence in the restart confirm.
    ///
    /// Derived from a small explicit set rather than from the unit's runtime
    /// properties, because systemd exposes nothing that honestly answers "is
    /// this how I am connected right now": `WantedBy=network.target` is true
    /// of plenty of units whose failure costs nothing. The membership
    /// criterion is narrow and checkable by eye: **system-scope units that
    /// either serve the remote login session or own the network link it runs
    /// over.** A user-scope unit can never qualify.
    pub fn is_remote_access_critical(&self) -> bool {
        if self.scope.is_user() {
            return false;
        }
        let name = self.unit.as_str();
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        REMOTE_ACCESS_UNITS.contains(&stem.to_ascii_lowercase().as_str())
    }
}

/// See [`RestartTarget::is_remote_access_critical`]. Lowercased unit stems
/// (suffix removed), so `NetworkManager.service` and `NetworkManager.socket`
/// both match.
const REMOTE_ACCESS_UNITS: [&str; 10] = [
    // the remote login server itself, under each distro's spelling
    "sshd",
    "ssh",
    "dropbear",
    // whatever owns the link that login arrives over
    "networkmanager",
    "systemd-networkd",
    "networking",
    "iwd",
    "wpa_supplicant",
    "dhcpcd",
    "tailscaled",
];

/// One `[panel].managed_units` entry, as written in `config.toml`.
///
/// ```toml
/// [panel]
/// managed_units = [
///   { key = "sshd", unit = "sshd.service", scope = "system" },
/// ]
/// ```
///
/// Nested under `[panel]` for the same reason [`PanelConfig::nodes`] is: the
/// daemon shares this file and would reject an unknown top-level table.
#[derive(Debug, Clone, Deserialize)]
pub struct RawManagedUnit {
    /// The token the client sends to `POST /system/services/restart/{key}`.
    /// An **index into this table**, never a unit name.
    pub key: String,
    /// The real unit name, resolved server-side.
    pub unit: String,
    /// `"system"` or `"user"` — see [`UnitScope`].
    pub scope: String,
}

/// Resolve `[panel].managed_units` into restart targets, or abort startup.
///
/// Every failure here is loud and specific on purpose: this list is a
/// privilege boundary, and the failure mode of a quietly-dropped entry is an
/// operator who believes a unit is restartable from the panel and finds out
/// otherwise while the box is already broken.
pub fn resolve_managed_units(raw: &[RawManagedUnit]) -> anyhow::Result<Vec<RestartTarget>> {
    let mut out: Vec<RestartTarget> = Vec::with_capacity(raw.len());
    for (i, entry) in raw.iter().enumerate() {
        let key = entry.key.trim();
        if key.is_empty() {
            return Err(anyhow::anyhow!(
                "config: [panel].managed_units[{i}] has an empty `key`; refusing to start — \
                 the key is what the restart route resolves against, so an unnamed entry \
                 could never be reached"
            ));
        }
        if BUILT_IN_UNIT_KEYS.contains(&key) {
            return Err(anyhow::anyhow!(
                "config: [panel].managed_units[{i}] uses key {key:?}, which is one of the \
                 built-in tv-shell unit keys ({}); refusing to start — rename it. The \
                 built-ins are hardcoded precisely so a config entry can never take the \
                 recovery path away",
                BUILT_IN_UNIT_KEYS.join(", ")
            ));
        }
        if let Some(bad) = key
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
        {
            return Err(anyhow::anyhow!(
                "config: [panel].managed_units[{i}] key {key:?} contains {bad:?}; a key is a \
                 URL path segment, so only ASCII letters, digits, '-' and '_' are accepted"
            ));
        }
        if out.iter().any(|t| t.key == key) {
            return Err(anyhow::anyhow!(
                "config: [panel].managed_units has two entries with key {key:?}; refusing to \
                 start — whichever one lost would be silently unreachable"
            ));
        }
        let unit = UnitName::parse(&entry.unit).map_err(|e| {
            anyhow::anyhow!("config: [panel].managed_units[{i}] ({key:?}) has a bad `unit`: {e}")
        })?;
        let scope = UnitScope::parse(&entry.scope).map_err(|e| {
            anyhow::anyhow!("config: [panel].managed_units[{i}] ({key:?}) has a bad `scope`: {e}")
        })?;
        out.push(RestartTarget {
            key: key.to_string(),
            unit,
            scope,
        });
    }
    Ok(out)
}

/// Resolve one of the three [`BUILT_IN_UNIT_KEYS`] to its restart target.
///
/// Hardcoded, not configurable: `docs/PANEL_IA.md` § Services keeps the
/// tv-shell units built in so a config typo cannot cost the recovery path.
/// All three are `--user` units, so none of them needs `sudo`.
pub fn builtin_target(key: &str) -> Option<RestartTarget> {
    let unit = match key {
        "daemon" => daemon_unit(),
        "shell" => shell_unit(),
        "panel" => panel_unit(),
        _ => return None,
    };
    Some(RestartTarget {
        key: key.to_string(),
        unit,
        scope: UnitScope::User,
    })
}

/// One `[[panel.nodes]]` entry, as written in `config.toml`.
///
/// ```toml
/// [[panel.nodes]]
/// id = "desktop-2"
/// base_url = "http://192.168.8.153:47995"
/// sidecar_token_file = "~/.config/tv-shell/desktop-2-sidecar-token"
/// ```
///
/// Nested under `[panel]` because `config.toml` is shared with the daemon —
/// see [`PanelConfig::nodes`].
///
/// **`sidecar_token_file`, not `token_file`.** `docs/MULTI_NODE_PANEL.md` §4
/// draws a hard line the field name has to carry: a panel may hold credentials
/// **only for sidecar nodes it serves**, and never another *shell* node's token
/// — those panels are peers reachable by `<a href>`, and a link carries no
/// credential. Two fields both called `token_file` would make the rule a
/// convention; naming this one for what it may only ever be makes a
/// shell-node token an obviously wrong thing to put here.
#[derive(Debug, Clone, Deserialize)]
pub struct RawRemoteNode {
    /// Stable identifier for this node, used in logs and (later) the node
    /// switcher. Not required to match the node's self-declared
    /// `Capabilities.node_id` — that is the node's statement about itself and
    /// is read from the handshake, never from here.
    pub id: String,
    /// Base URL of the sidecar's HTTP listener, e.g.
    /// `"http://192.168.8.153:47995"`.
    pub base_url: String,
    /// Path to a 0600 file under the config dir holding the sidecar's bearer
    /// token. Required: every sidecar route but `/art/{appid}` is bearer-auth'd
    /// (`host/src/main.rs`), so a node entry with no credential could do
    /// nothing but 401.
    pub sidecar_token_file: String,
}

/// A resolved remote sidecar node: its id, its base URL, and its bearer token
/// read eagerly at startup.
#[derive(Clone)]
pub struct RemoteNode {
    pub id: String,
    pub base_url: String,
    /// The sidecar's bearer token. Redacted by this type's [`Debug`] — see
    /// below.
    ///
    /// Read only by [`HttpTransport::for_node`](crate::http::HttpTransport::for_node),
    /// which is itself waiting on the node switcher — hence the same
    /// milestone-scoped `#[allow(dead_code)]` as
    /// [`AppConfig::remote_nodes`].
    #[allow(dead_code)]
    pub token: String,
}

/// Hand-written so a `{:?}` of the config — a startup log line, a panic
/// message, an `anyhow` chain — cannot print a node's bearer token.
///
/// The panel's own token and the daemon bridge token sit in a `derive(Debug)`
/// struct today and survive only because nothing formats them. That is a
/// property of the call sites, not of the type, and it does not scale to one
/// credential per served node.
impl std::fmt::Debug for RemoteNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteNode")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Top-level shape captured from `config.toml`. Deliberately does NOT declare
/// the daemon's other sections (`mcp`, `cec`, `plex`, `steam`, `observability`,
/// `input`) — serde ignores unknown top-level keys by default (no
/// `deny_unknown_fields`), so this struct tolerates the full daemon config
/// document unchanged.
#[derive(Debug, Clone, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    panel: PanelConfig,
    #[serde(default)]
    http: HttpSection,
    #[serde(default)]
    dev: DevSection,
}

/// Resolved, ready-to-use panel configuration.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Whether the panel should serve at all (`[panel].enabled`).
    pub enabled: bool,
    /// Resolved bind address for the panel's own HTTP listener.
    pub panel_bind: SocketAddr,
    /// Raw `[panel].bind` string, kept for diagnostics/logging.
    pub panel_bind_raw: String,
    /// `[panel].token_file` as configured. Its presence is what turns the
    /// panel's own auth ON — see [`AppConfig::auth_enabled`].
    pub panel_token_file: Option<String>,
    /// The panel's own credential, resolved EAGERLY at startup from
    /// `[panel].token_file` (config-dir-confined, 0600-checked, trimmed).
    /// `None` means "no token" — with `auth_enabled()` that is the
    /// fail-closed combination the auth layer rejects everything on.
    pub panel_token: Option<String>,
    /// `[panel].allow_dangerous` — gates registration of the root-equivalent
    /// routes (deploy/build/reboot/suspend/pacman/raw IPC).
    pub allow_dangerous: bool,
    /// `[dev].allow_insecure_lan` — the daemon's own escape hatch, reused
    /// here so a node has one insecure-LAN opt-in rather than two.
    pub allow_insecure_lan: bool,
    /// `Some("http://<http.bind>")` when the daemon's HTTP bridge is
    /// configured; `None` when `[http].bind` is absent (bridge off).
    pub http_bridge_base: Option<String>,
    /// The daemon HTTP bridge's bearer token, read from `[http].token_file`
    /// (tilde-expanded, trimmed). `None` on any error (missing/unreadable
    /// file, no `token_file` configured).
    pub http_token: Option<String>,
    /// Remote **sidecar** nodes from `[[panel.nodes]]`, each with its bearer token
    /// resolved eagerly under the same rules as the panel's own
    /// (`config`-dir-confined, 0600, non-empty) — see [`read_remote_nodes`].
    ///
    /// Empty on every node deployed today, and **nothing reads it yet**: this
    /// PR lands `HttpTransport` and the config that will point one at a node;
    /// the node switcher that actually serves a second node is the next
    /// milestone (`docs/MULTI_NODE_PANEL.md` §4, sequencing step 6). Same
    /// `#[allow(dead_code)]` treatment (and same reason) as
    /// [`shell_journal_tag`] and [`crate::transport::Reachability`]: a landed
    /// surface whose consumer is a later milestone, kept honest by tests
    /// rather than deleted and re-derived. The alternative — landing the
    /// config in the same PR as the switcher — is what makes a switcher PR
    /// carry an unreviewed credential-handling change.
    #[allow(dead_code)]
    pub remote_nodes: Vec<RemoteNode>,
    /// `[panel].managed_units`, resolved and validated (`resolve_managed_units`).
    /// The server-side table System ▸ Services' restart route indexes into —
    /// the client only ever sends a key. Empty by default: on a node with no
    /// `managed_units`, only the three built-in tv-shell units are restartable.
    pub managed_units: Vec<RestartTarget>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let panel = PanelConfig::default();
        let panel_bind = panel
            .bind
            .parse()
            .unwrap_or_else(|_| DEFAULT_PANEL_BIND.parse().expect("default bind is valid"));
        Self {
            enabled: panel.enabled,
            panel_bind,
            panel_bind_raw: panel.bind,
            panel_token_file: panel.token_file,
            panel_token: None,
            allow_dangerous: panel.allow_dangerous,
            allow_insecure_lan: false,
            http_bridge_base: None,
            http_token: None,
            remote_nodes: Vec::new(),
            managed_units: Vec::new(),
        }
    }
}

impl AppConfig {
    /// Whether the panel enforces its own authentication. Keyed on
    /// `[panel].token_file` being CONFIGURED, not on a token having been
    /// resolved: "auth on but no token" must reject everything (fail closed)
    /// rather than read as "auth off".
    pub fn auth_enabled(&self) -> bool {
        self.panel_token_file.is_some()
    }

    /// Resolve a restart `key` to the unit it names — the ONE lookup
    /// `POST /system/services/restart/{key}` performs.
    ///
    /// Built-ins are consulted first, so even if the collision check in
    /// [`resolve_managed_units`] were ever removed, config could not shadow
    /// the recovery path. `None` means "not in the table" and the caller must
    /// refuse before touching `systemctl`: an unknown key is not a unit name,
    /// it is a client asking for one.
    pub fn restart_target(&self, key: &str) -> Option<RestartTarget> {
        builtin_target(key).or_else(|| self.managed_units.iter().find(|t| t.key == key).cloned())
    }

    /// Every restartable unit, built-ins first, in the order System ▸ Services
    /// renders them.
    pub fn restart_targets(&self) -> Vec<RestartTarget> {
        BUILT_IN_UNIT_KEYS
            .iter()
            .filter_map(|k| builtin_target(k))
            .chain(self.managed_units.iter().cloned())
            .collect()
    }

    /// Refuse to start in a configuration that would expose the panel — the
    /// most privileged surface on the node — unauthenticated on the LAN.
    ///
    /// Mirrors `DaemonConfig::validate` (`daemon/src/daemon_config.rs`): the
    /// dangerous combination is a **non-loopback** bind + auth **effectively
    /// disabled** (no `token_file`, or no token resolvable from it).
    /// Returning `Err` aborts startup, BEFORE the listener binds.
    fn validate(&self) -> anyhow::Result<()> {
        let auth_effectively_disabled = !self.auth_enabled() || self.panel_token.is_none();
        if !self.panel_bind.ip().is_loopback() && auth_effectively_disabled {
            self.refuse_or_warn(
                "web control panel",
                self.panel_bind,
                "every panel route can restart units, rewrite config.toml, upload files \
                 and (with allow_dangerous) deploy, reboot or run a full system update",
            )?;
        }
        Ok(())
    }

    /// Either return an error (refuse to start) or, when the operator has
    /// opted into `[dev].allow_insecure_lan`, log a loud warning and continue.
    /// Mirrors `DaemonConfig::refuse_or_warn`, including the deliberate
    /// `error!`-not-`warn!` level on the permitted path.
    fn refuse_or_warn(&self, surface: &str, addr: SocketAddr, why: &str) -> anyhow::Result<()> {
        if self.allow_insecure_lan {
            // error!, not warn!: the escape hatch is a deliberate hole, and a
            // forgotten `allow_insecure_lan = true` (e.g. a copy-pasted dev
            // config) silently opens an unauthenticated root-equivalent
            // surface to the LAN. Logging at error level makes that impossible
            // to miss at startup.
            tracing::error!(
                "config: {surface} bound to non-loopback {addr} with auth effectively \
                 disabled — {why}. PERMITTED ONLY because [dev].allow_insecure_lan = true; \
                 remove it unless this box intentionally runs an unauthenticated LAN panel."
            );
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "refusing to start: {surface} is bound to non-loopback {addr} with auth \
                 effectively disabled (no token) — {why}. Set [panel].token_file (0600, \
                 under ~/.config/tv-shell/), bind to 127.0.0.1, or explicitly opt in with \
                 [dev].allow_insecure_lan = true."
            ))
        }
    }
}

/// Path to the daemon's `config.toml` (`tv_shell_protocol::brand::config_dir()`).
fn config_path() -> PathBuf {
    tv_shell_protocol::brand::config_dir().join("config.toml")
}

/// Public counterpart of [`config_path`] for pages that need to read (but
/// never write) `config.toml` directly — e.g. Shell ▸ Advanced's read-only
/// config.toml viewer. Kept as a separate function (rather than making
/// `config_path` `pub`) so it's obvious at a glance that this is a
/// deliberately-exposed read path, not the loader's internals.
pub fn config_toml_path() -> PathBuf {
    tv_shell_protocol::brand::config_dir().join("config.toml")
}

/// Load and resolve the panel configuration.
///
/// PARSING never panics: a missing file yields all defaults; a malformed file
/// logs a warning and falls back to defaults so the panel can still boot (and
/// an operator can reach the Dev recovery page) even with a broken config.toml.
///
/// RESOLUTION can fail: an unusable `[panel].token_file` or a non-loopback
/// bind with auth effectively disabled returns `Err` so `main` aborts before
/// binding the listener.
pub fn load() -> anyhow::Result<AppConfig> {
    let path = config_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<RawConfig>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    "panel: failed to parse {} — falling back to defaults: {e}",
                    path.display()
                );
                RawConfig::default()
            }
        },
        Err(_) => {
            // Missing file (or unreadable) ⇒ all defaults. Not worth a
            // warning: an absent config.toml is a normal fresh install.
            RawConfig::default()
        }
    };
    resolve(raw)
}

/// Resolve a parsed [`RawConfig`] into a ready-to-use [`AppConfig`], resolving
/// the panel's own token eagerly and validating the bind/auth combination.
///
/// Both the token resolve and the validation are skipped when
/// `[panel].enabled = false` — a disabled panel binds no listener, so there is
/// no surface to refuse (this mirrors the daemon, which only validates a
/// surface whose bind is actually configured).
fn resolve(raw: RawConfig) -> anyhow::Result<AppConfig> {
    let panel_bind = raw.panel.bind.parse().unwrap_or_else(|e| {
        tracing::warn!(
            "panel: invalid [panel].bind {:?} ({e}) — falling back to {DEFAULT_PANEL_BIND}",
            raw.panel.bind
        );
        DEFAULT_PANEL_BIND.parse().expect("default bind is valid")
    });
    let http_bridge_base = raw
        .http
        .bind
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|bind| format!("http://{bind}"));
    let http_token = raw.http.token_file.as_deref().and_then(read_token_file);

    let panel_token = match (raw.panel.enabled, raw.panel.token_file.as_deref()) {
        (true, Some(p)) => Some(read_panel_token(
            p,
            &tv_shell_protocol::brand::config_dir(),
        )?),
        _ => None,
    };

    // Resolved eagerly, and a failure ABORTS startup — same posture as the
    // panel's own token and for the same reason: a node entry whose token is
    // missing, world-readable or outside the config dir must not degrade into
    // a node the panel silently cannot talk to. It would 401 on every request
    // and the operator would go looking at the sidecar.
    let remote_nodes =
        read_remote_nodes(&raw.panel.nodes, &tv_shell_protocol::brand::config_dir())?;

    // Same posture, and the same reason: a `managed_units` entry that is
    // malformed, duplicated or shadowing a built-in ABORTS startup rather than
    // being dropped. The list is a privilege boundary; silently serving a
    // shorter one than the operator wrote is the failure mode that gets found
    // out mid-incident.
    let managed_units = resolve_managed_units(&raw.panel.managed_units)?;

    let cfg = AppConfig {
        enabled: raw.panel.enabled,
        panel_bind,
        panel_bind_raw: raw.panel.bind,
        panel_token_file: raw.panel.token_file,
        panel_token,
        allow_dangerous: raw.panel.allow_dangerous,
        allow_insecure_lan: raw.dev.allow_insecure_lan,
        http_bridge_base,
        http_token,
        remote_nodes,
        managed_units,
    };
    if cfg.enabled {
        cfg.validate()?;
    }
    Ok(cfg)
}

/// Resolve every `[[panel.nodes]]` entry into a [`RemoteNode`], or abort startup.
///
/// Each entry's token goes through the **same** [`resolve_token_path`] +
/// [`ensure_owner_only`] checks the panel's own credential does — config-dir
/// confinement (CWE-22), no group/other access, non-empty. Deliberately not a
/// looser check: `docs/MULTI_NODE_PANEL.md` §4 accepts that a panel serving a
/// sidecar holds that sidecar's token, on the condition that the blast radius
/// stays bounded, and "read a secret from wherever the config points" is how
/// that bound is lost.
///
/// The one rule NOT inherited is cookie-safety — see [`read_sidecar_token`].
fn read_remote_nodes(raw: &[RawRemoteNode], config_dir: &Path) -> anyhow::Result<Vec<RemoteNode>> {
    let mut out: Vec<RemoteNode> = Vec::with_capacity(raw.len());
    for entry in raw {
        let id = entry.id.trim();
        if id.is_empty() {
            return Err(anyhow::anyhow!(
                "config: a [[panel.nodes]] entry has an empty `id`; refusing to start — the \
                 id names the node in logs and in the node switcher, and an unnamed \
                 node cannot be told from another one"
            ));
        }
        if out.iter().any(|n| n.id == id) {
            return Err(anyhow::anyhow!(
                "config: two [[panel.nodes]] entries share id {id:?}; refusing to start — \
                 whichever one lost would be silently unreachable"
            ));
        }
        let base_url = validate_base_url(entry.base_url.trim(), id)?;
        let field = format!("nodes.{id}.sidecar_token_file");
        let resolved = resolve_token_path(&entry.sidecar_token_file, config_dir, &field)?;
        let token = read_sidecar_token(&resolved, &field)?;
        out.push(RemoteNode {
            id: id.to_string(),
            base_url,
            token,
        });
    }
    Ok(out)
}

/// Read a `[[panel.nodes]]` entry's bearer token: owner-only, non-empty, trimmed.
///
/// Separate from [`read_owner_only_token`] rather than sharing it, because the
/// two differ on one rule and on their advice:
///
/// - **No cookie-octet check.** `ensure_cookie_safe_token` exists because the
///   panel's own token is interpolated into `Set-Cookie` and parsed back out of
///   `Cookie:`, so a `;` in it silently truncates and breaks browser login
///   forever. A sidecar token only ever leaves as
///   `Authorization: Bearer <token>`, which has no such round trip. Inheriting
///   the rule would refuse valid bearer tokens for a reason that does not apply
///   here — a rule kept past its justification.
/// - **Different advice on empty.** An empty panel token bricks the recovery
///   panel; an empty sidecar token means one remote node 401s. Telling the
///   operator to `openssl rand -hex 32` into the panel's token file when the
///   problem is a node's file would send them to the wrong place.
fn read_sidecar_token(path: &Path, field: &str) -> anyhow::Result<String> {
    ensure_owner_only(path, field)?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("config: {field} {} unreadable: {e}", path.display()))?;
    let token = raw.trim();
    if token.is_empty() {
        return Err(anyhow::anyhow!(
            "config: {field} {} is empty; refusing to start — every route on a \
             tv-shell-host sidecar except /art/{{appid}} requires a bearer token, so \
             this node could do nothing but 401. Write the sidecar's \
             TV_SHELL_HOST_TOKEN into that file, or remove the [[panel.nodes]] entry.",
            path.display()
        ));
    }
    Ok(token.to_string())
}

/// Accept only an absolute `http`/`https` URL with a non-empty authority.
///
/// No `url` crate is in the graph and pulling one in for this would be
/// disproportionate, so the check is deliberately narrow rather than a full
/// parse: it rejects the mistakes that actually happen in a hand-edited
/// config — a bare `host:port` with no scheme, a `file://` or `unix://`
/// path, an empty string, embedded whitespace — and lets `reqwest` reject
/// anything subtler at request time.
///
/// Rejecting at startup matters because the alternative is a node that
/// appears configured and fails every request with a URL parse error the
/// operator sees only in a per-request log line.
fn validate_base_url(url: &str, id: &str) -> anyhow::Result<String> {
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "config: [[panel.nodes]] {id:?} has base_url {url:?}, which is not an \
                 http:// or https:// URL; refusing to start — write the sidecar's \
                 full listener URL, e.g. \"http://192.168.8.153:47995\""
            )
        })?;
    let authority = authority.trim_end_matches('/');
    if authority.is_empty() || authority.chars().any(char::is_whitespace) {
        return Err(anyhow::anyhow!(
            "config: [[panel.nodes]] {id:?} has base_url {url:?} with no usable host; \
             refusing to start"
        ));
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Resolve `[panel].token_file` into the panel's own credential.
///
/// Mirrors the daemon's `resolve_token_path` + `read_token_file` pair: the
/// path is confined to `config_dir` (CWE-22), the file must not be
/// group/other-accessible, and its contents must be a non-empty
/// cookie-value-safe token. Any violation is an `Err` that aborts startup —
/// a token file the operator meant to enable auth with must never degrade
/// silently into "no auth" or into "auth nobody can pass".
fn read_panel_token(path: &str, config_dir: &Path) -> anyhow::Result<String> {
    let resolved = resolve_token_path(path, config_dir, "panel.token_file")?;
    read_owner_only_token(&resolved, "panel.token_file")
}

/// Tilde-expand, canonicalize, and require the result to live under
/// `config_dir` — a config writer must not be able to point the panel at
/// `/etc/shadow` or an attacker-writable `/tmp` path. Canonicalizing also
/// resolves `..` and symlinks, so a symlink inside the config dir pointing
/// out is caught too.
fn resolve_token_path(p: &str, config_dir: &Path, field: &str) -> anyhow::Result<PathBuf> {
    let expanded = expand_tilde(p);
    let canonical = expanded.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "config: {field} {}: cannot resolve token file path: {e}",
            expanded.display()
        )
    })?;
    let config_dir = config_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!("config: cannot resolve config dir for {field} validation: {e}")
    })?;
    if !canonical.starts_with(&config_dir) {
        return Err(anyhow::anyhow!(
            "config: {field} {} escapes the config directory {} — a token file must live \
             under it (refusing to read a secret from an arbitrary path)",
            canonical.display(),
            config_dir.display()
        ));
    }
    Ok(canonical)
}

/// Read a 0600-style token file: trim, then hard-error on every way the file
/// can be unusable — group/other-accessible, unreadable, empty, or holding a
/// value that cannot survive a round trip through the session cookie.
///
/// **Empty is an error, not `Ok(None)`.** A configured-but-empty token file
/// used to start the panel with `auth_enabled() == true` and no token: every
/// route 401s and `/login` rejects every submission, so the operator's
/// browser-based recovery path for a wedged daemon is gone — with only a
/// `warn!` line as the signal. That is exactly the silent degradation the rest
/// of this module refuses.
fn read_owner_only_token(path: &Path, field: &str) -> anyhow::Result<String> {
    ensure_owner_only(path, field)?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("config: {field} {} unreadable: {e}", path.display()))?;
    let token = raw.trim();
    if token.is_empty() {
        return Err(anyhow::anyhow!(
            "config: {field} {} is empty; refusing to start — with a token file \
             configured the panel authenticates every route, so an empty token would \
             reject every request AND every /login submission, locking the recovery \
             panel out. Fix: write a token (openssl rand -hex 32 > {}) or remove \
             [panel].token_file to run unauthenticated on a loopback bind.",
            path.display(),
            path.display()
        ));
    }
    ensure_cookie_safe_token(token, field, path)?;
    Ok(token.to_string())
}

/// Refuse a token that cannot round-trip through the session cookie.
///
/// `auth::session_cookie` interpolates the token into `Set-Cookie` and
/// `auth::presented_token` parses it back by splitting the `Cookie:` header on
/// `;` and trimming — so a token containing `;` (or a leading/trailing space,
/// or any other non-`cookie-octet`) comes back as a PREFIX and the constant-time
/// compare then fails forever. Browser login would be permanently broken with
/// no diagnostic while `Authorization: Bearer` kept working. Refuse at startup
/// instead, naming the offending character.
fn ensure_cookie_safe_token(token: &str, field: &str, path: &Path) -> anyhow::Result<()> {
    if let Some(bad) = token.chars().find(|c| !is_cookie_octet(*c)) {
        return Err(anyhow::anyhow!(
            "config: {field} {} contains {bad:?}, which is not valid in a cookie value; \
             refusing to start — the panel's session cookie carries the token verbatim, \
             so such a token silently breaks browser login forever. Use a token of \
             printable ASCII without space, {:?}, {:?}, {:?} or {:?} — e.g. \
             openssl rand -hex 32.",
            path.display(),
            '"',
            ',',
            ';',
            '\\'
        ));
    }
    Ok(())
}

/// RFC 6265 `cookie-octet`: US-ASCII printable characters excluding space,
/// double quote, comma, semicolon and backslash.
fn is_cookie_octet(c: char) -> bool {
    matches!(c, '\x21' | '\x23'..='\x2B' | '\x2D'..='\x3A' | '\x3C'..='\x5B' | '\x5D'..='\x7E')
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
             start — the panel's credential must be private. Fix: chmod 600 {}",
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

/// Read a bearer token from a file path, tilde-expanding a leading `~/`.
/// Returns `None` on any error (missing file, unreadable, ...) or when the
/// trimmed content is empty.
fn read_token_file(path: &str) -> Option<String> {
    let expanded = expand_tilde(path);
    let content = std::fs::read_to_string(expanded).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Expand a leading `~/` to `$HOME/`. Paths without a leading `~/` pass
/// through unchanged.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Resolve the daemon's IPC Unix-socket path.
///
/// Preference order: `TV_SHELL_SOCK` (via [`tv_shell_protocol::brand::env`],
/// legacy `GAME_SHELL_SOCK` honored) → `$XDG_RUNTIME_DIR/<socket_name>` →
/// `/run/user/<uid>/<socket_name>` (uid from `libc::getuid()`).
pub fn socket_path() -> PathBuf {
    let name = tv_shell_protocol::brand::socket_name();
    if let Some(sock) = tv_shell_protocol::brand::env("SOCK") {
        return PathBuf::from(sock);
    }
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return PathBuf::from(runtime_dir).join(name);
        }
    }
    // SAFETY: libc::getuid() is always safe to call — POSIX defines it as
    // infallible (no error return, no invalid states), it takes no arguments,
    // and it only reads the caller's real UID.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{uid}/{name}"))
}

/// Build a [`UnitName`] from a brand-derived unit string.
///
/// These are compile-time-shaped (`<SLUG>-input.service` and friends, `SLUG`
/// being a `&'static str` const) so they cannot fail [`UnitName::parse`];
/// `built_in_unit_names_are_valid_unit_names` pins that rather than trusting
/// the reasoning.
fn brand_unit(suffix: &str) -> UnitName {
    let raw = format!("{}-{suffix}", tv_shell_protocol::brand::SLUG);
    UnitName::parse(&raw).expect("brand-derived unit names are valid unit names")
}

/// systemd unit name for the input daemon (`tv-shell-input.service`).
pub fn daemon_unit() -> UnitName {
    brand_unit("input.service")
}

/// systemd unit name for the Quickshell shell (`tv-shell-quickshell.service`).
pub fn shell_unit() -> UnitName {
    brand_unit("quickshell.service")
}

/// systemd unit name for the panel itself (`tv-shell-panel.service`).
pub fn panel_unit() -> UnitName {
    brand_unit("panel.service")
}

/// `journalctl --user -u <unit>` target for the input daemon
/// (`tv-shell-input`, no `.service` suffix — matches unit-name-as-journal-tag
/// convention).
pub fn daemon_journal_unit() -> String {
    format!("{}-input", tv_shell_protocol::brand::SLUG)
}

/// `journalctl --user -t <tag>` target for the Quickshell shell — the
/// `SyslogIdentifier` the quickshell unit sets (`tv-shell-quickshell`).
///
/// Not yet wired into the M1 Logs page (which sources the shell log via the
/// HTTP bridge only, per spec, and degrades to an inline message rather than
/// falling back to the journal when the bridge is down). Reserved for a
/// future milestone (e.g. a direct-exec shell-log fallback).
#[allow(dead_code)]
pub fn shell_journal_tag() -> String {
    format!("{}-quickshell", tv_shell_protocol::brand::SLUG)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that mutate process-global environment.
    ///
    /// `set_var` is not scoped to the calling test — the whole test binary is
    /// one process, and cargo runs tests on several threads. Without this,
    /// `expand_tilde_prefixes_home` can move `HOME` out from under any
    /// concurrently-running test that resolves a config path, and
    /// `socket_path_prefers_env_override` can do the same to `TV_SHELL_SOCK`.
    /// Nothing has flaked yet; this is the shape that flakes under load, and
    /// the failure would look like an unrelated path assertion breaking at
    /// random.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`ENV_LOCK`], surviving a poisoned mutex.
    ///
    /// A panicking env test poisons the lock; the guard is only ordering, so
    /// inheriting the poison would cascade one real failure into every other
    /// env test and bury the original.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn default_panel_config_is_enabled_on_loopback() {
        let cfg = AppConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.panel_bind_raw, DEFAULT_PANEL_BIND);
        assert_eq!(cfg.panel_bind, DEFAULT_PANEL_BIND.parse().unwrap());
        assert!(cfg.http_bridge_base.is_none());
        assert!(cfg.http_token.is_none());
    }

    #[test]
    fn default_config_has_auth_off_and_dangerous_actions_off() {
        let cfg = AppConfig::default();
        assert!(!cfg.auth_enabled(), "no [panel].token_file ⇒ auth off");
        assert!(cfg.panel_token.is_none());
        assert!(
            !cfg.allow_dangerous,
            "[panel].allow_dangerous must default to false (S5)"
        );
        assert!(!cfg.allow_insecure_lan);
    }

    #[test]
    fn resolve_missing_sections_yields_defaults() {
        let raw = RawConfig::default();
        let cfg = resolve(raw).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.panel_bind_raw, DEFAULT_PANEL_BIND);
    }

    #[test]
    fn resolve_parses_http_bind_into_bridge_base() {
        let mut raw = RawConfig::default();
        raw.http.bind = Some("127.0.0.1:8089".to_string());
        let cfg = resolve(raw).unwrap();
        assert_eq!(
            cfg.http_bridge_base.as_deref(),
            Some("http://127.0.0.1:8089")
        );
    }

    #[test]
    fn resolve_empty_http_bind_string_is_treated_as_off() {
        let mut raw = RawConfig::default();
        raw.http.bind = Some(String::new());
        let cfg = resolve(raw).unwrap();
        assert!(cfg.http_bridge_base.is_none());
    }

    #[test]
    fn resolve_falls_back_on_invalid_panel_bind() {
        let mut raw = RawConfig::default();
        raw.panel.bind = "not-an-addr".to_string();
        let cfg = resolve(raw).unwrap();
        assert_eq!(cfg.panel_bind, DEFAULT_PANEL_BIND.parse().unwrap());
    }

    // ── S3: startup refusal (mirrors DaemonConfig::validate) ────────────────

    #[test]
    fn refuses_to_start_on_non_loopback_bind_with_auth_disabled() {
        let mut raw = RawConfig::default();
        raw.panel.bind = "0.0.0.0:8091".to_string();
        let err = resolve(raw).expect_err("non-loopback + no token must refuse to start");
        let msg = err.to_string();
        assert!(msg.contains("refusing to start"), "{msg}");
        assert!(msg.contains("0.0.0.0:8091"), "{msg}");
        assert!(msg.contains("allow_insecure_lan"), "{msg}");
    }

    #[test]
    fn allow_insecure_lan_downgrades_the_refusal_to_a_loud_log() {
        let mut raw = RawConfig::default();
        raw.panel.bind = "0.0.0.0:8091".to_string();
        raw.dev.allow_insecure_lan = true;
        let cfg = resolve(raw).expect("[dev].allow_insecure_lan is the documented opt-in");
        assert!(cfg.allow_insecure_lan);
        assert_eq!(cfg.panel_bind, "0.0.0.0:8091".parse().unwrap());
    }

    #[test]
    fn loopback_bind_with_auth_disabled_still_starts() {
        let mut raw = RawConfig::default();
        raw.panel.bind = "127.0.0.1:8091".to_string();
        resolve(raw).expect("loopback + no auth is the documented dev default");
    }

    #[test]
    fn disabled_panel_never_refuses() {
        // No listener is ever bound, so there is no surface to refuse.
        let mut raw = RawConfig::default();
        raw.panel.enabled = false;
        raw.panel.bind = "0.0.0.0:8091".to_string();
        let cfg = resolve(raw).expect("a disabled panel binds nothing");
        assert!(!cfg.enabled);
    }

    // ── S1: panel token file hygiene (mirrors the daemon's eager resolve) ───

    /// A throwaway "config dir" plus a token file inside it.
    fn token_fixture(name: &str, contents: &str, mode: u32) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "tv-shell-panel-token-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("panel-token");
        std::fs::write(&token, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&token, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        (dir, token)
    }

    #[test]
    fn panel_token_reads_a_0600_file_inside_the_config_dir() {
        let (dir, token) = token_fixture("ok", "  s3kret\n", 0o600);
        let read = read_panel_token(token.to_str().unwrap(), &dir).unwrap();
        assert_eq!(read, "s3kret");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An empty token file is as unusable as a missing one, and must abort
    /// startup for the same reason: with `[panel].token_file` configured the
    /// panel authenticates every route, so "no token" means every request AND
    /// every `/login` submission is rejected — the recovery panel is bricked.
    #[test]
    fn panel_token_refuses_an_empty_file() {
        let (dir, token) = token_fixture("empty", "   \n", 0o600);
        let err = read_panel_token(token.to_str().unwrap(), &dir)
            .expect_err("an empty token file must abort startup, not degrade to no-token");
        let msg = err.to_string();
        assert!(msg.contains("is empty"), "{msg}");
        assert!(msg.contains("refusing to start"), "{msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A token holding `;`, a space, or any other non-`cookie-octet` would be
    /// truncated on the way back out of the `Cookie:` header, so browser login
    /// would fail forever with no diagnostic. Refuse it at startup.
    #[test]
    fn panel_token_refuses_characters_that_break_the_session_cookie() {
        for (name, contents) in [
            ("semicolon", "abc;def\n"),
            ("space", "abc def\n"),
            ("comma", "abc,def\n"),
            ("quote", "abc\"def\n"),
            ("backslash", "abc\\def\n"),
            ("non-ascii", "abcédef\n"),
        ] {
            let (dir, token) = token_fixture(name, contents, 0o600);
            let outcome = read_panel_token(token.to_str().unwrap(), &dir)
                .map(|t| format!("ACCEPTED {t:?}"))
                .unwrap_or_else(|e| e.to_string());
            assert!(
                outcome.contains("not valid in a cookie value"),
                "{name}: must be refused at startup, got: {outcome}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// Every token this module accepts survives the `Set-Cookie` →
    /// `Cookie:` → `presented_token` round trip byte for byte — the property
    /// `ensure_cookie_safe_token` exists to guarantee.
    #[test]
    fn an_accepted_token_round_trips_through_the_session_cookie() {
        let (dir, token) = token_fixture("roundtrip", "aZ09-_.~+/=!#$%&'*^`|{}[]()<>:?@\n", 0o600);
        let read = read_panel_token(token.to_str().unwrap(), &dir).unwrap();

        let set_cookie = crate::auth::session_cookie(&read);
        // What a browser sends back: just the `name=value` pair.
        let pair = set_cookie.split(';').next().unwrap().to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_str(&pair).unwrap(),
        );
        assert_eq!(
            crate::auth::presented_token(&headers),
            Some(read.as_str()),
            "an accepted token must come back out of the cookie unchanged"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn panel_token_refuses_a_group_or_world_readable_file() {
        let (dir, token) = token_fixture("perms", "s3kret\n", 0o644);
        let err = read_panel_token(token.to_str().unwrap(), &dir)
            .expect_err("a world-readable credential must abort startup");
        assert!(err.to_string().contains("group/other-accessible"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn panel_token_refuses_a_path_outside_the_config_dir() {
        let (dir, token) = token_fixture("escape", "s3kret\n", 0o600);
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let err = read_panel_token(token.to_str().unwrap(), &elsewhere)
            .expect_err("a token file outside the config dir must abort startup");
        assert!(
            err.to_string().contains("escapes the config directory"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn panel_token_refuses_a_missing_file() {
        let dir =
            std::env::temp_dir().join(format!("tv-shell-panel-token-{}-gone", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = read_panel_token(dir.join("nope").to_str().unwrap(), &dir)
            .expect_err("a configured-but-missing token file must abort startup");
        assert!(
            err.to_string().contains("cannot resolve token file path"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── [[panel.nodes]] — remote sidecar nodes (docs/MULTI_NODE_PANEL.md §4) ──────

    /// A config dir holding a token file for a `[[panel.nodes]]` entry.
    fn node_fixture(name: &str, contents: &str, mode: u32) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "tv-shell-panel-nodes-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("sidecar-token");
        std::fs::write(&token, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&token, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        (dir, token)
    }

    fn node_entry(token_path: &Path) -> RawRemoteNode {
        RawRemoteNode {
            id: "desktop-2".to_string(),
            base_url: "http://192.168.8.153:47995".to_string(),
            sidecar_token_file: token_path.to_str().unwrap().to_string(),
        }
    }

    #[test]
    fn a_panel_nodes_entry_resolves_its_base_url_and_token() {
        let (dir, token) = node_fixture("ok", "  sidecar-s3kret\n", 0o600);
        let nodes = read_remote_nodes(&[node_entry(&token)], &dir).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, "desktop-2");
        assert_eq!(nodes[0].base_url, "http://192.168.8.153:47995");
        assert_eq!(nodes[0].token, "sidecar-s3kret");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The three token-file hygiene rules the panel's own credential obeys
    /// apply unchanged to a node's, because the reason is the same: a token
    /// read from an arbitrary path, or readable by anyone on the box, is a
    /// blast radius that `docs/MULTI_NODE_PANEL.md` §4's "bounded" claim
    /// depends on not existing.
    #[test]
    fn a_panel_nodes_token_obeys_the_same_hygiene_as_the_panels_own() {
        // World-readable ⇒ refuse.
        #[cfg(unix)]
        {
            let (dir, token) = node_fixture("perms", "s3kret\n", 0o644);
            let err = read_remote_nodes(&[node_entry(&token)], &dir)
                .expect_err("a world-readable sidecar credential must abort startup");
            assert!(err.to_string().contains("group/other-accessible"), "{err}");
            assert!(
                err.to_string()
                    .contains("nodes.desktop-2.sidecar_token_file"),
                "the message must name WHICH node's token: {err}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        // Outside the config dir ⇒ refuse.
        let (dir, token) = node_fixture("escape", "s3kret\n", 0o600);
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let err = read_remote_nodes(&[node_entry(&token)], &elsewhere)
            .expect_err("a token file outside the config dir must abort startup");
        assert!(
            err.to_string().contains("escapes the config directory"),
            "{err}"
        );

        // Empty ⇒ refuse. Every sidecar route but /art is bearer-auth'd, so an
        // empty token is a node that can only ever 401.
        std::fs::write(&token, "   \n").unwrap();
        let err = read_remote_nodes(&[node_entry(&token)], &dir).expect_err("empty must abort");
        assert!(err.to_string().contains("is empty"), "{err}");

        // Missing ⇒ refuse.
        std::fs::remove_file(&token).unwrap();
        let err = read_remote_nodes(&[node_entry(&token)], &dir).expect_err("missing must abort");
        assert!(
            err.to_string().contains("cannot resolve token file path"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A sidecar token only ever leaves as `Authorization: Bearer`, never
    /// through a `Set-Cookie`, so the panel-token cookie-octet rule must NOT be
    /// inherited here — it would reject valid bearer tokens for no reason.
    #[test]
    fn a_panel_nodes_token_is_not_held_to_the_session_cookie_alphabet() {
        let (dir, token) = node_fixture("cookie", "abc;def ghi\n", 0o600);
        let nodes = read_remote_nodes(&[node_entry(&token)], &dir)
            .expect("a bearer token never round-trips through a cookie");
        assert_eq!(nodes[0].token, "abc;def ghi");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_panel_nodes_entry_refuses_a_base_url_that_is_not_http() {
        let (dir, token) = node_fixture("url", "s3kret\n", 0o600);
        for bad in [
            "192.168.8.153:47995", // no scheme — the common hand-edit mistake
            "unix:///run/user/1000/sock",
            "file:///etc/passwd",
            "",
            "http://",
            "http:// host:1",
        ] {
            let entry = RawRemoteNode {
                base_url: bad.to_string(),
                ..node_entry(&token)
            };
            let outcome = read_remote_nodes(&[entry], &dir)
                .map(|n| format!("ACCEPTED {:?}", n[0].base_url))
                .unwrap_or_else(|e| e.to_string());
            assert!(
                outcome.contains("refusing to start"),
                "{bad:?} must be refused at startup, got: {outcome}"
            );
        }

        // The other half: a valid URL is accepted, and a trailing slash is
        // normalized away so `…:47995/` and `…:47995` address one node rather
        // than building `…:47995//library` for one of them.
        for good in ["http://192.168.8.153:47995", "https://desktop-2.lan:47995/"] {
            let entry = RawRemoteNode {
                base_url: good.to_string(),
                ..node_entry(&token)
            };
            let nodes = read_remote_nodes(&[entry], &dir)
                .unwrap_or_else(|e| panic!("{good:?} must be accepted: {e}"));
            assert!(!nodes[0].base_url.ends_with('/'), "{:?}", nodes[0].base_url);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_or_unnamed_nodes_are_refused() {
        let (dir, token) = node_fixture("dupe", "s3kret\n", 0o600);
        let err = read_remote_nodes(&[node_entry(&token), node_entry(&token)], &dir)
            .expect_err("two nodes with one id: whichever lost would be unreachable");
        assert!(err.to_string().contains("share id"), "{err}");

        let unnamed = RawRemoteNode {
            id: "   ".to_string(),
            ..node_entry(&token)
        };
        let err = read_remote_nodes(&[unnamed], &dir).expect_err("an unnamed node must abort");
        assert!(err.to_string().contains("empty `id`"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A node's token must not be printable by an accidental `{:?}` — a
    /// startup log line, a panic payload, an `anyhow` chain. One credential per
    /// served node is exactly the scale at which "nothing formats it today"
    /// stops being a safety property.
    #[test]
    fn a_panel_nodes_debug_redacts_its_token() {
        let node = RemoteNode {
            id: "desktop-2".to_string(),
            base_url: "http://192.168.8.153:47995".to_string(),
            token: "super-secret-bearer".to_string(),
        };
        let rendered = format!("{node:?}");
        assert!(
            !rendered.contains("super-secret-bearer"),
            "the token leaked into Debug: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        // The non-secret fields must still be there — a Debug that hides
        // everything is useless for the diagnostics it exists for.
        assert!(rendered.contains("desktop-2"), "{rendered}");
        assert!(rendered.contains("192.168.8.153"), "{rendered}");
    }

    #[test]
    fn no_panel_nodes_section_is_the_normal_case_and_yields_an_empty_list() {
        let cfg = resolve(RawConfig::default()).unwrap();
        assert!(cfg.remote_nodes.is_empty());
    }

    #[test]
    fn panel_nodes_parse_from_the_documented_toml_shape() {
        let toml_text = r#"
            [panel]
            bind = "127.0.0.1:8091"

            [[panel.nodes]]
            id = "desktop-2"
            base_url = "http://192.168.8.153:47995"
            sidecar_token_file = "~/.config/tv-shell/desktop-2-sidecar-token"
        "#;
        let raw: RawConfig = toml::from_str(toml_text).expect("parse");
        assert_eq!(raw.panel.nodes.len(), 1);
        assert_eq!(raw.panel.nodes[0].id, "desktop-2");
        assert_eq!(raw.panel.nodes[0].base_url, "http://192.168.8.153:47995");
        assert_eq!(
            raw.panel.nodes[0].sidecar_token_file,
            "~/.config/tv-shell/desktop-2-sidecar-token"
        );
    }

    #[test]
    fn permissive_parse_still_ignores_unrelated_sections() {
        let toml_text = r#"
            [panel]
            enabled = false
            bind = "127.0.0.1:9000"

            [http]
            bind = "127.0.0.1:8089"

            [mcp]
            bind = "127.0.0.1:8090"
            dev = true

            [cec]
            lifecycle = true

            [plex]
            url = "http://plex:32400"

            [steam]
            url = "http://gaming-pc:47995"

            [observability]
            enabled = true

            [input]
            some_key = "some_value"

            [dev]
            allow_insecure_lan = true
            some_other_dev_key = "ignored"
        "#;
        let raw: RawConfig = toml::from_str(toml_text).expect("permissive parse should succeed");
        assert!(!raw.panel.enabled);
        assert_eq!(raw.panel.bind, "127.0.0.1:9000");
        assert_eq!(raw.http.bind.as_deref(), Some("127.0.0.1:8089"));
        // `[dev]` is no longer ignored: S3 reuses the daemon's own
        // `allow_insecure_lan` flag rather than inventing a second opt-in.
        // Unknown keys WITHIN `[dev]` are still tolerated.
        assert!(raw.dev.allow_insecure_lan);
    }

    #[test]
    fn panel_section_parses_token_file_and_allow_dangerous() {
        let toml_text = r#"
            [panel]
            token_file = "~/.config/tv-shell/panel-token"
            allow_dangerous = true
        "#;
        let raw: RawConfig = toml::from_str(toml_text).expect("parse");
        assert_eq!(
            raw.panel.token_file.as_deref(),
            Some("~/.config/tv-shell/panel-token")
        );
        assert!(raw.panel.allow_dangerous);
    }

    #[test]
    fn read_token_file_expands_tilde_and_trims() {
        let dir = std::env::temp_dir().join(format!("tv-shell-panel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token_path = dir.join("token");
        std::fs::write(&token_path, "  sekret-token\n").unwrap();

        let read = read_token_file(token_path.to_str().unwrap());
        assert_eq!(read.as_deref(), Some("sekret-token"));

        // Empty file ⇒ None.
        std::fs::write(&token_path, "   \n").unwrap();
        assert_eq!(read_token_file(token_path.to_str().unwrap()), None);

        // Missing file ⇒ None.
        std::fs::remove_file(&token_path).unwrap();
        assert_eq!(read_token_file(token_path.to_str().unwrap()), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expand_tilde_prefixes_home() {
        let _env = env_guard();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/testuser");
        assert_eq!(
            expand_tilde("~/config.toml"),
            PathBuf::from("/home/testuser/config.toml")
        );
        assert_eq!(
            expand_tilde("/absolute/path"),
            PathBuf::from("/absolute/path")
        );
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn unit_and_journal_names_use_slug() {
        assert_eq!(daemon_unit().as_str(), "tv-shell-input.service");
        assert_eq!(shell_unit().as_str(), "tv-shell-quickshell.service");
        assert_eq!(panel_unit().as_str(), "tv-shell-panel.service");
        assert_eq!(daemon_journal_unit(), "tv-shell-input");
        assert_eq!(shell_journal_tag(), "tv-shell-quickshell");
    }

    /// `brand_unit` `expect`s — this is what stops that being a latent panic.
    #[test]
    fn built_in_unit_names_are_valid_unit_names() {
        for key in BUILT_IN_UNIT_KEYS {
            let target = builtin_target(key).expect("every built-in key resolves");
            assert_eq!(target.key(), key);
            assert!(
                UnitName::parse(target.unit().as_str()).is_ok(),
                "built-in {key} resolves to an invalid unit name {}",
                target.unit()
            );
            assert_eq!(
                target.scope(),
                UnitScope::User,
                "all three tv-shell units are --user units; a System one would need sudo"
            );
        }
        assert!(builtin_target("sshd").is_none());
    }

    fn raw(key: &str, unit: &str, scope: &str) -> RawManagedUnit {
        RawManagedUnit {
            key: key.to_string(),
            unit: unit.to_string(),
            scope: scope.to_string(),
        }
    }

    #[test]
    fn managed_units_resolve_key_unit_and_scope() {
        let resolved = resolve_managed_units(&[
            raw("sshd", "sshd.service", "system"),
            raw("pipewire", "pipewire.service", "user"),
        ])
        .expect("a well-formed list resolves");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].key(), "sshd");
        assert_eq!(resolved[0].unit().as_str(), "sshd.service");
        assert_eq!(resolved[0].scope(), UnitScope::System);
        assert_eq!(resolved[1].scope(), UnitScope::User);
    }

    /// The built-ins are hardcoded so a config typo cannot cost the recovery
    /// path — which only holds if a colliding key is REFUSED rather than
    /// silently shadowing (or being shadowed by) the built-in.
    #[test]
    fn a_managed_unit_cannot_take_a_built_in_key() {
        for key in BUILT_IN_UNIT_KEYS {
            let err = resolve_managed_units(&[raw(key, "somethingelse.service", "system")])
                .expect_err("a built-in key collision must be a load error");
            let msg = err.to_string();
            assert!(
                msg.contains(key) && msg.contains("built-in"),
                "the error must name the colliding key and say why: {msg}"
            );
        }
    }

    /// Even if the collision check above were removed, resolution consults the
    /// built-ins first — so `daemon` can never resolve to a config-supplied
    /// unit. Belt and braces on the one property phase 5 must not lose.
    #[test]
    fn built_in_keys_win_the_lookup() {
        let cfg = AppConfig {
            managed_units: vec![RestartTarget {
                key: "daemon".to_string(),
                unit: UnitName::parse("evil.service").unwrap(),
                scope: UnitScope::System,
            }],
            ..AppConfig::default()
        };
        let resolved = cfg.restart_target("daemon").expect("daemon resolves");
        assert_eq!(resolved.unit(), &daemon_unit());
        assert_eq!(resolved.scope(), UnitScope::User);
    }

    #[test]
    fn managed_units_reject_duplicates_and_empty_keys() {
        let err = resolve_managed_units(&[
            raw("sshd", "sshd.service", "system"),
            raw("sshd", "ssh.service", "system"),
        ])
        .expect_err("a duplicate key must be a load error");
        assert!(err.to_string().contains("two entries"), "{err}");

        let err = resolve_managed_units(&[raw("  ", "sshd.service", "system")])
            .expect_err("an empty key must be a load error");
        assert!(err.to_string().contains("empty `key`"), "{err}");

        let err = resolve_managed_units(&[raw("ssh/../root", "sshd.service", "system")])
            .expect_err("a key that is not a path segment must be a load error");
        assert!(err.to_string().contains("URL path segment"), "{err}");
    }

    #[test]
    fn a_managed_unit_with_a_bad_scope_is_a_load_error() {
        for bad in ["System", "root", "", "user ; reboot"] {
            let err = resolve_managed_units(&[raw("x", "x.service", bad)])
                .expect_err("only \"system\"/\"user\" may parse");
            let msg = err.to_string();
            assert!(
                msg.contains("bad `scope`") && msg.contains("\"system\" or \"user\""),
                "the error must name the field and the two legal values: {msg}"
            );
        }
    }

    #[test]
    fn a_managed_unit_with_a_malformed_unit_is_a_load_error() {
        for bad in ["", "a b.service", "../etc/passwd", "-x.service", "sshd.sh"] {
            let err = resolve_managed_units(&[raw("x", bad, "system")])
                .expect_err("a malformed unit name must be a load error");
            assert!(
                err.to_string().contains("bad `unit`"),
                "the error must name the field: {err}"
            );
        }
    }

    /// The read path's validator: an operator types into this one, so it is
    /// the boundary between a text box and an `exec`.
    #[test]
    fn unit_name_parse_rejects_everything_that_is_not_a_unit_name() {
        for bad in [
            "",
            "   ",
            "sshd .service",
            "sshd\t.service",
            "sshd;reboot",
            "sshd&&reboot",
            "sshd|tee",
            "$(reboot)",
            "`reboot`",
            "/etc/systemd/system/sshd.service",
            "../../etc/shadow",
            "..",
            "-h",
            "--user",
            ".hidden.service",
            "sshd\nreboot.service",
            "sshd.sh",
            "sshd.",
            "sshd\\x2d.service",
        ] {
            assert!(
                UnitName::parse(bad).is_err(),
                "{bad:?} must be rejected by the read-path validator"
            );
        }
        let absurd = format!("{}.service", "a".repeat(4096));
        assert!(UnitName::parse(&absurd).is_err(), "length cap must hold");

        for good in [
            "sshd.service",
            "NetworkManager.service",
            "systemd-networkd.socket",
            "getty@tty1.service",
            "org.freedesktop.thing.service",
            "sshd",
            "  bluetooth.service  ",
        ] {
            UnitName::parse(good).unwrap_or_else(|e| panic!("{good:?} must be accepted: {e}"));
        }
        assert_eq!(
            UnitName::parse("  bluetooth.service  ").unwrap().as_str(),
            "bluetooth.service",
            "parse trims, so the validated value is what reaches systemctl"
        );
    }

    #[test]
    fn remote_access_criticality_covers_ssh_and_the_network_link_only() {
        let units = resolve_managed_units(&[
            raw("sshd", "sshd.service", "system"),
            raw("network", "NetworkManager.service", "system"),
            raw("bluetooth", "bluetooth.service", "system"),
            raw("usersshd", "sshd.service", "user"),
        ])
        .unwrap();
        assert!(units[0].is_remote_access_critical(), "sshd strands the box");
        assert!(
            units[1].is_remote_access_critical(),
            "NetworkManager owns the link ssh arrives over"
        );
        assert!(
            !units[2].is_remote_access_critical(),
            "bluetooth is not how anyone reaches this box"
        );
        assert!(
            !units[3].is_remote_access_critical(),
            "a --user unit cannot be the system ssh server"
        );
    }

    #[test]
    fn socket_path_prefers_env_override() {
        let _env = env_guard();
        // Restore rather than blindly removing: a dev running the suite with
        // TV_SHELL_SOCK exported would otherwise have it silently unset for
        // every test that ran after this one.
        let prev = std::env::var_os("TV_SHELL_SOCK");
        std::env::set_var("TV_SHELL_SOCK", "/tmp/custom.sock");
        assert_eq!(socket_path(), PathBuf::from("/tmp/custom.sock"));
        match prev {
            Some(v) => std::env::set_var("TV_SHELL_SOCK", v),
            None => std::env::remove_var("TV_SHELL_SOCK"),
        }
    }
}

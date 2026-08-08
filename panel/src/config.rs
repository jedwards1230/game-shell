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
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: DEFAULT_PANEL_BIND.to_string(),
            token_file: None,
            allow_dangerous: false,
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
/// never write) `config.toml` directly — e.g. the Settings page's read-only
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
    };
    if cfg.enabled {
        cfg.validate()?;
    }
    Ok(cfg)
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

/// systemd unit name for the input daemon (`tv-shell-input.service`).
pub fn daemon_unit() -> String {
    format!("{}-input.service", tv_shell_protocol::brand::SLUG)
}

/// systemd unit name for the Quickshell shell (`tv-shell-quickshell.service`).
pub fn shell_unit() -> String {
    format!("{}-quickshell.service", tv_shell_protocol::brand::SLUG)
}

/// systemd unit name for the panel itself (`tv-shell-panel.service`).
pub fn panel_unit() -> String {
    format!("{}-panel.service", tv_shell_protocol::brand::SLUG)
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
        assert_eq!(daemon_unit(), "tv-shell-input.service");
        assert_eq!(shell_unit(), "tv-shell-quickshell.service");
        assert_eq!(panel_unit(), "tv-shell-panel.service");
        assert_eq!(daemon_journal_unit(), "tv-shell-input");
        assert_eq!(shell_journal_tag(), "tv-shell-quickshell");
    }

    #[test]
    fn socket_path_prefers_env_override() {
        std::env::set_var("TV_SHELL_SOCK", "/tmp/custom.sock");
        assert_eq!(socket_path(), PathBuf::from("/tmp/custom.sock"));
        std::env::remove_var("TV_SHELL_SOCK");
    }
}

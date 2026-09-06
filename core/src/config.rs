//! v2 core configuration — `~/.config/tv-shell/core.toml`.
//!
//! # Why this is a separate file from v1's `config.toml`
//!
//! V2_DESIGN §11: "beside, not instead, at every shared layer". The v1 daemon's
//! `DaemonConfig` root carries `#[serde(deny_unknown_fields)]`, so adding a `[display]`
//! or `[session]` table to `config.toml` would make the **v1 daemon abort at
//! startup** — and the symptom would present as "v1 is broken", not "someone
//! added a v2 table". v1 must keep booting on the couch while v2 is developed
//! beside it, so v2 gets its own file. Nothing here is ever read by v1, and
//! nothing v1 reads is ever written here.
//!
//! The path is overridable with `TV_SHELL_CORE_CONFIG` for tests and for running
//! two cores on one box.
//!
//! # Conventions carried from `daemon/src/daemon_config.rs`
//!
//! * Root and every section are `#[serde(default, deny_unknown_fields)]`, so a
//!   typo fails loudly at startup rather than silently running a default.
//! * Three tiers: [`CoreConfig::load`] (path from env) → [`CoreConfig::load_from`]
//!   (I/O, testable) → [`CoreConfig::parse`] (pure).
//! * A missing file is not an error (all-defaults); a present-but-malformed one
//!   is, because an operator should learn their config was ignored.
//! * [`CoreConfig::validate`] is separate and runs before anything uses the
//!   values, with `anyhow::bail!("config: ...")` messages naming the bad value.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::atoms::AppId;

/// Env var overriding the config path.
pub const CONFIG_PATH_ENV: &str = "TV_SHELL_CORE_CONFIG";
/// Env var overriding the IPC socket path.
pub const SOCKET_PATH_ENV: &str = "TV_SHELL_CORE_SOCK";
/// Default socket basename. Deliberately NOT `tv-shell-input.sock`: §11 requires
/// v1 and v2 to share no socket, prefix or unit name, so a stray v1 client can
/// never reach the v2 core (or the reverse) and be answered by the wrong grammar.
pub const DEFAULT_SOCKET_NAME: &str = "tv-shell-core.sock";

/// The full typed core configuration.
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CoreConfig {
    pub display: DisplayConfig,
    pub session: SessionConfig,
    pub supervisor: SupervisorConfig,
}

/// `[display]` — the output mode gamescope is pinned to, and HDR policy.
///
/// The mode is pinned rather than negotiated because the EDID preferred mode on
/// this chain is 60 Hz (§6): letting the display choose gives 4K60, not 4K120.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    /// `-W`. Output width in pixels.
    ///
    /// **Consumed by [`CoreConfig::session_env`]**, which the session script
    /// renders into `$XDG_RUNTIME_DIR/tv-shell-gamescope-mode` before the target
    /// starts; the gamescope unit reads that file and substitutes
    /// `${TV_SHELL_GS_WIDTH}` into its `ExecStart`.
    ///
    /// It used to say "read by the unit's ExecStart" while the unit hard-coded
    /// `-W 3840 -H 2160 -r 120`, so setting `refresh = 60` here changed nothing
    /// and `validate()` accepted it — a config key whose stated consumer did not
    /// exist. The env file is the link that makes the label true.
    pub width: u32,
    /// `-H`. Output height in pixels. See [`Self::width`] for the link.
    pub height: u32,
    /// `-r`. Refresh rate in Hz. See [`Self::width`] for the link.
    pub refresh: u32,
    /// Whether HDR should be on by default. Applied as a root-atom write, never
    /// as a bare `gamescopectl <convar>` — §6: a value-less call RESETS the
    /// convar to its default and turns HDR off with no log line, which is how
    /// the phase-2 "feedback 0" was self-inflicted.
    ///
    /// **NOT YET READ BY ANYTHING.** The unit passes `--hdr-enabled`
    /// unconditionally, and the core does not write an HDR atom — the atom that
    /// would carry it is not even in [`crate::atoms::names::ALL`]. It becomes
    /// live with the HDR/VRR control surface (§6); until then this key records
    /// intent and changes nothing.
    pub hdr: bool,
    /// `--hdr-sdr-content-nits`: the shell's white point inside an HDR output.
    ///
    /// **NOT YET READ BY ANYTHING** — same PR as [`Self::hdr`]. The flag is not
    /// on the unit's `ExecStart` either.
    pub sdr_nits: u32,
    /// How long `GAMESCOPE_HDR_OUTPUT_FEEDBACK` must read the configured value
    /// before an HDR-capable launch is allowed to proceed.
    ///
    /// §6: an HDMI re-negotiation (~1 s) zeroes the HDR and VRR feedback atoms
    /// and then restores them, and a Vulkan surface created inside that window
    /// stays SDR for its life. This is the settle period that gates a launch
    /// past it.
    ///
    /// **NOT YET READ BY ANYTHING.** [`crate::launch`] does not gate on display
    /// feedback yet; the gate lands with the HDR-aware launch path (§6).
    pub hotplug_settle_ms: u64,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        // The measured-good mode from §6 on the target chain.
        Self {
            width: 3840,
            height: 2160,
            refresh: 120,
            hdr: true,
            sdr_nits: 400,
            hotplug_settle_ms: 2000,
        }
    }
}

/// `[session]` — the shell's identity and the Xwayland topology.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SessionConfig {
    /// The shell's own app id.
    ///
    /// **Private, and never 769.** §5: under `--steam`, 769 is the Steam
    /// client's own id (`window_is_steam`: forced fullscreen sizing,
    /// `focus=steam` in the stats pipe) and is reserved for the Steam client
    /// when it runs as an app. The shell sizes itself to the output instead of
    /// inheriting that path. 9001 is the id the measurement kit used.
    pub shell_app_id: u32,
    /// How many Xwayland servers gamescope starts (`--xwayland-count`).
    /// One for the shell plus one per concurrently launched app (§4).
    ///
    /// **NOT `-e`.** `-e` is the short form of `--steam`, which is why the unit
    /// passed it twice; `--xwayland-count` is long-only and takes an argument.
    /// The old label sent a reader straight into writing `-e 2`, where `2` is not
    /// consumed as an argument but becomes gamescope's child command; gamescope
    /// execs it, it fails instantly, `BindsTo=` stops the target and the
    /// television relogins forever.
    ///
    /// Consumed by [`CoreConfig::session_env`] — see [`DisplayConfig::width`].
    pub xwayland_count: u32,
    /// Bound on the base-layer read-back after a switch, for an app whose
    /// window is ALREADY MAPPED.
    ///
    /// The switch itself measured 14–19 ms over 20 switches; this is the
    /// failure bound, not the expected time. See
    /// [`crate::baselayer::DEFAULT_SWITCH_TIMEOUT`].
    ///
    /// Bounded above as well as below. An intent holds the intent lock for its
    /// whole write-and-verify, so `switch_timeout_ms = 99999999` would spin for
    /// ~28 hours with every later `show`/`home` queued behind it — including
    /// §9's "stuck in an app → `intent home`", the recovery this whole design
    /// exists to keep reachable. A bound that can disable the escape hatch is
    /// not a tuning knob.
    pub switch_timeout_ms: u64,
    /// Bound on waiting for an app that has NOT mapped a window yet.
    ///
    /// A separate, much larger bound, because a cold app start and a compositor
    /// switch are different waits (see [`crate::baselayer`]). Sharing one bound
    /// made `show <id>` right after `launch <id>` fail on every working launch.
    pub map_timeout_ms: u64,
    /// Bound on confirming that a launched process really is in its scope.
    ///
    /// [`crate::launch::launch`] polls `/proc/<pid>/cgroup` until the scope
    /// appears; this is how long it waits before calling the launch unconfirmed.
    pub launch_confirm_ms: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            shell_app_id: 9001,
            xwayland_count: 2,
            // DERIVED, not a second literal: the bound has exactly one source
            // (see [`crate::baselayer::DEFAULT_SWITCH_TIMEOUT`], which carries
            // the measurement it comes from), so it cannot be changed there and
            // left stale here.
            switch_timeout_ms: crate::baselayer::DEFAULT_SWITCH_TIMEOUT.as_millis() as u64,
            map_timeout_ms: crate::baselayer::DEFAULT_MAP_TIMEOUT.as_millis() as u64,
            launch_confirm_ms: DEFAULT_LAUNCH_CONFIRM.as_millis() as u64,
        }
    }
}

/// How long a launch gets to appear in its cgroup scope.
///
/// `systemd-run --scope` creates the unit before it execs, so the scope is
/// normally readable within a few milliseconds; two seconds is headroom for a
/// loaded box and a busy session bus, not an expected time.
pub const DEFAULT_LAUNCH_CONFIRM: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Upper bound on `switch_timeout_ms`. See the field docs.
pub const MAX_SWITCH_TIMEOUT_MS: u64 = 5_000;
/// Upper bound on `map_timeout_ms`. Five minutes is longer than any app start
/// this box will ever see; past it, "the app did not come up" is the answer.
pub const MAX_MAP_TIMEOUT_MS: u64 = 300_000;
/// Upper bound on `launch_confirm_ms`.
pub const MAX_LAUNCH_CONFIRM_MS: u64 = 60_000;

/// `[supervisor]` — stall detection and restart thresholds (§9).
///
/// The core does not yet act on these (the forced-paint heartbeat is a later
/// PR); they are defined here so the config file the units ship against is
/// complete rather than growing a new required key at deploy time.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SupervisorConfig {
    /// Frames must advance at least this often, or the shell is considered
    /// stalled. §9: this is a forced-paint probe, because gamescope's stats FIFO
    /// emits one `fps=` line per 300 paints and is legitimately silent on a
    /// static base layer under VRR.
    pub stall_secs: u64,
    /// How many restarts inside [`Self::restart_window_secs`] count as a
    /// crash-loop rather than a bad day.
    pub restart_threshold: u32,
    /// The window the restart threshold is counted over.
    pub restart_window_secs: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        // Matches the short-session tracker shape SteamOS/ChimeraOS ship and
        // the daemon unit's existing StartLimitBurst=3 / IntervalSec=60.
        Self {
            stall_secs: 10,
            restart_threshold: 3,
            restart_window_secs: 60,
        }
    }
}

impl CoreConfig {
    /// The shell's app id as an [`AppId`].
    pub fn shell_app_id(&self) -> AppId {
        AppId::new(self.session.shell_app_id)
    }

    /// The base-layer switch bound (for an already-mapped window).
    pub fn switch_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.session.switch_timeout_ms)
    }

    /// The bound on waiting for an app's first window to map.
    pub fn map_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.session.map_timeout_ms)
    }

    /// The bound on confirming a launch reached its scope.
    pub fn launch_confirm_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.session.launch_confirm_ms)
    }

    /// Render the environment file the gamescope unit's `ExecStart` reads.
    ///
    /// **This is the link that makes `[display]` and `session.xwayland_count`
    /// real settings.** The unit used to hard-code `-W 3840 -H 2160 -r 120`
    /// while the config's doc comments claimed the unit read those keys, so an
    /// operator could set `refresh = 60`, watch `validate()` accept it, and
    /// change nothing at all.  Now the session script writes this file before the
    /// target starts and the unit substitutes the variables into its `ExecStart`.
    ///
    /// Pure, so the exact bytes are asserted in a unit test rather than only on a
    /// boot nobody can run in CI. systemd's `EnvironmentFile` parser takes
    /// `KEY=value` lines; every value here is a bare integer, so no quoting
    /// question arises — deliberately, since a value needing quotes is a value
    /// whose `${VAR}` splitting rules would have to be reasoned about.
    pub fn session_env(&self) -> String {
        format!(
            "{ENV_WIDTH}={}\n{ENV_HEIGHT}={}\n{ENV_REFRESH}={}\n{ENV_XWAYLAND_COUNT}={}\n",
            self.display.width,
            self.display.height,
            self.display.refresh,
            self.session.xwayland_count,
        )
    }

    /// Load from the resolved path. A missing file yields all-defaults.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&config_path())
    }

    /// Load from an explicit path (testable; no env or global state).
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            // Absent ⇒ defaults, so a fresh install still boots. Any other read
            // error (permissions, a directory) surfaces.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("reading {}: {e}", path.display())),
        }
    }

    /// Parse a TOML document (no I/O).
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        toml::from_str(text).map_err(|e| anyhow::anyhow!("parsing core.toml: {e}"))
    }

    /// Reject values that would fail confusingly later.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.display.width == 0 || self.display.height == 0 {
            anyhow::bail!(
                "config: [display] width and height must both be non-zero (got {}x{})",
                self.display.width,
                self.display.height
            );
        }
        if self.display.refresh == 0 {
            anyhow::bail!("config: [display] refresh must be non-zero");
        }
        if self.session.xwayland_count == 0 {
            anyhow::bail!(
                "config: [session] xwayland_count must be at least 1 (the shell needs a server)"
            );
        }
        // §5: 769 is the Steam client's id under `--steam`. A shell claiming it
        // would inherit `window_is_steam` handling — forced fullscreen sizing
        // and `focus=steam` in the stats pipe — and would collide with the real
        // Steam client the moment it runs as an app.
        if self.session.shell_app_id == STEAM_CLIENT_APP_ID {
            anyhow::bail!(
                "config: [session] shell_app_id must not be {STEAM_CLIENT_APP_ID}; \
                 that id is reserved for the Steam client under gamescope's --steam \
                 focus policy (V2_DESIGN §5)"
            );
        }
        if self.session.switch_timeout_ms == 0 {
            anyhow::bail!(
                "config: [session] switch_timeout_ms must be non-zero; a zero bound would \
                 report every switch as failed before the compositor could publish it"
            );
        }
        if self.session.switch_timeout_ms > MAX_SWITCH_TIMEOUT_MS {
            anyhow::bail!(
                "config: [session] switch_timeout_ms must be at most {MAX_SWITCH_TIMEOUT_MS} \
                 (got {}); an intent holds the intent lock for its whole verify, so a larger \
                 bound queues every later show/home behind it — including the `intent home` \
                 escape hatch (V2_DESIGN §9)",
                self.session.switch_timeout_ms
            );
        }
        if self.session.map_timeout_ms == 0 || self.session.map_timeout_ms > MAX_MAP_TIMEOUT_MS {
            anyhow::bail!(
                "config: [session] map_timeout_ms must be between 1 and {MAX_MAP_TIMEOUT_MS} \
                 (got {})",
                self.session.map_timeout_ms
            );
        }
        if self.session.map_timeout_ms < self.session.switch_timeout_ms {
            anyhow::bail!(
                "config: [session] map_timeout_ms ({}) must be at least switch_timeout_ms ({}); \
                 a launching app is given the map bound, so a shorter one would fail a launch \
                 sooner than an already-mapped switch",
                self.session.map_timeout_ms,
                self.session.switch_timeout_ms
            );
        }
        if self.session.launch_confirm_ms == 0
            || self.session.launch_confirm_ms > MAX_LAUNCH_CONFIRM_MS
        {
            anyhow::bail!(
                "config: [session] launch_confirm_ms must be between 1 and \
                 {MAX_LAUNCH_CONFIRM_MS} (got {}); zero would call every launch unconfirmed \
                 before its scope could appear",
                self.session.launch_confirm_ms
            );
        }
        if self.supervisor.stall_secs == 0 {
            anyhow::bail!("config: [supervisor] stall_secs must be non-zero");
        }
        if self.supervisor.restart_threshold == 0 {
            anyhow::bail!(
                "config: [supervisor] restart_threshold must be at least 1; 0 would treat \
                 the first start as a crash-loop"
            );
        }
        if self.supervisor.restart_window_secs == 0 {
            anyhow::bail!("config: [supervisor] restart_window_secs must be non-zero");
        }
        Ok(())
    }
}

/// Env var names in the file [`CoreConfig::session_env`] renders.
///
/// `pub` because the test that defends the consumer link greps the unit file for
/// exactly these strings — a name changed here and not in the unit is the phantom
/// consumer this whole mechanism exists to prevent.
pub const ENV_WIDTH: &str = "TV_SHELL_GS_WIDTH";
/// See [`ENV_WIDTH`].
pub const ENV_HEIGHT: &str = "TV_SHELL_GS_HEIGHT";
/// See [`ENV_WIDTH`].
pub const ENV_REFRESH: &str = "TV_SHELL_GS_REFRESH";
/// See [`ENV_WIDTH`].
pub const ENV_XWAYLAND_COUNT: &str = "TV_SHELL_GS_XWAYLAND_COUNT";

/// The Steam client's app id under gamescope's `--steam` focus policy.
pub const STEAM_CLIENT_APP_ID: u32 = 769;

/// `$TV_SHELL_CORE_CONFIG`, else `${XDG_CONFIG_HOME:-$HOME/.config}/tv-shell/core.toml`.
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os(CONFIG_PATH_ENV) {
        return PathBuf::from(p);
    }
    config_dir().join("core.toml")
}

/// `$TV_SHELL_CORE_SOCK`, else `/run/user/<uid>/tv-shell-core.sock`.
pub fn socket_path() -> String {
    if let Ok(p) = std::env::var(SOCKET_PATH_ENV) {
        if !p.is_empty() {
            return p;
        }
    }
    // SAFETY: `getuid` is always safe — it cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    format!("/run/user/{uid}/{DEFAULT_SOCKET_NAME}")
}

/// `${XDG_CONFIG_HOME:-$HOME/.config}/tv-shell`.
///
/// Deliberately not the daemon's `brand::config_dir`: this crate does not depend
/// on `tv-shell-protocol`, and the legacy `game-shell` read-fallback that
/// function carries is v1 migration baggage a new file has no use for.
fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config")
        });
    base.join("tv-shell")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_document_is_all_defaults() {
        let c = CoreConfig::parse("").unwrap();
        assert_eq!(c, CoreConfig::default());
        assert_eq!(c.display.width, 3840);
        assert_eq!(c.display.height, 2160);
        assert_eq!(c.display.refresh, 120);
        assert!(c.display.hdr);
        assert_eq!(c.session.shell_app_id, 9001);
        assert_eq!(c.supervisor.restart_threshold, 3);
        c.validate().unwrap();
    }

    #[test]
    fn a_partial_section_keeps_the_other_defaults() {
        let c = CoreConfig::parse("[display]\nrefresh = 60\n").unwrap();
        assert_eq!(c.display.refresh, 60);
        assert_eq!(c.display.width, 3840, "unset keys keep their defaults");
        assert!(c.display.hdr);
    }

    #[test]
    fn every_documented_key_parses() {
        let text = "\
[display]
width = 1920
height = 1080
refresh = 60
hdr = false
sdr_nits = 203
hotplug_settle_ms = 1500

[session]
shell_app_id = 9002
xwayland_count = 4
switch_timeout_ms = 500

[supervisor]
stall_secs = 20
restart_threshold = 5
restart_window_secs = 120
";
        let c = CoreConfig::parse(text).unwrap();
        c.validate().unwrap();
        assert_eq!(c.display.sdr_nits, 203);
        assert!(!c.display.hdr);
        assert_eq!(c.session.xwayland_count, 4);
        assert_eq!(c.shell_app_id(), AppId::new(9002));
        assert_eq!(c.switch_timeout().as_millis(), 500);
        assert_eq!(c.supervisor.stall_secs, 20);
    }

    #[test]
    fn an_unknown_root_table_is_refused() {
        let err = CoreConfig::parse("[nonsense]\nx = 1\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("core.toml"), "{err}");
    }

    #[test]
    fn an_unknown_key_inside_a_section_is_refused() {
        // deny_unknown_fields is what turns a typo into a startup failure
        // instead of a silently-ignored setting.
        assert!(CoreConfig::parse("[display]\nrefres = 120\n").is_err());
        assert!(CoreConfig::parse("[session]\nshell_appid = 9001\n").is_err());
        assert!(CoreConfig::parse("[supervisor]\nstall = 10\n").is_err());
    }

    #[test]
    fn v1_tables_are_refused_so_the_two_files_cannot_be_confused() {
        // Pointing the core at v1's config.toml must fail loudly, not read as
        // all-defaults. §11: the two files share nothing.
        for v1 in ["[http]\nbind = \"127.0.0.1:8089\"\n", "[cec]\n", "[mqtt]\n"] {
            assert!(CoreConfig::parse(v1).is_err(), "should refuse: {v1}");
        }
    }

    #[test]
    fn malformed_toml_is_an_error_not_defaults() {
        assert!(CoreConfig::parse("[display").is_err());
        assert!(CoreConfig::parse("width = ").is_err());
    }

    #[test]
    fn zero_mode_values_are_refused() {
        for text in [
            "[display]\nwidth = 0\n",
            "[display]\nheight = 0\n",
            "[display]\nrefresh = 0\n",
        ] {
            let c = CoreConfig::parse(text).unwrap();
            let err = c.validate().unwrap_err().to_string();
            assert!(err.starts_with("config: "), "{err}");
        }
    }

    #[test]
    fn the_shell_may_not_claim_the_steam_client_id() {
        let c = CoreConfig::parse("[session]\nshell_app_id = 769\n").unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("769"), "{err}");
        assert!(err.contains("Steam"), "{err}");
    }

    #[test]
    fn zero_xwayland_count_is_refused() {
        let c = CoreConfig::parse("[session]\nxwayland_count = 0\n").unwrap();
        assert!(c.validate().is_err());
    }

    #[test]
    fn a_zero_switch_bound_is_refused() {
        let c = CoreConfig::parse("[session]\nswitch_timeout_ms = 0\n").unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("switch_timeout_ms"), "{err}");
    }

    #[test]
    fn zero_supervisor_values_are_refused() {
        for text in [
            "[supervisor]\nstall_secs = 0\n",
            "[supervisor]\nrestart_threshold = 0\n",
            "[supervisor]\nrestart_window_secs = 0\n",
        ] {
            let c = CoreConfig::parse(text).unwrap();
            assert!(c.validate().is_err(), "should refuse: {text}");
        }
    }

    #[test]
    fn a_missing_file_loads_as_defaults() {
        let path = Path::new("/nonexistent/tv-shell/definitely-not-here/core.toml");
        assert_eq!(CoreConfig::load_from(path).unwrap(), CoreConfig::default());
    }

    #[test]
    fn the_default_socket_name_cannot_collide_with_v1() {
        // §11: v1 and v2 share no socket name. v1's is tv-shell-input.sock.
        assert_ne!(DEFAULT_SOCKET_NAME, "tv-shell-input.sock");
        assert!(DEFAULT_SOCKET_NAME.ends_with(".sock"));
    }

    /// Serializes the env mutation below. Same shape as
    /// `daemon/src/daemon_config.rs`'s `ENV_GUARD`.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `XDG_CONFIG_HOME` pointed at a scratch dir and
    /// `TV_SHELL_CORE_CONFIG` unset, restoring both afterwards.
    ///
    /// Modelled on the daemon's `with_temp_config_dir`, including its `// SAFETY:`
    /// discipline around `set_var`/`remove_var`: those are process-global and
    /// unsound under concurrent readers, so every mutation is behind `ENV_GUARD`
    /// and undone before returning.
    fn with_scratch_config_home(f: impl FnOnce(&Path)) {
        let _g = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let base = std::env::temp_dir().join(format!("tv-core-cfg-{}", std::process::id()));
        std::fs::create_dir_all(base.join("tv-shell")).unwrap();

        let prev_home = std::env::var_os("XDG_CONFIG_HOME");
        let prev_override = std::env::var_os(CONFIG_PATH_ENV);
        // SAFETY: serialized by ENV_GUARD; both vars restored before returning.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &base);
            std::env::remove_var(CONFIG_PATH_ENV);
        }

        f(&base);

        // SAFETY: serialized by ENV_GUARD; this is the restore half.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match prev_override {
                Some(v) => std::env::set_var(CONFIG_PATH_ENV, v),
                None => std::env::remove_var(CONFIG_PATH_ENV),
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_config_file_name_cannot_collide_with_v1() {
        // v1's is config.toml, and its root is deny_unknown_fields, so sharing
        // the file would abort v1 at startup. This exercises the REAL
        // `config_path()`, not a re-implementation of it in the test.
        with_scratch_config_home(|base| {
            let resolved = config_path();
            assert_eq!(resolved, base.join("tv-shell").join("core.toml"));
            assert!(resolved.ends_with("tv-shell/core.toml"), "{resolved:?}");
            assert!(!resolved.ends_with("config.toml"), "{resolved:?}");
        });
    }

    #[test]
    fn the_path_override_is_honoured() {
        with_scratch_config_home(|_| {
            let want = std::env::temp_dir().join("somewhere-else.toml");
            // SAFETY: `with_scratch_config_home` holds ENV_GUARD for the whole
            // closure (std's Mutex is not reentrant, so this must NOT re-lock),
            // and it restores CONFIG_PATH_ENV on the way out.
            unsafe { std::env::set_var(CONFIG_PATH_ENV, &want) };
            assert_eq!(config_path(), want);
        });
    }

    #[test]
    fn the_switch_bound_default_has_exactly_one_source() {
        assert_eq!(
            SessionConfig::default().switch_timeout_ms,
            crate::baselayer::DEFAULT_SWITCH_TIMEOUT.as_millis() as u64
        );
    }

    #[test]
    fn a_switch_bound_large_enough_to_wedge_the_escape_hatch_is_refused() {
        // The reported shape: switch_timeout_ms = 99999999 spins ~28 h holding
        // the intent lock, queueing every later show/home — including §9's
        // "stuck in an app → intent home" recovery.
        let c = CoreConfig::parse("[session]\nswitch_timeout_ms = 99999999\n").unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("switch_timeout_ms"), "{err}");
        assert!(err.contains("escape hatch"), "{err}");
        // The boundary itself is fine; one past it is not.
        let ok = format!("[session]\nswitch_timeout_ms = {MAX_SWITCH_TIMEOUT_MS}\n");
        CoreConfig::parse(&ok).unwrap().validate().unwrap();
        let over = format!(
            "[session]\nswitch_timeout_ms = {}\n",
            MAX_SWITCH_TIMEOUT_MS + 1
        );
        assert!(CoreConfig::parse(&over).unwrap().validate().is_err());
    }

    #[test]
    fn the_map_bound_is_range_checked_and_never_shorter_than_the_switch_bound() {
        for text in [
            "[session]\nmap_timeout_ms = 0\n".to_string(),
            format!("[session]\nmap_timeout_ms = {}\n", MAX_MAP_TIMEOUT_MS + 1),
            // Shorter than the switch bound: a launching app would be failed
            // sooner than an already-mapped switch, which inverts the whole
            // point of having two bounds.
            "[session]\nswitch_timeout_ms = 4000\nmap_timeout_ms = 300\n".to_string(),
        ] {
            let c = CoreConfig::parse(&text).unwrap();
            assert!(c.validate().is_err(), "should refuse: {text}");
        }
    }

    #[test]
    fn the_launch_confirm_bound_is_range_checked() {
        for text in [
            "[session]\nlaunch_confirm_ms = 0\n".to_string(),
            format!(
                "[session]\nlaunch_confirm_ms = {}\n",
                MAX_LAUNCH_CONFIRM_MS + 1
            ),
        ] {
            assert!(
                CoreConfig::parse(&text).unwrap().validate().is_err(),
                "{text}"
            );
        }
    }

    // -- the consumer link ---------------------------------------------------

    /// The gamescope unit, read from the repo.
    ///
    /// Read rather than `include_str!` on purpose: the point is to check a file
    /// that ships, and a compile-time include would be just as satisfied by a
    /// file that had been deleted from the install tree.
    fn gamescope_unit() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("units")
            .join("tv-shell-gamescope.service");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    /// The env file the core renders is the file the unit actually reads, and
    /// every variable in it is actually substituted.
    ///
    /// This is the half `every_key_is_either_consumed_or_declared_unconsumed`
    /// could not do: that test checks a key is *classified*, so a key labelled
    /// "read by the unit's ExecStart" passed while the unit hard-coded the value
    /// and read nothing. Here the named consumer has to exist.
    #[test]
    fn the_display_keys_reach_the_unit_that_claims_to_read_them() {
        let unit = gamescope_unit();
        for var in [ENV_WIDTH, ENV_HEIGHT, ENV_REFRESH, ENV_XWAYLAND_COUNT] {
            assert!(
                unit.contains(&format!("${{{var}}}")),
                "the unit's ExecStart does not substitute ${{{var}}}, so the config key \
                 that claims it as a consumer changes nothing"
            );
        }
        assert!(
            unit.contains("EnvironmentFile=%t/tv-shell-gamescope-mode"),
            "the unit must read the mode env file the session script renders"
        );
        // And a non-default value really reaches it.
        let c = CoreConfig::parse("[display]\nrefresh = 60\nwidth = 1920\n").unwrap();
        let env = c.session_env();
        assert!(env.contains(&format!("{ENV_REFRESH}=60")), "{env}");
        assert!(env.contains(&format!("{ENV_WIDTH}=1920")), "{env}");
        // Height is unset in that document, so the default must still be
        // published — a key absent from the file must not vanish from the unit.
        assert!(env.contains(&format!("{ENV_HEIGHT}=2160")), "{env}");
    }

    #[test]
    fn the_rendered_env_file_is_one_key_value_line_per_key() {
        let env = CoreConfig::default().session_env();
        let lines: Vec<_> = env.lines().collect();
        assert_eq!(lines.len(), 4, "{env:?}");
        for l in lines {
            let (k, v) = l
                .split_once('=')
                .unwrap_or_else(|| panic!("not KEY=value: {l:?}"));
            assert!(!k.is_empty() && !v.is_empty(), "{l:?}");
            // Bare integers only: anything needing quotes would drag systemd's
            // ${VAR} splitting rules into the ExecStart.
            assert!(v.bytes().all(|b| b.is_ascii_digit()), "{l:?}");
        }
        assert!(
            env.ends_with('\n'),
            "a trailing newline or the last line is lost"
        );
    }

    #[test]
    fn the_unit_never_passes_the_short_form_of_steam_twice() {
        // H4: `-e` IS `--steam`. The unit passed both, and the config doc called
        // `-e` the xwayland-count flag — which sends a reader into writing
        // `-e 2`, where 2 becomes gamescope's child command.
        let unit = gamescope_unit();
        let exec: String = unit
            .lines()
            .skip_while(|l| !l.starts_with("ExecStart="))
            .take_while(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let bare_e = exec
            .split_whitespace()
            .filter(|t| *t == "-e" || *t == "--steam")
            .count();
        assert_eq!(
            bare_e, 1,
            "gamescope's `-e` is the short form of `--steam`; passing both is one \
             flag twice. ExecStart was: {exec}"
        );
        assert!(
            exec.contains("--xwayland-count"),
            "the xwayland count must be the long flag, which takes an argument: {exec}"
        );
    }

    /// The keys that are parsed, validated and documented but that **no code in
    /// this crate reads yet**.
    ///
    /// This is the repo's #416 class — a setting with a rendered control and no
    /// consumer is a control that reports an effect nothing applies. The v2 core
    /// cannot have consumers for all of these yet, so the rule here is the honest
    /// one: every such key is LABELLED as not-yet-read, in this list, in its doc
    /// comment on the struct field, and in `config/core.toml.example`. Adding a
    /// consumer — or adding a new key with none — must change this list
    /// deliberately, which is the whole point.
    ///
    /// **Classification alone is not enough**, which is why
    /// `the_display_keys_reach_the_unit_that_claims_to_read_them` sits above:
    /// this test would have passed a key whose named consumer did not exist.
    #[test]
    fn every_key_is_either_consumed_or_declared_unconsumed() {
        let unconsumed = [
            // Read by nothing yet; land with the HDR/VRR surface (§6).
            "display.hdr",
            "display.sdr_nits",
            "display.hotplug_settle_ms",
            // Read by nothing yet; land with the forced-paint heartbeat (§9).
            "supervisor.stall_secs",
            // §9's short-session tracker is NOT implemented. These two are its
            // config and nothing reads them; the units say so too.
            "supervisor.restart_threshold",
            "supervisor.restart_window_secs",
        ];
        // The consumed ones: each has a reader today — `shell_app_id`,
        // `switch_timeout`, `map_timeout` and `launch_confirm_timeout` in
        // `crate::compositor`, and the rest through `session_env()`, which the
        // session script renders for the gamescope unit.
        let consumed = [
            "session.shell_app_id",
            "session.switch_timeout_ms",
            "session.map_timeout_ms",
            "session.launch_confirm_ms",
            "display.width",
            "display.height",
            "display.refresh",
            "session.xwayland_count",
        ];

        // Exhaustive destructuring, so the lists above cannot drift from the
        // schema silently: ADDING A FIELD TO ANY OF THESE STRUCTS STOPS THIS
        // TEST COMPILING until the new key is classified. (A count alone would
        // not — it would pass for a key nobody had thought about.)
        let CoreConfig {
            display,
            session,
            supervisor,
        } = CoreConfig::default();
        let DisplayConfig {
            width,
            height,
            refresh,
            hdr,
            sdr_nits,
            hotplug_settle_ms,
        } = display;
        let SessionConfig {
            shell_app_id,
            xwayland_count,
            switch_timeout_ms,
            map_timeout_ms,
            launch_confirm_ms,
        } = session;
        let SupervisorConfig {
            stall_secs,
            restart_threshold,
            restart_window_secs,
        } = supervisor;
        let field_count = [
            u64::from(width),
            u64::from(height),
            u64::from(refresh),
            u64::from(hdr),
            u64::from(sdr_nits),
            hotplug_settle_ms,
            u64::from(shell_app_id),
            u64::from(xwayland_count),
            switch_timeout_ms,
            map_timeout_ms,
            launch_confirm_ms,
            stall_secs,
            u64::from(restart_threshold),
            restart_window_secs,
        ]
        .len();

        assert_eq!(
            unconsumed.len() + consumed.len(),
            field_count,
            "every config key must be classified as consumed or not-yet-consumed"
        );
        // And each listed name must really parse, so a rename cannot leave a
        // dead string in the list above.
        for key in unconsumed.iter().chain(consumed.iter()) {
            let (table, name) = key.split_once('.').expect("keys are `table.name`");
            let value = if name == "hdr" { "true" } else { "1" };
            CoreConfig::parse(&format!("[{table}]\n{name} = {value}\n"))
                .unwrap_or_else(|e| panic!("{key} is not a real config key: {e}"));
        }
    }
}

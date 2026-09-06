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
    /// `[[app]]` — the app-class table (§12).
    ///
    /// Named `app` because that is the TOML array-of-tables header an operator
    /// writes; see [`AppConfig`].
    pub app: Vec<AppConfig>,
}

/// One `[[app]]` entry: an app class the core knows how to start.
///
/// # Why the environment is part of the class, and why UNSET is not optional
///
/// Measured on hardware 2026-09-06: launching `/usr/bin/moonlight` bare inside
/// the v2 session gives it `DISPLAY=:0` — and also `WAYLAND_DISPLAY=gamescope-0`
/// and `XDG_SESSION_TYPE=wayland`, inherited from the session the core runs in.
/// Moonlight then selects native Wayland, and §6 records that Moonlight 6.1.0
/// segfaults on gamescope's native Wayland; it never maps a window, so the base
/// layer is set correctly and nothing appears. The core's own error said so
/// precisely ("app 9003 never mapped a window ... the base layer was set, so
/// this is the app failing to start"), which is the *right* failure — but the
/// launch should not need a human to remember the environment.
///
/// The working invocation was
/// `env -u WAYLAND_DISPLAY QT_QPA_PLATFORM=xcb SDL_VIDEODRIVER=x11
/// ENABLE_GAMESCOPE_WSI=1 /usr/bin/moonlight`, which is what
/// `dev/gamescope/lib.sh`'s `gs_moonlight_x11_env` already encodes for the
/// prototype. Note the shape: **one of the four operations is a REMOVAL.** A
/// set-only environment table could not express it, and no value substitutes for
/// absence — `WAYLAND_DISPLAY=""` is not the same as unset, and pressure-vessel
/// rewrites an empty one back to `wayland-0` (§11). So [`Self::env_unset`] is a
/// first-class half of this type, not a convenience.
///
/// # What §12 lists that is deliberately NOT here yet
///
/// §12 also gives an app class an **id strategy** (scope / pid / class), an
/// **input contract** (`gamepad` / `keyboard`) and an **HDR expectation**. None
/// of the three is modelled here, because nothing in this crate could read them:
/// the id strategy is fixed at "scope, tag as repair" in [`crate::launch`] and
/// is not selectable, §7's input layer does not exist, and the HDR settle gate
/// (`display.hotplug_settle_ms`) is itself still unconsumed. Adding them now
/// would be three more keys whose stated consumer does not exist — the #416
/// class this crate's config test exists to catch. They land with their readers.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// The gamescope app id. This is the `%u` in
    /// `app-steam-app<id>-<tag>.scope` and the value written to
    /// `GAMESCOPECTRL_BASELAYER_APPID`, so it is the whole identity of the class.
    ///
    /// Required — there is no sensible default for "which app is this".
    pub id: u32,
    /// argv, already split. Required and non-empty.
    ///
    /// A list rather than a string because the core never invokes a shell: a
    /// string would need quoting rules, and the one thing a shell would buy —
    /// `env -u VAR` — is expressed properly by [`Self::env_unset`] instead.
    pub command: Vec<String>,
    /// Variables to SET in the launched process's environment.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Variables to REMOVE from the launched process's environment.
    ///
    /// Applied after [`Self::env`], so a name in both is removed. See the type
    /// docs for why removal cannot be expressed as a value.
    #[serde(default)]
    pub env_unset: Vec<String>,
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
    /// Whether HDR is on.
    ///
    /// **Consumed by [`CoreConfig::session_env`]**: it renders
    /// `--hdr-enabled --hdr-sdr-content-nits <n>` (or nothing) into
    /// `TV_SHELL_GS_HDR_ARGS`, which the gamescope unit word-splits into its
    /// `ExecStart`. The unit used to pass `--hdr-enabled` unconditionally while
    /// this key documented itself as read by nothing — so an operator could set
    /// `hdr = false` and get HDR anyway. The measured kit gates the flag on
    /// `TV_SHELL_GS_HDR` for the same reason.
    ///
    /// Still NOT a runtime toggle: the core writes no HDR atom (the atom is not
    /// even in [`crate::atoms::names::ALL`]). Changing this takes a session
    /// restart. When the runtime surface lands (§6) it must be a **root-atom
    /// write**, never a bare `gamescopectl <convar>` — a value-less convar call
    /// RESETS the convar to its default and turns HDR off with no log line,
    /// which is how the phase-2 "feedback 0" was self-inflicted.
    pub hdr: bool,
    /// `--hdr-sdr-content-nits`: the shell's white point inside an HDR output.
    ///
    /// Consumed with [`Self::hdr`] (it is the second half of the same flag pair,
    /// exactly as the kit pairs them).
    ///
    /// **§6 leaves the right value OPEN** — it is an eyes-only criterion, and
    /// gamescope#1887 (SDR oversaturated on an HDR output) is the known risk for
    /// the shell's own colours. 200 is the only value ever run on this chain
    /// (`dev/gamescope/session.sh:51`), so it is the default here; it is not a
    /// settled answer, and this key exists so it can be moved without a rebuild.
    pub sdr_nits: u32,
    /// Whether VRR (`--adaptive-sync`) is on.
    ///
    /// **§13 Q11 IS OPEN, AND THIS KEY EXISTS SO THIS PR DOES NOT CLOSE IT.**
    /// §6 and the ops record both note OLED near-black flicker and AVR OSD
    /// problems with VRR on this exact chain and lean toward off; the week-long
    /// live measurement nonetheless ran with it ON (`TV_SHELL_GS_VRR` defaults
    /// to 1 in `dev/gamescope/session.sh:50`). The unit used to hard-code
    /// `--adaptive-sync` with no knob and no comment, which silently answered an
    /// open design question in the direction the ops record warns against.
    ///
    /// The default here matches the measured configuration, because that is the
    /// one we have evidence about. Turning it off is a one-line config change
    /// and a session restart. Q11 stays open.
    pub vrr: bool,
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
            // The kit's measured value, not a round number someone liked. It
            // was 400 here and 200 in the only invocation ever run on the
            // chain; §6 leaves the right answer to the eyes, so the default
            // should at least be a number that has been looked at.
            sdr_nits: 200,
            // The measured configuration. §13 Q11 is open — see the field docs.
            vrr: true,
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
    /// The app class the core starts and shows once, on a FRESH session.
    ///
    /// `0` means none, which is also the default: a core that launches something
    /// nobody configured is worse than one that launches nothing.
    ///
    /// **This is not "launch on start".** §5/§9 forbid the core writing the base
    /// layer at startup, because a core restart under a live game would yank the
    /// screen. The distinction is made by OBSERVATION, not by a flag: see
    /// [`crate::boot::decide`], which launches only when the reconcile shows an
    /// empty base layer *and* nothing on screen. Everything else — a populated
    /// list, an app already on screen, or a read that failed — is a session in
    /// use, and the core keeps its hands off it.
    ///
    /// Consumed by [`crate::boot`], which `main` runs after the IPC socket is
    /// listening.
    pub boot_app: u32,
    /// When the boot app exits, whether to start it again.
    ///
    /// `on-failure` (default), `always`, or `never`. See [`RelaunchPolicy`] for
    /// why the default is not `always` even though the prototype's supervisor
    /// relaunches unconditionally.
    pub boot_relaunch: RelaunchPolicy,
    /// An exit sooner than this after launch counts as a FAST exit.
    ///
    /// The prototype's `FAST_EXIT_SECS`, chosen against this hardware
    /// (`dev/gamescope/client.sh`).
    pub boot_fast_exit_secs: u64,
    /// How many fast exits in a row before the retry interval stretches to
    /// [`Self::boot_backoff_secs`]. The prototype's `FAST_EXIT_LIMIT`.
    pub boot_fast_exit_limit: u32,
    /// The stretched retry interval once the fast-exit limit is hit.
    /// The prototype's `BACKOFF_SECS`.
    pub boot_backoff_secs: u64,
    /// The ordinary retry interval, before any backoff.
    pub boot_relaunch_delay_secs: u64,
    /// Give up after this many consecutive fast exits. `0` = never give up.
    ///
    /// **Defaults to 0, deliberately.** A permanent give-up on an appliance
    /// guarantees a black television until a human intervenes, while a 60 s
    /// backoff costs nothing and recovers by itself the moment someone fixes the
    /// runtime — which is exactly the prototype's reasoning ("it never stops
    /// relaunching: a fixed runtime is picked up on the next attempt"). The key
    /// exists so a deployment that would rather fail loudly can choose to.
    pub boot_give_up_after: u32,
}

/// What the boot supervisor does when the app exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RelaunchPolicy {
    /// Relaunch only when the app exited NON-ZERO or on a signal. **Default.**
    ///
    /// The prototype relaunches unconditionally, and was right to: it *was* the
    /// session, so a quit left a black screen. v2 has a shell behind the app, so
    /// a clean exit has somewhere to land — and relaunching over a deliberate
    /// quit would fight the user, who pressed Quit and expects the shell.
    /// A crash is the case durability is actually about, and a crash is not a
    /// clean exit.
    #[default]
    OnFailure,
    /// Relaunch on ANY exit — the prototype's behaviour, for a deployment that
    /// wants the app to be the session.
    Always,
    /// Never relaunch. The boot client becomes launch-once.
    Never,
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
            // None. A default that started an app would make an all-defaults
            // config (a missing file) take over the television.
            boot_app: 0,
            boot_relaunch: RelaunchPolicy::OnFailure,
            // The prototype's measured constants (dev/gamescope/client.sh:211).
            boot_fast_exit_secs: 10,
            boot_fast_exit_limit: 3,
            boot_backoff_secs: 60,
            boot_relaunch_delay_secs: 2,
            // Never give up — see the field docs.
            boot_give_up_after: 0,
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

    /// The `[[app]]` entry for `app_id`, if one is configured.
    ///
    /// Linear over a handful of entries — a map would be a second
    /// representation of the same list to keep in sync, and `validate()` already
    /// refuses duplicate ids, which is the only thing an index would buy.
    pub fn app_class(&self, app_id: AppId) -> Option<&AppConfig> {
        self.app.iter().find(|a| a.id == app_id.get())
    }

    /// The configured boot app class, or `None` when `boot_app = 0`.
    ///
    /// Returning `Option` rather than a raw `0` sentinel at every call site is
    /// the point: "no boot app" is then a case the caller must handle, not a
    /// magic number it can forget to compare against.
    pub fn boot_app(&self) -> Option<AppId> {
        match self.session.boot_app {
            0 => None,
            id => Some(AppId::new(id)),
        }
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
    /// Two of the four values are ARGUMENT LISTS rather than scalars
    /// (`TV_SHELL_GS_HDR_ARGS`, `TV_SHELL_GS_VRR_ARGS`), because a flag that is
    /// sometimes absent cannot be expressed by substituting a value. systemd
    /// splits an unquoted `$VAR` into words and an empty one into no words at
    /// all, so `$TV_SHELL_GS_VRR_ARGS` in the `ExecStart` is exactly the kit's
    /// `if [ "$VRR" = 1 ]; then ARGS+=(--adaptive-sync); fi`. Every other value
    /// stays a bare integer and is substituted as `${VAR}` (no splitting).
    pub fn session_env(&self) -> String {
        let hdr_args = if self.display.hdr {
            format!(
                "--hdr-enabled --hdr-sdr-content-nits {}",
                self.display.sdr_nits
            )
        } else {
            String::new()
        };
        let vrr_args = if self.display.vrr {
            "--adaptive-sync"
        } else {
            ""
        };
        format!(
            "{ENV_WIDTH}={}\n\
             {ENV_HEIGHT}={}\n\
             {ENV_REFRESH}={}\n\
             {ENV_XWAYLAND_COUNT}={}\n\
             {ENV_HDR_ARGS}={hdr_args}\n\
             {ENV_VRR_ARGS}={vrr_args}\n",
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

        // -- [[app]] -------------------------------------------------------
        //
        // Every check here is a failure that would otherwise surface at launch
        // time, on the couch, as something that reads like a compositor problem.
        let mut seen: Vec<u32> = Vec::new();
        for app in &self.app {
            if app.id == 0 {
                anyhow::bail!(
                    "config: [[app]] id must be non-zero; 0 is gamescope's \"no app\" value \
                     and could never be focused"
                );
            }
            if app.id == STEAM_CLIENT_APP_ID {
                anyhow::bail!(
                    "config: [[app]] id must not be {STEAM_CLIENT_APP_ID}; that id belongs to \
                     the Steam client under gamescope's --steam focus policy (V2_DESIGN §5), \
                     and Steam rewrites the base layer for it"
                );
            }
            if app.id == self.session.shell_app_id {
                anyhow::bail!(
                    "config: [[app]] id {} is also [session] shell_app_id; an app class \
                     sharing the shell's id would make `show` ambiguous between them",
                    app.id
                );
            }
            if seen.contains(&app.id) {
                anyhow::bail!(
                    "config: two [[app]] entries share id {}; a lookup would silently take \
                     the first and the second would never launch",
                    app.id
                );
            }
            seen.push(app.id);
            if app.command.is_empty() {
                anyhow::bail!(
                    "config: [[app]] id {} has an empty command; there would be nothing to exec",
                    app.id
                );
            }
            if app.command[0].trim().is_empty() {
                anyhow::bail!(
                    "config: [[app]] id {} has an empty program name in command[0]",
                    app.id
                );
            }
            // An `=` or an empty name in an env key is not something the child
            // process could ever observe correctly — `Command::env` would either
            // panic or produce a variable nothing can read. Refuse it here,
            // where the message can name the app.
            for name in app.env.keys().chain(app.env_unset.iter()) {
                if name.is_empty() || name.contains('=') || name.contains('\0') {
                    anyhow::bail!(
                        "config: [[app]] id {} has an invalid environment variable name {name:?}; \
                         a name may not be empty or contain '=' or NUL",
                        app.id
                    );
                }
            }
        }

        // A boot app naming a class that does not exist is the one config error
        // whose symptom is a BLACK TELEVISION at the end of a boot, with the
        // core otherwise healthy. Catch it at startup, where it is one line.
        if let Some(boot) = self.boot_app() {
            if self.app_class(boot).is_none() {
                anyhow::bail!(
                    "config: [session] boot_app = {} names no [[app]] entry; the core would \
                     come up with nothing to show. Add an [[app]] with id = {} or set \
                     boot_app = 0",
                    boot.get(),
                    boot.get()
                );
            }
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
/// `--hdr-enabled --hdr-sdr-content-nits <n>`, or empty. An ARGUMENT LIST, not a
/// value: the unit substitutes it unquoted so systemd word-splits it, which is
/// how a sometimes-absent flag is expressed at all.
pub const ENV_HDR_ARGS: &str = "TV_SHELL_GS_HDR_ARGS";
/// `--adaptive-sync`, or empty. See [`ENV_HDR_ARGS`].
pub const ENV_VRR_ARGS: &str = "TV_SHELL_GS_VRR_ARGS";

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
    /// Just the unit's `ExecStart=`, joined into one line.
    fn gamescope_exec_start() -> String {
        let unit = gamescope_unit();
        unit.lines()
            .skip_while(|l| !l.starts_with("ExecStart="))
            .take_while(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every flag the measured kit passes unconditionally
    /// (`dev/gamescope/session.sh`'s `ARGS` array).
    ///
    /// The rule this pins: **a unit may not silently drop a flag the only
    /// measured invocation carries.** Each of these was absent at some point and
    /// each absence had a symptom nobody would trace back to a missing flag —
    /// an un-pinned backend, a shell upscaled from a nested size that was never
    /// set, a mouse cursor parked on a 10-foot UI, and (the bad one)
    /// `--keep-alive`, without which a shell crash takes the compositor, the
    /// session and any running game with it.
    const KIT_UNCONDITIONAL_FLAGS: &[&str] = &[
        "--backend",
        "--steam",
        "--keep-alive",
        "-W",
        "-H",
        "-r",
        "-w",
        "-h",
        "--stats-path",
        "--hide-cursor-delay",
    ];

    #[test]
    fn the_unit_carries_every_flag_the_measured_session_carries() {
        let exec = gamescope_exec_start();
        let tokens: Vec<&str> = exec.split_whitespace().collect();
        for flag in KIT_UNCONDITIONAL_FLAGS {
            assert!(
                tokens.contains(flag),
                "`{flag}` is in dev/gamescope/session.sh's ARGS — the only gamescope \
                 invocation this project has measured — and is missing from the unit. \
                 Either carry it or say in the unit why it differs. ExecStart: {exec}"
            );
        }
    }

    #[test]
    fn every_flag_the_kit_makes_configurable_is_configurable_or_explained() {
        // THE CLASS: the kit gated a flag on an env var BECAUSE the answer was
        // open. A unit that hard-codes one answer without saying it is answering
        // anything closes a design question in silence.
        let unit = gamescope_unit();
        let exec = gamescope_exec_start();

        // HDR and VRR are config-driven, as argument lists (a sometimes-absent
        // flag cannot be expressed by substituting a value).
        for var in [ENV_HDR_ARGS, ENV_VRR_ARGS] {
            assert!(
                exec.contains(&format!("${var}")),
                "{var} must be substituted UNQUOTED so systemd word-splits it: {exec}"
            );
            assert!(
                !exec.contains(&format!("${{{var}}}")),
                "${{{var}}} is the no-split form, which would pass the whole list as one \
                 argument (and an empty one as an empty argument): {exec}"
            );
        }
        for hardcoded in ["--hdr-enabled", "--adaptive-sync", "--hdr-sdr-content-nits"] {
            assert!(
                !exec.split_whitespace().any(|t| t == hardcoded),
                "`{hardcoded}` is hard-coded in the ExecStart, so the config key that \
                 gates it changes nothing: {exec}"
            );
        }
        // `--expose-wayland` IS hard-coded, and that is allowed only because the
        // unit names the decision it rests on. §11 settled it; §13 Q11 did not
        // settle VRR, which is why that one is a config key instead.
        assert!(
            exec.split_whitespace().any(|t| t == "--expose-wayland"),
            "{exec}"
        );
        assert!(
            unit.contains("§11"),
            "a hard-coded flag the kit makes configurable must cite the decision that \
             closed the question"
        );
        assert!(
            unit.contains("Q11"),
            "the unit must record that VRR is an OPEN question (§13 Q11) rather than \
             leaving a reader to assume the default was reasoned about"
        );
    }

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
        assert_eq!(lines.len(), 6, "{env:?}");
        for l in &lines {
            let (k, v) = l
                .split_once('=')
                .unwrap_or_else(|| panic!("not KEY=value: {l:?}"));
            assert!(!k.is_empty(), "{l:?}");
            if k.ends_with("_ARGS") {
                // An argument list: flags and bare integers, no quoting. A value
                // needing quotes would drag systemd's word-splitting rules into
                // the ExecStart, where they are much harder to reason about.
                assert!(
                    !v.contains(['"', '\'', '$', '\\']),
                    "an argument list must need no quoting: {l:?}"
                );
            } else {
                assert!(
                    !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()),
                    "a substituted value must be a bare integer: {l:?}"
                );
            }
        }
        assert!(
            env.ends_with('\n'),
            "a trailing newline or the last line is lost"
        );
    }

    #[test]
    fn hdr_and_vrr_render_as_present_or_absent_flags_not_as_values() {
        let on = CoreConfig::parse("[display]\nhdr = true\nvrr = true\nsdr_nits = 203\n")
            .unwrap()
            .session_env();
        assert!(
            on.contains(&format!(
                "{ENV_HDR_ARGS}=--hdr-enabled --hdr-sdr-content-nits 203"
            )),
            "{on}"
        );
        assert!(
            on.contains(&format!("{ENV_VRR_ARGS}=--adaptive-sync")),
            "{on}"
        );

        // Off must render EMPTY, not `--adaptive-sync=0` or a `0` argument:
        // systemd splits an empty $VAR into no words at all, which is the only
        // way to express "do not pass this flag".
        let off = CoreConfig::parse("[display]\nhdr = false\nvrr = false\n")
            .unwrap()
            .session_env();
        assert!(off.contains(&format!("{ENV_HDR_ARGS}=\n")), "{off}");
        assert!(off.contains(&format!("{ENV_VRR_ARGS}=\n")), "{off}");
        assert!(!off.contains("--hdr-enabled"), "{off}");
        assert!(!off.contains("--adaptive-sync"), "{off}");
    }

    #[test]
    fn the_sdr_nits_default_is_the_value_that_was_actually_measured() {
        // §6 leaves this to the eyes; 200 is the only value ever run on the
        // chain (dev/gamescope/session.sh:51). It was 400 here — a number
        // nobody had looked at through the television.
        assert_eq!(DisplayConfig::default().sdr_nits, 200);
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
            // Read by nothing yet; lands with the HDR-aware launch path (§6).
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
            // Read by `crate::boot` via `CoreConfig::boot_app()`, which `main`
            // acts on after the socket is listening.
            "session.boot_app",
            // Read by `boot::RestartPolicy::from_config`, which `boot::supervise`
            // acts on after every exit of the boot app.
            "session.boot_relaunch",
            "session.boot_fast_exit_secs",
            "session.boot_fast_exit_limit",
            "session.boot_backoff_secs",
            "session.boot_relaunch_delay_secs",
            "session.boot_give_up_after",
            "display.width",
            "display.height",
            "display.refresh",
            "display.hdr",
            "display.sdr_nits",
            "display.vrr",
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
            // The `[[app]]` table is a LIST, not a scalar key, so it is not part
            // of the scalar classification below — a per-entry field cannot be
            // named `table.field` or round-tripped as `[table]\nfield = 1`. It is
            // classified by `every_app_class_field_is_consumed` instead, which
            // destructures `AppConfig` exhaustively for exactly the same reason:
            // adding a field there stops THAT test compiling.
            app: _,
        } = CoreConfig::default();
        let DisplayConfig {
            width,
            height,
            refresh,
            hdr,
            sdr_nits,
            vrr,
            hotplug_settle_ms,
        } = display;
        let SessionConfig {
            shell_app_id,
            xwayland_count,
            switch_timeout_ms,
            map_timeout_ms,
            launch_confirm_ms,
            boot_app,
            boot_relaunch,
            boot_fast_exit_secs,
            boot_fast_exit_limit,
            boot_backoff_secs,
            boot_relaunch_delay_secs,
            boot_give_up_after,
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
            u64::from(vrr),
            hotplug_settle_ms,
            u64::from(shell_app_id),
            u64::from(xwayland_count),
            switch_timeout_ms,
            map_timeout_ms,
            launch_confirm_ms,
            u64::from(boot_app),
            // The supervisor's tunables. `boot_relaunch` is an enum, so it is
            // counted via its discriminant rather than a numeric cast.
            boot_relaunch as u64,
            boot_fast_exit_secs,
            u64::from(boot_fast_exit_limit),
            boot_backoff_secs,
            boot_relaunch_delay_secs,
            u64::from(boot_give_up_after),
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
            let value = match name {
                "hdr" | "vrr" => "true",
                // An enum key needs one of its own variants, not an integer.
                "boot_relaunch" => "\"on-failure\"",
                _ => "1",
            };
            CoreConfig::parse(&format!("[{table}]\n{name} = {value}\n"))
                .unwrap_or_else(|e| panic!("{key} is not a real config key: {e}"));
        }
    }

    /// **THE DEPLOYMENT QUESTION: does the core.toml already on the box still
    /// load once new keys exist?**
    ///
    /// `deny_unknown_fields` cuts both ways and the answer is not symmetric, so
    /// it is pinned here rather than reasoned about before a reboot:
    ///
    /// * A file MISSING new keys loads fine — every struct is `#[serde(default)]`,
    ///   so an absent key takes its default. There is no migration step and
    ///   nothing to run: an existing file is read as-is.
    /// * A file carrying an UNKNOWN key is REFUSED, loudly, naming the key.
    ///   That is the half `deny_unknown_fields` buys, and it is why a typo can
    ///   never silently run a default.
    ///
    /// So the file the installer seeded on 2026-09-06 keeps working untouched:
    /// it gains `boot_app = 0` (no boot client) and the prototype's restart
    /// constants, and changes behaviour only when someone adds `[[app]]` and a
    /// `boot_app` deliberately.
    #[test]
    fn an_existing_config_without_the_new_keys_still_loads() {
        // Exactly what `scripts/install-v2.sh` seeds, minus everything added
        // since: the file as it exists on the box today.
        let old = "\
[display]\n\
width = 3840\n\
height = 2160\n\
refresh = 120\n\
hdr = true\n\
\n\
[session]\n\
shell_app_id = 9001\n\
xwayland_count = 2\n\
";
        let c = CoreConfig::parse(old).expect("an older core.toml must still parse");
        c.validate().expect("and must still validate");

        // The new keys are present as defaults, and the defaults are inert.
        assert_eq!(c.boot_app(), None, "no boot client without an explicit key");
        assert!(c.app.is_empty(), "no app classes without explicit entries");
        assert_eq!(c.session.boot_relaunch, RelaunchPolicy::OnFailure);
        assert_eq!(c.session.boot_fast_exit_secs, 10);

        // And the values the operator DID set survive — a default must never
        // overwrite a written value.
        assert_eq!(c.display.width, 3840);
        assert_eq!(c.session.shell_app_id, 9001);
    }

    /// The other half: an unknown key is refused by name, not ignored.
    #[test]
    fn an_unknown_key_is_refused_and_named() {
        let err = CoreConfig::parse("[session]\nboot_ap = 9003\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("boot_ap"),
            "the error must name the key: {err}"
        );
    }

    /// A `boot_relaunch` typo is refused with the accepted values in reach,
    /// because it is the one new key whose value is a closed vocabulary.
    #[test]
    fn a_bad_relaunch_policy_is_refused() {
        assert!(CoreConfig::parse("[session]\nboot_relaunch = \"sometimes\"\n").is_err());
        for good in ["on-failure", "always", "never"] {
            CoreConfig::parse(&format!("[session]\nboot_relaunch = \"{good}\"\n"))
                .unwrap_or_else(|e| panic!("{good} must parse: {e}"));
        }
    }

    // -- the [[app]] class table ---------------------------------------------

    /// The stanza `config/core.toml.example` ships, parsed for real.
    const MOONLIGHT_STANZA: &str = r#"
[session]
boot_app = 9003

[[app]]
id = 9003
command = ["/usr/bin/moonlight"]
env_unset = ["WAYLAND_DISPLAY"]

[app.env]
QT_QPA_PLATFORM = "xcb"
SDL_VIDEODRIVER = "x11"
ENABLE_GAMESCOPE_WSI = "1"
"#;

    #[test]
    fn the_shipped_moonlight_class_parses_and_validates() {
        let c = CoreConfig::parse(MOONLIGHT_STANZA).expect("the example stanza must parse");
        c.validate().expect("and must validate");

        let class = c
            .app_class(AppId::new(9003))
            .expect("app_class finds the entry");
        assert_eq!(class.command, ["/usr/bin/moonlight".to_string()]);
        // THE MEASURED FIX, as data: three sets and one REMOVAL.
        assert_eq!(
            class.env.get("QT_QPA_PLATFORM").map(String::as_str),
            Some("xcb")
        );
        assert_eq!(
            class.env.get("SDL_VIDEODRIVER").map(String::as_str),
            Some("x11")
        );
        assert_eq!(
            class.env.get("ENABLE_GAMESCOPE_WSI").map(String::as_str),
            Some("1")
        );
        assert_eq!(class.env_unset, ["WAYLAND_DISPLAY".to_string()]);

        assert_eq!(c.boot_app(), Some(AppId::new(9003)));
    }

    #[test]
    fn an_absent_boot_app_is_none_not_zero() {
        let c = CoreConfig::default();
        assert_eq!(c.boot_app(), None);
        assert!(c.app.is_empty());
        c.validate().unwrap();
    }

    #[test]
    fn app_class_misses_cleanly_for_an_unconfigured_id() {
        let c = CoreConfig::parse(MOONLIGHT_STANZA).unwrap();
        assert!(c.app_class(AppId::new(4242)).is_none());
    }

    /// A boot app naming no class is the config error whose symptom is a black
    /// television at the end of a boot with a healthy-looking core.
    ///
    /// Mutation-check: delete the `boot_app` arm of `validate` and this goes red.
    #[test]
    fn a_boot_app_with_no_class_is_refused() {
        let c = CoreConfig::parse("[session]\nboot_app = 9003\n").unwrap();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("boot_app"), "{err}");
        assert!(err.contains("9003"), "{err}");
    }

    #[test]
    fn a_malformed_app_entry_is_refused_with_its_id() {
        // Each of these is a launch-time failure moved to startup.
        for (text, needle) in [
            ("[[app]]\nid = 0\ncommand = [\"x\"]\n", "non-zero"),
            ("[[app]]\nid = 769\ncommand = [\"x\"]\n", "769"),
            ("[[app]]\nid = 9001\ncommand = [\"x\"]\n", "shell_app_id"),
            ("[[app]]\nid = 9003\ncommand = []\n", "empty command"),
            (
                "[[app]]\nid = 9003\ncommand = [\"  \"]\n",
                "empty program name",
            ),
            (
                "[[app]]\nid = 9003\ncommand = [\"x\"]\nenv_unset = [\"BAD=NAME\"]\n",
                "environment variable name",
            ),
            (
                "[[app]]\nid = 9003\ncommand = [\"x\"]\n\n[[app]]\nid = 9003\ncommand = [\"y\"]\n",
                "share id",
            ),
        ] {
            let c = CoreConfig::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
            let err = c.validate().unwrap_err().to_string();
            assert!(
                err.contains(needle),
                "expected {needle:?} in the error for {text:?}, got: {err}"
            );
        }
    }

    /// `id` and `command` are REQUIRED — a class missing either is a parse
    /// error, not a default. There is no sensible default for "which app".
    #[test]
    fn an_app_entry_without_an_id_or_command_does_not_parse() {
        assert!(CoreConfig::parse("[[app]]\ncommand = [\"x\"]\n").is_err());
        assert!(CoreConfig::parse("[[app]]\nid = 9003\n").is_err());
        // And an unknown key is still refused, like every other table.
        assert!(CoreConfig::parse("[[app]]\nid = 9003\ncommand = [\"x\"]\nwat = 1\n").is_err());
    }

    /// The `AppConfig` counterpart of
    /// `every_key_is_either_consumed_or_declared_unconsumed`: an array-of-tables
    /// entry cannot be named `table.field`, so it is classified here — and the
    /// exhaustive destructure means ADDING A FIELD STOPS THIS COMPILING until
    /// someone says who reads it.
    ///
    /// §12 also lists an id strategy, an input contract and an HDR expectation
    /// for an app class. None is modelled: the id strategy is fixed at "scope,
    /// tag as repair" and not selectable, §7's input layer does not exist, and
    /// the HDR settle key is itself still unconsumed. Three more keys whose
    /// stated consumer does not exist is the #416 class; they land with readers.
    #[test]
    fn every_app_class_field_is_consumed() {
        let class = CoreConfig::parse(MOONLIGHT_STANZA)
            .unwrap()
            .app
            .into_iter()
            .next()
            .unwrap();
        let AppConfig {
            id,
            command,
            env,
            env_unset,
        } = class;
        // Each is read on the launch path: `id` by `app_class`, `command` and
        // both env halves by `compositor::resolve_launch` into
        // `launch::prepare_command`.
        assert_eq!(id, 9003);
        assert!(!command.is_empty());
        assert!(!env.is_empty());
        assert!(!env_unset.is_empty());
    }

    // -- the install link ----------------------------------------------------
    //
    // §11's "beside, not instead, at every shared layer" was a rule that nothing
    // checked, and the units shipped a hard-coded `/opt/tv-shell` — v1's prefix —
    // under a comment claiming an installer rewrote it. No installer knew the
    // units existed.
    //
    // These tests run the REAL `scripts/install-v2.sh` into a scratch tree and
    // assert on the files it actually writes. Re-implementing its substitution
    // in the test would be the #416 shape again: a property confirmed only by
    // the code that already believes it.

    use std::process::Command;

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("core/ is a workspace member, so it has a parent")
            .to_path_buf()
    }

    /// v1's session entry, written by `scripts/install.sh`. Reusing it would
    /// replace the rollback path §11 depends on.
    const V1_SESSION_FILE: &str = "tv-shell-wayland.desktop";
    /// The Ansible-owned gamescope measurement prototype's entry
    /// (`dev/gamescope/README.md`) — the §10 regression bench. V2_DESIGN §4 named
    /// this file for v2 before the prototype claimed it.
    const PROTOTYPE_SESSION_FILE: &str = "tv-shell-gamescope.desktop";
    /// The third name, which is v2's.
    const V2_SESSION_FILE: &str = "tv-shell-v2.desktop";
    /// The stand-in the committed units carry instead of an absolute path.
    const PREFIX_TOKEN: &str = "@TV_SHELL_V2_PREFIX@";
    /// v1's install prefix. A v2 unit naming a path under it would run v1's tree.
    const V1_PREFIX: &str = "/opt/tv-shell";

    struct Staged {
        root: std::path::PathBuf,
        prefix: std::path::PathBuf,
        units: std::path::PathBuf,
        sessions: std::path::PathBuf,
        config: std::path::PathBuf,
    }

    impl Drop for Staged {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Run `scripts/install-v2.sh --no-build` into a scratch tree.
    ///
    /// `--no-build` is not a shortcut: a `cargo build` from inside `cargo test`
    /// would block on the build lock this very test run holds. Every path this
    /// exercises is downstream of the binary, not of building it.
    ///
    /// `--user` is passed explicitly because the installer refuses an IMPLICIT
    /// root user (the root-owned-install footgun), and CI runs this in a
    /// root-only container.
    fn stage_install(tag: &str) -> Staged {
        stage_install_with(tag, &[])
    }

    /// `stage_install` plus extra installer arguments (e.g. `--no-session`).
    fn stage_install_with(tag: &str, extra: &[&str]) -> Staged {
        // Under `target/`, NOT `std::env::temp_dir()`. /tmp is shared with every
        // other job on the box and this went intermittently `Permission denied`
        // there; a scratch tree under the build directory is private to the
        // checkout, gitignored, and cleaned by `cargo clean`.
        let root = repo_root()
            .join("target")
            .join(format!("install-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Staged {
            prefix: root.join("prefix"),
            units: root.join("units"),
            sessions: root.join("sessions"),
            config: root.join("config"),
            root,
        };
        let user = String::from_utf8(
            Command::new("id")
                .arg("-un")
                .output()
                .expect("running id -un")
                .stdout,
        )
        .expect("id -un is utf-8");

        let out = Command::new("bash")
            .arg(repo_root().join("scripts/install-v2.sh"))
            .arg("--no-build")
            .args(["--user", user.trim()])
            .arg("--prefix")
            .arg(&s.prefix)
            .arg("--unit-dir")
            .arg(&s.units)
            .arg("--session-dir")
            .arg(&s.sessions)
            .arg("--config-dir")
            .arg(&s.config)
            .args(extra)
            .output()
            .expect("running scripts/install-v2.sh");
        assert!(
            out.status.success(),
            "install-v2.sh failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        s
    }

    /// The v2 unit files as installed, name and content, sorted by name.
    fn installed_units(s: &Staged) -> Vec<(String, String)> {
        let mut v: Vec<_> = std::fs::read_dir(&s.units)
            .expect("the installer created the unit dir")
            .map(|e| {
                let p = e.unwrap().path();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read_to_string(&p).unwrap(),
                )
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Every non-comment line of a unit, so a comment discussing v1's prefix
    /// (several do, at length) is not mistaken for a directive naming it.
    fn directive_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
        text.lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with('#'))
    }

    #[test]
    fn the_committed_units_name_no_absolute_install_path() {
        // The bug this pins: the units shipped `/opt/tv-shell/bin/...` while
        // claiming an installer rewrote it. CLAUDE.md forbids the hardcode and
        // §11 forbids the *prefix* — and the second is the one that would have
        // silently run v1's tree out of a v2 unit.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("units");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in directive_lines(&text) {
                assert!(
                    !line.contains(&format!("{V1_PREFIX}/")),
                    "{name}:{} names a path under v1's prefix: {line}",
                    n + 1
                );
            }
        }
        // And the two that need a prefix say so with the token, so the installer
        // has something to substitute and a hand-copied unit fails loudly.
        for (unit, needle) in [
            (
                "tv-shell-core.service",
                "ExecStart=@TV_SHELL_V2_PREFIX@/bin/tv-shell-core",
            ),
            (
                "tv-shell-gamescope.service",
                "@TV_SHELL_V2_PREFIX@/bin/tv-shell-gamescope-child.sh",
            ),
        ] {
            let text = std::fs::read_to_string(dir.join(unit)).unwrap();
            assert!(text.contains(needle), "{unit} must carry `{needle}`");
        }
    }

    #[test]
    fn the_installed_units_carry_no_token_and_no_v1_path() {
        let s = stage_install("units");
        let units = installed_units(&s);
        assert_eq!(units.len(), 3, "expected three v2 units, got {units:?}");
        for (name, text) in &units {
            assert!(
                !text.contains(PREFIX_TOKEN),
                "{name} still carries {PREFIX_TOKEN} after install — systemd would exec a path that does not exist"
            );
            for (n, line) in directive_lines(text) {
                assert!(
                    !line.contains(&format!("{V1_PREFIX}/")),
                    "{name}:{} names a path under v1's prefix after install: {line}",
                    n + 1
                );
            }
        }
    }

    #[test]
    fn the_rewritten_exec_paths_point_at_the_resolved_prefix() {
        let s = stage_install("exec");
        let prefix = s.prefix.display().to_string();

        let core = std::fs::read_to_string(s.units.join("tv-shell-core.service")).unwrap();
        assert!(
            core.contains(&format!("ExecStart={prefix}/bin/tv-shell-core\n")),
            "the core unit's ExecStart must be the resolved prefix's binary"
        );

        // The gamescope unit's prefix use is NOT on the `ExecStart=` line: it is
        // the child command at the end of a line-continued invocation, which is
        // exactly what a naive `/^ExecStart=/` rewrite (v1's shape) would miss.
        let gs = std::fs::read_to_string(s.units.join("tv-shell-gamescope.service")).unwrap();
        assert!(
            gs.contains(&format!("-- {prefix}/bin/tv-shell-gamescope-child.sh")),
            "the gamescope unit's child command must be the resolved prefix's script"
        );

        // And the files those paths name are actually there. The session script
        // resolves the core binary from its OWN directory, so this adjacency is
        // load-bearing, not incidental.
        for f in [
            "tv-shell-gamescope-child.sh",
            "tv-shell-gamescope-session.sh",
        ] {
            assert!(
                s.prefix.join("bin").join(f).is_file(),
                "{f} was not installed"
            );
        }

        let desktop = std::fs::read_to_string(s.sessions.join(V2_SESSION_FILE)).unwrap();
        assert!(
            desktop.contains(&format!("Exec={prefix}/bin/tv-shell-gamescope-session.sh")),
            "the session entry's Exec must be the resolved prefix's session script: {desktop}"
        );
        assert!(
            s.config.join("core.toml").is_file(),
            "core.toml was not seeded"
        );
    }

    #[test]
    fn the_v2_session_entry_collides_with_neither_v1_nor_the_prototype() {
        // Both other names are live: v1's is the §11 rollback the operator
        // selects when v2 misbehaves, and the prototype's is the §10 regression
        // bench. Overwriting either removes something someone still selects.
        assert_ne!(V2_SESSION_FILE, V1_SESSION_FILE);
        assert_ne!(V2_SESSION_FILE, PROTOTYPE_SESSION_FILE);

        let s = stage_install("session");
        let written: Vec<String> = std::fs::read_dir(&s.sessions)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            written,
            vec![V2_SESSION_FILE.to_string()],
            "the v2 installer must write exactly its own session entry"
        );

        // The committed reference copy in config/ carries the same name, so an
        // operator wiring the session by hand lands on the same file.
        assert!(
            repo_root().join("config").join(V2_SESSION_FILE).is_file(),
            "config/{V2_SESSION_FILE} must exist as the reference session entry"
        );
    }

    #[test]
    fn the_v2_unit_names_collide_with_none_of_v1s() {
        // §11: v1 and v2 share no unit name. v1's are the ones scripts/install.sh
        // installs, which are exactly the unit files committed in config/ — read
        // rather than listed, so a v1 unit added later cannot escape this.
        let v1: Vec<String> = std::fs::read_dir(repo_root().join("config"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".service") || n.ends_with(".target"))
            .collect();
        assert!(!v1.is_empty(), "expected v1 units in config/");

        let s = stage_install("names");
        for (name, _) in installed_units(&s) {
            assert!(
                !v1.contains(&name),
                "{name} is also a v1 unit name — a v2 target pulling it would start v1's process"
            );
        }
    }

    #[test]
    fn the_installer_refuses_v1s_prefix_however_it_is_spelled() {
        // The one refusal: a prefix at or under /opt/tv-shell would put v2's
        // binary and session script inside the tree the couch's v1 session runs
        // from — and as root, that overwrites a running appliance.
        //
        // Every spelling, because an EXACT STRING COMPARE was not enough and the
        // gap was silent: `/opt//tv-shell` sailed past it and only failed later
        // on permissions, which on a box that HAS /opt/tv-shell means it does not
        // fail at all. A path UNDER the prefix (`/opt/tv-shell/nested`) is the
        // same hazard and was equally unguarded. The script now normalises with
        // `realpath -m` and refuses the prefix or any descendant.
        for arg in [
            V1_PREFIX,
            "/opt/tv-shell/",
            "/opt//tv-shell",
            "/opt///tv-shell//",
            "/opt/tv-shell/nested",
            "/opt/tv-shell/./x",
            "/opt/tv-shell/../tv-shell",
        ] {
            let out = Command::new("bash")
                .arg(repo_root().join("scripts/install-v2.sh"))
                .args(["--no-build", "--prefix", arg])
                .output()
                .expect("running scripts/install-v2.sh");
            assert!(
                !out.status.success(),
                "installing to {arg} must fail, but it succeeded"
            );
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("refusing"),
                "{arg} must be refused BY THE GUARD, naming why — not by some later \
                 permission error that would not happen on a box that has v1 installed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // And a guard that refuses everything protects nothing: a normal prefix
        // must still be accepted. `stage_install` is that case end to end, so
        // this only has to be a prefix the guard sees and passes.
        let s = stage_install("accept");
        assert!(
            s.prefix.join("bin").is_dir(),
            "a prefix outside v1's tree must still install"
        );
    }

    #[test]
    fn no_session_suppresses_the_session_file_and_installs_everything_else() {
        // OWNERSHIP: on an Ansible-managed host, Ansible owns
        // /usr/share/wayland-sessions/tv-shell-v2.desktop and the installer is
        // run with --no-session. Two writers of one file is a config that
        // flip-flops per run, and Ansible's version is the capable one — only it
        // can render the session env into `Exec=` as a `/usr/bin/env` prefix
        // (there is no shell between a greeter and the session), and only its
        // toggle can REMOVE the entry. See the script header.
        let s = stage_install_with("nosession", &["--no-session"]);

        // Suppressed, not redirected: the directory is not even created. "Do not
        // write it" and "create it and write nothing" are different promises.
        assert!(
            !s.sessions.join(V2_SESSION_FILE).exists(),
            "--no-session still wrote the session entry"
        );
        assert!(
            !s.sessions.exists(),
            "--no-session created the session directory it does not own"
        );

        // And the flag must suppress ONLY that: a flag that quietly skipped the
        // rest would leave an Ansible-managed host with a session entry pointing
        // at nothing.
        assert_eq!(installed_units(&s).len(), 3, "the units must still install");
        assert!(
            s.prefix
                .join("bin")
                .join("tv-shell-gamescope-session.sh")
                .is_file(),
            "the launcher Ansible's Exec= points at must still install"
        );
        assert!(
            s.config.join("core.toml").is_file(),
            "core.toml must still be seeded"
        );
    }

    #[test]
    fn the_installers_help_prints_the_flags_and_no_code() {
        // `--help` is `sed -n '<range>p'` over the script's own header, so a
        // comment edit above it silently shifts the range — which already
        // happened once and leaked `set -euo pipefail` into the help output.
        let out = Command::new("bash")
            .arg(repo_root().join("scripts/install-v2.sh"))
            .arg("--help")
            .output()
            .expect("running scripts/install-v2.sh --help");
        assert!(out.status.success(), "--help must exit 0");
        let help = String::from_utf8_lossy(&out.stdout);
        for flag in [
            "--prefix",
            "--user",
            "--session-dir",
            "--session-exec",
            "--unit-dir",
            "--config-dir",
            "--no-build",
            "--no-session",
        ] {
            assert!(help.contains(flag), "--help omits {flag}:\n{help}");
        }
        assert!(
            !help.contains("set -euo pipefail"),
            "--help is printing script body, so its line range has drifted:\n{help}"
        );
    }
}

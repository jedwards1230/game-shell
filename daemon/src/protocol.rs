//! IPC wire protocol: parsing commands and rendering responses/events.
//!
//! The wire format is **newline-delimited bare UTF-8 text** in both directions
//! (see `docs/IPC_PROTOCOL.md`), NOT JSON — only the `get-bindings` *response*
//! body is a compact JSON object. The QML client talks to this exact format,
//! so every string here stayed byte-for-byte compatible with the former
//! `gamepad-input.py` (since deleted) — the QML client is unchanged.
//!
//! `Command`/`Event` are typed enums so the daemon's `match` arms are
//! compiler-checked exhaustive; the (de)serialization to/from legacy text lives
//! here rather than via `#[serde]` (serde-tagged JSON would change the wire and
//! break QML).

use std::fmt;

/// A command parsed from one inbound line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Grab,
    Release,
    Handoff,
    Status,
    Subscribe,
    GetBindings,
    SetBinding {
        action: String,
        button: String,
    },
    /// `set-binding` with the wrong number of arguments.
    SetBindingUsage,
    CaptureNext,
    CaptureCancel,
    /// `get-pads` — return the gamepad fleet as a compact JSON array, one object
    /// per connected pad (`{id,index,name,grabbed}`). Stable player indices
    /// (#101). Served by the input runtime (fleet state), so it round-trips the
    /// control channel like `status`.
    GetPads,
    /// `list-input-devices` — enumerate EVERY controller-like input device on
    /// the host (anything with `BTN_SOUTH` or a `js*` handler), including
    /// ungrabbed and virtual ones, as a compact JSON array. A diagnostics
    /// enumerator (replaces `ControllerSettings`' `/proc/bus/input/devices`
    /// reader), distinct from `get-pads` (which lists only the grabbed fleet).
    /// Served by the input runtime so it can mark which devices the fleet owns.
    /// Reply shape: `[{name,path,vendor,product,phys,handlers,grabbed}, …]`.
    ListInputDevices,
    /// `intent <name>` — inject a shell intent into the broadcast bus. `<name>`
    /// is validated against the closed vocabulary by the input runtime: a valid
    /// name re-broadcasts as `intent:<name>` and replies `ok`; an unknown name
    /// replies `error:unknown intent '<name>'`. This is the headless control
    /// surface for keyboard global-escape and automation (see
    /// `docs/IPC_PROTOCOL.md`). Pure broadcast — touches no device.
    Intent(String),
    /// `intent` with a missing/empty `<name>` body.
    IntentUsage,
    /// `rumble <id> <ms>` — fire a rumble (FF_RUMBLE) effect on the pad whose
    /// stable wire id is `<id>` for `<ms>` milliseconds. Served by the input
    /// runtime (Phase 5.5 ride-along, #99); a no-op for pads without `EV_FF`
    /// support or when the persisted `rumbleEnabled` setting is off. `<id>` is a
    /// single token (the wire id, which may itself contain `:`), `<ms>` a
    /// non-negative integer.
    Rumble {
        id: String,
        ms: u32,
    },
    /// `rumble` with a missing/incomplete `<id> <ms>` body, or a non-integer
    /// `<ms>`.
    RumbleUsage,
    /// `key <name>` — synthesize a single keystroke (press+release) on the
    /// shared virtual keyboard, the headless counterpart to a gamepad d-pad/A/B
    /// tap or a `wtype -k`. `<name>` is one token in the closed key vocabulary
    /// (`up`/`down`/`left`/`right`/`select`/`back`); the runtime maps it to a
    /// keycode (`config::key_for_action`) and rejects unknown names. Unlike
    /// `intent`, this **does** touch the device — it is the socket nav surface
    /// for automation/screenshot tours, kept distinct from the pure-broadcast
    /// `intent` control surface.
    Key(String),
    /// `key` with a missing/empty `<name>` body.
    KeyUsage,
    /// Scan installed `.desktop` apps; reply is a compact JSON array.
    /// Stateless (no input-runtime round-trip).
    ListApps,
    /// Return the full settings document as a compact JSON object.
    GetConfig,
    /// Merge a compact-JSON object of settings updates (read-modify-write,
    /// preserving foreign keys). The body is the raw JSON text after the
    /// command word.
    SetConfig(String),
    /// `set-config` with a missing/empty body.
    SetConfigUsage,
    /// List the web-app registry; reply is a compact JSON array of
    /// `{id,name,url,wmClass}` objects (#187).
    WebappList,
    /// Register a web app. Body is a compact-JSON `{"name":…,"url":…}` object;
    /// the daemon allocates the id/wmClass and generates the `.desktop`.
    WebappAdd(String),
    /// `webapp-add` with a missing/empty body.
    WebappAddUsage,
    /// Remove a web app by id, deleting its generated `.desktop`.
    WebappRemove(String),
    /// `webapp-remove` with a missing/empty id.
    WebappRemoveUsage,
    /// Record an app launch into recents.json. The body is the raw JSON text
    /// (a `{name,exec,comment}` object) after the command word.
    RecordLaunch(String),
    /// `record-launch` with a missing/empty body.
    RecordLaunchUsage,
    /// Return recent launches as a compact JSON array.
    GetRecents,
    /// Return notification history as a compact JSON array.
    GetNotifications,
    /// Record a notification into notifications.json. The body is the raw JSON text
    /// (a `{id,title,message,level,source,icon}` object) after the command word.
    RecordNotification(String),
    /// `record-notification` with a missing/empty body.
    RecordNotificationUsage,
    /// Overwrite the notifications list entirely. The body is a compact JSON array
    /// of notification objects after the command word. Used for clears/removals.
    SetNotifications(String),
    /// `set-notifications` with a missing/empty body.
    SetNotificationsUsage,

    // --- Phase 3: Bluetooth (bluer / BlueZ) ---
    /// Adapter power state -> `bt:on` / `bt:off` / `error:*`.
    BtPowerStatus,
    /// Power the default adapter on.
    BtPowerOn,
    /// Power the default adapter off.
    BtPowerOff,
    /// Start discovery; results arrive asynchronously as `bt:device` events.
    BtScanOn,
    /// Stop discovery.
    BtScanOff,
    /// List known devices as a compact JSON array.
    BtList,
    /// Connect to a device by MAC address.
    BtConnect(String),
    /// Disconnect a device by MAC address.
    BtDisconnect(String),
    /// Pair a device by MAC address (just-works via BlueZ default agent).
    BtPair(String),
    /// Trust a device by MAC address.
    BtTrust(String),
    /// A `bt-connect`/`bt-disconnect`/`bt-pair`/`bt-trust` with a missing MAC.
    /// `which` is the bare command word (e.g. `bt-connect`) for the usage line.
    BtMacUsage(&'static str),

    // --- Phase 3: Network READ (zbus / NetworkManager) ---
    /// Connectivity + primary connection as a compact JSON object.
    NetStatus,
    /// Visible Wi-Fi access points as a compact JSON array.
    NetWifiList,
    /// Trigger a Wi-Fi rescan (`RequestScan`).
    NetWifiRescan,
    /// `net-throughput <iface>` -> compact JSON object
    /// `{iface,rxBytes,txBytes}` of the interface's cumulative byte counters
    /// (from `/sys/class/net/<iface>/statistics`). Raw counters, not a rate —
    /// the caller computes the delta. Stateless; not routed through the NM actor.
    /// An error (unknown iface, non-Linux) degrades to
    /// `{iface,rxBytes:0,txBytes:0,error:"…"}` rather than a protocol error.
    NetThroughput {
        iface: String,
    },
    /// `net-throughput` with a missing `<iface>` body.
    NetThroughputUsage,
    /// `net-ping <host> [count]` -> compact JSON object
    /// `{host,reachable,rttMs}` from a bounded `ping`. `rttMs` is the average
    /// RTT when reachable, JSON `null` otherwise. Stateless + cross-platform
    /// (subprocess), served like `sunshine-status`. Fail-soft: an unreachable
    /// host is `reachable:false`, never a protocol error.
    NetPing {
        host: String,
        count: u32,
    },
    /// `net-ping` with a missing `<host>` body.
    NetPingUsage,

    // --- Phase 3: Power / idle (zbus / logind + UPower) ---
    /// Whether the system can suspend -> `yes` / `no` / `error:*`.
    PowerCanSuspend,
    /// Suspend the system (logind `Suspend(false)`).
    PowerSuspend,
    /// Battery state as a compact JSON object (`{"present":false}` on a desktop).
    PowerBattery,

    // --- Phase 4: Hyprland (direct IPC sockets) ---
    /// Active window as a compact JSON object `{class,title,address}` (`{}` if
    /// none). Replaces the `hyprctl`-based active-window read in QML.
    HyprActive,
    /// All Hyprland clients as a compact JSON array (mirrors `hyprctl clients -j`:
    /// at least `class,title,address,workspace`). Replaces the QML
    /// `hyprctl clients -j` shell-out.
    HyprClients,
    /// All monitors as a compact JSON array, including HDR-relevant fields
    /// (currentFormat + derived hdr bool). Replaces the QML hyprctl monitors -j
    /// READ.
    HyprMonitors,

    // --- Phase 4: Sunshine session detection (reqwest) ---
    /// `sunshine-status <host> <port>` -> compact JSON object
    /// `{online,paired,currentApp,httpsPort}` parsed from Sunshine's
    /// `/serverinfo` response. The body is `<host> <port>` (two whitespace-
    /// separated tokens) after the command word. Stateless (cross-platform).
    SunshineStatus {
        host: String,
        port: String,
    },
    /// `sunshine-status` with a missing/incomplete `<host> <port>` body.
    SunshineStatusUsage,

    /// `wol <host>` -> send a Wake-on-LAN magic packet to `<host>`. The body is
    /// a single token (the IP/hostname the shell uses; QML passes the configured
    /// streaming-target host). The host→MAC mapping is resolved from the kernel
    /// neighbor table (`ip neigh`) and a persisted MAC cache, so wake works even
    /// when the host is already asleep (its ARP entry is STALE/absent). Replies a
    /// compact-JSON object `{"status":"ok","mac":"…"}` on success or
    /// `{"status":"error","reason":"…"}` when the MAC can't be resolved. Stateless
    /// + cross-platform (UDP broadcast), served like `sunshine-status`.
    Wol {
        host: String,
    },
    /// `wol` with a missing/empty `<host>` body.
    WolUsage,

    /// `plex-hubs` -> compact JSON `{enabled,onDeck:[…],recentlyAdded:[…]}` for
    /// the home-screen Plex widget. Bare command (no body); the server URL +
    /// token come from the daemon environment (`TV_SHELL_PLEX_URL` /
    /// `TV_SHELL_PLEX_TOKEN`). Stateless + cross-platform (`reqwest`), served
    /// like `sunshine-status`. Unconfigured/unreachable degrades to
    /// `{"enabled":false,…}` / empty hubs rather than erroring.
    PlexHubs,

    /// `steam-library` -> compact JSON `{status,recentlyPlayed:[…],allGames:[…]}`
    /// for the home-screen Steam widget. Bare command (no body); the
    /// tv-shell-host base URL + token come from the daemon environment
    /// (`TV_SHELL_STEAM_URL` / `TV_SHELL_STEAM_TOKEN`). Stateless +
    /// cross-platform (`reqwest`), served like `plex-hubs`. Unconfigured ⇒
    /// `{"status":"disabled",…}`.
    SteamLibrary,

    /// `steam-launch <appid>` -> launch a Steam game on the host (proxies
    /// `POST /launch` to tv-shell-host). The body is a single numeric appid
    /// token. Replies `ok` / `error:*`. Stateless + cross-platform.
    SteamLaunch(u32),
    /// `steam-launch` with a missing/non-numeric `<appid>` body.
    SteamLaunchUsage,

    /// `steam-bigpicture` -> open Steam Big Picture's HOME screen on the host
    /// (proxies `POST /open-bpm` to tv-shell-host — no body). Bare command (no
    /// args), mirroring `steam-launch` but landing on the BPM home instead of a
    /// game's page. Replies a compact-JSON status object (`{"status":"ok"}` /
    /// `{"status":"error",…}`). Stateless + cross-platform.
    SteamBigPicture,

    /// `steam-quit <appid>` -> gracefully terminate a running Steam game on the
    /// host (proxies `POST /quit` to tv-shell-host with `{appid}`). The body is a
    /// single numeric appid token, mirroring `steam-launch`. Replies a compact-JSON
    /// status object (`{"status":"ok"}` / `{"status":"error",…}`). Stateless +
    /// cross-platform.
    SteamQuit(u32),
    /// `steam-quit` with a missing/non-numeric `<appid>` body.
    SteamQuitUsage,

    /// `steam-suspend` -> put the Steam host machine to sleep (proxies
    /// `POST /sleep` to tv-shell-host — no body). **Bare command, so there is no
    /// `…Usage` variant**: unlike `steam-quit`/`steam-launch` it takes no
    /// argument, matching `steam-bigpicture`.
    ///
    /// The host may REFUSE (a game is running, a stream is live), which is a
    /// normal answer rather than a failure — the reply carries the host's reason
    /// plus a `refused` flag: `{"status":"ok"}` /
    /// `{"status":"error","reason":…,"refused":true|false}`. Stateless +
    /// cross-platform.
    SteamSuspend,

    /// `steam-hosts` -> compact JSON `{status,active,hosts:[{name,host}]}`
    /// listing the configured tv-shell-host sidecars (`[[steam.hosts]]` in
    /// config.toml, or the legacy single `[steam].url`) and which one is
    /// active. Bare command (no body). Stateless + cross-platform. Unconfigured
    /// ⇒ `{"status":"disabled","active":null,"hosts":[]}`.
    SteamHosts,

    /// `steam-set-host <name>` -> select which configured Steam sidecar the
    /// daemon actively checks (persisted as `steamServer` in settings.json; the
    /// next `steam-library` poll / health probe uses it). The body is the host's
    /// configured name, verbatim (names may contain spaces). Replies `ok` /
    /// `error:*` (unknown names are rejected). Stateless + cross-platform.
    SteamSetHost(String),
    /// `steam-set-host` with a missing/empty `<name>` body.
    SteamSetHostUsage,

    // --- Moonlight local-config "forget" (creds-free unpair) ---
    /// `moonlight-forget <host>` -> remove a host from Moonlight's local config
    /// (`Moonlight.conf`) so this client is no longer paired with it. `host` is
    /// the IP/hostname string the shell uses (matched against the conf's
    /// hostname/localaddress/manualaddress/remoteaddress fields). Stateless
    /// (cross-platform — it's just file editing). Replies `ok` / `error:*`.
    MoonlightForget(String),
    /// `moonlight-forget` with a missing `<host>` body.
    MoonlightForgetUsage,

    // --- Phase 4: HDMI-CEC (cec-rs / libcec) ---
    /// `cec-scan` — return all visible CEC devices as a compact JSON array.
    CecScan,
    /// `cec-device <addr>` — return info for a single logical address as a
    /// compact JSON object (`{logicalAddress,physicalAddress,vendor,osdName,
    /// powerStatus,type}`), or `error:*` if the device is absent.
    CecDevice(String),
    /// `cec-power-on <addr>` — send a CEC power-on command to the device at
    /// the given logical address.
    CecPowerOn(String),
    /// `cec-power-off <addr>` — send a CEC standby command to the device at
    /// the given logical address.
    CecPowerOff(String),
    /// `cec-active-source` — set this adapter as the CEC active source.
    CecActiveSource,
    /// A `cec-device`/`cec-power-on`/`cec-power-off` with a missing address.
    /// `which` is the bare command word for the usage line (mirrors
    /// `BtMacUsage`).
    CecAddrUsage(&'static str),
    /// `cec-health` — return the current CEC transmit-wedge health as a compact
    /// JSON object `{transmit,reason,since,lastError}` (#19). Read-only: it
    /// reports the last-known transmit state and never drives the bus. `reason`
    /// is `null` while the adapter is open; when the adapter is unavailable the
    /// reply is `{transmit:"unavailable",reason:…}` (`no_libcec` / `no_adapter` /
    /// `adapter_open_failed`).
    CecHealth,
    /// `cec-test` — run an explicit on-demand, side-effect-free CEC poll probe,
    /// update the transmit-health, emit `cec:health` on a change, and reply with
    /// the same JSON object as `cec-health` (#19). The "Test CEC" button's
    /// backend.
    CecTest,

    /// `set-active-game <id>` — signal the current foreground game to the
    /// daemon. The daemon activates per-game binding overrides for `<id>` from
    /// `settings.json`'s `perGameBindings`. In-memory only.
    SetActiveGame(String),
    /// `set-active-game` with no body — clears the active game, reverting to
    /// player/global binding layers only.
    SetActiveGameClear,

    /// `overlay-focus on|off` — a modal shell overlay opened (`on`) or closed
    /// (`off`) over a running app (the nav overlay drawer, QAM/power overlays).
    /// While ON the daemon routes pad input to the SHELL key-map regardless of
    /// the base presenter (`Game`/`Handoff`) and force-GRABS every pad so the
    /// app stops seeing raw events — critical for `Handoff`, where the pad is
    /// normally ungrabbed. OFF restores the base presenter's routing + grab
    /// exactly (re-ungrab for `Handoff`). The base presenter is remembered by
    /// the input runtime, so the toggle carries no other argument. In-memory
    /// only; replies `ok`.
    OverlayFocus(bool),
    /// `overlay-focus` with a missing/invalid argument (not `on`/`off`).
    OverlayFocusUsage,
    /// `controllerdb-status` — return the current controller DB status as a
    /// compact JSON object: `{source, entryCount, lastDownloaded, upstreamUrl,
    /// error?}`. Stateless (no input-runtime round-trip); served directly by
    /// the IPC layer. See `docs/IPC_PROTOCOL.md`.
    ControllerDbStatus,
    /// `controllerdb-refresh` — re-download the upstream SDL_GameControllerDB,
    /// update the on-disk cache, and reload the active DB live (no daemon
    /// restart). Returns the same status JSON shape as `controllerdb-status`,
    /// with the `error` field set when the fetch fails. The daemon keeps the
    /// existing DB on failure (graceful degradation). Served by the IPC layer
    /// (async fetch), which sends `Control::ControllerDbRefreshed` to hot-swap
    /// the runtime DB after a successful refresh.
    ControllerDbRefresh,

    // --- #160: per-pad battery + rumble capability/status ---
    /// `pad-battery <id>` — return the current battery state for the pad whose
    /// stable wire id is `<id>` as a compact JSON object. `id` and `present`
    /// are always present; `level` and `charging` are added only when
    /// `present` is `true` (a battery reading is available). Wired pads / pads
    /// with no battery sysfs entry report `{"id":…,"present":false}`. An
    /// unknown id replies `error:pad not found '<id>'`. `<id>` is a single
    /// whitespace-trimmed token.
    PadBatteryQuery(String),
    /// `pad-battery` with a missing/empty `<id>` body.
    PadBatteryUsage,
    /// `pad-rumble-status <id>` — return the rumble capability and current
    /// status for the pad whose stable wire id is `<id>` as a compact JSON
    /// object `{id, supported, enabled}`. `<id>` is a single token.
    PadRumbleStatus(String),
    /// `pad-rumble-status` with a missing/empty `<id>` body.
    PadRumbleStatusUsage,

    // --- #164: sys-status / storage-status ---
    /// `sys-status` — return OS name, kernel version, hostname and uptime
    /// as a compact JSON object `{os, kernel, hostname, uptime}`.
    SysStatus,
    /// `storage-status` — return a JSON array of real filesystem mounts with
    /// raw-byte sizes `[{mount, size, used, avail, pct}, …]`.
    StorageStatus,
    /// `sys-metrics` — return live hardware telemetry as a compact JSON object
    /// `{cpuPct, memUsed, memTotal, memPct, load1, temps:[{label, celsius}]}` (#235).
    SysMetrics,
    /// `build-info` — return the currently-deployed build identity as a compact
    /// JSON object `{version, sha, branch}` (same live source as /metrics
    /// build_info + the `X-TvShell-*` headers). Backs the System settings page's
    /// version readout. Read live (async git at `install_root()`) so a
    /// `/dev/deploy` HEAD swap under the running daemon is reflected without a
    /// restart.
    BuildInfo,

    /// Anything unrecognized -> the daemon replies `unknown`.
    Unknown,
}

/// The closed vocabulary of shell intents accepted by the `intent <name>`
/// command and re-broadcast as `intent:<name>` events. Single source of truth
/// shared by the input runtime's validator and the IPC tests.
///
/// Semantics (mapped to shell actions by QML):
/// - `home` — global return-to-shell escape (keyboard Super, automation);
///   distinct from the gamepad neutrals below.
/// - `home-tap` / `home-hold` — gamepad Home neutrals (QML routes tap->menu,
///   hold->home/reset from the focus it owns).
/// - `menu` — nav-drawer toggle.
/// - `settings` — open settings.
/// - `power` — power menu.
///
/// These are the *high-level, focus-independent* actions QML interprets by
/// state. Directional focus moves + confirm/cancel are **not** here: they are
/// keyboard-layer concerns served by real key events (the gamepad d-pad/A/B that
/// the daemon synthesizes, `wtype`, or the `key <name>` IPC command), not the
/// broadcast bus. Earlier revisions listed `nav-up/down/left/right`/`select`/
/// `back` here, but nothing produced or consumed them — they were a dead
/// parallel path, removed in favor of `key <name>`.
///
/// ## Deep-link namespaces
///
/// In addition to the coarse vocabulary above, `is_known_intent` also accepts
/// deep-link targets in namespaced form `<ns>:<leaf>`:
///
/// - `settings:<page>` — open a named settings page. The daemon accepts any
///   non-empty `<leaf>`; the page registry lives in QML (`SettingsPanel`), so
///   unknown page slugs are accepted by the daemon (`ok` + broadcast) but are
///   a graceful no-op in QML (logged, no crash).
/// - `overlay:<target>` — open a QAM overlay popover. The daemon validates
///   `<target>` against the closed [`INTENT_OVERLAY_TARGETS`] set; unknown
///   overlay targets are rejected with `error:unknown intent '<name>'`.
/// - `app:<id>` — launch the local app whose `wmClass` (StartupWMClass) is
///   `<id>`. The daemon accepts any non-empty `<leaf>`; the live app list lives
///   in QML, so an absent app is a graceful no-op in QML (logged, no crash).
///
/// A name with an empty leaf (`settings:`, `overlay:`, `app:`) or an unknown
/// namespace (`foo:bar`) returns `false` → the daemon replies
/// `error:unknown intent '<name>'`.
///
/// The wire format is unchanged: `Event::Intent` already renders
/// `intent:<name>` verbatim, so `intent:settings:bluetooth` arrives in QML as
/// the string `"settings:bluetooth"` inside the `intent:` prefix — no new
/// Command/Event/Control variants are needed.
pub const INTENT_VOCAB: &[&str] = &["home", "home-tap", "home-hold", "menu", "settings", "power"];

/// The closed set of overlay targets accepted by the `overlay:<target>`
/// deep-link namespace. Validated here in the daemon (unlike `settings` and
/// `app` whose registries live in QML). `session` opens the right-edge Session
/// QAM drawer (#218).
pub const INTENT_OVERLAY_TARGETS: &[&str] = &["volume", "network", "session"];

/// True if `name` is a known intent — either a coarse vocabulary entry or a
/// valid deep-link target. The function is pure/side-effect-free and is shared
/// by the input runtime, the IPC fake, and the protocol tests.
///
/// Acceptance rules:
/// - `INTENT_VOCAB.contains(&name)` — unchanged coarse path.
/// - `name.split_once(':') ` into `(ns, leaf)` with non-empty `leaf` AND one
///   of the recognised namespaces:
///   - `ns == "settings"` — any non-empty leaf (page registry is in QML).
///   - `ns == "app"` — any non-empty leaf (app list lives in QML).
///   - `ns == "overlay"` with `INTENT_OVERLAY_TARGETS.contains(&leaf)`.
/// - Everything else returns `false`.
pub fn is_known_intent(name: &str) -> bool {
    if INTENT_VOCAB.contains(&name) {
        return true;
    }
    if let Some((ns, leaf)) = name.split_once(':') {
        if leaf.is_empty() {
            return false;
        }
        return match ns {
            "settings" => true,
            "app" => true,
            "overlay" => INTENT_OVERLAY_TARGETS.contains(&leaf),
            _ => false,
        };
    }
    false
}

/// If `cmd` is `word` (exact) or `word` followed by whitespace, return the
/// trimmed remainder (the body). `Some("")` means the bare command with no body;
/// `None` means `cmd` isn't this command at all (e.g. `set-configX`).
fn command_body<'a>(cmd: &'a str, word: &str) -> Option<&'a str> {
    let rest = cmd.strip_prefix(word)?;
    if rest.is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

impl Command {
    /// Parse one line (the trailing newline is already stripped by the codec).
    /// Surrounding whitespace is trimmed to mirror Python's `data.decode().strip()`.
    pub fn parse(line: &str) -> Command {
        let cmd = line.trim();
        match cmd {
            "grab" => Command::Grab,
            "release" => Command::Release,
            "handoff" => Command::Handoff,
            "status" => Command::Status,
            "subscribe" => Command::Subscribe,
            "get-bindings" => Command::GetBindings,
            "capture-next" => Command::CaptureNext,
            "capture-cancel" => Command::CaptureCancel,
            "get-pads" => Command::GetPads,
            "list-input-devices" => Command::ListInputDevices,
            "list-apps" => Command::ListApps,
            "webapp-list" => Command::WebappList,
            "get-config" => Command::GetConfig,
            "get-recents" => Command::GetRecents,
            "get-notifications" => Command::GetNotifications,
            // Phase 3 bare commands (no body).
            "bt-power-status" => Command::BtPowerStatus,
            "bt-power-on" => Command::BtPowerOn,
            "bt-power-off" => Command::BtPowerOff,
            "bt-scan-on" => Command::BtScanOn,
            "bt-scan-off" => Command::BtScanOff,
            "bt-list" => Command::BtList,
            "net-status" => Command::NetStatus,
            "net-wifi-list" => Command::NetWifiList,
            "net-wifi-rescan" => Command::NetWifiRescan,
            "power-can-suspend" => Command::PowerCanSuspend,
            "power-suspend" => Command::PowerSuspend,
            "power-battery" => Command::PowerBattery,
            "plex-hubs" => Command::PlexHubs,
            "steam-library" => Command::SteamLibrary,
            "steam-bigpicture" => Command::SteamBigPicture,
            "steam-suspend" => Command::SteamSuspend,
            "steam-hosts" => Command::SteamHosts,
            // Phase 4 bare commands (no body).
            "hypr-active" => Command::HyprActive,
            "hypr-clients" => Command::HyprClients,
            "hypr-monitors" => Command::HyprMonitors,
            // Phase 4 HDMI-CEC bare commands (no body).
            "cec-scan" => Command::CecScan,
            "cec-active-source" => Command::CecActiveSource,
            "cec-health" => Command::CecHealth,
            "cec-test" => Command::CecTest,
            "controllerdb-status" => Command::ControllerDbStatus,
            "controllerdb-refresh" => Command::ControllerDbRefresh,
            "sys-status" => Command::SysStatus,
            "storage-status" => Command::StorageStatus,
            "sys-metrics" => Command::SysMetrics,
            "build-info" => Command::BuildInfo,
            _ => {
                // `set-config <json>` / `record-launch <json>`: the rest of the
                // line is a compact single-line JSON body. The command word must
                // be followed by whitespace (or be bare); a bare command with no
                // body is a usage error. `command_body` enforces the word
                // boundary so e.g. `set-configX` is not mistaken for set-config.
                if let Some(body) = command_body(cmd, "set-config") {
                    return if body.is_empty() {
                        Command::SetConfigUsage
                    } else {
                        Command::SetConfig(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "webapp-add") {
                    return if body.is_empty() {
                        Command::WebappAddUsage
                    } else {
                        Command::WebappAdd(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "webapp-remove") {
                    return if body.is_empty() {
                        Command::WebappRemoveUsage
                    } else {
                        Command::WebappRemove(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "record-launch") {
                    return if body.is_empty() {
                        Command::RecordLaunchUsage
                    } else {
                        Command::RecordLaunch(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "record-notification") {
                    return if body.is_empty() {
                        Command::RecordNotificationUsage
                    } else {
                        Command::RecordNotification(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "set-notifications") {
                    return if body.is_empty() {
                        Command::SetNotificationsUsage
                    } else {
                        Command::SetNotifications(body.to_string())
                    };
                }
                // `intent <name>`: a single intent-name token (whitespace-
                // trimmed). A missing body is a usage error. `command_body`
                // enforces the word boundary so e.g. `intentX` is not mistaken
                // for `intent`. The closed-vocabulary check happens in the
                // input runtime, not here (parsing stays validation-free).
                if let Some(body) = command_body(cmd, "intent") {
                    return if body.is_empty() {
                        Command::IntentUsage
                    } else {
                        Command::Intent(body.to_string())
                    };
                }
                // `key <name>`: a single key-name token (whitespace-trimmed). A
                // missing body is a usage error. `command_body` enforces the word
                // boundary so e.g. `keyX` is not mistaken for `key`. The
                // closed-vocabulary check + keycode mapping happen in the input
                // runtime (parsing stays validation-free), mirroring `intent`.
                if let Some(body) = command_body(cmd, "key") {
                    return if body.is_empty() {
                        Command::KeyUsage
                    } else {
                        Command::Key(body.to_string())
                    };
                }
                // `rumble <id> <ms>`: two whitespace-separated tokens — the pad
                // wire id and a non-negative millisecond duration. A missing
                // token or a non-integer `<ms>` is a usage error.
                // `command_body` enforces the word boundary so e.g. `rumbleX`
                // is not mistaken for `rumble`.
                if let Some(body) = command_body(cmd, "rumble") {
                    let mut toks = body.split_whitespace();
                    return match (toks.next(), toks.next()) {
                        (Some(id), Some(ms)) => match ms.parse::<u32>() {
                            Ok(ms) => Command::Rumble {
                                id: id.to_string(),
                                ms,
                            },
                            Err(_) => Command::RumbleUsage,
                        },
                        _ => Command::RumbleUsage,
                    };
                }
                // Phase 3 MAC-argument commands: `bt-connect <mac>` etc. The body
                // is a single MAC token (whitespace-trimmed); a missing body is a
                // usage error. `command_body` enforces the word boundary so e.g.
                // `bt-connectX` is not mistaken for `bt-connect`. Order matters:
                // `bt-connect` is a prefix of nothing else here, but check the
                // longer-or-equal words explicitly to avoid surprises.
                for word in ["bt-connect", "bt-disconnect", "bt-pair", "bt-trust"] {
                    if let Some(body) = command_body(cmd, word) {
                        if body.is_empty() {
                            return Command::BtMacUsage(word);
                        }
                        let mac = body.to_string();
                        return match word {
                            "bt-connect" => Command::BtConnect(mac),
                            "bt-disconnect" => Command::BtDisconnect(mac),
                            "bt-pair" => Command::BtPair(mac),
                            "bt-trust" => Command::BtTrust(mac),
                            _ => unreachable!("word came from the literal list above"),
                        };
                    }
                }
                // Phase 4 `sunshine-status <host> <port>`: the body is two
                // whitespace-separated tokens. A missing/incomplete body is a
                // usage error. `command_body` enforces the word boundary so
                // e.g. `sunshine-statusX` is not mistaken for the command.
                if let Some(body) = command_body(cmd, "sunshine-status") {
                    let mut toks = body.split_whitespace();
                    return match (toks.next(), toks.next()) {
                        (Some(host), Some(port)) => Command::SunshineStatus {
                            host: host.to_string(),
                            port: port.to_string(),
                        },
                        _ => Command::SunshineStatusUsage,
                    };
                }
                // `net-throughput <iface>`: a single interface-name token. A
                // missing body is a usage error. `command_body` enforces the word
                // boundary so e.g. `net-throughputX` is not mistaken for it.
                if let Some(body) = command_body(cmd, "net-throughput") {
                    return match body.split_whitespace().next() {
                        Some(iface) => Command::NetThroughput {
                            iface: iface.to_string(),
                        },
                        None => Command::NetThroughputUsage,
                    };
                }
                // `net-ping <host> [count]`: a host token + optional ping count
                // (defaults to 1, clamped 1..=10 by the handler). A missing host
                // is a usage error. `command_body` enforces the word boundary so
                // e.g. `net-pingX` is not mistaken for `net-ping`.
                if let Some(body) = command_body(cmd, "net-ping") {
                    let mut toks = body.split_whitespace();
                    return match toks.next() {
                        Some(host) => Command::NetPing {
                            host: host.to_string(),
                            count: toks.next().and_then(|t| t.parse::<u32>().ok()).unwrap_or(1),
                        },
                        None => Command::NetPingUsage,
                    };
                }
                // `wol <host>`: a single host token (the IP/hostname the shell
                // uses). A missing body is a usage error. `command_body` enforces
                // the word boundary so e.g. `wolX` is not mistaken for `wol`.
                if let Some(body) = command_body(cmd, "wol") {
                    return if body.is_empty() {
                        Command::WolUsage
                    } else {
                        Command::Wol {
                            host: body.to_string(),
                        }
                    };
                }
                // `steam-launch <appid>`: the body is a single numeric appid
                // token. A missing/non-numeric body is a usage error.
                // `command_body` enforces the word boundary so e.g.
                // `steam-launchX` is not mistaken for the command.
                if let Some(body) = command_body(cmd, "steam-launch") {
                    return match body
                        .split_whitespace()
                        .next()
                        .and_then(|t| t.parse::<u32>().ok())
                    {
                        Some(appid) => Command::SteamLaunch(appid),
                        None => Command::SteamLaunchUsage,
                    };
                }
                // `steam-set-host <name>`: the body is the configured host name,
                // verbatim (names may contain spaces). Missing/empty ⇒ usage
                // error. `command_body` enforces the word boundary. Checked
                // before `steam-quit`/`steam-launch` order-independently — the
                // words don't prefix each other.
                if let Some(body) = command_body(cmd, "steam-set-host") {
                    return if body.is_empty() {
                        Command::SteamSetHostUsage
                    } else {
                        Command::SteamSetHost(body.to_string())
                    };
                }
                // `steam-quit <appid>`: same shape as `steam-launch` — a single
                // numeric appid token. Missing/non-numeric ⇒ usage error.
                // `command_body` enforces the word boundary so e.g. `steam-quitX`
                // is not mistaken for the command.
                if let Some(body) = command_body(cmd, "steam-quit") {
                    return match body
                        .split_whitespace()
                        .next()
                        .and_then(|t| t.parse::<u32>().ok())
                    {
                        Some(appid) => Command::SteamQuit(appid),
                        None => Command::SteamQuitUsage,
                    };
                }
                // `moonlight-forget <host>`: a single host token (the IP/hostname
                // the shell uses). Removes the host from Moonlight's local config
                // so this client is no longer paired. A missing body is a usage
                // error. `command_body` enforces the word boundary so e.g.
                // `moonlight-forgetX` is not mistaken for the command.
                if let Some(body) = command_body(cmd, "moonlight-forget") {
                    return if body.is_empty() {
                        Command::MoonlightForgetUsage
                    } else {
                        Command::MoonlightForget(body.to_string())
                    };
                }
                // Phase 4 CEC address-argument commands: `cec-device <addr>` etc.
                // The body is a single logical-address token (whitespace-trimmed);
                // a missing body is a usage error. `command_body` enforces the
                // word boundary so e.g. `cec-deviceX` is not mistaken for
                // `cec-device`.
                for word in ["cec-device", "cec-power-on", "cec-power-off"] {
                    if let Some(body) = command_body(cmd, word) {
                        if body.is_empty() {
                            return Command::CecAddrUsage(word);
                        }
                        let addr = body.to_string();
                        return match word {
                            "cec-device" => Command::CecDevice(addr),
                            "cec-power-on" => Command::CecPowerOn(addr),
                            "cec-power-off" => Command::CecPowerOff(addr),
                            _ => unreachable!("word came from the literal list above"),
                        };
                    }
                }
                // `pad-battery <id>` / `pad-rumble-status <id>`: a single pad
                // wire-id token (whitespace-trimmed). A missing body is a usage
                // error. `command_body` enforces the word boundary.
                if let Some(body) = command_body(cmd, "pad-battery") {
                    return if body.is_empty() {
                        Command::PadBatteryUsage
                    } else {
                        Command::PadBatteryQuery(body.to_string())
                    };
                }
                if let Some(body) = command_body(cmd, "pad-rumble-status") {
                    return if body.is_empty() {
                        Command::PadRumbleStatusUsage
                    } else {
                        Command::PadRumbleStatus(body.to_string())
                    };
                }
                // `set-active-game <id>`: a single game-id token (or bare for
                // clear). `command_body` enforces the word boundary so
                // `set-active-gameX` is not mistaken for this command.
                if let Some(body) = command_body(cmd, "set-active-game") {
                    return if body.is_empty() {
                        Command::SetActiveGameClear
                    } else {
                        Command::SetActiveGame(body.to_string())
                    };
                }
                // `overlay-focus on|off`: a modal shell overlay opened/closed
                // over a running app. The body must be exactly `on` or `off`;
                // a missing/other body is a usage error. `command_body`
                // enforces the word boundary so `overlay-focusX` stays Unknown.
                if let Some(body) = command_body(cmd, "overlay-focus") {
                    return match body {
                        "on" => Command::OverlayFocus(true),
                        "off" => Command::OverlayFocus(false),
                        _ => Command::OverlayFocusUsage,
                    };
                }
                // Python keys `set-binding` off the `"set-binding "` prefix
                // (with trailing space), so a bare `set-binding` is `unknown`.
                if let Some(rest) = cmd.strip_prefix("set-binding ") {
                    // Mirror Python `cmd.split(None, 2)`: at most two splits, so
                    // the button is everything after the action (e.g.
                    // "select BTN_SOUTH EXTRA" -> button "BTN_SOUTH EXTRA",
                    // which then fails as an invalid button — matching Python,
                    // not a usage error).
                    match rest.trim_start().split_once(char::is_whitespace) {
                        Some((action, button)) => {
                            let button = button.trim_start();
                            if action.is_empty() || button.is_empty() {
                                return Command::SetBindingUsage;
                            }
                            return Command::SetBinding {
                                action: action.to_string(),
                                button: button.to_string(),
                            };
                        }
                        // Only one token after the prefix -> wrong arg count.
                        None => return Command::SetBindingUsage,
                    }
                }
                Command::Unknown
            }
        }
    }
}

/// Events streamed to `subscribe` clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    ControllerWake,
    ControllerDisconnected,
    /// A physical pad joined the fleet and was assigned a stable player slot
    /// (#101). Payload is a compact JSON object `{id,index,name}`: `id` is the
    /// stable wire id, `index` the player slot (0 = P1), `name` the device name.
    /// Wire: `pad:connected:<json>`. Complements `controller-wake` (the
    /// single-pad legacy signal still fires for the first pad).
    PadConnected(String),
    /// A physical pad left the fleet; its slot is freed for reuse. Payload is the
    /// pad's stable wire id. Wire: `pad:disconnected:<id>`. Complements
    /// `controller-disconnected`.
    PadDisconnected(String),
    /// A pad's player LED was assigned (#101 LED, Phase 5.5). Payload is a
    /// compact JSON object `{id,index}`: `id` the stable wire id, `index` the
    /// player slot whose LED was lit. Wire: `pad:index:<json>`. Emitted at slot
    /// assignment only when the pad has a controllable LED (`EV_LED`); a no-op
    /// for pads without one.
    PadIndex(String),
    /// A pad's battery state changed (#100 battery, Phase 5.5). Payload is a
    /// compact JSON object `{id,level,charging}`: `id` the stable wire id,
    /// `level` the charge percentage (0..=100), `charging` whether it is
    /// charging. Wire: `pad:battery:<json>`. Only emitted for pads that report a
    /// battery (wireless); wired pads emit none.
    PadBattery(String),
    ComboEndSession,
    ComboForceQuit,
    ComboSuspendStream,
    /// A shell intent broadcast (`intent:<name>`), re-emitted from an accepted
    /// `intent <name>` command. `<name>` is one of the closed vocabulary
    /// (see `docs/IPC_PROTOCOL.md`). The control surface for keyboard
    /// global-escape and automation; QML maps each name to a shell action.
    Intent(String),
    InputMode(InputMode),
    /// Space-and-plus joined held controller inputs (may be empty).
    Buttons(String),

    // --- Phase 3 events ---
    /// Bluetooth adapter power changed (`bt:powered:on` / `bt:powered:off`).
    BtPowered(bool),
    /// A device was discovered/updated; payload is a compact JSON object
    /// (same shape as a `bt-list` element). Wire: `bt:device:<json>`.
    BtDevice(String),
    /// A device was removed from discovery; payload is the MAC.
    /// Wire: `bt:device-removed:<mac>`.
    BtDeviceRemoved(String),
    /// Discovery (scan) started/stopped (`bt:scanning:on` / `bt:scanning:off`).
    BtScanning(bool),
    /// NetworkManager connectivity changed; payload is a state word
    /// (`none`/`portal`/`limited`/`full`/`unknown`). Wire: `net:connectivity:<state>`.
    NetConnectivity(String),
    /// Wi-Fi state changed; payload is a compact JSON object (same shape as a
    /// `net-status` body). Wire: `net:wifi:<json>`.
    NetWifi(String),
    /// Primary connection changed; payload is its id/name (may be empty).
    /// Wire: `net:primary:<id>`.
    NetPrimary(String),
    /// Battery state changed; payload is a compact JSON object (same shape as a
    /// `power-battery` body). Wire: `power:battery:<json>`.
    PowerBattery(String),

    // --- Phase 4 events (Hyprland) ---
    /// Active window changed; payload is the active window's class (may be empty
    /// when no window is focused). Wire: `hypr:activewindow:<class>`.
    HyprActiveWindow(String),
    /// Active window fullscreen state changed. Wire: `hypr:fullscreen:<0|1>`.
    HyprFullscreen(bool),
    /// A new window was mapped. Payload is a compact JSON object
    /// `{"address":"0x..","class":"..","title":"..","workspace":".."}`.
    /// Wire: `hypr:openwindow:<json>`.
    HyprOpenWindow(String),
    /// A window was closed. Payload is the window's address. Wire:
    /// `hypr:closewindow:<address>`.
    HyprCloseWindow(String),

    // --- Phase 4 events (HDMI-CEC) ---
    /// A CEC device was discovered or updated; payload is a compact JSON object
    /// (`{logicalAddress,physicalAddress,vendor,osdName,powerStatus,type}`).
    /// Wire: `cec:device:<json>`.
    CecDevice(String),
    /// A CEC device's power status changed; payload is a compact JSON object
    /// `{addr,power}`. Wire: `cec:power:<json>`.
    CecPower(String),
    /// The CEC transmit-wedge health state CHANGED (#19); payload is a compact
    /// JSON object `{transmit,reason,since,lastError}` (same shape as the
    /// `cec-health` reply). Broadcast only on a real transition (not on every
    /// probe), so the AV Control page's status line updates without polling. Also
    /// broadcast ONCE when the libcec open handshake fails, carrying
    /// `transmit:"unavailable"` + the open-failure `reason` so the page can show
    /// the accurate "no adapter" vs "adapter wedged — re-seat it" message. Wire:
    /// `cec:health:<json>`.
    CecHealth(String),

    // --- Config live-reload ---
    /// `settings.json` was modified by an **external** writer (SSH / Ansible /
    /// web UI). The daemon suppresses its own `set-config`/`set-binding` writes
    /// via the self-write generation guard in `config.rs`, so this event fires
    /// only for foreign edits. Carries no payload — subscribers re-fetch the
    /// full document via `get-config`. Wire: `config:changed`.
    ConfigChanged,

    // --- #166: screenshot flash ---
    /// The HTTP bridge received a `GET /screenshot?flash=1` request. Emitted
    /// AFTER `grim` captures the frame (so the flash is post-capture feedback,
    /// not baked into the PNG). The QML `ScreenshotFlash` overlay paints a
    /// brief white vignette. Wire: `screenshot:flash`.
    ScreenshotFlash,

    // --- Remote-service health ---
    /// A remote service's reachability changed (or its initial state at poll
    /// startup). Payload is a compact JSON object
    /// `{"service":<name>,"status":"ok"|"disabled"|"unreachable"|"error"}`,
    /// built by [`crate::service_health::health_json`]. Emitted by the health
    /// poller; the QML `ServiceMonitor` filters the `health:` prefix and matches
    /// on `service`. Wire: `health:<json>`.
    ServiceHealth(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Controller,
    Mouse,
}

impl InputMode {
    pub fn as_str(self) -> &'static str {
        match self {
            InputMode::Controller => "controller",
            InputMode::Mouse => "mouse",
        }
    }
}

impl fmt::Display for Event {
    /// Render the event as its exact wire string (no trailing newline; the
    /// codec adds it).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::ControllerWake => f.write_str("controller-wake"),
            Event::ControllerDisconnected => f.write_str("controller-disconnected"),
            Event::PadConnected(json) => write!(f, "pad:connected:{json}"),
            Event::PadDisconnected(id) => write!(f, "pad:disconnected:{id}"),
            Event::PadIndex(json) => write!(f, "pad:index:{json}"),
            Event::PadBattery(json) => write!(f, "pad:battery:{json}"),
            Event::ComboEndSession => f.write_str("combo:end-session"),
            Event::ComboForceQuit => f.write_str("combo:force-quit"),
            Event::ComboSuspendStream => f.write_str("combo:suspend-stream"),
            Event::Intent(name) => write!(f, "intent:{name}"),
            Event::InputMode(m) => write!(f, "input-mode:{}", m.as_str()),
            Event::Buttons(s) => write!(f, "buttons:{s}"),
            Event::BtPowered(on) => write!(f, "bt:powered:{}", on_off(*on)),
            Event::BtDevice(json) => write!(f, "bt:device:{json}"),
            Event::BtDeviceRemoved(mac) => write!(f, "bt:device-removed:{mac}"),
            Event::BtScanning(on) => write!(f, "bt:scanning:{}", on_off(*on)),
            Event::NetConnectivity(state) => write!(f, "net:connectivity:{state}"),
            Event::NetWifi(json) => write!(f, "net:wifi:{json}"),
            Event::NetPrimary(id) => write!(f, "net:primary:{id}"),
            Event::PowerBattery(json) => write!(f, "power:battery:{json}"),
            Event::HyprActiveWindow(class) => write!(f, "hypr:activewindow:{class}"),
            Event::HyprFullscreen(fs) => write!(f, "hypr:fullscreen:{}", if *fs { 1 } else { 0 }),
            Event::HyprOpenWindow(json) => write!(f, "hypr:openwindow:{json}"),
            Event::HyprCloseWindow(address) => write!(f, "hypr:closewindow:{address}"),
            Event::CecDevice(json) => write!(f, "cec:device:{json}"),
            Event::CecPower(json) => write!(f, "cec:power:{json}"),
            Event::CecHealth(json) => write!(f, "cec:health:{json}"),
            Event::ConfigChanged => f.write_str("config:changed"),
            Event::ScreenshotFlash => f.write_str("screenshot:flash"),
            Event::ServiceHealth(json) => write!(f, "health:{json}"),
        }
    }
}

/// `on`/`off` for boolean wire payloads (bt:powered, bt:scanning).
fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

// ---------------------------------------------------------------------------
// Response builders (the exact reply strings, sans trailing newline).
// ---------------------------------------------------------------------------

pub fn resp_ok() -> String {
    "ok".to_string()
}

pub fn resp_unknown() -> String {
    "unknown".to_string()
}

pub fn resp_subscribed() -> String {
    "subscribed".to_string()
}

pub fn resp_status(connected: bool, grabbed: bool) -> String {
    let c = if connected {
        "connected"
    } else {
        "disconnected"
    };
    let g = if grabbed { "grabbed" } else { "released" };
    format!("{c}:{g}")
}

pub fn resp_set_binding_usage() -> String {
    "error:usage: set-binding <action> <button_name>".to_string()
}

pub fn resp_unknown_action(action: &str) -> String {
    format!("error:unknown action '{action}'")
}

pub fn resp_invalid_button(button: &str) -> String {
    format!("error:invalid button '{button}'")
}

pub fn resp_captured(button_name: &str) -> String {
    format!("captured:{button_name}")
}

pub fn resp_set_config_usage() -> String {
    "error:usage: set-config <json-object>".to_string()
}

pub fn resp_record_launch_usage() -> String {
    "error:usage: record-launch <json-object>".to_string()
}

pub fn resp_webapp_add_usage() -> String {
    "error:usage: webapp-add <json-object with name+url>".to_string()
}

pub fn resp_webapp_remove_usage() -> String {
    "error:usage: webapp-remove <id>".to_string()
}

pub fn resp_record_notification_usage() -> String {
    "error:usage: record-notification <json-object>".to_string()
}

pub fn resp_set_notifications_usage() -> String {
    "error:usage: set-notifications <json-array>".to_string()
}

pub fn resp_intent_usage() -> String {
    "error:usage: intent <name>".to_string()
}

/// Usage line for an `overlay-focus` issued without a valid `on`/`off` arg.
pub fn resp_overlay_focus_usage() -> String {
    "error:usage: overlay-focus on|off".to_string()
}

/// Usage line for a `rumble` issued without a valid `<id> <ms>` body.
pub fn resp_rumble_usage() -> String {
    "error:usage: rumble <id> <ms>".to_string()
}

/// Error reply for an `intent <name>` whose name is outside the closed
/// vocabulary.
pub fn resp_unknown_intent(name: &str) -> String {
    format!("error:unknown intent '{name}'")
}

/// Usage line for a `key` issued without a `<name>` body.
pub fn resp_key_usage() -> String {
    "error:usage: key <name>".to_string()
}

/// Error reply for a `key <name>` whose name is outside the closed key
/// vocabulary (`config::key_for_action` returned `None`).
pub fn resp_unknown_key(name: &str) -> String {
    format!("error:unknown key '{name}'")
}

/// Generic error reply for a malformed config/recents body.
pub fn resp_error(msg: &str) -> String {
    format!("error:{msg}")
}

/// Strip control characters (newlines, carriage returns, etc.) from a string
/// destined for an IPC error reply, replacing each with a space.
///
/// The wire protocol is newline-delimited and the QML side reads it line-by-line
/// (`SplitParser`), so a `\n`/`\r`/other control char embedded in an error body
/// (e.g. an error Display string, or a file value echoed back) could split one
/// reply into several lines and desync the client. Keeping the reply on a single
/// line preserves framing. Pure — unit-tested.
pub fn sanitize_ipc(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Await a reply-producing future under a hard timeout, returning the future's
/// `String` on completion or `error:<timeout_msg>` if the bound elapses first.
///
/// This is the safety bound for blocking-backend actors that forward a request
/// to a worker thread and await its reply (e.g. the CEC actor's `forward`): if
/// the worker wedges (a blocking libcec `open()`/transmit that never returns),
/// the await would otherwise hang forever and silence the whole actor. Capping
/// it guarantees the actor's request loop always makes progress — a wedged
/// worker yields prompt errors instead of silence. `timeout_msg` is the error
/// body used on elapse (sanitized so a stray control char can't desync the wire).
/// Generic over the future so it is cross-platform and unit-testable without any
/// backend (the CEC module is Linux+feature-gated, so its own tests can't run on
/// macOS/CI).
pub async fn reply_with_timeout<F>(bound: std::time::Duration, timeout_msg: &str, fut: F) -> String
where
    F: std::future::Future<Output = String>,
{
    match tokio::time::timeout(bound, fut).await {
        Ok(reply) => reply,
        Err(_elapsed) => resp_error(&sanitize_ipc(timeout_msg)),
    }
}

pub fn resp_timeout() -> String {
    "timeout".to_string()
}

// ---------------------------------------------------------------------------
// Phase 3 response builders (D-Bus query replies).
// ---------------------------------------------------------------------------

/// Returned when a Phase 3 command is issued on a host where the D-Bus backbone
/// isn't wired (e.g. non-Linux builds where the module is `cfg`-excluded). The
/// IPC layer substitutes this for the `Some(tx)` round-trip when the channel is
/// `None`.
pub fn resp_unsupported() -> String {
    "error:unsupported on this platform".to_string()
}

/// Bluetooth adapter power status reply (`bt-power-status`).
pub fn resp_bt_power(on: bool) -> String {
    if on {
        "bt:on".to_string()
    } else {
        "bt:off".to_string()
    }
}

/// Usage line for a `bt-connect`/`bt-disconnect`/`bt-pair`/`bt-trust` issued
/// without a MAC. `which` is the bare command word.
pub fn resp_bt_mac_usage(which: &str) -> String {
    format!("error:usage: {which} <mac>")
}

/// Usage line for a `cec-device`/`cec-power-on`/`cec-power-off` issued without
/// a logical address. `which` is the bare command word.
pub fn resp_cec_addr_usage(which: &str) -> String {
    format!("error:usage: {which} <addr>")
}

/// Build the compact-JSON body for a single CEC device (`cec-device` reply and
/// the `cec:device:<json>` event payload). Pure string/serde — no `cec-rs`
/// types — so it compiles and unit-tests in the default (C-free) build leg even
/// though the libcec actor that calls it is feature-gated. `power_word` is the
/// already-mapped wire word from `cec::power_status_word` (e.g. "on"/"standby").
///
/// Field order is fixed by `serde_json`'s `preserve_order` feature, so the wire
/// bytes are stable: `{"logicalAddress":N,"powerStatus":"WORD"}`.
pub fn cec_device_json(logical_addr: i32, power_word: &str) -> String {
    serde_json::json!({
        "logicalAddress": logical_addr,
        "powerStatus": power_word,
    })
    .to_string()
}

/// Build the compact-JSON body for a CEC power-status change (`cec:power:<json>`
/// event payload after a power-on/power-off). `addr` is the logical address as
/// a wire string (the daemon keeps it as the small-integer string it received);
/// `power_word` is the mapped wire word. Pure — testable in the default leg.
///
/// Wire bytes: `{"addr":"N","power":"WORD"}`.
pub fn cec_power_json(addr: &str, power_word: &str) -> String {
    serde_json::json!({
        "addr": addr,
        "power": power_word,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// CEC transmit-wedge health (#19).
// ---------------------------------------------------------------------------

/// The transmit-health of the CEC adapter: whether the last transmit op
/// succeeded, returned `TransmitFailed`, or has not been attempted yet.
///
/// The target device's Pulse-Eight USB CEC adapter periodically enters a "transmit
/// wedge": libcec opens fine and can RECEIVE, but every TRANSMIT (power-on,
/// active-source, the poll probe) returns `TransmitFailed`. This enum is the
/// observable health surface the AV Control page reads via `cec-health`. Pure
/// (no `cec-rs` types) so the state machine + JSON shape are unit-tested in the
/// default (C-free) build leg even though the libcec actor that drives it is
/// feature-gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CecTransmitHealth {
    /// No transmit attempted yet / indeterminate.
    Unknown,
    /// The last transmit succeeded.
    Ok,
    /// The last transmit returned `TransmitFailed` (the wedge).
    Failing,
}

impl CecTransmitHealth {
    /// The wire word for the `transmit` field of [`cec_health_json`].
    pub fn as_str(self) -> &'static str {
        match self {
            CecTransmitHealth::Unknown => "unknown",
            CecTransmitHealth::Ok => "ok",
            CecTransmitHealth::Failing => "failing",
        }
    }
}

/// The CEC transmit-wedge health state: the [`CecTransmitHealth`] variant, the
/// epoch-millis of the last state CHANGE, and the last transmit error when
/// failing.
///
/// `record_success` / `record_failure` mutate it and return whether the variant
/// actually CHANGED, so the actor broadcasts `cec:health` only on real
/// transitions (not on every probe). The clock value is passed in (epoch millis)
/// to keep the transitions deterministic in unit tests. Pure — no `cec-rs`
/// types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CecHealthState {
    transmit: CecTransmitHealth,
    since_millis: u64,
    last_error: Option<String>,
}

impl CecHealthState {
    /// A fresh state in the `Unknown` variant, stamped `now_millis`.
    pub fn new(now_millis: u64) -> Self {
        Self {
            transmit: CecTransmitHealth::Unknown,
            since_millis: now_millis,
            last_error: None,
        }
    }

    /// The current transmit-health variant.
    pub fn transmit(&self) -> CecTransmitHealth {
        self.transmit
    }

    /// Epoch-millis of the last state change.
    pub fn since_millis(&self) -> u64 {
        self.since_millis
    }

    /// The last transmit error string (present only while failing).
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Record a successful transmit. On a variant change (→ `Ok`) bumps `since`
    /// to `now_millis`, clears `last_error`, and returns `true`. A repeat success
    /// (already `Ok`) is a no-op returning `false`, so no spurious `cec:health`
    /// is emitted.
    pub fn record_success(&mut self, now_millis: u64) -> bool {
        if self.transmit == CecTransmitHealth::Ok {
            return false;
        }
        self.transmit = CecTransmitHealth::Ok;
        self.since_millis = now_millis;
        self.last_error = None;
        true
    }

    /// Record a failed transmit with its error string. On a variant change
    /// (→ `Failing`) bumps `since` to `now_millis` and returns `true`. When
    /// already `Failing` the variant does NOT change, so `since` is KEPT (the
    /// wedge's onset time) and `false` is returned — but the freshest
    /// `last_error` is still stored so a later `cec-health` read reports it.
    pub fn record_failure(&mut self, err: &str, now_millis: u64) -> bool {
        let changed = self.transmit != CecTransmitHealth::Failing;
        self.transmit = CecTransmitHealth::Failing;
        if changed {
            self.since_millis = now_millis;
        }
        self.last_error = Some(err.to_string());
        changed
    }

    /// Build the wire JSON for this state (the `cec-health` / `cec-test` reply
    /// and the `cec:health:<json>` event payload). An AVAILABLE state (the
    /// adapter is open) always carries a `null` `reason` — the `reason` field is
    /// only populated when the adapter is UNAVAILABLE (see
    /// [`cec_unavailable_json`]).
    pub fn to_json(&self) -> String {
        cec_health_json(
            self.transmit.as_str(),
            None,
            self.since_millis,
            self.last_error.as_deref(),
        )
    }
}

/// Build the compact-JSON body for the CEC transmit-health (`cec-health` /
/// `cec-test` reply and the `cec:health:<json>` event payload). Pure — no
/// `cec-rs` types — so it compiles and unit-tests in the default (C-free) leg.
/// `transmit_word` is `"ok"|"failing"|"unknown"|"unavailable"`; `reason` is the
/// unavailable reason word (`None` → JSON `null`, used for all available states);
/// `since_millis` the epoch millis of the last state change; `last_error` the
/// transmit error when failing (`None` → JSON `null`).
///
/// Field order is fixed by `serde_json`'s `preserve_order` feature, so the wire
/// bytes are stable: `{"transmit":"WORD","reason":"…"|null,"since":N,
/// "lastError":"…"|null}`. The `reason` key sits right after `transmit`.
pub fn cec_health_json(
    transmit_word: &str,
    reason: Option<&str>,
    since_millis: u64,
    last_error: Option<&str>,
) -> String {
    serde_json::json!({
        "transmit": transmit_word,
        "reason": reason,
        "since": since_millis,
        "lastError": last_error,
    })
    .to_string()
}

/// Build the compact-JSON body for an UNAVAILABLE CEC adapter — the structured
/// reply that now replaces the bare `error:libcec unavailable` for `cec-health`
/// and `cec-test` (and the `cec:health:<json>` event broadcast when the open
/// handshake fails). `transmit` is fixed to `"unavailable"`, `reason` is one of
/// `no_libcec` / `no_adapter` / `adapter_open_failed`, and `lastError` is always
/// `null` (the actionable signal is the `reason`, not a transmit error). Pure —
/// no `cec-rs` types — so it is callable from the non-`cec`/non-Linux ipc.rs arms
/// and unit-tested in the default leg.
///
/// Wire bytes: `{"transmit":"unavailable","reason":"REASON","since":N,
/// "lastError":null}`.
pub fn cec_unavailable_json(reason: &str, since_millis: u64) -> String {
    cec_health_json("unavailable", Some(reason), since_millis, None)
}

/// Usage line for `sunshine-status` issued without a `<host> <port>` body.
pub fn resp_sunshine_status_usage() -> String {
    "error:usage: sunshine-status <host> <port>".to_string()
}

/// Usage line for `wol` issued without a `<host>` body.
pub fn resp_wol_usage() -> String {
    "error:usage: wol <host>".to_string()
}

/// Usage line for `net-throughput` issued without an `<iface>` body.
pub fn resp_net_throughput_usage() -> String {
    "error:usage: net-throughput <iface>".to_string()
}

/// Usage line for `net-ping` issued without a `<host>` body.
pub fn resp_net_ping_usage() -> String {
    "error:usage: net-ping <host> [count]".to_string()
}

/// Usage line for `steam-launch` issued without a numeric `<appid>` body.
pub fn resp_steam_launch_usage() -> String {
    "error:usage: steam-launch <appid>".to_string()
}

/// Usage line for `steam-quit` issued without a numeric `<appid>` body.
pub fn resp_steam_quit_usage() -> String {
    "error:usage: steam-quit <appid>".to_string()
}

/// Usage line for `steam-set-host` issued without a `<name>` body.
pub fn resp_steam_set_host_usage() -> String {
    "error:usage: steam-set-host <name>".to_string()
}

/// Usage line for `moonlight-forget` issued without a `<host>` body.
pub fn resp_moonlight_forget_usage() -> String {
    "error:usage: moonlight-forget <host>".to_string()
}

/// `power-can-suspend` reply: `yes` / `no`.
pub fn resp_yes_no(yes: bool) -> String {
    if yes {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

pub fn resp_cancelled() -> String {
    "cancelled".to_string()
}

/// Compact single-line JSON object mapping action -> button code name, in the
/// given order. Mirrors Python `json.dumps(result, separators=(",", ":"))`.
pub fn resp_bindings(ordered: &[(String, String)]) -> String {
    let mut map = serde_json::Map::new();
    for (action, name) in ordered {
        map.insert(action.clone(), serde_json::Value::String(name.clone()));
    }
    serde_json::to_string(&serde_json::Value::Object(map)).expect("bindings serialize")
}

/// One pad's compact-JSON object `{id,index,name,grabbed}`. Shared shape for the
/// `pad:connected:<json>` event body (sans `grabbed`) and the `get-pads` array
/// elements. `grabbed` is omitted from the connect event (the slot is always
/// grabbed at connect) and present in `get-pads` so the UI can show per-pad grab
/// state.
fn pad_value(id: &str, index: u8, name: &str, grabbed: Option<bool>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("index".into(), serde_json::Value::Number(index.into()));
    obj.insert("name".into(), serde_json::Value::String(name.to_string()));
    if let Some(g) = grabbed {
        obj.insert("grabbed".into(), serde_json::Value::Bool(g));
    }
    serde_json::Value::Object(obj)
}

/// Compact single-line JSON object for a `pad:connected:<json>` event body.
pub fn pad_connected_json(id: &str, index: u8, name: &str) -> String {
    serde_json::to_string(&pad_value(id, index, name, None)).expect("pad serialize")
}

/// Compact single-line JSON object for a `pad:index:<json>` event body
/// (#101 LED). Shape: `{"id":..,"index":..}`.
pub fn pad_index_json(id: &str, index: u8) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("index".into(), serde_json::Value::Number(index.into()));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("pad index serialize")
}

/// Compact single-line JSON object for a `pad:battery:<json>` event body
/// (#100 battery). Shape: `{"id":..,"level":..,"charging":..}` where `level` is
/// the charge percentage (0..=100) and `charging` a bool.
pub fn pad_battery_json(id: &str, level: u8, charging: bool) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("level".into(), serde_json::Value::Number(level.into()));
    obj.insert("charging".into(), serde_json::Value::Bool(charging));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("pad battery serialize")
}

/// Compact single-line JSON array body for the `get-pads` reply. `pads` is
/// `(id, index, name, grabbed)` per connected pad; the caller should pass them
/// in ascending player-index order for a stable wire.
pub fn resp_pads(pads: &[(String, u8, String, bool)]) -> String {
    let arr: Vec<serde_json::Value> = pads
        .iter()
        .map(|(id, index, name, grabbed)| pad_value(id, *index, name, Some(*grabbed)))
        .collect();
    serde_json::to_string(&serde_json::Value::Array(arr)).expect("pads serialize")
}

/// One row of the `list-input-devices` diagnostics enumerator (#97). Carries the
/// raw fields the Linux runtime read from evdev / `/proc/bus/input/devices`; the
/// wire JSON is built by [`resp_input_devices`]. `vendor`/`product` are the raw
/// 16-bit ids (rendered as 4-hex-digit lowercase strings on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDeviceInfo {
    pub name: String,
    pub path: String,
    pub vendor: u16,
    pub product: u16,
    pub phys: String,
    pub handlers: Vec<String>,
    pub grabbed: bool,
}

/// Compact single-line JSON array body for the `list-input-devices` reply (#97).
/// One object per controller-like input device:
/// `{"name","path","vendor","product","phys","handlers":[..],"grabbed"}`.
/// `vendor`/`product` are 4-hex-digit lowercase strings (e.g. `"045e"`). The
/// caller passes the devices already ordered (by devnode path) for a stable
/// wire; an empty list serializes to `[]`.
pub fn resp_input_devices(devices: &[InputDeviceInfo]) -> String {
    let arr: Vec<serde_json::Value> = devices
        .iter()
        .map(|d| {
            let mut obj = serde_json::Map::new();
            obj.insert("name".into(), serde_json::Value::String(d.name.clone()));
            obj.insert("path".into(), serde_json::Value::String(d.path.clone()));
            obj.insert(
                "vendor".into(),
                serde_json::Value::String(format!("{:04x}", d.vendor)),
            );
            obj.insert(
                "product".into(),
                serde_json::Value::String(format!("{:04x}", d.product)),
            );
            obj.insert("phys".into(), serde_json::Value::String(d.phys.clone()));
            obj.insert(
                "handlers".into(),
                serde_json::Value::Array(
                    d.handlers
                        .iter()
                        .map(|h| serde_json::Value::String(h.clone()))
                        .collect(),
                ),
            );
            obj.insert("grabbed".into(), serde_json::Value::Bool(d.grabbed));
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::to_string(&serde_json::Value::Array(arr)).expect("input-devices serialize")
}

/// Usage line for a `pad-battery` issued without a `<id>` body.
pub fn resp_pad_battery_usage() -> String {
    "error:usage: pad-battery <id>".to_string()
}

/// Usage line for a `pad-rumble-status` issued without a `<id>` body.
pub fn resp_pad_rumble_status_usage() -> String {
    "error:usage: pad-rumble-status <id>".to_string()
}

/// Compact JSON reply for `pad-battery <id>` when the pad has no battery
/// (wired pad or battery state unknown).
pub fn resp_pad_battery_not_present(id: &str) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("present".into(), serde_json::Value::Bool(false));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("pad battery serialize")
}

/// Compact JSON reply for `pad-battery <id>` when the pad has a battery.
/// `level` is 0..=100, `charging` is true when charging.
pub fn resp_pad_battery_present(id: &str, level: u8, charging: bool) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("present".into(), serde_json::Value::Bool(true));
    obj.insert("level".into(), serde_json::Value::Number(level.into()));
    obj.insert("charging".into(), serde_json::Value::Bool(charging));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("pad battery serialize")
}

/// Compact JSON reply for `pad-rumble-status <id>`.
/// `supported` — pad advertises EV_FF/FF_RUMBLE; `enabled` — the daemon's
/// `rumbleEnabled` setting is true.
pub fn resp_pad_rumble_status(id: &str, supported: bool, enabled: bool) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(id.to_string()));
    obj.insert("supported".into(), serde_json::Value::Bool(supported));
    obj.insert("enabled".into(), serde_json::Value::Bool(enabled));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("pad rumble status serialize")
}

/// Error reply when a `pad-battery` or `pad-rumble-status` references a pad id
/// that is not in the current fleet.
pub fn resp_pad_not_found(id: &str) -> String {
    format!("error:pad not found '{id}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_commands() {
        assert_eq!(Command::parse("grab"), Command::Grab);
        assert_eq!(Command::parse("release"), Command::Release);
        assert_eq!(Command::parse("handoff"), Command::Handoff);
        assert_eq!(Command::parse("status"), Command::Status);
        assert_eq!(Command::parse("subscribe"), Command::Subscribe);
        assert_eq!(Command::parse("get-bindings"), Command::GetBindings);
        assert_eq!(Command::parse("capture-next"), Command::CaptureNext);
        assert_eq!(Command::parse("capture-cancel"), Command::CaptureCancel);
        assert_eq!(Command::parse("get-pads"), Command::GetPads);
        assert_eq!(Command::parse("  get-pads  "), Command::GetPads);
        assert_eq!(
            Command::parse("list-input-devices"),
            Command::ListInputDevices
        );
        assert_eq!(
            Command::parse("  list-input-devices  "),
            Command::ListInputDevices
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(Command::parse("  grab  "), Command::Grab);
        assert_eq!(Command::parse("grab\r"), Command::Grab);
    }

    #[test]
    fn parses_set_binding() {
        assert_eq!(
            Command::parse("set-binding select BTN_SOUTH"),
            Command::SetBinding {
                action: "select".into(),
                button: "BTN_SOUTH".into()
            }
        );
    }

    #[test]
    fn set_binding_arg_errors() {
        // One token after the prefix -> usage.
        assert_eq!(
            Command::parse("set-binding select"),
            Command::SetBindingUsage
        );
        // Bare `set-binding` (no trailing space/args) -> unknown, matching Python.
        assert_eq!(Command::parse("set-binding"), Command::Unknown);
    }

    #[test]
    fn set_binding_extra_tokens_match_python_split() {
        // Python `split(None, 2)` keeps the remainder as the button name, so
        // extra tokens become part of an (invalid) button, not a usage error.
        // Leading/internal whitespace runs collapse like Python's split.
        assert_eq!(
            Command::parse("set-binding select BTN_SOUTH EXTRA"),
            Command::SetBinding {
                action: "select".into(),
                button: "BTN_SOUTH EXTRA".into()
            }
        );
        assert_eq!(
            Command::parse("set-binding   select    BTN_SOUTH"),
            Command::SetBinding {
                action: "select".into(),
                button: "BTN_SOUTH".into()
            }
        );
    }

    #[test]
    fn unrecognized_is_unknown() {
        assert_eq!(Command::parse("frobnicate"), Command::Unknown);
        // `kbd-log` is gone entirely (keyboard snoop removed in Phase 2).
        assert_eq!(Command::parse("kbd-log on"), Command::Unknown);
        assert_eq!(Command::parse("kbd-log maybe"), Command::Unknown);
        assert_eq!(Command::parse(""), Command::Unknown);
    }

    #[test]
    fn parses_phase2_simple_commands() {
        assert_eq!(Command::parse("list-apps"), Command::ListApps);
        assert_eq!(Command::parse("get-config"), Command::GetConfig);
        assert_eq!(Command::parse("get-recents"), Command::GetRecents);
        assert_eq!(Command::parse("  list-apps  "), Command::ListApps);
    }

    #[test]
    fn parses_set_config_body() {
        assert_eq!(
            Command::parse(r#"set-config {"themeMode":"dark"}"#),
            Command::SetConfig(r#"{"themeMode":"dark"}"#.into())
        );
        // Body is trimmed of surrounding whitespace.
        assert_eq!(
            Command::parse("set-config   {\"a\":1}  "),
            Command::SetConfig(r#"{"a":1}"#.into())
        );
        // Bare command (no body) -> usage.
        assert_eq!(Command::parse("set-config"), Command::SetConfigUsage);
        assert_eq!(Command::parse("set-config   "), Command::SetConfigUsage);
        // Word boundary: `set-configX` is NOT set-config.
        assert_eq!(Command::parse("set-configX"), Command::Unknown);
    }

    #[test]
    fn parses_record_launch_body() {
        assert_eq!(
            Command::parse(r#"record-launch {"name":"Firefox","exec":"firefox"}"#),
            Command::RecordLaunch(r#"{"name":"Firefox","exec":"firefox"}"#.into())
        );
        assert_eq!(Command::parse("record-launch"), Command::RecordLaunchUsage);
        assert_eq!(Command::parse("record-launchX"), Command::Unknown);
    }

    #[test]
    fn parses_notification_bare_command() {
        assert_eq!(
            Command::parse("get-notifications"),
            Command::GetNotifications
        );
        assert_eq!(
            Command::parse("  get-notifications  "),
            Command::GetNotifications
        );
        // Word boundary.
        assert_eq!(Command::parse("get-notificationsX"), Command::Unknown);
    }

    #[test]
    fn parses_record_notification_body() {
        assert_eq!(
            Command::parse(r#"record-notification {"id":1,"title":"Alert","message":"","level":"info","source":"system","icon":""}"#),
            Command::RecordNotification(r#"{"id":1,"title":"Alert","message":"","level":"info","source":"system","icon":""}"#.into())
        );
        // Bare command (no body) -> usage.
        assert_eq!(
            Command::parse("record-notification"),
            Command::RecordNotificationUsage
        );
        assert_eq!(
            Command::parse("record-notification   "),
            Command::RecordNotificationUsage
        );
        // Word boundary.
        assert_eq!(Command::parse("record-notificationX"), Command::Unknown);
    }

    #[test]
    fn parses_set_notifications_body() {
        assert_eq!(
            Command::parse(r#"set-notifications [{"id":1,"title":"A","message":"","level":"info","source":"system","icon":"","time":1.0}]"#),
            Command::SetNotifications(r#"[{"id":1,"title":"A","message":"","level":"info","source":"system","icon":"","time":1.0}]"#.into())
        );
        // Bare command -> usage.
        assert_eq!(
            Command::parse("set-notifications"),
            Command::SetNotificationsUsage
        );
        assert_eq!(
            Command::parse("set-notifications   "),
            Command::SetNotificationsUsage
        );
        // Word boundary.
        assert_eq!(Command::parse("set-notificationsX"), Command::Unknown);
    }

    #[test]
    fn notification_usage_strings() {
        assert_eq!(
            resp_record_notification_usage(),
            "error:usage: record-notification <json-object>"
        );
        assert_eq!(
            resp_set_notifications_usage(),
            "error:usage: set-notifications <json-array>"
        );
    }

    #[test]
    fn hypr_openwindow_closewindow_event_wire_strings() {
        let json = r#"{"address":"0x1","class":"steam","title":"Steam","workspace":"1"}"#;
        assert_eq!(
            Event::HyprOpenWindow(json.to_string()).to_string(),
            format!("hypr:openwindow:{json}")
        );
        assert_eq!(
            Event::HyprCloseWindow("0x1".to_string()).to_string(),
            "hypr:closewindow:0x1"
        );
        // Empty address is allowed (degenerate case).
        assert_eq!(
            Event::HyprCloseWindow(String::new()).to_string(),
            "hypr:closewindow:"
        );
    }

    #[test]
    fn parses_intent_command() {
        // Valid name token -> Intent (vocabulary is NOT checked at parse time).
        assert_eq!(
            Command::parse("intent home-tap"),
            Command::Intent("home-tap".into())
        );
        assert_eq!(
            Command::parse("intent home"),
            Command::Intent("home".into())
        );
        // An unknown-but-well-formed token still parses; the runtime rejects it.
        assert_eq!(
            Command::parse("intent frobnicate"),
            Command::Intent("frobnicate".into())
        );
        // Body is whitespace-trimmed.
        assert_eq!(
            Command::parse("  intent   menu  "),
            Command::Intent("menu".into())
        );
        // Bare command (no name) -> usage.
        assert_eq!(Command::parse("intent"), Command::IntentUsage);
        assert_eq!(Command::parse("intent   "), Command::IntentUsage);
        // Word boundary: `intentX` is NOT intent.
        assert_eq!(Command::parse("intentX"), Command::Unknown);
    }

    #[test]
    fn parses_key_command() {
        // Valid token -> Key (vocabulary is NOT checked at parse time).
        assert_eq!(Command::parse("key up"), Command::Key("up".into()));
        assert_eq!(Command::parse("key select"), Command::Key("select".into()));
        // An unknown-but-well-formed token still parses; the runtime rejects it.
        assert_eq!(
            Command::parse("key frobnicate"),
            Command::Key("frobnicate".into())
        );
        // Body is whitespace-trimmed.
        assert_eq!(
            Command::parse("  key   down  "),
            Command::Key("down".into())
        );
        // Bare command (no name) -> usage.
        assert_eq!(Command::parse("key"), Command::KeyUsage);
        assert_eq!(Command::parse("key   "), Command::KeyUsage);
        // Word boundary: `keyX` is NOT key.
        assert_eq!(Command::parse("keyX"), Command::Unknown);
    }

    #[test]
    fn intent_event_wire_strings_round_trip() {
        // Each closed-vocabulary intent renders as `intent:<name>`.
        for &name in INTENT_VOCAB {
            assert_eq!(
                Event::Intent(name.to_string()).to_string(),
                format!("intent:{name}")
            );
        }
    }

    #[test]
    fn phase2_usage_strings() {
        assert_eq!(
            resp_set_config_usage(),
            "error:usage: set-config <json-object>"
        );
        assert_eq!(
            resp_record_launch_usage(),
            "error:usage: record-launch <json-object>"
        );
        assert_eq!(resp_error("bad body"), "error:bad body");
        assert_eq!(resp_intent_usage(), "error:usage: intent <name>");
        assert_eq!(
            resp_unknown_intent("frobnicate"),
            "error:unknown intent 'frobnicate'"
        );
    }

    #[test]
    fn event_wire_strings() {
        assert_eq!(Event::ControllerWake.to_string(), "controller-wake");
        assert_eq!(
            Event::ControllerDisconnected.to_string(),
            "controller-disconnected"
        );
        assert_eq!(Event::ComboEndSession.to_string(), "combo:end-session");
        assert_eq!(Event::ComboForceQuit.to_string(), "combo:force-quit");
        assert_eq!(
            Event::ComboSuspendStream.to_string(),
            "combo:suspend-stream"
        );
        assert_eq!(
            Event::InputMode(InputMode::Controller).to_string(),
            "input-mode:controller"
        );
        assert_eq!(
            Event::InputMode(InputMode::Mouse).to_string(),
            "input-mode:mouse"
        );
        assert_eq!(
            Event::Buttons("Home + B".into()).to_string(),
            "buttons:Home + B"
        );
        assert_eq!(Event::Buttons(String::new()).to_string(), "buttons:");
    }

    #[test]
    fn response_strings() {
        assert_eq!(resp_ok(), "ok");
        assert_eq!(resp_unknown(), "unknown");
        assert_eq!(resp_subscribed(), "subscribed");
        assert_eq!(resp_status(true, true), "connected:grabbed");
        assert_eq!(resp_status(false, false), "disconnected:released");
        assert_eq!(
            resp_set_binding_usage(),
            "error:usage: set-binding <action> <button_name>"
        );
        assert_eq!(
            resp_unknown_action("drawer"),
            "error:unknown action 'drawer'"
        );
        assert_eq!(
            resp_invalid_button("BTN_LEFT"),
            "error:invalid button 'BTN_LEFT'"
        );
        assert_eq!(resp_captured("BTN_SOUTH"), "captured:BTN_SOUTH");
        assert_eq!(resp_timeout(), "timeout");
        assert_eq!(resp_cancelled(), "cancelled");
    }

    #[test]
    fn parses_phase3_bare_commands() {
        assert_eq!(Command::parse("bt-power-status"), Command::BtPowerStatus);
        assert_eq!(Command::parse("bt-power-on"), Command::BtPowerOn);
        assert_eq!(Command::parse("bt-power-off"), Command::BtPowerOff);
        assert_eq!(Command::parse("bt-scan-on"), Command::BtScanOn);
        assert_eq!(Command::parse("bt-scan-off"), Command::BtScanOff);
        assert_eq!(Command::parse("bt-list"), Command::BtList);
        assert_eq!(Command::parse("net-status"), Command::NetStatus);
        assert_eq!(Command::parse("net-wifi-list"), Command::NetWifiList);
        assert_eq!(Command::parse("net-wifi-rescan"), Command::NetWifiRescan);
        assert_eq!(
            Command::parse("power-can-suspend"),
            Command::PowerCanSuspend
        );
        assert_eq!(Command::parse("power-suspend"), Command::PowerSuspend);
        assert_eq!(Command::parse("power-battery"), Command::PowerBattery);
        assert_eq!(Command::parse("  bt-list  "), Command::BtList);
    }

    #[test]
    fn parses_phase3_mac_commands() {
        assert_eq!(
            Command::parse("bt-connect AA:BB:CC:DD:EE:FF"),
            Command::BtConnect("AA:BB:CC:DD:EE:FF".into())
        );
        assert_eq!(
            Command::parse("bt-disconnect AA:BB:CC:DD:EE:FF"),
            Command::BtDisconnect("AA:BB:CC:DD:EE:FF".into())
        );
        assert_eq!(
            Command::parse("bt-pair AA:BB:CC:DD:EE:FF"),
            Command::BtPair("AA:BB:CC:DD:EE:FF".into())
        );
        assert_eq!(
            Command::parse("bt-trust AA:BB:CC:DD:EE:FF"),
            Command::BtTrust("AA:BB:CC:DD:EE:FF".into())
        );
        // Body is trimmed.
        assert_eq!(
            Command::parse("bt-connect   AA:BB:CC:DD:EE:FF  "),
            Command::BtConnect("AA:BB:CC:DD:EE:FF".into())
        );
    }

    #[test]
    fn phase3_mac_usage_and_word_boundary() {
        assert_eq!(
            Command::parse("bt-connect"),
            Command::BtMacUsage("bt-connect")
        );
        assert_eq!(
            Command::parse("bt-disconnect   "),
            Command::BtMacUsage("bt-disconnect")
        );
        assert_eq!(Command::parse("bt-pair"), Command::BtMacUsage("bt-pair"));
        assert_eq!(Command::parse("bt-trust"), Command::BtMacUsage("bt-trust"));
        // Word boundary: a longer word is NOT a MAC command.
        assert_eq!(Command::parse("bt-connectX"), Command::Unknown);
        assert_eq!(Command::parse("bt-listX"), Command::Unknown);
    }

    #[test]
    fn phase3_event_wire_strings() {
        assert_eq!(Event::BtPowered(true).to_string(), "bt:powered:on");
        assert_eq!(Event::BtPowered(false).to_string(), "bt:powered:off");
        assert_eq!(
            Event::BtDevice(r#"{"mac":"AA","name":"Pad"}"#.into()).to_string(),
            r#"bt:device:{"mac":"AA","name":"Pad"}"#
        );
        assert_eq!(
            Event::BtDeviceRemoved("AA:BB:CC:DD:EE:FF".into()).to_string(),
            "bt:device-removed:AA:BB:CC:DD:EE:FF"
        );
        assert_eq!(Event::BtScanning(true).to_string(), "bt:scanning:on");
        assert_eq!(Event::BtScanning(false).to_string(), "bt:scanning:off");
        assert_eq!(
            Event::NetConnectivity("full".into()).to_string(),
            "net:connectivity:full"
        );
        assert_eq!(
            Event::NetWifi(r#"{"connectivity":"full"}"#.into()).to_string(),
            r#"net:wifi:{"connectivity":"full"}"#
        );
        assert_eq!(
            Event::NetPrimary("Wired connection 1".into()).to_string(),
            "net:primary:Wired connection 1"
        );
        assert_eq!(Event::NetPrimary(String::new()).to_string(), "net:primary:");
        assert_eq!(
            Event::PowerBattery(r#"{"present":false}"#.into()).to_string(),
            r#"power:battery:{"present":false}"#
        );
    }

    #[test]
    fn parses_phase4_bare_commands() {
        assert_eq!(Command::parse("hypr-active"), Command::HyprActive);
        assert_eq!(Command::parse("hypr-clients"), Command::HyprClients);
        assert_eq!(Command::parse("  hypr-active  "), Command::HyprActive);
        // Word boundary: a longer word is NOT a hypr command.
        assert_eq!(Command::parse("hypr-activeX"), Command::Unknown);
        assert_eq!(Command::parse("hypr-clientsX"), Command::Unknown);
    }

    #[test]
    fn parses_phase4_hypr_monitors_command() {
        assert_eq!(Command::parse("hypr-monitors"), Command::HyprMonitors);
        assert_eq!(Command::parse("  hypr-monitors  "), Command::HyprMonitors);
        // Word boundary: a longer word is NOT hypr-monitors.
        assert_eq!(Command::parse("hypr-monitorsX"), Command::Unknown);
    }

    #[test]
    fn parses_sunshine_status_body() {
        assert_eq!(
            Command::parse("sunshine-status 192.0.2.1 47990"),
            Command::SunshineStatus {
                host: "192.0.2.1".into(),
                port: "47990".into()
            }
        );
        // Surrounding/internal whitespace collapses like split_whitespace.
        assert_eq!(
            Command::parse("sunshine-status   host   1234  "),
            Command::SunshineStatus {
                host: "host".into(),
                port: "1234".into()
            }
        );
        // Missing port -> usage; bare command -> usage.
        assert_eq!(
            Command::parse("sunshine-status host"),
            Command::SunshineStatusUsage
        );
        assert_eq!(
            Command::parse("sunshine-status"),
            Command::SunshineStatusUsage
        );
        // Word boundary: `sunshine-statusX` is NOT sunshine-status.
        assert_eq!(Command::parse("sunshine-statusX"), Command::Unknown);
    }

    #[test]
    fn parses_wol_body() {
        assert_eq!(
            Command::parse("wol 192.0.2.1"),
            Command::Wol {
                host: "192.0.2.1".into()
            }
        );
        // Surrounding/internal whitespace is trimmed.
        assert_eq!(
            Command::parse("  wol   host-1  "),
            Command::Wol {
                host: "host-1".into()
            }
        );
        // Missing host -> usage; bare command -> usage.
        assert_eq!(Command::parse("wol"), Command::WolUsage);
        assert_eq!(Command::parse("wol   "), Command::WolUsage);
        // Word boundary: `wolX` is NOT wol.
        assert_eq!(Command::parse("wolX"), Command::Unknown);
    }

    #[test]
    fn wol_usage_string() {
        assert_eq!(resp_wol_usage(), "error:usage: wol <host>");
    }

    #[test]
    fn parses_net_throughput_body() {
        assert_eq!(
            Command::parse("net-throughput eth0"),
            Command::NetThroughput {
                iface: "eth0".into()
            }
        );
        // Surrounding whitespace trimmed; only the first token is the iface.
        assert_eq!(
            Command::parse("  net-throughput   wlan0  "),
            Command::NetThroughput {
                iface: "wlan0".into()
            }
        );
        // Missing iface -> usage; bare command -> usage.
        assert_eq!(
            Command::parse("net-throughput"),
            Command::NetThroughputUsage
        );
        assert_eq!(
            Command::parse("net-throughput   "),
            Command::NetThroughputUsage
        );
        // Word boundary: `net-throughputX` is NOT net-throughput.
        assert_eq!(Command::parse("net-throughputX"), Command::Unknown);
    }

    #[test]
    fn parses_net_ping_body() {
        // Host only -> count defaults to 1.
        assert_eq!(
            Command::parse("net-ping 1.1.1.1"),
            Command::NetPing {
                host: "1.1.1.1".into(),
                count: 1
            }
        );
        // Host + explicit count.
        assert_eq!(
            Command::parse("net-ping 1.1.1.1 3"),
            Command::NetPing {
                host: "1.1.1.1".into(),
                count: 3
            }
        );
        // Non-numeric count falls back to the default.
        assert_eq!(
            Command::parse("net-ping host abc"),
            Command::NetPing {
                host: "host".into(),
                count: 1
            }
        );
        // Missing host -> usage; bare command -> usage.
        assert_eq!(Command::parse("net-ping"), Command::NetPingUsage);
        assert_eq!(Command::parse("net-ping   "), Command::NetPingUsage);
        // Word boundary: `net-pingX` is NOT net-ping.
        assert_eq!(Command::parse("net-pingX"), Command::Unknown);
    }

    #[test]
    fn net_read_usage_strings() {
        assert_eq!(
            resp_net_throughput_usage(),
            "error:usage: net-throughput <iface>"
        );
        assert_eq!(
            resp_net_ping_usage(),
            "error:usage: net-ping <host> [count]"
        );
    }

    #[test]
    fn parses_steam_library_bare() {
        assert_eq!(Command::parse("steam-library"), Command::SteamLibrary);
        assert_eq!(Command::parse("  steam-library  "), Command::SteamLibrary);
        // Word boundary: `steam-libraryX` is NOT steam-library.
        assert_eq!(Command::parse("steam-libraryX"), Command::Unknown);
    }

    #[test]
    fn parses_steam_launch_body() {
        assert_eq!(
            Command::parse("steam-launch 730"),
            Command::SteamLaunch(730)
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            Command::parse("  steam-launch   220  "),
            Command::SteamLaunch(220)
        );
        // Missing appid -> usage.
        assert_eq!(Command::parse("steam-launch"), Command::SteamLaunchUsage);
        // Non-numeric appid -> usage.
        assert_eq!(
            Command::parse("steam-launch abc"),
            Command::SteamLaunchUsage
        );
        // Word boundary: `steam-launchX` is NOT steam-launch.
        assert_eq!(Command::parse("steam-launchX"), Command::Unknown);
    }

    #[test]
    fn steam_launch_usage_string() {
        assert_eq!(
            resp_steam_launch_usage(),
            "error:usage: steam-launch <appid>"
        );
    }

    #[test]
    fn parses_steam_suspend_bare() {
        assert_eq!(Command::parse("steam-suspend"), Command::SteamSuspend);
        assert_eq!(Command::parse("  steam-suspend  "), Command::SteamSuspend);
        // Word boundary: `steam-suspendX` is NOT steam-suspend.
        assert_eq!(Command::parse("steam-suspendX"), Command::Unknown);
        // It takes no argument, so a trailing token is not the command either.
        assert_eq!(Command::parse("steam-suspend now"), Command::Unknown);
        // And it must not be confused with the LOCAL (TV box) suspend.
        assert_ne!(Command::parse("power-suspend"), Command::SteamSuspend);
    }

    #[test]
    fn parses_steam_bigpicture_bare() {
        assert_eq!(Command::parse("steam-bigpicture"), Command::SteamBigPicture);
        assert_eq!(
            Command::parse("  steam-bigpicture  "),
            Command::SteamBigPicture
        );
        // Word boundary: `steam-bigpictureX` is NOT steam-bigpicture.
        assert_eq!(Command::parse("steam-bigpictureX"), Command::Unknown);
    }

    #[test]
    fn parses_steam_hosts_bare() {
        assert_eq!(Command::parse("steam-hosts"), Command::SteamHosts);
        assert_eq!(Command::parse("  steam-hosts  "), Command::SteamHosts);
        // Word boundary: `steam-hostsX` is NOT steam-hosts.
        assert_eq!(Command::parse("steam-hostsX"), Command::Unknown);
    }

    #[test]
    fn parses_steam_set_host_body() {
        assert_eq!(
            Command::parse("steam-set-host desktop-2"),
            Command::SteamSetHost("desktop-2".into())
        );
        // The name is taken verbatim (names may contain spaces).
        assert_eq!(
            Command::parse("steam-set-host Living Room PC"),
            Command::SteamSetHost("Living Room PC".into())
        );
        // Missing name -> usage.
        assert_eq!(Command::parse("steam-set-host"), Command::SteamSetHostUsage);
        assert_eq!(
            Command::parse("steam-set-host   "),
            Command::SteamSetHostUsage
        );
        // Word boundary: `steam-set-hostX` is NOT steam-set-host.
        assert_eq!(Command::parse("steam-set-hostX abc"), Command::Unknown);
    }

    #[test]
    fn steam_set_host_usage_string() {
        assert_eq!(
            resp_steam_set_host_usage(),
            "error:usage: steam-set-host <name>"
        );
    }

    #[test]
    fn parses_steam_quit_body() {
        assert_eq!(Command::parse("steam-quit 730"), Command::SteamQuit(730));
        // Surrounding whitespace is trimmed.
        assert_eq!(
            Command::parse("  steam-quit   220  "),
            Command::SteamQuit(220)
        );
        // Missing appid -> usage.
        assert_eq!(Command::parse("steam-quit"), Command::SteamQuitUsage);
        // Non-numeric appid -> usage.
        assert_eq!(Command::parse("steam-quit abc"), Command::SteamQuitUsage);
        // Word boundary: `steam-quitX` is NOT steam-quit.
        assert_eq!(Command::parse("steam-quitX"), Command::Unknown);
    }

    #[test]
    fn steam_quit_usage_string() {
        assert_eq!(resp_steam_quit_usage(), "error:usage: steam-quit <appid>");
    }

    #[test]
    fn parses_moonlight_forget_body() {
        assert_eq!(
            Command::parse("moonlight-forget 192.0.2.1"),
            Command::MoonlightForget("192.0.2.1".into())
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            Command::parse("  moonlight-forget   host-1  "),
            Command::MoonlightForget("host-1".into())
        );
        // Missing host -> usage.
        assert_eq!(
            Command::parse("moonlight-forget"),
            Command::MoonlightForgetUsage
        );
        // Word boundary: `moonlight-forgetX` is NOT moonlight-forget.
        assert_eq!(Command::parse("moonlight-forgetX"), Command::Unknown);
    }

    #[test]
    fn phase4_event_wire_strings() {
        assert_eq!(
            Event::HyprActiveWindow("firefox".into()).to_string(),
            "hypr:activewindow:firefox"
        );
        // Empty class is allowed (no focused window).
        assert_eq!(
            Event::HyprActiveWindow(String::new()).to_string(),
            "hypr:activewindow:"
        );
        assert_eq!(Event::HyprFullscreen(true).to_string(), "hypr:fullscreen:1");
        assert_eq!(
            Event::HyprFullscreen(false).to_string(),
            "hypr:fullscreen:0"
        );
    }

    #[test]
    fn phase4_usage_string() {
        assert_eq!(
            resp_sunshine_status_usage(),
            "error:usage: sunshine-status <host> <port>"
        );
        assert_eq!(
            resp_moonlight_forget_usage(),
            "error:usage: moonlight-forget <host>"
        );
    }

    #[test]
    fn sanitize_ipc_strips_control_chars() {
        // Newlines / carriage returns / tabs become spaces; no line splitting.
        assert_eq!(sanitize_ipc("a\nb\r\nc"), "a b  c");
        assert_eq!(sanitize_ipc("tab\there"), "tab here");
        // Plain text is untouched.
        assert_eq!(sanitize_ipc("normal error text"), "normal error text");
        // The result never contains a newline, carriage return, or any control.
        let out = sanitize_ipc("multi\nline\rerror\u{0007}bell");
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert!(!out.chars().any(|c| c.is_control()));
    }

    // Pure tokio — no libcec — so this exercises the CEC actor's reply-timeout
    // safety bound on macOS/CI where the cec module itself can't compile. Uses
    // short REAL durations (no `start_paused`, which needs tokio `test-util`):
    // the hang cases resolve in a few ms because the bound itself is tiny.
    #[tokio::test]
    async fn reply_with_timeout_returns_reply_when_prompt() {
        // A future that completes well within the bound yields its reply verbatim.
        let out = reply_with_timeout(
            std::time::Duration::from_secs(15),
            "cec timeout (adapter busy)",
            async { "ok".to_string() },
        )
        .await;
        assert_eq!(out, "ok");
    }

    #[tokio::test]
    async fn reply_with_timeout_errors_when_worker_hangs() {
        // A future that never completes (a wedged worker) must yield the timeout
        // error within the bound, not hang. A 20ms bound keeps the test fast.
        let bound = std::time::Duration::from_millis(20);
        let start = std::time::Instant::now();
        let out = reply_with_timeout(
            bound,
            "cec timeout (adapter busy)",
            std::future::pending::<String>(),
        )
        .await;
        assert_eq!(out, "error:cec timeout (adapter busy)");
        // It returned at/after the bound, not before, and didn't hang.
        assert!(start.elapsed() >= bound);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // The timeout message is sanitized so a stray control char can't desync
        // the wire.
        let out = reply_with_timeout(bound, "ce\nc busy", std::future::pending::<String>()).await;
        assert_eq!(out, "error:ce c busy");
    }

    #[test]
    fn phase3_response_strings() {
        assert_eq!(resp_unsupported(), "error:unsupported on this platform");
        assert_eq!(resp_bt_power(true), "bt:on");
        assert_eq!(resp_bt_power(false), "bt:off");
        assert_eq!(resp_yes_no(true), "yes");
        assert_eq!(resp_yes_no(false), "no");
        assert_eq!(
            resp_bt_mac_usage("bt-connect"),
            "error:usage: bt-connect <mac>"
        );
        assert_eq!(resp_bt_mac_usage("bt-pair"), "error:usage: bt-pair <mac>");
    }

    #[test]
    fn pad_event_wire_strings() {
        // `pad:connected:<json>` carries a compact `{id,index,name}` object.
        assert_eq!(
            Event::PadConnected(r#"{"id":"uniq:aa","index":0,"name":"Xbox"}"#.into()).to_string(),
            r#"pad:connected:{"id":"uniq:aa","index":0,"name":"Xbox"}"#
        );
        // `pad:disconnected:<id>` carries the bare wire id.
        assert_eq!(
            Event::PadDisconnected("uniq:aa".into()).to_string(),
            "pad:disconnected:uniq:aa"
        );
    }

    #[test]
    fn pad_connected_json_is_compact_object() {
        assert_eq!(
            pad_connected_json("uniq:aa:bb", 1, "DualSense"),
            r#"{"id":"uniq:aa:bb","index":1,"name":"DualSense"}"#
        );
    }

    #[test]
    fn parses_rumble_command() {
        assert_eq!(
            Command::parse("rumble uniq:aa:bb 200"),
            Command::Rumble {
                id: "uniq:aa:bb".into(),
                ms: 200
            }
        );
        // Surrounding/internal whitespace collapses like split_whitespace.
        assert_eq!(
            Command::parse("  rumble   vp:045e:028e:/dev/input/event5   50  "),
            Command::Rumble {
                id: "vp:045e:028e:/dev/input/event5".into(),
                ms: 50
            }
        );
        // Zero duration is a valid (instant/cancel) request.
        assert_eq!(
            Command::parse("rumble uniq:aa 0"),
            Command::Rumble {
                id: "uniq:aa".into(),
                ms: 0
            }
        );
        // Missing ms / bare command / non-integer ms / negative -> usage.
        assert_eq!(Command::parse("rumble uniq:aa"), Command::RumbleUsage);
        assert_eq!(Command::parse("rumble"), Command::RumbleUsage);
        assert_eq!(Command::parse("rumble uniq:aa abc"), Command::RumbleUsage);
        assert_eq!(Command::parse("rumble uniq:aa -5"), Command::RumbleUsage);
        // Word boundary: `rumbleX` is NOT rumble.
        assert_eq!(Command::parse("rumbleX"), Command::Unknown);
    }

    #[test]
    fn rumble_usage_string() {
        assert_eq!(resp_rumble_usage(), "error:usage: rumble <id> <ms>");
    }

    #[test]
    fn pad_index_and_battery_event_wire_strings() {
        // `pad:index:<json>` carries a compact `{id,index}` object (#101 LED).
        assert_eq!(
            Event::PadIndex(r#"{"id":"uniq:aa","index":0}"#.into()).to_string(),
            r#"pad:index:{"id":"uniq:aa","index":0}"#
        );
        // `pad:battery:<json>` carries a compact `{id,level,charging}` object (#100).
        assert_eq!(
            Event::PadBattery(r#"{"id":"uniq:aa","level":80,"charging":false}"#.into()).to_string(),
            r#"pad:battery:{"id":"uniq:aa","level":80,"charging":false}"#
        );
    }

    #[test]
    fn pad_index_json_is_compact_object() {
        assert_eq!(
            pad_index_json("uniq:aa:bb", 2),
            r#"{"id":"uniq:aa:bb","index":2}"#
        );
    }

    #[test]
    fn is_known_intent_deep_links() {
        // Deep-link namespaces accepted by the daemon.
        assert!(is_known_intent("settings:bluetooth"));
        assert!(is_known_intent("overlay:volume"));
        assert!(is_known_intent("overlay:network"));
        assert!(is_known_intent("overlay:session"));
        assert!(is_known_intent("app:firefox"));
        // Unknown overlay leaf -> rejected.
        assert!(!is_known_intent("overlay:bogus"));
        // Empty leaf -> rejected for all namespaces.
        assert!(!is_known_intent("settings:"));
        assert!(!is_known_intent("app:"));
        assert!(!is_known_intent("overlay:"));
        // Unknown namespace -> rejected.
        assert!(!is_known_intent("foo:bar"));
        // Bare coarse intent still works (settings without a colon).
        assert!(is_known_intent("settings"));
        // Event wire form: intent:settings:bluetooth -> name is "settings:bluetooth".
        assert_eq!(
            Event::Intent("settings:bluetooth".into()).to_string(),
            "intent:settings:bluetooth"
        );
    }

    #[test]
    fn pad_battery_json_is_compact_object() {
        assert_eq!(
            pad_battery_json("uniq:aa:bb", 73, true),
            r#"{"id":"uniq:aa:bb","level":73,"charging":true}"#
        );
        assert_eq!(
            pad_battery_json("phys:usb-1", 100, false),
            r#"{"id":"phys:usb-1","level":100,"charging":false}"#
        );
    }

    #[test]
    fn parses_phase4_cec_bare_commands() {
        assert_eq!(Command::parse("cec-scan"), Command::CecScan);
        assert_eq!(
            Command::parse("cec-active-source"),
            Command::CecActiveSource
        );
        assert_eq!(Command::parse("  cec-scan  "), Command::CecScan);
        assert_eq!(
            Command::parse("  cec-active-source  "),
            Command::CecActiveSource
        );
        // Word boundary: a longer word is NOT a CEC command.
        assert_eq!(Command::parse("cec-scanX"), Command::Unknown);
        assert_eq!(Command::parse("cec-active-sourceX"), Command::Unknown);
    }

    #[test]
    fn parses_phase4_cec_addr_commands() {
        assert_eq!(
            Command::parse("cec-device 0"),
            Command::CecDevice("0".into())
        );
        assert_eq!(
            Command::parse("cec-power-on 0"),
            Command::CecPowerOn("0".into())
        );
        assert_eq!(
            Command::parse("cec-power-off 0"),
            Command::CecPowerOff("0".into())
        );
        // Body is trimmed.
        assert_eq!(
            Command::parse("  cec-device   5  "),
            Command::CecDevice("5".into())
        );
        // Word boundary: a longer word is NOT a CEC command.
        assert_eq!(Command::parse("cec-deviceX"), Command::Unknown);
        assert_eq!(Command::parse("cec-power-onX"), Command::Unknown);
    }

    #[test]
    fn phase4_cec_addr_usage_and_word_boundary() {
        assert_eq!(
            Command::parse("cec-device"),
            Command::CecAddrUsage("cec-device")
        );
        assert_eq!(
            Command::parse("cec-device   "),
            Command::CecAddrUsage("cec-device")
        );
        assert_eq!(
            Command::parse("cec-power-on"),
            Command::CecAddrUsage("cec-power-on")
        );
        assert_eq!(
            Command::parse("cec-power-off"),
            Command::CecAddrUsage("cec-power-off")
        );
    }

    #[test]
    fn phase4_cec_event_wire_strings() {
        assert_eq!(
            Event::CecDevice(r#"{"logicalAddress":5}"#.into()).to_string(),
            r#"cec:device:{"logicalAddress":5}"#
        );
        assert_eq!(
            Event::CecPower(r#"{"addr":5,"power":"on"}"#.into()).to_string(),
            r#"cec:power:{"addr":5,"power":"on"}"#
        );
    }

    #[test]
    fn cec_device_json_is_compact_ordered() {
        // Field order is fixed (preserve_order): logicalAddress then powerStatus.
        assert_eq!(
            cec_device_json(0, "on"),
            r#"{"logicalAddress":0,"powerStatus":"on"}"#
        );
        assert_eq!(
            cec_device_json(5, "standby"),
            r#"{"logicalAddress":5,"powerStatus":"standby"}"#
        );
    }

    #[test]
    fn cec_power_json_is_compact_ordered() {
        // addr stays a wire string; field order is addr then power.
        assert_eq!(cec_power_json("0", "on"), r#"{"addr":"0","power":"on"}"#);
        assert_eq!(
            cec_power_json("5", "sleeping"),
            r#"{"addr":"5","power":"sleeping"}"#
        );
    }

    #[test]
    fn cec_device_json_round_trips_through_event() {
        // The device builder output is exactly what the CecDevice event wraps.
        let body = cec_device_json(4, "waking");
        assert_eq!(
            Event::CecDevice(body).to_string(),
            r#"cec:device:{"logicalAddress":4,"powerStatus":"waking"}"#
        );
    }

    #[test]
    fn phase4_cec_usage_string() {
        assert_eq!(
            resp_cec_addr_usage("cec-device"),
            "error:usage: cec-device <addr>"
        );
        assert_eq!(
            resp_cec_addr_usage("cec-power-on"),
            "error:usage: cec-power-on <addr>"
        );
    }

    #[test]
    fn cec_health_and_test_parse_as_bare_commands() {
        assert_eq!(Command::parse("cec-health"), Command::CecHealth);
        assert_eq!(Command::parse("cec-test"), Command::CecTest);
        // Whitespace is trimmed (mirrors the other bare CEC commands).
        assert_eq!(Command::parse("  cec-health  "), Command::CecHealth);
        // A word-boundary collision must NOT match (not mistaken for the command).
        assert_ne!(Command::parse("cec-healthy"), Command::CecHealth);
        assert_ne!(Command::parse("cec-tested"), Command::CecTest);
    }

    #[test]
    fn cec_health_json_is_compact_ordered() {
        // Field order is fixed (preserve_order): transmit, reason, since,
        // lastError. Available states always carry a null `reason`.
        assert_eq!(
            cec_health_json("ok", None, 1000, None),
            r#"{"transmit":"ok","reason":null,"since":1000,"lastError":null}"#
        );
        assert_eq!(
            cec_health_json(
                "failing",
                None,
                2500,
                Some("active-source failed: TransmitFailed")
            ),
            r#"{"transmit":"failing","reason":null,"since":2500,"lastError":"active-source failed: TransmitFailed"}"#
        );
        assert_eq!(
            cec_health_json("unknown", None, 0, None),
            r#"{"transmit":"unknown","reason":null,"since":0,"lastError":null}"#
        );
    }

    #[test]
    fn cec_unavailable_json_carries_reason_and_null_last_error() {
        // Each of the three unavailable reasons: transmit="unavailable",
        // the given reason word, lastError always null. `no_libcec` is the
        // static reply used by the non-cec / non-Linux ipc.rs arms (since:0).
        assert_eq!(
            cec_unavailable_json("no_libcec", 0),
            r#"{"transmit":"unavailable","reason":"no_libcec","since":0,"lastError":null}"#
        );
        assert_eq!(
            cec_unavailable_json("no_adapter", 1234),
            r#"{"transmit":"unavailable","reason":"no_adapter","since":1234,"lastError":null}"#
        );
        assert_eq!(
            cec_unavailable_json("adapter_open_failed", 5678),
            r#"{"transmit":"unavailable","reason":"adapter_open_failed","since":5678,"lastError":null}"#
        );
    }

    #[test]
    fn cec_health_state_starts_unknown() {
        let s = CecHealthState::new(100);
        assert_eq!(s.transmit(), CecTransmitHealth::Unknown);
        assert_eq!(s.since_millis(), 100);
        assert_eq!(s.last_error(), None);
        // Available state → reason is null.
        assert_eq!(
            s.to_json(),
            r#"{"transmit":"unknown","reason":null,"since":100,"lastError":null}"#
        );
    }

    #[test]
    fn cec_health_unknown_to_ok_records_change_and_since() {
        let mut s = CecHealthState::new(100);
        // Unknown → Ok is a real transition: bumps `since`, returns true.
        assert!(s.record_success(200));
        assert_eq!(s.transmit(), CecTransmitHealth::Ok);
        assert_eq!(s.since_millis(), 200);
        assert_eq!(s.last_error(), None);
    }

    #[test]
    fn cec_health_repeat_success_is_no_change() {
        let mut s = CecHealthState::new(100);
        assert!(s.record_success(200));
        // A second success keeps the variant: no change, `since` is KEPT.
        assert!(!s.record_success(300));
        assert_eq!(s.transmit(), CecTransmitHealth::Ok);
        assert_eq!(s.since_millis(), 200);
    }

    #[test]
    fn cec_health_ok_to_failing_records_error_and_new_since() {
        let mut s = CecHealthState::new(100);
        s.record_success(200);
        // Ok → Failing is a real transition: records the error + a new `since`.
        assert!(s.record_failure("power-on failed: TransmitFailed", 500));
        assert_eq!(s.transmit(), CecTransmitHealth::Failing);
        assert_eq!(s.since_millis(), 500);
        assert_eq!(s.last_error(), Some("power-on failed: TransmitFailed"));
    }

    #[test]
    fn cec_health_failing_to_failing_keeps_since_but_refreshes_error() {
        let mut s = CecHealthState::new(100);
        s.record_failure("first error", 500);
        // Failing → Failing is NOT a change: `since` is kept (the wedge onset),
        // returns false, but the freshest error is stored for the next read.
        assert!(!s.record_failure("second error", 900));
        assert_eq!(s.transmit(), CecTransmitHealth::Failing);
        assert_eq!(s.since_millis(), 500);
        assert_eq!(s.last_error(), Some("second error"));
    }

    #[test]
    fn cec_health_failing_to_ok_clears_error() {
        let mut s = CecHealthState::new(100);
        s.record_failure("transient wedge", 500);
        // Failing → Ok is a real transition: clears the error + bumps `since`.
        assert!(s.record_success(1200));
        assert_eq!(s.transmit(), CecTransmitHealth::Ok);
        assert_eq!(s.since_millis(), 1200);
        assert_eq!(s.last_error(), None);
        assert_eq!(
            s.to_json(),
            r#"{"transmit":"ok","reason":null,"since":1200,"lastError":null}"#
        );
    }

    #[test]
    fn cec_health_json_round_trips_through_event() {
        // The health builder output is exactly what the CecHealth event wraps.
        let body = cec_health_json("failing", None, 4242, Some("wedged"));
        assert_eq!(
            Event::CecHealth(body).to_string(),
            r#"cec:health:{"transmit":"failing","reason":null,"since":4242,"lastError":"wedged"}"#
        );
    }

    #[test]
    fn cec_unavailable_json_round_trips_through_event() {
        // The unavailable builder output is also wrapped verbatim by CecHealth,
        // so the open-handshake-failure broadcast carries the structured reason.
        let body = cec_unavailable_json("adapter_open_failed", 9000);
        assert_eq!(
            Event::CecHealth(body).to_string(),
            r#"cec:health:{"transmit":"unavailable","reason":"adapter_open_failed","since":9000,"lastError":null}"#
        );
    }

    #[test]
    fn resp_pads_is_compact_array_with_grabbed() {
        let pads = vec![
            ("uniq:p1".to_string(), 0u8, "Xbox".to_string(), true),
            ("uniq:p2".to_string(), 1u8, "PS5".to_string(), false),
        ];
        assert_eq!(
            resp_pads(&pads),
            r#"[{"id":"uniq:p1","index":0,"name":"Xbox","grabbed":true},{"id":"uniq:p2","index":1,"name":"PS5","grabbed":false}]"#
        );
        // Empty fleet -> empty array.
        assert_eq!(resp_pads(&[]), "[]");
    }

    #[test]
    fn resp_input_devices_is_compact_array() {
        let devs = vec![
            InputDeviceInfo {
                name: "Xbox 360 Controller".into(),
                path: "/dev/input/event18".into(),
                vendor: 0x045e,
                product: 0x028e,
                phys: "usb-0000:00:14.0-1/input0".into(),
                handlers: vec!["event18".into(), "js0".into()],
                grabbed: true,
            },
            InputDeviceInfo {
                name: "Virtual Pad".into(),
                path: "/dev/input/event20".into(),
                vendor: 0x0000,
                product: 0x0000,
                phys: "".into(),
                handlers: vec!["event20".into()],
                grabbed: false,
            },
        ];
        assert_eq!(
            resp_input_devices(&devs),
            r#"[{"name":"Xbox 360 Controller","path":"/dev/input/event18","vendor":"045e","product":"028e","phys":"usb-0000:00:14.0-1/input0","handlers":["event18","js0"],"grabbed":true},{"name":"Virtual Pad","path":"/dev/input/event20","vendor":"0000","product":"0000","phys":"","handlers":["event20"],"grabbed":false}]"#
        );
        // Empty list -> empty array.
        assert_eq!(resp_input_devices(&[]), "[]");
    }

    #[test]
    fn bindings_response_is_ordered_compact_json() {
        let ordered = vec![
            ("select".to_string(), "BTN_SOUTH".to_string()),
            ("back".to_string(), "BTN_EAST".to_string()),
            ("altSelect".to_string(), "BTN_NORTH".to_string()),
            ("menu".to_string(), "BTN_START".to_string()),
        ];
        assert_eq!(
            resp_bindings(&ordered),
            r#"{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","menu":"BTN_START"}"#
        );
    }

    #[test]
    fn parses_set_active_game_body() {
        // Non-empty body -> SetActiveGame.
        assert_eq!(
            Command::parse("set-active-game steam_12345"),
            Command::SetActiveGame("steam_12345".into())
        );
        // Body is trimmed.
        assert_eq!(
            Command::parse("  set-active-game   my-game-id  "),
            Command::SetActiveGame("my-game-id".into())
        );
        // Bare command (no body) -> SetActiveGameClear.
        assert_eq!(
            Command::parse("set-active-game"),
            Command::SetActiveGameClear
        );
        assert_eq!(
            Command::parse("  set-active-game  "),
            Command::SetActiveGameClear
        );
        // Word boundary: `set-active-gameX` is NOT set-active-game.
        assert_eq!(Command::parse("set-active-gameX"), Command::Unknown);
    }

    #[test]
    fn parses_overlay_focus_command() {
        assert_eq!(
            Command::parse("overlay-focus on"),
            Command::OverlayFocus(true)
        );
        assert_eq!(
            Command::parse("overlay-focus off"),
            Command::OverlayFocus(false)
        );
        // Surrounding + inner whitespace is trimmed/collapsed by `command_body`.
        assert_eq!(
            Command::parse("  overlay-focus   on  "),
            Command::OverlayFocus(true)
        );
        // Missing arg -> usage.
        assert_eq!(Command::parse("overlay-focus"), Command::OverlayFocusUsage);
        assert_eq!(
            Command::parse("overlay-focus   "),
            Command::OverlayFocusUsage
        );
        // Invalid / extra args -> usage (only bare `on`/`off` accepted).
        assert_eq!(
            Command::parse("overlay-focus maybe"),
            Command::OverlayFocusUsage
        );
        assert_eq!(
            Command::parse("overlay-focus on now"),
            Command::OverlayFocusUsage
        );
        // Word boundary: `overlay-focusX` is NOT overlay-focus.
        assert_eq!(Command::parse("overlay-focusX"), Command::Unknown);
    }

    #[test]
    fn config_changed_event_wire_string() {
        // config:changed is payload-less — the subscriber re-fetches via get-config.
        assert_eq!(Event::ConfigChanged.to_string(), "config:changed");
    }

    #[test]
    fn parses_controllerdb_commands() {
        assert_eq!(
            Command::parse("controllerdb-status"),
            Command::ControllerDbStatus
        );
        assert_eq!(
            Command::parse("  controllerdb-status  "),
            Command::ControllerDbStatus
        );
        assert_eq!(
            Command::parse("controllerdb-refresh"),
            Command::ControllerDbRefresh
        );
        assert_eq!(
            Command::parse("  controllerdb-refresh  "),
            Command::ControllerDbRefresh
        );
        // Word boundary: a longer word is NOT the command.
        assert_eq!(Command::parse("controllerdb-statusX"), Command::Unknown);
        assert_eq!(Command::parse("controllerdb-refreshX"), Command::Unknown);
    }

    #[test]
    fn parses_pad_battery_and_rumble_status_commands() {
        assert_eq!(
            Command::parse("pad-battery uniq:aa:bb:cc"),
            Command::PadBatteryQuery("uniq:aa:bb:cc".into())
        );
        // Body is trimmed.
        assert_eq!(
            Command::parse("  pad-battery   phys:usb-1/input0  "),
            Command::PadBatteryQuery("phys:usb-1/input0".into())
        );
        // Bare command -> usage.
        assert_eq!(Command::parse("pad-battery"), Command::PadBatteryUsage);
        assert_eq!(Command::parse("pad-battery   "), Command::PadBatteryUsage);
        // Word boundary.
        assert_eq!(Command::parse("pad-batteryX"), Command::Unknown);

        assert_eq!(
            Command::parse("pad-rumble-status uniq:aa:bb"),
            Command::PadRumbleStatus("uniq:aa:bb".into())
        );
        assert_eq!(
            Command::parse("pad-rumble-status"),
            Command::PadRumbleStatusUsage
        );
        assert_eq!(Command::parse("pad-rumble-statusX"), Command::Unknown);
    }

    #[test]
    fn pad_battery_resp_builders() {
        // present=false (wired pad)
        let j = resp_pad_battery_not_present("uniq:aa");
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["id"], "uniq:aa");
        assert_eq!(v["present"], false);
        assert!(v.get("level").is_none());

        // present=true (wireless pad)
        let j = resp_pad_battery_present("phys:usb-1", 80, true);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["id"], "phys:usb-1");
        assert_eq!(v["present"], true);
        assert_eq!(v["level"], 80);
        assert_eq!(v["charging"], true);
    }

    #[test]
    fn pad_rumble_status_resp_builder() {
        let j = resp_pad_rumble_status("uniq:aa", true, false);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["id"], "uniq:aa");
        assert_eq!(v["supported"], true);
        assert_eq!(v["enabled"], false);
    }

    // --- #166: screenshot flash event ---

    #[test]
    fn screenshot_flash_event_wire_string() {
        assert_eq!(Event::ScreenshotFlash.to_string(), "screenshot:flash");
    }
}

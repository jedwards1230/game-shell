//! MQTT client for the desktop sidecar — publishes this machine's state to a
//! broker and accepts a small set of remote commands.
//!
//! # One machine, one device
//!
//! This process runs on **the desktop**: ONE physical machine that dual-boots
//! CachyOS and Windows 11. Only one OS is up at a time, and both boots share a
//! single `device_id`, a single Home Assistant device, and a single MQTT
//! username. That is why [`settings_from_env`] **fails closed** when a broker is
//! configured without an explicit `TV_SHELL_MQTT_DEVICE_ID`: deriving the id from
//! the hostname or the OS would split one machine into two devices that alternate
//! as it reboots.
//!
//! # Why `published_at`, `seq` and the floor heartbeat exist
//!
//! A client can keep "publishing" into a **half-open socket** long after the
//! broker gave up on it and fired its Last Will. That happened on this broker for
//! 13.5 hours: every consumer read `unavailable` while the publisher's own logs
//! looked perfectly healthy. Availability cannot express *"connected, but nothing
//! is arriving"* — a `published_at` that stops advancing and a `seq` that stops
//! incrementing can. The publish loop therefore emits on change **and** on a floor
//! heartbeat (default 30 s) so those two fields always move on a healthy link.
//! They are the point of the design, not bookkeeping.
//!
//! # Configuration — environment only
//!
//! The sidecar has no config file, and `brand::config_dir()` is unusable here: on
//! Windows neither `XDG_CONFIG_HOME` nor `HOME` is normally set, so it resolves to
//! a CWD-relative path. **This module never calls it.** Every knob is an env var,
//! read through [`tv_shell_protocol::brand::env`] so the legacy `GAME_SHELL_*`
//! prefix keeps working:
//!
//! | var | meaning |
//! |---|---|
//! | `TV_SHELL_MQTT_BROKER` | `mqtts://host:8883` or `mqtt://host:1883`. **Unset ⇒ MQTT off entirely.** |
//! | `TV_SHELL_MQTT_DEVICE_ID` | explicit device id. **Required when the broker is set.** |
//! | `TV_SHELL_MQTT_USERNAME` | broker username (both-or-neither with the password) |
//! | `TV_SHELL_MQTT_PASSWORD` | the password itself, not a path — Windows has no mode bits |
//! | `TV_SHELL_MQTT_CA_FILE` | path to a PEM CA bundle; unset ⇒ platform roots |
//! | `TV_SHELL_MQTT_HEARTBEAT_SECS` | floor heartbeat, default 30 |
//! | `TV_SHELL_MQTT_KEEPALIVE_SECS` | MQTT keepalive, default 60 |
//!
//! Deployment: on Linux these extend `~/.config/tv-shell-host/host.env` (0600,
//! Ansible-managed, `no_log`); on Windows they extend the existing per-user
//! environment variables, which are ACL-protected rather than mode-bit protected
//! and are readable by any process running as that user. That is **the same trust
//! model as the bearer token already deployed there — no regression** — but it is
//! not parity with Linux. There is no reload path either: the process reads the
//! environment once, so any change (including a credential rotation) needs a
//! restart of the sidecar.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rumqttc::{AsyncClient, Event, LastWill, MqttOptions, Packet, QoS, Transport};
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tv_shell_protocol::brand;
use tv_shell_protocol::mqtt::{
    host_discovery, DeviceId, DeviceOs, HostState, StateEnvelope, AVAIL_OFFLINE, AVAIL_ONLINE,
    TOPIC_ROOT,
};
use tv_shell_protocol::StatusResponse;

/// This crate's version — the `sw_version` in discovery and `status.version` in
/// state, so both sides of the wire report the same build.
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Floor heartbeat when `TV_SHELL_MQTT_HEARTBEAT_SECS` is unset.
const DEFAULT_HEARTBEAT_SECS: u64 = 30;
/// MQTT keepalive when `TV_SHELL_MQTT_KEEPALIVE_SECS` is unset. Generous on
/// purpose: the Windows scheduled task has a `PT5M` watchdog and a session-unlock
/// trigger, so reconnect churn on a broker that Zigbee and Z-Wave ride on is a
/// live risk.
const DEFAULT_KEEPALIVE_SECS: u64 = 60;

/// How often the publish loop probes Steam/Sunshine.
///
/// 5 s, not 1 s: both probes are relatively expensive here — a `/proc` scan or a
/// registry read, plus a **blocking loopback HTTPS GET to Sunshine**. (The daemon
/// ticks faster because its cheap half is in-memory.)
const PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// First reconnect delay after a connection error.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling for the exponential reconnect backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Outbound request queue handed to [`AsyncClient::new`].
const REQUEST_CAPACITY: usize = 32;

// ─────────────────────────────────────────────────────────────────────────────
// Settings
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the MQTT actor needs, fully validated at startup.
///
/// `Debug` is hand-written to redact the password — this struct is the one place
/// a broker credential lives in memory, and a stray `{settings:?}` in a log line
/// (or an `assert_eq!` failure message) must not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct MqttSettings {
    /// Validated device id — the one input every topic is built from.
    pub device_id: DeviceId,
    /// Broker host (no scheme, no brackets around an IPv6 literal).
    pub host: String,
    /// Broker port.
    pub port: u16,
    /// Whether to wrap the connection in TLS (`mqtts://`).
    pub tls: bool,
    /// Broker username; both-or-neither with [`MqttSettings::password`].
    pub username: Option<String>,
    /// Broker password.
    pub password: Option<String>,
    /// PEM CA bundle to trust instead of the platform roots.
    pub ca_file: Option<PathBuf>,
    /// Floor heartbeat — publish at least this often even when nothing changed.
    pub heartbeat: Duration,
    /// MQTT keepalive.
    pub keepalive: Duration,
}

impl std::fmt::Debug for MqttSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MqttSettings")
            .field("device_id", &self.device_id)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("ca_file", &self.ca_file)
            .field("heartbeat", &self.heartbeat)
            .field("keepalive", &self.keepalive)
            .finish()
    }
}

/// Read the MQTT configuration from the process environment.
///
/// `Ok(None)` means "no broker configured, MQTT is off" — the sidecar still
/// serves HTTP normally. `Err` means the configuration is **wrong**, and the
/// caller must fail startup rather than run on silently without MQTT.
pub fn settings_from_env() -> Result<Option<MqttSettings>, String> {
    settings_from(brand::env)
}

/// [`settings_from_env`] over an injected lookup, so the table-driven tests never
/// mutate the process environment (which would race across parallel tests).
fn settings_from<F>(env: F) -> Result<Option<MqttSettings>, String>
where
    F: Fn(&str) -> Option<String>,
{
    // An empty value is treated as unset throughout: a stray `TV_SHELL_MQTT_X=`
    // in an env file must not read as a configured-but-blank setting.
    let read = |suffix: &str| {
        env(suffix)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let Some(broker) = read("MQTT_BROKER") else {
        return Ok(None);
    };
    let (host, port, tls) = parse_broker_url(&broker)?;

    // Fail closed. The desktop is ONE dual-boot machine, so the id has to be set
    // explicitly and identically on both boots.
    let Some(raw_id) = read("MQTT_DEVICE_ID") else {
        return Err(
            "TV_SHELL_MQTT_BROKER is set but TV_SHELL_MQTT_DEVICE_ID is not: the device id must \
             be configured explicitly, and identically on both boots of this dual-boot machine. \
             Deriving it from the hostname or the OS would produce two Home Assistant devices for \
             one machine."
                .to_string(),
        );
    };
    let device_id =
        DeviceId::new(&raw_id).map_err(|e| format!("TV_SHELL_MQTT_DEVICE_ID is invalid: {e}"))?;

    // The password is NOT trimmed — leading/trailing whitespace can be part of a
    // generated secret. It is only checked for emptiness.
    let username = read("MQTT_USERNAME");
    let password = env("MQTT_PASSWORD").filter(|v| !v.is_empty());
    match (&username, &password) {
        (Some(_), None) => {
            return Err(
                "TV_SHELL_MQTT_USERNAME is set but TV_SHELL_MQTT_PASSWORD is not: set both or \
                 neither."
                    .to_string(),
            )
        }
        (None, Some(_)) => {
            return Err(
                "TV_SHELL_MQTT_PASSWORD is set but TV_SHELL_MQTT_USERNAME is not: set both or \
                 neither."
                    .to_string(),
            )
        }
        _ => {}
    }

    Ok(Some(MqttSettings {
        device_id,
        host,
        port,
        tls,
        username,
        password,
        ca_file: read("MQTT_CA_FILE").map(PathBuf::from),
        heartbeat: positive_secs(
            read("MQTT_HEARTBEAT_SECS"),
            DEFAULT_HEARTBEAT_SECS,
            "TV_SHELL_MQTT_HEARTBEAT_SECS",
        )?,
        keepalive: positive_secs(
            read("MQTT_KEEPALIVE_SECS"),
            DEFAULT_KEEPALIVE_SECS,
            "TV_SHELL_MQTT_KEEPALIVE_SECS",
        )?,
    }))
}

/// Parse a positive number of seconds, defaulting when unset.
///
/// Malformed or `0` is an **error**, never a silent fallback to the default: a
/// zero heartbeat would busy-loop the publisher, and a typo that quietly reverts
/// to the default is exactly the kind of misconfiguration this design removes.
fn positive_secs(raw: Option<String>, default: u64, var: &str) -> Result<Duration, String> {
    let Some(raw) = raw else {
        return Ok(Duration::from_secs(default));
    };
    match raw.parse::<u64>() {
        Ok(0) => Err(format!("{var} must be greater than 0 (got {raw:?})")),
        Ok(secs) => Ok(Duration::from_secs(secs)),
        Err(_) => Err(format!(
            "{var} must be a whole number of seconds (got {raw:?})"
        )),
    }
}

/// Parse a broker URL into `(host, port, tls)`.
///
/// Hand-rolled: `host/` has no `url` crate and must not gain one for two schemes.
/// Only `mqtt://` (default 1883) and `mqtts://` (default 8883) are accepted;
/// anything else, an empty host, a path, port `0`, and non-numeric ports are all
/// hard errors with their own message.
fn parse_broker_url(raw: &str) -> Result<(String, u16, bool), String> {
    let (rest, tls, default_port) = if let Some(rest) = raw.strip_prefix("mqtts://") {
        (rest, true, 8883u16)
    } else if let Some(rest) = raw.strip_prefix("mqtt://") {
        (rest, false, 1883u16)
    } else {
        return Err(format!(
            "TV_SHELL_MQTT_BROKER must start with mqtt:// or mqtts:// (got {raw:?})"
        ));
    };
    if rest.contains('/') {
        return Err(format!(
            "TV_SHELL_MQTT_BROKER must be host[:port] with no path (got {raw:?})"
        ));
    }

    // Split host from port, tolerating a bracketed IPv6 literal (`[::1]:8883`)
    // whose address itself contains colons. The brackets are stripped: rumqttc
    // resolves the host through `ToSocketAddrs`, which wants the bare address.
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let Some(close) = inner.find(']') else {
            return Err(format!(
                "TV_SHELL_MQTT_BROKER has an unterminated IPv6 literal (got {raw:?})"
            ));
        };
        let (host, after) = (&inner[..close], &inner[close + 1..]);
        match after {
            "" => (host, None),
            other => match other.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => {
                    return Err(format!(
                        "TV_SHELL_MQTT_BROKER has trailing junk after the IPv6 host (got {raw:?})"
                    ))
                }
            },
        }
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (rest, None),
        }
    };

    if host.is_empty() {
        return Err(format!(
            "TV_SHELL_MQTT_BROKER has an empty host (got {raw:?})"
        ));
    }
    let port = match port {
        None => default_port,
        Some(port) => match port.parse::<u16>() {
            Ok(0) | Err(_) => {
                return Err(format!(
                    "TV_SHELL_MQTT_BROKER has an invalid port {port:?} (got {raw:?})"
                ))
            }
            Ok(port) => port,
        },
    };
    Ok((host.to_string(), port, tls))
}

/// Read the configured PEM CA bundle, if any.
///
/// Called from `main` before the actor is spawned. An unreadable bundle is an
/// `Err` **the caller warns about and degrades from** — it does not fail startup:
/// the broker presents a publicly-trusted certificate, so the platform trust
/// store is the normal path and `ca_file` only matters for a private CA.
pub async fn load_ca(path: Option<&Path>) -> anyhow::Result<Option<Vec<u8>>> {
    match path {
        None => Ok(None),
        Some(path) => {
            let bytes = tokio::fs::read(path).await.map_err(|e| {
                anyhow::anyhow!(
                    "TV_SHELL_MQTT_CA_FILE {} is unreadable: {e}",
                    path.display()
                )
            })?;
            Ok(Some(bytes))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Actor
// ─────────────────────────────────────────────────────────────────────────────

/// Install `ring` as the process-wide rustls crypto provider.
///
/// Idempotent by design: `install_default` returns `Err` when a provider is
/// already installed, and that "error" is a no-op we deliberately ignore rather
/// than a failure.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Build the transport for the configured broker.
///
/// `ca` present ⇒ trust exactly that PEM bundle; absent ⇒ the platform roots.
fn transport(tls: bool, ca: Option<Vec<u8>>) -> Option<Transport> {
    if !tls {
        return Some(Transport::Tcp);
    }
    match ca {
        Some(ca) => Some(Transport::tls_with_config(
            rumqttc::TlsConfiguration::Simple {
                ca,
                alpn: None,
                client_auth: None,
            },
        )),
        // `TlsConfiguration::default()` loads the platform trust store and
        // **panics** if it cannot (`expect("could not load platform certs")`).
        // Catch that: a panic inside a spawned task would take MQTT down with a
        // backtrace and no actionable message. Configure TV_SHELL_MQTT_CA_FILE —
        // the private-CA path — to avoid the platform store entirely.
        None => std::panic::catch_unwind(|| {
            Transport::tls_with_config(rumqttc::TlsConfiguration::default())
        })
        .map_err(|_| {
            tracing::error!(
                "MQTT: could not load the platform trust store; set TV_SHELL_MQTT_CA_FILE to a \
                 PEM CA bundle. MQTT is disabled for this run."
            );
        })
        .ok(),
    }
}

/// Run the MQTT actor forever: connect, republish on every ConnAck, publish state
/// on change or on the floor heartbeat, and serve the command topics.
///
/// Fire-and-forget — `main` spawns this and never joins it, exactly like the
/// daemon's actors. It never returns except when the transport cannot be built.
pub async fn run(settings: MqttSettings, ca: Option<Vec<u8>>) {
    install_crypto_provider();

    let Some(transport) = transport(settings.tls, ca) else {
        return;
    };

    // Client id is the frozen `tv-shell-<device_id>` form.
    let mut opts = MqttOptions::new(
        format!("{TOPIC_ROOT}-{}", settings.device_id),
        settings.host.clone(),
        settings.port,
    );
    opts.set_keep_alive(settings.keepalive);
    opts.set_transport(transport);
    if let (Some(user), Some(pass)) = (&settings.username, &settings.password) {
        opts.set_credentials(user.clone(), pass.clone());
    }
    // The Last Will is registered with the CONNECT packet, so it must be set
    // before the client is built. Retained, so a consumer that subscribes after
    // we died still reads "offline".
    opts.set_last_will(LastWill::new(
        settings.device_id.avail_topic(),
        AVAIL_OFFLINE,
        QoS::AtLeastOnce,
        true,
    ));

    let (client, mut eventloop) = AsyncClient::new(opts, REQUEST_CAPACITY);

    // Signals the publish loop to publish immediately (used after every ConnAck).
    let force_publish = Arc::new(Notify::new());
    tokio::spawn(publish_loop(
        client.clone(),
        settings.device_id.clone(),
        settings.heartbeat,
        force_publish.clone(),
    ));

    tracing::info!(
        "MQTT: connecting to {}:{} as {} (tls={}, heartbeat={}s, keepalive={}s)",
        settings.host,
        settings.port,
        settings.device_id,
        settings.tls,
        settings.heartbeat.as_secs(),
        settings.keepalive.as_secs(),
    );

    let mut backoff = INITIAL_BACKOFF;
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                backoff = INITIAL_BACKOFF;
                tracing::info!("MQTT: connected as {}", settings.device_id);
                // Republish off the event-loop task: awaiting a publish here
                // would stop draining the request channel and could deadlock
                // against a full queue.
                tokio::spawn(on_connected(
                    client.clone(),
                    settings.device_id.clone(),
                    force_publish.clone(),
                ));
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let topic = publish.topic;
                match command_name(&topic, &settings.device_id).map(str::to_string) {
                    Some(name) => {
                        // Commands do blocking OS work; run each in its own task
                        // so the event loop keeps pinging and reading.
                        tokio::spawn(async move { dispatch_command(&name, &topic).await });
                    }
                    None => tracing::warn!("MQTT: ignoring publish on unexpected topic {topic}"),
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "MQTT: connection error ({e}); retrying in {}s",
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Republish everything after a ConnAck. A reconnect may mean the broker dropped
/// our session, so discovery, availability and the subscription are all re-sent
/// rather than assumed to have survived.
async fn on_connected(client: AsyncClient, device_id: DeviceId, force_publish: Arc<Notify>) {
    // §4a — the desktop is ONE dual-boot machine publishing a RETAINED discovery
    // message, so BOTH boots must publish a byte-identical component set. There
    // is deliberately NO `cfg!(target_os = ...)` and no `DeviceOs` anywhere on
    // this path: if the component set differed per boot, every OS switch would
    // rewrite the retained config and add/remove Home Assistant entities.
    // `current_os` belongs to the STATE payload, where flipping is correct.
    match discovery_payload(&device_id) {
        Ok(payload) => {
            if let Err(e) = client
                .publish(device_id.discovery_topic(), QoS::AtLeastOnce, true, payload)
                .await
            {
                tracing::warn!("MQTT: discovery publish failed: {e}");
            }
        }
        Err(e) => tracing::error!("MQTT: could not serialize the discovery document: {e}"),
    }

    if let Err(e) = client
        .publish(
            device_id.avail_topic(),
            QoS::AtLeastOnce,
            true,
            AVAIL_ONLINE,
        )
        .await
    {
        tracing::warn!("MQTT: availability publish failed: {e}");
    }

    if let Err(e) = client
        .subscribe(device_id.cmd_topic_filter(), QoS::AtLeastOnce)
        .await
    {
        tracing::warn!("MQTT: command subscribe failed: {e}");
    }

    force_publish.notify_one();
}

/// Serialize the Home Assistant discovery document exactly as the publish site
/// does. Pinned OS-free by `discovery_payload_has_no_os_branching`.
fn discovery_payload(device_id: &DeviceId) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&host_discovery(device_id))
}

/// Probe the host and publish the state envelope on change or on the heartbeat.
async fn publish_loop(
    client: AsyncClient,
    device_id: DeviceId,
    heartbeat: Duration,
    force_publish: Arc<Notify>,
) {
    let topic = device_id.state_topic();
    let mut ticker = tokio::time::interval(PROBE_INTERVAL);
    // A stalled probe must not queue a burst of catch-up ticks.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut state = StateBuilder::default();
    let mut last: Option<StatusResponse> = None;
    let mut last_publish = Instant::now();
    let mut forced = false;

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = force_publish.notified() => forced = true,
        }

        let status = probe_status().await;
        let changed = last.as_ref() != Some(&status);
        if !forced && !should_publish(changed, last_publish.elapsed(), heartbeat) {
            continue;
        }
        forced = false;
        last = Some(status.clone());
        last_publish = Instant::now();

        let envelope = state.next(now_unix(), status);
        match serde_json::to_vec(&envelope) {
            // `try_publish`, not `publish`: while the broker is unreachable the
            // request queue does not drain, and awaiting would park this task and
            // then flush a burst of stale envelopes on reconnect. State is
            // periodic and self-superseding, so dropping one is the right answer
            // — and the ConnAck path forces a fresh publish anyway.
            Ok(payload) => {
                if let Err(e) = client.try_publish(topic.clone(), QoS::AtLeastOnce, true, payload) {
                    tracing::debug!("MQTT: state publish dropped (link down?): {e}");
                }
            }
            Err(e) => tracing::error!("MQTT: could not serialize the state envelope: {e}"),
        }
    }
}

/// Should this tick publish? Pure, so the cadence is directly testable.
fn should_publish(changed: bool, since_last: Duration, heartbeat: Duration) -> bool {
    changed || since_last >= heartbeat
}

/// Unix seconds now, degrading to 0 if the clock is before the epoch.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Monotonic per-process sequence stamper. Starts at 0 and only ever advances —
/// a frozen `seq` is how a half-open socket becomes visible downstream.
#[derive(Default)]
struct StateBuilder {
    seq: u64,
}

impl StateBuilder {
    fn next(&mut self, published_at: u64, status: StatusResponse) -> HostState {
        let envelope = StateEnvelope::new(published_at, self.seq, DeviceOs::current(), status);
        self.seq = self.seq.wrapping_add(1);
        envelope
    }
}

/// Gather the same two signals `GET /status` publishes.
///
/// Both probes touch the OS off-band (a `/proc` scan or a registry read, plus a
/// blocking loopback HTTPS GET to Sunshine), so both go through
/// `spawn_blocking` and degrade to their safe value on a `JoinError` — exactly as
/// the HTTP handlers do.
async fn probe_status() -> StatusResponse {
    let running = tokio::task::spawn_blocking(crate::steam::running_appid)
        .await
        .unwrap_or(None);
    let streaming = tokio::task::spawn_blocking(crate::steam::streaming)
        .await
        .unwrap_or(false);
    StatusResponse {
        version: HOST_VERSION.to_string(),
        running_appid: running,
        streaming,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Commands
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the command name from a command topic, or `None` when the topic is not
/// one of ours. The payload is ignored everywhere — Home Assistant buttons send an
/// arbitrary press payload.
fn command_name<'a>(topic: &'a str, device_id: &DeviceId) -> Option<&'a str> {
    let name = topic.strip_prefix(&device_id.cmd_topic(""))?;
    // `tv-shell/<id>/cmd/+` is a single-level filter, so a name may not contain
    // another separator, and the empty name is not a command.
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some(name)
}

/// Run one command. Unknown names are logged and dropped — a command topic is a
/// remote-control surface, so accepted commands log at `info` and rejected ones at
/// `warn`, both with the topic, and nothing here may panic the actor.
///
/// **There is deliberately no `wake` command.** Home Assistant's `wake_on_lan`
/// already emits the packet correctly, and a command topic on a machine that is
/// off cannot be actioned by that machine — the entity would be unavailable
/// exactly when it is needed.
async fn dispatch_command(name: &str, topic: &str) {
    match name {
        "sleep" => {
            tracing::info!("MQTT: command accepted on {topic}");
            match crate::request_sleep().await {
                Ok(resp) if resp.ok => tracing::info!("MQTT: sleep dispatched"),
                Ok(resp) => tracing::warn!(
                    "MQTT: sleep refused — {}",
                    resp.reason.as_deref().unwrap_or("no reason given")
                ),
                Err(e) => tracing::warn!("MQTT: sleep failed: {e}"),
            }
        }
        // The HTTP route takes an explicit appid; over MQTT there is no body, so
        // the contract is "quit whatever is in the foreground".
        "quit" => {
            tracing::info!("MQTT: command accepted on {topic}");
            let running = tokio::task::spawn_blocking(crate::steam::running_appid)
                .await
                .unwrap_or(None);
            let Some(appid) = running else {
                tracing::info!("MQTT: quit — nothing running");
                return;
            };
            let result = tokio::task::spawn_blocking(move || crate::steam::quit(appid))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("quit task panicked: {e}")));
            match result {
                Ok(true) => tracing::info!("MQTT: quit {appid} — terminated"),
                Ok(false) => tracing::warn!("MQTT: quit {appid} — not running"),
                Err(e) => tracing::warn!("MQTT: quit {appid} failed: {e}"),
            }
        }
        "open-bpm" => {
            tracing::info!("MQTT: command accepted on {topic}");
            let result = tokio::task::spawn_blocking(crate::launch::open_bigpicture)
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("open-bpm task panicked: {e}")));
            match result {
                Ok(()) => tracing::info!("MQTT: open-bpm dispatched"),
                Err(e) => tracing::warn!("MQTT: open-bpm failed: {e}"),
            }
        }
        other => tracing::warn!("MQTT: unknown command {other:?} on {topic} — ignored"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an env lookup from a table, so no test touches the real environment.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |suffix: &str| {
            owned
                .iter()
                .find(|(k, _)| k == suffix)
                .map(|(_, v)| v.clone())
        }
    }

    fn device(raw: &str) -> DeviceId {
        DeviceId::new(raw).expect("test device id must be valid")
    }

    #[test]
    fn broker_unset_disables_mqtt() {
        assert_eq!(settings_from(lookup(&[])), Ok(None));
        // An empty value reads as unset, not as a configured-but-blank broker.
        assert_eq!(settings_from(lookup(&[("MQTT_BROKER", "  ")])), Ok(None));
    }

    /// A broken MQTT environment disables MQTT and nothing else.
    ///
    /// The QML shell's Steam widget depends on this sidecar's HTTP routes, so an
    /// MQTT typo — most likely on Windows, where these arrive through per-user
    /// `win_environment` variables — must not take the HTTP listener down with
    /// it. `main` logs the error and carries on to `axum::serve`; the contract
    /// this test pins is that resolution reports the failure as a value rather
    /// than panicking or aborting.
    #[test]
    fn a_broken_mqtt_environment_only_disables_mqtt() {
        let broken: [&[(&str, &str)]; 4] = [
            // Broker set, identity missing.
            &[("MQTT_BROKER", "mqtts://broker:8883")],
            // Identity present but not topic-safe.
            &[("MQTT_BROKER", "mqtt://broker"), ("MQTT_DEVICE_ID", "a/b")],
            // Unparseable broker URL.
            &[
                ("MQTT_BROKER", "http://broker"),
                ("MQTT_DEVICE_ID", "desktop"),
            ],
            // Half-configured credentials.
            &[
                ("MQTT_BROKER", "mqtts://broker"),
                ("MQTT_DEVICE_ID", "desktop"),
                ("MQTT_USERNAME", "tv-shell-desktop"),
            ],
        ];
        for env in broken {
            let got = settings_from(lookup(env));
            assert!(got.is_err(), "expected a config error for {env:?}");
        }
        // And the healthy case still resolves, so the test cannot pass vacuously.
        assert!(settings_from(lookup(&[
            ("MQTT_BROKER", "mqtts://broker"),
            ("MQTT_DEVICE_ID", "desktop"),
        ]))
        .expect("valid config resolves")
        .is_some());
    }

    #[test]
    fn broker_without_device_id_fails_closed() {
        let err = settings_from(lookup(&[("MQTT_BROKER", "mqtts://broker:8883")]))
            .expect_err("a broker with no device id must be a startup error");
        assert!(err.contains("TV_SHELL_MQTT_DEVICE_ID"), "{err}");
        assert!(err.contains("both boots"), "{err}");
        assert!(err.contains("Home Assistant devices"), "{err}");
    }

    #[test]
    fn invalid_device_id_fails() {
        let err = settings_from(lookup(&[
            ("MQTT_BROKER", "mqtt://broker"),
            ("MQTT_DEVICE_ID", "desk/top"),
        ]))
        .expect_err("a wildcard-adjacent device id must be rejected");
        // Carries the protocol crate's DeviceIdError verbatim.
        assert!(err.contains("TV_SHELL_MQTT_DEVICE_ID is invalid"), "{err}");
        assert!(err.contains("'/'"), "{err}");
    }

    #[test]
    fn credentials_must_be_both_or_neither() {
        let both_unset = settings_from(lookup(&[
            ("MQTT_BROKER", "mqtt://broker"),
            ("MQTT_DEVICE_ID", "desktop"),
        ]))
        .expect("neither set is fine")
        .expect("settings");
        assert_eq!(both_unset.username, None);
        assert_eq!(both_unset.password, None);

        let user_only = settings_from(lookup(&[
            ("MQTT_BROKER", "mqtt://broker"),
            ("MQTT_DEVICE_ID", "desktop"),
            ("MQTT_USERNAME", "tv-shell-desktop"),
        ]))
        .expect_err("username without password must fail");
        assert!(
            user_only.contains("TV_SHELL_MQTT_PASSWORD is not"),
            "{user_only}"
        );

        let pass_only = settings_from(lookup(&[
            ("MQTT_BROKER", "mqtt://broker"),
            ("MQTT_DEVICE_ID", "desktop"),
            ("MQTT_PASSWORD", "hunter2"),
        ]))
        .expect_err("password without username must fail");
        assert!(
            pass_only.contains("TV_SHELL_MQTT_USERNAME is not"),
            "{pass_only}"
        );
    }

    #[test]
    fn full_valid_set_parses() {
        let settings = settings_from(lookup(&[
            ("MQTT_BROKER", "mqtts://mqtt.example:1884"),
            ("MQTT_DEVICE_ID", "desktop"),
            ("MQTT_USERNAME", "tv-shell-desktop"),
            ("MQTT_PASSWORD", "hunter2"),
            ("MQTT_CA_FILE", "/etc/ssl/homelab-ca.pem"),
            ("MQTT_HEARTBEAT_SECS", "15"),
            ("MQTT_KEEPALIVE_SECS", "90"),
        ]))
        .expect("valid settings")
        .expect("settings");
        assert_eq!(
            settings,
            MqttSettings {
                device_id: device("desktop"),
                host: "mqtt.example".to_string(),
                port: 1884,
                tls: true,
                username: Some("tv-shell-desktop".to_string()),
                password: Some("hunter2".to_string()),
                ca_file: Some(PathBuf::from("/etc/ssl/homelab-ca.pem")),
                heartbeat: Duration::from_secs(15),
                keepalive: Duration::from_secs(90),
            }
        );
    }

    #[test]
    fn debug_redacts_the_password() {
        let settings = settings_from(lookup(&[
            ("MQTT_BROKER", "mqtts://broker"),
            ("MQTT_DEVICE_ID", "desktop"),
            ("MQTT_USERNAME", "tv-shell-desktop"),
            ("MQTT_PASSWORD", "hunter2"),
        ]))
        .expect("valid settings")
        .expect("settings");
        let rendered = format!("{settings:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        // The username is not a secret and stays legible for diagnosis.
        assert!(rendered.contains("tv-shell-desktop"), "{rendered}");
    }

    #[test]
    fn broker_urls_parse_scheme_host_and_port() {
        assert_eq!(
            parse_broker_url("mqtt://h"),
            Ok(("h".to_string(), 1883, false))
        );
        assert_eq!(
            parse_broker_url("mqtts://h"),
            Ok(("h".to_string(), 8883, true))
        );
        assert_eq!(
            parse_broker_url("mqtts://h:1234"),
            Ok(("h".to_string(), 1234, true))
        );
        // Bracketed IPv6 literal: brackets stripped, colons in the address kept.
        assert_eq!(
            parse_broker_url("mqtts://[fd00::1]:8883"),
            Ok(("fd00::1".to_string(), 8883, true))
        );
        assert_eq!(
            parse_broker_url("mqtt://[fd00::1]"),
            Ok(("fd00::1".to_string(), 1883, false))
        );
    }

    #[test]
    fn broker_urls_reject_everything_else() {
        for raw in [
            "http://h",     // wrong scheme
            "h:1883",       // no scheme
            "mqtts://",     // empty host
            "mqtts://:123", // empty host with a port
            "mqtts://h:0",  // port 0
            "mqtts://h:abc",
            "mqtts://h:99999", // out of u16 range
            "mqtts://h/topic", // path
            "mqtts://[fd00::1]junk",
            "mqtts://[fd00::1",   // unterminated IPv6 literal
            "mqtts://[]:8883",    // empty bracketed host
            "mqtts://[fd00::1]:", // empty port
        ] {
            assert!(
                parse_broker_url(raw).is_err(),
                "{raw:?} should have been rejected"
            );
        }
    }

    #[test]
    fn heartbeat_and_keepalive_default_parse_and_reject() {
        let base: &[(&str, &str)] = &[
            ("MQTT_BROKER", "mqtt://broker"),
            ("MQTT_DEVICE_ID", "desktop"),
        ];
        let with = |extra: &[(&str, &str)]| {
            let mut pairs = base.to_vec();
            pairs.extend_from_slice(extra);
            settings_from(lookup(&pairs))
        };

        let defaults = with(&[]).expect("defaults").expect("settings");
        assert_eq!(defaults.heartbeat, Duration::from_secs(30));
        assert_eq!(defaults.keepalive, Duration::from_secs(60));

        let parsed = with(&[("MQTT_HEARTBEAT_SECS", "7"), ("MQTT_KEEPALIVE_SECS", "120")])
            .expect("parsed")
            .expect("settings");
        assert_eq!(parsed.heartbeat, Duration::from_secs(7));
        assert_eq!(parsed.keepalive, Duration::from_secs(120));

        // 0 and garbage are errors, NOT a silent fallback to the default: a zero
        // heartbeat would busy-loop the publisher.
        for (var, value) in [
            ("MQTT_HEARTBEAT_SECS", "0"),
            ("MQTT_KEEPALIVE_SECS", "0"),
            ("MQTT_HEARTBEAT_SECS", "soon"),
            ("MQTT_KEEPALIVE_SECS", "-1"),
            ("MQTT_HEARTBEAT_SECS", "1.5"),
        ] {
            assert!(
                with(&[(var, value)]).is_err(),
                "{var}={value:?} should have been rejected"
            );
        }
    }

    #[test]
    fn command_names_come_only_from_our_command_topics() {
        let id = device("desktop");
        assert_eq!(
            command_name("tv-shell/desktop/cmd/sleep", &id),
            Some("sleep")
        );
        assert_eq!(
            command_name("tv-shell/desktop/cmd/open-bpm", &id),
            Some("open-bpm")
        );
        // An unknown name is still a command name — dispatch logs and drops it.
        assert_eq!(command_name("tv-shell/desktop/cmd/nope", &id), Some("nope"));

        for topic in [
            "tv-shell/desktop/cmd/",         // empty name
            "tv-shell/desktop/cmd/a/b",      // extra level
            "tv-shell/desktop/state",        // wrong suffix
            "tv-shell/htpc-1/cmd/sleep",     // another device
            "homeassistant/device/x/config", // wrong prefix entirely
            "cmd/sleep",
        ] {
            assert_eq!(command_name(topic, &id), None, "{topic}");
        }
    }

    #[test]
    fn should_publish_truth_table() {
        let heartbeat = Duration::from_secs(30);
        // changed ⇒ always publish, however recent the last one was.
        assert!(should_publish(true, Duration::ZERO, heartbeat));
        assert!(should_publish(true, Duration::from_secs(29), heartbeat));
        // unchanged ⇒ only once the heartbeat has elapsed.
        assert!(!should_publish(false, Duration::ZERO, heartbeat));
        assert!(!should_publish(false, Duration::from_secs(29), heartbeat));
        assert!(should_publish(false, heartbeat, heartbeat));
        assert!(should_publish(false, Duration::from_secs(31), heartbeat));
    }

    #[test]
    fn seq_starts_at_zero_and_only_advances() {
        let mut state = StateBuilder::default();
        let status = StatusResponse {
            version: "1.2.3".to_string(),
            running_appid: None,
            streaming: false,
        };
        let seqs: Vec<u64> = (0..5)
            .map(|i| state.next(1_785_109_000 + i, status.clone()).seq)
            .collect();
        assert_eq!(seqs, [0, 1, 2, 3, 4]);
        // published_at is caller-supplied and rides along untouched.
        assert_eq!(
            state.next(1_785_109_009, status).published_at,
            1_785_109_009
        );
    }

    #[test]
    fn discovery_payload_has_no_os_branching() {
        // §4a at the CALL SITE: the desktop is ONE dual-boot machine publishing a
        // RETAINED discovery message, so both boots must produce identical bytes.
        // Nothing OS-dependent may reach this payload — `current_os` belongs to
        // the state payload only.
        let payload = discovery_payload(&device("desktop")).expect("discovery serializes");
        let text = String::from_utf8(payload).expect("discovery is utf-8");
        assert!(!text.contains("linux"), "{text}");
        assert!(!text.contains("windows"), "{text}");
        assert!(!text.contains("macos"), "{text}");
        // ...and it is stable across calls within a process, too.
        let again = discovery_payload(&device("desktop")).expect("discovery serializes");
        assert_eq!(text.as_bytes(), again.as_slice());
    }

    #[tokio::test]
    async fn load_ca_reads_nothing_when_unconfigured() {
        assert_eq!(load_ca(None).await.expect("no ca file"), None);
    }

    #[tokio::test]
    async fn load_ca_fails_closed_on_a_missing_file() {
        let missing = std::env::temp_dir().join("tv-shell-host-no-such-ca-bundle.pem");
        let err = load_ca(Some(&missing))
            .await
            .expect_err("a missing CA bundle must fail startup");
        assert!(err.to_string().contains("TV_SHELL_MQTT_CA_FILE"), "{err}");
    }
}

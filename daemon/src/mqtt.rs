//! MQTT state publisher + command surface.
//!
//! The daemon opens its **own** broker connection so it carries its **own** Last
//! Will. Availability is then a fact the broker asserts when our socket dies —
//! not something a consumer has to probe for. No other tv-shell process proxies
//! for this one.
//!
//! ## Frozen topic + identity contract
//!
//! Do not change any of these strings; consumers (Home Assistant discovery,
//! retained state) are bound to them. The builders live in
//! [`tv_shell_protocol::mqtt`] so the daemon and the desktop sidecar cannot drift.
//!
//! ```text
//! tv-shell/<device_id>/state                        retained     device -> broker
//! tv-shell/<device_id>/avail                        retained     LWT: "online" / "offline"
//! tv-shell/<device_id>/cmd/<name>                   NOT retained broker -> device
//! homeassistant/device/tv-shell-<device_id>/config  retained     discovery
//!
//! HA device identifier : tv-shell-<device_id>
//! entity unique_id     : tv-shell-<device_id>-<entity_key>
//! ```
//!
//! This daemon's `device_id` is `htpc-1`. It is **explicitly configured, never
//! derived** — see [`crate::daemon_config::DaemonConfig::mqtt_device_id`].
//!
//! ## Why `published_at` / `seq` / the floor heartbeat exist
//!
//! A client can keep "publishing" into a **half-open socket** long after the
//! broker gave up on it and fired its Last Will. That happened on this broker for
//! 13.5 hours: every consumer read `unavailable` while the publisher's own logs
//! looked perfectly healthy. **Availability did not catch it** — availability
//! cannot express *"connected, but nothing is arriving"*.
//!
//! A `published_at` that stops advancing and a `seq` that stops incrementing can,
//! but only if the publisher is guaranteed to keep publishing when nothing has
//! changed. That guarantee is the **floor heartbeat** ([`MqttSettings::heartbeat`],
//! default 30 s): the publish loop is emit-on-change *plus* a floor, so a frozen
//! `published_at` is unambiguous evidence of a wedge rather than a quiet house.
//!
//! [`crate::service_health::run`] is the emit-on-change precedent this follows,
//! but it has **no** floor heartbeat — the floor here is a genuine addition, and
//! the whole point of the design. Do not "simplify" it away.
//!
//! ## There is no config-reload path
//!
//! `DaemonConfig::load()` runs once into a `OnceLock` and `watch.rs` watches
//! `settings.json` only. Any `[mqtt]` change — **including a credential
//! rotation** — requires a **daemon restart**, and a daemon restart hands the CEC
//! adapter off to whatever grabs it next (see the CEC notes in `CLAUDE.md`).
//! Rotating the MQTT password is therefore an outage-adjacent operation, not a
//! config edit.
//!
//! ## Shape
//!
//! Three cooperating tasks, all fire-and-forget and all cancelled by the shared
//! [`CancellationToken`]:
//!
//! 1. **the event loop** ([`run`]'s body) — drives `EventLoop::poll()` and does
//!    nothing else that can block, so protocol keepalives are never starved.
//! 2. **the publisher** — owns every outbound publish/subscribe. Keeping the
//!    sends off the polling task is what makes a full request channel impossible
//!    to deadlock on: the event loop is always draining it.
//! 3. **the command executor** — runs one incoming command at a time off a small
//!    bounded channel, so a slow command (`restart-shell` shells out to
//!    `systemctl`) can neither stall the protocol loop nor spawn unboundedly.
//!
//! Cross-platform on purpose — this module is **not** `cfg`-gated in `lib.rs`.
//! Gating it would drop it from macOS `cargo test` coverage, and everything it
//! calls ([`crate::ipc::dispatch_dbus`], [`crate::bridge_core`],
//! [`crate::shell_state`], [`crate::display_owner`]) is already cross-platform.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rumqttc::{
    AsyncClient, Event, LastWill, MqttOptions, Packet, QoS, TlsConfiguration, Transport,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tv_shell_protocol::mqtt::{
    shell_discovery, DeviceId, DeviceOs, ShellSnapshot, StateEnvelope, AVAIL_OFFLINE, AVAIL_ONLINE,
};

use crate::daemon_config::MqttEndpoint;
use crate::display_owner::{DisplayOwnerStatus, Ownership, SharedDisplayOwner};
use crate::shell_state::{SharedShellState, ShellStatus};
use crate::state::Control;

/// Outbound request-channel depth handed to [`AsyncClient::new`].
const REQUEST_CAP: usize = 32;

/// Publish-loop tick. The *cadence* of change detection, not of publishing —
/// [`should_publish`] decides whether a tick actually emits anything.
const TICK: Duration = Duration::from_secs(1);

/// Depth of the incoming-command queue. Commands are human button presses, so a
/// deeper queue would only mean executing a stale backlog; a full queue drops
/// with a warning instead of growing without bound.
const COMMAND_QUEUE: usize = 8;

/// First reconnect backoff after a poll error.
const BACKOFF_MIN: Duration = Duration::from_secs(1);

/// Reconnect backoff ceiling. A broker outage must not become a reconnect storm
/// on a bus that Zigbee and Z-Wave also ride on.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Everything the actor needs, resolved and validated at startup by
/// [`crate::daemon_config::DaemonConfig::validate`].
pub struct MqttSettings {
    /// This device's validated identity — the one input every topic is built from.
    pub device_id: DeviceId,
    /// Parsed `[mqtt].broker`.
    pub endpoint: MqttEndpoint,
    /// PEM bytes of `[mqtt].ca_file`, when configured. `None` ⇒ platform roots.
    pub ca_pem: Option<Vec<u8>>,
    /// MQTT username; paired with `password` by config validation.
    pub username: Option<String>,
    /// MQTT password, read from the 0600 `[mqtt].password_file`.
    pub password: Option<String>,
    /// Floor republish interval — see the module docs.
    pub heartbeat: Duration,
    /// MQTT keepalive.
    pub keepalive: Duration,
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure helpers (unit-tested; no broker, no network, no sockets)
// ─────────────────────────────────────────────────────────────────────────────

/// Whether this tick should publish: something changed, **or** the floor
/// heartbeat has elapsed since the last publish.
///
/// Split out as a pure function precisely so the "unchanged but overdue" case —
/// the one that makes a half-open wedge detectable — is directly testable.
fn should_publish(changed: bool, since_last: Duration, heartbeat: Duration) -> bool {
    changed || since_last >= heartbeat
}

/// Extract the command name from an incoming topic, or `None` when the topic is
/// not one of ours.
///
/// Borrowed from `topic` so the happy path allocates nothing. A name containing
/// `/` cannot arrive through the `cmd/+` single-level filter, but is rejected
/// anyway rather than trusted.
fn command_name<'a>(device_id: &DeviceId, topic: &'a str) -> Option<&'a str> {
    let name = topic.strip_prefix(device_id.cmd_topic("").as_str())?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some(name)
}

/// A command accepted off `tv-shell/<id>/cmd/+`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MqttCommand {
    /// Suspend this machine, through the same two-step gate as `POST /suspend`.
    Suspend,
    /// Dispatch a shell intent (`home` / `menu` / `settings` / …).
    Intent(String),
    /// Restart the QML shell.
    RestartShell,
}

/// Classify a command name. `None` for anything unrecognised — a command topic
/// is a remote-control surface, so unknown input is warned about and dropped,
/// never guessed at.
///
/// Intents are gated on [`crate::bridge_core::is_valid_intent`] (the same closed
/// vocabulary the IPC surface enforces) rather than a hard-coded list, so the
/// two can never disagree about what a valid intent is.
fn parse_command(name: &str) -> Option<MqttCommand> {
    match name {
        "suspend" => Some(MqttCommand::Suspend),
        "restart-shell" => Some(MqttCommand::RestartShell),
        other if crate::bridge_core::is_valid_intent(other) => {
            Some(MqttCommand::Intent(other.to_string()))
        }
        _ => None,
    }
}

/// The subset of the published state that constitutes a **change**.
///
/// Deliberately NOT the whole status: three families of field advance on their
/// own every single tick and would make every tick a publish —
///
/// * `age_seconds` / `cec_display_owner_held_seconds` — derived from the clock;
/// * `stale_after_seconds` — a constant, so it can never be a change;
/// * **system metrics** (CPU%, memory, uptime) — CPU% moves constantly.
///
/// Including any of them would publish once a second onto a broker that home
/// automation depends on. Metrics ride along on whatever publish happens for a
/// real reason; they never *cause* one. `published_at` still advances every
/// heartbeat, so freshness is not lost by excluding them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangeKey {
    shell_state: Option<String>,
    media_playing: bool,
    stale: bool,
    shell_running: bool,
    cec_display_ownership: Ownership,
    cec_display_owner: Option<i32>,
    cec_local_address: Option<i32>,
    cec_display_owner_changed_unix: Option<u64>,
    cec_display_owner_ever_observed: bool,
    cec_display_owner_tracking: bool,
}

fn change_key(shell: &ShellStatus, display: &DisplayOwnerStatus) -> ChangeKey {
    ChangeKey {
        shell_state: shell.shell_state.clone(),
        media_playing: shell.media_playing,
        stale: shell.stale,
        shell_running: shell.shell_running,
        cec_display_ownership: display.cec_display_ownership,
        cec_display_owner: display.cec_display_owner,
        cec_local_address: display.cec_local_address,
        cec_display_owner_changed_unix: display.cec_display_owner_changed_unix,
        cec_display_owner_ever_observed: display.cec_display_owner_ever_observed,
        cec_display_owner_tracking: display.cec_display_owner_tracking,
    }
}

/// The daemon's [`Ownership`] enum as the string the wire carries.
///
/// Serialized through serde rather than hand-written, so the payload can never
/// drift from the enum's `#[serde(rename_all = "snake_case")]` values. Total: a
/// serialization failure degrades to `"unknown"` (the protocol's own default)
/// instead of panicking a daemon task.
fn ownership_string(ownership: Ownership) -> String {
    serde_json::to_value(ownership)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Assemble the published snapshot from the daemon's own status types.
///
/// The six [`ShellStatus`] fields and the seven [`DisplayOwnerStatus`] fields
/// carry across one-for-one; only the system half is new. `metrics`/`uptime` are
/// `None` when sampling failed, which the protocol type models as `null` rather
/// than a misleading zero.
fn build_snapshot(
    shell: &ShellStatus,
    display: &DisplayOwnerStatus,
    metrics: Option<&crate::system::SysMetrics>,
    uptime_seconds: Option<u64>,
) -> ShellSnapshot {
    ShellSnapshot {
        shell_state: shell.shell_state.clone(),
        media_playing: shell.media_playing,
        stale: shell.stale,
        age_seconds: shell.age_seconds,
        stale_after_seconds: shell.stale_after_seconds,
        shell_running: shell.shell_running,

        cec_display_ownership: ownership_string(display.cec_display_ownership),
        cec_display_owner: display.cec_display_owner,
        cec_local_address: display.cec_local_address,
        cec_display_owner_changed_unix: display.cec_display_owner_changed_unix,
        cec_display_owner_held_seconds: display.cec_display_owner_held_seconds,
        cec_display_owner_ever_observed: display.cec_display_owner_ever_observed,
        cec_display_owner_tracking: display.cec_display_owner_tracking,

        version: env!("CARGO_PKG_VERSION").to_string(),
        cpu_percent: metrics.map(|m| m.cpu_pct),
        mem_percent: metrics.map(|m| f64::from(m.mem_pct)),
        uptime_seconds,
    }
}

/// Next backoff in the capped exponential ladder.
fn next_backoff(current: Duration) -> Duration {
    std::cmp::min(current.saturating_mul(2), BACKOFF_MAX)
}

// ─────────────────────────────────────────────────────────────────────────────
// Connection setup
// ─────────────────────────────────────────────────────────────────────────────

/// Install `ring` as the process-wide rustls crypto provider, once.
///
/// `rumqttc` is built with `use-rustls-no-provider` (see `Cargo.toml`) precisely
/// so it does **not** drag in `aws-lc-rs` (C + cmake), which means nothing
/// installs a provider for us. An `Err` here means someone already installed one
/// — that is fine and must be ignored, never unwrapped.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Build the TLS configuration for this endpoint.
///
/// With a configured CA we hand rumqttc the PEM bytes (`Simple`), which it parses
/// at connect time and reports as a connection error — no panic path. Without
/// one we fall back to platform roots.
///
/// `TlsConfiguration::default()` is wrapped in `catch_unwind` on purpose:
/// rumqttc's `Default` impl `.expect()`s on loading the platform trust store, and
/// a daemon must not die because a host has no system CA bundle. A failure
/// degrades to "MQTT off" with a loud log, not a dead task.
fn tls_configuration(ca_pem: Option<Vec<u8>>) -> Option<TlsConfiguration> {
    match ca_pem {
        Some(ca) => Some(TlsConfiguration::Simple {
            ca,
            alpn: None,
            client_auth: None,
        }),
        None => std::panic::catch_unwind(TlsConfiguration::default)
            .map_err(|_| {
                tracing::error!(
                    "mqtt: could not load the platform CA trust store — MQTT is disabled. \
                     Set [mqtt].ca_file to the broker's CA certificate."
                );
            })
            .ok(),
    }
}

/// Build the connect options: client id, keepalive, credentials, Last Will, TLS.
///
/// Returns `None` only when TLS was requested and no trust anchor could be
/// established (see [`tls_configuration`]).
fn build_options(settings: &MqttSettings) -> Option<MqttOptions> {
    let client_id = format!("tv-shell-{}", settings.device_id);
    let mut opts = MqttOptions::new(
        client_id,
        settings.endpoint.host.clone(),
        settings.endpoint.port,
    );
    opts.set_keep_alive(settings.keepalive);

    if let (Some(user), Some(pass)) = (&settings.username, &settings.password) {
        opts.set_credentials(user.clone(), pass.clone());
    }

    // The Last Will MUST be registered before connecting — a will set after the
    // fact does not exist as far as the broker is concerned. Retained, so a
    // consumer that subscribes after we died still sees "offline".
    opts.set_last_will(LastWill::new(
        settings.device_id.avail_topic(),
        AVAIL_OFFLINE,
        QoS::AtLeastOnce,
        true,
    ));

    if settings.endpoint.tls {
        opts.set_transport(Transport::tls_with_config(tls_configuration(
            settings.ca_pem.clone(),
        )?));
    }

    Some(opts)
}

// ─────────────────────────────────────────────────────────────────────────────
// Actor
// ─────────────────────────────────────────────────────────────────────────────

/// Run the MQTT actor until `shutdown` is cancelled.
///
/// Fire-and-forget, like every other actor in `main.rs`: it logs and degrades and
/// must **never** panic or abort the daemon. A broker that is unreachable is a
/// warning and a retry, not a startup failure.
pub async fn run(
    settings: MqttSettings,
    ui_state: SharedShellState,
    display_owner: SharedDisplayOwner,
    control_tx: mpsc::Sender<Control>,
    dbus: crate::ipc::DbusSenders,
    metrics: Arc<crate::metrics::Metrics>,
    shutdown: CancellationToken,
) {
    install_crypto_provider();

    let Some(opts) = build_options(&settings) else {
        return;
    };
    let device_id = settings.device_id.clone();
    tracing::info!(
        "mqtt: connecting to {}:{} (tls={}) as tv-shell-{device_id}",
        settings.endpoint.host,
        settings.endpoint.port,
        settings.endpoint.tls
    );

    let (client, mut eventloop) = AsyncClient::new(opts, REQUEST_CAP);

    // Set by the event loop on every ConnAck; consumed by the publisher, which
    // then republishes EVERYTHING (a reconnect may mean the broker expired our
    // session, so retained discovery/availability and the subscription can all
    // be gone). An AtomicBool rather than a notification because a missed edge
    // must be impossible: the publisher swaps it on its own tick.
    let connected = Arc::new(AtomicBool::new(false));

    let (cmd_tx, cmd_rx) = mpsc::channel::<MqttCommand>(COMMAND_QUEUE);

    let publisher = tokio::spawn(publish_loop(
        client.clone(),
        device_id.clone(),
        settings.heartbeat,
        ui_state,
        display_owner,
        Arc::clone(&connected),
        shutdown.clone(),
    ));
    let executor = tokio::spawn(command_loop(
        cmd_rx,
        control_tx,
        dbus,
        metrics,
        shutdown.clone(),
    ));

    let mut backoff = BACKOFF_MIN;
    loop {
        let event = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            event = eventloop.poll() => event,
        };

        match event {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                tracing::info!("mqtt: connected to broker as tv-shell-{device_id}");
                backoff = BACKOFF_MIN;
                connected.store(true, Ordering::Release);
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let topic = publish.topic.clone();
                match command_name(&device_id, &topic).and_then(parse_command) {
                    // The payload is deliberately ignored — a Home Assistant
                    // button sends an arbitrary press payload.
                    Some(command) => {
                        tracing::info!("mqtt: command {command:?} from {topic}");
                        if cmd_tx.try_send(command).is_err() {
                            tracing::warn!("mqtt: command queue full or closed, dropping {topic}");
                        }
                    }
                    None => tracing::warn!("mqtt: ignoring unrecognised command topic {topic}"),
                }
            }
            Ok(_) => {}
            Err(e) => {
                // rumqttc reconnects on its own, but with no delay of its own —
                // an explicit capped backoff is what keeps a broker outage from
                // becoming a reconnect storm.
                connected.store(false, Ordering::Release);
                tracing::warn!("mqtt: connection error ({e}); retrying in {backoff:?}");
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = next_backoff(backoff);
            }
        }
    }

    tracing::info!("mqtt: shutting down");
    // Both children also watch `shutdown`, so this is belt-and-braces for the
    // case where one is parked mid-await on a channel that will never resolve.
    publisher.abort();
    executor.abort();
}

/// Owns every outbound publish and the subscription.
///
/// Ticks every second, but [`should_publish`] gates whether a tick emits: state
/// goes out on change, plus the floor heartbeat. See the module docs for why the
/// floor is load-bearing.
async fn publish_loop(
    client: AsyncClient,
    device_id: DeviceId,
    heartbeat: Duration,
    ui_state: SharedShellState,
    display_owner: SharedDisplayOwner,
    connected: Arc<AtomicBool>,
    shutdown: CancellationToken,
) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut last_key: Option<ChangeKey> = None;
    let mut last_publish: Option<Instant> = None;
    // Nothing is published before the first ConnAck. Otherwise the very first
    // tick would enqueue a state message that the broker receives BEFORE the
    // discovery document that gives it meaning.
    let mut announced = false;
    // Monotonic per process, starting at 0. NEVER reset except at process start —
    // a `seq` that goes backwards destroys the wedge signal it exists to provide.
    let mut seq: u64 = 0;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {}
        }

        // A (re)connect means the broker may have expired our session: republish
        // discovery + availability, re-subscribe, and force the next state
        // publish by clearing the change cache.
        if connected.swap(false, Ordering::AcqRel) {
            if let Err(e) = announce(&client, &device_id).await {
                // The next ConnAck retries; a failure here means the connection
                // is already gone, which the event loop is about to observe.
                tracing::warn!("mqtt: (re)announce failed: {e}");
            }
            announced = true;
            last_key = None;
            last_publish = None;
        }
        if !announced {
            continue;
        }

        // The cheap half: cross-platform reads only, no ~200 ms metrics sample.
        let shell_running = crate::bridge_core::quickshell_running().await;
        let now = crate::shell_state::now_unix();
        let cached = ui_state.read().await.clone();
        // Lock-free atomics — no await, nothing held.
        let owner_snapshot = display_owner.snapshot();
        let shell = crate::shell_state::status(&cached, now, shell_running);
        let display = crate::display_owner::status(&owner_snapshot, now);

        let key = change_key(&shell, &display);
        let changed = last_key.as_ref() != Some(&key);
        let since_last = last_publish.map_or(Duration::MAX, |t| t.elapsed());
        if !should_publish(changed, since_last, heartbeat) {
            continue;
        }

        // Only now — on a tick that is actually publishing — pay for the system
        // sample. `sys_metrics()` sleeps ~200 ms taking a CPU delta and would
        // stall the runtime; `/proc/uptime` is sync I/O. Both go to the blocking
        // pool together, mirroring `metrics::run_textfile_writer`. A JoinError
        // publishes `None` metrics rather than skipping the publish: a heartbeat
        // that stops because metrics failed would look exactly like a wedge.
        let sampled = tokio::task::spawn_blocking(|| {
            (
                crate::system::sys_metrics(),
                crate::system::uptime_seconds(),
            )
        })
        .await;
        let (sys, uptime) = match sampled {
            Ok((sys, uptime)) => (Some(sys), uptime),
            Err(e) => {
                tracing::warn!("mqtt: system metrics sample failed ({e}); publishing without them");
                (None, None)
            }
        };

        let envelope = StateEnvelope::new(
            crate::shell_state::now_unix(),
            seq,
            DeviceOs::current(),
            build_snapshot(&shell, &display, sys.as_ref(), uptime),
        );
        seq = seq.wrapping_add(1);

        let payload = match serde_json::to_vec(&envelope) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("mqtt: could not serialise the state payload: {e}");
                continue;
            }
        };
        if let Err(e) = client
            .publish(device_id.state_topic(), QoS::AtLeastOnce, true, payload)
            .await
        {
            tracing::warn!("mqtt: state publish failed: {e}");
            continue;
        }
        last_key = Some(key);
        last_publish = Some(Instant::now());
    }
}

/// Publish the retained discovery document + the `online` birth message and
/// (re)subscribe to the command topics. Run on every ConnAck.
async fn announce(client: &AsyncClient, device_id: &DeviceId) -> anyhow::Result<()> {
    // No version argument on purpose: a software version in the RETAINED
    // discovery document would rewrite it on every release, and on the desktop
    // (whose two boots update independently) on every OS switch. The running
    // version is published as a `daemon_version` diagnostic ENTITY from the
    // state payload instead — see `ShellSnapshot::version`.
    let discovery = serde_json::to_vec(&shell_discovery(device_id))
        .map_err(|e| anyhow::anyhow!("serialising the discovery document: {e}"))?;
    client
        .publish(
            device_id.discovery_topic(),
            QoS::AtLeastOnce,
            true,
            discovery,
        )
        .await
        .map_err(|e| anyhow::anyhow!("publishing {}: {e}", device_id.discovery_topic()))?;
    client
        .publish(
            device_id.avail_topic(),
            QoS::AtLeastOnce,
            true,
            AVAIL_ONLINE,
        )
        .await
        .map_err(|e| anyhow::anyhow!("publishing {}: {e}", device_id.avail_topic()))?;
    client
        .subscribe(device_id.cmd_topic_filter(), QoS::AtLeastOnce)
        .await
        .map_err(|e| anyhow::anyhow!("subscribing to {}: {e}", device_id.cmd_topic_filter()))?;
    Ok(())
}

/// Execute incoming commands one at a time.
///
/// Serialized on purpose: `restart-shell` shells out to `systemctl` and suspend
/// can freeze the process mid-call, neither of which should overlap or run on the
/// protocol loop.
async fn command_loop(
    mut cmd_rx: mpsc::Receiver<MqttCommand>,
    control_tx: mpsc::Sender<Control>,
    dbus: crate::ipc::DbusSenders,
    metrics: Arc<crate::metrics::Metrics>,
    shutdown: CancellationToken,
) {
    loop {
        let command = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            command = cmd_rx.recv() => match command {
                Some(c) => c,
                None => return,
            },
        };

        match command {
            MqttCommand::Suspend => handle_suspend(&dbus).await,
            MqttCommand::Intent(name) => {
                match crate::bridge_core::dispatch_intent(&control_tx, name.clone()).await {
                    Some(reply) => tracing::info!("mqtt: intent {name} -> {reply}"),
                    None => tracing::warn!("mqtt: intent {name} dropped (control channel closed)"),
                }
            }
            MqttCommand::RestartShell => {
                match crate::bridge_core::dev_restart_shell(&metrics).await {
                    Ok(msg) => tracing::info!("mqtt: restart-shell ok: {}", msg.trim()),
                    Err(e) => tracing::warn!("mqtt: restart-shell failed: {}", e.trim()),
                }
            }
        }
    }
}

/// Suspend through the **same** two-step gate as `POST /suspend`: ask
/// `power-can-suspend` first and only suspend on a plain `yes`.
///
/// [`crate::http::interpret_suspend`] is reused rather than duplicated, so the
/// HTTP route's truth-table test remains the single source of truth for how the
/// power replies are read.
async fn handle_suspend(dbus: &crate::ipc::DbusSenders) {
    use crate::protocol::Command;

    let can = crate::ipc::dispatch_dbus(dbus, &Command::PowerCanSuspend)
        .await
        .unwrap_or_else(crate::protocol::resp_unsupported);

    let suspend = if can.trim() == "yes" {
        Some(
            crate::ipc::dispatch_dbus(dbus, &Command::PowerSuspend)
                .await
                .unwrap_or_else(crate::protocol::resp_unsupported),
        )
    } else {
        None
    };

    match crate::http::interpret_suspend(&can, suspend.as_deref()) {
        crate::http::SuspendOutcome::Accepted => tracing::info!("mqtt: suspend accepted"),
        crate::http::SuspendOutcome::Refused => {
            tracing::warn!("mqtt: suspend refused (power-can-suspend replied {can:?})")
        }
        crate::http::SuspendOutcome::Failed(reason) => {
            tracing::warn!("mqtt: suspend failed: {reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_owner::Snapshot;
    use crate::shell_state::ShellState;
    use crate::system::SysMetrics;

    fn id(raw: &str) -> DeviceId {
        DeviceId::new(raw).expect("test device id must be valid")
    }

    /// Compile-time only: `run` is spawned onto the multi-thread runtime from
    /// `main.rs`, which is `cfg(target_os = "linux")` — so nothing else would
    /// check the `Send` bound (or this exact signature) on a macOS `cargo test`.
    /// Never called; the parameters exist only to name the argument types.
    #[allow(dead_code)]
    fn assert_run_future_is_send(
        settings: MqttSettings,
        ui_state: SharedShellState,
        display_owner: SharedDisplayOwner,
        control_tx: mpsc::Sender<Control>,
        dbus: crate::ipc::DbusSenders,
        metrics: Arc<crate::metrics::Metrics>,
        shutdown: CancellationToken,
    ) {
        fn assert_send<F: std::future::Future + Send + 'static>(_: F) {}
        assert_send(run(
            settings,
            ui_state,
            display_owner,
            control_tx,
            dbus,
            metrics,
            shutdown,
        ));
    }

    fn shell_status() -> ShellStatus {
        let mut cache = ShellState::default();
        cache.record("streaming".to_string(), true, 1_000);
        crate::shell_state::status(&cache, 1_002, true)
    }

    fn display_status() -> DisplayOwnerStatus {
        crate::display_owner::status(
            &Snapshot {
                owner: 4,
                ours: 4,
                changed_unix: 900,
                ever_observed: true,
                tracking_active: true,
            },
            1_002,
        )
    }

    #[test]
    fn command_name_strips_only_our_prefix() {
        let device = id("htpc-1");
        assert_eq!(
            command_name(&device, "tv-shell/htpc-1/cmd/suspend"),
            Some("suspend")
        );
        assert_eq!(
            command_name(&device, "tv-shell/htpc-1/cmd/restart-shell"),
            Some("restart-shell")
        );
        // Another device's command topic must never be actioned by this one.
        assert_eq!(command_name(&device, "tv-shell/desktop/cmd/sleep"), None);
        // Our state/avail topics are not commands.
        assert_eq!(command_name(&device, "tv-shell/htpc-1/state"), None);
        assert_eq!(command_name(&device, "tv-shell/htpc-1/avail"), None);
        // Bare prefix, extra level, and an unrelated topic.
        assert_eq!(command_name(&device, "tv-shell/htpc-1/cmd/"), None);
        assert_eq!(command_name(&device, "tv-shell/htpc-1/cmd/a/b"), None);
        assert_eq!(command_name(&device, "homeassistant/status"), None);
    }

    #[test]
    fn parse_command_accepts_the_documented_set_and_rejects_the_rest() {
        assert_eq!(parse_command("suspend"), Some(MqttCommand::Suspend));
        assert_eq!(
            parse_command("restart-shell"),
            Some(MqttCommand::RestartShell)
        );
        for intent in ["home", "menu", "settings"] {
            assert_eq!(
                parse_command(intent),
                Some(MqttCommand::Intent(intent.to_string())),
                "{intent} is a valid intent"
            );
        }
        // Attacker-ish / typo'd input is dropped, never guessed at.
        for bad in ["", "SUSPEND", "reboot", "restart_shell", "home-", "../home"] {
            assert_eq!(parse_command(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn should_publish_on_change_or_heartbeat() {
        let heartbeat = Duration::from_secs(30);
        // Changed ⇒ publish immediately, however recently we published.
        assert!(should_publish(true, Duration::ZERO, heartbeat));
        assert!(should_publish(true, Duration::from_secs(1), heartbeat));
        // Unchanged and inside the floor ⇒ stay quiet (this is the flood guard).
        assert!(!should_publish(false, Duration::ZERO, heartbeat));
        assert!(!should_publish(false, Duration::from_secs(29), heartbeat));
        // Unchanged but the floor elapsed ⇒ publish, so `published_at` advances
        // and a half-open socket stays detectable.
        assert!(should_publish(false, Duration::from_secs(30), heartbeat));
        assert!(should_publish(false, Duration::from_secs(31), heartbeat));
        // The first tick has never published: Duration::MAX ⇒ publish.
        assert!(should_publish(false, Duration::MAX, heartbeat));
    }

    #[test]
    fn change_key_ignores_clock_derived_fields() {
        // THE flood guard: the same cached push read one second later differs in
        // `age_seconds` and `cec_display_owner_held_seconds`, but that is not a
        // change — otherwise every single tick would publish.
        let mut cache = ShellState::default();
        cache.record("idle".to_string(), false, 1_000);
        let owner = Snapshot {
            owner: 4,
            ours: 4,
            changed_unix: 900,
            ever_observed: true,
            tracking_active: true,
        };

        let a = change_key(
            &crate::shell_state::status(&cache, 1_001, true),
            &crate::display_owner::status(&owner, 1_001),
        );
        let b = change_key(
            &crate::shell_state::status(&cache, 1_002, true),
            &crate::display_owner::status(&owner, 1_002),
        );
        assert_eq!(a, b, "only the clock moved — that is not a change");

        // A real state push IS a change.
        cache.record("streaming".to_string(), true, 1_003);
        let c = change_key(
            &crate::shell_state::status(&cache, 1_003, true),
            &crate::display_owner::status(&owner, 1_003),
        );
        assert_ne!(b, c);

        // So is the shell disappearing, and so is CEC ownership moving.
        let d = change_key(
            &crate::shell_state::status(&cache, 1_003, false),
            &crate::display_owner::status(&owner, 1_003),
        );
        assert_ne!(c, d);
        let moved = Snapshot { owner: 5, ..owner };
        let e = change_key(
            &crate::shell_state::status(&cache, 1_003, false),
            &crate::display_owner::status(&moved, 1_003),
        );
        assert_ne!(d, e);
    }

    #[test]
    fn envelope_seq_increments_and_published_at_carries_through() {
        // Synthesize the publish path's envelope construction over a few rounds.
        let snapshot = build_snapshot(&shell_status(), &display_status(), None, None);
        let mut seq = 0u64;
        let mut envelopes = Vec::new();
        for published_at in [1_000u64, 1_030, 1_060] {
            envelopes.push(StateEnvelope::new(
                published_at,
                seq,
                DeviceOs::current(),
                snapshot.clone(),
            ));
            seq = seq.wrapping_add(1);
        }
        assert_eq!(
            envelopes.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "seq starts at 0 and increments monotonically"
        );
        assert_eq!(
            envelopes.iter().map(|e| e.published_at).collect::<Vec<_>>(),
            vec![1_000, 1_030, 1_060],
            "published_at is carried through verbatim and always advances"
        );
        assert!(envelopes.iter().all(|e| e.schema_version == 1));
    }

    #[test]
    fn ownership_string_matches_the_enums_serialized_values() {
        // Anti-drift: the protocol carries this as a String, so the daemon owns
        // the test pinning its enum's serialized values.
        assert_eq!(ownership_string(Ownership::OwnedByUs), "owned_by_us");
        assert_eq!(ownership_string(Ownership::OwnedByOther), "owned_by_other");
        assert_eq!(ownership_string(Ownership::Unknown), "unknown");
    }

    #[test]
    fn snapshot_serialises_to_the_expected_json() {
        // The anti-drift test between the daemon's own status types and the
        // protocol payload: a known ShellStatus + DisplayOwnerStatus + SysMetrics
        // must produce exactly these bytes.
        let sys = SysMetrics {
            cpu_pct: 12.5,
            mem_used: 8_000_000_000,
            mem_total: 16_000_000_000,
            mem_pct: 50,
            load1: 0.42,
            temps: Vec::new(),
        };
        let snapshot = build_snapshot(&shell_status(), &display_status(), Some(&sys), Some(3_600));
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "shell_state": "streaming",
                "media_playing": true,
                "stale": false,
                "age_seconds": 2,
                "stale_after_seconds": crate::shell_state::STALE_AFTER_SECS,
                "shell_running": true,
                "cec_display_ownership": "owned_by_us",
                "cec_display_owner": 4,
                "cec_local_address": 4,
                "cec_display_owner_changed_unix": 900,
                "cec_display_owner_held_seconds": 102,
                "cec_display_owner_ever_observed": true,
                "cec_display_owner_tracking": true,
                "version": env!("CARGO_PKG_VERSION"),
                "cpu_percent": 12.5,
                "mem_percent": 50.0,
                "uptime_seconds": 3600,
            })
        );
    }

    #[test]
    fn snapshot_without_metrics_publishes_nulls_not_zeroes() {
        // A failed sample must be honest: `null`, not a 0% CPU / "just booted".
        let snapshot = build_snapshot(&shell_status(), &display_status(), None, None);
        assert_eq!(snapshot.cpu_percent, None);
        assert_eq!(snapshot.mem_percent, None);
        assert_eq!(snapshot.uptime_seconds, None);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = BACKOFF_MIN;
        let mut seen = vec![b];
        for _ in 0..10 {
            b = next_backoff(b);
            seen.push(b);
        }
        assert_eq!(
            &seen[..7],
            &[
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(32),
                BACKOFF_MAX,
            ]
        );
        // Capped, never unbounded.
        assert_eq!(*seen.last().unwrap(), BACKOFF_MAX);
    }

    #[test]
    fn last_will_is_registered_before_connecting_and_retained() {
        // A will set after connecting does not exist; retain=true so a late
        // subscriber still sees "offline".
        let settings = MqttSettings {
            device_id: id("htpc-1"),
            endpoint: MqttEndpoint {
                host: "broker.invalid".to_string(),
                port: 1883,
                tls: false,
            },
            ca_pem: None,
            username: Some("tv-shell-htpc-1".to_string()),
            password: Some("secret".to_string()),
            heartbeat: Duration::from_secs(30),
            keepalive: Duration::from_secs(60),
        };
        // No socket is opened here — MqttOptions is a plain value object.
        let opts = build_options(&settings).expect("plaintext options always build");
        assert_eq!(opts.client_id(), "tv-shell-htpc-1");
        assert_eq!(opts.keep_alive(), Duration::from_secs(60));
        let login = opts.credentials().expect("credentials must be set");
        assert_eq!(login.username, "tv-shell-htpc-1");
        assert_eq!(login.password, "secret");
        let will = opts.last_will().expect("a Last Will must be registered");
        assert_eq!(will.topic, "tv-shell/htpc-1/avail");
        assert_eq!(&will.message[..], AVAIL_OFFLINE.as_bytes());
        assert_eq!(will.qos, QoS::AtLeastOnce);
        assert!(will.retain);
    }

    #[test]
    fn options_omit_credentials_when_unset() {
        let settings = MqttSettings {
            device_id: id("htpc-1"),
            endpoint: MqttEndpoint {
                host: "broker.invalid".to_string(),
                port: 1883,
                tls: false,
            },
            ca_pem: None,
            username: None,
            password: None,
            heartbeat: Duration::from_secs(30),
            keepalive: Duration::from_secs(60),
        };
        assert_eq!(build_options(&settings).unwrap().credentials(), None);
    }

    #[test]
    fn configured_ca_builds_a_simple_tls_config() {
        // With a CA we hand rumqttc the PEM bytes; parsing failures then surface
        // as a connection error rather than a panic at construction time.
        let ca = b"-----BEGIN CERTIFICATE-----\n".to_vec();
        match tls_configuration(Some(ca.clone())) {
            Some(TlsConfiguration::Simple {
                ca: got,
                alpn,
                client_auth,
            }) => {
                assert_eq!(got, ca);
                assert!(alpn.is_none());
                assert!(client_auth.is_none());
            }
            other => panic!("expected TlsConfiguration::Simple, got {other:?}"),
        }
    }
}

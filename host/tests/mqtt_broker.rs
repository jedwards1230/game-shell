//! Broker-backed MQTT integration tests for `tv-shell-host`.
//!
//! # What this covers that the unit tests cannot
//!
//! Every *decision* in `host/src/mqtt.rs` was deliberately factored into a pure
//! function so it needs no broker (`settings_from`, `parse_broker_url`,
//! `should_publish`, the `StateBuilder`, the discovery serializer). This file
//! covers the part that design leaves uncovered: the **I/O glue**. Topic names,
//! retain flags, the discovery document's shape on the wire, the state envelope,
//! the floor heartbeat, and the Last Will are all contract — and three of the
//! four messages are RETAINED, so a regression there orphans a Home Assistant
//! device silently rather than throwing.
//!
//! # How it runs
//!
//! The host crate is **binary-only** (no `src/lib.rs`), so this file cannot
//! `use` anything from it. It drives the built binary through
//! `env!("CARGO_BIN_EXE_tv-shell-host")` and asserts everything from a
//! broker-side subscriber, plus `tv_shell_protocol` for topic *subscription*.
//!
//! Every test is `#[ignore]`d and keyed on `TV_SHELL_TEST_BROKER`:
//!
//! ```text
//! docker compose -f dev/mqtt/compose.yaml up -d --wait
//! TV_SHELL_TEST_BROKER=mqtt://127.0.0.1:1883 \
//!   cargo test -p tv-shell-host --test mqtt_broker -- \
//!     --ignored --test-threads=1 --nocapture
//! docker compose -f dev/mqtt/compose.yaml down -v
//! ```
//!
//! `--test-threads=1` because each test drives a real process and the broker's
//! retained namespace is shared; `--nocapture` because the host's own `RUST_LOG`
//! output and this harness's progress echo are the only way a CI failure is
//! debuggable.
//!
//! **Run with `--ignored` but no `TV_SHELL_TEST_BROKER` and these PANIC** rather
//! than quietly passing. A skipped check is indistinguishable from a passing
//! one, and deliberate opt-in is the only way these ever execute.
//!
//! There is deliberately **no `cfg(target_os)` gate**: `#[ignore]` already keeps
//! these out of `host.yml`'s three-OS matrix, and the file must stay compiled
//! and clippy-clean on Linux, macOS and Windows. So only cross-platform APIs are
//! used — notably `std::process::Child::kill()` (SIGKILL on Unix,
//! `TerminateProcess` on Windows), never `libc`.
//!
//! # The MQTT semantics that shape these tests
//!
//! In MQTT 3.1.1 a broker forwards a message to an **already-subscribed** client
//! with the RETAIN flag **cleared**; the flag is only set on the retained
//! *replay* a client receives when it subscribes. So a live subscriber sees
//! `retain == false` even for messages the publisher marked retained. Retention
//! is therefore proved in two phases: a live subscriber proves the messages are
//! published (and yields the payloads), and a second, **fresh** subscriber that
//! connects *after* the publishes proves the retain flags.
//!
//! # Two traps these tests are shaped to design out
//!
//! 1. **The wrong binary.** A build from a checkout without the MQTT code
//!    publishes nothing, which reads identically to "MQTT is broken".
//!    [`host_binary`] asserts the literal `TV_SHELL_MQTT_BROKER` is present in
//!    the binary's bytes before any assertion rests on silence.
//! 2. **A kill that killed nothing.** A `pkill -f` that matched no process exits
//!    0, and the harness then waits out a Last Will that had no reason to fire.
//!    [`HostProcess::kill_ungracefully`] proves the child is still running
//!    first, and proves it died non-successfully after.
//!
//! Generalised: *a check that silently does nothing looks exactly like a check
//! that passed.* Every assertion below proves the precondition it depends on.

use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rumqttc::{Client, Event, MqttOptions, Packet, QoS};
use serde_json::Value;
use tv_shell_protocol::mqtt::DeviceId;

// ─────────────────────────────────────────────────────────────────────────────
// Tunables
// ─────────────────────────────────────────────────────────────────────────────

/// Floor heartbeat handed to the host under test.
///
/// The *default* is 30 s, and that VALUE is already pinned by the pure
/// env-parsing unit tests in `host/src/mqtt.rs`. This harness covers the
/// MECHANISM, so it turns the knob down to keep CI fast. Note the publish loop
/// also has a fixed 5 s probe tick, which is the real floor: three heartbeat
/// messages take ~10 s, not ~4 s.
const HEARTBEAT_SECS: u64 = 2;

/// MQTT keepalive handed to the host under test. The broker waits roughly
/// 1.5 × keepalive before firing a Last Will, so this sets the LWT latency.
const KEEPALIVE_SECS: u64 = 5;

/// Budget for "the host connected and published something".
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// Budget for a retained replay to a freshly-subscribed client.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for the Last Will. Generous on purpose: with a 5 s keepalive the
/// broker fires at ~7.5 s, but a loaded CI runner is not a stopwatch, and a
/// too-short wait is a false failure.
const WILL_TIMEOUT: Duration = Duration::from_secs(45);

/// Budget for the reconnect + ConnAck republish (1 s initial backoff, then a
/// fresh TCP connect, CONNECT/CONNACK and a publish).
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Budget for collecting three heartbeat state messages (~10 s expected).
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

/// Budget for a subscriber's own CONNACK/SUBACK handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest payload echoed into a failure dump, per message.
const MAX_PAYLOAD_DUMP: usize = 2048;

// ─────────────────────────────────────────────────────────────────────────────
// The frozen contract, written out as literals
// ─────────────────────────────────────────────────────────────────────────────
//
// These are spelled out here rather than computed from `DeviceId`'s builders on
// purpose. Asserting a builder's output against the same builder's output is a
// tautology that keeps passing after the contract changes. The builders are
// still used to SUBSCRIBE (getting the topic wrong there just means the test
// sees nothing, which fails loudly) — but never to assert.

/// `homeassistant/device/tv-shell-<device_id>/config` — retained.
fn contract_discovery_topic(device_id: &str) -> String {
    format!("homeassistant/device/tv-shell-{device_id}/config")
}

/// `tv-shell/<device_id>/avail` — retained, and the connection's Last Will.
fn contract_avail_topic(device_id: &str) -> String {
    format!("tv-shell/{device_id}/avail")
}

/// `tv-shell/<device_id>/state` — retained.
fn contract_state_topic(device_id: &str) -> String {
    format!("tv-shell/{device_id}/state")
}

/// `tv-shell/<device_id>/cmd/<name>` — note the command segment is spelled with
/// a HYPHEN (`open-bpm`) while the `cmps` key is spelled with an UNDERSCORE
/// (`open_bpm`). That split is contract, not a typo.
fn contract_cmd_topic(device_id: &str, name: &str) -> String {
    format!("tv-shell/{device_id}/cmd/{name}")
}

/// `tv-shell-<device_id>-<entity_key>`.
fn contract_unique_id(device_id: &str, entity_key: &str) -> String {
    format!("tv-shell-{device_id}-{entity_key}")
}

/// The MQTT client id the host is contractually required to connect with.
fn contract_client_id(device_id: &str) -> String {
    format!("tv-shell-{device_id}")
}

// ─────────────────────────────────────────────────────────────────────────────
// Opt-in gate
// ─────────────────────────────────────────────────────────────────────────────

/// Where the broker lives, split into the pieces rumqttc wants.
struct Broker {
    /// The original `mqtt://host:port` URL, handed to the host verbatim.
    url: String,
    /// Hostname or address, no scheme.
    host: String,
    /// Broker port.
    port: u16,
}

/// Resolve the broker from `TV_SHELL_TEST_BROKER`, or **panic**.
///
/// Deliberately not a skip: `--ignored` is already the opt-in, and a test that
/// quietly returns when its dependency is missing is indistinguishable from a
/// test that passed.
fn broker() -> Broker {
    const VAR: &str = "TV_SHELL_TEST_BROKER";
    let raw = std::env::var(VAR).unwrap_or_default().trim().to_string();
    assert!(
        !raw.is_empty(),
        "{VAR} is unset or empty, so this test has NOTHING to talk to.\n\
         This is a PANIC and not a skip on purpose: a skipped check is \
         indistinguishable from a passing one.\n\
         Bring a broker up and opt in explicitly:\n  \
           docker compose -f dev/mqtt/compose.yaml up -d --wait\n  \
           {VAR}=mqtt://127.0.0.1:1883 cargo test -p tv-shell-host \
         --test mqtt_broker -- --ignored --test-threads=1 --nocapture"
    );

    let rest = raw.strip_prefix("mqtt://").unwrap_or_else(|| {
        panic!(
            "{VAR} must be a plain mqtt:// URL (got {raw:?}); TLS is out of scope for this harness"
        )
    });
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>()
                .unwrap_or_else(|_| panic!("{VAR} has an unparseable port in {raw:?}")),
        ),
        None => (rest.to_string(), 1883u16),
    };
    assert!(!host.is_empty(), "{VAR} has an empty host in {raw:?}");

    Broker {
        url: raw,
        host,
        port,
    }
}

/// A per-test device id, so a retained message from one test can never satisfy
/// another test's assertion (which would be a false pass, not a false failure).
///
/// Validated through `DeviceId` so a bad generator fails here — in the harness —
/// rather than at the host's first publish.
fn unique_device_id(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let raw = format!("it{tag}-{}-{}", std::process::id(), nanos % 1_000_000_000);
    DeviceId::new(&raw)
        .unwrap_or_else(|e| panic!("harness generated an invalid device id {raw:?}: {e}"));
    raw
}

// ─────────────────────────────────────────────────────────────────────────────
// Precondition: the binary under test actually has MQTT in it
// ─────────────────────────────────────────────────────────────────────────────

/// Path to the freshly-built host binary, **after** proving it carries the MQTT
/// code.
///
/// A build from a checkout without MQTT publishes nothing, and "published
/// nothing" reads exactly like "the broker is broken" or "MQTT regressed".
/// `CARGO_BIN_EXE_*` mostly rules that out, but silence has to be attributable
/// before any assertion rests on it — so search the binary's bytes for the
/// literal env-var name. Plain byte-window scan; no new crate.
fn host_binary() -> &'static Path {
    static CHECKED: OnceLock<&'static str> = OnceLock::new();
    let path = CHECKED.get_or_init(|| {
        let path = env!("CARGO_BIN_EXE_tv-shell-host");
        let bytes = std::fs::read(path)
            .unwrap_or_else(|e| panic!("could not read the host binary at {path}: {e}"));
        const NEEDLE: &[u8] = b"TV_SHELL_MQTT_BROKER";
        assert!(
            bytes.windows(NEEDLE.len()).any(|w| w == NEEDLE),
            "the built host binary at {path} ({} bytes) contains no `TV_SHELL_MQTT_BROKER` \
             string, so it has no MQTT support. Every assertion below would be resting on \
             silence from a binary that was never going to publish. Rebuild from a checkout \
             that has host/src/mqtt.rs: cargo build -p tv-shell-host",
            bytes.len()
        );
        println!(
            "precondition ok: {path} carries the MQTT env surface ({} bytes)",
            bytes.len()
        );
        path
    });
    Path::new(path)
}

// ─────────────────────────────────────────────────────────────────────────────
// The host process under test
// ─────────────────────────────────────────────────────────────────────────────

/// A running `tv-shell-host`, killed on every exit path including a panic.
///
/// A leaked host process would keep publishing under its own device id and,
/// worse, keep holding an HTTP port — poisoning later tests. The `Drop` impl is
/// the only thing that makes that structurally impossible.
struct HostProcess {
    child: Child,
    device_id: String,
    http_port: u16,
    /// Set once the child has been waited on, so `Drop` does not double-reap.
    reaped: Option<ExitStatus>,
}

impl HostProcess {
    /// Spawn the host against `broker`, publishing as `device_id`.
    ///
    /// `http_port` must be distinct per test: the host binds an axum listener and
    /// `axum::serve` blocks forever, so a bind collision **exits the process** —
    /// which would present as "MQTT never published".
    fn spawn(broker: &Broker, device_id: &str, http_port: u16) -> HostProcess {
        let bin = host_binary();
        println!(
            "  spawning {} (device_id={device_id}, http=127.0.0.1:{http_port}, \
             heartbeat={HEARTBEAT_SECS}s, keepalive={KEEPALIVE_SECS}s)",
            bin.display()
        );
        let child = Command::new(bin)
            .env("TV_SHELL_MQTT_BROKER", &broker.url)
            .env("TV_SHELL_MQTT_DEVICE_ID", device_id)
            .env("TV_SHELL_MQTT_HEARTBEAT_SECS", HEARTBEAT_SECS.to_string())
            .env("TV_SHELL_MQTT_KEEPALIVE_SECS", KEEPALIVE_SECS.to_string())
            .env("TV_SHELL_HOST_PORT", http_port.to_string())
            .env("TV_SHELL_HOST_BIND", "127.0.0.1")
            .env("TV_SHELL_HOST_TOKEN", "mqtt-integration-test")
            // Inherited, not piped: the host's own log is the other half of any
            // CI post-mortem, and a piped stream nobody reads would also block
            // the child once the pipe buffer filled.
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("could not spawn the host binary {}: {e}", bin.display()));

        HostProcess {
            child,
            device_id: device_id.to_string(),
            http_port,
            reaped: None,
        }
    }

    /// Assert the child is still running. Used before anything that depends on it
    /// being alive — an already-dead host explains a missing publish, and must be
    /// reported as that rather than as a mysterious timeout.
    fn assert_running(&mut self, when: &str) {
        match self.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => panic!(
                "the host process (device_id={}) had ALREADY EXITED {when}, with {status}. \
                 It published nothing after that point, so any wait below would have been \
                 waiting on a dead process. Read its inherited log above — a bind failure on \
                 127.0.0.1:{} exits the process and looks exactly like an MQTT fault.",
                self.device_id, self.http_port
            ),
            Err(e) => panic!(
                "could not determine whether the host process (device_id={}) is alive {when}: {e}",
                self.device_id
            ),
        }
    }

    /// SIGKILL-equivalent termination with **no** graceful MQTT DISCONNECT, which
    /// is what makes the broker fire the Last Will.
    ///
    /// `Child::kill()` is SIGKILL on Unix and `TerminateProcess` on Windows —
    /// matching production, where the Windows restart path is `taskkill /F` and
    /// there is no signal handler to publish `offline` politely.
    fn kill_ungracefully(&mut self) -> ExitStatus {
        self.assert_running(
            "before the ungraceful kill — so the kill would have killed NOTHING, and the \
             Last Will asserted below would have had no reason to fire",
        );
        self.child.kill().unwrap_or_else(|e| {
            panic!(
                "could not kill the host process (device_id={}): {e}",
                self.device_id
            )
        });
        let status = self.child.wait().unwrap_or_else(|e| {
            panic!(
                "could not reap the killed host process (device_id={}): {e}",
                self.device_id
            )
        });
        self.reaped = Some(status);
        assert!(
            !status.success(),
            "the host process exited SUCCESSFULLY ({status}) after being killed — it must have \
             raced to a clean exit, which means it may have disconnected gracefully and the \
             broker would never fire the Last Will"
        );
        status
    }

    /// One line about the child, folded into every timeout panic — "the host had
    /// already exited" is the single most common cause of a missing publish.
    fn status_note(&mut self) -> String {
        match self.child.try_wait() {
            Ok(None) => format!(
                "host state: still running (device_id={}, http port {})",
                self.device_id, self.http_port
            ),
            Ok(Some(status)) => format!(
                "host state: ALREADY EXITED with {status} (device_id={}, http port {}) — that \
                 is almost certainly why nothing arrived; check its log above",
                self.device_id, self.http_port
            ),
            Err(e) => format!("host state: unknown ({e})"),
        }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if self.reaped.is_some() {
            return;
        }
        // Best-effort on every exit path, panics included. Errors are ignored:
        // the child may already be gone, and Drop must not mask the real failure.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Broker-side subscriber
// ─────────────────────────────────────────────────────────────────────────────

/// One message as it arrived from the broker.
struct Received {
    topic: String,
    payload: Vec<u8>,
    /// The RETAIN flag **as delivered** — false on a live forward, true only on
    /// a retained replay to a freshly-subscribed client.
    retain: bool,
}

/// What the pump thread forwards to the test thread.
enum Note {
    Publish(Received),
    /// A connection-level event or error, kept purely for failure dumps.
    Event(String),
}

/// A broker-side subscriber whose event loop runs on its own thread.
///
/// The event loop is pumped continuously by a thread rather than polled with a
/// timeout from the test thread, so no `poll()` future is ever cancelled
/// mid-read and no packet can be lost by a timeout that fired at the wrong
/// moment. The test thread only ever reads an ordinary `std::sync::mpsc`
/// channel — with a deadline on every wait.
struct Subscriber {
    /// Declared FIRST so it is dropped FIRST: dropping the client closes
    /// rumqttc's request channel, which ends the pump thread and the TCP
    /// connection. No join is attempted (a join could hang); the thread simply
    /// unwinds to its end.
    ///
    /// This is why the pump thread must NOT hold a `Client` clone: a live clone
    /// would keep that request channel open forever, the pump would never end,
    /// and `evict_session`'s `drop(rogue)` would leave the rogue connected —
    /// trading evictions with the host indefinitely.
    client: Client,
    rx: Receiver<Note>,
    label: String,
    /// The filters this subscriber asked for, kept so they can be RE-subscribed
    /// after a reconnect (see [`Subscriber::resubscribe`]).
    filters: Vec<String>,
    /// Every publish received so far, in arrival order — the failure dump.
    log: Vec<Received>,
    /// Every connection-level note so far.
    events: Vec<String>,
}

impl Subscriber {
    /// Connect, wait for the CONNACK, subscribe to every filter, and wait for
    /// every SUBACK before returning.
    ///
    /// Returning only after the SUBACKs is what makes "subscribe before the host
    /// starts" a fact rather than a hope — which in turn is what makes the
    /// retain-flag reasoning in [`retains_discovery_availability_and_state`]
    /// valid.
    fn connect(label: &str, broker: &Broker, client_id: &str, filters: &[String]) -> Subscriber {
        let mut opts = MqttOptions::new(client_id, &broker.host, broker.port);
        opts.set_keep_alive(Duration::from_secs(5));
        opts.set_clean_session(true);

        let (client, mut connection) = Client::new(opts, 64);
        let (tx, rx) = std::sync::mpsc::channel::<Note>();
        let thread_label = label.to_string();
        std::thread::Builder::new()
            .name(format!("mqtt-sub-{label}"))
            .spawn(move || {
                for event in connection.iter() {
                    let note = match event {
                        Ok(Event::Incoming(Packet::Publish(publish))) => Note::Publish(Received {
                            topic: publish.topic,
                            payload: publish.payload.to_vec(),
                            retain: publish.retain,
                        }),
                        Ok(Event::Incoming(Packet::ConnAck(_))) => {
                            Note::Event("connack".to_string())
                        }
                        Ok(Event::Incoming(Packet::SubAck(_))) => Note::Event("suback".to_string()),
                        Ok(_) => continue,
                        Err(e) => {
                            // rumqttc yields `Some(Err(_))` for every failure
                            // except `RequestsDone`, and the NEXT `poll()`
                            // silently reconnects. Logging is therefore not the
                            // whole fix: under `set_clean_session(true)` the
                            // re-established session carries NO subscriptions,
                            // so the subscriber would go permanently deaf. The
                            // re-subscribe is driven off the next CONNACK in
                            // `drain_once`.
                            //
                            // Never hot-loop a broker that is hard down; the
                            // deadline on the test side is what fails the run.
                            std::thread::sleep(Duration::from_millis(250));
                            Note::Event(format!("connection error: {e}"))
                        }
                    };
                    if tx.send(note).is_err() {
                        break; // the test dropped the subscriber
                    }
                }
            })
            .unwrap_or_else(|e| panic!("could not spawn the pump thread for {thread_label}: {e}"));

        let mut sub = Subscriber {
            client,
            rx,
            label: label.to_string(),
            filters: filters.to_vec(),
            log: Vec::new(),
            events: Vec::new(),
        };

        sub.await_events("connack", 1, "the broker's CONNACK");
        for filter in filters {
            sub.client
                .subscribe(filter.clone(), QoS::AtLeastOnce)
                .unwrap_or_else(|e| {
                    panic!("subscriber {label} could not subscribe to {filter}: {e}")
                });
        }
        sub.await_events(
            "suback",
            filters.len(),
            "a SUBACK for every subscribed filter",
        );
        println!(
            "  subscriber {label} is connected and subscribed to {} filter(s)",
            filters.len()
        );
        sub
    }

    /// Block until `count` connection notes equal to `needle` have arrived.
    fn await_events(&mut self, needle: &str, count: usize, what: &str) {
        if count == 0 {
            return;
        }
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            let seen = self.events.iter().filter(|e| *e == needle).count();
            if seen >= count {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "TIMED OUT after {HANDSHAKE_TIMEOUT:?} waiting for {what} on subscriber {} \
                 (wanted {count} × {needle:?}, saw {seen}). The broker is unreachable, \
                 refusing the connection, or refusing the subscription.\n{}",
                self.label,
                self.dump()
            );
            self.drain_once(Duration::from_millis(250));
        }
    }

    /// A cursor into [`Subscriber::log`], so a later wait can be scoped to
    /// messages that arrive **after** some event (an eviction, a kill).
    fn mark(&self) -> usize {
        self.log.len()
    }

    /// Wait for one message at or after `from` satisfying `pred`, returning its
    /// index. Panics at the deadline with the full receive log.
    fn wait_for(
        &mut self,
        host: &mut HostProcess,
        what: &str,
        timeout: Duration,
        from: usize,
        pred: impl Fn(&Received) -> bool,
    ) -> usize {
        self.wait_for_matches(host, what, timeout, from, 1, pred)[0]
    }

    /// Wait for `want` messages at or after `from` satisfying `pred`, returning
    /// their indices in arrival order.
    fn wait_for_matches(
        &mut self,
        host: &mut HostProcess,
        what: &str,
        timeout: Duration,
        from: usize,
        want: usize,
        pred: impl Fn(&Received) -> bool,
    ) -> Vec<usize> {
        let deadline = Instant::now() + timeout;
        let mut cursor = from;
        let mut hits: Vec<usize> = Vec::new();
        loop {
            while cursor < self.log.len() {
                if pred(&self.log[cursor]) {
                    hits.push(cursor);
                }
                cursor += 1;
                if hits.len() >= want {
                    println!("  ok: saw {want} × {what}");
                    return hits;
                }
            }
            assert!(
                Instant::now() < deadline,
                "TIMED OUT after {timeout:?} waiting for {want} × {what} \
                 (on subscriber {}, scanning from index {from}; matched {} so far).\n{}\n{}",
                self.label,
                hits.len(),
                host.status_note(),
                self.dump()
            );
            self.drain_once(Duration::from_millis(250));
        }
    }

    /// Re-subscribe to every filter, after a reconnect dropped them.
    ///
    /// `try_subscribe` rather than `subscribe`: the blocking form parks on
    /// rumqttc's bounded request channel, and a wedged event loop would then
    /// hang the TEST thread past its own deadline instead of failing it. A
    /// refusal is recorded loudly instead of panicking, so the dump on the next
    /// timeout names the real cause rather than reporting "nothing arrived".
    fn resubscribe(&mut self) {
        let mut failed: Vec<String> = Vec::new();
        for filter in &self.filters {
            if let Err(e) = self.client.try_subscribe(filter.clone(), QoS::AtLeastOnce) {
                failed.push(format!("{filter} ({e})"));
            }
        }
        let note = if failed.is_empty() {
            format!(
                "reconnected: re-subscribed to {} filter(s)",
                self.filters.len()
            )
        } else {
            format!(
                "reconnected: re-subscribe FAILED for {failed:?} — this subscriber is now DEAF, \
                 so every wait below will time out reporting that nothing arrived even if the \
                 host published correctly"
            )
        };
        println!("  subscriber {}: {note}", self.label);
        self.events.push(note);
    }

    /// Move at most one message from the channel into the log.
    fn drain_once(&mut self, budget: Duration) {
        match self.rx.recv_timeout(budget) {
            Ok(Note::Publish(received)) => self.log.push(received),
            Ok(Note::Event(event)) => {
                // A CONNACK after the first one is a RECONNECT. The session was
                // opened clean, so the broker restored no subscriptions and this
                // subscriber is deaf until it asks again.
                let reconnected = event == "connack" && self.events.iter().any(|e| e == "connack");
                self.events.push(event);
                if reconnected && !self.filters.is_empty() {
                    self.resubscribe();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => panic!(
                "the pump thread for subscriber {} ended, so its MQTT connection is gone and \
                 no assertion below could ever be satisfied.\n{}",
                self.label,
                self.dump()
            ),
        }
    }

    /// Everything received so far, for a failure message. This harness is only
    /// debuggable from a CI log, so a timeout must say what DID arrive, not just
    /// that something did not.
    fn dump(&self) -> String {
        let mut out = format!(
            "--- subscriber {}: {} message(s) received ---\n",
            self.label,
            self.log.len()
        );
        if self.log.is_empty() {
            out.push_str("  (nothing at all — not one message arrived on any subscribed topic)\n");
        }
        for (i, message) in self.log.iter().enumerate() {
            out.push_str(&format!(
                "  [{i}] retain={} topic={} payload={}\n",
                message.retain,
                message.topic,
                render_payload(&message.payload)
            ));
        }
        out.push_str(&format!(
            "--- subscriber {}: connection notes {:?} ---",
            self.label, self.events
        ));
        out
    }
}

/// Render a payload for a failure dump, truncated so one fat discovery document
/// cannot bury the rest of the log.
fn render_payload(payload: &[u8]) -> String {
    let shown = payload.len().min(MAX_PAYLOAD_DUMP);
    let text = String::from_utf8_lossy(&payload[..shown]);
    if shown < payload.len() {
        format!("{text}... (truncated; {} bytes total)", payload.len())
    } else {
        text.into_owned()
    }
}

/// A received payload as text.
fn text(message: &Received) -> String {
    String::from_utf8_lossy(&message.payload).into_owned()
}

/// Parse a received payload as JSON, reporting the raw bytes on failure.
fn json(message: &Received, what: &str) -> Value {
    serde_json::from_slice(&message.payload).unwrap_or_else(|e| {
        panic!(
            "{what} on topic {} is not valid JSON: {e}\n  payload: {}",
            message.topic,
            render_payload(&message.payload)
        )
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixture
// ─────────────────────────────────────────────────────────────────────────────

/// A live subscriber (already subscribed) plus a freshly-spawned host.
struct Fixture {
    broker: Broker,
    device_id: String,
    sub: Subscriber,
    host: HostProcess,
}

/// Subscribe **first**, then start the host.
///
/// The ordering is load-bearing twice over: every message the live subscriber
/// sees is a live forward (retain cleared), and nothing can be missed in the
/// window between process start and subscription.
fn start(tag: &str, http_port: u16) -> Fixture {
    let broker = broker();
    let device_id = unique_device_id(tag);
    let device = DeviceId::new(&device_id).expect("harness device id must be valid");

    println!(
        "\n=== [{tag}] device_id={device_id} broker={} ===",
        broker.url
    );

    // The protocol builders are fine for SUBSCRIBING — a wrong topic here just
    // means the test sees nothing and fails loudly. Assertions use literals.
    let filters = vec![
        device.discovery_topic(),
        device.avail_topic(),
        device.state_topic(),
    ];
    let sub = Subscriber::connect(tag, &broker, &format!("itest-{device_id}"), &filters);
    let host = HostProcess::spawn(&broker, &device_id, http_port);

    Fixture {
        broker,
        device_id,
        sub,
        host,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. All three topics are published, under their literal names
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn publishes_on_the_three_frozen_topics() {
    let mut f = start("topics", 47901);
    let discovery = contract_discovery_topic(&f.device_id);
    let avail = contract_avail_topic(&f.device_id);
    let state = contract_state_topic(&f.device_id);

    println!(
        "asserting: the host publishes to exactly these three literal topics\n  \
         {discovery}\n  {avail}\n  {state}"
    );
    f.host.assert_running("immediately after spawn");

    let discovery_at = f.sub.wait_for(
        &mut f.host,
        &format!("a publish on the discovery topic {discovery}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == discovery,
    );
    let avail_at = f.sub.wait_for(
        &mut f.host,
        &format!("a publish on the availability topic {avail}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == avail,
    );
    let state_at = f.sub.wait_for(
        &mut f.host,
        &format!("a publish on the state topic {state}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == state,
    );

    assert!(
        !f.sub.log[discovery_at].payload.is_empty(),
        "the discovery message is EMPTY; an empty retained payload is how Home Assistant is \
         told to DELETE a device"
    );
    assert_eq!(
        text(&f.sub.log[avail_at]),
        "online",
        "the birth message on {avail} must be the literal `online` (the LWT payload is \
         `offline`), got {:?}",
        text(&f.sub.log[avail_at])
    );
    assert!(
        !f.sub.log[state_at].payload.is_empty(),
        "the state message is EMPTY"
    );

    f.host
        .assert_running("after all three contract topics were observed");
    println!("ok: all three frozen topics were published");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. All three are RETAINED
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn retains_discovery_availability_and_state() {
    let mut f = start("retain", 47902);
    let discovery = contract_discovery_topic(&f.device_id);
    let avail = contract_avail_topic(&f.device_id);
    let state = contract_state_topic(&f.device_id);
    let topics = [discovery.clone(), avail.clone(), state.clone()];

    // ── Phase 1: a LIVE subscriber proves the messages are published ──────────
    //
    // It proves NOTHING about retention. MQTT 3.1.1 §3.3.1.3: a broker forwards
    // to an already-subscribed client with the RETAIN flag CLEARED. Asserting
    // retain here would assert the wrong thing and fail on a correct publisher.
    println!("phase 1: live subscriber — the three messages are published (retain flag cleared on a live forward)");
    let mut first_seen = Vec::new();
    for topic in &topics {
        let at = f.sub.wait_for(
            &mut f.host,
            &format!("a live publish on {topic}"),
            PUBLISH_TIMEOUT,
            0,
            |m| &m.topic == topic,
        );
        assert!(
            !f.sub.log[at].retain,
            "{topic} arrived at an ALREADY-SUBSCRIBED client with retain=true. That is not how \
             MQTT 3.1.1 forwards live messages, so this harness's retain reasoning (phase 2) \
             would be testing the wrong thing."
        );
        first_seen.push(at);
    }
    println!("  live deliveries at indices {first_seen:?}");

    // ── Phase 2: a FRESH subscriber proves the retain flags ───────────────────
    //
    // It subscribes AFTER the publishes, so everything it receives on these
    // topics is a retained replay — and the broker sets RETAIN on a replay. This
    // is the only place retention is observable at all.
    println!("phase 2: fresh subscriber — the retained replay must carry retain=true on all three");
    let mut fresh = Subscriber::connect(
        "retain-fresh",
        &f.broker,
        &format!("itest-fresh-{}", f.device_id),
        &topics,
    );
    for topic in &topics {
        let at = fresh.wait_for(
            &mut f.host,
            &format!("a RETAINED replay of {topic} to a freshly-subscribed client"),
            REPLAY_TIMEOUT,
            0,
            |m| &m.topic == topic,
        );
        assert!(
            fresh.log[at].retain,
            "{topic} was replayed to a FRESH subscriber without the retain flag, which means \
             it is not retained on the broker. A consumer that subscribes after the fact — \
             Home Assistant restarting, for instance — would see nothing at all."
        );
    }

    println!("ok: discovery, availability and state are all retained");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The discovery document's shape
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn discovery_document_has_the_shape_home_assistant_reads() {
    let mut f = start("disco", 47903);
    let discovery = contract_discovery_topic(&f.device_id);
    let avail = contract_avail_topic(&f.device_id);
    let state = contract_state_topic(&f.device_id);

    let at = f.sub.wait_for(
        &mut f.host,
        &format!("the discovery document on {discovery}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == discovery,
    );
    let doc = json(&f.sub.log[at], "the discovery document");

    // ── availability: the LIST form, with the payloads INSIDE the entry ───────
    //
    // `availability_topic` at the root is an unknown key, and Home Assistant
    // ignores unknown keys rather than rejecting them: every entity would
    // register and then sit permanently "available" while the Last Will fired
    // into the void. That is invisible without a live HA, so it is pinned here.
    println!("asserting: availability is the list form and availability_topic is NOT at the root");
    let entries = doc
        .get("availability")
        .unwrap_or_else(|| panic!("no `availability` key at the document root: {doc}"))
        .as_array()
        .unwrap_or_else(|| panic!("`availability` is not a LIST: {doc}"));
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one availability entry: {doc}"
    );
    let entry = &entries[0];
    assert_eq!(
        entry.get("topic").and_then(Value::as_str),
        Some(avail.as_str()),
        "the availability entry must point at the literal LWT topic: {entry}"
    );
    assert_eq!(
        entry.get("payload_available").and_then(Value::as_str),
        Some("online"),
        "payload_available must live INSIDE the availability entry: {entry}"
    );
    assert_eq!(
        entry.get("payload_not_available").and_then(Value::as_str),
        Some("offline"),
        "payload_not_available must live INSIDE the availability entry: {entry}"
    );
    assert!(
        doc.get("availability_topic").is_none(),
        "`availability_topic` appears at the document ROOT, where Home Assistant silently \
         ignores it — every entity would register and stay permanently available: {doc}"
    );

    // ── no software version anywhere in this RETAINED document ────────────────
    //
    // The desktop's two boots install independently, so a version here would
    // rewrite the retained config on every OS switch.
    println!("asserting: no `sw` in `dev` or `o` (a retained doc must not churn per boot)");
    for block in ["dev", "o"] {
        let value = doc
            .get(block)
            .unwrap_or_else(|| panic!("no `{block}` block in the discovery document: {doc}"));
        // Prove it is an OBJECT first. `Value::get` returns `None` on a string,
        // number or null, so a `dev` that regressed to a non-object would sail
        // through the `sw` check below having proved nothing.
        let object = value.as_object().unwrap_or_else(|| {
            panic!("`{block}` is not an OBJECT, so the `sw` check below would be vacuous: {doc}")
        });
        assert!(
            !object.contains_key("sw"),
            "`{block}.sw` is set; a software version in a RETAINED discovery document rewrites \
             it on every independent boot update: {value}"
        );
    }

    // ── components ────────────────────────────────────────────────────────────
    println!("asserting: components use `p` (not `platform`), sensors carry state_topic, buttons do not, unique_ids are prefixed");
    let cmps = doc
        .get("cmps")
        .unwrap_or_else(|| panic!("no `cmps` block in the discovery document: {doc}"))
        .as_object()
        .unwrap_or_else(|| panic!("`cmps` is not an object: {doc}"));
    assert!(!cmps.is_empty(), "`cmps` is empty: {doc}");

    for (key, component) in cmps {
        let platform = component
            .get("p")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("component {key} has no `p` (platform) key: {component}"));
        assert!(
            component.get("platform").is_none(),
            "component {key} spells the platform key `platform`; the wire form is `p`: {component}"
        );
        assert_eq!(
            component.get("unique_id").and_then(Value::as_str),
            Some(contract_unique_id(&f.device_id, key).as_str()),
            "component {key} has the wrong unique_id: {component}"
        );
        if platform == "button" {
            assert!(
                component.get("state_topic").is_none(),
                "button {key} carries a state_topic, an option its platform does not accept: \
                 {component}"
            );
        } else {
            assert_eq!(
                component.get("state_topic").and_then(Value::as_str),
                Some(state.as_str()),
                "sensor {key} has the wrong (or missing) state_topic: {component}"
            );
        }
    }

    // ── the underscore/hyphen split, spelled out ──────────────────────────────
    //
    // The `cmps` KEY is `open_bpm` (underscore); the command TOPIC segment is
    // `open-bpm` (hyphen). Both spellings are contract.
    println!("asserting: the `open_bpm` component (underscore) commands `open-bpm` (hyphen)");
    let open_bpm = cmps.get("open_bpm").unwrap_or_else(|| {
        panic!(
            "no `open_bpm` component (UNDERSCORE key) — present keys: {:?}",
            cmps.keys().collect::<Vec<_>>()
        )
    });
    assert_eq!(
        open_bpm.get("p").and_then(Value::as_str),
        Some("button"),
        "`open_bpm` must be a button: {open_bpm}"
    );
    assert!(
        open_bpm.get("state_topic").is_none(),
        "the `open_bpm` button must NOT carry a state_topic: {open_bpm}"
    );
    assert_eq!(
        open_bpm.get("command_topic").and_then(Value::as_str),
        Some(contract_cmd_topic(&f.device_id, "open-bpm").as_str()),
        "the `open_bpm` button's command topic segment is spelled with a HYPHEN: {open_bpm}"
    );

    f.host
        .assert_running("after the discovery document was validated");
    println!("ok: the discovery document matches the frozen shape");
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. The state envelope
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn state_envelope_carries_the_schema_and_staleness_fields() {
    let mut f = start("envelope", 47904);
    let state = contract_state_topic(&f.device_id);

    let at = f.sub.wait_for(
        &mut f.host,
        &format!("a state envelope on {state}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == state,
    );
    let envelope = json(&f.sub.log[at], "the state envelope");
    println!("asserting: schema_version == 1; published_at + seq are INTEGERS; current_os set; status nested");

    assert_eq!(
        envelope.get("schema_version").and_then(Value::as_u64),
        Some(1),
        "schema_version must be the integer 1: {envelope}"
    );

    // `published_at` and `seq` are the half-open-socket detectors. If either
    // shipped as a STRING, Home Assistant's `| int` and `timestamp_utc`
    // templates would degrade quietly rather than error.
    for field in ["published_at", "seq"] {
        let value = envelope
            .get(field)
            .unwrap_or_else(|| panic!("no `{field}` in the state envelope: {envelope}"));
        assert!(
            value.is_u64(),
            "`{field}` must be an INTEGER, got {value} — a string here would make the Home \
             Assistant timestamp/measurement templates degrade silently: {envelope}"
        );
    }

    let current_os = envelope
        .get("current_os")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no string `current_os` in the state envelope: {envelope}"));
    assert!(
        ["linux", "windows", "macos", "unknown"].contains(&current_os),
        "`current_os` is {current_os:?}, not one of the DeviceOs spellings: {envelope}"
    );

    let status = envelope
        .get("status")
        .unwrap_or_else(|| panic!("no `status` object in the state envelope: {envelope}"))
        .as_object()
        .unwrap_or_else(|| panic!("`status` is not a NESTED object: {envelope}"));
    // `status` is contractually `StatusResponse` VERBATIM, so assert the types,
    // not merely the keys: `contains_key` passes on a `version` that regressed
    // to a number or a `streaming` that became the string "true", which is
    // exactly the drift this block exists to catch.
    let version = status
        .get("version")
        .unwrap_or_else(|| panic!("`status.version` is missing: {envelope}"));
    assert!(
        version.is_string(),
        "`status.version` must be a STRING (StatusResponse::version), got {version}: {envelope}"
    );
    let streaming = status
        .get("streaming")
        .unwrap_or_else(|| panic!("`status.streaming` is missing: {envelope}"));
    assert!(
        streaming.is_boolean(),
        "`status.streaming` must be a BOOLEAN (StatusResponse::streaming), got {streaming}: \
         {envelope}"
    );
    // `running_appid` is `Option<u32>`, so JSON `null` is the correct value on a
    // runner with nothing running — presence is all that can be asserted here.
    let running_appid = status
        .get("running_appid")
        .unwrap_or_else(|| panic!("`status.running_appid` is missing: {envelope}"));
    assert!(
        running_appid.is_u64() || running_appid.is_null(),
        "`status.running_appid` must be an INTEGER or null (StatusResponse::running_appid is \
         Option<u32>), got {running_appid}: {envelope}"
    );

    f.host
        .assert_running("after the state envelope was validated");
    println!("ok: the state envelope matches the frozen schema");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. The floor heartbeat
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn heartbeat_keeps_seq_and_published_at_advancing() {
    // On a bare CI runner with no Steam, `probe_status()` returns a constant, so
    // `changed` is false after the first publish and nothing but the heartbeat
    // can produce a second message. That makes this a clean heartbeat assertion
    // — and it is also why "publish on change" is not testable here (the ConnAck
    // forced-publish path in `reconnect_republishes_discovery` covers that).
    //
    // The DEFAULT heartbeat is 30 s. That VALUE is pinned by the pure
    // env-parsing unit tests in `host/src/mqtt.rs`; this test covers the
    // MECHANISM, so it runs at TV_SHELL_MQTT_HEARTBEAT_SECS=2.
    let mut f = start("hb", 47905);
    let state = contract_state_topic(&f.device_id);

    println!(
        "asserting: at least 3 state messages arrive, with seq strictly increasing and \
         published_at advancing (heartbeat={HEARTBEAT_SECS}s, but the publish loop's fixed 5s \
         probe tick is the real floor, so expect ~10s)"
    );
    let indices = f.sub.wait_for_matches(
        &mut f.host,
        &format!("a state message on {state}"),
        HEARTBEAT_TIMEOUT,
        0,
        3,
        |m| m.topic == state,
    );

    let mut seqs = Vec::new();
    let mut stamps = Vec::new();
    for at in &indices {
        let envelope = json(&f.sub.log[*at], "a heartbeat state envelope");
        seqs.push(
            envelope
                .get("seq")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("no integer `seq` in {envelope}")),
        );
        stamps.push(
            envelope
                .get("published_at")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("no integer `published_at` in {envelope}")),
        );
    }
    println!("  observed seqs={seqs:?} published_at={stamps:?}");

    // STRICTLY INCREASING, deliberately NOT gap-free. `publish_loop` uses
    // `try_publish`, so envelopes are DROPPED while the link is down, and `seq`
    // advances before serialization — a gap is correct behaviour and asserting
    // gap-freedom would be flaky.
    for pair in seqs.windows(2) {
        assert!(
            pair[1] > pair[0],
            "seq must strictly increase across heartbeats (gaps are EXPECTED — try_publish \
             drops envelopes while the link is down and seq advances before serialization); \
             a frozen or rewinding seq is exactly the half-open-socket wedge this field exists \
             to expose. Observed: {seqs:?}"
        );
    }
    let first = stamps.first().copied().unwrap_or_default();
    let last = stamps.last().copied().unwrap_or_default();
    assert!(
        last > first,
        "published_at did not advance across the heartbeat window ({stamps:?}). A frozen \
         published_at on a connected client is the 13.5-hour half-open-socket failure this \
         envelope was designed to make visible."
    );

    f.host.assert_running("after the heartbeat window");
    println!("ok: the heartbeat advances seq and published_at");
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. The Last Will, on an ungraceful death
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn ungraceful_termination_fires_the_last_will() {
    let mut f = start("lwt", 47906);
    let avail = contract_avail_topic(&f.device_id);

    let online_at = f.sub.wait_for(
        &mut f.host,
        &format!("the `online` birth message on {avail}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == avail,
    );
    assert_eq!(
        text(&f.sub.log[online_at]),
        "online",
        "expected the birth message before the kill"
    );

    let mark = f.sub.mark();
    println!(
        "asserting: after an ungraceful kill (SIGKILL / TerminateProcess — no graceful \
         DISCONNECT, matching the Windows `taskkill /F` restart path), the broker publishes \
         the retained Last Will `offline` on {avail}"
    );
    // Proves the child was alive first: a kill that killed nothing exits happily
    // and leaves the harness waiting on a will that had no reason to fire.
    let status = f.host.kill_ungracefully();
    println!("  killed the host ungracefully; it exited with {status}");

    let offline_at = f.sub.wait_for(
        &mut f.host,
        &format!(
            "the Last Will `offline` on {avail} (the broker waits ~1.5x keepalive = ~{}s \
             after the socket dies before firing it)",
            KEEPALIVE_SECS * 3 / 2
        ),
        WILL_TIMEOUT,
        mark,
        |m| m.topic == avail && m.payload == b"offline",
    );
    assert_eq!(
        text(&f.sub.log[offline_at]),
        "offline",
        "the Last Will payload must be the literal `offline`"
    );

    println!("ok: the Last Will fired on ungraceful termination");
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. ConnAck republishes discovery
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a live broker: set TV_SHELL_TEST_BROKER and run with --ignored"]
fn reconnect_republishes_discovery() {
    let mut f = start("reconn", 47907);
    let discovery = contract_discovery_topic(&f.device_id);
    let avail = contract_avail_topic(&f.device_id);

    let first = f.sub.wait_for(
        &mut f.host,
        &format!("the FIRST discovery publish on {discovery}"),
        PUBLISH_TIMEOUT,
        0,
        |m| m.topic == discovery,
    );
    println!("  first discovery publish at index {first}");
    let mark = f.sub.mark();

    // Force a reconnect from OUTSIDE the process. `probe_status()` is constant on
    // a bare CI runner, so no state change can be provoked; the one lever left is
    // MQTT 3.1.1 §3.1.4-2, which requires the broker to disconnect an existing
    // session when a second client connects with the SAME client id.
    println!(
        "asserting: evicting the host's session (a rogue client using its client id \
         `{}`) makes it reconnect, and the ConnAck path republishes discovery",
        contract_client_id(&f.device_id)
    );
    evict_session(&f.broker, &contract_client_id(&f.device_id));

    // Prove the EVICTION landed before asserting anything about the republish.
    //
    // The rogue's own CONNACK proves only that the ROGUE connected — it says
    // nothing about whose session was displaced. If `contract_client_id` ever
    // drifts from the client id the host actually connects with, no session is
    // evicted, the host never reconnects, and the wait below fails blaming the
    // ConnAck republish path for what is really a harness bug.
    //
    // A client-id takeover closes the old socket WITHOUT a DISCONNECT, so the
    // broker fires the retained Last Will. That `offline` is the observable
    // proof the takeover hit the host. (The host reconnects on a 1 s backoff and
    // republishes `online` shortly after; harmless — this scans an indexed log,
    // so the `offline` entry stays put at its index.)
    let offline_at = f.sub.wait_for(
        &mut f.host,
        &format!(
            "the Last Will `offline` on {avail} — the proof that the eviction actually \
             displaced the HOST's session, and not merely that the rogue connected (a \
             client-id drift between `contract_client_id` and the host's real client id \
             evicts nothing at all)"
        ),
        WILL_TIMEOUT,
        mark,
        |m| m.topic == avail && m.payload == b"offline",
    );
    println!("  the host's session was evicted (Last Will `offline` at index {offline_at})");

    f.sub.wait_for(
        &mut f.host,
        &format!(
            "a SECOND discovery publish on {discovery} after the session eviction — the \
             ConnAck path must republish, because a reconnect may mean the broker dropped \
             our session. A timeout here means EITHER the ConnAck path failed to republish, \
             OR the session was never evicted (client-id drift between `contract_client_id` \
             and the host's real client id)"
        ),
        RECONNECT_TIMEOUT,
        mark,
        |m| m.topic == discovery,
    );

    f.host
        .assert_running("after the reconnect republish (it must have survived the eviction)");
    println!("ok: the host reconnected and republished discovery");
}

/// Connect a throwaway client under `client_id`, so the broker evicts whoever
/// already holds it, then leave immediately.
///
/// Leaving immediately matters: if the rogue stayed connected, the host's own
/// reconnect would evict IT, and the two would trade evictions indefinitely.
/// Dropping the [`Subscriber`] closes the rogue's request channel, which ends its
/// pump thread and its TCP connection.
fn evict_session(broker: &Broker, client_id: &str) {
    // Zero filters: `Subscriber::connect` still waits for the CONNACK, which is
    // the proof that the eviction actually happened. Without that proof a
    // failed connect would leave the caller waiting for a reconnect that had no
    // reason to occur — the silent-no-op failure mode.
    let rogue = Subscriber::connect("rogue", broker, client_id, &[]);
    println!("  rogue client got its CONNACK under {client_id}; the host's session is evicted");
    drop(rogue);
}

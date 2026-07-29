//! Shared MQTT wire types: the state envelope, device identity, and the Home
//! Assistant device-based discovery document.
//!
//! Two tv-shell processes publish to MQTT, each over its **own** connection so
//! each carries its **own** Last Will:
//!
//! | device | `device_id` | crate | `status` shape |
//! |---|---|---|---|
//! | the TV client | `htpc-1` | `tv-shell-input` (`daemon/`) | [`ShellSnapshot`] |
//! | the gaming PC (dual-boot, ONE machine) | `desktop` | `tv-shell-host` (`host/`) | [`crate::StatusResponse`] |
//!
//! One retained message on one topic per device, so a consumer never sees a torn
//! read across topics. The two devices share the [`StateEnvelope`] but **not** the
//! `status` type: the host sidecar already has a canonical three-field
//! [`crate::StatusResponse`], while the daemon's `GET /status` body is assembled
//! ad-hoc from daemon-local structs and therefore needs its own shape here.
//!
//! Pure serde — no clock, no I/O, no time crate. `published_at` is supplied by the
//! caller (see [`StateEnvelope`] for why that field is load-bearing).
//!
//! ## Availability is per-entity, in three tiers
//!
//! These machines **sleep as their normal resting state** — the HTPC and the
//! gaming PC are both suspended most of the day. A single shared `availability`
//! at the document root made every entity `unavailable` for all of that time,
//! which is a bad trade: the state topic is *retained*, so the broker is still
//! holding a perfectly good last-known reading that availability then hides.
//!
//! So availability is stamped per-component by [`base_discovery`], in tiers:
//!
//! | tier | entities | gated? | rendering while asleep |
//! |---|---|---|---|
//! | commands | every `button` | **yes** | `unavailable` — pressing it cannot work |
//! | liveness | `status_stale`, `age_seconds` | **yes** | `unavailable` — a frozen value would lie |
//! | facts | everything else | no | last known value, as of `published_at` |
//!
//! Two things make the ungated tier safe rather than merely quieter. First,
//! `published_at` carries `device_class: timestamp`, which Home Assistant renders
//! as relative time — so "how old is this?" is answered on the face of the
//! dashboard rather than inferred from an entity being missing. Second, the
//! `connected` binary sensor reads the LWT topic directly, so the online/offline
//! fact the root availability used to encode is still a first-class entity.
//!
//! Getting the tiers wrong is silent in both directions: an ungated liveness
//! sensor reports stale data as fresh, and a gated connectivity sensor goes
//! `unavailable` exactly when it has something to report.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Envelope schema version. Bump only on a breaking envelope change.
pub const SCHEMA_VERSION: u32 = 1;
/// Root segment of every device topic (`tv-shell/<device_id>/…`).
pub const TOPIC_ROOT: &str = "tv-shell";
/// Home Assistant MQTT discovery prefix.
pub const DISCOVERY_PREFIX: &str = "homeassistant";
/// Availability payload published on connect (and as the retained birth message).
pub const AVAIL_ONLINE: &str = "online";
/// Availability payload registered as the connection's Last Will.
pub const AVAIL_OFFLINE: &str = "offline";

// ─────────────────────────────────────────────────────────────────────────────
// Device identity
// ─────────────────────────────────────────────────────────────────────────────

/// Why a [`DeviceId`] string was rejected.
///
/// Hand-rolled rather than `thiserror` — `protocol/` is deliberately a
/// serde-only crate and gains no dependencies for one three-variant error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceIdError {
    /// The configured id was the empty string.
    Empty,
    /// The configured id exceeded 64 bytes.
    TooLong,
    /// The configured id contained a character outside `[A-Za-z0-9_-]`.
    InvalidChar(char),
}

impl fmt::Display for DeviceIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceIdError::Empty => write!(f, "device_id must not be empty"),
            DeviceIdError::TooLong => write!(f, "device_id must be at most 64 bytes"),
            DeviceIdError::InvalidChar(c) => write!(
                f,
                "device_id contains {c:?}; only ASCII letters, digits, '_' and '-' are allowed"
            ),
        }
    }
}

impl std::error::Error for DeviceIdError {}

/// A validated MQTT device identifier — the one input every topic is built from.
///
/// `device_id` is **explicitly configured, never derived** from hostname/OS/IP:
/// the gaming PC is one physical machine that dual-boots CachyOS and Windows, and
/// deriving the id would split it into two Home Assistant devices that alternate.
/// Making it a validated newtype is how "fail closed if unset" is enforced
/// structurally — a topic cannot be constructed without one.
///
/// The accepted alphabet is `[A-Za-z0-9_-]`, deliberately **narrower than MQTT
/// permits**. It excludes `/` (would inject extra topic levels), `+` and `#`
/// (single/multi-level wildcards — a subscriber id would silently match sibling
/// devices), `$` (reserved topic prefix), whitespace, control characters, and all
/// non-ASCII — every one of which would either break topic construction or create
/// a wildcard/reserved topic that looks like it works.
///
/// Serde goes through `String`, so a bad `device_id` in a config file fails the
/// **parse** at daemon startup rather than at first publish.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(into = "String", try_from = "String")]
pub struct DeviceId(String);

impl DeviceId {
    /// Validate `raw` and wrap it.
    ///
    /// Checks run in order: empty → longer than 64 bytes → disallowed character.
    pub fn new(raw: &str) -> Result<Self, DeviceIdError> {
        if raw.is_empty() {
            return Err(DeviceIdError::Empty);
        }
        if raw.len() > 64 {
            return Err(DeviceIdError::TooLong);
        }
        if let Some(c) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
        {
            return Err(DeviceIdError::InvalidChar(c));
        }
        Ok(DeviceId(raw.to_string()))
    }

    /// The validated id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Home Assistant device identifier: `tv-shell-<id>`.
    pub fn ha_device_identifier(&self) -> String {
        format!("{TOPIC_ROOT}-{}", self.0)
    }

    /// Home Assistant entity unique id: `tv-shell-<id>-<entity_key>`.
    pub fn unique_id(&self, entity_key: &str) -> String {
        format!("{}-{entity_key}", self.ha_device_identifier())
    }

    /// Retained state topic: `tv-shell/<id>/state`.
    pub fn state_topic(&self) -> String {
        format!("{TOPIC_ROOT}/{}/state", self.0)
    }

    /// Retained availability topic (the connection's LWT): `tv-shell/<id>/avail`.
    pub fn avail_topic(&self) -> String {
        format!("{TOPIC_ROOT}/{}/avail", self.0)
    }

    /// Non-retained command topic: `tv-shell/<id>/cmd/<name>`.
    pub fn cmd_topic(&self, name: &str) -> String {
        format!("{TOPIC_ROOT}/{}/cmd/{name}", self.0)
    }

    /// Subscription filter covering every command topic: `tv-shell/<id>/cmd/+`.
    pub fn cmd_topic_filter(&self) -> String {
        self.cmd_topic("+")
    }

    /// Retained discovery topic: `homeassistant/device/tv-shell-<id>/config`.
    pub fn discovery_topic(&self) -> String {
        format!(
            "{DISCOVERY_PREFIX}/device/{}/config",
            self.ha_device_identifier()
        )
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<DeviceId> for String {
    fn from(id: DeviceId) -> String {
        id.0
    }
}

impl TryFrom<String> for DeviceId {
    type Error = DeviceIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        DeviceId::new(&value)
    }
}

/// Which OS the publishing process is running on.
///
/// `Macos` and `Unknown` exist only so [`DeviceOs::current`] is **total** and can
/// never panic — the host crate is CI-built on macOS. In production only `linux`
/// (htpc-1, and the desktop's CachyOS boot) and `windows` (the desktop's Windows
/// boot) are ever emitted.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceOs {
    /// Linux.
    Linux,
    /// Windows.
    Windows,
    /// macOS (CI only).
    Macos,
    /// Any other build target (CI only).
    Unknown,
}

impl DeviceOs {
    /// The OS this binary was compiled for. Total; never panics.
    pub fn current() -> DeviceOs {
        if cfg!(target_os = "linux") {
            DeviceOs::Linux
        } else if cfg!(target_os = "windows") {
            DeviceOs::Windows
        } else if cfg!(target_os = "macos") {
            DeviceOs::Macos
        } else {
            DeviceOs::Unknown
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// State envelope
// ─────────────────────────────────────────────────────────────────────────────

/// The one shared state envelope, published retained to
/// [`DeviceId::state_topic`].
///
/// Field order is the **wire order** — do not reorder.
///
/// # Why `published_at` and `seq` exist
///
/// They are not bookkeeping; they are the reason this envelope has a shape at
/// all. A client can keep "publishing" into a **half-open socket** long after the
/// broker has given up on it and fired the client's Last Will. That failure mode
/// really happened on this broker for 13.5 hours: every consumer read
/// `unavailable` while the publisher's own logs looked perfectly healthy.
///
/// Availability cannot express *"connected, but nothing is arriving"*. A
/// `published_at` that stops advancing and a `seq` that stops incrementing can —
/// paired with a floor heartbeat on the publisher, they turn a silent wedge into
/// a measurable staleness. Never "simplify" them away.
///
/// Both are plain `u64`: `protocol/` has no clock and must not gain a time crate,
/// so the **caller** supplies the unix-seconds timestamp and the per-process
/// monotonic sequence number (starting at 0).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StateEnvelope<S> {
    /// Envelope schema version — always [`SCHEMA_VERSION`] when built via
    /// [`StateEnvelope::new`].
    pub schema_version: u32,
    /// Unix seconds at publish time. MUST advance on every publish.
    pub published_at: u64,
    /// Monotonic per-process counter, starting at 0. MUST increment per publish.
    pub seq: u64,
    /// Which OS the publishing process is running on.
    pub current_os: DeviceOs,
    /// The per-device status object — a different type per device.
    pub status: S,
}

impl<S> StateEnvelope<S> {
    /// Build an envelope, stamping [`SCHEMA_VERSION`] so a caller cannot forget it.
    pub fn new(published_at: u64, seq: u64, current_os: DeviceOs, status: S) -> Self {
        StateEnvelope {
            schema_version: SCHEMA_VERSION,
            published_at,
            seq,
            current_os,
            status,
        }
    }
}

/// What the desktop sidecar (`tv-shell-host`) publishes.
///
/// `status` is [`crate::StatusResponse`] **verbatim** — the same three-field type
/// the daemon already parses over HTTP, with its byte-exact field order intact.
pub type HostState = StateEnvelope<crate::StatusResponse>;

/// What the TV client daemon (`tv-shell-input`) publishes.
pub type ShellState = StateEnvelope<ShellSnapshot>;

/// Default for [`ShellSnapshot::cec_display_ownership`].
fn unknown_ownership() -> String {
    "unknown".to_string()
}

/// htpc-1's status shape — the daemon has no single serde type for `GET /status`.
///
/// The daemon's HTTP status body is a `#[serde(flatten)]` of two daemon-local
/// structs assembled per-request, and [`crate::StatusResponse`] is the *host's*
/// three-field type, so neither can be reused. This mirrors the daemon's shell,
/// CEC display-ownership, and system halves in one flat object.
///
/// Every field defaults, so a consumer parsing an older or newer payload degrades
/// field-by-field instead of failing the whole parse. Not `Eq` — it carries `f64`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ShellSnapshot {
    // ── shell half (mirrors daemon `shell_state::ShellStatus`) ──
    /// Last shell state pushed by the QML shell (`idle`/`streaming`/…), if any.
    #[serde(default)]
    pub shell_state: Option<String>,
    /// Whether media is currently playing.
    #[serde(default)]
    pub media_playing: bool,
    /// Whether the shell's last state push is older than `stale_after_seconds`.
    #[serde(default)]
    pub stale: bool,
    /// Age of the last shell state push, in seconds.
    #[serde(default)]
    pub age_seconds: Option<u64>,
    /// Staleness threshold the daemon applies to `age_seconds`.
    #[serde(default)]
    pub stale_after_seconds: u64,
    /// Whether the QML shell process is up.
    #[serde(default)]
    pub shell_running: bool,

    // ── CEC display-ownership half (mirrors daemon `display_owner::DisplayOwnerStatus`) ──
    /// The daemon's `display_owner::Ownership` **serialized value**, carried as a
    /// `String` on purpose: `protocol/` does not duplicate (and therefore cannot
    /// drift from) the daemon's enum. The daemon owns a test pinning its enum's
    /// serialized values to the strings that appear here.
    ///
    /// Defaults to `"unknown"` — never `""`. `"unknown"` means *no evidence at
    /// all* and must not be read as "nobody is watching"; an empty string would be
    /// a third, undefined state.
    #[serde(default = "unknown_ownership")]
    pub cec_display_ownership: String,
    /// CEC logical address currently believed to own the display.
    #[serde(default)]
    pub cec_display_owner: Option<i32>,
    /// This box's own CEC logical address.
    #[serde(default)]
    pub cec_local_address: Option<i32>,
    /// Unix seconds at which display ownership last changed.
    #[serde(default)]
    pub cec_display_owner_changed_unix: Option<u64>,
    /// How long the current owner has held the display, in seconds.
    #[serde(default)]
    pub cec_display_owner_held_seconds: Option<u64>,
    /// Whether an owner has ever been observed since daemon start.
    #[serde(default)]
    pub cec_display_owner_ever_observed: bool,
    /// Whether ownership tracking is active at all.
    #[serde(default)]
    pub cec_display_owner_tracking: bool,

    // ── system half ──
    /// `tv-shell-input` package version.
    #[serde(default)]
    pub version: String,
    /// System-wide CPU utilisation percentage.
    #[serde(default)]
    pub cpu_percent: Option<f64>,
    /// System-wide memory utilisation percentage.
    #[serde(default)]
    pub mem_percent: Option<f64>,
    /// Host uptime in seconds.
    #[serde(default)]
    pub uptime_seconds: Option<u64>,
}

impl Default for ShellSnapshot {
    /// Hand-written, not derived: `cec_display_ownership` must default to
    /// `"unknown"`, and `String::default()` would give `""`. Every other field
    /// takes its natural default.
    fn default() -> Self {
        ShellSnapshot {
            shell_state: None,
            media_playing: false,
            stale: false,
            age_seconds: None,
            stale_after_seconds: 0,
            shell_running: false,
            cec_display_ownership: unknown_ownership(),
            cec_display_owner: None,
            cec_local_address: None,
            cec_display_owner_changed_unix: None,
            cec_display_owner_held_seconds: None,
            cec_display_owner_ever_observed: false,
            cec_display_owner_tracking: false,
            version: String::new(),
            cpu_percent: None,
            mem_percent: None,
            uptime_seconds: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Home Assistant device-based discovery
// ─────────────────────────────────────────────────────────────────────────────
//
// These types are publish-only — tv-shell writes the discovery document and
// never parses one back — so they derive `Serialize` but not `Deserialize`.

/// One retained device-based discovery document, published to
/// [`DeviceId::discovery_topic`].
///
/// Only genuinely global options (QoS) sit at the document root; every
/// per-entity option — including **availability** — lives under `cmps`.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct DeviceDiscovery {
    /// The Home Assistant device this document registers.
    pub dev: DeviceInfo,
    /// Origin (which software published this).
    pub o: OriginInfo,
    /// Components (entities), keyed by a stable entity key.
    ///
    /// **`BTreeMap`, never a `HashMap`.** This message is *retained*: a
    /// nondeterministic map order would serialize to different bytes on every
    /// boot and rewrite the retained config each time — exactly the churn that
    /// publishing an identical component set from both desktop boots exists to
    /// prevent. Do not "optimize" this to a `HashMap`.
    pub cmps: BTreeMap<String, Component>,
    /// QoS for the shared subscriptions.
    pub qos: u8,
}

/// One entry in a component's `availability` list.
///
/// `payload_available` / `payload_not_available` are only meaningful *inside*
/// an entry; as siblings of `availability` they are unknown keys and are ignored.
///
/// **Always the list form, never `availability_topic`.** Home Assistant documents
/// `availability` (the list form) as the supported spelling and treats unknown
/// keys as *allowed but ignored* rather than rejecting them. So a document that
/// said `availability_topic` would register every entity and leave them
/// permanently "available" while the Last Will fired into the void — a silent
/// failure invisible without both a live broker and a live Home Assistant. What
/// has **not** been established is whether `availability_topic` also happens to
/// work; treat "the other form is broken" as untested and this one as the safe
/// encoding either way.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AvailabilityEntry {
    /// The retained LWT topic: `tv-shell/<device_id>/avail`.
    pub topic: String,
    /// Payload meaning "available". Matches HA's default, sent explicitly.
    pub payload_available: String,
    /// Payload meaning "not available". Matches HA's default, sent explicitly.
    pub payload_not_available: String,
}

/// The Home Assistant device record.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Device identifiers — always exactly `[tv-shell-<device_id>]`.
    #[serde(rename = "ids")]
    pub identifiers: Vec<String>,
    /// Device display name.
    pub name: String,
    /// Manufacturer.
    #[serde(rename = "mf", skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Model.
    #[serde(rename = "mdl", skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Software version.
    #[serde(rename = "sw", skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
}

/// The discovery origin block (what software published this document).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct OriginInfo {
    /// Origin name.
    pub name: String,
    /// Origin software version.
    #[serde(rename = "sw", skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
    /// Support URL.
    #[serde(rename = "url", skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
}

/// One entity within a [`DeviceDiscovery`].
///
/// Every optional field is skipped when `None`, so a component serializes to only
/// the keys it actually sets.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Component {
    /// Home Assistant platform (`sensor`, `binary_sensor`, `button`).
    #[serde(rename = "p")]
    pub platform: String,
    /// Entity display name.
    pub name: String,
    /// Entity unique id — always [`DeviceId::unique_id`] of this component's key.
    pub unique_id: String,
    /// The topic this entity reads its state from.
    ///
    /// Set per-component rather than once at the document root, even though
    /// every state-bearing entity here shares the same topic. `button` has no
    /// state topic at all, so a root `state_topic` would apply an option its
    /// platform does not accept. It is *probably* stripped harmlessly — but
    /// "probably harmless because Home Assistant discards it" is exactly the
    /// assumption that made the root `availability_topic` bug possible, and this
    /// way the document is correct under every reading of the merge rules.
    ///
    /// Stamped automatically by [`base_discovery`] onto every non-`button`
    /// component, so it cannot be forgotten one entity at a time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_topic: Option<String>,
    /// This entity's availability, as a **list** — see [`AvailabilityEntry`].
    ///
    /// `None` on purpose for most entities: the state topic is *retained*, so an
    /// entity with no availability keeps showing its last published value while
    /// the machine is asleep, instead of going `unavailable` and hiding data the
    /// broker still holds. See [`base_discovery`] for which entities opt in and
    /// why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<Vec<AvailabilityEntry>>,
    /// Jinja template extracting this entity's value from the state payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_template: Option<String>,
    /// Home Assistant device class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_class: Option<String>,
    /// Home Assistant state class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_class: Option<String>,
    /// Unit of measurement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<String>,
    /// MDI icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Entity category (`diagnostic`, `config`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_category: Option<String>,
    /// Payload meaning "on" (binary sensors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_on: Option<String>,
    /// Payload meaning "off" (binary sensors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_off: Option<String>,
    /// Command topic (buttons).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_topic: Option<String>,
    /// Payload published on press (buttons).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_press: Option<String>,
    /// Suggested Home Assistant object id.
    ///
    /// Deliberately left `None` everywhere in this phase: populating it pins HA
    /// `entity_id`s, and choosing those belongs to the deferred HA-cutover phase.
    /// The field exists now so that phase needs no protocol change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    /// Whether the entity is enabled when first added.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_by_default: Option<bool>,
    /// Build-time intent, **never serialised**: this entity measures liveness, so
    /// a retained last-known value would be an active lie rather than stale-but-
    /// useful data. [`base_discovery`] turns it into an `availability` list.
    ///
    /// Set it via [`Component::liveness_gated`], not by hand.
    #[serde(skip)]
    pub liveness_gated: bool,
}

impl Component {
    fn new(platform: &str, name: impl Into<String>, unique_id: impl Into<String>) -> Self {
        Component {
            platform: platform.to_string(),
            name: name.into(),
            unique_id: unique_id.into(),
            state_topic: None,
            availability: None,
            value_template: None,
            device_class: None,
            state_class: None,
            unit_of_measurement: None,
            icon: None,
            entity_category: None,
            payload_on: None,
            payload_off: None,
            command_topic: None,
            payload_press: None,
            object_id: None,
            enabled_by_default: None,
            liveness_gated: false,
        }
    }

    /// A `sensor` component with every option unset.
    pub fn sensor(name: impl Into<String>, unique_id: impl Into<String>) -> Self {
        Component::new("sensor", name, unique_id)
    }

    /// A `binary_sensor` component with every option unset.
    pub fn binary_sensor(name: impl Into<String>, unique_id: impl Into<String>) -> Self {
        Component::new("binary_sensor", name, unique_id)
    }

    /// A `button` component with every option unset.
    pub fn button(name: impl Into<String>, unique_id: impl Into<String>) -> Self {
        Component::new("button", name, unique_id)
    }

    /// Set the value template.
    pub fn with_value_template(mut self, template: impl Into<String>) -> Self {
        self.value_template = Some(template.into());
        self
    }

    /// Mark this entity as measuring **liveness**, so [`base_discovery`] gates it
    /// on availability.
    ///
    /// Reach for this only when a retained last-known value would *lie*. Both
    /// current users report how fresh the state envelope is (`status.stale`,
    /// `status.age_seconds`) and are computed **by the publisher**, not by a Home
    /// Assistant template that could re-render on a clock tick. So the last
    /// message a sleeping machine ever sent says "not stale, a few seconds old",
    /// and it says that for as long as the machine stays asleep. `unavailable` is
    /// the honest rendering of "I cannot tell you how stale this is"; a frozen
    /// `stale: false` is worse than no answer.
    ///
    /// This is the exception. Ordinary state (which OS, which app, CPU load) is
    /// still true-as-of-`published_at` while the machine sleeps, so it stays
    /// ungated and keeps showing its retained value.
    pub fn liveness_gated(mut self) -> Self {
        self.liveness_gated = true;
        self
    }

    /// Override the state topic.
    ///
    /// Normally [`base_discovery`] stamps the shared state topic onto every
    /// non-`button` component, and this is not needed. Use it only for an entity
    /// that reads a *different* topic — today just the connectivity sensor, which
    /// reads the raw LWT payload off the avail topic. A preset topic suppresses
    /// the automatic stamp.
    pub fn with_state_topic(mut self, topic: impl Into<String>) -> Self {
        self.state_topic = Some(topic.into());
        self
    }

    /// Set the device class.
    pub fn with_device_class(mut self, class: impl Into<String>) -> Self {
        self.device_class = Some(class.into());
        self
    }

    /// Set the state class.
    pub fn with_state_class(mut self, class: impl Into<String>) -> Self {
        self.state_class = Some(class.into());
        self
    }

    /// Set the unit of measurement.
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit_of_measurement = Some(unit.into());
        self
    }

    /// Set the MDI icon.
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Mark the entity as diagnostic.
    pub fn diagnostic(mut self) -> Self {
        self.entity_category = Some("diagnostic".to_string());
        self
    }

    /// Set the on/off payloads (binary sensors).
    pub fn with_payloads(mut self, on: impl Into<String>, off: impl Into<String>) -> Self {
        self.payload_on = Some(on.into());
        self.payload_off = Some(off.into());
        self
    }

    /// Set the command topic and press payload (buttons).
    pub fn with_command(mut self, topic: impl Into<String>, press: impl Into<String>) -> Self {
        self.command_topic = Some(topic.into());
        self.payload_press = Some(press.into());
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Jinja template helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Render a boolean as `ON`/`OFF`.
///
/// **Never emit a bare `{{ expr }}` for a bool.** Jinja renders Python booleans as
/// `True`/`False`, which matches neither `payload_on` nor `payload_off`, so the
/// entity would sit `unknown` forever. Pair this with [`Component::with_payloads`]
/// (`"ON"` / `"OFF"`).
fn bool_template(expr: &str) -> String {
    format!("{{% if {expr} %}}ON{{% else %}}OFF{{% endif %}}")
}

/// Render a possibly-null value, falling back to Home Assistant's null sentinel.
///
/// Two independent traps here, both of which produce a *silently* wrong entity:
///
/// 1. **The fallback must be `none`, not the string `'unknown'`.** Jinja renders
///    `none` as the literal `None`, which is the sentinel Home Assistant's MQTT
///    sensor documents: *"a `'None'` value will set the sensor to an `unknown`
///    state."* Several of these sensors carry `unit_of_measurement` or
///    `state_class`, which marks them numeric — and the null case is not exotic,
///    it is every publish before the first metrics sample. `None` is the
///    documented spelling for all of them, string-valued sensors included.
/// 2. **Written explicitly rather than with `| default(x, true)`.** That filter
///    treats `0` as falsy, so a real `0` — a zero CPU percentage, CEC logical
///    address `0`, which is the TV — would be reported as null. Verified against
///    the live template engine: `{{ 0 | default('unknown', true) }}` renders
///    `unknown`, whereas the form below renders `0`.
fn nullable_template(expr: &str) -> String {
    format!("{{{{ {expr} if {expr} is not none else none }}}}")
}

/// A plain `{{ expr }}` template — only for values that are never null.
fn plain_template(expr: &str) -> String {
    format!("{{{{ {expr} }}}}")
}

/// A binary sensor wired to the ON/OFF convention in one step, so the
/// bare-bool foot-gun cannot be reintroduced component-by-component.
fn bool_sensor(name: &str, unique_id: String, expr: &str) -> Component {
    Component::binary_sensor(name, unique_id)
        .with_value_template(bool_template(expr))
        .with_payloads("ON", "OFF")
}

/// A button bound to a command topic with the shared `press` payload.
fn press_button(name: &str, unique_id: String, command_topic: String) -> Component {
    Component::button(name, unique_id).with_command(command_topic, "press")
}

/// The `published_at` diagnostic sensor, shared by both builders.
///
/// `published_at` and `seq` are **both** exposed on purpose. If the
/// `timestamp_utc` template turns out to be wrong against live Home Assistant
/// (which cannot be verified without a broker), a frozen `seq` still makes a
/// half-open-socket wedge detectable. That redundancy is deliberate for the one
/// signal that matters — do not "clean it up".
fn published_at_component(device_id: &DeviceId) -> Component {
    Component::sensor("Published At", device_id.unique_id("published_at"))
        .with_value_template("{{ (value_json.published_at | int) | timestamp_utc }}")
        .with_device_class("timestamp")
        .diagnostic()
}

/// The `seq` diagnostic sensor, shared by both builders. See
/// [`published_at_component`] for why both exist.
fn seq_component(device_id: &DeviceId) -> Component {
    Component::sensor("Sequence", device_id.unique_id("seq"))
        .with_value_template(plain_template("value_json.seq"))
        .with_state_class("measurement")
        .diagnostic()
}

/// Assemble a discovery document from a component set.
///
/// **Deliberately takes no software version.** An earlier revision stamped one
/// into `dev.sw_version` and `o.sw_version`, which was a real §4a hole: the
/// desktop's two boots install `tv-shell-host` independently, so updating
/// CachyOS but not Windows made every OS switch rewrite the *retained* discovery
/// document with different bytes — the same churn mechanism the identical
/// component set exists to prevent. It also made the
/// "identical across boots" claim false while the test still passed, because the
/// test happened to pass the same version to both calls.
///
/// The running version is not lost: it is published as a `*_version` diagnostic
/// **entity** from the state payload, which is the right home for something that
/// legitimately changes per boot. That leaves this function a pure function of
/// `device_id` alone, so identical-across-boots is structural rather than a
/// convention someone has to remember.
fn base_discovery(
    device_id: &DeviceId,
    model: &str,
    mut cmps: BTreeMap<String, Component>,
) -> DeviceDiscovery {
    // The one entity that reports the connection itself. It reads the LWT topic
    // raw ("online"/"offline"), NOT the JSON state envelope, so it is the single
    // place the avail topic is still legible now that most entities are ungated.
    //
    // It must never be `liveness_gated`: an availability-gated connectivity
    // sensor is `unavailable` exactly when it has something to say. That is the
    // same shape of bug as a `status_stale` that freezes at `false`, inverted.
    cmps.insert(
        "connected".to_string(),
        Component::binary_sensor("Connected", device_id.unique_id("connected"))
            .with_state_topic(device_id.avail_topic())
            .with_payloads(AVAIL_ONLINE, AVAIL_OFFLINE)
            .with_device_class("connectivity")
            .diagnostic(),
    );

    // Stamp the shared state topic onto every entity that has one. `button` has
    // no state topic, so it is skipped rather than inheriting an option its
    // platform does not accept (see `Component::state_topic`). A component that
    // already set its own topic (the connectivity sensor above, which reads the
    // avail topic) keeps it.
    //
    // Availability is stamped here too, on exactly two tiers — see the module
    // docs for the reasoning:
    //
    //   * `button` — a command topic on a sleeping machine cannot be actioned by
    //     that machine, so an available button is a lie about what pressing it
    //     will do. (This is the same reasoning that keeps `wake` OUT of the
    //     command surface entirely: HA's `wake_on_lan` owns that, precisely
    //     because a wake button gated on availability would be unavailable
    //     exactly when it is needed.)
    //   * `liveness_gated` — see [`Component::liveness_gated`].
    //
    // Everything else is deliberately ungated: the state topic is retained, so
    // those entities keep reporting their last known value (true as of
    // `published_at`) while the machine sleeps, instead of all going
    // `unavailable` and hiding data the broker is still holding.
    let availability = vec![AvailabilityEntry {
        topic: device_id.avail_topic(),
        payload_available: AVAIL_ONLINE.to_string(),
        payload_not_available: AVAIL_OFFLINE.to_string(),
    }];
    let cmps = cmps
        .into_iter()
        .map(|(key, mut component)| {
            if component.platform != "button" && component.state_topic.is_none() {
                component.state_topic = Some(device_id.state_topic());
            }
            if component.platform == "button" || component.liveness_gated {
                component.availability = Some(availability.clone());
            }
            (key, component)
        })
        .collect();

    DeviceDiscovery {
        dev: DeviceInfo {
            identifiers: vec![device_id.ha_device_identifier()],
            name: format!("{TOPIC_ROOT} {device_id}"),
            manufacturer: Some(TOPIC_ROOT.to_string()),
            model: Some(model.to_string()),
            sw_version: None,
        },
        o: OriginInfo {
            name: TOPIC_ROOT.to_string(),
            sw_version: None,
            support_url: Some("https://github.com/jedwards1230/tv-shell".to_string()),
        },
        cmps,
        qos: 0,
    }
}

/// Discovery document for the desktop sidecar (`status` = [`crate::StatusResponse`]).
///
/// **Takes no OS parameter, and must never read [`DeviceOs::current`] or any
/// `cfg!`.** The desktop is one physical machine that dual-boots CachyOS and
/// Windows, and this message is *retained*: if the component set differed per
/// boot, every OS switch would rewrite the retained config, adding and removing
/// Home Assistant entities each time. The guarantee is structural — the function
/// The guarantee is structural: `device_id` is this function's ONLY input, and
/// it is by definition the same on both boots (one machine, one id). It is
/// pinned by `host_discovery_is_identical_across_boots`. Entities that only
/// apply to one boot simply report `unknown`/`unavailable` on the other.
///
/// A software version is deliberately absent — see [`base_discovery`] for why
/// stamping one in was a real hole rather than a cosmetic one.
///
/// Steam library size and the current game's *name* are deliberately **not**
/// exposed: both would need fields [`crate::StatusResponse`] does not carry, and
/// `status` is contractually that type verbatim. Deferred.
pub fn host_discovery(device_id: &DeviceId) -> DeviceDiscovery {
    let mut cmps = BTreeMap::new();

    cmps.insert(
        "current_os".to_string(),
        Component::sensor("Current OS", device_id.unique_id("current_os"))
            .with_value_template(plain_template("value_json.current_os"))
            .with_icon("mdi:desktop-tower"),
    );
    cmps.insert(
        "running_appid".to_string(),
        Component::sensor("Running App ID", device_id.unique_id("running_appid"))
            .with_value_template(nullable_template("value_json.status.running_appid")),
    );
    cmps.insert(
        "streaming".to_string(),
        bool_sensor(
            "Streaming",
            device_id.unique_id("streaming"),
            "value_json.status.streaming",
        ),
    );
    cmps.insert(
        "host_version".to_string(),
        Component::sensor("Host Version", device_id.unique_id("host_version"))
            .with_value_template(plain_template("value_json.status.version"))
            .diagnostic(),
    );
    cmps.insert(
        "published_at".to_string(),
        published_at_component(device_id),
    );
    cmps.insert("seq".to_string(), seq_component(device_id));

    cmps.insert(
        "sleep".to_string(),
        press_button(
            "Sleep",
            device_id.unique_id("sleep"),
            device_id.cmd_topic("sleep"),
        ),
    );
    cmps.insert(
        "quit".to_string(),
        press_button(
            "Quit Game",
            device_id.unique_id("quit"),
            device_id.cmd_topic("quit"),
        ),
    );
    cmps.insert(
        "open_bpm".to_string(),
        press_button(
            "Open Big Picture",
            device_id.unique_id("open_bpm"),
            device_id.cmd_topic("open-bpm"),
        ),
    );

    base_discovery(device_id, "tv-shell-host", cmps)
}

/// Discovery document for the TV client daemon (`status` = [`ShellSnapshot`]).
///
/// Settings switches/numbers/selects are a later phase and are intentionally
/// absent — adding one rewrites this retained message.
pub fn shell_discovery(device_id: &DeviceId) -> DeviceDiscovery {
    let mut cmps = BTreeMap::new();

    cmps.insert(
        "shell_state".to_string(),
        Component::sensor("Shell State", device_id.unique_id("shell_state"))
            .with_value_template(nullable_template("value_json.status.shell_state")),
    );
    cmps.insert(
        "media_playing".to_string(),
        bool_sensor(
            "Media Playing",
            device_id.unique_id("media_playing"),
            "value_json.status.media_playing",
        ),
    );
    cmps.insert(
        "status_stale".to_string(),
        bool_sensor(
            "Status Stale",
            device_id.unique_id("status_stale"),
            "value_json.status.stale",
        )
        .with_device_class("problem")
        .liveness_gated(),
    );
    cmps.insert(
        "shell_running".to_string(),
        bool_sensor(
            "Shell Running",
            device_id.unique_id("shell_running"),
            "value_json.status.shell_running",
        )
        .with_device_class("running"),
    );
    cmps.insert(
        "age_seconds".to_string(),
        Component::sensor("Status Age", device_id.unique_id("age_seconds"))
            .with_value_template(nullable_template("value_json.status.age_seconds"))
            .with_unit("s")
            .diagnostic()
            .liveness_gated(),
    );
    cmps.insert(
        "cec_display_ownership".to_string(),
        Component::sensor(
            "CEC Display Ownership",
            device_id.unique_id("cec_display_ownership"),
        )
        .with_value_template(plain_template("value_json.status.cec_display_ownership")),
    );
    cmps.insert(
        "cec_display_owner".to_string(),
        Component::sensor(
            "CEC Display Owner",
            device_id.unique_id("cec_display_owner"),
        )
        .with_value_template(nullable_template("value_json.status.cec_display_owner"))
        .diagnostic(),
    );
    cmps.insert(
        "cpu_percent".to_string(),
        Component::sensor("CPU Usage", device_id.unique_id("cpu_percent"))
            .with_value_template(nullable_template("value_json.status.cpu_percent"))
            .with_unit("%")
            .with_state_class("measurement"),
    );
    cmps.insert(
        "mem_percent".to_string(),
        Component::sensor("Memory Usage", device_id.unique_id("mem_percent"))
            .with_value_template(nullable_template("value_json.status.mem_percent"))
            .with_unit("%")
            .with_state_class("measurement"),
    );
    cmps.insert(
        "uptime_seconds".to_string(),
        Component::sensor("Uptime", device_id.unique_id("uptime_seconds"))
            .with_value_template(nullable_template("value_json.status.uptime_seconds"))
            .with_unit("s")
            .diagnostic(),
    );
    cmps.insert(
        "current_os".to_string(),
        Component::sensor("Current OS", device_id.unique_id("current_os"))
            .with_value_template(plain_template("value_json.current_os"))
            .diagnostic(),
    );
    cmps.insert(
        "daemon_version".to_string(),
        Component::sensor("Daemon Version", device_id.unique_id("daemon_version"))
            .with_value_template(plain_template("value_json.status.version"))
            .diagnostic(),
    );
    cmps.insert(
        "published_at".to_string(),
        published_at_component(device_id),
    );
    cmps.insert("seq".to_string(), seq_component(device_id));

    for (key, name, cmd) in [
        ("suspend", "Suspend", "suspend"),
        ("home", "Home", "home"),
        ("menu", "Menu", "menu"),
        ("settings", "Settings", "settings"),
        ("restart_shell", "Restart Shell", "restart-shell"),
    ] {
        cmps.insert(
            key.to_string(),
            press_button(name, device_id.unique_id(key), device_id.cmd_topic(cmd)),
        );
    }

    base_discovery(device_id, "tv-shell-input", cmps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusResponse;

    fn id(raw: &str) -> DeviceId {
        DeviceId::new(raw).expect("test device id must be valid")
    }

    #[test]
    fn device_id_rejects_topic_wildcards_and_separators() {
        let too_long = "a".repeat(65);
        let cases: &[(&str, Option<DeviceIdError>)] = &[
            ("", Some(DeviceIdError::Empty)),
            ("a/b", Some(DeviceIdError::InvalidChar('/'))),
            ("a+b", Some(DeviceIdError::InvalidChar('+'))),
            ("a#b", Some(DeviceIdError::InvalidChar('#'))),
            ("a b", Some(DeviceIdError::InvalidChar(' '))),
            ("a$b", Some(DeviceIdError::InvalidChar('$'))),
            ("café", Some(DeviceIdError::InvalidChar('é'))),
            (&too_long, Some(DeviceIdError::TooLong)),
            ("htpc-1", None),
            ("desktop", None),
            ("a_b-1", None),
        ];
        for (raw, expected) in cases {
            match (DeviceId::new(raw), expected) {
                (Err(actual), Some(want)) => assert_eq!(&actual, want, "for input {raw:?}"),
                (Ok(ok), None) => assert_eq!(ok.as_str(), *raw),
                (got, want) => panic!("for input {raw:?}: got {got:?}, wanted {want:?}"),
            }
        }
    }

    #[test]
    fn device_id_builds_the_frozen_topics() {
        // These literals ARE the frozen contract — written out, never computed.
        let htpc = id("htpc-1");
        assert_eq!(htpc.ha_device_identifier(), "tv-shell-htpc-1");
        assert_eq!(htpc.unique_id("shell_state"), "tv-shell-htpc-1-shell_state");
        assert_eq!(htpc.state_topic(), "tv-shell/htpc-1/state");
        assert_eq!(htpc.avail_topic(), "tv-shell/htpc-1/avail");
        assert_eq!(htpc.cmd_topic("home"), "tv-shell/htpc-1/cmd/home");
        assert_eq!(htpc.cmd_topic_filter(), "tv-shell/htpc-1/cmd/+");
        assert_eq!(
            htpc.discovery_topic(),
            "homeassistant/device/tv-shell-htpc-1/config"
        );

        let desktop = id("desktop");
        assert_eq!(desktop.ha_device_identifier(), "tv-shell-desktop");
        assert_eq!(desktop.unique_id("streaming"), "tv-shell-desktop-streaming");
        assert_eq!(desktop.state_topic(), "tv-shell/desktop/state");
        assert_eq!(desktop.avail_topic(), "tv-shell/desktop/avail");
        assert_eq!(desktop.cmd_topic("sleep"), "tv-shell/desktop/cmd/sleep");
        assert_eq!(desktop.cmd_topic_filter(), "tv-shell/desktop/cmd/+");
        assert_eq!(
            desktop.discovery_topic(),
            "homeassistant/device/tv-shell-desktop/config"
        );
    }

    #[test]
    fn device_id_serde_goes_through_a_validated_string() {
        let parsed: DeviceId = serde_json::from_str(r#""htpc-1""#).unwrap();
        assert_eq!(parsed.as_str(), "htpc-1");
        assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""htpc-1""#);
        // A bad device_id must fail the PARSE (i.e. daemon startup), not later.
        assert!(serde_json::from_str::<DeviceId>(r#""a/b""#).is_err());
    }

    #[test]
    fn envelope_serialises_in_wire_order() {
        let env = StateEnvelope::new(
            1_785_109_000,
            4213,
            DeviceOs::Windows,
            StatusResponse {
                version: "1.2.3".to_string(),
                running_appid: Some(730),
                streaming: true,
            },
        );
        // Byte-exact: schema_version first, status last, and `status` is the
        // exact three-field StatusResponse object in its own declaration order.
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"schema_version":1,"published_at":1785109000,"seq":4213,"current_os":"windows","status":{"version":"1.2.3","running_appid":730,"streaming":true}}"#
        );
        let back: HostState = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn envelope_new_always_sets_schema_version() {
        let env = StateEnvelope::new(0, 0, DeviceOs::Linux, ShellSnapshot::default());
        assert_eq!(env.schema_version, SCHEMA_VERSION);
        assert_eq!(env.schema_version, 1);
    }

    #[test]
    fn shell_snapshot_default_ownership_is_unknown() {
        // "unknown" means "no evidence at all" — an empty string would be a
        // third, undefined state.
        assert_eq!(ShellSnapshot::default().cec_display_ownership, "unknown");
    }

    #[test]
    fn shell_snapshot_roundtrips() {
        let snap = ShellSnapshot {
            shell_state: Some("idle".to_string()),
            media_playing: true,
            stale: false,
            age_seconds: Some(2),
            stale_after_seconds: 10,
            shell_running: true,
            cec_display_ownership: "self".to_string(),
            cec_display_owner: Some(4),
            cec_local_address: Some(1),
            cec_display_owner_changed_unix: Some(1_785_109_000),
            cec_display_owner_held_seconds: Some(42),
            cec_display_owner_ever_observed: true,
            cec_display_owner_tracking: true,
            version: "0.1.0".to_string(),
            cpu_percent: Some(12.5),
            mem_percent: Some(48.0),
            uptime_seconds: Some(3600),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: ShellSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn shell_snapshot_missing_fields_use_defaults() {
        let back: ShellSnapshot = serde_json::from_str("{}").unwrap();
        assert_eq!(back, ShellSnapshot::default());
        assert_eq!(back.cec_display_ownership, "unknown");
        assert!(back.shell_state.is_none());
        assert!(!back.shell_running);
        assert!(back.cpu_percent.is_none());
    }

    #[test]
    fn host_discovery_is_identical_across_boots() {
        // The desktop is ONE dual-boot machine publishing a RETAINED discovery
        // message. If the component set could vary per boot, every OS switch
        // would rewrite that retained config and churn HA entities. This pins
        // the structural guarantee: host_discovery has no OS input at all.
        let device = id("desktop");
        let a = serde_json::to_string(&host_discovery(&device)).unwrap();
        let b = serde_json::to_string(&host_discovery(&device)).unwrap();
        assert_eq!(a, b);
        assert!(
            !a.contains("linux"),
            "discovery must not mention an OS: {a}"
        );
        assert!(
            !a.contains("windows"),
            "discovery must not mention an OS: {a}"
        );
        // `device_id` is the ONLY input, so there is nothing else a boot could
        // vary. An earlier revision also took `sw_version` and stamped it into
        // this retained document — which the previous version of this test could
        // not catch, because it passed the same version to both calls. The two
        // boots install independently, so that was a real hole: update CachyOS,
        // skip Windows, and every OS switch rewrote the retained config.
        //
        // Assert the absence directly rather than trusting the signature.
        assert!(
            !a.contains("\"sw\""),
            "no software version belongs in the retained discovery doc: {a}"
        );
    }

    #[test]
    fn discovery_component_keys_are_stable() {
        // Adding or removing an entity silently rewrites a RETAINED message —
        // any change here must be a conscious one.
        let host = host_discovery(&id("desktop"));
        let host_keys: Vec<&str> = host.cmps.keys().map(String::as_str).collect();
        assert_eq!(
            host_keys,
            [
                "connected",
                "current_os",
                "host_version",
                "open_bpm",
                "published_at",
                "quit",
                "running_appid",
                "seq",
                "sleep",
                "streaming",
            ]
        );

        let shell = shell_discovery(&id("htpc-1"));
        let shell_keys: Vec<&str> = shell.cmps.keys().map(String::as_str).collect();
        assert_eq!(
            shell_keys,
            [
                "age_seconds",
                "cec_display_owner",
                "cec_display_ownership",
                "connected",
                "cpu_percent",
                "current_os",
                "daemon_version",
                "home",
                "media_playing",
                "mem_percent",
                "menu",
                "published_at",
                "restart_shell",
                "seq",
                "settings",
                "shell_running",
                "shell_state",
                "status_stale",
                "suspend",
                "uptime_seconds",
            ]
        );
    }

    #[test]
    fn discovery_unique_ids_are_prefixed() {
        for (device, doc) in [
            (id("desktop"), host_discovery(&id("desktop"))),
            (id("htpc-1"), shell_discovery(&id("htpc-1"))),
        ] {
            let prefix = format!("{}-", device.ha_device_identifier());
            for (key, cmp) in &doc.cmps {
                assert!(
                    cmp.unique_id.starts_with(&prefix),
                    "{} is not prefixed with {prefix}",
                    cmp.unique_id
                );
                assert_eq!(cmp.unique_id, device.unique_id(key));
            }
        }
    }

    #[test]
    fn binary_sensor_templates_never_emit_bare_bools() {
        // Jinja renders Python bools as True/False, which matches neither
        // payload — so every binary sensor must use the if/else ON/OFF form.
        for (device, doc) in [
            (id("desktop"), host_discovery(&id("desktop"))),
            (id("htpc-1"), shell_discovery(&id("htpc-1"))),
        ] {
            for (key, cmp) in &doc.cmps {
                if cmp.platform != "binary_sensor" {
                    continue;
                }
                // The connectivity sensor reads the raw LWT payload off the avail
                // topic, not the JSON state envelope, so it has no template to
                // get wrong. Keyed on the topic rather than the name so a future
                // envelope-reading sensor cannot opt out by accident.
                if cmp.state_topic.as_deref() == Some(device.avail_topic().as_str()) {
                    assert!(
                        cmp.value_template.is_none(),
                        "{key} templates a raw payload"
                    );
                    continue;
                }
                let template = cmp
                    .value_template
                    .as_deref()
                    .unwrap_or_else(|| panic!("{key} has no value_template"));
                assert!(template.contains("{% if"), "{key}: bare bool in {template}");
                assert_eq!(cmp.payload_on.as_deref(), Some("ON"), "{key}");
                assert_eq!(cmp.payload_off.as_deref(), Some("OFF"), "{key}");
            }
        }
    }

    #[test]
    fn nullable_templates_do_not_use_the_default_filter() {
        // `| default('unknown', true)` treats a real 0 as falsy.
        for doc in [
            host_discovery(&id("desktop")),
            shell_discovery(&id("htpc-1")),
        ] {
            for (key, cmp) in &doc.cmps {
                if let Some(template) = &cmp.value_template {
                    assert!(!template.contains("default("), "{key}: {template}");
                }
            }
        }
    }

    /// The null fallback must be Home Assistant's documented sentinel — the
    /// literal `None` that Jinja's `none` renders to — never the string
    /// `'unknown'`.
    ///
    /// HA's MQTT sensor documents *"a `'None'` value will set the sensor to an
    /// `unknown` state."* Several of these sensors carry `unit_of_measurement`
    /// or `state_class`, which marks them numeric, and the null case is the
    /// common path rather than an edge: every publish before the first metrics
    /// sample has no CPU/memory reading at all.
    ///
    /// Pinned here because it is invisible without a live broker AND a live
    /// Home Assistant — the payload would serialise, the entity would register,
    /// and it would simply never show a value.
    #[test]
    fn nullable_templates_use_the_documented_none_sentinel() {
        for doc in [
            host_discovery(&id("desktop")),
            shell_discovery(&id("htpc-1")),
        ] {
            for (key, cmp) in &doc.cmps {
                let Some(template) = &cmp.value_template else {
                    continue;
                };
                if !template.contains("is not none") {
                    continue;
                }
                assert!(
                    template.ends_with("else none }}"),
                    "{key} must fall back to the `none` sentinel, not a string: {template}"
                );
                assert!(
                    !template.contains("'unknown'"),
                    "{key} uses the 'unknown' string instead of the None sentinel: {template}"
                );
            }
        }
    }

    #[test]
    fn discovery_serialises_platform_as_p() {
        let json = serde_json::to_string(&host_discovery(&id("desktop"))).unwrap();
        assert!(json.contains(r#""p":"sensor""#), "{json}");
        assert!(!json.contains(r#""platform":"#), "{json}");
    }

    #[test]
    fn discovery_root_carries_the_shared_topics_and_payloads() {
        let device = id("htpc-1");
        let doc = shell_discovery(&device);
        assert_eq!(doc.dev.identifiers, vec!["tv-shell-htpc-1".to_string()]);
        assert_eq!(doc.o.name, "tv-shell");
        // No software version in the RETAINED document — the two desktop boots
        // update independently, so a version here would rewrite the retained
        // config on every boot switch. It is published as a state entity instead.
        assert_eq!(doc.o.sw_version, None);
        assert_eq!(doc.dev.sw_version, None);
        // The state topic is stamped per-component, not at the root — buttons
        // have no state topic and must not inherit one.
        assert_eq!(
            doc.cmps["shell_state"].state_topic.as_deref(),
            Some("tv-shell/htpc-1/state")
        );
        assert_eq!(doc.cmps["suspend"].state_topic, None);
        // Availability is per-component, not at the root — see the module docs.
        let avail = doc.cmps["suspend"]
            .availability
            .as_ref()
            .expect("a button must be availability-gated");
        assert_eq!(avail.len(), 1);
        assert_eq!(avail[0].topic, "tv-shell/htpc-1/avail");
        assert_eq!(avail[0].payload_available, "online");
        assert_eq!(avail[0].payload_not_available, "offline");
        assert_eq!(doc.qos, 0);
    }

    /// The tiering that keeps a sleeping machine's dashboard useful.
    ///
    /// Both of these machines are suspended most of the day. Gating every entity
    /// on availability hid a retained, perfectly good last-known reading for all
    /// of that time; gating none of them would let the two liveness sensors report
    /// stale data as fresh. Each tier is pinned by name because the failure in
    /// either direction is invisible without a live broker AND a live HA.
    #[test]
    fn availability_is_gated_per_tier() {
        let doc = shell_discovery(&id("htpc-1"));

        // Commands: unavailable while asleep, because pressing them cannot work.
        for key in ["suspend", "home", "menu", "settings", "restart_shell"] {
            assert!(
                doc.cmps[key].availability.is_some(),
                "{key} is a command and must be availability-gated"
            );
        }

        // Liveness: publisher-computed freshness. A retained `stale: false` from a
        // machine that has been asleep for hours is an active lie, so these must
        // go unavailable rather than freeze.
        for key in ["status_stale", "age_seconds"] {
            assert!(
                doc.cmps[key].availability.is_some(),
                "{key} measures liveness and must be availability-gated"
            );
        }

        // Facts: still true as of `published_at`, so they keep their retained
        // value instead of vanishing.
        for key in [
            "shell_state",
            "shell_running",
            "media_playing",
            "cec_display_owner",
            "cec_display_ownership",
            "cpu_percent",
            "mem_percent",
            "uptime_seconds",
            "current_os",
            "daemon_version",
            "published_at",
            "seq",
        ] {
            assert!(
                doc.cmps[key].availability.is_none(),
                "{key} is a fact and must NOT be gated — it would hide retained state"
            );
        }

        let host = host_discovery(&id("desktop"));
        for key in ["sleep", "quit", "open_bpm"] {
            assert!(
                host.cmps[key].availability.is_some(),
                "{key} is a command and must be availability-gated"
            );
        }
        for key in ["current_os", "running_appid", "streaming", "host_version"] {
            assert!(
                host.cmps[key].availability.is_none(),
                "{key} is a fact and must NOT be gated"
            );
        }
    }

    /// The connectivity sensor must read the LWT topic and must NOT be gated on
    /// it — an availability-gated connectivity sensor is `unavailable` exactly
    /// when it has something to say.
    #[test]
    fn connected_sensor_reads_the_avail_topic_ungated() {
        for (device, doc) in [
            (id("desktop"), host_discovery(&id("desktop"))),
            (id("htpc-1"), shell_discovery(&id("htpc-1"))),
        ] {
            let cmp = &doc.cmps["connected"];
            assert_eq!(cmp.platform, "binary_sensor");
            assert_eq!(
                cmp.state_topic.as_deref(),
                Some(device.avail_topic().as_str())
            );
            assert_eq!(cmp.device_class.as_deref(), Some("connectivity"));
            // Raw LWT payloads, not the ON/OFF envelope convention.
            assert_eq!(cmp.payload_on.as_deref(), Some("online"));
            assert_eq!(cmp.payload_off.as_deref(), Some("offline"));
            assert!(cmp.value_template.is_none(), "the LWT payload is not JSON");
            assert!(
                cmp.availability.is_none(),
                "gating the connectivity sensor makes it useless"
            );
        }
    }

    /// Availability must serialise as the LIST form Home Assistant actually
    /// reads, and must no longer appear at the document root at all.
    ///
    /// `availability_topic` is an *unknown key*, and HA ignores unknown keys
    /// rather than rejecting them — so the wrong spelling would register every
    /// entity and leave them permanently "available", with the LWT firing to no
    /// visible effect. That failure is invisible without a live broker and a live
    /// Home Assistant, which is exactly why it is pinned here.
    #[test]
    fn availability_serialises_as_the_list_form_ha_reads() {
        for doc in [
            shell_discovery(&id("htpc-1")),
            host_discovery(&id("desktop")),
        ] {
            let json = serde_json::to_string(&doc).unwrap();
            assert!(json.contains(r#""availability":[{"topic":""#), "{json}");
            assert!(
                !json.contains(r#""availability_topic""#),
                "availability_topic is silently ignored by HA: {json}"
            );

            // Availability now lives per-component. At the root it would apply to
            // every entity and re-introduce the all-unavailable-while-asleep
            // behaviour this tiering exists to remove — and it would do so
            // silently, since the per-component lists would still be present and
            // correct. Checked against the parsed root keys rather than the raw
            // string, so it cannot be dodged by field ordering.
            let value = serde_json::to_value(&doc).unwrap();
            let root = value.as_object().expect("the document is an object");
            for key in ["availability", "availability_topic", "payload_available"] {
                assert!(
                    !root.contains_key(key),
                    "{key} belongs on a component, not the document root: {json}"
                );
            }
            assert_eq!(
                root.len(),
                4,
                "an unreviewed root key appeared (expected dev/o/cmps/qos): {json}"
            );
        }
    }

    #[test]
    fn object_id_is_unset_everywhere() {
        // Pinning HA entity_ids belongs to the deferred cutover phase.
        for doc in [
            host_discovery(&id("desktop")),
            shell_discovery(&id("htpc-1")),
        ] {
            for (key, cmp) in &doc.cmps {
                assert!(cmp.object_id.is_none(), "{key} pins an object_id");
            }
        }
    }

    #[test]
    fn device_os_current_is_total() {
        // Never panics on any target; CI builds this crate on Linux/macOS/Windows.
        let os = DeviceOs::current();
        assert!(matches!(
            os,
            DeviceOs::Linux | DeviceOs::Windows | DeviceOs::Macos | DeviceOs::Unknown
        ));
    }

    #[test]
    fn device_os_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&DeviceOs::Linux).unwrap(),
            r#""linux""#
        );
        assert_eq!(
            serde_json::to_string(&DeviceOs::Windows).unwrap(),
            r#""windows""#
        );
    }
}

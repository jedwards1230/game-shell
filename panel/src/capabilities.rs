//! The capability snapshot the panel resolves **once at startup**, and the
//! [`Gate`] vocabulary that both route registration (`crate::build_router`)
//! and the nav (`templates/base.html`) are driven from.
//!
//! ## Capability is declared by the node, never inferred
//!
//! `docs/MULTI_NODE_PANEL.md` §1. The node answers a `capabilities` handshake
//! naming what it can serve; the panel registers exactly the surfaces that set
//! supports and nothing else. **Gating is on the route, not just the nav** — a
//! hidden link in front of a live handler is not a gate, so a gated-off route
//! is simply never registered and answers **404 (it does not exist)** rather
//! than a 403 from a handler. That also leaks nothing about the node.
//!
//! ## Four tiers
//!
//! 1. **Recovery** ([`Gate::Recovery`]) — always registered, gated on nothing.
//!    These do not need the daemon: they are the panel's own exec tier
//!    (`systemctl`/`journalctl`/`ps`/`pacman`), its own filesystem work, or
//!    static assets. *This is the panel's reason to exist*, so a failed
//!    handshake must never take one away.
//! 2. **Node** ([`Gate::Node`]) — registered iff the handshake SUCCEEDED.
//!    Routes that need *some* node speaking the IPC line protocol but map to
//!    no single [`Feature`] (the Tools console's intent/key/apps/bt/net/power/
//!    sys commands). The honest statement is "these exist iff a node answered".
//! 3. **Capability** (every other [`Gate`]) — registered iff a specific
//!    [`Feature`] is in the declared set.
//! 4. **Danger** — `[panel].allow_dangerous`, unchanged, and **intersected**
//!    with a capability gate where a route is both (`/dev/deploy`,
//!    `/dev/build` need `allow_dangerous` AND [`Feature::DevDeploy`]).
//!
//! ## Registration is static, so a capability change needs a panel restart
//!
//! The snapshot is taken before the router is built and never re-taken. That is
//! sound because the node's set is itself static: `daemon/src/ipc.rs::features()`
//! derives it from compile-time cfgs (cargo features, `target_os`) plus startup
//! config (`[http]`/`[mcp]` binds), and health is deliberately **not** in it — a
//! wedged CEC adapter does not drop `cec`. So a capability change already
//! implies a new binary or a config edit plus a daemon restart; nothing
//! transient can flip it under a running panel.
//!
//! ## A failed handshake falls back to recovery-only, never to "everything"
//!
//! [`CapabilitySnapshot::unreachable`] is an EMPTY feature set with
//! `handshake_ok = false`, so only the recovery tier registers. Fail-closed and
//! daemon-independent are the same set here, which is the whole point: with the
//! daemon down the panel keeps precisely the surface that still works, and
//! gains nothing that would lie. Registering everything on a failed handshake
//! would defeat the gate entirely.

use std::collections::BTreeSet;
use std::time::Duration;

use tv_shell_protocol::{Capabilities, Feature};

use crate::transport::NodeTransport;

/// Per-attempt bound on the capability handshake.
///
/// Deliberately shorter than `IpcTransport`'s 3s default: this runs on the
/// startup path before the listener binds, so every millisecond here is a
/// millisecond the panel is not answering. Wrapping the call in a timeout is
/// safe in this direction (it only ever *shortens* the wait) — unlike
/// [`NodeTransport::command_timeout`], whose contract exists because some
/// callers need a bound that EXCEEDS the transport default.
const ATTEMPT_TIMEOUT: Duration = Duration::from_millis(1200);

/// How many times to ask before giving up.
const ATTEMPTS: usize = 4;

/// Pause between attempts. `ATTEMPTS * ATTEMPT_TIMEOUT + (ATTEMPTS - 1) *
/// RETRY_DELAY` ≈ 9.3s worst case — a bounded window, never an open-ended
/// await. The retry exists for the documented htpc-1 cold-boot race where the
/// panel unit starts before the daemon's socket exists.
const RETRY_DELAY: Duration = Duration::from_millis(1500);

/// Which gate a panel surface — a route-registration block or a nav item —
/// sits behind.
///
/// One value per registration block in `crate::build_router`, and one per nav
/// item, so the two cannot drift: both ask [`CapabilitySnapshot::allows`].
///
/// `Copy` on purpose. [`Feature`] is not (`Feature::Unknown` carries a
/// `String`), but no gate can name an unknown feature — the panel only gates on
/// surfaces it actually implements — so the mapping is a small closed set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gate {
    /// Always available: panel-local exec, panel-local filesystem, or static.
    Recovery,
    /// Needs a node that answered the capability handshake, but no specific
    /// feature.
    Node,
    /// [`Feature::Cec`] — HDMI-CEC control.
    Cec,
    /// [`Feature::Controllers`] — the gamepad fleet.
    Controllers,
    /// [`Feature::Widgets`] — the per-widget config subtree.
    Widgets,
    /// [`Feature::SettingsStore`] — `get-config` / `set-config`.
    SettingsStore,
    /// [`Feature::WebApps`] — the daemon-owned web-app registry.
    WebApps,
    /// [`Feature::Screenshot`] — a frame of the live session.
    Screenshot,
    /// [`Feature::DevDeploy`] — pull a ref and rebuild on-device.
    DevDeploy,
}

impl Gate {
    /// Every gate, in declaration order.
    ///
    /// Exhaustive by construction (`gates_are_exhaustive_and_idents_are_unique`
    /// fails if a variant is added without landing here), which is what lets
    /// `crate::tests`'s `main.rs` parser resolve a `Gate::<Ident>` out of a
    /// registration condition without a second, drift-prone table.
    ///
    /// Test-only, and `#[cfg(test)]` rather than `#[allow(dead_code)]` because
    /// that consumer exists NOW — there is no later milestone to keep it alive
    /// for.
    #[cfg(test)]
    pub const ALL: &'static [Gate] = &[
        Gate::Recovery,
        Gate::Node,
        Gate::Cec,
        Gate::Controllers,
        Gate::Widgets,
        Gate::SettingsStore,
        Gate::WebApps,
        Gate::Screenshot,
        Gate::DevDeploy,
    ];

    /// The Rust identifier of this variant — the literal text that appears in
    /// `main.rs` as `Gate::<ident>`.
    #[cfg(test)]
    pub fn ident(self) -> &'static str {
        match self {
            Gate::Recovery => "Recovery",
            Gate::Node => "Node",
            Gate::Cec => "Cec",
            Gate::Controllers => "Controllers",
            Gate::Widgets => "Widgets",
            Gate::SettingsStore => "SettingsStore",
            Gate::WebApps => "WebApps",
            Gate::Screenshot => "Screenshot",
            Gate::DevDeploy => "DevDeploy",
        }
    }

    /// The declared [`Feature`] this gate requires, or `None` for the two
    /// non-feature tiers ([`Gate::Recovery`], [`Gate::Node`]).
    ///
    /// **Every feature named here must be one `daemon/src/ipc.rs::features()`
    /// actually emits.** That function deliberately never emits `wallpapers`,
    /// `processes`, `system_updates`, `steam_library` or `game_launch` — the
    /// daemon serves none of them (they belong to QML, to the panel's own exec
    /// tier, or to the sidecar it merely proxies). Gating `/media/wallpaper/*`
    /// on `Feature::Wallpapers` or `/processes*` on `Feature::Processes` would
    /// therefore delete working pages from a live node. Those routes are
    /// recovery tier precisely because the panel serves them itself.
    pub fn feature(self) -> Option<Feature> {
        Some(match self {
            Gate::Recovery | Gate::Node => return None,
            Gate::Cec => Feature::Cec,
            Gate::Controllers => Feature::Controllers,
            Gate::Widgets => Feature::Widgets,
            Gate::SettingsStore => Feature::SettingsStore,
            Gate::WebApps => Feature::WebApps,
            Gate::Screenshot => Feature::Screenshot,
            Gate::DevDeploy => Feature::DevDeploy,
        })
    }
}

/// What the node said it can do, resolved once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySnapshot {
    /// Whether the node answered the handshake at all. `false` puts the panel
    /// in recovery mode: only [`Gate::Recovery`] surfaces are registered.
    pub handshake_ok: bool,
    /// The node's declared identity, for display. Empty when the handshake
    /// failed.
    pub node_id: String,
    /// Everything the node declared. Empty when the handshake failed.
    pub features: BTreeSet<Feature>,
}

impl CapabilitySnapshot {
    /// The fail-closed snapshot: no handshake, no features, recovery tier only.
    pub fn unreachable() -> Self {
        Self {
            handshake_ok: false,
            node_id: String::new(),
            features: BTreeSet::new(),
        }
    }

    /// A snapshot satisfying EVERY [`Gate`] — the maximal registered surface.
    ///
    /// Test-only; the hermetic page-render tests use it so a render assertion
    /// is never silently answering "that section wasn't rendered" when it means
    /// "that section rendered wrongly".
    #[cfg(test)]
    pub fn fully_capable() -> Self {
        Self {
            handshake_ok: true,
            node_id: "test-node".to_string(),
            features: Gate::ALL.iter().filter_map(|g| g.feature()).collect(),
        }
    }

    /// Whether the surface behind `gate` should be registered / rendered.
    ///
    /// The single predicate route registration and the nav both consume, so a
    /// nav link can never point at a route that was not registered.
    pub fn allows(&self, gate: Gate) -> bool {
        match gate {
            Gate::Recovery => true,
            Gate::Node => self.handshake_ok,
            other => other
                .feature()
                .is_some_and(|f| self.handshake_ok && self.features.contains(&f)),
        }
    }

    /// The declared set as a stable, comma-separated wire-name list — for the
    /// startup log line, so a support question is answerable from the journal.
    pub fn feature_list(&self) -> String {
        self.features
            .iter()
            .map(Feature::as_str)
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl From<Capabilities> for CapabilitySnapshot {
    fn from(caps: Capabilities) -> Self {
        Self {
            handshake_ok: true,
            node_id: caps.node_id,
            features: caps.features,
        }
    }
}

/// Ask the node what it can do, with a bounded retry window.
///
/// Retries **only** while the node is unreachable — the cold-boot race the
/// window exists for. A node that answers with an error or unparseable body has
/// answered: it is either older than the `capabilities` command or broken, and
/// asking again would not change either. Those fail fast to
/// [`CapabilitySnapshot::unreachable`], which is the fail-closed direction.
pub async fn handshake(node: &dyn NodeTransport) -> CapabilitySnapshot {
    for attempt in 1..=ATTEMPTS {
        match tokio::time::timeout(ATTEMPT_TIMEOUT, node.capabilities()).await {
            Ok(Ok(caps)) => return caps.into(),
            Ok(Err(e)) if !e.is_unreachable() => {
                tracing::warn!(
                    "capability handshake refused by the node ({e}) — \
                     serving the recovery tier only"
                );
                return CapabilitySnapshot::unreachable();
            }
            Ok(Err(e)) => tracing::warn!("capability handshake attempt {attempt}/{ATTEMPTS}: {e}"),
            Err(_) => tracing::warn!(
                "capability handshake attempt {attempt}/{ATTEMPTS}: timed out after {ATTEMPT_TIMEOUT:?}"
            ),
        }
        if attempt < ATTEMPTS {
            tokio::time::sleep(RETRY_DELAY).await;
        }
    }
    CapabilitySnapshot::unreachable()
}

/// One topnav link. Built from [`NAV`] filtered through
/// [`CapabilitySnapshot::allows`], so the nav shows exactly the pages that were
/// registered.
pub struct NavItem {
    /// Where the link points.
    pub href: &'static str,
    /// The visible label.
    pub label: &'static str,
    /// The page's `active` key, matched against [`Chrome::active`].
    pub key: &'static str,
    /// The gate the target page sits behind.
    pub gate: Gate,
}

/// Every topnav link, with the gate of the page it points at. Order is the
/// rendered order.
///
/// `gate` is hand-assigned here while a page's *real* gate is the
/// `build_router` block it sits in — two statements that could drift, and the
/// dangerous direction (a route moves to a stricter gate, the nav keeps the
/// looser one) is a rendered link to an unregistered route. Crate-visible so
/// `crate::tests::nav_items_agree_with_the_route_table_they_link_to` can pin
/// the two together.
pub const NAV: &[NavItem] = &[
    NavItem {
        href: "/",
        label: "Dashboard",
        key: "dashboard",
        gate: Gate::Recovery,
    },
    NavItem {
        href: "/processes",
        label: "Processes",
        key: "processes",
        gate: Gate::Recovery,
    },
    NavItem {
        href: "/settings",
        label: "Settings",
        key: "settings",
        gate: Gate::SettingsStore,
    },
    NavItem {
        href: "/widgets",
        label: "Widgets",
        key: "widgets",
        gate: Gate::Widgets,
    },
    NavItem {
        href: "/media",
        label: "Media",
        key: "media",
        gate: Gate::Recovery,
    },
    NavItem {
        href: "/tools",
        label: "Tools",
        key: "tools",
        gate: Gate::Node,
    },
    NavItem {
        href: "/controllers",
        label: "Controllers",
        key: "controllers",
        gate: Gate::Controllers,
    },
    NavItem {
        href: "/cec",
        label: "CEC",
        key: "cec",
        gate: Gate::Cec,
    },
    NavItem {
        href: "/dev",
        label: "Dev",
        key: "dev",
        gate: Gate::Recovery,
    },
    NavItem {
        href: "/logs",
        label: "Logs",
        key: "logs",
        gate: Gate::Recovery,
    },
];

/// The chrome every full page template carries: which nav entry is current,
/// which links exist on this node, and whether the panel is in recovery mode.
///
/// One struct rather than three template fields so adding a fourth piece of
/// chrome does not touch ten page structs again.
pub struct Chrome {
    /// The current page's [`NavItem::key`].
    pub active: &'static str,
    /// The links this node's capability set supports.
    pub nav: Vec<&'static NavItem>,
    /// The handshake failed — the panel is serving the recovery tier only and
    /// needs a restart once the daemon is back. Rendered as a banner.
    pub recovery_mode: bool,
}

impl Chrome {
    /// Build the chrome for `active` from `caps`.
    pub fn new(caps: &CapabilitySnapshot, active: &'static str) -> Self {
        Self {
            active,
            nav: NAV.iter().filter(|i| caps.allows(i.gate)).collect(),
            recovery_mode: !caps.handshake_ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::transport::{Reachability, TransportError};
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A transport whose `capabilities()` fails `fail_first` times before
    /// answering — the cold-boot race, on demand.
    struct FlakyNode {
        fail_first: usize,
        /// How the failing attempts fail.
        error: fn() -> TransportError,
        calls: AtomicUsize,
    }

    impl FlakyNode {
        fn unreachable_for(n: usize) -> Self {
            Self {
                fail_first: n,
                error: || TransportError::Unreachable,
                calls: AtomicUsize::new(0),
            }
        }

        fn refusing() -> Self {
            Self {
                fail_first: usize::MAX,
                error: || TransportError::Command("unknown command".to_string()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl NodeTransport for FlakyNode {
        async fn capabilities(&self) -> Result<Capabilities, TransportError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err((self.error)());
            }
            Ok(Capabilities {
                node_id: "htpc-1".to_string(),
                kind: tv_shell_protocol::NodeKind::Shell,
                agent_version: "0.2.2".to_string(),
                platform: tv_shell_protocol::Platform::Linux,
                features: [Feature::Cec].into_iter().collect(),
            })
        }

        async fn command(&self, _line: &str) -> Result<String, TransportError> {
            Err(TransportError::Unreachable)
        }

        async fn command_timeout(
            &self,
            _line: &str,
            _timeout: Duration,
        ) -> Result<String, TransportError> {
            Err(TransportError::Unreachable)
        }

        fn reachability(&self) -> Reachability {
            Reachability::LocalSocket(PathBuf::from("/tmp/flaky.sock"))
        }
    }

    /// A node that never answers at all — the wedged-daemon case the panel
    /// exists for.
    struct DeadNode;

    #[async_trait]
    impl NodeTransport for DeadNode {
        async fn capabilities(&self) -> Result<Capabilities, TransportError> {
            // Longer than ATTEMPT_TIMEOUT: the handshake's own bound, not the
            // transport's, has to be what ends each attempt.
            tokio::time::sleep(Duration::from_secs(60)).await;
            Err(TransportError::Unreachable)
        }

        async fn command(&self, _line: &str) -> Result<String, TransportError> {
            Err(TransportError::Unreachable)
        }

        async fn command_timeout(
            &self,
            _line: &str,
            _timeout: Duration,
        ) -> Result<String, TransportError> {
            Err(TransportError::Unreachable)
        }

        fn reachability(&self) -> Reachability {
            Reachability::LocalSocket(PathBuf::from("/tmp/dead.sock"))
        }
    }

    /// The cold-boot race: the socket isn't there yet, then it is.
    #[tokio::test(start_paused = true)]
    async fn handshake_retries_an_unreachable_node_within_its_window() {
        let node = FlakyNode::unreachable_for(2);
        let snap = handshake(&node).await;
        assert!(snap.handshake_ok, "the third attempt answered");
        assert!(snap.allows(Gate::Cec));
        assert_eq!(node.calls(), 3);
    }

    /// The window is BOUNDED — a node that never answers does not hang startup,
    /// and the fallback is the empty (recovery-only) set, never fail-open.
    #[tokio::test(start_paused = true)]
    async fn handshake_gives_up_and_falls_back_to_recovery_only() {
        let started = tokio::time::Instant::now();
        let snap = handshake(&DeadNode).await;
        assert!(!snap.handshake_ok);
        assert!(snap.features.is_empty(), "must not fail open");
        assert!(snap.allows(Gate::Recovery));
        assert!(!snap.allows(Gate::Node));

        let elapsed = started.elapsed();
        let ceiling = ATTEMPT_TIMEOUT * ATTEMPTS as u32 + RETRY_DELAY * (ATTEMPTS as u32 - 1);
        assert!(
            elapsed <= ceiling,
            "handshake ran {elapsed:?}, past its own {ceiling:?} bound"
        );
    }

    /// A node that ANSWERS with a refusal has answered: it is older than the
    /// `capabilities` command or broken, and asking again cannot change either.
    /// Retrying it would just add ~9s to every startup against such a node.
    #[tokio::test(start_paused = true)]
    async fn handshake_does_not_retry_a_node_that_refused() {
        let node = FlakyNode::refusing();
        let snap = handshake(&node).await;
        assert!(!snap.handshake_ok);
        assert_eq!(node.calls(), 1, "a refusal is an answer — do not retry it");
    }

    fn snapshot(features: &[Feature]) -> CapabilitySnapshot {
        CapabilitySnapshot {
            handshake_ok: true,
            node_id: "htpc-1".to_string(),
            features: features.iter().cloned().collect(),
        }
    }

    /// [`Gate::ALL`] really is every variant — the `main.rs` parser resolves
    /// `Gate::<Ident>` through it, so a variant missing here would be a gate
    /// the parser silently cannot attribute.
    #[test]
    fn gates_are_exhaustive_and_idents_are_unique() {
        // Exhaustiveness: every variant must be constructible from its own
        // ident via ALL. The match below is the compiler's half — adding a
        // variant fails to compile until it is handled — and the length check
        // is the half that catches a variant handled in `ident()` but never
        // listed in ALL.
        let idents: BTreeSet<&str> = Gate::ALL.iter().map(|g| g.ident()).collect();
        assert_eq!(idents.len(), Gate::ALL.len(), "duplicate Gate::ident()");
        for gate in Gate::ALL {
            let round = Gate::ALL
                .iter()
                .find(|g| g.ident() == gate.ident())
                .copied()
                .expect("every gate resolves from its own ident");
            assert_eq!(round, *gate);
        }
        assert_eq!(Gate::ALL.len(), 9, "a Gate variant was added or removed");
    }

    /// Every gate that names a feature names one the daemon can actually emit.
    /// The five the daemon deliberately never emits must never appear.
    #[test]
    fn no_gate_names_a_feature_the_daemon_never_emits() {
        // `daemon/src/ipc.rs::features()`: "Deliberately absent: wallpapers,
        // processes, system_updates ... and steam_library / game_launch".
        let never = [
            Feature::Wallpapers,
            Feature::Processes,
            Feature::SystemUpdates,
            Feature::SteamLibrary,
            Feature::GameLaunch,
        ];
        for gate in Gate::ALL {
            if let Some(f) = gate.feature() {
                assert!(
                    !never.contains(&f),
                    "Gate::{} gates on {:?}, which daemon/src/ipc.rs::features() \
                     never emits — the routes behind it would vanish from a live node",
                    gate.ident(),
                    f
                );
            }
        }
    }

    #[test]
    fn recovery_is_allowed_even_with_no_handshake() {
        let down = CapabilitySnapshot::unreachable();
        assert!(down.allows(Gate::Recovery));
        assert!(!down.allows(Gate::Node));
        for gate in Gate::ALL {
            if gate.feature().is_some() {
                assert!(
                    !down.allows(*gate),
                    "Gate::{} must fail closed",
                    gate.ident()
                );
            }
        }
    }

    /// A feature can only be honoured behind a successful handshake — an empty
    /// `handshake_ok = false` snapshot that somehow carried features must still
    /// gate everything off.
    #[test]
    fn features_without_a_handshake_ungate_nothing() {
        let lying = CapabilitySnapshot {
            handshake_ok: false,
            node_id: String::new(),
            features: [Feature::Cec, Feature::Widgets].into_iter().collect(),
        };
        assert!(!lying.allows(Gate::Cec));
        assert!(!lying.allows(Gate::Widgets));
        assert!(lying.allows(Gate::Recovery));
    }

    #[test]
    fn a_declared_feature_opens_exactly_its_own_gate() {
        let caps = snapshot(&[Feature::Cec]);
        assert!(caps.allows(Gate::Cec));
        assert!(caps.allows(Gate::Node));
        assert!(!caps.allows(Gate::Controllers));
        assert!(!caps.allows(Gate::Widgets));
    }

    /// An unknown feature a newer node reports round-trips into the set but
    /// opens no gate — the panel gates on what it implements, not on strings.
    #[test]
    fn an_unknown_feature_opens_no_gate() {
        let caps = snapshot(&[Feature::from("quantum_tunnel".to_string())]);
        for gate in Gate::ALL {
            if gate.feature().is_some() {
                assert!(!caps.allows(*gate));
            }
        }
        assert!(caps.allows(Gate::Node), "the node still answered");
        assert_eq!(caps.feature_list(), "quantum_tunnel");
    }

    #[test]
    fn feature_list_is_stable_and_wire_named() {
        let caps = snapshot(&[Feature::Widgets, Feature::Cec, Feature::WebApps]);
        assert_eq!(caps.feature_list(), "cec,widgets,web_apps");
    }

    #[test]
    fn nav_hides_every_link_whose_page_is_not_registered() {
        let down = Chrome::new(&CapabilitySnapshot::unreachable(), "dashboard");
        let hrefs: Vec<&str> = down.nav.iter().map(|i| i.href).collect();
        assert_eq!(hrefs, vec!["/", "/processes", "/media", "/dev", "/logs"]);
        assert!(down.recovery_mode);
    }

    #[test]
    fn nav_shows_every_link_the_full_set_supports() {
        let caps = snapshot(&[
            Feature::Cec,
            Feature::Controllers,
            Feature::Widgets,
            Feature::SettingsStore,
            Feature::WebApps,
            Feature::Screenshot,
            Feature::DevDeploy,
        ]);
        let chrome = Chrome::new(&caps, "cec");
        assert_eq!(chrome.nav.len(), NAV.len());
        assert!(!chrome.recovery_mode);
        assert_eq!(chrome.active, "cec");
    }

    /// Every nav item's key must be one a page actually sets, and the hrefs
    /// must be unique.
    #[test]
    fn nav_keys_and_hrefs_are_unique() {
        let keys: BTreeSet<&str> = NAV.iter().map(|i| i.key).collect();
        let hrefs: BTreeSet<&str> = NAV.iter().map(|i| i.href).collect();
        assert_eq!(keys.len(), NAV.len());
        assert_eq!(hrefs.len(), NAV.len());
    }

    #[test]
    fn a_capabilities_reply_becomes_a_successful_snapshot() {
        let caps = Capabilities {
            node_id: "htpc-1".to_string(),
            kind: tv_shell_protocol::NodeKind::Shell,
            agent_version: "0.2.2".to_string(),
            platform: tv_shell_protocol::Platform::Linux,
            features: [Feature::Cec].into_iter().collect(),
        };
        let snap: CapabilitySnapshot = caps.into();
        assert!(snap.handshake_ok);
        assert_eq!(snap.node_id, "htpc-1");
        assert!(snap.allows(Gate::Cec));
    }
}

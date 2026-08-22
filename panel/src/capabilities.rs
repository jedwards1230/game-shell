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

/// Declare the [`Gate`] enum, its [`Gate::ALL`] list, its [`Gate::ident`] and
/// its [`Gate::feature`] mapping from **one** list of variants.
///
/// This exists for exactly one reason: `ALL` must be exhaustive, and a
/// hand-written `const ALL` is not. Adding a variant forces new arms in the
/// `match`es (the compiler checks those), but nothing forces the const — and
/// **no test that iterates `ALL` can detect a variant missing from `ALL`**,
/// because the missing variant is precisely what it never visits. A witness
/// `index()` match plus an in-bounds assertion does not close it either; that
/// was tried and stayed green. Generating both from the same list is what makes
/// the exhaustiveness claim true rather than merely asserted.
macro_rules! gates {
    (
        $(#[$enum_meta:meta])*
        pub enum $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident => $feature:expr, )+
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $name {
            /// Every gate, in declaration order.
            ///
            /// Generated from the same list as the enum itself, so it cannot
            /// omit a variant. `crate::tests`'s `main.rs` parser resolves a
            /// `Gate::<Ident>` out of a registration condition through it.
            ///
            /// Test-only, and `#[cfg(test)]` rather than `#[allow(dead_code)]`
            /// because that consumer exists NOW — there is no later milestone
            /// to keep it alive for.
            #[cfg(test)]
            pub const ALL: &'static [$name] = &[ $( $name::$variant, )+ ];

            /// The Rust identifier of this variant — the literal text that
            /// appears in `main.rs` as `Gate::<ident>`.
            #[cfg(test)]
            pub fn ident(self) -> &'static str {
                match self { $( $name::$variant => stringify!($variant), )+ }
            }

            /// The declared [`Feature`] this gate requires, or `None` for the
            /// two non-feature tiers ([`Gate::Recovery`], [`Gate::Node`]).
            ///
            /// **Every feature named here must be one
            /// `daemon/src/ipc.rs::features()` actually emits.** That function
            /// deliberately never emits `wallpapers`, `processes`,
            /// `system_updates`, `steam_library` or `game_launch` — the daemon
            /// serves none of them (they belong to QML, to the panel's own exec
            /// tier, or to the sidecar it merely proxies). Gating
            /// `/media/wallpaper/*` on `Feature::Wallpapers` or `/processes*`
            /// on `Feature::Processes` would therefore delete working pages
            /// from a live node. Those routes are recovery tier precisely
            /// because the panel serves them itself.
            pub fn feature(self) -> Option<Feature> {
                match self { $( $name::$variant => $feature, )+ }
            }
        }
    };
}

gates! {
    /// Which gate a panel surface — a route-registration block or a nav item —
    /// sits behind.
    ///
    /// One value per registration block in `crate::build_router`, and one per
    /// nav item, so the two cannot drift: both ask
    /// [`CapabilitySnapshot::allows`].
    ///
    /// `Copy` on purpose. [`Feature`] is not (`Feature::Unknown` carries a
    /// `String`), but no gate can name an unknown feature — the panel only
    /// gates on surfaces it actually implements — so the mapping is a small
    /// closed set.
    pub enum Gate {
        /// Always available: panel-local exec, panel-local filesystem, or static.
        Recovery => None,
        /// Needs a node that answered the capability handshake, but no specific
        /// feature.
        Node => None,
        /// [`Feature::Cec`] — HDMI-CEC control.
        Cec => Some(Feature::Cec),
        /// [`Feature::Controllers`] — the gamepad fleet.
        Controllers => Some(Feature::Controllers),
        /// [`Feature::Widgets`] — the per-widget config subtree.
        Widgets => Some(Feature::Widgets),
        /// [`Feature::SettingsStore`] — `get-config` / `set-config`.
        SettingsStore => Some(Feature::SettingsStore),
        /// [`Feature::WebApps`] — the daemon-owned web-app registry.
        WebApps => Some(Feature::WebApps),
        /// [`Feature::Screenshot`] — a frame of the live session.
        Screenshot => Some(Feature::Screenshot),
        /// [`Feature::DevDeploy`] — pull a ref and rebuild on-device.
        DevDeploy => Some(Feature::DevDeploy),
    }
}

/// How the startup handshake ended.
///
/// The two failure modes need **different operator advice**, which a bare
/// "did it work" bool cannot carry:
///
/// - [`Handshake::Unreachable`] — the daemon was down. It will come back, and
///   restarting the panel then is the fix.
/// - [`Handshake::Refused`] — the daemon is UP and answered, it just does not
///   speak `capabilities`. The realistic trigger is a panel binary newer than
///   the on-device daemon. Telling that operator to "restart the panel once the
///   daemon is back" is advice about a daemon that is already back, and it
///   would stay wrong forever.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handshake {
    /// The node answered with a capability set.
    Ok,
    /// Nothing answered within the retry window.
    Unreachable,
    /// The node answered, but not with a capability set — carries its own
    /// message (an `error:` reply, or a parse failure).
    Refused(String),
}

impl Handshake {
    /// Whether a capability set was actually received.
    pub fn is_ok(&self) -> bool {
        matches!(self, Handshake::Ok)
    }
}

/// What the node said it can do, resolved once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySnapshot {
    /// How the handshake ended. Anything but [`Handshake::Ok`] puts the panel
    /// in recovery mode: only [`Gate::Recovery`] surfaces are registered.
    pub handshake: Handshake,
    /// The node's declared identity, for display. Empty when the handshake
    /// failed.
    pub node_id: String,
    /// Everything the node declared. Empty when the handshake failed.
    pub features: BTreeSet<Feature>,
}

impl CapabilitySnapshot {
    /// The fail-closed snapshot for a node that never answered.
    pub fn unreachable() -> Self {
        Self::failed(Handshake::Unreachable)
    }

    /// The fail-closed snapshot for a node that answered with something other
    /// than a capability set. Same empty feature set — only the operator advice
    /// differs.
    pub fn refused(why: impl Into<String>) -> Self {
        Self::failed(Handshake::Refused(why.into()))
    }

    fn failed(handshake: Handshake) -> Self {
        Self {
            handshake,
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
            handshake: Handshake::Ok,
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
            Gate::Node => self.handshake.is_ok(),
            other => other
                .feature()
                .is_some_and(|f| self.handshake.is_ok() && self.features.contains(&f)),
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
            handshake: Handshake::Ok,
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
                return CapabilitySnapshot::refused(e.to_string());
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

/// One page inside a [`NavGroup`] — a sub-nav link, and the drawer target when
/// it is the first registered page of its group.
///
/// Built from [`NAV`] filtered through [`CapabilitySnapshot::allows`], so the
/// nav shows exactly the pages that were registered.
pub struct NavPage {
    /// Where the link points.
    pub href: &'static str,
    /// The visible label.
    pub label: &'static str,
    /// The page's `active` key, matched against [`Chrome::active`].
    ///
    /// `"<group>.<page>"` for every page but Overview, whose group holds a
    /// single page and whose key is therefore the bare group key — see
    /// [`group_of`].
    pub key: &'static str,
    /// The gate the target page sits behind.
    pub gate: Gate,
}

/// One drawer entry: a subject group and the pages that belong to it.
pub struct NavGroup {
    /// The visible label.
    pub label: &'static str,
    /// The group key, matched against the prefix of a page key.
    pub key: &'static str,
    /// Declaration order is rendered order; the drawer link targets the first
    /// *registered* one.
    pub pages: &'static [NavPage],
}

/// The group a page key belongs to — everything before the first `.`, or the
/// whole key when it carries none (`"overview"`).
///
/// A bare key is not a special case to work around: Overview is a
/// single-page group, so `"overview.overview"` would only repeat itself.
fn group_of(page_key: &str) -> &str {
    match page_key.split_once('.') {
        Some((group, _)) => group,
        None => page_key,
    }
}

/// The whole two-level nav, with the gate of the page each link points at.
/// Declaration order is rendered order, at both levels.
///
/// `gate` is hand-assigned here while a page's *real* gate is the
/// `build_router` block it sits in — two statements that could drift, and the
/// dangerous direction (a route moves to a stricter gate, the nav keeps the
/// looser one) is a rendered link to an unregistered route. Crate-visible so
/// `crate::tests::nav_items_agree_with_the_route_table_they_link_to` can pin
/// the two together.
///
/// This is the phase-3 shape of `docs/PANEL_IA.md`: the groups are final,
/// System and Shell are at their final pages apart from Shell ▸ Media, and
/// Devices is one page short of its final four. Media and Remote ▸ Tools are
/// still the pre-split grab-bags and dissolve in phase 4, which also brings
/// Devices ▸ Network and Dev ▸ Console/Screenshot.
pub const NAV: &[NavGroup] = &[
    NavGroup {
        label: "Overview",
        key: "overview",
        pages: &[NavPage {
            href: "/",
            label: "Overview",
            key: "overview",
            gate: Gate::Recovery,
        }],
    },
    NavGroup {
        label: "System",
        key: "system",
        pages: &[
            NavPage {
                href: "/system/services",
                label: "Services",
                key: "system.services",
                gate: Gate::Recovery,
            },
            NavPage {
                href: "/system/processes",
                label: "Processes",
                key: "system.processes",
                gate: Gate::Recovery,
            },
            NavPage {
                href: "/system/updates",
                label: "Updates",
                key: "system.updates",
                gate: Gate::Recovery,
            },
            NavPage {
                href: "/system/logs",
                label: "Logs",
                key: "system.logs",
                gate: Gate::Recovery,
            },
        ],
    },
    NavGroup {
        label: "Shell",
        key: "shell",
        pages: &[
            NavPage {
                href: "/shell/appearance",
                label: "Appearance",
                key: "shell.appearance",
                gate: Gate::SettingsStore,
            },
            NavPage {
                href: "/shell/widgets",
                label: "Widgets",
                key: "shell.widgets",
                gate: Gate::Widgets,
            },
            NavPage {
                href: "/shell/apps",
                label: "Apps",
                key: "shell.apps",
                gate: Gate::SettingsStore,
            },
            NavPage {
                href: "/shell/advanced",
                label: "Advanced",
                key: "shell.advanced",
                gate: Gate::SettingsStore,
            },
            // Last on purpose: Media is the pre-IA grab-bag (wallpapers + web
            // apps) and phase 4 dissolves it into Appearance and Apps above,
            // at which point this entry and its page go away.
            NavPage {
                href: "/shell/media",
                label: "Media",
                key: "shell.media",
                gate: Gate::SettingsStore,
            },
        ],
    },
    NavGroup {
        label: "Devices",
        key: "devices",
        pages: &[
            NavPage {
                href: "/devices/controllers",
                label: "Controllers",
                key: "devices.controllers",
                gate: Gate::Controllers,
            },
            NavPage {
                href: "/devices/display-audio",
                label: "Display & Audio",
                key: "devices.display-audio",
                gate: Gate::SettingsStore,
            },
            NavPage {
                href: "/devices/cec",
                label: "CEC",
                key: "devices.cec",
                gate: Gate::Cec,
            },
            // Network arrives in phase 4, out of the Tools page.
        ],
    },
    NavGroup {
        label: "Remote",
        key: "remote",
        pages: &[NavPage {
            href: "/remote/tools",
            label: "Tools",
            key: "remote.tools",
            gate: Gate::Node,
        }],
    },
    NavGroup {
        label: "Dev",
        key: "dev",
        pages: &[NavPage {
            href: "/dev/recovery",
            label: "Recovery",
            key: "dev.recovery",
            gate: Gate::Recovery,
        }],
    },
];

/// One rendered drawer entry — a [`NavGroup`] that has at least one registered
/// page, pointing at that page.
pub struct GroupLink {
    /// The group's visible label.
    pub label: &'static str,
    /// The group key. Rendered as `data-group` on the drawer link, so the
    /// stylesheet (and a test) can address one group without matching on its
    /// human-facing label.
    pub key: &'static str,
    /// The group's **first registered** page — not a fixed default, so a group
    /// whose usual landing page is gated off still lands somewhere real.
    pub href: &'static str,
    /// Whether the active page belongs to this group.
    pub active: bool,
}

/// The chrome every full page template carries: which page is current, which
/// drawer groups and sub-nav pages exist on this node, and whether the panel is
/// in recovery mode.
///
/// One struct rather than five template fields so adding a sixth piece of
/// chrome does not touch ten page structs again.
pub struct Chrome {
    /// The current page's [`NavPage::key`].
    pub active: &'static str,
    /// The drawer: the groups this node's capability set supports. A group with
    /// no registered page is absent entirely — never an empty shell.
    pub groups: Vec<GroupLink>,
    /// The sub-nav: the registered pages of the ACTIVE group. **Empty when
    /// fewer than two**, because a one-tab tab bar is noise (this is what gives
    /// Overview no sub-nav, per `docs/PANEL_IA.md`).
    pub subnav: Vec<&'static NavPage>,
    /// The handshake failed — the panel is serving the recovery tier only.
    /// Rendered as a banner.
    pub recovery_mode: bool,
    /// The node answered but does not speak `capabilities`, and this is what it
    /// said. Empty unless the handshake was [`Handshake::Refused`].
    ///
    /// Separate from `recovery_mode` because the two need opposite advice:
    /// "wait for the daemon and restart the panel" is actively wrong when the
    /// daemon is already up and simply too old.
    pub refused_reason: String,
}

impl Chrome {
    /// Build the chrome for `active` from `caps`.
    pub fn new(caps: &CapabilitySnapshot, active: &'static str) -> Self {
        let active_group = group_of(active);
        let registered =
            |g: &'static NavGroup| g.pages.iter().filter(|p| caps.allows(p.gate)).collect();

        let groups = NAV
            .iter()
            .filter_map(|g| {
                let pages: Vec<&NavPage> = registered(g);
                pages.first().map(|first| GroupLink {
                    label: g.label,
                    key: g.key,
                    href: first.href,
                    active: g.key == active_group,
                })
            })
            .collect();

        let subnav: Vec<&'static NavPage> = NAV
            .iter()
            .find(|g| g.key == active_group)
            .map(registered)
            .unwrap_or_default();

        Self {
            active,
            groups,
            subnav: if subnav.len() < 2 { Vec::new() } else { subnav },
            recovery_mode: !caps.handshake.is_ok(),
            refused_reason: match &caps.handshake {
                Handshake::Refused(why) => why.clone(),
                _ => String::new(),
            },
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
        assert!(snap.handshake.is_ok(), "the third attempt answered");
        assert!(snap.allows(Gate::Cec));
        assert_eq!(node.calls(), 3);
    }

    /// The window is BOUNDED — a node that never answers does not hang startup,
    /// and the fallback is the empty (recovery-only) set, never fail-open.
    #[tokio::test(start_paused = true)]
    async fn handshake_gives_up_and_falls_back_to_recovery_only() {
        let started = tokio::time::Instant::now();
        let snap = handshake(&DeadNode).await;
        assert_eq!(snap.handshake, Handshake::Unreachable);
        assert!(snap.features.is_empty(), "must not fail open");
        assert!(snap.allows(Gate::Recovery));
        assert!(!snap.allows(Gate::Node));

        let elapsed = started.elapsed();
        let ceiling = ATTEMPT_TIMEOUT * ATTEMPTS as u32 + RETRY_DELAY * (ATTEMPTS as u32 - 1);

        // ABSOLUTE pin first. `ceiling` is derived from the very constants this
        // test guards, so on its own it is satisfied by ANY window — raising
        // ATTEMPTS to 100 would keep it green while startup grew to ~4 minutes
        // and `docs/PANEL.md`'s documented "~9.3s worst case" quietly became a
        // lie. This is the startup path: the panel is not answering until it
        // returns.
        assert!(
            ceiling <= Duration::from_secs(10),
            "the capability handshake's worst-case window grew to {ceiling:?}, past \
             the 10s bound documented in docs/PANEL.md — the panel does not serve \
             until this returns, so widening it is a deployment decision, not a \
             constant tweak"
        );
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
        assert!(!snap.handshake.is_ok());
        assert_eq!(node.calls(), 1, "a refusal is an answer — do not retry it");

        // The refusal must survive as a REASON, not collapse into
        // "unreachable" — the banner's advice depends on telling them apart.
        match &snap.handshake {
            Handshake::Refused(why) => assert!(
                why.contains("unknown command"),
                "the node's own message must reach the operator: {why}"
            ),
            other => panic!("a refusal must not be reported as {other:?}"),
        }
        let chrome = Chrome::new(&snap, "overview");
        assert!(chrome.recovery_mode);
        assert!(
            !chrome.refused_reason.is_empty(),
            "the refusal banner has nothing to show"
        );
    }

    fn snapshot(features: &[Feature]) -> CapabilitySnapshot {
        CapabilitySnapshot {
            handshake: Handshake::Ok,
            node_id: "htpc-1".to_string(),
            features: features.iter().cloned().collect(),
        }
    }

    /// [`Gate::ALL`] is generated from the enum's own variant list by the
    /// `gates!` macro, so **exhaustiveness is structural, not asserted** —
    /// there is no way to add a variant and leave `ALL` behind.
    ///
    /// What is still worth pinning is that `ident()` is injective: the
    /// `main.rs` parser resolves a registration condition by matching the
    /// ident text, so two gates sharing one would make it silently attribute
    /// routes to the wrong tier. (`stringify!` makes a collision impossible
    /// while the idents come from variant names, but that is the macro's
    /// doing, and this test is what notices if the macro stops doing it.)
    #[test]
    fn gate_idents_are_unique_and_match_their_variant_names() {
        let idents: BTreeSet<&str> = Gate::ALL.iter().map(|g| g.ident()).collect();
        assert_eq!(idents.len(), Gate::ALL.len(), "duplicate Gate::ident()");
        assert!(Gate::ALL.contains(&Gate::Recovery));
        assert_eq!(Gate::Recovery.ident(), "Recovery");
        assert_eq!(Gate::SettingsStore.ident(), "SettingsStore");
        assert_eq!(Gate::ALL.first(), Some(&Gate::Recovery));
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
            handshake: Handshake::Unreachable,
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

    /// Every page declared anywhere in [`NAV`], flattened.
    fn all_pages() -> Vec<&'static NavPage> {
        NAV.iter().flat_map(|g| g.pages.iter()).collect()
    }

    /// The drawer labels a chrome renders, in order.
    fn drawer(chrome: &Chrome) -> Vec<&'static str> {
        chrome.groups.iter().map(|g| g.label).collect()
    }

    /// **Recovery mode collapses the drawer to Overview + System + Dev.**
    ///
    /// `docs/PANEL_IA.md` says "System and Dev"; Overview is deliberately kept
    /// (`docs/PANEL.md` records the correction). `/` is the landing page and
    /// its tiles already have a daemon-down branch that reads unit state from
    /// systemd, so deleting it would leave `/` 404ing or force a conditional
    /// root redirect. Shell, Devices and Remote must all vanish — no empty
    /// group shells.
    #[test]
    fn recovery_mode_drawer_is_exactly_overview_system_and_dev() {
        let down = Chrome::new(&CapabilitySnapshot::unreachable(), "overview");
        assert_eq!(drawer(&down), vec!["Overview", "System", "Dev"]);
        assert_eq!(
            down.groups.iter().map(|g| g.href).collect::<Vec<_>>(),
            vec!["/", "/system/services", "/dev/recovery"]
        );
        assert!(down.recovery_mode);
        assert!(
            down.refused_reason.is_empty(),
            "an unreachable node did not refuse anything"
        );

        // A refusal gates identically but carries the reason that changes the
        // advice.
        let refused = Chrome::new(&CapabilitySnapshot::refused("unknown command"), "overview");
        assert_eq!(drawer(&refused), drawer(&down));
        assert_eq!(refused.refused_reason, "unknown command");
    }

    #[test]
    fn drawer_shows_every_group_the_full_set_supports() {
        let caps = snapshot(&[
            Feature::Cec,
            Feature::Controllers,
            Feature::Widgets,
            Feature::SettingsStore,
            Feature::WebApps,
            Feature::Screenshot,
            Feature::DevDeploy,
        ]);
        let chrome = Chrome::new(&caps, "devices.cec");
        assert_eq!(chrome.groups.len(), NAV.len());
        assert!(!chrome.recovery_mode);
        assert_eq!(chrome.active, "devices.cec");
        assert!(
            chrome.groups.iter().any(|g| g.key == "devices" && g.active),
            "the active page's group must be marked active: {:?}",
            drawer(&chrome)
        );
        assert_eq!(
            chrome.groups.iter().filter(|g| g.active).count(),
            1,
            "exactly one group is active"
        );
    }

    /// **No group ever renders as an empty shell** — under any capability set,
    /// every drawer entry has at least one registered page and points at it.
    #[test]
    fn no_group_renders_without_a_registered_page() {
        let sets: [CapabilitySnapshot; 5] = [
            CapabilitySnapshot::unreachable(),
            snapshot(&[]),
            snapshot(&[Feature::Cec]),
            snapshot(&[Feature::SettingsStore, Feature::Widgets]),
            CapabilitySnapshot::fully_capable(),
        ];
        for caps in sets {
            let chrome = Chrome::new(&caps, "overview");
            for link in &chrome.groups {
                let group = NAV
                    .iter()
                    .find(|g| g.key == link.key)
                    .expect("a drawer entry names a group that exists");
                let registered: Vec<&NavPage> =
                    group.pages.iter().filter(|p| caps.allows(p.gate)).collect();
                assert!(
                    !registered.is_empty(),
                    "group {} rendered with no registered page",
                    link.key
                );
                assert_eq!(
                    link.href, registered[0].href,
                    "group {}'s drawer link must target its FIRST registered page",
                    link.key
                );
            }
        }
    }

    /// A group's drawer href follows registration, not declaration: Shell
    /// declares Settings first, but with `settings_store` off and `widgets` on
    /// the only registered Shell page is Widgets, so that is where the drawer
    /// must land.
    #[test]
    fn a_groups_drawer_link_skips_its_gated_off_first_page() {
        let caps = snapshot(&[Feature::Widgets]);
        let chrome = Chrome::new(&caps, "shell.widgets");
        let shell = chrome
            .groups
            .iter()
            .find(|g| g.key == "shell")
            .expect("Shell still has one registered page");
        assert_eq!(shell.href, "/shell/widgets");
        assert!(
            !chrome.groups.iter().any(|g| g.key == "devices"),
            "Devices has no registered page here and must not render"
        );
    }

    /// Fewer than two registered pages ⇒ no sub-nav bar at all. Overview is
    /// the permanent case (one page by design); Shell is the conditional one.
    #[test]
    fn a_group_with_one_registered_page_renders_no_subnav() {
        let full = Chrome::new(&CapabilitySnapshot::fully_capable(), "overview");
        assert!(
            full.subnav.is_empty(),
            "Overview is a single-page group — a one-tab bar is noise"
        );

        let shell_one = Chrome::new(&snapshot(&[Feature::Widgets]), "shell.widgets");
        assert!(shell_one.subnav.is_empty(), "only Widgets is registered");

        let shell_all = Chrome::new(&CapabilitySnapshot::fully_capable(), "shell.widgets");
        assert_eq!(
            shell_all.subnav.iter().map(|p| p.href).collect::<Vec<_>>(),
            vec![
                "/shell/appearance",
                "/shell/widgets",
                "/shell/apps",
                "/shell/advanced",
                "/shell/media",
            ],
            "with every gate open the whole group is its own sub-nav"
        );
    }

    /// Keys and hrefs are unique across the whole nav, and every page key
    /// names the group it is declared in.
    #[test]
    fn nav_keys_and_hrefs_are_unique_and_group_scoped() {
        let pages = all_pages();
        let keys: BTreeSet<&str> = pages.iter().map(|p| p.key).collect();
        let hrefs: BTreeSet<&str> = pages.iter().map(|p| p.href).collect();
        assert_eq!(keys.len(), pages.len(), "duplicate NavPage::key");
        assert_eq!(hrefs.len(), pages.len(), "duplicate NavPage::href");

        let group_keys: BTreeSet<&str> = NAV.iter().map(|g| g.key).collect();
        assert_eq!(group_keys.len(), NAV.len(), "duplicate NavGroup::key");

        for group in NAV {
            for page in group.pages {
                assert_eq!(
                    group_of(page.key),
                    group.key,
                    "page {} is declared in group {} but its key names another",
                    page.href,
                    group.key
                );
            }
        }
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
        assert!(snap.handshake.is_ok());
        assert_eq!(snap.node_id, "htpc-1");
        assert!(snap.allows(Gate::Cec));
    }
}

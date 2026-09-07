//! `[input]` in `~/.config/tv-shell/core.toml`.
//!
//! # `enabled` defaults to FALSE, and that is the whole point
//!
//! The HTPC this ships to is a daily driver that streams games nightly, and the
//! pad reaching Moonlight is the single thing that must not break. So the input
//! layer is **off unless an operator turns it on**, and with it off the core
//! does not enumerate, open, grab or create anything: no session is constructed,
//! so there is no code path from a disabled core to `/dev/input` or
//! `/dev/uinput` at all (see [`crate::input::start`]). Merging and deploying
//! this is a no-op until someone edits the file.
//!
//! Turning it back off is equally total: `enabled = false` plus a core restart
//! releases every `EVIOCGRAB` and removes every presenter, because both are tied
//! to file descriptors the exiting process closes.

use serde::Deserialize;

use super::discovery::Pin;
use super::identity::{bundled_db, ControllerDb};

/// The largest `players` the config accepts.
///
/// Eight is XInput's ceiling and well past any couch. The bound exists because
/// `players` is the presenter count, and each presenter is a real uinput device
/// held open for the life of the session — a typo of `400` would create four
/// hundred of them at startup.
pub const MAX_PLAYERS: u8 = 8;

/// Bounds on the discovery poll.
const MIN_POLL_MS: u64 = 100;
const MAX_POLL_MS: u64 = 60_000;

/// The `[input]` table.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    /// **The safety flag.** Off by default; see the module docs.
    pub enabled: bool,

    /// How many player slots — and therefore how many permanent uinput
    /// presenters — the session creates at startup.
    ///
    /// This is a *capacity*, not a count of connected pads: the presenters exist
    /// whether or not anyone is holding a controller, because a presenter that
    /// appeared when a pad did would be a hotplug event to every game (§7).
    pub players: u8,

    /// How often the fleet is re-enumerated, in milliseconds.
    ///
    /// Polled rather than driven by a udev/netlink listener, per V2_DESIGN §10:
    /// v1's residual defect was an attached listener that processed nothing, and
    /// a poll that stops has no equivalent quiet failure mode.
    pub poll_interval_ms: u64,

    /// An optional `gamecontrollerdb.txt`, layered **over** the bundled
    /// baseline. Empty means the baseline alone.
    pub controller_db: String,

    /// Operator pin: claim only this `(vendor, product)`, bypassing the
    /// database. Both must be set together or neither.
    pub pin_vendor: Option<u16>,
    pub pin_product: Option<u16>,
}

impl Default for InputConfig {
    fn default() -> InputConfig {
        InputConfig {
            // DEFAULT OFF. Changing this line changes what a deploy does on a
            // box nobody reconfigured.
            enabled: false,
            players: 4,
            // v1's discovery poll interval, which has run on this hardware for
            // months: fast enough that plugging a pad in feels immediate, slow
            // enough that `evdev::enumerate` is not a background cost.
            poll_interval_ms: 2_000,
            controller_db: String::new(),
            pin_vendor: None,
            pin_product: None,
        }
    }
}

impl InputConfig {
    /// Reject nonsense before anything acts on it.
    pub fn validate(&self) -> anyhow::Result<()> {
        // Deliberately validated even when disabled: an operator who typed a bad
        // value should learn it now, not the first time they flip `enabled`.
        if self.players == 0 {
            anyhow::bail!(
                "config: [input] players must be at least 1; with zero presenters no pad \
                 could be presented and every claim would be released again immediately"
            );
        }
        if self.players > MAX_PLAYERS {
            anyhow::bail!(
                "config: [input] players must be at most {MAX_PLAYERS} (got {}); each player \
                 is a uinput device held open for the whole session",
                self.players
            );
        }
        if self.poll_interval_ms < MIN_POLL_MS || self.poll_interval_ms > MAX_POLL_MS {
            anyhow::bail!(
                "config: [input] poll_interval_ms must be between {MIN_POLL_MS} and \
                 {MAX_POLL_MS} (got {})",
                self.poll_interval_ms
            );
        }
        // A half-pin is the dangerous shape: "vendor only" would read as "claim
        // everything from this vendor", which is not what either key says.
        match (self.pin_vendor, self.pin_product) {
            (Some(_), Some(_)) | (None, None) => {}
            _ => anyhow::bail!(
                "config: [input] pin_vendor and pin_product must be set together or not at \
                 all; a half-pin has no meaning and would silently widen or narrow discovery"
            ),
        }
        Ok(())
    }

    /// The pin, if a complete one is configured.
    pub fn pin(&self) -> Pin {
        match (self.pin_vendor, self.pin_product) {
            (Some(v), Some(p)) => Some((v, p)),
            _ => None,
        }
    }

    /// Load the controller database and settle the derived values.
    ///
    /// Reads `controller_db` if one is named. **A named file that cannot be read
    /// is an error**, not a fallback to the baseline: an operator who pointed at
    /// a database and silently got the bundled one would conclude their
    /// controller is unsupported.
    pub fn resolve(&self) -> anyhow::Result<ResolvedInput> {
        let mut db = bundled_db();
        if !self.controller_db.is_empty() {
            let text = std::fs::read_to_string(&self.controller_db).map_err(|e| {
                anyhow::anyhow!("config: [input] controller_db {}: {e}", self.controller_db)
            })?;
            let extra = ControllerDb::parse(&text);
            if extra.is_empty() {
                anyhow::bail!(
                    "config: [input] controller_db {} parsed to zero entries; it is not in \
                     SDL_GameControllerDB format",
                    self.controller_db
                );
            }
            tracing::info!(
                entries = extra.len(),
                path = %self.controller_db,
                "loaded an operator controller database over the bundled baseline"
            );
            db.merge(&extra);
        }
        Ok(ResolvedInput {
            players: self.players,
            db,
            pin: self.pin(),
            poll_interval: std::time::Duration::from_millis(self.poll_interval_ms),
        })
    }
}

/// `[input]` with its file-backed values loaded.
#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub players: u8,
    pub db: ControllerDb,
    pub pin: Pin,
    pub poll_interval: std::time::Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A default config with some fields changed.
    ///
    /// Via a closure rather than `let mut c = default(); c.field = ...` so the
    /// tests below read as "the default, except X" without tripping clippy's
    /// `field_reassign_with_default`.
    fn cfg(mutate: impl FnOnce(&mut InputConfig)) -> InputConfig {
        let mut c = InputConfig::default();
        mutate(&mut c);
        c
    }

    /// **Rule: input is OFF by default.**
    ///
    /// The safety property this whole PR rests on. A core built from a config
    /// file that never mentions `[input]` must touch no device.
    #[test]
    fn input_is_disabled_by_default() {
        assert!(!InputConfig::default().enabled);
        let parsed: InputConfig = toml::from_str("").unwrap();
        assert!(
            !parsed.enabled,
            "an absent [input] table must not enable it"
        );
    }

    #[test]
    fn the_defaults_are_sane_and_valid() {
        let c = InputConfig::default();
        assert_eq!(c.players, 4);
        assert_eq!(c.poll_interval_ms, 2_000);
        assert_eq!(c.pin(), None);
        c.validate().unwrap();
    }

    #[test]
    fn a_full_table_parses() {
        let c: InputConfig = toml::from_str(
            r#"
            enabled = true
            players = 2
            poll_interval_ms = 500
            controller_db = "/etc/tv-shell/gamecontrollerdb.txt"
            pin_vendor = 0x045e
            pin_product = 0x028e
            "#,
        )
        .unwrap();
        assert!(c.enabled);
        assert_eq!(c.players, 2);
        assert_eq!(c.pin(), Some((0x045e, 0x028e)));
        c.validate().unwrap();
    }

    /// A typo must fail at startup rather than silently running a default —
    /// the same `deny_unknown_fields` contract every other core section has.
    #[test]
    fn an_unknown_key_is_rejected() {
        let err = toml::from_str::<InputConfig>("enbaled = true").unwrap_err();
        assert!(err.to_string().contains("enbaled"), "{err}");
    }

    #[test]
    fn players_must_be_within_bounds() {
        assert!(cfg(|c| c.players = 0).validate().is_err());
        assert!(cfg(|c| c.players = MAX_PLAYERS + 1).validate().is_err());
        cfg(|c| c.players = MAX_PLAYERS).validate().unwrap();
    }

    #[test]
    fn the_poll_interval_must_be_within_bounds() {
        assert!(cfg(|c| c.poll_interval_ms = 0).validate().is_err());
        assert!(cfg(|c| c.poll_interval_ms = MAX_POLL_MS + 1)
            .validate()
            .is_err());
        cfg(|c| c.poll_interval_ms = MIN_POLL_MS)
            .validate()
            .unwrap();
    }

    /// **Rule: a half-pin is rejected.**
    ///
    /// "vendor only" reads as "claim everything from this vendor", which is not
    /// what the key says, and would silently widen discovery.
    #[test]
    fn a_half_pin_is_rejected() {
        let vendor_only = cfg(|c| c.pin_vendor = Some(0x045e));
        assert!(vendor_only.validate().is_err(), "vendor without product");
        assert_eq!(vendor_only.pin(), None, "and it yields no pin");

        let product_only = cfg(|c| c.pin_product = Some(0x028e));
        assert!(product_only.validate().is_err(), "product without vendor");
        assert_eq!(product_only.pin(), None);

        let both = cfg(|c| {
            c.pin_vendor = Some(0x045e);
            c.pin_product = Some(0x028e);
        });
        both.validate().unwrap();
        assert_eq!(both.pin(), Some((0x045e, 0x028e)));
    }

    /// A config is validated even when disabled, so an operator learns about a
    /// bad value before they flip the flag rather than after.
    #[test]
    fn a_disabled_config_is_still_validated() {
        let c = cfg(|c| {
            c.enabled = false;
            c.players = 0;
        });
        assert!(c.validate().is_err());
    }

    /// **Every `[input]` key is classified, and every classified name parses.**
    ///
    /// `CoreConfig`'s own schema test defers `[input]` to this one, because its
    /// numeric field count cannot represent a `String` or an `Option`. The
    /// exhaustive destructure is the mechanism: **adding a field to
    /// [`InputConfig`] stops this test compiling** until the new key is listed,
    /// so the list cannot drift from the schema silently.
    #[test]
    fn every_input_key_is_classified() {
        let InputConfig {
            enabled,
            players,
            poll_interval_ms,
            controller_db,
            pin_vendor,
            pin_product,
        } = InputConfig::default();
        // Bind each one so an unused-variable warning cannot hide a field that
        // was destructured and then forgotten.
        let _ = (
            enabled,
            players,
            poll_interval_ms,
            &controller_db,
            pin_vendor,
            pin_product,
        );

        let keys = [
            ("enabled", "true"),
            ("players", "2"),
            ("poll_interval_ms", "500"),
            ("controller_db", "\"/etc/tv-shell/db.txt\""),
            ("pin_vendor", "1"),
            ("pin_product", "2"),
        ];
        assert_eq!(keys.len(), 6, "one entry per field destructured above");
        for (name, value) in keys {
            // Through the FULL core config, so this also pins that `[input]`
            // really is reachable at that table name and not only in isolation.
            let text = format!("[input]\n{name} = {value}\n");
            crate::config::CoreConfig::parse(&text)
                .unwrap_or_else(|e| panic!("[input] {name} is not a real config key: {e}"));
        }
    }

    /// `[input]` reaches the core config under that exact table name, and its
    /// default there is off.
    #[test]
    fn the_input_table_is_off_by_default_in_the_full_core_config() {
        let c = crate::config::CoreConfig::parse("").unwrap();
        assert!(!c.input.enabled);
        c.validate().expect("the default core config is valid");

        let c = crate::config::CoreConfig::parse("[input]\nenabled = true\n").unwrap();
        assert!(c.input.enabled);

        // And a bad [input] value fails the WHOLE core config's validation, so
        // it is caught at startup rather than when the layer first runs.
        let c = crate::config::CoreConfig::parse("[input]\nplayers = 0\n").unwrap();
        assert!(c.validate().is_err());
    }

    #[test]
    fn resolve_with_no_operator_db_yields_the_baseline() {
        let r = InputConfig::default().resolve().unwrap();
        assert!(r.db.is_known(0x045e, 0x028e));
        assert_eq!(r.players, 4);
        assert_eq!(r.poll_interval, std::time::Duration::from_millis(2_000));
    }

    /// **Rule: a named database that cannot be read is an ERROR.**
    ///
    /// Falling back to the baseline would leave an operator concluding their
    /// controller is unsupported, when in fact their path was wrong.
    #[test]
    fn a_missing_operator_db_is_an_error_not_a_silent_fallback() {
        let c = cfg(|c| c.controller_db = "/nonexistent/gamecontrollerdb.txt".into());
        let err = c.resolve().unwrap_err().to_string();
        assert!(err.contains("controller_db"), "{err}");
        assert!(err.contains("/nonexistent/"), "{err}");
        // It must name the I/O failure specifically, and NOT be the
        // parsed-to-nothing error. A `read_to_string(...).unwrap_or_default()`
        // still errors — an empty string parses to zero entries — and that
        // error also mentions `controller_db` and the path, so the two
        // assertions above alone cannot tell them apart. They are different
        // faults with different fixes ("your path is wrong" versus "your file
        // is not a controller database"), and only the message distinguishes.
        assert!(
            !err.contains("zero entries"),
            "an unreadable file must not be reported as an unparseable one: {err}"
        );
        assert!(
            err.contains("No such file") || err.contains("os error 2"),
            "the underlying I/O error must survive into the message: {err}"
        );
    }

    /// A file that reads but is not a controller database is equally a
    /// misconfiguration, and equally must not pass as the baseline.
    #[test]
    fn an_operator_db_that_parses_to_nothing_is_an_error() {
        let path = std::env::temp_dir().join(format!("tv-core-db-{}.txt", std::process::id()));
        std::fs::write(&path, "this is not a controller database\n").unwrap();
        let c = cfg(|c| c.controller_db = path.to_string_lossy().to_string());
        let err = c.resolve().unwrap_err().to_string();
        assert!(err.contains("zero entries"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_operator_db_is_layered_over_the_baseline_not_instead_of_it() {
        let path = std::env::temp_dir().join(format!("tv-core-db2-{}.txt", std::process::id()));
        // An 8BitDo pad the bundled baseline may not carry.
        std::fs::write(&path, "03000000381000003014000075010000,Custom Pad,a:b1,\n").unwrap();
        let c = cfg(|c| c.controller_db = path.to_string_lossy().to_string());
        let r = c.resolve().unwrap();
        assert!(
            r.db.is_known(0x1038, 0x1430),
            "the operator entry is present"
        );
        assert!(
            r.db.is_known(0x045e, 0x028e),
            "and the baseline is still there"
        );
        let _ = std::fs::remove_file(&path);
    }
}

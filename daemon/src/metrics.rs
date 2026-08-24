//! Observability metrics: app-specific counters + a Prometheus/OpenMetrics text
//! renderer shared by the `/metrics` HTTP route and the node_exporter
//! textfile-collector writer.
//!
//! **Design goals** (see `docs/OBSERVABILITY.md`):
//! - Emit Linux-native, standard, self-describing formats so ANY consumer can
//!   collect it their way. Collection/forwarding stays out of this repo.
//! - The genuinely valuable signal is the **app-specific counters**
//!   (`tv_shell_*_total`) that node_exporter cannot give: input events,
//!   intents, shell↔game transitions, pad joins/leaves, shell restarts.
//! - Resource gauges (cpu/mem/load/temps) are a convenience reusing the existing
//!   `system::SysMetrics` reader; they are better sourced from node_exporter if
//!   one is present on the host.
//!
//! **Shared render**: [`render`] produces the full exposition text used by both
//! the HTTP endpoint and the textfile writer, so the two never drift.
//!
//! **Cross-platform**: no Linux-only imports — the struct and renderer compile
//! and unit-test on macOS/CI. The sys gauges degrade to zero/empty there (see
//! `system::sys_metrics_json` / `SysMetrics`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared, cheap-to-update counters for daemon activity. Held behind an `Arc`
/// and cloned into every subsystem that records an event. All increments are
/// `Relaxed` — these are independent monotonic counters with no inter-counter
/// ordering requirement, and the reader (textfile/HTTP) only needs eventual
/// consistency.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Raw evdev input events read from the gamepad fleet and processed by the
    /// input runtime (`handle_event`). The hot path; `Relaxed` add is a single
    /// atomic instruction.
    pub input_events: AtomicU64,
    /// `intent:<name>` broadcasts accepted and emitted on the event bus
    /// (IPC `intent`, HTTP `/intent/*`, MCP `send_intent`, and the gamepad
    /// Home-tap/Home-hold all funnel through `Shared::publish(Event::Intent)`).
    pub intents_emitted: AtomicU64,
    /// Shell↔game presenter transitions (`grab`/`release`/`handoff`). Each
    /// presenter switch increments this once.
    pub transitions: AtomicU64,
    /// Pads that joined the fleet (hot-join or initial enumeration).
    pub pad_joins: AtomicU64,
    /// Pads that left the fleet (USB/Bluetooth disconnect).
    pub pad_leaves: AtomicU64,
    /// Daemon starts. Incremented once at startup; because the daemon re-execs
    /// on `/dev/restart-daemon` and is otherwise a supervised long-runner, the
    /// running total is the shell-input restart count for this boot session.
    pub shell_restarts: AtomicU64,

    // --- Input-runtime supervision (in-process respawn on panic) -------------
    /// Input-runtime liveness gauge (1 = running, 0 = dead). Set by the input
    /// runtime supervisor: 1 while the supervised event loop runs, 0 during a
    /// respawn gap and after retries are exhausted. A gauge stored as `AtomicU64`
    /// (0/1) alongside the counters for uniform access.
    pub runtime_up: AtomicU64,
    /// In-process input-runtime respawns after a caught panic. DISTINCT from
    /// `shell_restarts` (whole-daemon process starts): this counts the supervisor
    /// rebuilding the input event loop without re-execing the daemon, so a
    /// nonzero value flags a recurring panic in the input path.
    pub runtime_restarts: AtomicU64,
    /// Detected grab-state drift: a pad's physical `EVIOCGRAB` disagreed with the
    /// presenter policy (`should_grab`) after a transition. Should stay 0; a
    /// nonzero value means the daemon's grab bookkeeping and the kernel diverged.
    pub grab_invariant_violations: AtomicU64,

    // --- Dev/deployment action counters (HTTP-bridge handlers) ---------------
    /// `POST /dev/deploy` attempts that succeeded (git fetch+checkout+reset OK).
    pub deploy_ok: AtomicU64,
    /// `POST /dev/deploy` attempts that failed (git error). Together with
    /// `deploy_ok` these render as `tv_shell_deploy_total{outcome="ok|error"}`.
    pub deploy_err: AtomicU64,
    /// `POST /dev/build` attempts (build via scripts/build-daemon.sh + install).
    pub build_actions: AtomicU64,
    /// `POST /dev/restart-shell` attempts (kill + relaunch quickshell).
    pub restart_shell_actions: AtomicU64,
    /// `POST /dev/restart-daemon` attempts (re-exec the daemon). Counted when the
    /// re-exec is requested — the response is written before the process image is
    /// replaced, so the increment is durable in this process's metrics until the
    /// re-exec lands (the new process starts its own counters at zero).
    pub restart_daemon_actions: AtomicU64,
    /// Times a shell restart (HTTP `/dev/restart-shell` or MCP `restart_shell`,
    /// which share `dev_restart_shell`) detected >1 quickshell process after the
    /// restart settle — the #254 stacked-instance bug; should stay 0.
    pub quickshell_multi_instance: AtomicU64,

    // --- Quickshell (QML) log noise ------------------------------------------
    /// WARN/ERROR lines the Quickshell QML process wrote to `/tmp/qs-log.txt`,
    /// accumulated by [`run_quickshell_warning_scanner`]. Makes the "a healthy
    /// shell start emits a handful of WARN lines, not hundreds" invariant
    /// (docs/OBSERVABILITY.md) alertable — the repo regressed to 10,498 WARN
    /// lines over one session unnoticed before #441 fixed the emitters.
    pub quickshell_warnings: AtomicU64,
}

impl Metrics {
    /// Build a fresh, zeroed metrics set behind an `Arc` for sharing.
    pub fn new() -> Arc<Metrics> {
        Arc::new(Metrics::default())
    }

    #[inline]
    pub fn inc_input_events(&self) {
        self.input_events.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_intents(&self) {
        self.intents_emitted.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_transitions(&self) {
        self.transitions.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_pad_joins(&self) {
        self.pad_joins.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_pad_leaves(&self) {
        self.pad_leaves.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_shell_restarts(&self) {
        self.shell_restarts.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the input-runtime liveness gauge (`true` = running, `false` = dead).
    #[inline]
    pub fn set_runtime_up(&self, up: bool) {
        self.runtime_up.store(up as u64, Ordering::Relaxed);
    }

    /// Count one in-process input-runtime respawn (supervisor caught a panic).
    #[inline]
    pub fn inc_runtime_restarts(&self) {
        self.runtime_restarts.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one detected grab-invariant violation (grab-state drift).
    #[inline]
    pub fn inc_grab_invariant_violations(&self) {
        self.grab_invariant_violations
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a `/dev/deploy` outcome (`true` = success, `false` = failure).
    #[inline]
    pub fn inc_deploy(&self, ok: bool) {
        if ok {
            self.deploy_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.deploy_err.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn inc_build(&self) {
        self.build_actions.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_restart_shell(&self) {
        self.restart_shell_actions.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_restart_daemon(&self) {
        self.restart_daemon_actions.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_quickshell_multi_instance(&self) {
        self.quickshell_multi_instance
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Add `n` newly-observed Quickshell WARN/ERROR lines. Takes a count rather
    /// than incrementing by one because the scanner observes the log file in
    /// batches (one delta per tick), not line by line.
    #[inline]
    pub fn add_quickshell_warnings(&self, n: u64) {
        self.quickshell_warnings.fetch_add(n, Ordering::Relaxed);
    }
}

/// Current-deployment provenance for the `tv_shell_build_info` info-metric.
/// Resolved live (re-read on each render) from the same `capture_meta()` source
/// that backs the `/screenshot` `X-TvShell-*` headers and `/dev/status`, so a
/// `/dev/deploy` HEAD swap under the live daemon is reflected next render.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    pub sha: String,
    pub branch: String,
    pub version: String,
}

/// Escape a Prometheus label value per the exposition format: backslash,
/// double-quote, and newline are the only characters that must be escaped.
fn escape_label_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Format an `f64` for the exposition format. Prometheus accepts plain decimal;
/// we keep it compact and locale-independent (Rust's `Display` for f64 already
/// uses `.` and never a thousands separator).
fn fmt_f64(v: f64) -> String {
    // Guard against NaN/inf which are invalid in the text format apart from the
    // literal `+Inf`/`-Inf`/`NaN` tokens — clamp to 0 for our gauge use.
    if v.is_finite() {
        format!("{v}")
    } else {
        "0".to_string()
    }
}

/// Render the full OpenMetrics/Prometheus exposition text for the daemon.
///
/// `counters` supplies the app-specific `*_total` values; `sys` (optional)
/// supplies the convenience resource gauges; `build` (optional) supplies the
/// `tv_shell_build_info` deployment identity. When an optional is `None` that
/// section is omitted — the counters are always emitted.
///
/// Every metric carries `# HELP` and `# TYPE` lines. All metrics are namespaced
/// `tv_shell_`. The output ends with a trailing newline (required by the
/// node_exporter textfile collector parser).
pub fn render(
    counters: &Metrics,
    sys: Option<&crate::system::SysMetrics>,
    build: Option<&BuildInfo>,
) -> String {
    let mut out = String::with_capacity(1024);

    // ── Current-deployment info metric (always value 1; identity in labels) ───
    if let Some(b) = build {
        out.push_str(
            "# HELP tv_shell_build_info Currently deployed tv-shell revision (value is always 1; identity is in the labels).\n",
        );
        out.push_str("# TYPE tv_shell_build_info gauge\n");
        out.push_str(&format!(
            "tv_shell_build_info{{sha=\"{}\",branch=\"{}\",version=\"{}\"}} 1\n",
            escape_label_value(&b.sha),
            escape_label_value(&b.branch),
            escape_label_value(&b.version),
        ));
    }

    // ── App-specific counters ────────────────────────────────────────────────
    let counter = |out: &mut String, name: &str, help: &str, val: u64| {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} counter\n"));
        out.push_str(&format!("{name} {val}\n"));
    };

    counter(
        &mut out,
        "tv_shell_input_events_total",
        "Raw gamepad input events read and processed by the input runtime.",
        counters.input_events.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_intents_emitted_total",
        "Shell intents broadcast on the event bus (intent:<name>).",
        counters.intents_emitted.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_transitions_total",
        "Shell<->game presenter transitions (grab/release/handoff).",
        counters.transitions.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_pad_joins_total",
        "Gamepads that joined the fleet (hot-join or initial enumeration).",
        counters.pad_joins.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_pad_leaves_total",
        "Gamepads that left the fleet (disconnect).",
        counters.pad_leaves.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_shell_restarts_total",
        "tv-shell-input daemon starts observed this boot session.",
        counters.shell_restarts.load(Ordering::Relaxed),
    );

    // ── Input-runtime supervision ────────────────────────────────────────────
    counter(
        &mut out,
        "tv_shell_input_runtime_restarts_total",
        "In-process input-runtime respawns after a panic (distinct from daemon process starts in tv_shell_shell_restarts_total).",
        counters.runtime_restarts.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_grab_invariant_violations_total",
        "Grab-state drift detected: a pad's physical EVIOCGRAB disagreed with the presenter policy after a transition (should stay 0).",
        counters.grab_invariant_violations.load(Ordering::Relaxed),
    );
    // The input-runtime liveness gauge is app-level and ALWAYS emitted (unlike the
    // convenience sys gauges gated behind `Some(sys)` below): a scrape must be able
    // to alert on the runtime being dead even when no sys metrics are present.
    out.push_str(
        "# HELP tv_shell_input_runtime_up Input runtime liveness (1 = supervised event loop running, 0 = dead/panic-exhausted).\n",
    );
    out.push_str("# TYPE tv_shell_input_runtime_up gauge\n");
    out.push_str(&format!(
        "tv_shell_input_runtime_up {}\n",
        counters.runtime_up.load(Ordering::Relaxed),
    ));

    // ── Dev/deployment action counters ───────────────────────────────────────
    // deploy carries an outcome label so failed deploys are visible; one
    // HELP/TYPE block, two labelled samples.
    out.push_str(
        "# HELP tv_shell_deploy_total /dev/deploy attempts via the HTTP bridge, by outcome.\n",
    );
    out.push_str("# TYPE tv_shell_deploy_total counter\n");
    out.push_str(&format!(
        "tv_shell_deploy_total{{outcome=\"ok\"}} {}\n",
        counters.deploy_ok.load(Ordering::Relaxed),
    ));
    out.push_str(&format!(
        "tv_shell_deploy_total{{outcome=\"error\"}} {}\n",
        counters.deploy_err.load(Ordering::Relaxed),
    ));
    counter(
        &mut out,
        "tv_shell_build_total",
        "/dev/build attempts via the HTTP bridge.",
        counters.build_actions.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_restart_shell_total",
        "/dev/restart-shell attempts via the HTTP bridge.",
        counters.restart_shell_actions.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_restart_daemon_total",
        "/dev/restart-daemon (re-exec) requests via the HTTP bridge.",
        counters.restart_daemon_actions.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_quickshell_multi_instance_total",
        "Times a shell restart (HTTP /dev/restart-shell or MCP restart_shell) detected >1 quickshell process after a restart settle (#254; should stay 0).",
        counters.quickshell_multi_instance.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "tv_shell_quickshell_warnings_total",
        "WARN/ERROR lines written by the Quickshell QML process to /tmp/qs-log.txt, sampled periodically by the daemon (#441). A healthy shell start emits a handful, not hundreds. The log is truncated on every shell start and the scanner treats a shrink or an inode change as a new file, so the counter stays monotonic across shell restarts; on daemon start it seeds from the current file WITHOUT counting its history, so a daemon restart does not replay a whole session. A missing log counts as an empty one, so on a cold boot the seed lands at zero before the shell starts and the startup burst IS counted. Warnings written and truncated away inside one scan interval are not counted.",
        counters.quickshell_warnings.load(Ordering::Relaxed),
    );

    // ── Convenience resource gauges (better sourced from node_exporter) ───────
    if let Some(m) = sys {
        let gauge = |out: &mut String, name: &str, help: &str, val: String| {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} gauge\n"));
            out.push_str(&format!("{name} {val}\n"));
        };

        gauge(
            &mut out,
            "tv_shell_cpu_percent",
            "Aggregate CPU utilisation 0..=100 (convenience; prefer node_exporter).",
            fmt_f64(m.cpu_pct),
        );
        gauge(
            &mut out,
            "tv_shell_mem_used_bytes",
            "Used memory in bytes (convenience; prefer node_exporter).",
            m.mem_used.to_string(),
        );
        gauge(
            &mut out,
            "tv_shell_mem_total_bytes",
            "Total memory in bytes (convenience; prefer node_exporter).",
            m.mem_total.to_string(),
        );
        gauge(
            &mut out,
            "tv_shell_load1",
            "1-minute load average (convenience; prefer node_exporter).",
            fmt_f64(m.load1),
        );

        // Temperature gauges carry a `sensor` label. One HELP/TYPE block, then a
        // sample per sensor (multiple labelled samples of the same metric).
        if !m.temps.is_empty() {
            out.push_str(
                "# HELP tv_shell_temperature_celsius Hardware temperature sensor reading in degrees Celsius (convenience; prefer node_exporter).\n",
            );
            out.push_str("# TYPE tv_shell_temperature_celsius gauge\n");
            for t in &m.temps {
                out.push_str(&format!(
                    "tv_shell_temperature_celsius{{sensor=\"{}\"}} {}\n",
                    escape_label_value(&t.label),
                    fmt_f64(t.celsius),
                ));
            }
        }
    }

    out
}

/// Resolve the current-deployment [`BuildInfo`] live from the shared
/// `capture_meta()` provenance resolver (same source as the `/screenshot`
/// `X-TvShell-*` headers and `/dev/status`). Async because it shells out to
/// `git`; callers `.await` this BEFORE `render_blocking` and pass the result in,
/// so a `/dev/deploy` HEAD swap is reflected on the next render (re-read on
/// render, not cached at startup).
pub async fn resolve_build_info() -> BuildInfo {
    let meta = crate::bridge_core::capture_meta().await;
    BuildInfo {
        sha: meta.sha,
        branch: meta.branch,
        version: meta.version.to_owned(),
    }
}

/// Read the live system metrics on a blocking thread and render the full
/// exposition text. `cpu_percent` sleeps ~200ms internally, so this MUST run on
/// the blocking pool (the textfile task and the HTTP handler both wrap it in
/// `spawn_blocking`).
///
/// `build` is resolved by the caller via [`resolve_build_info`] (async git) and
/// passed in, since this fn runs on the blocking pool and cannot `.await`.
pub fn render_blocking(counters: &Metrics, build: Option<BuildInfo>) -> String {
    let sys = crate::system::sys_metrics();
    render(counters, Some(&sys), build.as_ref())
}

// ─── node_exporter textfile-collector writer ─────────────────────────────────

// The metrics write interval now comes from `[observability].metrics_interval`
// (default 15, clamped to ≥1 by `DaemonConfig::metrics_interval_secs()`); there
// is no longer a local DEFAULT_INTERVAL_SECS const here.

/// Atomically write `text` to `path` via the temp-file + rename pattern the
/// node_exporter textfile collector requires (it reads `*.prom` files and a
/// partial read of a non-atomic write would surface a malformed scrape).
///
/// The temp file is created in the SAME directory as the target so the final
/// `rename(2)` is on one filesystem (cross-device rename fails). The temp name
/// carries the daemon pid to avoid collisions if two writers ever share a dir.
fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let pid = std::process::id();
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("tv-shell"),
        pid
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        // fsync so the rename publishes durable bytes, matching the collector's
        // atomic-write contract under power loss.
        let _ = f.sync_all();
    }
    // rename is atomic on the same filesystem; on error, clean up the temp file.
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Background task: periodically render the metrics exposition text and write it
/// atomically to `textfile_path` (from `[observability].metrics_textfile`).
///
/// **Disabled when `textfile_path` is `None`/empty** — the task returns
/// immediately and no file is ever written. This is the PRIMARY metrics path
/// (node_exporter textfile collector); the `/metrics` HTTP route is the portable
/// alternative. `interval_secs` comes from `[observability].metrics_interval`
/// (already clamped ≥1 by `DaemonConfig::metrics_interval_secs`).
///
/// Mirrors the fire-and-forget spawn pattern of the other daemon actors: it logs
/// and degrades gracefully (a failed write is logged at warn, not fatal) and
/// never panics the daemon.
pub async fn run_textfile_writer(
    counters: Arc<Metrics>,
    textfile_path: Option<String>,
    interval_secs: u64,
) {
    let Some(path_str) = textfile_path.filter(|p| !p.is_empty()) else {
        // Unset/empty → writer disabled (no file). The /metrics route is unaffected.
        tracing::debug!(
            "metrics: [observability].metrics_textfile unset, textfile writer disabled"
        );
        return;
    };
    let path = std::path::PathBuf::from(path_str);
    let secs = interval_secs;
    tracing::info!(
        "metrics: writing textfile-collector metrics to {} every {secs}s",
        path.display()
    );

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
    loop {
        ticker.tick().await;
        // Resolve build identity live (async git) so a /dev/deploy HEAD swap is
        // reflected next render — then render on the blocking pool (sys_metrics()
        // sleeps ~200ms for the CPU sample).
        let build = resolve_build_info().await;
        let counters = Arc::clone(&counters);
        let text = match tokio::task::spawn_blocking(move || {
            render_blocking(&counters, Some(build))
        })
        .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("metrics: render task failed: {e}");
                continue;
            }
        };
        let path = path.clone();
        // The atomic write is also blocking I/O.
        let write_res = tokio::task::spawn_blocking(move || write_atomic(&path, &text)).await;
        match write_res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("metrics: textfile write failed: {e}"),
            Err(e) => tracing::warn!("metrics: write task panicked: {e}"),
        }
    }
}

// ─── Quickshell (QML) warning-log scanner ────────────────────────────────────

/// How often the Quickshell log is sampled for new WARN/ERROR lines. Matches the
/// daemon's existing polling cadence culture (the metrics textfile writer
/// defaults to 15s, `service_health` polls every 30s). Deliberately a const, not
/// a config key: the counter is monotonic, so the interval only bounds how
/// quickly a burst becomes visible and how much a truncate-within-a-tick can
/// swallow.
pub const QS_WARNING_SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Count WARN/ERROR lines in a Quickshell log body.
///
/// Pure (no filesystem) so the predicate is unit-testable, mirroring
/// `bridge_core::count_live_quickshell`. The per-line predicate is
/// `bridge_core::is_warning_line` — shared verbatim with the
/// `/dev/restart-shell` warning tail so the counter and the tail can never
/// disagree. In particular there is **no icon-noise filtering**: noise is fixed
/// at the emitter, never hidden at the reader (docs/OBSERVABILITY.md).
pub(crate) fn count_warning_lines(text: &str) -> u64 {
    text.lines()
        .filter(|l| crate::bridge_core::is_warning_line(l))
        .count() as u64
}

/// What the scanner remembers between ticks so it can turn a repeatedly-read
/// (and repeatedly-truncated) file into a monotonic counter.
#[derive(Debug, Default)]
pub(crate) struct ScanState {
    /// False until the first successful read. The first read only SEEDS the
    /// state; it contributes no delta (see [`warning_delta`]).
    seeded: bool,
    /// Byte length of the file as of the last successful read.
    last_len: u64,
    /// Inode of the file as of the last successful read (`None` on non-unix or
    /// if the metadata read did not expose one).
    last_ino: Option<u64>,
    /// Warning count of the file as of the last successful read.
    last_count: u64,
}

/// Fold one observation of the log file into `state` and return how many
/// warnings to add to the counter.
///
/// Pure, so the whole truncation/restart story is testable without a filesystem.
/// Three cases:
///
/// 1. **First observation of this daemon's life** (`!seeded`) → delta `0`. On a
///    mid-session `/dev/restart-daemon` re-exec the file already holds a whole
///    session's warnings, and counting them would replay history into a single
///    scrape interval. Note the scanner treats a **missing** log as a valid
///    observation of an empty file (`len 0`, `count 0`) rather than as a skip —
///    so on a cold boot the seed lands at zero *before* the shell starts, and
///    the startup burst is then counted as an ordinary append (case 3). Seeding
///    off a skip instead would swallow exactly the flood this metric exists to
///    catch.
/// 2. **New file** — `len` shrank, or the inode changed → the whole current
///    count is new. `/tmp/qs-log.txt` is truncated on every shell start, by the
///    unit's `tee` and by the daemon's fallback spawn; `tee` truncates the SAME
///    inode, so in practice the length check is the one that fires and the inode
///    check covers an unlink-and-replace.
/// 3. **Append** → the difference. `saturating_sub` is defensive: a file that
///    was truncated and then grew back past `last_len` inside one interval can
///    present a smaller count at a larger length, and losing those warnings is
///    the documented, accepted cost of sampling.
pub(crate) fn warning_delta(state: &mut ScanState, len: u64, ino: Option<u64>, count: u64) -> u64 {
    let replaced = match (ino, state.last_ino) {
        (Some(now), Some(before)) => now != before,
        // Unknown inode on either side: fall back to the length check alone.
        _ => false,
    };
    let delta = if !state.seeded {
        0
    } else if len < state.last_len || replaced {
        count
    } else {
        count.saturating_sub(state.last_count)
    };

    state.seeded = true;
    state.last_len = len;
    state.last_ino = ino;
    state.last_count = count;
    delta
}

/// One observation of the log file.
struct ScanSample {
    len: u64,
    ino: Option<u64>,
    count: u64,
}

/// Read `path` and count its warning lines. `Ok(None)` means the file does not
/// exist yet — the normal state before the first shell start, on CI, and on any
/// non-Linux host. Callers treat that as a definitive observation of an EMPTY
/// log (not as a failed read): the shell truncates-or-creates this file, so
/// "absent" and "present but empty" are the same fact, and conflating them is
/// what lets a cold-boot warning burst be counted. Contrast `Err(_)`, which
/// means the read genuinely told us nothing.
///
/// Blocking I/O; callers run it on the blocking pool. `len` is the number of
/// bytes actually read rather than `metadata().len()` so the length and the
/// count always describe the same bytes.
fn sample_log(path: &std::path::Path) -> std::io::Result<Option<ScanSample>> {
    use std::io::Read;

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let ino = f.metadata().ok().and_then(|m| metadata_ino(&m));
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    // The shell can emit non-UTF-8 bytes (raw child-process stderr); lossy
    // decoding keeps the surrounding line countable instead of dropping the read.
    let text = String::from_utf8_lossy(&buf);
    Ok(Some(ScanSample {
        len: buf.len() as u64,
        ino,
        count: count_warning_lines(&text),
    }))
}

/// The file's inode, where the platform exposes one. `std::os::unix` (not
/// `std::os::linux`) keeps the module's cross-platform invariant: this still
/// compiles and unit-tests on macOS, and degrades to `None` elsewhere.
#[cfg(unix)]
fn metadata_ino(m: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(m.ino())
}

#[cfg(not(unix))]
fn metadata_ino(_m: &std::fs::Metadata) -> Option<u64> {
    None
}

/// Background task: periodically sample the Quickshell log at `path` and fold
/// newly-appeared WARN/ERROR lines into `tv_shell_quickshell_warnings_total`.
///
/// Spawned **unconditionally** from `main.rs` — deliberately NOT inside
/// [`run_textfile_writer`], which early-returns when
/// `[observability].metrics_textfile` is unset. The counter must be there for the
/// `/metrics` HTTP route whether or not the textfile sink is configured.
///
/// The scan lives here and never in [`render`]: `render` runs on every scrape AND
/// every textfile tick, so reading the file there would tie counting to scrape
/// frequency and double-count whenever both sinks are active.
///
/// Fire-and-forget like the other daemon actors: a missing file is a `debug!`
/// skip (the normal pre-first-shell-start / CI / non-Linux state, not worth
/// warn-spamming every tick), an I/O error is a `debug!` skip, and nothing here
/// can panic the daemon.
pub async fn run_quickshell_warning_scanner(
    counters: Arc<Metrics>,
    path: std::path::PathBuf,
    interval: std::time::Duration,
) {
    tracing::info!(
        "metrics: scanning {} for quickshell WARN/ERROR lines every {}s",
        path.display(),
        interval.as_secs()
    );

    let mut state = ScanState::default();
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;

        let scan_path = path.clone();
        // Blocking file I/O — off the async runtime, like the textfile writer.
        let sample = match tokio::task::spawn_blocking(move || sample_log(&scan_path)).await {
            Ok(Ok(Some(s))) => s,
            Ok(Ok(None)) => {
                // NOT a skip. An absent log is a definitive observation of an
                // EMPTY one, and treating it as such is what makes a cold boot
                // work: the daemon's first tick fires before the shell exists,
                // seeds at zero, and the startup burst is then counted as a
                // plain append on the next tick. Skipping here would leave the
                // state unseeded until after the burst had already landed, and
                // the seed would swallow it whole.
                tracing::debug!(
                    "metrics: {} absent, counting it as an empty log",
                    path.display()
                );
                ScanSample {
                    len: 0,
                    ino: None,
                    count: 0,
                }
            }
            Ok(Err(e)) => {
                // A real read error IS a skip: unlike NotFound it tells us
                // nothing about the file's contents, so seeding off it could
                // baseline at a wrong count.
                tracing::debug!("metrics: quickshell log read failed: {e}");
                continue;
            }
            Err(e) => {
                tracing::warn!("metrics: quickshell log scan task panicked: {e}");
                continue;
            }
        };

        let seeding = !state.seeded;
        let delta = warning_delta(&mut state, sample.len, sample.ino, sample.count);
        if seeding {
            tracing::debug!(
                "metrics: seeded quickshell warning scanner at {} pre-existing WARN/ERROR \
                 line(s); anything already in the log is treated as history and not counted",
                sample.count
            );
        } else if delta > 0 {
            counters.add_quickshell_warnings(delta);
            tracing::debug!("metrics: +{delta} quickshell WARN/ERROR lines");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::{SysMetrics, TempEntry};

    #[test]
    fn counters_render_with_help_and_type() {
        let m = Metrics::default();
        m.inc_input_events();
        m.inc_input_events();
        m.inc_intents();
        m.inc_transitions();
        m.inc_pad_joins();
        m.inc_shell_restarts();
        let text = render(&m, None, None);

        // Each counter has HELP + TYPE + a sample line.
        assert!(text.contains("# HELP tv_shell_input_events_total"));
        assert!(text.contains("# TYPE tv_shell_input_events_total counter"));
        assert!(text.contains("\ntv_shell_input_events_total 2\n"));
        assert!(text.contains("\ntv_shell_intents_emitted_total 1\n"));
        assert!(text.contains("\ntv_shell_transitions_total 1\n"));
        assert!(text.contains("\ntv_shell_pad_joins_total 1\n"));
        assert!(text.contains("\ntv_shell_pad_leaves_total 0\n"));
        assert!(text.contains("\ntv_shell_shell_restarts_total 1\n"));
        // Counters that were never touched are still always present at 0 — a
        // scrape must be able to alert on them without waiting for a first event.
        assert!(text.contains("\ntv_shell_quickshell_multi_instance_total 0\n"));
        assert!(text.contains("\ntv_shell_quickshell_warnings_total 0\n"));
        // Trailing newline (textfile collector requirement).
        assert!(text.ends_with('\n'));
        // No gauges when sys is None; no build_info when build is None.
        assert!(!text.contains("tv_shell_cpu_percent"));
        assert!(!text.contains("tv_shell_build_info"));
    }

    #[test]
    fn runtime_supervision_metrics_render() {
        let m = Metrics::default();
        // Fresh: the up-gauge defaults to 0 and both new counters are 0, but all
        // three are ALWAYS present (no Some(sys)/Some(build) gating).
        let text0 = render(&m, None, None);
        assert!(text0.contains("# TYPE tv_shell_input_runtime_up gauge"));
        assert!(text0.contains("\ntv_shell_input_runtime_up 0\n"));
        assert!(text0.contains("# TYPE tv_shell_input_runtime_restarts_total counter"));
        assert!(text0.contains("\ntv_shell_input_runtime_restarts_total 0\n"));
        assert!(text0.contains("# TYPE tv_shell_grab_invariant_violations_total counter"));
        assert!(text0.contains("\ntv_shell_grab_invariant_violations_total 0\n"));

        m.set_runtime_up(true);
        m.inc_runtime_restarts();
        m.inc_runtime_restarts();
        m.inc_grab_invariant_violations();
        let text = render(&m, None, None);
        assert!(text.contains("\ntv_shell_input_runtime_up 1\n"));
        assert!(text.contains("\ntv_shell_input_runtime_restarts_total 2\n"));
        assert!(text.contains("\ntv_shell_grab_invariant_violations_total 1\n"));

        // The gauge tracks liveness both directions.
        m.set_runtime_up(false);
        assert!(render(&m, None, None).contains("\ntv_shell_input_runtime_up 0\n"));
    }

    #[test]
    fn dev_action_counters_render() {
        let m = Metrics::default();
        m.inc_deploy(true);
        m.inc_deploy(true);
        m.inc_deploy(false);
        m.inc_build();
        m.inc_restart_shell();
        m.inc_restart_daemon();
        m.inc_quickshell_multi_instance();
        let text = render(&m, None, None);

        assert!(text.contains("# TYPE tv_shell_deploy_total counter"));
        assert!(text.contains("tv_shell_deploy_total{outcome=\"ok\"} 2\n"));
        assert!(text.contains("tv_shell_deploy_total{outcome=\"error\"} 1\n"));
        assert!(text.contains("\ntv_shell_build_total 1\n"));
        assert!(text.contains("\ntv_shell_restart_shell_total 1\n"));
        assert!(text.contains("\ntv_shell_restart_daemon_total 1\n"));
        assert!(text.contains("\ntv_shell_quickshell_multi_instance_total 1\n"));
    }

    #[test]
    fn build_info_renders_value_1_with_labels() {
        let m = Metrics::default();
        let build = BuildInfo {
            sha: "a1b2c3d".into(),
            branch: "feat/daemon-observability".into(),
            version: "0.1.0".into(),
        };
        let text = render(&m, None, Some(&build));
        assert!(text.contains("# TYPE tv_shell_build_info gauge"));
        assert!(text.contains(
            "tv_shell_build_info{sha=\"a1b2c3d\",branch=\"feat/daemon-observability\",version=\"0.1.0\"} 1\n"
        ));
    }

    #[test]
    fn gauges_render_when_sys_present() {
        let m = Metrics::default();
        let sys = SysMetrics {
            cpu_pct: 12.5,
            mem_used: 1024,
            mem_total: 4096,
            mem_pct: 25,
            load1: 0.42,
            temps: vec![
                TempEntry {
                    label: "CPU Tctl".into(),
                    celsius: 55.0,
                },
                TempEntry {
                    label: "GPU edge".into(),
                    celsius: 48.5,
                },
            ],
        };
        let text = render(&m, Some(&sys), None);

        assert!(text.contains("# TYPE tv_shell_cpu_percent gauge"));
        assert!(text.contains("\ntv_shell_cpu_percent 12.5\n"));
        assert!(text.contains("\ntv_shell_mem_used_bytes 1024\n"));
        assert!(text.contains("\ntv_shell_mem_total_bytes 4096\n"));
        assert!(text.contains("\ntv_shell_load1 0.42\n"));
        assert!(text.contains("# TYPE tv_shell_temperature_celsius gauge"));
        assert!(text.contains("tv_shell_temperature_celsius{sensor=\"CPU Tctl\"} 55\n"));
        assert!(text.contains("tv_shell_temperature_celsius{sensor=\"GPU edge\"} 48.5\n"));
    }

    #[test]
    fn label_value_escaping() {
        assert_eq!(escape_label_value("plain"), "plain");
        assert_eq!(escape_label_value("a\"b"), "a\\\"b");
        assert_eq!(escape_label_value("a\\b"), "a\\\\b");
        assert_eq!(escape_label_value("a\nb"), "a\\nb");
    }

    #[test]
    fn non_finite_gauge_is_zero() {
        assert_eq!(fmt_f64(f64::NAN), "0");
        assert_eq!(fmt_f64(f64::INFINITY), "0");
        assert_eq!(fmt_f64(3.0), "3");
    }

    #[test]
    fn write_atomic_creates_file_with_exact_contents() {
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir.
        let dir = crate::testutil::scratch_dir("gs-metrics-test");
        let path = dir.join("tv-shell.prom");
        let body = "# HELP x test\n# TYPE x counter\nx 1\n";
        write_atomic(&path, body).expect("atomic write succeeds");
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, body);
        // No stray temp files left behind in the dir.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    // The interval default/clamp now lives in
    // `DaemonConfig::metrics_interval_secs()` and is tested in daemon_config.rs.

    // ── Quickshell warning scanner ───────────────────────────────────────────

    #[test]
    fn count_warning_lines_matches_the_restart_tail_predicate() {
        // WARN and ERROR, in the shapes quickshell actually emits.
        assert_eq!(count_warning_lines("qml: WARN something\n"), 1);
        assert_eq!(count_warning_lines("ERROR: failed to load\n"), 1);
        // Case-insensitive: Qt category lines are lowercase, QML console.warn is not.
        assert_eq!(
            count_warning_lines("warning: x\nWarning: y\nqt.svg.draw: error here\n"),
            3
        );
        // Lines containing neither are not counted.
        assert_eq!(count_warning_lines("info: started\ndebug: tick\n"), 0);
        // Empty input.
        assert_eq!(count_warning_lines(""), 0);
        // Mixed body: only the WARN/ERROR lines count.
        assert_eq!(
            count_warning_lines("info: a\nWARN b\ndebug: c\nERROR d\n"),
            2
        );
        // A trailing line with no newline still counts (str::lines yields it).
        assert_eq!(count_warning_lines("WARN no trailing newline"), 1);
    }

    #[test]
    fn count_warning_lines_does_not_filter_icon_noise() {
        // The `/dev/restart-shell` tail used to drop these unconditionally. The
        // filter is gone on purpose (docs/OBSERVABILITY.md) — hiding it at the
        // reader would hide exactly the regression this counter exists to catch.
        let icon_flood = "qt.gui.icc: WARNING: COULD NOT LOAD ICON application-x-foo\n".repeat(5);
        assert_eq!(count_warning_lines(&icon_flood), 5);
        assert_eq!(
            count_warning_lines("qt.svg.draw: The requested buffer size is too big, ignoring\n"),
            0,
            "a line with neither WARN nor ERROR is still not counted"
        );
    }

    #[test]
    fn quickshell_warnings_render() {
        let m = Metrics::default();
        m.add_quickshell_warnings(3);
        m.add_quickshell_warnings(4);
        let text = render(&m, None, None);

        assert!(text.contains("# HELP tv_shell_quickshell_warnings_total"));
        assert!(text.contains("# TYPE tv_shell_quickshell_warnings_total counter"));
        assert!(text.contains("\ntv_shell_quickshell_warnings_total 7\n"));
    }

    #[test]
    fn warning_delta_seeds_without_counting_history() {
        // The daemon restarts independently of the shell, so its FIRST read of an
        // existing log must not replay a whole session into one scrape interval.
        let mut st = ScanState::default();
        assert_eq!(warning_delta(&mut st, 4096, Some(7), 500), 0);
        // ...but it now tracks that file, so subsequent appends do count.
        assert_eq!(warning_delta(&mut st, 4200, Some(7), 503), 3);
    }

    #[test]
    fn warning_delta_counts_a_cold_boot_burst_after_an_absent_log() {
        // THE regression this metric exists to catch is a cold-boot warning
        // flood, and the scanner's first tick fires before the shell has created
        // /tmp/qs-log.txt at all. The absent log must therefore seed at ZERO
        // (not be skipped), so the burst that lands moments later is an ordinary
        // append and gets counted. If this returns 0, the counter sits at 0
        // through exactly the event it was added to alert on.
        let mut st = ScanState::default();
        assert_eq!(
            warning_delta(&mut st, 0, None, 0),
            0,
            "absent log seeds at zero"
        );
        assert_eq!(
            warning_delta(&mut st, 3923, Some(1), 37),
            37,
            "the shell's startup burst must be counted, not swallowed by the seed"
        );
    }

    #[test]
    fn warning_delta_still_ignores_history_on_a_daemon_re_exec() {
        // The companion to the test above: /dev/restart-daemon re-execs into a
        // fresh ScanState while the shell keeps running, so the FIRST observation
        // is a pre-existing non-empty log. That history must NOT be replayed into
        // one scrape interval. Both behaviours have to hold at once — a change
        // that "fixes" either by breaking the other fails here.
        let mut st = ScanState::default();
        assert_eq!(warning_delta(&mut st, 3923, Some(1), 37), 0);
        assert_eq!(warning_delta(&mut st, 4100, Some(1), 39), 2);
    }

    #[test]
    fn warning_delta_counts_appends_only() {
        let mut st = ScanState::default();
        warning_delta(&mut st, 0, Some(7), 0); // seed on an empty file
        assert_eq!(warning_delta(&mut st, 100, Some(7), 2), 2);
        assert_eq!(
            warning_delta(&mut st, 100, Some(7), 2),
            0,
            "no growth, no delta"
        );
        assert_eq!(warning_delta(&mut st, 300, Some(7), 9), 7);
    }

    #[test]
    fn warning_delta_treats_a_shrink_as_a_fresh_file() {
        // `tee` truncates the SAME inode on every shell start: size shrinks,
        // inode unchanged. The whole new count is new.
        let mut st = ScanState::default();
        warning_delta(&mut st, 0, Some(7), 0);
        assert_eq!(warning_delta(&mut st, 9000, Some(7), 40), 40);
        assert_eq!(
            warning_delta(&mut st, 120, Some(7), 3),
            3,
            "truncate + 3 fresh warnings = 3, not a saturating 0 and never negative"
        );
        // And it keeps counting from the new baseline afterwards.
        assert_eq!(warning_delta(&mut st, 400, Some(7), 5), 2);
    }

    #[test]
    fn warning_delta_treats_an_inode_change_as_a_fresh_file() {
        // An unlink-and-replace can leave the file LARGER than before, so the
        // length check alone would miscount it as an append.
        let mut st = ScanState::default();
        warning_delta(&mut st, 100, Some(7), 5);
        assert_eq!(warning_delta(&mut st, 900, Some(8), 6), 6);
    }

    #[test]
    fn warning_delta_without_inodes_falls_back_to_length() {
        // Non-unix / metadata unavailable: `None` inodes must not be read as a
        // replacement on every single tick.
        let mut st = ScanState::default();
        warning_delta(&mut st, 100, None, 5);
        assert_eq!(warning_delta(&mut st, 200, None, 8), 3);
        assert_eq!(
            warning_delta(&mut st, 10, None, 1),
            1,
            "shrink still detected"
        );
    }

    #[test]
    fn sample_log_reads_counts_and_reports_missing_files() {
        // See `crate::testutil` for why this is based on `current_exe()` rather
        // than the system temp dir.
        let dir = crate::testutil::scratch_dir("qs-warning-scan-test");
        let path = dir.join("qs-log.txt");

        // Missing file → Ok(None), the pre-first-shell-start / CI / non-Linux case.
        assert!(sample_log(&path)
            .expect("missing file is not an error")
            .is_none());

        let body = "info: start\nWARN a\nERROR b\n";
        std::fs::write(&path, body).unwrap();
        let s = sample_log(&path).unwrap().expect("file exists");
        assert_eq!(s.count, 2);
        assert_eq!(s.len, body.len() as u64);

        // Truncate + rewrite through the same path (what `tee` does): the sample
        // shrinks, which is what `warning_delta` keys off.
        std::fs::write(&path, "WARN only\n").unwrap();
        let s2 = sample_log(&path).unwrap().expect("file exists");
        assert_eq!(s2.count, 1);
        assert!(s2.len < s.len);

        std::fs::remove_dir_all(&dir).ok();
    }
}

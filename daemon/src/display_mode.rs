//! Display-mode plumbing: the pure half of "change the resolution, refresh
//! rate or VRR mode of a Hyprland output".
//!
//! The compositor half (talking to Hyprland's request socket, arming the
//! revert timer) lives in [`crate::hyprland`], which is Linux-only. Everything
//! here is string and file work with no compositor dependency, so it builds
//! and unit-tests on every host — the same split `moonlight` and `netinfo`
//! use.
//!
//! ## Why a `monitor=` line and not `misc:vrr`
//!
//! The obvious way to toggle VRR is `hyprctl keyword misc:vrr <0|1|2>`. On a
//! box that pins its output in config it does nothing: Hyprland's **per-output
//! `vrr` argument overrides the global `misc:vrr`**, and the reference config
//! for this shell (`config/hyprland.conf.example`) ships exactly such a line —
//! `monitor=HDMI-A-1,3840x2160@120,0x0,2,vrr,1,bitdepth,10,cm,hdr,…`. So the
//! only mechanism that actually moves VRR on a configured output is re-issuing
//! that output's `monitor=` keyword.
//!
//! That in turn is why this module rewrites **one field of an existing line**
//! rather than composing a fresh one. `hyprctl monitors -j` reports the mode,
//! position and scale, but **not** `bitdepth`, `cm`, `sdrbrightness` or
//! `sdrsaturation` — so a line rebuilt from the live read would silently drop
//! the 10-bit HDR setup while appearing to succeed. [`rewrite_mode`] and
//! [`rewrite_vrr`] therefore take the existing line and replace a single
//! field, leaving every other argument byte-identical.
//!
//! ## Where the "existing line" comes from
//!
//! `~/.config/tv-shell/hyprland-local.conf` — the per-machine override
//! `config/hyprland.conf` already `source`s. That file is the truth about what
//! this output boots with, and it is the only file this module ever writes.

use std::path::PathBuf;

/// How long an applied mode stays live before it reverts on its own.
///
/// The TV is a couch device with no keyboard: a mode the display cannot lock
/// leaves no way to undo it. So a change is provisional until confirmed —
/// the same apply-then-auto-revert contract Windows and GNOME use.
pub const REVERT_SECONDS: u64 = 15;

/// `~/.config/tv-shell/hyprland-local.conf` — the per-machine display override
/// `config/hyprland.conf` sources. The only file this module writes.
pub fn local_conf_path() -> PathBuf {
    tv_shell_protocol::brand::config_dir().join("hyprland-local.conf")
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

/// One `WIDTHxHEIGHT@REFRESH` display mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh: f64,
}

impl Mode {
    /// Parse `3840x2160@120`, `3840x2160@120.00000` or Hyprland's own
    /// `availableModes` spelling `3840x2160@120.00000Hz` (the `Hz` suffix
    /// appears in current Hyprland and not in older ones, so it is optional).
    ///
    /// Rejects anything else, including a bare `WxH` with no refresh: an
    /// unspecified refresh would let Hyprland pick, and a control that may
    /// silently pick a different rate than the one clicked is worse than one
    /// that refuses.
    pub fn parse(s: &str) -> Option<Mode> {
        let s = s.trim();
        let s = s
            .strip_suffix("Hz")
            .or_else(|| s.strip_suffix("hz"))
            .unwrap_or(s);
        let (res, rate) = s.split_once('@')?;
        let (w, h) = res.split_once('x')?;
        let width: u32 = w.trim().parse().ok()?;
        let height: u32 = h.trim().parse().ok()?;
        let refresh: f64 = rate.trim().parse().ok()?;
        if width == 0 || height == 0 || !refresh.is_finite() || refresh <= 0.0 {
            return None;
        }
        Some(Mode {
            width,
            height,
            refresh,
        })
    }

    /// The canonical spelling this module writes into a `monitor=` line:
    /// `3840x2160@120`, with the refresh trimmed of trailing zeros. Hyprland
    /// resolves a rate to the closest mode the output reports, so the trimmed
    /// form selects the same mode as the five-decimal one.
    pub fn to_keyword(self) -> String {
        format!(
            "{}x{}@{}",
            self.width,
            self.height,
            trim_float(self.refresh, 5)
        )
    }

    /// A human label for the panel's `<option>` text: `3840 × 2160 @ 120 Hz`.
    pub fn label(self) -> String {
        format!(
            "{} × {} @ {} Hz",
            self.width,
            self.height,
            trim_float(self.refresh, 2)
        )
    }

    /// Whether two modes name the same output mode, comparing the refresh with
    /// the tolerance Hyprland's own rounding needs (`120` vs `119.99800`).
    pub fn same(self, other: Mode) -> bool {
        self.width == other.width
            && self.height == other.height
            && (self.refresh - other.refresh).abs() < 0.5
    }
}

/// Format `v` with at most `places` decimals and no trailing zeros, so a mode
/// reads `120` rather than `120.00000` and a scale reads `1.5` rather than
/// `1.500000`.
fn trim_float(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// `monitor=` line surgery
// ---------------------------------------------------------------------------

/// Everything after `monitor=`, split on commas with surrounding whitespace
/// trimmed. Field 0 is the output name, 1 the resolution, 2 the position, 3
/// the scale; 4 onwards are `key,value` pairs (`vrr,1`, `bitdepth,10`,
/// `cm,hdr`, …).
fn fields(line: &str) -> Vec<String> {
    line.split(',').map(|f| f.trim().to_string()).collect()
}

/// The output name a `monitor=` value addresses, or `""` for the catch-all
/// `monitor=,preferred,auto,auto`.
pub fn line_name(line: &str) -> String {
    fields(line).first().cloned().unwrap_or_default()
}

/// The mode field of a `monitor=` value, when it is an explicit `WxH@R` rather
/// than a keyword like `preferred` / `highres` / `highrr`.
pub fn line_mode(line: &str) -> Option<Mode> {
    fields(line).get(1).and_then(|f| Mode::parse(f))
}

/// The per-output `vrr` argument, when the line carries one.
pub fn line_vrr(line: &str) -> Option<u8> {
    let f = fields(line);
    let idx = f.iter().skip(4).position(|t| t == "vrr")? + 4;
    f.get(idx + 1)?.parse().ok()
}

/// Replace the resolution field, leaving every other argument untouched.
///
/// Returns `None` for a value with no resolution field at all (a malformed
/// line) — the caller reports that rather than writing a guess.
pub fn rewrite_mode(line: &str, mode: Mode) -> Option<String> {
    let mut f = fields(line);
    if f.len() < 2 {
        return None;
    }
    f[1] = mode.to_keyword();
    Some(f.join(","))
}

/// Set the per-output `vrr` argument, replacing an existing one in place or
/// appending the pair when the line has none.
///
/// Appending is what makes this work on the generic committed default: a line
/// that never mentioned VRR gains `,vrr,<n>` and starts overriding
/// `misc:vrr`, which is the only way the setting sticks per output.
pub fn rewrite_vrr(line: &str, vrr: u8) -> Option<String> {
    let mut f = fields(line);
    // A line needs at least name,res,pos,scale before a key/value pair can be
    // appended — Hyprland parses the first four positionally.
    if f.len() < 4 {
        return None;
    }
    match f.iter().skip(4).position(|t| t == "vrr") {
        Some(rel) => {
            let idx = rel + 4;
            if idx + 1 < f.len() {
                f[idx + 1] = vrr.to_string();
            } else {
                f.push(vrr.to_string());
            }
        }
        None => {
            f.push("vrr".to_string());
            f.push(vrr.to_string());
        }
    }
    Some(f.join(","))
}

/// Compose a `monitor=` value from a live `hypr-monitors` entry, for an output
/// the local conf says nothing about.
///
/// Deliberately minimal — name, mode, position, scale. It carries no HDR /
/// bitdepth arguments because the live read does not report them; an output
/// with none configured has none to preserve.
pub fn synthesize_line(name: &str, mode: Mode, x: i64, y: i64, scale: f64) -> String {
    format!(
        "{},{},{}x{},{}",
        name,
        mode.to_keyword(),
        x,
        y,
        trim_float(scale, 6)
    )
}

/// One entry of the mode picker the panel renders.
#[derive(Debug, PartialEq)]
pub struct ModeOption {
    /// The wire value, i.e. what `hypr-set-mode` takes back.
    pub value: String,
    /// `3840 × 2160 @ 120 Hz`.
    pub label: String,
    /// Whether this is the mode the output is running right now.
    pub current: bool,
}

/// Turn an output's raw `availableModes` into the picker the panel shows:
/// parseable entries only, de-duplicated, and ordered largest-and-fastest
/// first.
///
/// Hyprland repeats modes (the same resolution at rates that round to the
/// same value) and emits them in EDID order, which puts obscure low modes at
/// the top. Both are worth fixing here rather than in the template: the panel
/// is a renderer, and this is the list `hypr-set-mode` validates against.
pub fn mode_options(available: &[String], current: Option<Mode>) -> Vec<ModeOption> {
    let mut modes: Vec<Mode> = available.iter().filter_map(|s| Mode::parse(s)).collect();
    // Largest area first, then highest refresh — the order someone scanning a
    // TV's mode list expects.
    modes.sort_by(|a, b| {
        let area = |m: &Mode| u64::from(m.width) * u64::from(m.height);
        area(b)
            .cmp(&area(a))
            .then(b.refresh.total_cmp(&a.refresh))
            .then(b.width.cmp(&a.width))
    });
    let mut out: Vec<ModeOption> = Vec::new();
    let mut seen: Vec<Mode> = Vec::new();
    for m in modes {
        // De-duplicate by [`Mode::same`], not by the rendered string: Hyprland
        // lists `120.00000` and `119.99800` as separate modes and they are the
        // same 120 Hz entry to a viewer — and to `set_mode`, which validates
        // with the same tolerance. Sort order puts the rounder rate first, so
        // that is the one kept.
        if seen.iter().any(|s| s.same(m)) {
            continue;
        }
        seen.push(m);
        out.push(ModeOption {
            value: m.to_keyword(),
            label: m.label(),
            current: current.is_some_and(|c| c.same(m)),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// hyprland-local.conf editing
// ---------------------------------------------------------------------------

/// Split a config line into its code and its trailing `# comment`, so a
/// rewrite preserves an operator's note about why that line looks like it does.
fn split_comment(line: &str) -> (&str, &str) {
    match line.find('#') {
        Some(i) => (&line[..i], &line[i..]),
        None => (line, ""),
    }
}

/// The `monitor=` value on this line, if it is one. Tolerates
/// `monitor = X` and leading indentation.
fn monitor_value(line: &str) -> Option<&str> {
    let (code, _) = split_comment(line);
    let rest = code.trim_start().strip_prefix("monitor")?;
    let rest = rest.trim_start().strip_prefix('=')?;
    Some(rest.trim())
}

/// Index of the line declaring `name`'s monitor value.
///
/// The catch-all `monitor=,preferred,auto,auto` is **not** a match for a named
/// output: rewriting it would repoint every display on the box, so a named
/// output with no line of its own gets one appended instead.
pub fn find_monitor_line(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    text.lines()
        .position(|l| monitor_value(l).is_some_and(|v| line_name(v) == name))
}

/// The configured `monitor=` value for `name`, if the file declares one.
pub fn configured_line(text: &str, name: &str) -> Option<String> {
    let idx = find_monitor_line(text, name)?;
    monitor_value(text.lines().nth(idx)?).map(str::to_string)
}

/// Return `text` with `name`'s `monitor=` line set to `value`, appending a new
/// line when the file declares none.
///
/// Only that one line changes: indentation, trailing comment, and every other
/// line (the `quirks`/`render` HDR blocks the example config ships) survive
/// byte-identical. A file with no trailing newline gains one.
pub fn upsert_monitor_line(text: &str, name: &str, value: &str) -> String {
    if let Some(idx) = find_monitor_line(text, name) {
        let mut out: Vec<String> = text.lines().map(str::to_string).collect();
        let original = out[idx].clone();
        let indent: String = original.chars().take_while(|c| c.is_whitespace()).collect();
        let (_, comment) = split_comment(&original);
        let suffix = if comment.is_empty() {
            String::new()
        } else {
            format!("  {comment}")
        };
        out[idx] = format!("{indent}monitor={value}{suffix}");
        let mut joined = out.join("\n");
        joined.push('\n');
        return joined;
    }

    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(
        "# Written by tv-shell-panel (Devices > Display & Audio). Edit freely — the\n\
         # panel only ever rewrites this line's resolution and vrr fields.\n",
    );
    out.push_str(&format!("monitor={value}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deployed line from `config/hyprland.conf.example`: 4K120 10-bit HDR
    /// over an AVR. Every rewrite test uses it, because the whole point of
    /// field surgery is that these arguments survive.
    const HDR_LINE: &str =
        "HDMI-A-1,3840x2160@120,0x0,2,vrr,1,bitdepth,10,cm,hdr,sdrbrightness,1.2,sdrsaturation,1.05";

    #[test]
    fn mode_parses_every_spelling_hyprland_emits() {
        let want = Mode {
            width: 3840,
            height: 2160,
            refresh: 120.0,
        };
        for s in [
            "3840x2160@120",
            "3840x2160@120.00000",
            "3840x2160@120.00000Hz",
            "  3840x2160@120  ",
        ] {
            assert_eq!(Mode::parse(s), Some(want), "parsing {s:?}");
        }
    }

    #[test]
    fn mode_rejects_junk_and_a_missing_refresh() {
        // A bare WxH is rejected on purpose — see `Mode::parse`.
        for s in [
            "3840x2160",
            "preferred",
            "highrr",
            "@120",
            "0x0@60",
            "3840x2160@0",
            "",
        ] {
            assert_eq!(Mode::parse(s), None, "parsing {s:?}");
        }
    }

    #[test]
    fn mode_keyword_trims_hyprlands_trailing_zeros() {
        assert_eq!(
            Mode::parse("3840x2160@120.00000").unwrap().to_keyword(),
            "3840x2160@120"
        );
        assert_eq!(
            Mode::parse("1920x1080@59.94000").unwrap().to_keyword(),
            "1920x1080@59.94"
        );
    }

    #[test]
    fn mode_label_reads_as_a_menu_entry() {
        assert_eq!(
            Mode::parse("3840x2160@119.998Hz").unwrap().label(),
            "3840 × 2160 @ 120 Hz"
        );
    }

    #[test]
    fn same_tolerates_hyprlands_rounding() {
        let a = Mode::parse("3840x2160@120").unwrap();
        let b = Mode::parse("3840x2160@119.998Hz").unwrap();
        assert!(a.same(b));
        assert!(!a.same(Mode::parse("3840x2160@60").unwrap()));
        assert!(!a.same(Mode::parse("1920x1080@120").unwrap()));
    }

    /// The invariant the whole module exists for: changing the mode must not
    /// drop bitdepth/cm/sdr* — those are what keep 4K120 HDR pinned.
    #[test]
    fn rewrite_mode_preserves_every_other_argument() {
        let out = rewrite_mode(HDR_LINE, Mode::parse("1920x1080@60").unwrap()).unwrap();
        assert_eq!(
            out,
            "HDMI-A-1,1920x1080@60,0x0,2,vrr,1,bitdepth,10,cm,hdr,sdrbrightness,1.2,sdrsaturation,1.05"
        );
        assert_eq!(line_vrr(&out), Some(1));
    }

    #[test]
    fn rewrite_vrr_replaces_in_place_and_leaves_the_mode_alone() {
        let out = rewrite_vrr(HDR_LINE, 0).unwrap();
        assert_eq!(line_vrr(&out), Some(0));
        assert_eq!(line_mode(&out), line_mode(HDR_LINE));
        assert!(out.contains("cm,hdr"), "HDR arguments dropped: {out}");
    }

    #[test]
    fn rewrite_vrr_appends_when_the_line_has_none() {
        let out = rewrite_vrr("HDMI-A-1,3840x2160@120,0x0,2", 2).unwrap();
        assert_eq!(out, "HDMI-A-1,3840x2160@120,0x0,2,vrr,2");
        assert_eq!(line_vrr(&out), Some(2));
    }

    #[test]
    fn rewrites_refuse_a_line_too_short_to_be_one() {
        assert_eq!(
            rewrite_mode("HDMI-A-1", Mode::parse("1920x1080@60").unwrap()),
            None
        );
        assert_eq!(rewrite_vrr("HDMI-A-1,3840x2160@120", 1), None);
    }

    #[test]
    fn line_accessors_read_the_example_config() {
        assert_eq!(line_name(HDR_LINE), "HDMI-A-1");
        assert_eq!(line_mode(HDR_LINE), Mode::parse("3840x2160@120"));
        assert_eq!(line_vrr(HDR_LINE), Some(1));
        // A keyword mode is not a `Mode` — the panel offers explicit modes only.
        assert_eq!(line_mode(",preferred,auto,auto"), None);
        assert_eq!(line_vrr("HDMI-A-1,3840x2160@120,0x0,2"), None);
    }

    #[test]
    fn synthesize_line_trims_the_scale() {
        let m = Mode::parse("1920x1080@60").unwrap();
        assert_eq!(
            synthesize_line("DP-1", m, 0, 0, 1.0),
            "DP-1,1920x1080@60,0x0,1"
        );
        assert_eq!(
            synthesize_line("DP-1", m, 1920, -100, 1.5),
            "DP-1,1920x1080@60,1920x-100,1.5"
        );
    }

    #[test]
    fn mode_options_dedupe_sort_and_mark_the_current_mode() {
        let available: Vec<String> = [
            "1920x1080@60.00000Hz",
            "3840x2160@60.00000Hz",
            "3840x2160@120.00000Hz",
            // Hyprland repeats near-identical rates; both round to 120.
            "3840x2160@119.99800Hz",
            "2560x1440@144.00000Hz",
            "garbage",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let opts = mode_options(&available, Mode::parse("3840x2160@120"));
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(
            values,
            vec![
                "3840x2160@120",
                "3840x2160@60",
                "2560x1440@144",
                "1920x1080@60"
            ]
        );
        assert_eq!(opts[0].label, "3840 × 2160 @ 120 Hz");
        assert!(opts[0].current);
        assert!(opts[1..].iter().all(|o| !o.current));
    }

    #[test]
    fn mode_options_on_an_output_that_reports_nothing_is_empty() {
        assert!(mode_options(&[], Mode::parse("3840x2160@120")).is_empty());
    }

    const CONF: &str = "\
# Per-machine display override
monitor=HDMI-A-1,3840x2160@120,0x0,2,vrr,1,cm,hdr  # 4K120 HDR over the AVR

quirks {
    prefer_hdr = 1
}
";

    #[test]
    fn finds_a_named_line_but_never_the_catch_all() {
        assert_eq!(find_monitor_line(CONF, "HDMI-A-1"), Some(1));
        assert_eq!(find_monitor_line(CONF, "DP-1"), None);
        // The generic default must never be rewritten in a named output's name.
        assert_eq!(
            find_monitor_line("monitor=,preferred,auto,auto\n", "HDMI-A-1"),
            None
        );
        assert_eq!(find_monitor_line(CONF, ""), None);
    }

    #[test]
    fn monitor_value_tolerates_spacing_and_indentation() {
        assert_eq!(
            find_monitor_line("  monitor = DP-1,1920x1080@60,0x0,1\n", "DP-1"),
            Some(0)
        );
        // A commented-out line declares nothing.
        assert_eq!(
            find_monitor_line("# monitor=DP-1,1920x1080@60,0x0,1\n", "DP-1"),
            None
        );
    }

    #[test]
    fn upsert_rewrites_one_line_and_keeps_its_comment_and_neighbours() {
        let out = upsert_monitor_line(CONF, "HDMI-A-1", "HDMI-A-1,1920x1080@60,0x0,2,vrr,0,cm,hdr");
        assert!(
            out.contains(
                "monitor=HDMI-A-1,1920x1080@60,0x0,2,vrr,0,cm,hdr  # 4K120 HDR over the AVR"
            ),
            "{out}"
        );
        assert!(
            out.contains("prefer_hdr = 1"),
            "neighbouring block lost: {out}"
        );
        assert!(
            out.contains("# Per-machine display override"),
            "leading comment lost: {out}"
        );
        // Exactly one monitor line, still.
        assert_eq!(
            out.lines()
                .filter(|l| l.trim_start().starts_with("monitor"))
                .count(),
            1
        );
    }

    #[test]
    fn upsert_appends_for_an_output_the_file_never_mentioned() {
        let out = upsert_monitor_line(CONF, "DP-1", "DP-1,1920x1080@60,0x0,1");
        assert!(out.contains("monitor=DP-1,1920x1080@60,0x0,1"), "{out}");
        // The pre-existing output is untouched.
        assert!(
            out.contains("monitor=HDMI-A-1,3840x2160@120,0x0,2,vrr,1,cm,hdr"),
            "{out}"
        );
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn upsert_into_an_empty_file_produces_a_valid_one() {
        let out = upsert_monitor_line("", "DP-1", "DP-1,1920x1080@60,0x0,1");
        assert_eq!(
            configured_line(&out, "DP-1").as_deref(),
            Some("DP-1,1920x1080@60,0x0,1")
        );
    }

    #[test]
    fn configured_line_round_trips_through_upsert() {
        let line = "HDMI-A-1,2560x1440@144,0x0,1,vrr,2,bitdepth,10";
        let out = upsert_monitor_line(CONF, "HDMI-A-1", line);
        assert_eq!(configured_line(&out, "HDMI-A-1").as_deref(), Some(line));
    }
}

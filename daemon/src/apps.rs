//! Installed-application discovery by scanning XDG `.desktop` entries.
//!
//! This replaces the inline `python3 -c` configparser scanner that
//! `components/AppDiscoveryManager.qml` used to shell out to. It is exposed over
//! IPC as the stateless `list-apps` command (see `docs/IPC_PROTOCOL.md`), whose
//! response body is a compact single-line JSON array of app objects.
//!
//! Deliberately platform-independent: parsing/filtering is driven by the pure
//! `freedesktop-desktop-entry` crate over in-memory `(path, contents)` pairs, so
//! the wire-critical behavior is unit-tested on macOS without touching real
//! `/usr/share` files. Only `scan_apps` (directory walking) touches the
//! filesystem, and it delegates every per-file decision to the pure helpers
//! here.
//!
//! Behavior mirrors the legacy Python scanner exactly:
//!   - scan `/usr/share/applications` then `~/.local/share/applications`
//!   - skip entries with `NoDisplay=true`, `Hidden=true`, or `Type != Application`
//!   - skip entries with an empty `Name`, and de-duplicate by `Name`
//!     (first occurrence wins, in directory+filename order)
//!   - strip the field codes `%u %U %f %F %i %c %k` from `Exec` and trim it
//!   - sort the final list by `name.to_lowercase()`

use freedesktop_desktop_entry::DesktopEntry;
use std::collections::HashSet;
use std::path::PathBuf;

/// One discovered application, matching the QML model shape
/// `{name, exec, icon, comment, wmClass}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct App {
    pub name: String,
    pub exec: String,
    pub icon: String,
    pub comment: String,
    #[serde(rename = "wmClass")]
    pub wm_class: String,
}

/// Field codes stripped from `Exec` before launching (mirrors the Python
/// `for tok in ['%u','%U','%f','%F','%i','%c','%k']: ex = ex.replace(tok,'')`).
const EXEC_FIELD_CODES: [&str; 7] = ["%u", "%U", "%f", "%F", "%i", "%c", "%k"];

/// Remove freedesktop field codes from an `Exec` line and trim, like the Python
/// scanner. Plain substring replacement (not whitespace-aware) to match exactly.
fn strip_exec_field_codes(exec: &str) -> String {
    let mut ex = exec.to_string();
    for tok in EXEC_FIELD_CODES {
        ex = ex.replace(tok, "");
    }
    ex.trim().to_string()
}

/// Parse one `.desktop` file's contents into an [`App`], applying the display
/// filters. Returns `None` when the entry should be skipped (`NoDisplay`,
/// `Hidden`, non-`Application` type, or empty `Name`).
///
/// `path` is only used by the parser to derive the appid; the contents drive
/// everything else, so this is fully testable from a string fixture.
pub fn parse_entry(path: impl Into<PathBuf>, contents: &str) -> Option<App> {
    let path = path.into();
    let entry = DesktopEntry::from_str::<&str>(path, contents, None).ok()?;

    // Type must be exactly "Application" (Python: fallback '' != 'Application').
    if entry.type_() != Some("Application") {
        return None;
    }
    if entry.no_display() || entry.hidden() {
        return None;
    }

    // Unlocalized Name (empty locales == Python configparser's plain `Name`).
    let name = entry.name::<&str>(&[])?.into_owned();
    if name.is_empty() {
        return None;
    }

    let exec = strip_exec_field_codes(entry.exec().unwrap_or(""));
    let icon = entry.icon().unwrap_or("").to_string();
    let comment = entry
        .comment::<&str>(&[])
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let wm_class = entry.startup_wm_class().unwrap_or("").to_string();

    Some(App {
        name,
        exec,
        icon,
        comment,
        wm_class,
    })
}

/// De-duplicate by `name` (first wins, preserving input order) then sort by
/// `name.to_lowercase()`. Input order is directory-then-filename, matching the
/// Python scanner's `for d in [...]: for f in sorted(os.listdir(d))`.
pub fn dedup_and_sort(mut apps: Vec<App>) -> Vec<App> {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert(a.name.clone()));
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// The XDG application directories to scan, in priority order.
///
/// Covers system + per-user app dirs **and** the Flatpak export dirs, so
/// Flatpak-installed apps (e.g. Plex HTPC) land on the home rail without
/// requiring `XDG_DATA_DIRS` to be set in the daemon's environment. We add the
/// Flatpak export paths explicitly rather than expanding the full
/// `XDG_DATA_DIRS` to keep the scanned set small and predictable.
pub fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/applications"),
        // System-wide Flatpak exports (e.g. `flatpak install --system`).
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/applications"));
        // Per-user Flatpak exports (e.g. `flatpak install --user`).
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs
}

// ---------------------------------------------------------------------------
// Icon resolvability (#441 follow-up)
// ---------------------------------------------------------------------------

/// Icon file extensions we index. Matches what `image://icon/` can actually
/// render from a theme directory.
const ICON_EXTENSIONS: [&str; 3] = ["svg", "png", "xpm"];

/// How deep to walk an icon root. Real theme layouts bottom out around
/// `<theme>/<context>/<size>/<name>.svg` (3) and `<theme>/<size>/<context>/`
/// variants; 6 covers those with room to spare while refusing to chase a
/// pathological symlink farm.
const ICON_WALK_MAX_DEPTH: usize = 6;

/// The standard icon roots, in no particular order — this is a flat existence
/// index, not a lookup path (see [`icon_is_resolvable`]).
pub fn icon_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/icons"),
        PathBuf::from("/usr/share/pixmaps"),
        // System-wide Flatpak exports, matching `app_dirs`.
        PathBuf::from("/var/lib/flatpak/exports/share/icons"),
    ];
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(data_home).join("icons"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".icons"));
        dirs.push(home.join(".local/share/icons"));
        dirs.push(home.join(".local/share/flatpak/exports/share/icons"));
    }
    dirs
}

/// Walk `dirs` and collect every icon **basename** (filename without its
/// extension) found underneath.
///
/// Deliberately flat: theme inheritance, `index.theme`, size buckets and
/// contexts are all ignored — see [`icon_is_resolvable`] for why that is the
/// point rather than a shortcut.
pub fn build_icon_index(dirs: &[PathBuf]) -> HashSet<String> {
    let mut index = HashSet::new();
    // Iterative BFS with an explicit depth so a symlink cycle can't recurse the
    // daemon into a stack overflow.
    let mut queue: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = queue.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            // `file_type()` does not follow symlinks; `is_dir()` on the path
            // does, which is what we want (themes symlink size buckets).
            if path.is_dir() {
                if depth < ICON_WALK_MAX_DEPTH {
                    queue.push((path, depth + 1));
                }
                continue;
            }
            let has_icon_ext = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|ext| ICON_EXTENSIONS.iter().any(|k| ext.eq_ignore_ascii_case(k)));
            if !has_icon_ext {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                index.insert(stem.to_owned());
            }
        }
    }
    index
}

/// The process-wide icon index, built lazily on first use.
///
/// **Cached for the life of the process on purpose.** `/usr/share/icons` on a
/// KDE box is tens of thousands of files, and `scan_apps` runs on every
/// `list-apps` IPC call — rebuilding per scan (let alone per app) would turn a
/// cheap request into a full filesystem walk. Installing a new icon theme
/// mid-session therefore needs a daemon restart before its icons count as
/// resolvable; that is an acceptable trade for a value that changes about as
/// often as the OS does.
fn icon_index() -> &'static HashSet<String> {
    static INDEX: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let index = build_icon_index(&icon_dirs());
        tracing::debug!("apps: indexed {} icon names", index.len());
        index
    })
}

/// Would `icon` resolve to *some* file on this system?
///
/// Pure over an injected `index` so the policy is unit-testable with no
/// filesystem, matching `count_live_quickshell` / `metrics::count_warning_lines`.
///
/// **Deliberately permissive, and it must stay that way.** A flat basename index
/// knows nothing about theme inheritance or `index.theme`, so it answers "yes"
/// for a name that exists only in a theme Qt will never search — `web-browser`,
/// present under `AdwaitaLegacy` but outside Breeze's inheritance chain, is
/// exactly such a name and is NOT caught here. That is intended: the only thing
/// this check may ever do is blank a name that exists **nowhere**, so it can
/// never blank a working icon. It is also why the generated web-app launcher
/// needed its own fix (`webapps::WEBAPP_ICON`) rather than relying on this.
///
/// If you are tempted to "improve" this into a real theme resolver: don't. A
/// resolver that models inheritance will start returning `false` for icons that
/// render fine, and the failure mode is silent — apps lose their icons and fall
/// back to letter tiles with nothing in the log to explain it.
pub fn icon_is_resolvable(icon: &str, index: &HashSet<String>) -> bool {
    // A path (absolute, or anything with a separator) is passed through
    // untouched: the spec allows `Icon=` to be a file path, and we deliberately
    // do not stat it — a network/removable path could block, and a wrong answer
    // here would blank a perfectly good icon.
    if icon.contains('/') {
        return true;
    }
    if index.contains(icon) {
        return true;
    }
    // Deprecated-but-real: `Icon=foo.png`. The index stores stems, so retry
    // without a known icon extension before giving up.
    std::path::Path::new(icon)
        .extension()
        .and_then(|x| x.to_str())
        .filter(|ext| ICON_EXTENSIONS.iter().any(|k| ext.eq_ignore_ascii_case(k)))
        .and_then(|_| std::path::Path::new(icon).file_stem())
        .and_then(|s| s.to_str())
        .is_some_and(|stem| index.contains(stem))
}

/// Blank an `Icon=` that resolves to nothing, so the shell renders its designed
/// letter-tile fallback instead of issuing a doomed `image://icon/` request.
///
/// Returns `icon` unchanged when it is empty (already the fallback) or
/// resolvable. See [`icon_is_resolvable`] for the conservatism contract.
pub fn resolve_icon(icon: &str, index: &HashSet<String>) -> String {
    if icon.is_empty() || icon_is_resolvable(icon, index) {
        icon.to_owned()
    } else {
        String::new()
    }
}

/// Scan the given directories for `.desktop` files, parse + filter each, then
/// de-duplicate and sort. Files that fail to read or parse are skipped.
pub fn scan_apps_in(dirs: &[PathBuf]) -> Vec<App> {
    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            continue;
        };
        // Sort filenames within a directory (Python: `sorted(os.listdir(d))`).
        let mut names: Vec<PathBuf> = read_dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("desktop"))
            .collect();
        names.sort();
        for path in names {
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(mut app) = parse_entry(&path, &contents) {
                // Drop an icon name that resolves to no file at all. A distro
                // entry with a dangling `Icon=` (e.g. lstopo's `hwloc`, which
                // ships no icon anywhere) would otherwise make the shell request
                // it on every restage and log a warning each time — 12 per cold
                // start, measured. `debug!`, never `warn!`: a third-party entry
                // with a broken reference is not ours to shout about, and log
                // volume is the whole point of this change.
                let resolved = resolve_icon(&app.icon, icon_index());
                if resolved.is_empty() && !app.icon.is_empty() {
                    tracing::debug!(
                        "apps: icon {:?} for {:?} resolves to no file; using the letter fallback",
                        app.icon,
                        app.name
                    );
                }
                app.icon = resolved;
                apps.push(app);
            }
        }
    }
    dedup_and_sort(apps)
}

/// Scan the standard XDG application directories.
pub fn scan_apps() -> Vec<App> {
    scan_apps_in(&app_dirs())
}

/// Serialize a list of apps as a compact single-line JSON array — the exact
/// `list-apps` IPC response body. Mirrors Python `json.dumps(apps)` (but with
/// compact separators, like the rest of this daemon's JSON bodies).
pub fn apps_to_json(apps: &[App]) -> String {
    serde_json::to_string(apps).expect("apps serialize")
}

/// Convenience: scan the standard directories and render the `list-apps`
/// response body.
pub fn list_apps_json() -> String {
    apps_to_json(&scan_apps())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, contents: &str) -> Option<App> {
        parse_entry(name, contents)
    }

    #[test]
    fn app_dirs_include_flatpak_export_dirs() {
        // Flatpak-installed apps export their .desktop into the flatpak
        // exports dirs; the home rail must scan those, not just the plain
        // XDG application dirs, or Flatpaks (e.g. Plex HTPC) never appear.
        let dirs = app_dirs();
        assert!(
            dirs.iter()
                .any(|d| d.ends_with("var/lib/flatpak/exports/share/applications")),
            "missing system flatpak exports dir: {dirs:?}"
        );
        assert!(
            dirs.iter()
                .any(|d| d.ends_with(".local/share/flatpak/exports/share/applications")),
            "missing user flatpak exports dir: {dirs:?}"
        );
        // Still scans the plain XDG dirs.
        assert!(dirs.iter().any(|d| d.ends_with("usr/share/applications")));
    }

    // ── Icon resolvability (#441 follow-up) ──────────────────────────────────

    fn index(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn absolute_and_relative_icon_paths_pass_through_untouched() {
        let idx = index(&[]);
        // A path is never blanked and never stat'ed: `Icon=` may legally be a
        // file path, and a wrong answer here would kill a working icon.
        assert_eq!(
            resolve_icon("/usr/share/pixmaps/thing.png", &idx),
            "/usr/share/pixmaps/thing.png"
        );
        assert_eq!(
            resolve_icon("./relative/thing.svg", &idx),
            "./relative/thing.svg"
        );
        assert!(icon_is_resolvable("/nonexistent/anywhere.png", &idx));
    }

    #[test]
    fn an_indexed_icon_name_survives() {
        let idx = index(&["firefox", "internet-web-browser"]);
        assert_eq!(resolve_icon("firefox", &idx), "firefox");
        assert_eq!(
            resolve_icon("internet-web-browser", &idx),
            "internet-web-browser"
        );
    }

    #[test]
    fn an_unindexed_icon_name_is_blanked() {
        // `hwloc` is the real case: /usr/share/applications/lstopo.desktop
        // references it and NO hwloc icon file exists anywhere on the system, so
        // every request is doomed and logs a warning. Blanking hands the shell
        // its designed letter-tile fallback instead.
        let idx = index(&["firefox"]);
        assert_eq!(resolve_icon("hwloc", &idx), "");
        assert!(!icon_is_resolvable("hwloc", &idx));
    }

    #[test]
    fn an_empty_icon_stays_empty() {
        assert_eq!(resolve_icon("", &index(&["firefox"])), "");
        assert_eq!(resolve_icon("", &index(&[])), "");
    }

    #[test]
    fn icon_lookup_is_case_sensitive_like_the_real_thing() {
        // Icon names resolve to real filenames, and the freedesktop lookup is
        // case-sensitive — `Firefox` genuinely does not find `firefox.svg`.
        // Asserted rather than assumed, since a "helpful" case-insensitive
        // match would make the index claim names that never render.
        let idx = index(&["firefox"]);
        assert!(icon_is_resolvable("firefox", &idx));
        assert!(!icon_is_resolvable("Firefox", &idx));
        assert_eq!(resolve_icon("FireFox", &idx), "");
    }

    #[test]
    fn a_name_carrying_an_icon_extension_matches_its_stem() {
        // Deprecated but real: `Icon=foo.png`. The index stores stems, so this
        // must not be blanked just because of the suffix.
        let idx = index(&["foo"]);
        assert!(icon_is_resolvable("foo.png", &idx));
        assert!(icon_is_resolvable("foo.svg", &idx));
        // A non-icon extension is not stripped — `foo.bar` is its own name.
        assert!(!icon_is_resolvable("foo.bar", &idx));
        // And the stem must actually be present.
        assert!(!icon_is_resolvable("nope.png", &idx));
    }

    #[test]
    fn the_check_is_permissive_by_design_not_a_theme_resolver() {
        // `web-browser` exists under AdwaitaLegacy, which is NOT in Breeze's
        // inheritance chain, so Qt cannot load it — yet a flat basename index
        // says "present" and we deliberately keep the icon. Blanking it would
        // require modelling theme inheritance, and such a resolver would start
        // silently blanking icons that render fine. This is exactly why the
        // generated web-app launcher needed its own fix (webapps::WEBAPP_ICON)
        // instead of relying on this check.
        let idx = index(&["web-browser"]);
        assert!(icon_is_resolvable("web-browser", &idx));
        assert_eq!(resolve_icon("web-browser", &idx), "web-browser");
    }

    #[test]
    fn build_icon_index_collects_stems_recursively() {
        // See `crate::testutil` for why this is based on `current_exe()`
        // rather than the system temp dir.
        let root = crate::testutil::scratch_dir("apps-icon-index-test");
        let nested = root.join("breeze/apps/48");
        std::fs::create_dir_all(&nested).unwrap();
        crate::testutil::harden_dir(&root.join("breeze"));
        crate::testutil::harden_dir(&root.join("breeze/apps"));
        crate::testutil::harden_dir(&nested);
        std::fs::write(nested.join("internet-web-browser.svg"), "<svg/>").unwrap();
        std::fs::write(root.join("legacy.png"), "").unwrap();
        std::fs::write(root.join("notes.txt"), "").unwrap();

        let idx = build_icon_index(std::slice::from_ref(&root));
        assert!(idx.contains("internet-web-browser"), "nested svg: {idx:?}");
        assert!(idx.contains("legacy"), "top-level png: {idx:?}");
        assert!(!idx.contains("notes"), "non-icon extension must not index");

        // A nonexistent root is skipped, not an error.
        assert!(build_icon_index(&[root.join("nope")]).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_a_basic_application() {
        let app = entry(
            "/usr/share/applications/firefox.desktop",
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Firefox\n\
             Exec=firefox %u\n\
             Icon=firefox\n\
             Comment=Browse the web\n\
             StartupWMClass=firefox\n",
        )
        .unwrap();
        assert_eq!(
            app,
            App {
                name: "Firefox".into(),
                exec: "firefox".into(),
                icon: "firefox".into(),
                comment: "Browse the web".into(),
                wm_class: "firefox".into(),
            }
        );
    }

    #[test]
    fn strips_all_field_codes_and_trims() {
        // Field codes are removed by plain substring replacement (like Python
        // `str.replace`), so the whitespace that surrounded them is left behind;
        // only the leading/trailing whitespace is trimmed. Here `%U %f %i ` and
        // ` %c` are removed, leaving "/bin/app " + "  " + "--flag" -> trimmed.
        let app = entry(
            "/x/a.desktop",
            "[Desktop Entry]\nType=Application\nName=A\nExec=/bin/app %U %f %i --flag %c\n",
        )
        .unwrap();
        assert_eq!(app.exec, "/bin/app    --flag");
        // Trailing field code + space is stripped down to just the program.
        assert_eq!(strip_exec_field_codes("run %F %k"), "run");
        assert_eq!(strip_exec_field_codes("%u%U%f%F%i%c%k"), "");
    }

    #[test]
    fn missing_optional_fields_default_to_empty() {
        let app = entry(
            "/x/b.desktop",
            "[Desktop Entry]\nType=Application\nName=Bare\n",
        )
        .unwrap();
        assert_eq!(app.exec, "");
        assert_eq!(app.icon, "");
        assert_eq!(app.comment, "");
        assert_eq!(app.wm_class, "");
    }

    #[test]
    fn skips_nodisplay_hidden_and_non_application() {
        assert!(entry(
            "/x/nd.desktop",
            "[Desktop Entry]\nType=Application\nName=ND\nNoDisplay=true\n"
        )
        .is_none());
        assert!(entry(
            "/x/h.desktop",
            "[Desktop Entry]\nType=Application\nName=H\nHidden=true\n"
        )
        .is_none());
        // Type=Link (not Application) -> skipped.
        assert!(entry(
            "/x/link.desktop",
            "[Desktop Entry]\nType=Link\nName=L\nURL=http://x\n"
        )
        .is_none());
        // Missing Type (Python fallback '') -> skipped.
        assert!(entry("/x/notype.desktop", "[Desktop Entry]\nName=NT\n").is_none());
    }

    #[test]
    fn keeps_when_nodisplay_is_false() {
        assert!(entry(
            "/x/keep.desktop",
            "[Desktop Entry]\nType=Application\nName=Keep\nNoDisplay=false\nHidden=false\n"
        )
        .is_some());
    }

    #[test]
    fn skips_empty_name() {
        assert!(entry(
            "/x/empty.desktop",
            "[Desktop Entry]\nType=Application\nName=\n"
        )
        .is_none());
        // No Name key at all -> also skipped.
        assert!(entry(
            "/x/noname.desktop",
            "[Desktop Entry]\nType=Application\nExec=x\n"
        )
        .is_none());
    }

    #[test]
    fn dedup_keeps_first_and_sorts_case_insensitively() {
        let apps = vec![
            App {
                name: "zed".into(),
                exec: "zed".into(),
                icon: String::new(),
                comment: String::new(),
                wm_class: String::new(),
            },
            App {
                name: "Apple".into(),
                exec: "apple1".into(),
                icon: String::new(),
                comment: String::new(),
                wm_class: String::new(),
            },
            // Duplicate name — must be dropped (first "Apple" wins).
            App {
                name: "Apple".into(),
                exec: "apple2".into(),
                icon: String::new(),
                comment: String::new(),
                wm_class: String::new(),
            },
            App {
                name: "banana".into(),
                exec: "banana".into(),
                icon: String::new(),
                comment: String::new(),
                wm_class: String::new(),
            },
        ];
        let out = dedup_and_sort(apps);
        let names: Vec<&str> = out.iter().map(|a| a.name.as_str()).collect();
        // Sorted case-insensitively: Apple, banana, zed. Dup Apple removed.
        assert_eq!(names, ["Apple", "banana", "zed"]);
        // First "Apple" wins -> exec is apple1.
        assert_eq!(out[0].exec, "apple1");
    }

    #[test]
    fn json_escapes_special_chars_in_desktop_fields() {
        // Characters that break hand-rolled JSON: backslash, double-quote,
        // control characters, and non-ASCII Unicode. serde_json must escape
        // all of these so the output round-trips through a JSON parser.
        //
        // This is the regression guard for the bug surfaced in issue #168:
        // a real /usr/share/applications file on the deploy host had a name or
        // comment field with a backslash/quote that broke the IPC response.
        let nasty = App {
            name: "My \"App\" with \\ backslash and \u{65e5}\u{672c}\u{8a9e}".into(),
            exec: "run --flag=\"val\" --path=C:\\foo".into(),
            icon: "".into(),
            comment: "Line1\nLine2\tTabbed\u{0008}backspace\u{000c}form-feed".into(),
            wm_class: "myapp".into(),
        };
        let json = apps_to_json(&[nasty]);
        // Must be parseable by serde_json without error.
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("json with special chars must be valid JSON");
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let obj = &arr[0];
        // Verify the name round-trips exactly (incl. the literal backslash,
        // double-quotes, and non-ASCII Unicode).
        assert_eq!(
            obj["name"].as_str().unwrap(),
            "My \"App\" with \\ backslash and \u{65e5}\u{672c}\u{8a9e}"
        );
        // Verify the exec round-trips exactly (Windows-style backslash path).
        assert_eq!(
            obj["exec"].as_str().unwrap(),
            "run --flag=\"val\" --path=C:\\foo"
        );
        // Must be a single line (no embedded newlines in the JSON envelope).
        assert!(
            !json.contains('\n'),
            "apps_to_json must produce a single line"
        );
        // The raw JSON must NOT contain unescaped double-quote inside a string
        // value (serde_json serialises " as ").
        // We can verify indirectly: the only way `from_str` succeeds above is
        // if serde_json correctly escaped all the special characters.
    }

    #[test]
    fn json_is_compact_single_line_array() {
        let apps = vec![App {
            name: "Firefox".into(),
            exec: "firefox".into(),
            icon: "firefox".into(),
            comment: "Web".into(),
            wm_class: "firefox".into(),
        }];
        let json = apps_to_json(&apps);
        assert!(!json.contains('\n'));
        assert!(!json.contains(": "));
        assert_eq!(
            json,
            r#"[{"name":"Firefox","exec":"firefox","icon":"firefox","comment":"Web","wmClass":"firefox"}]"#
        );
        // Empty list is an empty array.
        assert_eq!(apps_to_json(&[]), "[]");
    }
}

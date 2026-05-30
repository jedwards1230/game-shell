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

/// The XDG application directories to scan, in priority order. Mirrors the
/// Python list `['/usr/share/applications', '~/.local/share/applications']`
/// exactly (it intentionally does not expand the full `XDG_DATA_DIRS`).
pub fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs
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
            if let Some(app) = parse_entry(&path, &contents) {
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

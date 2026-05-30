//! Recently-launched-app tracking (`~/.local/share/game-shell/recents.json`).
//!
//! Replaces the two inline `python3 -c` processes that
//! `components/RecentsTracker.qml` used to read and write the recents file.
//! Exposed over IPC as `record-launch` and `get-recents` (see
//! `docs/IPC_PROTOCOL.md`).
//!
//! Platform-independent: the file format logic is pure read-modify-write over
//! strings (like `config.rs`), so it is unit-tested on any host. Only the thin
//! `record_launch`/`load_recents` wrappers touch the filesystem.
//!
//! Behavior mirrors the legacy Python writer/loader exactly:
//!   - newest entry first; an existing entry with the same `name` is removed
//!     before the new one is prepended (most-recent-wins de-dup by name)
//!   - the stored file is capped at `MAX_ENTRIES` (20) entries
//!   - the loader returns at most `LOAD_LIMIT` (15) entries
//!   - written single-line compact JSON (QML's `SplitParser` reads line-by-line)
//!   - each entry is `{name, exec, comment, time}` where `time` is unix seconds

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Cap on stored entries (Python: `d = d[:20]`).
pub const MAX_ENTRIES: usize = 20;
/// Cap on returned entries (Python loader: `data[:15]`).
pub const LOAD_LIMIT: usize = 15;

/// One recents entry, matching the QML model `{name, exec, comment, time}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub exec: String,
    #[serde(default)]
    pub comment: String,
    /// Unix seconds (float, matching Python `time.time()`).
    #[serde(default)]
    pub time: f64,
}

/// Default recents path: `~/.local/share/game-shell/recents.json`.
pub fn recents_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/share/game-shell/recents.json")
}

/// Parse the recents file contents into a list. Invalid/missing JSON yields an
/// empty list (mirrors the Python loader's bare `except: print('[]')`).
pub fn parse_recents(contents: &str) -> Vec<Recent> {
    serde_json::from_str(contents).unwrap_or_default()
}

/// Prepend a new launch onto the existing list: drop any existing entry with the
/// same `name`, insert the new one at the front, and cap at [`MAX_ENTRIES`].
/// Returns the new list (caller serializes it). Pure — `now` is injected so the
/// timestamp is testable. Port of the Python writer's list manipulation.
pub fn record(existing: Vec<Recent>, entry: Recent) -> Vec<Recent> {
    let mut out: Vec<Recent> = existing
        .into_iter()
        .filter(|e| e.name != entry.name)
        .collect();
    out.insert(0, entry);
    out.truncate(MAX_ENTRIES);
    out
}

/// Serialize recents as compact single-line JSON (QML requires single-line).
pub fn recents_to_json(recents: &[Recent]) -> String {
    serde_json::to_string(recents).expect("recents serialize")
}

/// The `get-recents` response body: the stored list truncated to [`LOAD_LIMIT`].
pub fn recents_response_json(recents: &[Recent]) -> String {
    let limited: Vec<&Recent> = recents.iter().take(LOAD_LIMIT).collect();
    serde_json::to_string(&limited).expect("recents serialize")
}

/// Read the recents file and return up to [`LOAD_LIMIT`] entries as a compact
/// JSON array (the `get-recents` response body). Missing/invalid file -> `[]`.
pub fn load_recents(path: &Path) -> String {
    let recents = std::fs::read_to_string(path)
        .ok()
        .map(|t| parse_recents(&t))
        .unwrap_or_default();
    recents_response_json(&recents)
}

/// Record a launch: read existing, prepend, cap, and write single-line. The
/// timestamp is supplied by the caller (unix seconds) so the file logic stays
/// pure/testable; the daemon passes the real wall-clock time.
pub fn record_launch(path: &Path, entry: Recent) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path)
        .ok()
        .map(|t| parse_recents(&t))
        .unwrap_or_default();
    let updated = record(existing, entry);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, recents_to_json(&updated))
}

/// Current wall-clock time in unix seconds (float), like Python `time.time()`.
pub fn now_unix_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, time: f64) -> Recent {
        Recent {
            name: name.into(),
            exec: format!("{}-exec", name.to_lowercase()),
            comment: String::new(),
            time,
        }
    }

    #[test]
    fn parse_missing_or_invalid_is_empty() {
        assert_eq!(parse_recents(""), vec![]);
        assert_eq!(parse_recents("not json"), vec![]);
        assert_eq!(parse_recents("{}"), vec![]);
    }

    #[test]
    fn parse_round_trips_entries() {
        let json = r#"[{"name":"A","exec":"a","comment":"c","time":1.5}]"#;
        let parsed = parse_recents(json);
        assert_eq!(
            parsed,
            vec![Recent {
                name: "A".into(),
                exec: "a".into(),
                comment: "c".into(),
                time: 1.5,
            }]
        );
    }

    #[test]
    fn record_prepends_and_dedups_by_name() {
        let existing = vec![r("A", 1.0), r("B", 2.0), r("C", 3.0)];
        // Relaunch B -> B moves to front, old B removed.
        let out = record(existing, r("B", 9.0));
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["B", "A", "C"]);
        assert_eq!(out[0].time, 9.0);
    }

    #[test]
    fn record_caps_at_max_entries() {
        let existing: Vec<Recent> = (0..MAX_ENTRIES)
            .map(|i| r(&format!("App{i}"), i as f64))
            .collect();
        let out = record(existing, r("New", 100.0));
        assert_eq!(out.len(), MAX_ENTRIES);
        assert_eq!(out[0].name, "New");
        // The oldest (App19) fell off the end.
        assert!(!out.iter().any(|e| e.name == "App19"));
    }

    #[test]
    fn response_limits_to_load_limit() {
        let recents: Vec<Recent> = (0..MAX_ENTRIES)
            .map(|i| r(&format!("App{i}"), i as f64))
            .collect();
        let json = recents_response_json(&recents);
        let parsed = parse_recents(&json);
        assert_eq!(parsed.len(), LOAD_LIMIT);
        assert_eq!(parsed[0].name, "App0");
        assert_eq!(
            parsed[LOAD_LIMIT - 1].name,
            format!("App{}", LOAD_LIMIT - 1)
        );
    }

    #[test]
    fn record_launch_then_load_round_trips_on_disk() {
        let path = std::env::temp_dir().join(format!(
            "gs-recents-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);

        // Missing file -> empty array.
        assert_eq!(load_recents(&path), "[]");

        record_launch(&path, r("Firefox", 1.0)).unwrap();
        record_launch(&path, r("Steam", 2.0)).unwrap();
        // Relaunch Firefox -> moves to front.
        record_launch(&path, r("Firefox", 3.0)).unwrap();

        let loaded = parse_recents(&load_recents(&path));
        let names: Vec<&str> = loaded.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Firefox", "Steam"]);
        assert_eq!(loaded[0].time, 3.0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn json_is_compact_single_line() {
        let json = recents_to_json(&[r("X", 1.0)]);
        assert!(!json.contains('\n'));
        assert!(!json.contains(": "));
        assert_eq!(
            json,
            r#"[{"name":"X","exec":"x-exec","comment":"","time":1.0}]"#
        );
        assert_eq!(recents_to_json(&[]), "[]");
    }
}

//! The one systemd-unit-state presentation helper the whole panel shares.
//!
//! Three pages render the same `systemctl is-active` string — Overview's
//! Units tile, System ▸ Services, and Dev ▸ Recovery's post-action chips —
//! and each used to carry its own verbatim copy of the mapping. One copy
//! means the three can no longer disagree about what `activating` looks like.

/// Map a raw `systemctl is-active` string to a colored dot class + a short
/// status word — color is always paired with explicit text (#6), never the
/// dot alone. `active` is the healthy state; `failed` is the one state that
/// reads as an outright problem; everything else (`inactive`, `activating`,
/// `deactivating`, `unknown`, ...) is a neutral "not running" state rather
/// than an alarm, since a stopped-but-not-failed unit isn't necessarily
/// wrong (e.g. between restarts).
///
/// Callers must render the returned dot and word **inside a single
/// `.unit-chip`** (`panel/assets/style.css`): the dot is an inline-block and
/// the word is ordinary text, so without `white-space: nowrap` around the
/// pair a narrow column breaks the line between them and leaves an orphan
/// dot at the end of the previous line.
pub fn unit_dot(state: &str) -> (&'static str, &'static str) {
    match state {
        "active" => ("dot-ok", "active"),
        "failed" => ("dot-error", "failed"),
        "activating" => ("dot-warn", "activating"),
        "deactivating" => ("dot-warn", "deactivating"),
        "inactive" => ("dot-neutral", "inactive"),
        _ => ("dot-neutral", "unknown"),
    }
}

/// The subset of `systemctl show` System ▸ Services renders, parsed out of the
/// `KEY=VALUE` lines [`crate::exec::Recovery::show_unit`] returns.
///
/// Every field is a plain `String` and every one may be empty: `systemctl
/// show` omits properties that do not apply to a unit type, and the page must
/// render a partial answer rather than an error when it does.
#[derive(Debug, Default, Clone)]
pub struct UnitStatus {
    /// `Id` — the unit's canonical name (may differ from what was asked for
    /// when the name was an alias, or be empty when the unit is not found).
    pub id: String,
    pub description: String,
    /// `loaded` / `not-found` / `masked` / `error`.
    pub load_state: String,
    /// `active` / `inactive` / `failed` / `activating` / `deactivating`.
    pub active_state: String,
    /// `running` / `exited` / `dead` / `failed` — the finer-grained state.
    pub sub_state: String,
    /// `enabled` / `disabled` / `static` / `masked` / `` (transient units).
    pub unit_file_state: String,
    /// `ActiveEnterTimestamp`, verbatim from systemd.
    pub active_since: String,
    /// `Result` — `success` unless the unit failed, then `exit-code`,
    /// `signal`, `timeout`, `oom-kill`, ...
    pub result: String,
    /// `StatusText` — the unit's own last status line, when it publishes one.
    pub status_text: String,
    /// `ExecMainStatus` — the main process's exit status.
    pub exec_main_status: String,
    /// `LoadError` — why the unit file could not be loaded, when it could not.
    pub load_error: String,
}

impl UnitStatus {
    /// Whether systemd knows this unit at all.
    pub fn found(&self) -> bool {
        !self.load_state.is_empty() && self.load_state != "not-found"
    }

    /// A one-line explanation of *why* a unit is failed, or an empty string
    /// when it is not failed (or when systemd offered no reason).
    ///
    /// Assembled rather than taken from one property because no single
    /// property carries it: `Result` says how it died, `ExecMainStatus` says
    /// with what, and `StatusText` is whatever the unit last published.
    pub fn failure_reason(&self) -> String {
        if !self.load_error.is_empty() && self.load_error != "\"\" \"\"" {
            return format!("load error: {}", self.load_error);
        }
        if self.active_state != "failed" {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        match self.result.as_str() {
            "" | "success" => {}
            other => parts.push(format!("result: {other}")),
        }
        if !self.exec_main_status.is_empty() && self.exec_main_status != "0" {
            parts.push(format!("exit status {}", self.exec_main_status));
        }
        if !self.status_text.is_empty() {
            parts.push(self.status_text.clone());
        }
        if parts.is_empty() {
            "failed (systemd reported no reason — check the journal)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Parse `systemctl show`'s `KEY=VALUE` output.
///
/// Lenient by construction: unknown keys are ignored, a line with no `=` is
/// skipped, and a value containing `=` keeps everything after the first one
/// (`Description=a=b` is a real possibility). Values are NOT unquoted —
/// systemd's own escaping is left visible rather than half-undone.
pub fn parse_show(raw: &str) -> UnitStatus {
    let mut status = UnitStatus::default();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "Id" => status.id = value,
            "Description" => status.description = value,
            "LoadState" => status.load_state = value,
            "ActiveState" => status.active_state = value,
            "SubState" => status.sub_state = value,
            "UnitFileState" => status.unit_file_state = value,
            "ActiveEnterTimestamp" => status.active_since = value,
            "Result" => status.result = value,
            "StatusText" => status.status_text = value,
            "ExecMainStatus" => status.exec_main_status = value,
            "LoadError" => status.load_error = value,
            _ => {}
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_show_reads_the_properties_the_page_renders() {
        let raw = "Id=sshd.service\nDescription=OpenSSH Daemon\nLoadState=loaded\n\
                   ActiveState=active\nSubState=running\nUnitFileState=enabled\n\
                   ActiveEnterTimestamp=Fri 2026-08-22 09:14:02 EDT\nResult=success\n\
                   StatusText=\nExecMainStatus=0\nLoadError=\"\" \"\"\n";
        let s = parse_show(raw);
        assert_eq!(s.id, "sshd.service");
        assert_eq!(s.active_state, "active");
        assert_eq!(s.unit_file_state, "enabled");
        assert_eq!(s.active_since, "Fri 2026-08-22 09:14:02 EDT");
        assert!(s.found());
        assert_eq!(s.failure_reason(), "");
    }

    #[test]
    fn parse_show_tolerates_a_missing_unit_and_junk_lines() {
        let s = parse_show("LoadState=not-found\nActiveState=inactive\nnot a property\n");
        assert!(!s.found());
        assert_eq!(s.unit_file_state, "");
    }

    #[test]
    fn a_failed_unit_reports_why() {
        let s = parse_show(
            "LoadState=loaded\nActiveState=failed\nSubState=failed\nResult=exit-code\n\
             ExecMainStatus=255\nStatusText=port already in use\n",
        );
        let reason = s.failure_reason();
        assert!(reason.contains("exit-code"), "{reason}");
        assert!(reason.contains("255"), "{reason}");
        assert!(reason.contains("port already in use"), "{reason}");
    }

    #[test]
    fn a_failed_unit_with_no_detail_still_says_something() {
        let s = parse_show("LoadState=loaded\nActiveState=failed\nResult=success\n");
        assert!(s.failure_reason().contains("no reason"));
    }

    #[test]
    fn unit_dot_maps_active_and_failed_to_distinct_colors() {
        assert_eq!(unit_dot("active"), ("dot-ok", "active"));
        assert_eq!(unit_dot("failed"), ("dot-error", "failed"));
        assert_eq!(unit_dot("activating"), ("dot-warn", "activating"));
        assert_eq!(unit_dot("deactivating"), ("dot-warn", "deactivating"));
        assert_eq!(unit_dot("inactive"), ("dot-neutral", "inactive"));
        assert_eq!(unit_dot("something-unexpected"), ("dot-neutral", "unknown"));
    }
}

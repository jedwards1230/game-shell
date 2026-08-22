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

#[cfg(test)]
mod tests {
    use super::*;

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

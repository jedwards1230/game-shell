//! `/shell/appearance` — how the TV UI looks: the `Appearance` slice of
//! `settings.json` (theme mode, the two auto-theme hours, reduce-motion, text
//! scale), and the **wallpaper** surface that writes `wallpaperPath`.
//!
//! One of the five pages the Settings page dissolved into (`docs/PANEL_IA.md`
//! phase 3); phase 4 folded the Media page's wallpaper half in here, which is
//! where it always belonged — the picker and the theme it is picked to sit
//! beside are one subject. Everything about the settings schema, the form
//! rendering and the scoped patch lives in [`crate::pages::settings`].
//!
//! ## Wallpapers
//!
//! Upload image files into `~/.config/tv-shell/wallpapers/`, preview them,
//! pick the active one (persisted as `wallpaperPath` via the daemon's
//! `set-config`), and delete. The shell's Settings ▸ Wallpaper page is a
//! read-only `FolderListModel` over that same directory, so it has no way to
//! get a file onto the box — this page is that missing half.
//!
//! **This page writes files.** These routes are behind the panel's auth layer
//! (`crate::auth`), but auth is opt-in via `[panel].token_file` and a loopback
//! panel may legitimately run without it, so the upload path is still treated
//! as an attack surface in its own right: extension allowlist, filename
//! sanitization, a re-checked containment test against the wallpapers dir, a
//! body-size cap, and magic-byte sniffing so a `.png` that isn't an image is
//! rejected. Reads back out (`/shell/appearance/wallpaper/file`) go through
//! the exact same resolver, so there is no arbitrary-filesystem-read endpoint.
//!
//! Degradation: with the daemon unreachable the page still returns 200 with a
//! clear banner and no form — never a 500. The wallpaper *files* still list;
//! only the daemon-backed selection is unavailable.

use askama::Template;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::capabilities::{CapabilitySnapshot, Chrome};
use crate::pages::settings::{self, GroupView};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The `SettingField::group`s this page owns — rendered as the form's
/// `__group` companions AND enforced server-side in [`save`], so this route
/// can only ever patch `Appearance`.
///
/// `wallpaperPath` is in this group as of phase 4 but is deliberately NOT a
/// typed input ([`settings::CUSTOM_EDITOR_KEYS`]): the grid below the form is
/// its editor. Being a `FieldKind::Str`, its absence from a patch leaves the
/// stored value untouched by the daemon's shallow merge.
const OWNED: &[&str] = &["Appearance"];

/// Upload cap. Generous for a 4K wallpaper, far below anything that would
/// wedge the box's memory.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Accepted wallpaper extensions — must stay in sync with the QML
/// `FolderListModel` nameFilters in `shell/settings/WallpaperSettings.qml`.
const ALLOWED_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];

// ---------------------------------------------------------------------------
// Paths, sanitization, containment  (pure — heavily unit-tested below)
// ---------------------------------------------------------------------------

/// `~/.config/tv-shell/wallpapers` — the directory the shell's Wallpaper page
/// reads. Derived from the same brand config dir the rest of the panel uses.
pub fn wallpapers_dir() -> PathBuf {
    tv_shell_protocol::brand::config_dir().join("wallpapers")
}

/// Reduce an untrusted upload filename to a safe, single-component basename.
///
/// Rejects (rather than silently rewrites) anything suspicious so a caller
/// can't smuggle a path: directory separators, `..`, absolute paths, empty or
/// dot-only names, control characters, and any extension outside
/// [`ALLOWED_EXTS`].
pub fn sanitize_filename(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("filename must not be empty".to_string());
    }
    if raw.contains('/') || raw.contains('\\') || raw.contains('\0') {
        return Err("filename must not contain path separators".to_string());
    }
    if raw.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return Err("filename must not contain control characters".to_string());
    }
    // `..`, `.`, and any name that isn't a plain file component.
    if raw == "." || raw == ".." || raw.starts_with('.') {
        return Err("filename must not start with a dot".to_string());
    }
    // Belt and braces: after Path parsing it must still be exactly one normal
    // component equal to the input.
    let mut comps = Path::new(raw).components();
    let only = comps.next();
    if comps.next().is_some() {
        return Err("filename must be a single path component".to_string());
    }
    match only {
        Some(std::path::Component::Normal(c)) if c == std::ffi::OsStr::new(raw) => {}
        _ => return Err("filename must be a simple file name".to_string()),
    }
    let ext = Path::new(raw)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_EXTS.contains(&ext.as_str()) {
        return Err(format!(
            "unsupported file type {ext:?} — allowed: {}",
            ALLOWED_EXTS.join(", ")
        ));
    }
    Ok(raw.to_string())
}

/// Resolve `name` inside `dir`, verifying the result really is contained by
/// `dir` after symlink resolution. Used by BOTH the write and the read path.
pub fn resolve_in_dir(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = sanitize_filename(name)?;
    let candidate = dir.join(&name);
    // The parent must canonicalize to the wallpapers dir itself; this catches a
    // symlinked entry pointing outside the directory.
    let dir_canon = dir
        .canonicalize()
        .map_err(|e| format!("wallpapers directory unavailable: {e}"))?;
    let parent_canon = candidate
        .parent()
        .ok_or_else(|| "invalid path".to_string())?
        .canonicalize()
        .map_err(|e| format!("wallpapers directory unavailable: {e}"))?;
    if parent_canon != dir_canon {
        return Err("resolved outside the wallpapers directory".to_string());
    }
    Ok(candidate)
}

/// Identify an image by magic bytes, so an executable renamed to `.png` is
/// refused. Returns the MIME type used when serving the file back.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    // A BMP file header is 14 bytes, so require at least that much before
    // trusting the two-byte "BM" signature — otherwise "BM" + garbage passes.
    if bytes.len() >= 14 && bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}

/// List wallpaper files currently on disk, sorted by name. A missing directory
/// is an empty list, never an error.
pub fn list_wallpapers(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| sanitize_filename(n).is_ok())
        .collect();
    names.sort_by_key(|n| n.to_ascii_lowercase());
    names
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

struct WallpaperView {
    name: String,
    selected: bool,
}

#[derive(Template)]
#[template(path = "appearance.html")]
struct AppearanceTemplate {
    chrome: Chrome,
    daemon_up: bool,
    /// One hidden `__group` input per entry — see [`settings::build_patch`].
    scope: Vec<&'static str>,
    groups: Vec<GroupView>,
    wallpapers: Vec<WallpaperView>,
    wallpapers_dir: String,
    any_selected: bool,
    max_upload_mb: usize,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    render(&state.caps, state.node.get_config().await.ok().as_ref())
}

fn render(caps: &CapabilitySnapshot, cfg: Option<&Value>) -> String {
    let dir = wallpapers_dir();
    let selected = cfg
        .and_then(|c| c.get("wallpaperPath").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let wallpapers: Vec<WallpaperView> = list_wallpapers(&dir)
        .into_iter()
        .map(|name| {
            let full = dir.join(&name);
            WallpaperView {
                selected: !selected.is_empty() && Path::new(&selected) == full,
                name,
            }
        })
        .collect();

    let tmpl = AppearanceTemplate {
        chrome: Chrome::new(caps, "shell.appearance"),
        daemon_up: cfg.is_some(),
        scope: OWNED.to_vec(),
        groups: cfg
            .map(|c| settings::build_groups(c, OWNED))
            .unwrap_or_default(),
        any_selected: wallpapers.iter().any(|w| w.selected),
        wallpapers,
        wallpapers_dir: dir.display().to_string(),
        max_upload_mb: MAX_UPLOAD_BYTES / (1024 * 1024),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /shell/appearance/save` — the `Appearance` group only.
///
/// The extractor is a `Vec` of pairs rather than a map because the form emits
/// one `__group` companion per group it owns and a map would collapse them.
pub async fn save(
    State(state): State<SharedState>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    Html(render_save(&state, &pairs).await)
}

pub async fn render_save(state: &AppState, pairs: &[(String, String)]) -> String {
    settings::render_save(state, OWNED, pairs).await
}

// ---------------------------------------------------------------------------
// Wallpapers — upload / select / delete / serve
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "action_result.html")]
struct ActionResultTemplate {
    ok: bool,
    message: String,
}

fn result_html(ok: bool, message: &str) -> String {
    let tmpl = ActionResultTemplate {
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// htmx result + an out-of-band refresh of the wallpaper grid, so a successful
/// upload/delete/select updates the page without a full reload.
fn result_with_refresh(ok: bool, message: &str, refreshed: String) -> String {
    format!("{}{}", result_html(ok, message), refreshed)
}

/// `POST /shell/appearance/wallpaper/upload` — multipart upload of one or
/// more images.
pub async fn upload(State(state): State<SharedState>, multipart: Multipart) -> impl IntoResponse {
    Html(render_upload(&state, multipart).await)
}

pub async fn render_upload(state: &AppState, mut multipart: Multipart) -> String {
    let dir = wallpapers_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return result_html(false, &format!("Could not create {}: {e}", dir.display()));
    }
    let mut saved: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                errors.push(format!("upload stream error: {e}"));
                break;
            }
        };
        let Some(raw_name) = field.file_name().map(str::to_string) else {
            continue; // not a file part
        };
        if raw_name.trim().is_empty() {
            continue; // empty file input
        }
        let name = match sanitize_filename(&raw_name) {
            Ok(n) => n,
            Err(e) => {
                errors.push(format!("{raw_name:?}: {e}"));
                continue;
            }
        };
        let bytes = match field.bytes().await {
            Ok(b) => b,
            Err(e) => {
                errors.push(format!("{name}: could not read upload ({e})"));
                continue;
            }
        };
        if bytes.is_empty() {
            errors.push(format!("{name}: file is empty"));
            continue;
        }
        if bytes.len() > MAX_UPLOAD_BYTES {
            errors.push(format!(
                "{name}: larger than the {} MB limit",
                MAX_UPLOAD_BYTES / (1024 * 1024)
            ));
            continue;
        }
        if sniff_image(&bytes).is_none() {
            errors.push(format!("{name}: not a recognized image file"));
            continue;
        }
        // Re-resolve through the containment check before writing.
        let path = match resolve_in_dir(&dir, &name) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        match std::fs::write(&path, &bytes) {
            Ok(()) => saved.push(name),
            Err(e) => errors.push(format!("{name}: write failed ({e})")),
        }
    }

    let refreshed = render_wallpaper_list_oob(state).await;
    if saved.is_empty() {
        let msg = if errors.is_empty() {
            "No file selected.".to_string()
        } else {
            format!("Nothing uploaded. {}", errors.join("; "))
        };
        return result_with_refresh(false, &msg, refreshed);
    }
    let mut msg = format!("Uploaded {}.", saved.join(", "));
    if !errors.is_empty() {
        msg.push_str(&format!(" Skipped: {}", errors.join("; ")));
    }
    result_with_refresh(true, &msg, refreshed)
}

#[derive(Deserialize)]
pub struct NameForm {
    name: String,
}

/// `POST /shell/appearance/wallpaper/select` — set `wallpaperPath`. An empty
/// name selects "None" (clears the wallpaper).
pub async fn select(
    State(state): State<SharedState>,
    Form(form): Form<NameForm>,
) -> impl IntoResponse {
    Html(render_select(&state, &form.name).await)
}

pub async fn render_select(state: &AppState, name: &str) -> String {
    let dir = wallpapers_dir();
    let value = if name.trim().is_empty() {
        String::new()
    } else {
        match resolve_in_dir(&dir, name) {
            Ok(p) => p.display().to_string(),
            Err(e) => return result_html(false, &format!("Not selected: {e}")),
        }
    };
    match state
        .node
        .set_config(&json!({ "wallpaperPath": value }))
        .await
    {
        Ok(()) => {
            let msg = if value.is_empty() {
                "Wallpaper cleared.".to_string()
            } else {
                format!("Wallpaper set to {name}.")
            };
            let refreshed = render_wallpaper_list_oob(state).await;
            result_with_refresh(true, &msg, refreshed)
        }
        Err(e) => result_html(false, &format!("Could not set the wallpaper: {e}")),
    }
}

/// `POST /shell/appearance/wallpaper/delete` — remove a wallpaper file. If it
/// was the selected one, `wallpaperPath` is cleared too so the shell doesn't
/// point at a file that no longer exists.
pub async fn delete(
    State(state): State<SharedState>,
    Form(form): Form<NameForm>,
) -> impl IntoResponse {
    Html(render_delete(&state, &form.name).await)
}

pub async fn render_delete(state: &AppState, name: &str) -> String {
    let dir = wallpapers_dir();
    let path = match resolve_in_dir(&dir, name) {
        Ok(p) => p,
        Err(e) => return result_html(false, &format!("Not deleted: {e}")),
    };
    if let Err(e) = std::fs::remove_file(&path) {
        return result_html(false, &format!("Could not delete {name}: {e}"));
    }
    // Clear the selection when the deleted file was the active wallpaper.
    let was_selected = state
        .node
        .get_config()
        .await
        .ok()
        .and_then(|c| {
            c.get("wallpaperPath")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .map(|sel| Path::new(&sel) == path)
        .unwrap_or(false);
    let mut msg = format!("Deleted {name}.");
    if was_selected {
        match state.node.set_config(&json!({ "wallpaperPath": "" })).await {
            Ok(()) => msg.push_str(" It was the active wallpaper, so the selection was cleared."),
            Err(e) => msg.push_str(&format!(
                " WARNING: it was the active wallpaper but clearing the selection failed: {e}"
            )),
        }
    }
    let refreshed = render_wallpaper_list_oob(state).await;
    result_with_refresh(true, &msg, refreshed)
}

#[derive(Deserialize)]
pub struct FileQuery {
    name: String,
}

/// `GET /shell/appearance/wallpaper/file?name=…` — serve a wallpaper's bytes
/// for the preview thumbnails. Goes through the SAME resolver as every write
/// path, and re-sniffs the content, so this can never become an arbitrary file
/// read.
pub async fn file(Query(q): Query<FileQuery>) -> Response {
    let dir = wallpapers_dir();
    let path = match resolve_in_dir(&dir, &q.name) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid name").into_response(),
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let Some(mime) = sniff_image(&bytes) else {
        return (StatusCode::BAD_REQUEST, "not an image").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response()
}

/// The wallpaper list as an out-of-band htmx swap, so actions refresh the grid
/// in place.
///
/// Re-renders THIS page and string-slices the list section out of it between
/// the two HTML comment markers `appearance.html` carries — simpler and less
/// drift-prone than maintaining a second template for the same markup, at the
/// cost of a coupling to those markers that
/// `crate::tests::wallpaper_oob_refresh_returns_the_list_fragment` pins.
/// `pub` for that test: a silently-empty fragment is the exact failure mode.
pub async fn render_wallpaper_list_oob(state: &AppState) -> String {
    let inner = render_page(state).await;
    match (
        inner.find("<!--wallpaper-list-start-->"),
        inner.find("<!--wallpaper-list-end-->"),
    ) {
        (Some(a), Some(b)) if b > a => format!(
            r#"<div id="wallpaper-list" hx-swap-oob="innerHTML">{}</div>"#,
            &inner[a..b]
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_rejects_traversal_and_junk() {
        assert_eq!(sanitize_filename("wall.png").unwrap(), "wall.png");
        assert_eq!(
            sanitize_filename("  My Photo.JPG  ").unwrap(),
            "My Photo.JPG"
        );
        for bad in [
            "",
            "   ",
            "../../etc/passwd",
            "foo/../../bar.png",
            "/etc/passwd",
            "/abs/path.png",
            "sub/dir.png",
            "back\\slash.png",
            ".",
            "..",
            ".hidden.png",
            "no-extension",
            "script.sh",
            "payload.png.sh",
            "evil\u{0}.png",
            "new\nline.png",
        ] {
            assert!(sanitize_filename(bad).is_err(), "should reject {bad:?}");
        }
        // Extension allowlist is case-insensitive.
        for ok in ["a.png", "a.PNG", "a.jpeg", "a.webp", "a.bmp"] {
            assert!(sanitize_filename(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn sniff_image_identifies_real_formats_only() {
        assert_eq!(
            sniff_image(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]),
            Some("image/png")
        );
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image(&webp), Some("image/webp"));
        assert_eq!(sniff_image(b"BM123456789012"), Some("image/bmp"));
        // "BM" + too few bytes to be a real BMP header is not an image.
        assert_eq!(sniff_image(b"BM123456"), None);
        // Not images: a script, an ELF, empty, and a truncated RIFF.
        assert_eq!(sniff_image(b"#!/bin/sh\nrm -rf /"), None);
        assert_eq!(sniff_image(&[0x7f, b'E', b'L', b'F']), None);
        assert_eq!(sniff_image(b""), None);
        assert_eq!(sniff_image(b"RIFF1234"), None);
    }

    fn scratch(tag: &str) -> PathBuf {
        let base = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap();
        let dir = base.join(format!("wallpaper-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_in_dir_contains_paths() {
        let dir = scratch("resolve");
        let ok = resolve_in_dir(&dir, "a.png").unwrap();
        assert_eq!(
            ok.parent().unwrap().canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        for bad in ["../a.png", "../../a.png", "/etc/a.png", "b/a.png"] {
            assert!(resolve_in_dir(&dir, bad).is_err(), "should reject {bad:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_wallpapers_filters_and_sorts() {
        let dir = scratch("list");
        for f in ["b.png", "A.jpg", "notes.txt", ".hidden.png"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::create_dir_all(dir.join("subdir.png")).unwrap();
        let got = list_wallpapers(&dir);
        // Only the two real image files, case-insensitively sorted; the .txt,
        // the dotfile, and the directory are all excluded.
        assert_eq!(got, vec!["A.jpg".to_string(), "b.png".to_string()]);
        // A missing directory is an empty list, not an error.
        assert!(list_wallpapers(&dir.join("nope")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

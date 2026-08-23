//! Hermetic handler-level tests: build an [`AppState`] whose IPC client
//! points at a non-existent socket and whose bridge has no base URL, then
//! exercise the internal `render_*` functions the axum handlers wrap
//! (rather than the handlers/extractors directly). Asserts they degrade
//! gracefully — non-empty HTML with the expected degraded markers, never a
//! panic — with no real daemon or network involved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use crate::bridge::BridgeClient;
use crate::config::AppConfig;
use crate::exec::Recovery;
use crate::ipc::IpcTransport;
use crate::pages;
use crate::state::AppState;
use tv_shell_protocol::Feature;

fn hermetic_state() -> Arc<AppState> {
    // `/tmp` directly (short and stable): the socket is never bound here —
    // only connected-to, and the connection is expected to fail — but a
    // short path keeps this consistent with the `ipc` module's own tests.
    let sock = std::path::PathBuf::from(format!(
        "/tmp/tvshp-hermetic-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ));
    Arc::new(AppState {
        cfg: AppConfig::default(),
        // Capabilities are resolved at STARTUP, so "the daemon is unreachable
        // right now" and "the node declared nothing" are different states. The
        // page-render tests below are about the former; a fully-capable
        // snapshot keeps every section rendered so an assertion can't pass by
        // the section being gated away.
        caps: CapabilitySnapshot::fully_capable(),
        node: Arc::new(IpcTransport::new(sock)),
        bridge: Arc::new(BridgeClient::new(None, None)),
        recovery: Recovery::new(),
        updates: crate::updates::UpdatesState::default(),
    })
}

#[tokio::test]
async fn dashboard_tiles_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::dashboard::render_tiles(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.contains("/dev"),
        "degraded dashboard must link to /dev for recovery: {html}"
    );
    assert!(
        html.to_lowercase().contains("unreachable"),
        "degraded dashboard must show an unreachable marker: {html}"
    );
}

#[tokio::test]
async fn logs_view_degrades_when_bridge_and_daemon_absent() {
    let state = hermetic_state();
    let html = pages::logs::render_view(&state, 50, None, false).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("bridge"),
        "log view must mention the unavailable HTTP bridge: {html}"
    );
}

#[tokio::test]
async fn dev_page_renders_with_daemon_down_banner() {
    let state = hermetic_state();
    let html = pages::dev::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.contains("down"),
        "dev page must show the daemon as down when unreachable: {html}"
    );
}

// ---------------------------------------------------------------------------
// UI-polish pass: status humanizer, nav daemon dot, OOB refreshes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dashboard_tiles_humanizes_status_token() {
    let mut replies = HashMap::new();
    replies.insert("status", "connected:grabbed");
    let sock = spawn_canned_daemon("dashboard-status-humanize", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::dashboard::render_tiles(&state).await;
    assert!(
        html.contains("Connected · grabbed"),
        "expected the humanized status label: {html}"
    );
    assert!(
        html.contains("connected:grabbed"),
        "expected the raw token to remain visible for debugging: {html}"
    );
}

#[tokio::test]
async fn controllers_fleet_humanizes_status_token() {
    let mut replies = HashMap::new();
    replies.insert("status", "disconnected:grabbed");
    replies.insert("get-pads", "[]");
    replies.insert("get-bindings", "{}");
    replies.insert("get-config", "{}");
    replies.insert("controllerdb-status", "{}");
    let sock = spawn_canned_daemon("controllers-fleet-humanize", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::controllers::render_page(&state).await;
    assert!(
        html.contains("No controllers connected · grab armed"),
        "expected the humanized fleet status label: {html}"
    );
}

#[tokio::test]
async fn controllers_grab_includes_oob_fleet_refresh() {
    let mut replies = HashMap::new();
    replies.insert("grab", "ok");
    replies.insert("status", "connected:grabbed");
    replies.insert("get-pads", "[]");
    let sock = spawn_canned_daemon("controllers-grab-oob", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::controllers::render_grab(&state).await;
    assert!(
        html.contains(r#"id="controllers-fleet""#) && html.contains(r#"hx-swap-oob="true""#),
        "expected an out-of-band fleet refresh bolted onto the grab response: {html}"
    );
}

#[tokio::test]
async fn cec_active_source_includes_oob_health_refresh() {
    let mut replies = HashMap::new();
    replies.insert("cec-active-source", "ok");
    replies.insert(
        "cec-health",
        r#"{"transmit":"ok","reason":null,"since":1719500000000,"lastError":null}"#,
    );
    let sock = spawn_canned_daemon("cec-active-source-oob", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::cec::render_active_source(&state).await;
    assert!(
        html.contains(r#"id="cec-health""#) && html.contains(r#"hx-swap-oob="true""#),
        "expected an out-of-band health refresh bolted onto the active-source response: {html}"
    );
}

#[tokio::test]
async fn nav_dot_shows_ok_when_daemon_reachable() {
    let mut replies = HashMap::new();
    replies.insert("status", "connected:grabbed");
    let sock = spawn_canned_daemon("nav-dot-ok", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::nav::render_dot(&state).await;
    assert!(
        html.contains("dot-ok"),
        "expected a green dot when the daemon answers: {html}"
    );
}

#[tokio::test]
async fn nav_dot_shows_error_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::nav::render_dot(&state).await;
    assert!(
        html.contains("dot-error"),
        "expected a red dot when the daemon is unreachable: {html}"
    );
}

/// A minimal multi-connection fake daemon for `get-config`/`set-config`
/// round-trip tests. Unlike `ipc`'s private one-shot `spawn_fake_daemon`
/// (good for a single `IpcTransport::command` call), the real Settings/Widgets
/// flows make TWO separate connections per page load or save (each
/// `IpcTransport` request opens its own connection — see `ipc.rs`'s doc
/// comment), so this helper loops accepting connections indefinitely rather
/// than closing after one.
///
/// Replies:
/// - `get-config` → `canned_get_config` verbatim (the fixed document tests
///   assert against).
/// - `set-config <json>` → records the raw JSON body (everything after the
///   first space) into the returned `Arc<Mutex<Vec<String>>>`, in receipt
///   order, and replies `ok`.
/// - anything else → `error:unknown command` (shouldn't be hit by these
///   tests, but avoids a silent hang if it is).
///
/// Reusable as-is by the Widgets-page implementer for its own
/// `widgets`-subtree round-trip tests — just spawn it and point a fresh
/// `IpcTransport` at the returned socket path.
pub fn spawn_config_daemon(
    name: &str,
    canned_get_config: &'static str,
) -> (std::path::PathBuf, Arc<Mutex<Vec<String>>>) {
    let sock = std::path::PathBuf::from(format!(
        "/tmp/tvshp-cfgd-{name}-{}-{}.sock",
        std::process::id(),
        config_daemon_uniquifier()
    ));
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind fake config daemon socket");
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let received_for_task = Arc::clone(&received);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let received = Arc::clone(&received_for_task);
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line == "get-config" {
                    let _ = write_half
                        .write_all(format!("{canned_get_config}\n").as_bytes())
                        .await;
                } else if let Some(body) = line.strip_prefix("set-config ") {
                    received.lock().unwrap().push(body.to_string());
                    let _ = write_half.write_all(b"ok\n").await;
                } else {
                    let _ = write_half.write_all(b"error:unknown command\n").await;
                }
            });
        }
    });
    (sock, received)
}

/// Like [`spawn_config_daemon`], but records the FULL command line of every
/// request (not just `set-config` bodies) and answers each with `canned_reply`.
/// Use it to assert the exact wire text a page sends for non-config commands.
pub fn spawn_recording_daemon(
    name: &str,
    canned_reply: &'static str,
) -> (std::path::PathBuf, Arc<Mutex<Vec<String>>>) {
    let sock = std::path::PathBuf::from(format!(
        "/tmp/tvshp-recd-{name}-{}-{}.sock",
        std::process::id(),
        config_daemon_uniquifier()
    ));
    let _ = std::fs::remove_file(&sock);
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(&sock).expect("bind recording daemon socket");
    let recorded = Arc::clone(&received);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut lines = BufReader::new(read_half).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    recorded.lock().unwrap().push(line.clone());
                    let _ = write_half
                        .write_all(format!("{canned_reply}\n").as_bytes())
                        .await;
                }
            });
        }
    });
    (sock, received)
}

fn config_daemon_uniquifier() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Like [`spawn_config_daemon`], but answers an arbitrary map of exact
/// request-line → reply-line pairs instead of only understanding
/// `get-config`/`set-config`. Used by the Tools console's round-trip tests,
/// which exercise many distinct IPC commands against one fake daemon.
/// Requests not present in `replies` get `error:unknown command`.
pub fn spawn_canned_daemon(
    name: &str,
    replies: std::collections::HashMap<&'static str, &'static str>,
) -> std::path::PathBuf {
    let sock = std::path::PathBuf::from(format!(
        "/tmp/tvshp-canned-{name}-{}-{}.sock",
        std::process::id(),
        config_daemon_uniquifier()
    ));
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind fake canned daemon socket");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let replies = replies.clone();
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                let reply = replies
                    .get(line)
                    .copied()
                    .unwrap_or("error:unknown command");
                let _ = write_half.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    sock
}

fn state_for_socket(sock: std::path::PathBuf) -> Arc<AppState> {
    state_for_socket_with_caps(sock, CapabilitySnapshot::fully_capable())
}

/// A state wired to a real (canned) daemon socket AND a chosen startup
/// snapshot — the two are independent, which is the whole point: "the daemon
/// answers right now" and "the node declared this at startup" are different
/// facts, and a page that conflates them renders links to routes that were
/// never registered.
fn state_for_socket_with_caps(sock: std::path::PathBuf, caps: CapabilitySnapshot) -> Arc<AppState> {
    Arc::new(AppState {
        cfg: AppConfig::default(),
        caps,
        node: Arc::new(IpcTransport::new(sock)),
        bridge: Arc::new(BridgeClient::new(None, None)),
        recovery: Recovery::new(),
        updates: crate::updates::UpdatesState::default(),
    })
}

/// Form fields as a browser posts them — an ordered list of pairs, so the
/// repeated `__group` companions survive (a map would keep only the last).
fn form(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// The one `set-config` line a save is expected to have produced, parsed.
fn only_patch(received: &Arc<std::sync::Mutex<Vec<String>>>) -> serde_json::Value {
    let sent = received.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one set-config call: {sent:?}"
    );
    serde_json::from_str(&sent[0]).unwrap()
}

#[tokio::test]
async fn appearance_page_renders_current_config() {
    let (sock, _received) = spawn_config_daemon(
        "appearance-page",
        r#"{"themeMode":"light","rumbleEnabled":false}"#,
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::appearance::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.contains("light"),
        "the Appearance page must render the current themeMode value: {html}"
    );
    assert!(
        html.contains(r#"name="__group" value="Appearance""#),
        "the form must declare the schema group it owns, or its save would \
         patch every group: {html}"
    );
    assert!(
        !html.contains(r#"name="hdrEnabled""#),
        "hdrEnabled belongs to Display & Audio and must not render here: {html}"
    );
}

/// **The scoped-save property, and the reason it exists.** Splitting one form
/// into five means an unscoped patch would write `false` to every `Bool` the
/// submitting page does not even render — silent data loss across four other
/// pages. A save must touch only the groups its form declared.
#[tokio::test]
async fn appearance_save_patches_only_the_appearance_group() {
    let (sock, received) = spawn_config_daemon("appearance-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    // `reduceMotion` is deliberately absent: an unchecked box WITHIN the
    // submitted group must still be written as explicit `false`.
    let pairs = form(&[
        ("__group", "Appearance"),
        ("themeMode", "light"),
        ("autoThemeDarkStart", "21"),
        ("textScale", "1.25"),
    ]);
    let html = pages::appearance::render_save(&state, &pairs).await;
    assert!(
        html.to_lowercase().contains("saved"),
        "expected ok result: {html}"
    );

    let patch = only_patch(&received);
    assert_eq!(patch["themeMode"], "light");
    assert_eq!(patch["autoThemeDarkStart"], 21);
    assert_eq!(
        patch["reduceMotion"], false,
        "an unchecked box inside the submitted group is still an explicit \
         false — that behaviour predates the split and must survive: {patch}"
    );
    for other in [
        "hdrEnabled",        // Display
        "nightLightEnabled", // Night Light
        "wakeOnController",  // Power
        "defaultSink",       // Audio
        "controllerDebug",   // Input
        "rumbleEnabled",     // Input
        "cecFocusOnWake",    // CEC
        "prewarmApps",       // Apps
    ] {
        assert!(
            patch.get(other).is_none(),
            "{other} belongs to another page's group and must be ABSENT from an \
             Appearance patch, not written false: {patch}"
        );
    }
    // The daemon-owned layers and the Complex keys stay out, as before.
    for never in [
        "webApps",
        "keyBindings",
        "perGameBindings",
        "perPlayerBindings",
        "widgets",
    ] {
        assert!(
            patch.get(never).is_none(),
            "{never} must never appear in a typed save patch: {patch}"
        );
    }
}

/// **`wallpaperPath` has one editor, and the typed form is not it.**
///
/// Phase 4 moved the key from the `Display` group (where it rendered as a raw
/// path text field on a page that cannot browse the wallpapers dir) into
/// `Appearance`, whose wallpaper grid writes it — and excluded it from the
/// rendered form via `settings::CUSTOM_EDITOR_KEYS`.
///
/// Omitting a field from a form omits it from the patch, which is only safe
/// because it is a `FieldKind::Str`: non-`Bool` kinds are written only when
/// present, and the daemon's shallow merge leaves an unmentioned key alone. So
/// an Appearance save must leave the stored wallpaper selection untouched
/// rather than clearing it — that is the data-loss question this pins.
#[tokio::test]
async fn an_appearance_save_never_touches_the_wallpaper_selection() {
    let (sock, received) =
        spawn_config_daemon("wallpaper-scope", r#"{"wallpaperPath":"/w/a.png"}"#);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::appearance::render_page(&state).await;
    assert!(
        !html.contains(r#"name="wallpaperPath""#),
        "the grid is wallpaperPath's editor — a raw path input beside it would \
         be a second, worse one that bypasses the containment checks: {html}"
    );

    let pairs = form(&[("__group", "Appearance"), ("themeMode", "light")]);
    let _ = pages::appearance::render_save(&state, &pairs).await;
    let patch = only_patch(&received);
    assert!(
        patch.get("wallpaperPath").is_none(),
        "wallpaperPath must be ABSENT from the patch, so the shallow merge \
         leaves the current selection alone: {patch}"
    );

    // It is still in the schema (Advanced's raw hatch and the picker both rely
    // on that), and in the group whose page owns its editor.
    let field = crate::pages::settings::SCHEMA
        .iter()
        .find(|f| f.key == "wallpaperPath")
        .expect("wallpaperPath stays in the schema");
    assert_eq!(field.group, "Appearance");
    assert!(
        matches!(field.kind, crate::pages::settings::FieldKind::Str),
        "the omission-is-safe argument holds only for a non-Bool kind"
    );
    assert!(crate::pages::settings::CUSTOM_EDITOR_KEYS.contains(&"wallpaperPath"));
}

/// The picker writes the key the typed form no longer renders — the other half
/// of the test above. Without this, "absent from the patch" could equally
/// describe a key nothing writes at all.
#[tokio::test]
async fn the_wallpaper_picker_is_what_writes_wallpaper_path() {
    let (sock, received) = spawn_config_daemon("wallpaper-select", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::appearance::render_select(&state, "").await;
    assert!(
        html.to_lowercase().contains("cleared"),
        "an empty name is the None tile — it clears the selection: {html}"
    );
    let patch = only_patch(&received);
    assert_eq!(
        patch["wallpaperPath"], "",
        "the picker is the writer of this key: {patch}"
    );
}

#[tokio::test]
async fn apps_save_patches_only_the_apps_group() {
    let (sock, received) = spawn_config_daemon("apps-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    // StrList textarea: blank + padded lines dropped, order kept.
    let pairs = form(&[
        ("__group", "Apps"),
        (
            "prewarmApps",
            "tv.plex.PlexHTPC\r\n\r\n  com.spotify.Client  \n",
        ),
    ]);
    let html = pages::apps::render_save(&state, &pairs).await;
    assert!(
        html.to_lowercase().contains("saved"),
        "expected ok result: {html}"
    );

    let patch = only_patch(&received);
    assert_eq!(
        patch["prewarmApps"],
        serde_json::json!(["tv.plex.PlexHTPC", "com.spotify.Client"])
    );
    assert_eq!(
        patch.as_object().unwrap().len(),
        1,
        "the Apps group is one key — nothing else may ride along: {patch}"
    );
}

#[tokio::test]
async fn display_audio_page_declares_all_four_of_its_groups() {
    let (sock, _received) = spawn_config_daemon("display-audio-page", r#"{"overscan":3}"#);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::display_audio::render_page(&state).await;
    for group in ["Display", "Night Light", "Power", "Audio"] {
        assert!(
            html.contains(&format!(r#"name="__group" value="{group}""#)),
            "a form owning four groups must emit a companion for each — {group} \
             is missing, so its fields would be dropped from the patch: {html}"
        );
    }
    assert!(
        html.contains(r#"value="3""#),
        "the page must prefill from the current document: {html}"
    );
    assert!(
        !html.contains(r#"name="themeMode""#),
        "themeMode belongs to Appearance and must not render here: {html}"
    );
}

#[tokio::test]
async fn display_audio_save_patches_its_four_groups_and_no_others() {
    let (sock, received) = spawn_config_daemon("display-audio-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let pairs = form(&[
        ("__group", "Display"),
        ("__group", "Night Light"),
        ("__group", "Power"),
        ("__group", "Audio"),
        ("hdrEnabled", "on"),
        ("overscan", "2"),
        ("nightLightTemp", "3800"),
        ("sleepTimerMinutes", "30"),
        ("defaultSink", "hdmi"),
    ]);
    let html = pages::display_audio::render_save(&state, &pairs).await;
    assert!(
        html.to_lowercase().contains("saved"),
        "expected ok result: {html}"
    );

    let patch = only_patch(&received);
    assert_eq!(patch["hdrEnabled"], true);
    assert_eq!(patch["overscan"], 2);
    assert_eq!(patch["nightLightTemp"], 3800);
    assert_eq!(patch["sleepTimerMinutes"], 30);
    assert_eq!(patch["defaultSink"], "hdmi");
    // Unchecked boxes across all four submitted groups: explicit false.
    assert_eq!(patch["autoDimEnabled"], false);
    assert_eq!(patch["nightLightEnabled"], false);
    assert_eq!(patch["wakeOnController"], false);
    for other in [
        "themeMode",
        "reduceMotion",
        "controllerDebug",
        "cecFocusOnWake",
        // Moved to Appearance in phase 4, with the wallpaper grid as its
        // editor — this page must no longer write it at all.
        "wallpaperPath",
    ] {
        assert!(
            patch.get(other).is_none(),
            "{other} is another page's group: {patch}"
        );
    }
}

#[tokio::test]
async fn cec_config_save_patches_only_the_cec_group() {
    let (sock, received) = spawn_config_daemon("cec-config-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let pairs = form(&[
        ("__group", "CEC"),
        ("cecFocusOnWake", "on"),
        ("cecDefaultInput", "4"),
    ]);
    let html = pages::settings::render_save(&state, &["CEC"], &pairs).await;
    assert!(
        html.to_lowercase().contains("saved"),
        "expected ok result: {html}"
    );

    let patch = only_patch(&received);
    assert_eq!(patch["cecFocusOnWake"], true);
    assert_eq!(patch["cecDefaultInput"], 4);
    assert_eq!(patch["cecFocusOnStartup"], false);
    assert_eq!(patch["cecAutoSwitchOnPowerOn"], false);
    assert!(
        patch.get("cecDeviceNames").is_none(),
        "cecDeviceNames is FieldKind::Complex — raw-JSON only: {patch}"
    );
    for other in ["themeMode", "rumbleEnabled", "hdrEnabled"] {
        assert!(
            patch.get(other).is_none(),
            "{other} is another page's group: {patch}"
        );
    }
}

#[tokio::test]
async fn controllers_settings_save_patches_only_the_input_group() {
    let (sock, received) = spawn_config_daemon("controllers-settings-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let pairs = form(&[("__group", "Input"), ("rumbleEnabled", "on")]);
    let html = pages::settings::render_save(&state, &["Input"], &pairs).await;
    assert!(
        html.to_lowercase().contains("saved"),
        "expected ok result: {html}"
    );

    let patch = only_patch(&received);
    assert_eq!(patch["rumbleEnabled"], true);
    assert_eq!(patch["controllerDebug"], false);
    assert_eq!(
        patch.as_object().unwrap().len(),
        2,
        "the Input group is exactly two keys: {patch}"
    );
}

/// **Fail closed on a form that declares nothing.** Defaulting to "all groups"
/// is precisely the data-loss bug the `__group` companions exist to prevent,
/// so a submission without one is an error and reaches no daemon at all.
#[tokio::test]
async fn a_settings_save_with_no_group_is_refused() {
    let (sock, received) = spawn_config_daemon("settings-no-group", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let pairs = form(&[("themeMode", "light")]);
    let html = pages::appearance::render_save(&state, &pairs).await;
    assert!(
        html.contains("__group"),
        "the error must name the missing field: {html}"
    );
    assert!(
        !html.to_lowercase().contains("settings saved"),
        "nothing may be reported as saved: {html}"
    );
    assert!(
        received.lock().unwrap().is_empty(),
        "a scope-less submission must not reach set-config at all"
    );
}

#[test]
fn an_unknown_settings_group_is_refused() {
    let pairs = form(&[("__group", "Appearanc"), ("themeMode", "light")]);
    let err = pages::settings::build_patch(&["Appearance"], &pairs)
        .expect_err("a typo'd group must not silently skip every field");
    assert!(err.contains("unknown settings group"), "{err}");
}

/// A route's group list is a server-side constant, so a hand-rolled POST
/// cannot borrow one page's save route to write another page's group.
#[test]
fn a_form_cannot_declare_a_group_its_route_does_not_own() {
    let pairs = form(&[("__group", "Display"), ("hdrEnabled", "on")]);
    let err = pages::settings::build_patch(&["Appearance"], &pairs)
        .expect_err("Appearance's route must refuse to patch Display");
    assert!(err.contains("not owned"), "{err}");
}

#[tokio::test]
async fn advanced_page_renders_pretty_printed_raw_json() {
    let (sock, _received) = spawn_config_daemon(
        "advanced-raw-pretty-render",
        r#"{"themeMode":"dark","rumbleEnabled":true}"#,
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::advanced::render_page(&state).await;
    assert!(
        html.contains("{\n"),
        "expected the raw JSON escape hatch to be pretty-printed (multi-line): {html}"
    );
    assert!(
        html.contains("shallow merge") && html.contains("<code>null</code>"),
        "the hatch must keep its shallow-merge / null-deletes warning: {html}"
    );
}

#[tokio::test]
async fn advanced_raw_pretty_input_is_sent_compact() {
    // The textarea round-trips a pretty-printed, multi-line document — the
    // set-config call it triggers must still be a single compact line.
    let (sock, received) = spawn_config_daemon("advanced-raw-compact", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let pretty = "{\n  \"themeMode\": \"light\"\n}";
    let html = pages::advanced::render_save_raw(&state, pretty).await;
    assert!(
        html.to_lowercase().contains("merged"),
        "expected an ok result: {html}"
    );
    let sent = received.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one set-config call: {sent:?}"
    );
    assert_eq!(
        sent[0], r#"{"themeMode":"light"}"#,
        "raw JSON must be compacted to a single line before set-config: {sent:?}"
    );
}

#[tokio::test]
async fn advanced_raw_rejects_malformed_json() {
    // No daemon needed: malformed JSON must be rejected before any IPC call.
    let state = hermetic_state();
    let html = pages::advanced::render_save_raw(&state, "not json").await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("invalid"),
        "expected an error marker for malformed JSON: {html}"
    );
}

#[tokio::test]
async fn advanced_raw_rejects_non_object_json() {
    let state = hermetic_state();
    let html = pages::advanced::render_save_raw(&state, "[1,2,3]").await;
    assert!(
        html.to_lowercase().contains("invalid") || html.to_lowercase().contains("object"),
        "expected an error marker for a non-object JSON body: {html}"
    );
}

/// Every page the Settings page dissolved into degrades the same way it did:
/// still 200, an honest note, no form — and Advanced still shows the
/// `config.toml` view it owns, which needs no daemon.
#[tokio::test]
async fn the_settings_pages_degrade_when_the_daemon_is_unreachable() {
    let state = hermetic_state();
    for (name, html) in [
        ("appearance", pages::appearance::render_page(&state).await),
        ("apps", pages::apps::render_page(&state).await),
        ("advanced", pages::advanced::render_page(&state).await),
        (
            "display-audio",
            pages::display_audio::render_page(&state).await,
        ),
    ] {
        assert!(!html.is_empty(), "{name} rendered nothing");
        assert!(
            html.to_lowercase().contains("unreachable"),
            "{name} must show an unreachable marker when the daemon is down: {html}"
        );
        assert!(
            !html.contains(r#"name="__group""#),
            "{name} must not render a settings form it cannot submit: {html}"
        );
    }
    let advanced = pages::advanced::render_page(&state).await;
    assert!(
        advanced.contains("config.toml"),
        "Advanced owns the config.toml view, which reads the panel's own \
         filesystem and survives a dead daemon: {advanced}"
    );
    assert!(
        !advanced.contains("raw_json"),
        "the raw hatch writes through the daemon — it must not render with the \
         daemon down: {advanced}"
    );
}

/// The two settings forms that live on a page in a DIFFERENT `build_router`
/// block: their save routes are registered under `Gate::SettingsStore`, so the
/// form must not render when that gate is closed — the panel never draws a
/// control for a route that was not registered.
#[tokio::test]
async fn a_settings_form_is_absent_when_its_save_route_is_not_registered() {
    let (sock, _received) = spawn_config_daemon("settings-form-gating", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;

    let without = state_for_socket_with_caps(sock.clone(), caps_with(BTreeSet::new()));
    let controllers = pages::controllers::render_page(&without).await;
    assert!(
        !controllers.contains("/devices/controllers/settings/save"),
        "the Input form posts to a SettingsStore route: {controllers}"
    );
    let cec = pages::cec::render_page(&without).await;
    assert!(
        !cec.contains("/devices/cec/config"),
        "the CEC settings form posts to a SettingsStore route: {cec}"
    );

    let with = state_for_socket(sock);
    let controllers = pages::controllers::render_page(&with).await;
    assert!(
        controllers.contains("/devices/controllers/settings/save")
            && controllers.contains(r#"name="__group" value="Input""#),
        "with the gate open the Input form renders, scoped: {controllers}"
    );
    let cec = pages::cec::render_page(&with).await;
    assert!(
        cec.contains("/devices/cec/config") && cec.contains(r#"name="__group" value="CEC""#),
        "with the gate open the CEC settings form renders, scoped: {cec}"
    );
}

#[tokio::test]
async fn widgets_page_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::widgets::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "widgets page must show an unreachable marker when the daemon is down: {html}"
    );
}

#[tokio::test]
async fn appearance_page_carries_the_wallpaper_surface_and_its_oob_markers() {
    // The daemon owns wallpaperPath, so with it down the page must still
    // render (200 + honest banner) rather than 500 — the wallpaper FILES are
    // local and still listable.
    let state = hermetic_state();
    let html = pages::appearance::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "the appearance page must show an unreachable marker when the daemon is \
         down: {html}"
    );
    assert!(
        html.contains("<h2>Wallpaper</h2>"),
        "the Media page's wallpaper half lives here as of phase 4: {html}"
    );
    assert!(
        html.contains("<!--wallpaper-list-start-->") && html.contains("<!--wallpaper-list-end-->"),
        "the OOB list-refresh markers must survive any template restructuring — \
         without them every post-action grid refresh silently swaps in nothing: \
         {html}"
    );
}

/// **The OOB refresh, end to end.** `render_wallpaper_list_oob` re-renders the
/// page that owns the list and string-slices it between the two comment
/// markers, so a template edit that drops or renames a marker degrades to an
/// empty string — a silent failure the eye does not catch on a page that
/// otherwise looks fine.
#[tokio::test]
async fn wallpaper_oob_refresh_returns_the_list_fragment() {
    let state = hermetic_state();
    let frag = pages::appearance::render_wallpaper_list_oob(&state).await;
    assert!(
        frag.starts_with(r#"<div id="wallpaper-list" hx-swap-oob="innerHTML">"#),
        "expected an OOB swap wrapper for the grid: {frag}"
    );
    assert!(
        frag.len() > 60 && frag.contains("wallpaper"),
        "expected non-empty list markup inside the wrapper: {frag}"
    );
    assert!(
        !frag.contains("<h1>") && !frag.contains("<nav"),
        "the fragment must be a SLICE of the page, not the whole page: {frag}"
    );
}

#[tokio::test]
async fn apps_page_carries_the_webapp_registry_and_its_oob_markers() {
    let state = hermetic_state();
    let html = pages::apps::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "the apps page must show an unreachable marker when the daemon is down: {html}"
    );
    assert!(
        html.contains("<h2>Web apps</h2>"),
        "the Media page's web-app half lives here as of phase 4: {html}"
    );
    assert!(
        html.contains("<!--webapp-list-start-->") && html.contains("<!--webapp-list-end-->"),
        "the OOB list-refresh markers must survive any template restructuring: {html}"
    );
}

/// The webapp half of the same coupling, with a daemon that actually answers
/// `webapp-list` — so the fragment must carry the real table markup, not just
/// an empty-state paragraph.
#[tokio::test]
async fn webapp_oob_refresh_returns_the_list_fragment() {
    let mut replies = HashMap::new();
    replies.insert(
        "webapp-list",
        r#"[{"id":"yt","name":"YouTube","url":"https://y.tv","wmClass":"tvshell-yt"}]"#,
    );
    replies.insert("webapp-remove yt", "ok");
    let sock = spawn_canned_daemon("webapp-oob", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let frag = pages::apps::render_webapp_list_oob(&state).await;
    assert!(
        frag.starts_with(r#"<div id="webapp-list" hx-swap-oob="innerHTML">"#),
        "expected an OOB swap wrapper for the registry table: {frag}"
    );
    assert!(
        frag.contains("tvshell-yt") && frag.contains("<table>"),
        "expected the registry table markup inside the wrapper: {frag}"
    );
    assert!(
        !frag.contains("<h1>"),
        "the fragment must be a SLICE of the page: {frag}"
    );

    // And the action that triggers it appends the fragment to its result.
    let html = pages::apps::render_webapp_remove(&state, "yt").await;
    assert!(
        html.contains(r#"id="webapp-list" hx-swap-oob="innerHTML""#),
        "a remove must carry the OOB table refresh: {html}"
    );
}

#[tokio::test]
async fn apps_webapp_add_relays_a_compact_json_body() {
    // The panel must not validate/allocate ids itself — it relays name+url and
    // lets the daemon (the registry's sole writer) do the work.
    let (sock, received) = spawn_recording_daemon(
        "apps-webapp-add",
        r#"{"id":"youtube","name":"YouTube","url":"https://youtube.com/tv","wmClass":"tvshell-youtube"}"#,
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let _ = pages::apps::render_webapp_add(&state, "  YouTube  ", " https://youtube.com/tv ").await;
    let sent: Vec<String> = received
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("webapp-add"))
        .cloned()
        .collect();
    assert_eq!(sent.len(), 1, "expected exactly one webapp-add: {sent:?}");
    assert!(
        sent[0].starts_with("webapp-add {"),
        "expected a webapp-add with a JSON body, got: {}",
        sent[0]
    );
    assert!(!sent[0].contains('\n'), "command must stay single-line");
    let body: serde_json::Value =
        serde_json::from_str(sent[0].trim_start_matches("webapp-add ")).unwrap();
    assert_eq!(body["name"], "YouTube", "name must be trimmed");
    assert_eq!(body["url"], "https://youtube.com/tv", "url must be trimmed");
}

#[tokio::test]
async fn widgets_page_default_fills_missing_subtree() {
    // No "widgets" key at all in the canned get-config document — every
    // widget must still render, default-filled per WidgetManifests.qml.
    let (sock, _received) = spawn_config_daemon("widgets-page-empty", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::widgets::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.contains("Moonlight"),
        "expected all 5 widget cards: {html}"
    );
    assert!(
        html.contains("Now Playing"),
        "expected all 5 widget cards: {html}"
    );
    assert!(html.contains("Plex"), "expected all 5 widget cards: {html}");
    assert!(html.contains("Apps"), "expected all 5 widget cards: {html}");
    assert!(
        html.contains("Steam"),
        "expected all 5 widget cards: {html}"
    );
}

#[tokio::test]
async fn widgets_save_sends_all_five_widgets_with_valid_sizes() {
    let (sock, received) = spawn_config_daemon("widgets-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    // Mirrors what the whole-page pre-filled form would submit: every
    // widget's fields present, one value (moonlight's size) changed.
    let mut form: HashMap<String, String> = HashMap::new();
    form.insert("w_moonlight_enabled".to_string(), "on".to_string());
    form.insert("w_moonlight_order".to_string(), "0".to_string());
    form.insert("w_moonlight_size".to_string(), "large".to_string());
    form.insert("w_nowplaying_enabled".to_string(), "on".to_string());
    form.insert("w_nowplaying_order".to_string(), "1".to_string());
    form.insert("w_nowplaying_size".to_string(), "medium".to_string());
    form.insert(
        "w_nowplaying_pref_hideFromRecent".to_string(),
        "on".to_string(),
    );
    form.insert("w_plex_enabled".to_string(), "on".to_string());
    form.insert("w_plex_order".to_string(), "2".to_string());
    form.insert("w_plex_size".to_string(), "medium".to_string());
    form.insert("w_plex_pref_hideFromRecent".to_string(), "on".to_string());
    form.insert("w_recent_enabled".to_string(), "on".to_string());
    form.insert("w_recent_order".to_string(), "3".to_string());
    form.insert("w_recent_size".to_string(), "medium".to_string());
    form.insert("w_steam_order".to_string(), "4".to_string());
    form.insert("w_steam_size".to_string(), "medium".to_string());
    // w_steam_enabled intentionally absent — steam defaults disabled.

    let html = pages::widgets::render_save(&state, &form).await;
    assert!(
        html.to_lowercase().contains("ok"),
        "expected an ok result: {html}"
    );

    let sent = received.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one set-config call: {sent:?}"
    );
    let patch: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    let widgets = patch["widgets"]
        .as_object()
        .expect("set-config body must contain a widgets object");
    for id in ["moonlight", "nowplaying", "plex", "recent", "steam"] {
        assert!(
            widgets.contains_key(id),
            "set-config body must include widget {id} (shallow merge would wipe \
             siblings if omitted): {patch}"
        );
    }
    assert_eq!(widgets["moonlight"]["size"], "large");
    assert_eq!(widgets["steam"]["enabled"], false);
    assert_eq!(widgets["steam"]["size"], "medium");
    assert_eq!(widgets["nowplaying"]["prefs"]["hideFromRecent"], true);
    assert_eq!(widgets["plex"]["prefs"]["hideFromRecent"], true);

    // Every widget's size must be one of its own manifest's allowed values.
    assert!(["small", "medium", "large"].contains(&widgets["moonlight"]["size"].as_str().unwrap()));
    assert!(["medium", "large"].contains(&widgets["steam"]["size"].as_str().unwrap()));
}

#[tokio::test]
async fn widgets_save_rejects_invalid_size_for_widget() {
    // "small" is not a valid Steam size (steam only offers medium/large) —
    // validation must reject this before any IPC call, so no daemon needed.
    let state = hermetic_state();
    let mut form: HashMap<String, String> = HashMap::new();
    form.insert("w_steam_size".to_string(), "small".to_string());
    let html = pages::widgets::render_save(&state, &form).await;
    assert!(
        html.to_lowercase().contains("invalid"),
        "expected a validation error for an out-of-enum size: {html}"
    );
}

#[tokio::test]
async fn widgets_reorder_up_swaps_with_predecessor_and_renumbers() {
    // Default (empty) config: declaration order is moonlight(0), nowplaying(1),
    // plex(2), recent(3), steam(4) — moving plex up should swap it with
    // nowplaying and renumber both.
    let (sock, received) = spawn_config_daemon("widgets-reorder-up", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::widgets::render_reorder(&state, "plex", "up").await;
    assert!(
        html.contains("Plex") && html.contains("Now Playing"),
        "expected the refreshed grid to still show all cards: {html}"
    );

    let sent = received.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one set-config call: {sent:?}"
    );
    let patch: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    let widgets = patch["widgets"]
        .as_object()
        .expect("set-config body must contain a widgets object");
    for id in ["moonlight", "nowplaying", "plex", "recent", "steam"] {
        assert!(
            widgets.contains_key(id),
            "set-config body must include widget {id} (shallow merge would wipe \
             siblings if omitted): {patch}"
        );
    }
    assert_eq!(widgets["plex"]["order"], 1);
    assert_eq!(widgets["nowplaying"]["order"], 2);
    assert_eq!(
        widgets["moonlight"]["order"], 0,
        "a widget not involved in the swap keeps its position"
    );
}

#[tokio::test]
async fn widgets_reorder_at_the_boundary_is_a_position_noop_but_still_renumbers() {
    // moonlight is already first — "up" has no predecessor to swap with, but
    // the order fields still get renumbered to a clean 0..N sequence.
    let (sock, received) = spawn_config_daemon("widgets-reorder-boundary", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::widgets::render_reorder(&state, "moonlight", "up").await;
    assert!(!html.is_empty());

    let sent = received.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let patch: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    let widgets = patch["widgets"].as_object().unwrap();
    assert_eq!(widgets["moonlight"]["order"], 0);
    assert_eq!(widgets["nowplaying"]["order"], 1);
    assert_eq!(widgets["steam"]["order"], 4);
}

// ---------------------------------------------------------------------------
// IA phase 4: the pages the Tools console dissolved into
//
// Same coverage as the Tools console's own tests, re-pointed at the modules
// that now own each domain. The validators they exercise moved intact into
// `pages::ipc_console`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn navigation_intent_rejects_whitespace_without_ipc() {
    // Validation must fail before any IPC call — no daemon needed.
    let state = hermetic_state();
    let html = pages::navigation::render_intent(&state, "settings audio").await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

#[tokio::test]
async fn navigation_intent_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::navigation::render_intent(&state, "home").await;
    assert!(
        html.to_lowercase().contains("unreachable"),
        "expected a daemon-unreachable marker: {html}"
    );
}

#[tokio::test]
async fn navigation_key_rejects_unknown_key_without_ipc() {
    let state = hermetic_state();
    let html = pages::navigation::render_key(&state, "north").await;
    assert!(
        html.to_lowercase().contains("unknown key"),
        "expected an unknown-key error: {html}"
    );
}

#[tokio::test]
async fn launcher_launch_rejects_whitespace_in_wm_class() {
    let state = hermetic_state();
    let html = pages::launcher::render_launch_app(&state, "org.mozilla firefox").await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

#[tokio::test]
async fn network_ping_rejects_whitespace_in_host() {
    let state = hermetic_state();
    let html = pages::network::render_ping(&state, "1.1.1.1 extra", None).await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

#[tokio::test]
async fn network_ping_rejects_out_of_range_count() {
    let state = hermetic_state();
    let html = pages::network::render_ping(&state, "1.1.1.1", Some("99")).await;
    assert!(
        html.contains("1 and 10"),
        "expected a count-range validation error: {html}"
    );
}

#[tokio::test]
async fn network_throughput_rejects_path_separator_in_iface() {
    let state = hermetic_state();
    let html = pages::network::render_throughput(&state, "../etc").await;
    assert!(
        html.to_lowercase().contains("invalid interface"),
        "expected an invalid-interface error: {html}"
    );
}

#[tokio::test]
async fn network_bt_action_rejects_unknown_action() {
    let state = hermetic_state();
    let html = pages::network::render_bt_action(&state, "AA:BB:CC:DD:EE:FF", "reboot").await;
    assert!(
        html.to_lowercase().contains("unknown bluetooth action"),
        "expected an unknown-action error: {html}"
    );
}

#[tokio::test]
async fn ipc_console_json_reply_is_pretty_printed() {
    let mut replies = HashMap::new();
    replies.insert(
        "sys-status",
        r#"{"os":"Test OS","kernel":"1.2.3","hostname":"h","uptime":"1h"}"#,
    );
    let sock = spawn_canned_daemon("ipc-json", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::ipc_console::run_line(&state, "sys-status").await;
    assert!(
        html.contains("Test OS"),
        "expected the pretty-printed sys-status JSON: {html}"
    );
}

#[tokio::test]
async fn ipc_console_bare_text_reply_round_trips() {
    let mut replies = HashMap::new();
    replies.insert("bt-power-status", "bt:on");
    let sock = spawn_canned_daemon("ipc-bare-text", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::ipc_console::run_line(&state, "bt-power-status").await;
    assert!(
        html.contains("bt:on"),
        "expected the bare-text reply: {html}"
    );
}

#[tokio::test]
async fn console_raw_error_reply_roundtrip() {
    let mut replies = HashMap::new();
    replies.insert("sys-metrics", "error:input-runtime-down");
    let sock = spawn_canned_daemon("console-raw-error", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::console::render_raw(&state, "sys-metrics").await;
    assert!(
        html.to_lowercase().contains("input-runtime-down"),
        "expected the daemon's error message: {html}"
    );
}

#[tokio::test]
async fn console_raw_warns_on_guarded_command() {
    let mut replies = HashMap::new();
    replies.insert("grab", "ok");
    let sock = spawn_canned_daemon("console-raw-warn", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::console::render_raw(&state, "grab").await;
    assert!(
        html.to_lowercase().contains("guarded"),
        "expected a warning banner for a guarded command: {html}"
    );
}

#[tokio::test]
async fn console_raw_rejects_empty_command() {
    let state = hermetic_state();
    let html = pages::console::render_raw(&state, "   ").await;
    assert!(
        html.to_lowercase().contains("empty"),
        "expected an empty-command validation error: {html}"
    );
}

// ---------------------------------------------------------------------------
// M3 / IA phase 2: the three pages the Processes page split into
// ---------------------------------------------------------------------------

#[tokio::test]
async fn processes_page_renders_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::processes::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("hyprland"),
        "expected the Hyprland section to render: {html}"
    );
    assert!(
        html.to_lowercase().contains("unavailable"),
        "expected a Hyprland-unavailable note when the daemon is down: {html}"
    );
    assert!(
        html.contains("Top processes"),
        "expected the top-processes section: {html}"
    );
}

/// The point of the phase-2 split: Processes is **purely read-only
/// observation**. Unit control moved to Services and pacman to Updates, so
/// this page must render no action affordance and no updates section at all.
#[tokio::test]
async fn processes_page_renders_no_restart_control_or_updates_section() {
    let state = hermetic_state();
    let html = pages::processes::render_page(&state).await;
    assert!(
        !html.contains("hx-post="),
        "Processes mutates nothing — it must render no form target: {html}"
    );
    assert!(
        !html.contains("System Updates") && !html.contains(r#"id="updates-check""#),
        "the System Updates section belongs to /system/updates now: {html}"
    );
    assert!(
        !html.contains("systemd units") && !html.contains(">Restart<"),
        "the unit table and its Restart buttons belong to /system/services now: {html}"
    );
}

/// Services owns the unit table: the three built-in tv-shell units, each with
/// its own restart form.
#[tokio::test]
async fn services_page_renders_the_three_built_in_units() {
    let state = hermetic_state();
    let html = pages::services::render_page(&state).await;
    for key in ["daemon", "shell", "panel"] {
        assert!(
            html.contains(&format!(r#"hx-post="/system/services/restart/{key}""#)),
            "expected a restart form for the built-in {key} unit: {html}"
        );
    }
    assert!(
        html.contains("This is the panel serving THIS page"),
        "the panel's own unit keeps its distinct disconnect confirm: {html}"
    );
    assert!(
        html.matches(r#"<span class="unit-chip">"#).count() == 3,
        "each unit's dot and status word must sit in one nowrap chip: {html}"
    );
    // All three are `--user` units, so none of them is severe-tier.
    assert!(
        !html.contains("danger-severe"),
        "a --user restart needs no elevation and stays tier 1: {html}"
    );
}

/// With nothing configured, the page says so rather than rendering an empty
/// table — and still offers the (unrestricted) read path.
#[tokio::test]
async fn services_page_explains_an_empty_allowlist_and_still_offers_the_read_path() {
    let state = hermetic_state();
    let html = pages::services::render_page(&state).await;
    assert!(
        html.contains("No <code>[panel].managed_units</code> are configured"),
        "an empty allowlist must be explained: {html}"
    );
    assert!(
        html.contains(r#"hx-get="/system/services/inspect""#) && html.contains(r#"name="unit""#),
        "the inspect form is the read side and is never gated: {html}"
    );
}

// ── phase 5: the restart allowlist ─────────────────────────────────────────

fn managed(entries: &[(&str, &str, &str)]) -> AppConfig {
    let raw: Vec<crate::config::RawManagedUnit> = entries
        .iter()
        .map(|(key, unit, scope)| crate::config::RawManagedUnit {
            key: key.to_string(),
            unit: unit.to_string(),
            scope: scope.to_string(),
        })
        .collect();
    AppConfig {
        managed_units: crate::config::resolve_managed_units(&raw).expect("well-formed test list"),
        ..AppConfig::default()
    }
}

#[tokio::test]
async fn services_page_renders_the_configured_allowlist_with_severe_tier_confirms() {
    let state = state_with(managed(&[
        ("sshd", "sshd.service", "system"),
        ("network", "NetworkManager.service", "system"),
        ("bluetooth", "bluetooth.service", "system"),
        ("pipewire", "pipewire.service", "user"),
    ]));
    let html = pages::services::render_page(&state).await;

    for key in ["sshd", "network", "bluetooth", "pipewire"] {
        assert!(
            html.contains(&format!(r#"hx-post="/system/services/restart/{key}""#)),
            "expected a restart form for the allowlisted {key}: {html}"
        );
    }
    // The form carries the KEY, never the unit name — that is the property.
    assert!(
        !html.contains("restart/sshd.service") && !html.contains("restart/NetworkManager.service"),
        "the client must only ever be handed a key: {html}"
    );
    // Elevated restarts are severe tier; the user-scope one is not.
    assert_eq!(
        html.matches(r#"<button class="danger-severe""#).count(),
        3,
        "the three system-scope restarts are severe tier and nothing else is: {html}"
    );
    // Confirms name the specific unit, and the two that can strand the box
    // say so.
    assert!(html.contains("Restart sshd.service now?"), "{html}");
    assert!(
        html.matches("end remote access entirely").count() == 2,
        "sshd and NetworkManager warn about losing remote access; bluetooth and \
         pipewire must not: {html}"
    );
}

/// An unknown key is refused **before any exec** — it is not a unit name, it
/// is a client asking for one.
#[tokio::test]
async fn services_restart_rejects_an_unknown_unit_key_before_touching_systemctl() {
    let state = state_with(managed(&[("sshd", "sshd.service", "system")]));
    for key in [
        "bogus",
        "sshd.service",   // the unit name is not a key
        "NetworkManager", // not in this node's table
        "",
        "../../dev/reboot",
    ] {
        let html = pages::services::render_restart(&state, key).await;
        assert!(
            html.contains("unknown unit key") && html.contains("nothing was run"),
            "{key:?} must be refused with no exec: {html}"
        );
        assert!(html.contains("result-error"), "{key:?}: {html}");
    }
}

/// The built-ins are hardcoded so a config typo cannot cost the recovery path.
/// Config can neither shadow them nor be shadowed by them — it is a load
/// error, and even the lookup consults the built-ins first.
#[test]
fn a_managed_unit_may_not_shadow_a_built_in_key() {
    for key in crate::config::BUILT_IN_UNIT_KEYS {
        let raw = [crate::config::RawManagedUnit {
            key: key.to_string(),
            unit: "somethingelse.service".to_string(),
            scope: "system".to_string(),
        }];
        assert!(
            crate::config::resolve_managed_units(&raw).is_err(),
            "{key} must be refused at config load, not silently shadowed"
        );
    }
}

/// **The read path is where an operator types.** It must reject anything that
/// is not plausibly a unit name, before the string is capable of reaching an
/// argv at all.
#[tokio::test]
async fn services_inspect_validates_the_typed_unit_name() {
    let state = hermetic_state();
    for bad in [
        "",
        "   ",
        "sshd .service",
        "sshd;reboot",
        "sshd && reboot",
        "sshd | tee /tmp/x",
        "$(reboot)",
        "`reboot`",
        "/etc/systemd/system/sshd.service",
        "../../etc/shadow",
        "-h",
        "--user",
    ] {
        let html = pages::services::render_inspect(&state, bad, "system").await;
        assert!(
            html.contains("banner-error"),
            "{bad:?} must be rejected by the inspect form: {html}"
        );
        assert!(
            !html.contains("unit-inspect"),
            "{bad:?} must not render a status table: {html}"
        );
    }
    let absurd = "a".repeat(9000);
    let html = pages::services::render_inspect(&state, &absurd, "system").await;
    assert!(html.contains("banner-error"), "an absurd name is rejected");

    // ... and a bad scope is refused too, rather than defaulting.
    let html = pages::services::render_inspect(&state, "sshd.service", "root").await;
    assert!(html.contains("Not a scope"), "{html}");
}

/// A readable unit is not a restartable one. The inspect fragment says which
/// it is, so the page never implies an affordance the allowlist does not grant.
#[tokio::test]
async fn services_inspect_says_whether_the_unit_is_restartable() {
    let state = state_with(managed(&[("sshd", "sshd.service", "system")]));
    let html = pages::services::render_inspect(&state, "sshd.service", "system").await;
    assert!(
        html.contains("restart allowlist as <code>sshd</code>"),
        "an allowlisted unit points back at its key: {html}"
    );
    let html = pages::services::render_inspect(&state, "cups.service", "system").await;
    assert!(
        html.contains("not in the restart allowlist"),
        "a merely-readable unit says so: {html}"
    );
}

/// **The structural half of the no-arbitrary-unit property.**
///
/// Every mutating `systemctl` argv in `exec.rs` is built from string literals
/// plus, at most, `target.unit().as_str()` — where `target` is the
/// [`crate::config::RestartTarget`] parameter. A `&str` unit name has no
/// signature to arrive through, and this test fails if one is ever added.
#[test]
fn the_only_mutating_systemctl_argv_is_a_restart_target() {
    const MUTATING_VERBS: [&str; 14] = [
        "restart",
        "start",
        "stop",
        "reload",
        "try-restart",
        "kill",
        "isolate",
        "mask",
        "unmask",
        "enable",
        "disable",
        "reboot",
        "suspend",
        "poweroff",
    ];
    /// The one non-literal argv element the mutating path may use.
    const ALLOWED_DYNAMIC: &str = "target.unit().as_str()";

    let src = include_str!("exec.rs");
    let mut checked = 0usize;
    for (signature, body) in fn_blocks(src) {
        let mutating = MUTATING_VERBS
            .iter()
            .any(|v| body.contains(&format!("\"{v}\"")));
        if !mutating {
            continue;
        }
        checked += 1;
        for argv in argv_slices(&body) {
            for element in argv.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                if element.starts_with('"') {
                    continue; // a literal — fixed at compile time
                }
                assert_eq!(
                    element, ALLOWED_DYNAMIC,
                    "exec.rs `{signature}` passes {element:?} to a mutating systemctl \
                     argv. The only non-literal argument permitted there is \
                     {ALLOWED_DYNAMIC} — a unit name must come from the server-side \
                     table (docs/PANEL_IA.md § Preserving the no-arbitrary-unit property)"
                );
                assert!(
                    signature.contains("target: &RestartTarget"),
                    "exec.rs `{signature}` uses a dynamic unit name without taking a \
                     &RestartTarget"
                );
            }
        }
    }
    assert!(
        checked >= 3,
        "expected to have checked restart/reboot/suspend at least; the scanner \
         found only {checked} mutating fn(s) — it has probably stopped matching \
         exec.rs's shape and is asserting nothing"
    );
}

/// The counterpart in `config.rs`: [`crate::config::RestartTarget`] is
/// constructible only by resolving a KEY against a server-side table, so there
/// is no way to hand [`crate::exec::Recovery::restart`] a name off the wire.
#[test]
fn restart_target_is_only_constructible_from_the_server_side_table() {
    let src = include_str!("config.rs");
    // Exactly three public constructors, each of which takes a key or the
    // raw config list — never a bare unit name.
    let constructors: Vec<String> = fn_blocks(src)
        .into_iter()
        .map(|(sig, _)| sig)
        .filter(|sig| sig.starts_with("pub fn") && sig.contains("RestartTarget"))
        .collect();
    let mut names: Vec<&str> = constructors
        .iter()
        .map(|sig| sig.split_whitespace().nth(2).unwrap_or(sig))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            // key -> built-in table
            "builtin_target(key:",
            // the raw `[panel].managed_units` list -> validated table
            "resolve_managed_units(raw:",
            // key -> built-ins, then the configured table
            "restart_target(&self,",
            // the whole table, no argument at all
            "restart_targets(&self)",
        ],
        "config.rs grew a new public way to make a RestartTarget. Every one must \
         resolve a key (or take no argument at all) against the server-side table — \
         a constructor taking a unit name would hand the restart path straight to \
         the client. Public constructors found: {constructors:?}"
    );
    // No trait conversion, and no public field, either.
    assert!(
        !src.contains("for RestartTarget"),
        "a From/TryFrom impl would be a fourth constructor"
    );
    let decl = src
        .split_once("pub struct RestartTarget {")
        .and_then(|(_, rest)| rest.split_once('}').map(|(body, _)| body.to_string()))
        .expect("RestartTarget is a braced struct");
    assert!(
        !decl.contains("pub "),
        "RestartTarget's fields stay private — a pub field is a fourth \
         constructor, and a mutable one: {decl}"
    );
}

/// Split a Rust source file into `(signature, body)` pairs, one per `fn`, for
/// the two structural scans above. Comment lines are dropped first so prose
/// mentioning a quoted verb cannot trip the scanner.
fn fn_blocks(src: &str) -> Vec<(String, String)> {
    let code: Vec<&str> = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in code {
        let t = line.trim();
        let is_fn = t.starts_with("fn ")
            || t.starts_with("pub fn ")
            || t.starts_with("async fn ")
            || t.starts_with("pub async fn ")
            || t.starts_with("pub(crate) fn ");
        if is_fn {
            out.push((t.to_string(), String::new()));
        } else if let Some(last) = out.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

/// Pull the contents of every `&[ ... ]` array literal out of a fn body — the
/// argv slices `exec::run` is called with.
fn argv_slices(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("&[") {
        rest = &rest[i + 2..];
        let Some(end) = rest.find(']') else { break };
        out.push(rest[..end].replace('\n', " "));
        rest = &rest[end..];
    }
    out
}

#[tokio::test]
async fn updates_page_renders_the_system_updates_section() {
    let state = hermetic_state();
    let html = pages::updates::render_page(&state).await;
    assert!(
        html.contains("System Updates"),
        "expected the System Updates section heading: {html}"
    );
    assert!(
        html.contains(r#"id="updates-check""#),
        "expected the updates-check partial: {html}"
    );
    assert!(
        html.contains(r#"id="update-job-status""#),
        "expected the self-polling job-status partial: {html}"
    );
    assert!(
        html.contains(r#"hx-trigger="every 2s [this.dataset.running=='1']""#),
        "the job poll must still terminate itself once the job is done: {html}"
    );
    assert!(
        html.contains(r#"hx-post="/system/updates/refresh""#),
        "the Refresh button bypasses the 5-minute checkupdates TTL: {html}"
    );
}

// ---------------------------------------------------------------------------
// Dev ▸ Screenshot (its own page since IA phase 4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dev_screenshot_capture_degrades_when_bridge_not_configured() {
    let state = hermetic_state();
    let html = pages::screenshot::render_capture(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("not configured"),
        "expected a bridge-not-configured message: {html}"
    );
    assert!(
        !html.contains("<img"),
        "must never emit an <img> tag when the capture itself failed: {html}"
    );
}

// ---------------------------------------------------------------------------
// M4: Controllers page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn controllers_page_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::controllers::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "expected a daemon-unreachable marker somewhere on the page: {html}"
    );
}

#[tokio::test]
async fn controllers_page_renders_pads_bindings_and_controllerdb() {
    let mut replies = HashMap::new();
    replies.insert("status", "connected:grabbed");
    replies.insert(
        "get-pads",
        r#"[{"id":"uniq:a","index":0,"name":"Test Pad","grabbed":true}]"#,
    );
    replies.insert(
        "get-bindings",
        r#"{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START"}"#,
    );
    replies.insert(
        "get-config",
        r#"{"perGameBindings":{"steam_1":{"select":"BTN_SOUTH"}}}"#,
    );
    replies.insert(
        "controllerdb-status",
        r#"{"source":"bundled_baseline","entryCount":100,"lastDownloaded":0,"upstreamUrl":"https://example.test"}"#,
    );
    let sock = spawn_canned_daemon("controllers-page", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::controllers::render_page(&state).await;
    assert!(
        html.contains("Test Pad"),
        "expected the fleet table to render the pad: {html}"
    );
    assert!(
        html.contains("BTN_SOUTH"),
        "expected the bindings table to render the current button: {html}"
    );
    assert!(
        html.contains("steam_1"),
        "expected the per-game bindings JSON to render: {html}"
    );
    assert!(
        html.contains("bundled_baseline"),
        "expected the controllerdb status JSON to render: {html}"
    );
}

#[tokio::test]
async fn controllers_bindings_set_rejects_unknown_action_without_ipc() {
    // Validation must fail before any IPC call — no daemon needed.
    let state = hermetic_state();
    let html = pages::controllers::render_set_binding(&state, "bogus", "BTN_SOUTH").await;
    assert!(
        html.to_lowercase().contains("unknown action"),
        "expected an unknown-action error: {html}"
    );
}

#[tokio::test]
async fn controllers_bindings_set_rejects_unknown_button_without_ipc() {
    let state = hermetic_state();
    let html = pages::controllers::render_set_binding(&state, "select", "BTN_BOGUS").await;
    assert!(
        html.to_lowercase().contains("unknown button"),
        "expected an unknown-button error: {html}"
    );
}

#[tokio::test]
async fn controllers_capture_rejects_unknown_action_without_ipc() {
    let state = hermetic_state();
    let html = pages::controllers::render_capture(&state, "bogus").await;
    assert!(
        html.to_lowercase().contains("unknown action"),
        "expected an unknown-action error: {html}"
    );
}

#[tokio::test]
async fn controllers_capture_applies_captured_button_to_binding() {
    let mut replies = HashMap::new();
    replies.insert("capture-next", "captured:BTN_NORTH");
    replies.insert("set-binding select BTN_NORTH", "ok");
    replies.insert("get-bindings", r#"{"select":"BTN_NORTH"}"#);
    let sock = spawn_canned_daemon("controllers-capture", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::controllers::render_capture(&state, "select").await;
    assert!(
        html.contains("BTN_NORTH") && html.to_lowercase().contains("captured"),
        "expected the captured button to be reported and applied: {html}"
    );
}

#[tokio::test]
async fn controllers_capture_reports_timeout() {
    let mut replies = HashMap::new();
    replies.insert("capture-next", "timeout");
    replies.insert("get-bindings", "{}");
    let sock = spawn_canned_daemon("controllers-capture-timeout", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::controllers::render_capture(&state, "select").await;
    assert!(
        html.to_lowercase().contains("timed out"),
        "expected a timeout message: {html}"
    );
}

#[tokio::test]
async fn controllers_pad_rumble_rejects_out_of_range_ms() {
    let state = hermetic_state();
    let html = pages::controllers::render_pad_rumble(&state, "uniq:a", "99999").await;
    assert!(
        html.to_lowercase().contains("between"),
        "expected an out-of-range ms error: {html}"
    );
}

#[tokio::test]
async fn controllers_pad_battery_rejects_whitespace_id() {
    let state = hermetic_state();
    let html = pages::controllers::render_pad_battery(&state, "bad id").await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

#[tokio::test]
async fn controllers_active_game_set_rejects_whitespace_id() {
    let state = hermetic_state();
    let html = pages::controllers::render_active_game_set(&state, "bad id").await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

// ---------------------------------------------------------------------------
// M4: CEC page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cec_page_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::cec::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "expected a daemon-unreachable marker in the health panel: {html}"
    );
}

#[tokio::test]
async fn cec_health_ok_round_trip() {
    let mut replies = HashMap::new();
    replies.insert(
        "cec-health",
        r#"{"transmit":"ok","reason":null,"since":1719500000000,"lastError":null}"#,
    );
    let sock = spawn_canned_daemon("cec-health-ok", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::cec::render_page(&state).await;
    assert!(
        html.to_lowercase().contains("healthy"),
        "expected a healthy marker: {html}"
    );
}

#[tokio::test]
async fn cec_test_wedge_recommends_restart() {
    let mut replies = HashMap::new();
    replies.insert(
        "cec-test",
        r#"{"transmit":"failing","reason":null,"since":1719500000000,"lastError":"TransmitFailed"}"#,
    );
    let sock = spawn_canned_daemon("cec-test-wedge", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::cec::render_test(&state).await;
    assert!(
        html.to_lowercase().contains("wedge"),
        "expected a transmit-wedge marker: {html}"
    );
    assert!(
        html.contains("Restart daemon (recommended)"),
        "expected the restart step to be flagged recommended: {html}"
    );
}

#[tokio::test]
async fn cec_scan_merges_device_names_and_falls_back_to_default() {
    let mut replies = HashMap::new();
    replies.insert(
        "cec-scan",
        r#"[{"logicalAddress":0,"powerStatus":"on"},{"logicalAddress":5,"powerStatus":"standby"}]"#,
    );
    replies.insert("get-config", r#"{"cecDeviceNames":{"0":"Living Room TV"}}"#);
    let sock = spawn_canned_daemon("cec-scan", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::cec::render_scan(&state).await;
    assert!(
        html.contains("Living Room TV"),
        "expected the cecDeviceNames override to render: {html}"
    );
    assert!(
        html.contains("Audio System"),
        "expected the default name for addr 5 (no override) to render: {html}"
    );
}

#[tokio::test]
async fn cec_scan_disabled_build_renders_honest_state_not_a_failure() {
    let mut replies = HashMap::new();
    replies.insert("cec-scan", "error:unsupported on this platform");
    replies.insert("get-config", "{}");
    let sock = spawn_canned_daemon("cec-scan-disabled", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let html = pages::cec::render_scan(&state).await;
    assert!(
        html.contains("not available in this daemon build"),
        "expected the honest not-available message: {html}"
    );
    assert!(
        !html.contains("result-error"),
        "a disabled build must not render as a failure: {html}"
    );
}

#[tokio::test]
async fn cec_device_rejects_out_of_range_addr_without_ipc() {
    let state = hermetic_state();
    let html = pages::cec::render_device(&state, "99").await;
    assert!(
        html.contains("between 0 and 15"),
        "expected an out-of-range addr error: {html}"
    );
}

#[tokio::test]
async fn cec_power_on_rejects_non_integer_addr_without_ipc() {
    let state = hermetic_state();
    let html = pages::cec::render_power_on(&state, "not-a-number").await;
    assert!(
        html.to_lowercase().contains("integer"),
        "expected an invalid-addr error: {html}"
    );
}

#[tokio::test]
async fn cec_recover_restart_daemon_falls_back_to_exec_and_reports_health() {
    // Hermetic: no bridge configured and no real daemon — exercises the
    // bridge-unavailable -> direct-exec fallback path end to end (the exec
    // call itself will fail too, since `systemctl` isn't a real unit here /
    // may not exist on the test host, but the response must still degrade
    // gracefully rather than panicking).
    let state = hermetic_state();
    let html = pages::cec::render_recover_restart_daemon(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("restart-daemon"),
        "expected the restart-daemon action to be reported: {html}"
    );
}

// ===========================================================================
// S1/S2/S5 — the auth layer, proven against the REAL router
//
// These tests spin the actual `crate::build_router` behind `axum::serve` on an
// ephemeral loopback port and drive it with `reqwest` (already a dependency —
// no test-only HTTP crate). Every request is made WITHOUT valid credentials
// except where a test is explicitly about the authenticated path, so no
// mutating handler ever executes: a registered route answers 401 (auth rejects
// before the handler), an unregistered one answers 404. That distinction is
// what makes the `allow_dangerous` gate testable without ever running
// `systemctl reboot` on the machine running `cargo test`.
// ===========================================================================

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::capabilities::{CapabilitySnapshot, Gate};
use crate::state::SharedState;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Method {
    Get,
    Post,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    /// One of the four documented auth exemptions.
    Public,
    /// Gated by `auth::require_auth`.
    Authenticated,
}

use Access::{Authenticated, Public};
use Method::{Get, Post};

/// One row of the route table. `declared` must match `main.rs` verbatim
/// (placeholders intact); `request` is a concrete path with the placeholders
/// substituted so the row can actually be dispatched.
struct RouteSpec {
    declared: &'static str,
    request: &'static str,
    method: Method,
    access: Access,
    /// The registration block this route sits in — [`Gate::Recovery`] for the
    /// unconditional chain. Asserted against what the `main.rs` parser
    /// attributes, so the table and the router cannot drift.
    gate: Gate,
    /// Part of the S5 root-equivalent set — registered only under
    /// `[panel].allow_dangerous = true`. Orthogonal to `gate`: `/dev/deploy`
    /// and `/dev/build` are both dangerous AND behind [`Gate::DevDeploy`].
    dangerous: bool,
    /// This handler shells out (`systemctl`, `pacman`, the daemon bridge's
    /// deploy/build) rather than just rendering. Probing such a route with its
    /// real method would EXECUTE it if the gate under test were broken — on a
    /// live HTPC that means a reboot. They are probed with [`Method::Get`]
    /// instead: an unexempt path still answers 401 when it is registered and
    /// gated, 404 when it is not registered, and 405 (harmlessly) if the auth
    /// layer ever went missing — the handler is never reached either way.
    exec_backed: bool,
}

const fn r(
    declared: &'static str,
    request: &'static str,
    method: Method,
    access: Access,
) -> RouteSpec {
    RouteSpec {
        declared,
        request,
        method,
        access,
        gate: Gate::Recovery,
        dangerous: false,
        exec_backed: false,
    }
}

/// A registered-but-exec-backed recovery route (not part of the S5 set).
const fn recovery(declared: &'static str, request: &'static str, method: Method) -> RouteSpec {
    RouteSpec {
        declared,
        request,
        method,
        access: Authenticated,
        gate: Gate::Recovery,
        dangerous: false,
        exec_backed: true,
    }
}

const fn danger(declared: &'static str, method: Method) -> RouteSpec {
    RouteSpec {
        declared,
        request: declared,
        method,
        access: Authenticated,
        gate: Gate::Recovery,
        dangerous: true,
        exec_backed: true,
    }
}

/// Move a batch of rows onto `gate` — the table's mirror of one `let app = if
/// caps.allows(Gate::..)` block in `main.rs`.
fn on(gate: Gate, rows: Vec<RouteSpec>) -> Vec<RouteSpec> {
    rows.into_iter().map(|s| RouteSpec { gate, ..s }).collect()
}

/// The method to probe a row with — see [`RouteSpec::exec_backed`].
fn probe_method(spec: &RouteSpec) -> Method {
    if spec.exec_backed {
        Method::Get
    } else {
        spec.method
    }
}

/// THE table. `route_table_matches_main_rs_declarations` asserts this is
/// exactly the set of routes `main.rs` declares — path, method, gate AND
/// dangerous-ness — so adding a route without adding it here, or moving one
/// between registration blocks, fails the suite.
fn route_table() -> Vec<RouteSpec> {
    // ── Recovery tier: registered unconditionally ──
    let mut table = vec![
        r("/", "/", Get, Authenticated),
        r("/overview", "/overview", Get, Authenticated),
        r("/overview/tiles", "/overview/tiles", Get, Authenticated),
        r(
            "/overview/updates-tile",
            "/overview/updates-tile",
            Get,
            Authenticated,
        ),
        r(
            "/overview/services-tile",
            "/overview/services-tile",
            Get,
            Authenticated,
        ),
        r("/system/services", "/system/services", Get, Authenticated),
        // Read-only (`systemctl show`), and probed with no query at all, so
        // the handler renders the empty prompt without spawning anything.
        r(
            "/system/services/inspect",
            "/system/services/inspect",
            Get,
            Authenticated,
        ),
        // Deliberately an unknown unit key: the handler rejects it before
        // touching `systemctl`, so this row is inert even if probed.
        recovery(
            "/system/services/restart/{key}",
            "/system/services/restart/not-a-unit",
            Post,
        ),
        r("/system/processes", "/system/processes", Get, Authenticated),
        r("/system/updates", "/system/updates", Get, Authenticated),
        r(
            "/system/updates/refresh",
            "/system/updates/refresh",
            Post,
            Authenticated,
        ),
        r(
            "/system/updates/job",
            "/system/updates/job",
            Get,
            Authenticated,
        ),
        r("/system/logs", "/system/logs", Get, Authenticated),
        r("/system/logs/view", "/system/logs/view", Get, Authenticated),
        r("/dev/recovery", "/dev/recovery", Get, Authenticated),
        // Recovery, deliberately NOT part of the dangerous set: these restart
        // the very same units `POST /system/services/restart/{key}` restarts.
        recovery("/dev/restart-daemon", "/dev/restart-daemon", Post),
        recovery("/dev/restart-shell", "/dev/restart-shell", Post),
        r(
            "/nav/daemon-status",
            "/nav/daemon-status",
            Get,
            Authenticated,
        ),
        // ── forwarding addresses for the pre-IA paths (recovery tier half) ──
        r("/dashboard", "/dashboard", Get, Authenticated),
        r("/processes", "/processes", Get, Authenticated),
        r("/logs", "/logs", Get, Authenticated),
        r("/dev", "/dev", Get, Authenticated),
        // ── the four auth exemptions ──
        r("/login", "/login", Get, Public),
        r("/login", "/login", Post, Public),
        r("/assets/htmx.min.js", "/assets/htmx.min.js", Get, Public),
        r("/assets/style.css", "/assets/style.css", Get, Public),
    ];

    // ── Node tier: the IPC surface, registered iff a node answered ──
    //
    // Phase 4 dissolved the Tools console into the four pages that own its
    // subjects, plus the two power probes on a page registered elsewhere.
    table.extend(on(
        Gate::Node,
        vec![
            r("/devices/network", "/devices/network", Get, Authenticated),
            r(
                "/devices/network/status",
                "/devices/network/status",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/wifi-list",
                "/devices/network/wifi-list",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/wifi-rescan",
                "/devices/network/wifi-rescan",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/throughput",
                "/devices/network/throughput",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/ping",
                "/devices/network/ping",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/power-status",
                "/devices/network/bt/power-status",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/power-on",
                "/devices/network/bt/power-on",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/power-off",
                "/devices/network/bt/power-off",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/scan-on",
                "/devices/network/bt/scan-on",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/scan-off",
                "/devices/network/bt/scan-off",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/list",
                "/devices/network/bt/list",
                Post,
                Authenticated,
            ),
            r(
                "/devices/network/bt/action",
                "/devices/network/bt/action",
                Post,
                Authenticated,
            ),
            // Two node-tier routes whose PAGE is in the `settings_store`
            // block — same one-capability-per-block reason as the CEC and
            // Input saves below, in the other direction. Display & Audio
            // renders these two buttons only under `Gate::Node`.
            r(
                "/devices/display-audio/power/can-suspend",
                "/devices/display-audio/power/can-suspend",
                Post,
                Authenticated,
            ),
            r(
                "/devices/display-audio/power/battery",
                "/devices/display-audio/power/battery",
                Post,
                Authenticated,
            ),
            r(
                "/remote/navigation",
                "/remote/navigation",
                Get,
                Authenticated,
            ),
            r(
                "/remote/navigation/intent",
                "/remote/navigation/intent",
                Post,
                Authenticated,
            ),
            r(
                "/remote/navigation/key",
                "/remote/navigation/key",
                Post,
                Authenticated,
            ),
            r("/remote/launcher", "/remote/launcher", Get, Authenticated),
            r(
                "/remote/launcher/list",
                "/remote/launcher/list",
                Post,
                Authenticated,
            ),
            r(
                "/remote/launcher/launch",
                "/remote/launcher/launch",
                Post,
                Authenticated,
            ),
            r(
                "/remote/launcher/recents",
                "/remote/launcher/recents",
                Post,
                Authenticated,
            ),
            // The console PAGE is node tier; `POST /dev/console/raw` is in the
            // danger set below.
            r("/dev/console", "/dev/console", Get, Authenticated),
            r("/tools", "/tools", Get, Authenticated),
        ],
    ));

    // ── Capability tier: `Feature::Controllers` ──
    table.extend(on(
        Gate::Controllers,
        vec![
            r(
                "/devices/controllers",
                "/devices/controllers",
                Get,
                Authenticated,
            ),
            r("/controllers", "/controllers", Get, Authenticated),
            r(
                "/devices/controllers/grab",
                "/devices/controllers/grab",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/release",
                "/devices/controllers/release",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/handoff",
                "/devices/controllers/handoff",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/pad/battery",
                "/devices/controllers/pad/battery",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/pad/rumble-status",
                "/devices/controllers/pad/rumble-status",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/pad/rumble",
                "/devices/controllers/pad/rumble",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/input-devices",
                "/devices/controllers/input-devices",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/bindings/set",
                "/devices/controllers/bindings/set",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/bindings/capture",
                "/devices/controllers/bindings/capture",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/bindings/capture-cancel",
                "/devices/controllers/bindings/capture-cancel",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/active-game/set",
                "/devices/controllers/active-game/set",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/active-game/clear",
                "/devices/controllers/active-game/clear",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/controllerdb/status",
                "/devices/controllers/controllerdb/status",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/controllerdb/refresh",
                "/devices/controllers/controllerdb/refresh",
                Post,
                Authenticated,
            ),
        ],
    ));

    // ── Capability tier: `Feature::Cec` ──
    table.extend(on(
        Gate::Cec,
        vec![
            r("/devices/cec", "/devices/cec", Get, Authenticated),
            r("/cec", "/cec", Get, Authenticated),
            r(
                "/devices/cec/scan",
                "/devices/cec/scan",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/device",
                "/devices/cec/device",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/active-source",
                "/devices/cec/active-source",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/power-on",
                "/devices/cec/power-on",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/power-off",
                "/devices/cec/power-off",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/test",
                "/devices/cec/test",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/osd-name",
                "/devices/cec/osd-name",
                Post,
                Authenticated,
            ),
            // A unit restart under a gated prefix, on purpose: it is the CEC
            // page's own recovery ladder rung, and the two ALWAYS-registered
            // paths to that same unit are untouched.
            recovery(
                "/devices/cec/recover/restart-daemon",
                "/devices/cec/recover/restart-daemon",
                Post,
            ),
        ],
    ));

    // ── Capability tier: `Feature::Widgets` ──
    table.extend(on(
        Gate::Widgets,
        vec![
            r("/shell/widgets", "/shell/widgets", Get, Authenticated),
            r("/widgets", "/widgets", Get, Authenticated),
            r(
                "/shell/widgets/save",
                "/shell/widgets/save",
                Post,
                Authenticated,
            ),
            r(
                "/shell/widgets/reorder/{id}/up",
                "/shell/widgets/reorder/plex/up",
                Post,
                Authenticated,
            ),
            r(
                "/shell/widgets/reorder/{id}/down",
                "/shell/widgets/reorder/plex/down",
                Post,
                Authenticated,
            ),
        ],
    ));

    // ── Capability tier: `Feature::SettingsStore` ──
    table.extend(on(
        Gate::SettingsStore,
        vec![
            // The five pages the Settings page dissolved into (phase 3), plus
            // the two save routes whose PAGES sit in another block: a block
            // condition may name only one capability, and `set-config` is the
            // one these need. Each renders its form only under
            // `Gate::SettingsStore`, so neither is a control pointing at an
            // unregistered route.
            r("/shell/appearance", "/shell/appearance", Get, Authenticated),
            r(
                "/shell/appearance/save",
                "/shell/appearance/save",
                Post,
                Authenticated,
            ),
            r("/shell/apps", "/shell/apps", Get, Authenticated),
            r("/shell/apps/save", "/shell/apps/save", Post, Authenticated),
            r("/shell/advanced", "/shell/advanced", Get, Authenticated),
            r(
                "/shell/advanced/raw",
                "/shell/advanced/raw",
                Post,
                Authenticated,
            ),
            r(
                "/devices/display-audio",
                "/devices/display-audio",
                Get,
                Authenticated,
            ),
            r(
                "/devices/display-audio/save",
                "/devices/display-audio/save",
                Post,
                Authenticated,
            ),
            r(
                "/devices/cec/config",
                "/devices/cec/config",
                Post,
                Authenticated,
            ),
            r(
                "/devices/controllers/settings/save",
                "/devices/controllers/settings/save",
                Post,
                Authenticated,
            ),
            // The whole wallpaper surface moved here from the recovery tier
            // (`docs/PANEL_IA.md` phase 1): selecting one always needed
            // `set-config`, and gating the rest with it is what lets the Shell
            // group vanish cleanly with the daemon down. The accepted cost is
            // that wallpaper UPLOAD now needs the handshake to have succeeded.
            // Phase 4 re-prefixed them onto the page that now owns them, and
            // deleted the Media page they came from.
            r(
                "/shell/appearance/wallpaper/upload",
                "/shell/appearance/wallpaper/upload",
                Post,
                Authenticated,
            ),
            r(
                "/shell/appearance/wallpaper/delete",
                "/shell/appearance/wallpaper/delete",
                Post,
                Authenticated,
            ),
            r(
                "/shell/appearance/wallpaper/file",
                "/shell/appearance/wallpaper/file",
                Get,
                Authenticated,
            ),
            r(
                "/shell/appearance/wallpaper/select",
                "/shell/appearance/wallpaper/select",
                Post,
                Authenticated,
            ),
            r("/settings", "/settings", Get, Authenticated),
            r("/media", "/media", Get, Authenticated),
        ],
    ));

    // ── Capability tier: `Feature::WebApps` ──
    table.extend(on(
        Gate::WebApps,
        vec![
            r(
                "/shell/apps/webapp/add",
                "/shell/apps/webapp/add",
                Post,
                Authenticated,
            ),
            r(
                "/shell/apps/webapp/remove",
                "/shell/apps/webapp/remove",
                Post,
                Authenticated,
            ),
        ],
    ));

    // ── Capability tier: `Feature::Screenshot` ──
    table.extend(on(
        Gate::Screenshot,
        vec![
            // `/dev/screenshot` is the PAGE as of phase 4; the PNG proxy it
            // used to be moved to `/dev/screenshot/image`.
            r("/dev/screenshot", "/dev/screenshot", Get, Authenticated),
            r(
                "/dev/screenshot/image",
                "/dev/screenshot/image",
                Get,
                Authenticated,
            ),
            r(
                "/dev/screenshot/capture",
                "/dev/screenshot/capture",
                Post,
                Authenticated,
            ),
        ],
    ));

    // ── Danger ∩ capability: `allow_dangerous` AND `Feature::DevDeploy` ──
    table.extend(on(
        Gate::DevDeploy,
        vec![danger("/dev/deploy", Post), danger("/dev/build", Post)],
    ));

    // ── the rest of the S5 root-equivalent set (danger only) ──
    table.extend([
        danger("/dev/reboot", Post),
        danger("/dev/suspend", Post),
        danger("/system/updates/apply", Post),
        danger("/dev/console/raw", Post),
    ]);

    table
}

/// Every route registered UNCONDITIONALLY (the recovery tier) whose method
/// mutates, each with the reason it is allowed to be ungated.
///
/// **This list is the gate.** `unconditional_mutating_routes_are_an_explicit_allowlist`
/// fails the suite on any unconditional `post` that is not here, so a new
/// ungated mutating route is a test failure rather than a review comment
/// (`docs/MULTI_NODE_PANEL.md` §1).
const RECOVERY_TIER_MUTATING: [(&str, &str); 5] = [
    (
        "/system/services/restart/{key}",
        "restarting a wedged systemd unit is the reason the panel exists; \
         panel-local exec, no node involved",
    ),
    (
        "/system/updates/refresh",
        "runs the panel's own unprivileged `checkupdates`; the daemon declares \
         no `system_updates` capability because it does not serve one",
    ),
    (
        "/dev/restart-daemon",
        "same systemd unit as `/system/services/restart/{key}` — recovery",
    ),
    (
        "/dev/restart-shell",
        "same reasoning, for the Quickshell unit",
    ),
    (
        "/login",
        "an auth exemption by definition: it mints the session cookie, so it \
         cannot itself require one — and it touches nothing but that cookie",
    ),
];

// ── the `main.rs` parser ───────────────────────────────────────────────────

/// One route declaration read out of `main.rs`, with the registration block it
/// sits in.
#[derive(Debug, PartialEq, Eq)]
struct Declared {
    path: String,
    method: Method,
    gate: Gate,
    dangerous: bool,
}

/// One `let app = if <condition> { .. } else { app };` block in `build_router`.
struct Block {
    /// Byte offset of the block body's opening brace.
    start: usize,
    /// Byte offset one past the block's last route declaration.
    end: usize,
    gate: Gate,
    dangerous: bool,
}

/// The literal form every conditional registration block opens with. The
/// parser understands exactly this; anything else panics.
const BLOCK_OPEN: &str = "let app = if ";

/// Replace every whole-line `//` comment with spaces, **preserving byte
/// offsets**, so prose can mention `if` or a path without confusing the
/// structural scan below. Block comments are rejected outright rather than
/// half-handled.
fn blank_comment_lines(src: &str) -> String {
    assert!(
        !src.contains("/*"),
        "main.rs uses a /* block comment */; the registration-block parser only \
         knows how to blank whole-line `//` comments. Use `//` or teach it."
    );
    src.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                " ".repeat(l.len())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolve a block condition to the tier it registers.
///
/// **Strict on purpose.** An unrecognized condition means the parser cannot say
/// which tier a route landed in, and an unattributed route is an unchecked
/// route — so this panics rather than guessing or skipping.
fn tier_for_condition(cond: &str) -> (Gate, bool) {
    let teach = || -> ! {
        panic!(
            "main.rs: registration block gated on `{cond}`, a condition this parser \
             does not recognize. It understands exactly `allow_dangerous`, \
             `caps.allows(Gate::<Variant>)`, and the two ANDed. Teach \
             `tier_for_condition` the new form AND give the routes a tier in \
             `route_table()` — an unattributed route is an UNCHECKED route."
        )
    };
    if cond == "allow_dangerous" {
        return (Gate::Recovery, true);
    }
    let (dangerous, rest) = match cond.strip_prefix("allow_dangerous && ") {
        Some(rest) => (true, rest),
        None => (false, cond),
    };
    let Some(ident) = rest
        .strip_prefix("caps.allows(Gate::")
        .and_then(|s| s.strip_suffix(')'))
    else {
        teach()
    };
    let Some(gate) = Gate::ALL.iter().copied().find(|g| g.ident() == ident) else {
        teach()
    };
    assert_ne!(
        gate,
        Gate::Recovery,
        "main.rs: a block gated on `Gate::Recovery` is a no-op — that gate is \
         always true. Register those routes in the unconditional chain."
    );
    (gate, dangerous)
}

/// The body of `build_router`, comment-blanked — the only region routes may be
/// declared in.
///
/// Scoping to one function matters: `main` also binds `let app =
/// build_router(state)`, and the block scan below counts `app` bindings to
/// prove no route escaped into an unmodelled form. It also asserts every
/// `.route(` in the file really is inside this function.
fn router_body(src: &str) -> String {
    let code = blank_comment_lines(src);
    let start = code
        .find("fn build_router(")
        .expect("main.rs must define build_router");
    let body = code[start..].to_string();
    assert_eq!(
        body.matches(".route(").count(),
        src.matches(".route(").count(),
        "main.rs declares a route OUTSIDE `build_router` — routes must all live in \
         the one function the tier parser reads"
    );
    body
}

/// Find every conditional registration block, and refuse to proceed if
/// `build_router` shapes its router any other way.
fn parse_blocks(code: &str) -> Vec<Block> {
    let opens: Vec<usize> = code.match_indices(BLOCK_OPEN).map(|(i, _)| i).collect();

    // `app` is bound exactly once for the unconditional chain plus once per
    // block. Any other binding form — `let app: Router = ..`, a rebind inside
    // a block, a helper that returns a Router — would move routes somewhere
    // the offset attribution below cannot see them.
    assert_eq!(
        code.matches("let app").count(),
        opens.len() + 1,
        "main.rs binds `app` in a form this parser does not model (expected one \
         `let app = Router::new()` plus one `{BLOCK_OPEN}..` per block). Teach \
         `parse_blocks` — an unattributed route is an UNCHECKED route."
    );

    let layer = code
        .find("app.route_layer(")
        .expect("main.rs must attach the auth layer with route_layer");

    let mut blocks = Vec::new();
    for (n, &open) in opens.iter().enumerate() {
        let after = open + BLOCK_OPEN.len();
        let brace = code[after..].find(" {").unwrap_or_else(|| {
            panic!("main.rs: `{BLOCK_OPEN}` at byte {open} has no opening brace")
        });
        let cond = code[after..after + brace].trim();
        let (gate, dangerous) = tier_for_condition(cond);
        let start = after + brace;
        let end = opens.get(n + 1).copied().unwrap_or(layer);

        // No nested conditional inside a block: routes registered under one
        // would inherit this block's tier while actually being gated on
        // something else entirely.
        assert!(
            !code[start..end].contains(" if "),
            "main.rs: the `{cond}` block contains a nested conditional. Every \
             route must sit in exactly one flat registration block — split it \
             into its own `{BLOCK_OPEN}..` block, or teach the parser."
        );
        blocks.push(Block {
            start,
            end,
            gate,
            dangerous,
        });
    }
    blocks
}

/// Parse every `.route("<path>", get|post` declaration out of `main.rs`, and
/// attribute each to the registration block it sits in.
///
/// Multi-line tolerant: rustfmt wraps a number of these across three lines, so
/// whitespace (including newlines) is skipped between every token rather than
/// matching a single line.
///
/// **Every `.route(` must parse.** An earlier version `continue`d past any
/// form it did not recognize, which made a route declared with a method other
/// than `get`/`post` — `.route("/x", axum::routing::put(h))` — vanish from the
/// table silently, so the route-completeness gate below passed while an ungated
/// route was live. Anything unrecognized now panics with the offending snippet:
/// a new method form must be taught to this parser (and added to
/// `route_table()`), never skipped.
fn parse_declared_routes(src: &str) -> Vec<Declared> {
    /// The offending declaration, clipped, for the panic message.
    fn snippet(s: &str) -> String {
        s.chars().take(80).collect::<String>().replace('\n', " ")
    }

    assert_eq!(
        blank_comment_lines(src).matches(".route(").count(),
        src.matches(".route(").count(),
        "main.rs mentions `.route(` inside a comment — the structural scan blanks \
         comment lines, so the two counts must agree or a real declaration could \
         be hiding behind a `//`"
    );
    let code = router_body(src);
    let code = code.as_str();
    let blocks = parse_blocks(code);

    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = code[pos..].find(".route(") {
        let at = pos + rel;
        pos = at + ".route(".len();
        let rest = &code[pos..];

        let after_open = rest.trim_start();
        let quoted = after_open.strip_prefix('"').unwrap_or_else(|| {
            panic!(
                "main.rs: `.route(` whose first argument is not a string literal — \
                 the route-completeness gate cannot see it: `{}`",
                snippet(after_open)
            )
        });
        let end = quoted.find('"').unwrap_or_else(|| {
            panic!(
                "main.rs: `.route(` with an unterminated path literal: `{}`",
                snippet(quoted)
            )
        });
        let path = &quoted[..end];
        let after_path = quoted[end + 1..].trim_start();
        let after_comma = after_path.strip_prefix(',').unwrap_or_else(|| {
            panic!(
                "main.rs: `.route(\"{path}\"` is not followed by `, <method>(`: `{}`",
                snippet(after_path)
            )
        });
        let after_comma = after_comma.trim_start();
        let method = if after_comma.starts_with("get(") {
            Method::Get
        } else if after_comma.starts_with("post(") {
            Method::Post
        } else {
            panic!(
                "main.rs: `.route(\"{path}\"` uses a method form this parser does not \
                 recognize (only bare `get(` / `post(` are): `{}`. Teach \
                 `parse_declared_routes` the new form AND add the route to \
                 `route_table()` — an unparsed route is an UNCHECKED route.",
                snippet(after_comma)
            )
        };

        // Attribution: the block whose body contains this declaration, else
        // the unconditional recovery chain.
        let (gate, dangerous) = blocks
            .iter()
            .find(|b| at >= b.start && at < b.end)
            .map(|b| (b.gate, b.dangerous))
            .unwrap_or((Gate::Recovery, false));

        out.push(Declared {
            path: path.to_string(),
            method,
            gate,
            dangerous,
        });
    }
    out
}

/// Ways to register a handler that the parser above and/or the `route_layer`
/// auth gate would not see. `route_service` bypasses the method-router form
/// the parser reads; `.merge(`/`.nest(`/`.nest_service(` graft in a sub-router
/// whose routes are never named in `main.rs`; `.fallback(` registers a
/// catch-all that `route_layer` (matched routes only) does not wrap.
const ROUTER_FORMS_THE_GATE_CANNOT_SEE: [&str; 5] = [
    "route_service",
    ".merge(",
    ".nest(",
    ".nest_service(",
    ".fallback(",
];

/// The parser must see EVERY `.route(` in `main.rs`, every one of them must be
/// declared BEFORE the auth layer, and no route may enter the router by a form
/// the parser cannot read.
///
/// This is the gate that closes the hole `route_table_matches_main_rs_declarations`
/// alone left open: `Router::route_layer` documents that "routes added after
/// this call are not wrapped", so a `.route(...)` appended below `.route_layer(...)`
/// is live and unauthenticated no matter how correct the table is.
#[test]
fn every_route_is_visible_to_the_parser_and_declared_before_the_auth_layer() {
    let src = include_str!("main.rs");

    let declared = parse_declared_routes(src);
    assert_eq!(
        declared.len(),
        src.matches(".route(").count(),
        "the parser did not account for every `.route(` in main.rs"
    );

    let last_route = src
        .rfind(".route(")
        .expect("main.rs must declare at least one route");
    let layer = src
        .find(".route_layer(")
        .expect("main.rs must attach the auth layer with route_layer");
    assert!(
        last_route < layer,
        "a `.route(` is declared AFTER `.route_layer(` — axum does not wrap routes \
         added after that call, so it would serve unauthenticated"
    );

    for form in ROUTER_FORMS_THE_GATE_CANNOT_SEE {
        assert!(
            !src.contains(form),
            "main.rs uses `{form}`, which registers a handler the route-completeness \
             gate cannot see. Register routes with `.route(\"<path>\", get|post(..))` \
             before `.route_layer(..)`, or extend this test to cover the new form."
        );
    }
}

/// The gate that stops the auth layer regressing silently: the table above
/// must be EXACTLY the set of routes `main.rs` declares. axum's `Router` has
/// no introspection API, so the source is the source of truth.
///
/// Since the capability tiers landed this also pins the TIER of every route:
/// moving a `/cec/` route out of its block and into the unconditional chain is
/// a drift the table catches, not something a reviewer has to notice.
#[test]
fn route_table_matches_main_rs_declarations() {
    let declared = parse_declared_routes(include_str!("main.rs"));
    let table = route_table();

    let declared_paths: BTreeSet<&str> = declared.iter().map(|d| d.path.as_str()).collect();
    let table_paths: BTreeSet<&str> = table.iter().map(|r| r.declared).collect();
    assert_eq!(
        declared_paths, table_paths,
        "main.rs declares a route the auth route table doesn't cover (or vice versa) — \
         add it to `route_table()` with its access class"
    );

    // Stronger than the path-set check: also pin the (path, method, gate,
    // dangerous) tuples, so adding a second method to an existing path — or
    // registering an existing one behind a different gate — can't slip through.
    let mut declared_rows: Vec<(&str, Method, &'static str, bool)> = declared
        .iter()
        .map(|d| (d.path.as_str(), d.method, d.gate.ident(), d.dangerous))
        .collect();
    let mut table_rows: Vec<(&str, Method, &'static str, bool)> = table
        .iter()
        .map(|r| (r.declared, r.method, r.gate.ident(), r.dangerous))
        .collect();
    declared_rows.sort_unstable();
    table_rows.sort_unstable();
    assert_eq!(
        declared_rows, table_rows,
        "a route's registration TIER in main.rs disagrees with `route_table()` — \
         one of the two moved"
    );

    assert_eq!(
        declared.len(),
        108,
        "expected the 109 routes phase 3 left, then phase 4: 6 deleted outright \
         (the four `/tools/sys/*` probes, already on the Overview tiles, and the \
         two `controllerdb-*` duplicates of the Controllers page's own), the \
         Media and Tools page GETs gone, and `/dev/screenshot/image` net-new \
         (the PNG proxy renamed to free `/dev/screenshot` for the page) — then \
         phase 5's single net-new route, `GET /system/services/inspect`, and \
         phase 6's single net-new route, `GET /overview/services-tile` (the \
         system-services tile's own poll target — Overview ADDED no mutating \
         route, and removed none, because its actions had already moved). \
         Everything else moved rather than being added (docs/PANEL_IA.md)"
    );
}

/// **The ungated-mutating-route gate.** Every `post` registered in the
/// unconditional (recovery) chain must be justified in
/// [`RECOVERY_TIER_MUTATING`]; anything else has to sit behind a capability,
/// the node, or `allow_dangerous`.
///
/// `docs/MULTI_NODE_PANEL.md`: *"Registering an ungated mutating route should
/// be a test failure, not a review comment."* This is that failure.
///
/// **Known limit — it keys on the DECLARED METHOD, not on what the handler
/// does.** A mutating handler registered as `get(..)` in the unconditional
/// chain passes this gate untouched. There is no such route today (every
/// unconditional `get` renders or reads), and axum gives no way to ask a
/// handler whether it mutates, so the honest coverage statement is "no ungated
/// `post`" rather than "no ungated mutation". A reviewer still owns the
/// method choice; this owns everything downstream of it.
#[test]
fn unconditional_mutating_routes_are_an_explicit_allowlist() {
    let declared = parse_declared_routes(include_str!("main.rs"));

    let allowed: BTreeSet<&str> = RECOVERY_TIER_MUTATING.iter().map(|(p, _)| *p).collect();
    assert_eq!(
        allowed.len(),
        RECOVERY_TIER_MUTATING.len(),
        "duplicate entry in RECOVERY_TIER_MUTATING"
    );
    for (path, reason) in RECOVERY_TIER_MUTATING {
        assert!(
            !reason.trim().is_empty(),
            "{path} is allowlisted with no written reason"
        );
    }

    let ungated: BTreeSet<&str> = declared
        .iter()
        .filter(|d| d.method == Post && d.gate == Gate::Recovery && !d.dangerous)
        .map(|d| d.path.as_str())
        .collect();

    let unjustified: Vec<&str> = ungated.difference(&allowed).copied().collect();
    assert!(
        unjustified.is_empty(),
        "main.rs registers mutating route(s) {unjustified:?} unconditionally — no \
         capability gate, no node gate, no `allow_dangerous`, and not in \
         RECOVERY_TIER_MUTATING. Either move it behind the gate the node actually \
         declares for it, or add it to RECOVERY_TIER_MUTATING **with the reason it \
         must work when the daemon is down**."
    );

    let stale: Vec<&str> = allowed.difference(&ungated).copied().collect();
    assert!(
        stale.is_empty(),
        "RECOVERY_TIER_MUTATING lists {stale:?}, which main.rs no longer registers \
         unconditionally — drop the entry so the allowlist stays a statement about \
         the real router"
    );
}

/// The exemption list is exactly four routes — and the middleware's own
/// `PUBLIC_PATHS` covers exactly their paths.
#[test]
fn public_routes_are_exactly_the_four_documented_exemptions() {
    let table = route_table();
    let mut public: Vec<(Method, &str)> = table
        .iter()
        .filter(|r| r.access == Public)
        .map(|r| (r.method, r.declared))
        .collect();
    public.sort_unstable();
    assert_eq!(
        public,
        vec![
            (Get, "/assets/htmx.min.js"),
            (Get, "/assets/style.css"),
            (Get, "/login"),
            (Post, "/login"),
        ]
    );

    let public_paths: BTreeSet<&str> = public.iter().map(|(_, p)| *p).collect();
    let middleware_paths: BTreeSet<&str> = crate::auth::PUBLIC_PATHS.iter().copied().collect();
    assert_eq!(public_paths, middleware_paths);
}

/// The S5 set, pinned. The rule: **restarting a unit is recovery** (ungated);
/// **changing what code runs, powering the box, or running arbitrary commands
/// is root-equivalent** (gated). So every `restart` route — including
/// `/dev/restart-daemon` and `/dev/restart-shell`, which drive the SAME systemd
/// units as `POST /system/services/restart/{key}` — is deliberately absent.
#[test]
fn dangerous_set_is_exactly_the_root_equivalent_actions() {
    let table = route_table();
    let mut dangerous: Vec<&str> = table
        .iter()
        .filter(|r| r.dangerous)
        .map(|r| r.declared)
        .collect();
    dangerous.sort_unstable();
    assert_eq!(
        dangerous,
        vec![
            "/dev/build",
            "/dev/console/raw",
            "/dev/deploy",
            "/dev/reboot",
            "/dev/suspend",
            "/system/updates/apply",
        ]
    );
    for recovery_route in RECOVERY_ROUTES {
        assert!(
            table
                .iter()
                .any(|r| r.declared == recovery_route && !r.dangerous),
            "{recovery_route} restarts a unit — it is recovery and must stay ungated"
        );
    }
}

/// **#408's headline claim, checked rather than restated.** Moving the raw
/// console to Dev was supposed to leave every `allow_dangerous`-gated control
/// in one group. It very nearly does — and the exception is named here rather
/// than quietly dropped, because a claim the code does not support is worse
/// than a documented exception.
///
/// `POST /system/updates/apply` is the one dangerous route outside `/dev/`. It
/// stays where it is deliberately: it is the button at the bottom of the
/// pending-package table on System ▸ Updates, sharing that page's background
/// job and its self-terminating status poll. Moving the button to Dev would
/// separate it from the list it applies and the log tail it produces, which is
/// a worse page for a marginal gain in tidiness. `docs/PANEL.md` and
/// `docs/PANEL_IA.md` both state the claim with this exception attached.
#[test]
fn the_dangerous_set_is_the_dev_group_plus_the_updates_apply() {
    let table = route_table();
    let dangerous: BTreeSet<&str> = table
        .iter()
        .filter(|r| r.dangerous)
        .map(|r| r.declared)
        .collect();

    const UPDATES_APPLY: &str = "/system/updates/apply";
    let outside: Vec<&str> = dangerous
        .iter()
        .copied()
        .filter(|p| !p.starts_with("/dev/") && *p != UPDATES_APPLY)
        .collect();
    assert!(
        outside.is_empty(),
        "every allow_dangerous route must live under /dev/ (the Dev group) — \
         {outside:?} does not. The ONE documented exception is {UPDATES_APPLY}; \
         adding a second means the claim in docs/PANEL.md is no longer true and \
         must be rewritten, not extended"
    );
    assert!(
        dangerous.contains(UPDATES_APPLY),
        "the exception must still exist — if the pacman apply moved into Dev, \
         drop it from this test and strengthen the claim in the docs"
    );
    // And the console really did land in Dev rather than staying on a
    // general-purpose page.
    assert!(
        dangerous.contains("/dev/console/raw"),
        "the raw IPC console is the route this phase moved: {dangerous:?}"
    );
}

/// The unit-restart routes: recovery, always registered, never in the S5 set.
/// `/dev/restart-{daemon,shell}` and `/system/services/restart/{key}` hit the same
/// two systemd units, so gating one and leaving the other open bought nothing.
const RECOVERY_ROUTES: [&str; 4] = [
    "/system/services/restart/{key}",
    "/devices/cec/recover/restart-daemon",
    "/dev/restart-daemon",
    "/dev/restart-shell",
];

// ── live-router harness ────────────────────────────────────────────────────

const TEST_TOKEN: &str = "panel-test-token";

fn cfg_authenticated(allow_dangerous: bool) -> AppConfig {
    AppConfig {
        panel_token_file: Some("~/.config/tv-shell/panel-token".to_string()),
        panel_token: Some(TEST_TOKEN.to_string()),
        allow_dangerous,
        ..AppConfig::default()
    }
}

/// The capability set **htpc-1's real daemon build declares**, derived line by
/// line from `daemon/src/ipc.rs::features()` for that build — Linux,
/// `--features cec,mcp` (`scripts/build-daemon.sh`), `[http].bind` set:
///
/// | Gate in `features()` | Emits |
/// |---|---|
/// | always compiled in | `settings_store`, `widgets`, `web_apps` |
/// | `cfg!(feature = "cec")` | `cec` |
/// | `cfg!(target_os = "linux")` | `controllers`, `shell_lifecycle`, `sleep` |
/// | `http \|\| mcp` | `logs`, `dev_deploy` |
/// | linux **and** (`http \|\| mcp`) | `screenshot` |
///
/// **`wallpapers`, `processes`, `system_updates`, `steam_library` and
/// `game_launch` are deliberately absent** — the daemon serves none of them.
/// Gating a route on one would delete a working page from this node, which is
/// exactly what `htpc_1_declared_set_registers_todays_entire_route_set` exists
/// to catch.
fn htpc_1_features() -> BTreeSet<Feature> {
    [
        Feature::SettingsStore,
        Feature::Widgets,
        Feature::WebApps,
        Feature::Cec,
        Feature::Controllers,
        Feature::ShellLifecycle,
        Feature::Sleep,
        Feature::Logs,
        Feature::Screenshot,
        Feature::DevDeploy,
    ]
    .into_iter()
    .collect()
}

/// A successful handshake declaring `features`.
fn caps_with(features: BTreeSet<Feature>) -> CapabilitySnapshot {
    CapabilitySnapshot {
        handshake: crate::capabilities::Handshake::Ok,
        node_id: "htpc-1".to_string(),
        features,
    }
}

/// Every feature any [`Gate`] gates on — the maximal registered surface.
fn every_gated_feature() -> BTreeSet<Feature> {
    Gate::ALL.iter().filter_map(|g| g.feature()).collect()
}

fn state_with(cfg: AppConfig) -> SharedState {
    state_with_caps(cfg, caps_with(every_gated_feature()))
}

fn state_with_caps(cfg: AppConfig, caps: CapabilitySnapshot) -> SharedState {
    let sock = std::path::PathBuf::from(format!(
        "/tmp/tvshp-router-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ));
    let bridge = Arc::new(BridgeClient::new(
        cfg.http_bridge_base.clone(),
        cfg.http_token.clone(),
    ));
    Arc::new(AppState {
        cfg,
        caps,
        node: Arc::new(IpcTransport::new(sock)),
        bridge,
        recovery: Recovery::new(),
        updates: crate::updates::UpdatesState::default(),
    })
}

/// Serve the REAL router on an ephemeral loopback port; returns its base URL.
async fn spawn_panel(state: SharedState) -> String {
    let app = crate::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A client that does NOT follow redirects — the 303 to `/login` is the thing
/// under test, not a step on the way to something else.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn request(c: &reqwest::Client, base: &str, method: Method, path: &str) -> reqwest::RequestBuilder {
    let url = format!("{base}{path}");
    match method {
        Method::Get => c.get(url),
        Method::Post => c.post(url),
    }
}

/// Every authenticated route rejects a request that carries no credentials —
/// checked against the real router with the dangerous set registered, so all
/// 90 routes are live. No valid credential is ever sent, so no handler runs.
#[tokio::test]
async fn every_authenticated_route_rejects_unauthenticated_requests() {
    let base = spawn_panel(state_with(cfg_authenticated(true))).await;
    let c = client();
    for spec in route_table() {
        let method = probe_method(&spec);
        let status = request(&c, &base, method, spec.request)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{method:?} {} failed: {e}", spec.request))
            .status()
            .as_u16();
        match spec.access {
            Authenticated => assert_eq!(
                status, 401,
                "{method:?} {} must be gated, got {status}",
                spec.request
            ),
            Public => assert_ne!(
                status, 401,
                "{method:?} {} is a documented exemption but was gated",
                spec.request
            ),
        }
    }
}

/// The exempt routes actually serve their content without credentials.
#[tokio::test]
async fn exempt_routes_serve_without_credentials() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let c = client();
    for path in ["/assets/htmx.min.js", "/assets/style.css", "/login"] {
        let resp = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{path} must serve anonymously");
        assert!(!resp.text().await.unwrap().is_empty());
    }
}

/// S5 — with `allow_dangerous = false` the root-equivalent routes are not
/// registered at all (404), while the recovery routes stay registered (401,
/// i.e. auth rejected them before any handler ran).
#[tokio::test]
async fn dangerous_routes_are_unregistered_when_allow_dangerous_is_false() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let c = client();
    for spec in route_table().iter().filter(|s| s.dangerous) {
        let status = request(&c, &base, probe_method(spec), spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status, 404,
            "{} must not exist with allow_dangerous = false",
            spec.request
        );
    }
    // The recovery routes are NOT dangerous and must stay registered:
    // 401 (gated) rather than 404 (gone). Probed with GET so the handler is
    // never reached even if the auth layer were missing (see `exec_backed`).
    for path in [
        "/system/services/restart/not-a-unit",
        "/devices/cec/recover/restart-daemon",
        "/dev/restart-daemon",
        "/dev/restart-shell",
    ] {
        let status = c
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(status, 401, "{path} is recovery and must stay registered");
    }
}

/// S5 — with `allow_dangerous = true` they exist again (401, not 404: the
/// auth layer still rejects the unauthenticated probe, so nothing executes).
#[tokio::test]
async fn dangerous_routes_are_registered_when_allow_dangerous_is_true() {
    let base = spawn_panel(state_with(cfg_authenticated(true))).await;
    let c = client();
    for spec in route_table().iter().filter(|s| s.dangerous) {
        let status = request(&c, &base, probe_method(spec), spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status, 401,
            "{} must be registered (and gated) with allow_dangerous = true",
            spec.request
        );
    }
}

/// **The console page never renders a button for a route that is not there.**
///
/// The page is node tier and `POST /dev/console/raw` is in the danger block,
/// so `allow_dangerous = false` — the default, and what the reference node
/// htpc-1 runs — leaves a registered page in front of an unregistered action.
/// It must explain itself and render no form; the failure mode this forbids is
/// a Send button that 404s.
#[tokio::test]
async fn the_console_page_renders_no_form_when_the_raw_route_is_unregistered() {
    let c = client();
    let caps = caps_with(every_gated_feature());

    let off = spawn_panel(state_with_caps(cfg_authenticated(false), caps.clone())).await;
    let body = c
        .get(format!("{off}/dev/console"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        body.contains("raw IPC console is disabled"),
        "the page must say why it is inert: {body}"
    );
    assert!(
        !body.contains(r#"hx-post="/dev/console/raw""#) && !body.contains("<form"),
        "no control may target the unregistered route — the banner names the \
         path in prose, but there must be no form: {body}"
    );
    assert_eq!(
        c.post(format!("{off}/dev/console/raw"))
            .bearer_auth(TEST_TOKEN)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404,
        "the route really is absent — that is what the page is explaining"
    );

    let on = spawn_panel(state_with_caps(cfg_authenticated(true), caps)).await;
    let body = c
        .get(format!("{on}/dev/console"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        body.contains(r#"hx-post="/dev/console/raw""#),
        "with the route registered the form comes back — otherwise the check \
         above would pass on a page that never has a form: {body}"
    );
}

/// The guarded-verb vocabulary is duplicated: `WARN_COMMANDS` server-side
/// warns after the fact, and a JS regex in `console.html` sharpens the confirm
/// prompt before the request. Two copies of one list drift silently — a verb
/// added to one keeps working, just without half its guard.
#[test]
fn the_console_guard_list_matches_its_template_regex() {
    const TEMPLATE: &str = include_str!("../templates/console.html");
    let line = TEMPLATE
        .lines()
        .find(|l| l.contains("var GUARDED ="))
        .expect("console.html declares the GUARDED regex");
    let alternation = line
        .split_once("/^(")
        .and_then(|(_, rest)| rest.split_once(")"))
        .map(|(inner, _)| inner)
        .expect("the regex is an anchored alternation");
    let in_template: Vec<&str> = alternation.split('|').collect();
    assert_eq!(
        in_template,
        crate::pages::console::WARN_COMMANDS,
        "console.html's GUARDED regex and pages::console::WARN_COMMANDS are the \
         same vocabulary in two places — update both"
    );
}

/// **The capability gate, proven against the real router.** With an EMPTY
/// capability snapshot — the fail-closed state a failed handshake produces —
/// every node-tier and capability-tier route answers **404 (it does not
/// exist)**, not 403 from a handler, while every recovery-tier route stays
/// registered and answers 401.
///
/// `allow_dangerous = true` throughout, so the danger flag is never what makes
/// a route disappear here. Exec-backed rows are probed with GET (see
/// [`RouteSpec::exec_backed`]) so no handler can run either way.
#[tokio::test]
async fn gated_routes_are_unregistered_with_an_empty_capability_set() {
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        CapabilitySnapshot::unreachable(),
    ))
    .await;
    let c = client();

    let mut gone = 0usize;
    let mut kept = 0usize;
    for spec in route_table() {
        let method = probe_method(&spec);
        let status = request(&c, &base, method, spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        if spec.gate == Gate::Recovery {
            kept += 1;
            match spec.access {
                Authenticated => assert_eq!(
                    status, 401,
                    "{} is recovery tier — a failed handshake must not remove it \
                     (got {status})",
                    spec.request
                ),
                Public => assert_ne!(status, 404, "{} must stay served", spec.request),
            }
        } else {
            gone += 1;
            assert_eq!(
                status,
                404,
                "{} is behind Gate::{} and must NOT be registered with an empty \
                 capability set — a gated-off route does not exist (404), it is \
                 not a 403 from a handler",
                spec.request,
                spec.gate.ident()
            );
        }
    }
    assert!(gone > 0 && kept > 0, "the split must be non-trivial");
}

/// The other half: with the full declared set the same routes are registered
/// again — 401 (gated), never 404. Without this, a gate that removed
/// *everything* would pass the test above.
#[tokio::test]
async fn gated_routes_are_registered_with_the_full_capability_set() {
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        caps_with(every_gated_feature()),
    ))
    .await;
    let c = client();
    for spec in route_table().iter().filter(|s| s.gate != Gate::Recovery) {
        let status = request(&c, &base, probe_method(spec), spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status,
            401,
            "{} must be registered (and gated) once Gate::{} is satisfied",
            spec.request,
            spec.gate.ident()
        );
    }
}

/// One feature opens exactly its own block and nothing else — the gate is
/// per-capability, not one global on/off switch.
#[tokio::test]
async fn a_single_feature_opens_only_its_own_block() {
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        caps_with([Feature::Cec].into_iter().collect()),
    ))
    .await;
    let c = client();
    for spec in route_table() {
        if spec.access == Public {
            continue;
        }
        // `allow_dangerous` is on, so the danger flag alone never removes a
        // route here — `/dev/deploy` and `/dev/build` still vanish because
        // their block ALSO needs `Gate::DevDeploy`.
        let expected = if matches!(spec.gate, Gate::Recovery | Gate::Node | Gate::Cec) {
            401
        } else {
            404
        };
        let status = request(&c, &base, probe_method(&spec), spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status,
            expected,
            "{} (Gate::{}, dangerous={}) with only `cec` declared",
            spec.request,
            spec.gate.ident(),
            spec.dangerous
        );
    }
}

/// **The no-regression test for the one deployed node.** Given the feature set
/// htpc-1's real daemon build declares ([`htpc_1_features`]), the registered
/// route set is exactly today's — every row of `route_table()` is live. This
/// PR therefore changes nothing on htpc-1.
///
/// This is the test that catches the trap: gating the wallpaper routes on
/// `Feature::Wallpapers`, `/processes` on `Feature::Processes`, or
/// `/system/updates/*` on `Feature::SystemUpdates` would fail here, because
/// `daemon/src/ipc.rs::features()` never emits any of those three.
#[tokio::test]
async fn htpc_1_declared_set_registers_todays_entire_route_set() {
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        caps_with(htpc_1_features()),
    ))
    .await;
    let c = client();
    for spec in route_table() {
        let status = request(&c, &base, probe_method(&spec), spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        match spec.access {
            Authenticated => assert_eq!(
                status, 401,
                "{} vanished from htpc-1's panel — it is gated on a capability that \
                 daemon/src/ipc.rs::features() does not emit for that build",
                spec.request
            ),
            Public => assert_ne!(status, 404, "{} must stay served", spec.request),
        }
    }
}

/// desktop-2's REAL declared feature set, parsed from the live sidecar's
/// `GET /capabilities` (`host-v0.7.0` at 192.168.8.153:47995, captured
/// 2026-08-07):
///
/// ```text
/// {"node_id":"desktop","kind":"sidecar","agent_version":"0.7.0",
///  "platform":"windows","features":["steam_library","game_launch","sleep"]}
/// ```
///
/// **Deserialized from that payload rather than hand-listed.** A hand-written
/// `[Feature::SteamLibrary, …]` would be a fixture asserting what this test
/// already believes; going through the wire format means a rename or an
/// `as_str()` drift on either side shows up here.
fn desktop_2_capabilities() -> CapabilitySnapshot {
    const LIVE: &str = r#"{"node_id":"desktop","kind":"sidecar","agent_version":"0.7.0","platform":"windows","features":["steam_library","game_launch","sleep"]}"#;
    let caps: tv_shell_protocol::Capabilities =
        serde_json::from_str(LIVE).expect("desktop-2's live /capabilities payload");
    assert_eq!(caps.kind, tv_shell_protocol::NodeKind::Sidecar);
    assert_eq!(
        caps.features,
        [Feature::SteamLibrary, Feature::GameLaunch, Feature::Sleep]
            .into_iter()
            .collect(),
        "the captured payload changed shape — re-capture it, do not edit the \
         expectation"
    );
    caps.into()
}

/// **The gating claim for the node `HttpTransport` exists to serve, checked
/// against the real router rather than eyeballed.**
///
/// A sidecar declares `steam_library`, `game_launch` and `sleep`. **No [`Gate`]
/// names any of the three** (`no_gate_names_a_feature_the_daemon_never_emits`
/// pins that), so a panel pointed at desktop-2 must register exactly the
/// recovery tier plus the node tier — the handshake did succeed — and **not one
/// capability-tier route**. CEC, Controllers, Widgets, Settings, WebApps and
/// Screenshot must all 404: desktop-2 has no CEC adapter, no gamepad fleet, no
/// QML shell and no `settings.json`, so rendering any of those pages would be
/// the panel inventing a surface the node never claimed.
///
/// `allow_dangerous = true` throughout, so nothing here disappears merely for
/// being in the danger tier — `/dev/deploy` and `/dev/build` still vanish
/// because their block ALSO requires `Gate::DevDeploy`, which a sidecar does
/// not declare.
#[tokio::test]
async fn desktop_2_sidecar_registers_the_node_tier_and_no_capability_route() {
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        desktop_2_capabilities(),
    ))
    .await;
    let c = client();

    let mut registered = 0usize;
    let mut absent = 0usize;
    for spec in route_table() {
        if spec.access == Public {
            continue;
        }
        // Recovery is unconditional; Node is open because the handshake
        // succeeded; every capability gate names a feature no sidecar declares.
        let expected = match spec.gate {
            Gate::Recovery | Gate::Node => 401,
            _ => 404,
        };
        let status = request(&c, &base, probe_method(&spec), spec.request)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status,
            expected,
            "{} (Gate::{}) against desktop-2's declared set \
             [steam_library, game_launch, sleep]",
            spec.request,
            spec.gate.ident()
        );
        if expected == 401 {
            registered += 1;
        } else {
            absent += 1;
        }
    }

    // The split must be non-trivial in BOTH directions, or the assertion above
    // is satisfied by a router that registers everything or nothing.
    assert!(
        registered > 0 && absent > 0,
        "registered={registered}, absent={absent} — the sidecar gating claim is vacuous"
    );

    // And name the pages explicitly, so the test says what it means rather than
    // only what its loop happens to cover.
    for path in [
        "/devices/cec",
        "/devices/controllers",
        "/shell/widgets",
        "/shell/appearance",
        "/shell/advanced",
        "/devices/display-audio",
    ] {
        let status = c
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status, 404,
            "{path} must not exist on a sidecar — it has no CEC adapter, no gamepad \
             fleet, no QML shell and no settings.json"
        );
    }
}

/// The five features `daemon/src/ipc.rs::features()` deliberately never emits
/// must not appear in htpc-1's set — if one crept in, the test above would go
/// green on a fiction.
#[test]
fn htpc_1_set_omits_every_feature_the_daemon_never_emits() {
    let set = htpc_1_features();
    for absent in [
        Feature::Wallpapers,
        Feature::Processes,
        Feature::SystemUpdates,
        Feature::SteamLibrary,
        Feature::GameLaunch,
    ] {
        assert!(
            !set.contains(&absent),
            "{absent:?} is not emitted by daemon/src/ipc.rs::features() — a test \
             fixture that claims it would prove nothing about the live node"
        );
    }
    // And htpc-1 must satisfy every gate the panel has, or a page really would
    // disappear from the deployed node.
    let caps = caps_with(set);
    for gate in Gate::ALL {
        assert!(
            caps.allows(*gate),
            "htpc-1 does not satisfy Gate::{} — the pages behind it would 404 on \
             the one node this panel is deployed to",
            gate.ident()
        );
    }
}

/// **The reachable-but-ungated dashboard.** The one state the page-level
/// affordance gate above cannot reach on its own, because it needs the daemon
/// ANSWERING while the startup snapshot is empty:
///
/// handshake fails at startup → `Gate::Controllers` off → the daemon comes back
/// → the ~5s tile poll sees `reachable == true` → the Dashboard (recovery tier,
/// always registered) renders its tiles.
///
/// `reachable` is *this poll's* live probe; the gate is the *startup* snapshot.
/// Conflating them put two `/controllers` links on a router with no such route.
#[tokio::test]
async fn dashboard_tiles_never_link_to_a_page_the_snapshot_gated_away() {
    let replies = std::collections::HashMap::from([
        ("status", "connected:grabbed"),
        (
            "build-info",
            r#"{"version":"0.2.2","sha":"abc1234","branch":"main"}"#,
        ),
        (
            "sys-status",
            r#"{"os":"Arch","kernel":"6.12","hostname":"htpc-1","uptime":"1h"}"#,
        ),
        ("sys-metrics", "{}"),
        ("storage-status", "[]"),
        (
            "get-pads",
            r#"[{"id":"a","index":0,"name":"Pad","grabbed":true}]"#,
        ),
    ]);
    let sock = spawn_canned_daemon("tiles-ungated", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;

    // The daemon is up and answering, but the handshake failed at startup.
    let state = state_for_socket_with_caps(sock, CapabilitySnapshot::unreachable());
    let html = pages::dashboard::render_tiles(&state).await;

    assert!(
        html.contains("Input daemon"),
        "the tiles must still render — this is the recovery tier: {html}"
    );
    assert!(
        !html.contains("href=\"/devices/controllers\""),
        "the tiles linked to /devices/controllers while Gate::Controllers is closed — that \
         route is not registered, so the link 404s: {html}"
    );

    // Control: with the gate open the links come back, so the assertion above
    // is the gate's doing and not a tile that never renders a link at all.
    let sock2 = spawn_canned_daemon("tiles-gated", replies_for_tiles());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let open = state_for_socket_with_caps(sock2, CapabilitySnapshot::fully_capable());
    let html = pages::dashboard::render_tiles(&open).await;
    assert!(
        html.contains("href=\"/devices/controllers\""),
        "with Gate::Controllers open the tiles must link to it: {html}"
    );
}

/// The canned replies a fully-populated tile render needs.
fn replies_for_tiles() -> std::collections::HashMap<&'static str, &'static str> {
    std::collections::HashMap::from([
        ("status", "connected:grabbed"),
        (
            "build-info",
            r#"{"version":"0.2.2","sha":"abc1234","branch":"main"}"#,
        ),
        (
            "sys-status",
            r#"{"os":"Arch","kernel":"6.12","hostname":"htpc-1","uptime":"1h"}"#,
        ),
        ("sys-metrics", "{}"),
        ("storage-status", "[]"),
        (
            "get-pads",
            r#"[{"id":"a","index":0,"name":"Pad","grabbed":true}]"#,
        ),
    ])
}

// ── phase 6: Overview is read-only tiles with deep links ───────────────────

/// The four routes that make up Overview: the page shell and its three
/// independently-polled tile fragments.
const OVERVIEW_ROUTES: [&str; 4] = [
    "/",
    "/overview/tiles",
    "/overview/services-tile",
    "/overview/updates-tile",
];

/// **#410's headline claim.** With every action redistributed into the group
/// that owns its subject, Overview is the panel's one purely read-only
/// surface: *is everything healthy right now, and where do I go to fix what
/// isn't*. So it renders links and nothing else — not a form, not a button,
/// not an `hx-post`.
///
/// Pinned as an absence over the real router rather than restated in prose,
/// because the failure mode is additive: the next person who wants "just a
/// Restart button, it's right there" has to delete this test to get it, and
/// deleting a test is a visible decision in a way that adding a button is not.
async fn assert_overview_renders_no_mutating_control(base: &str, label: &str) {
    let c = client();
    for path in OVERVIEW_ROUTES {
        let resp = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(
            resp.status().as_u16(),
            200,
            "[{label}] {path} must render — Overview is recovery tier"
        );
        let body = resp.text().await.unwrap().to_lowercase();
        for mutating in ["hx-post", "<form", "<button"] {
            assert!(
                !body.contains(mutating),
                "[{label}] {path} renders {mutating}, and Overview mutates nothing — \
                 the control belongs on the page that owns its subject: {body}"
            );
        }
    }
}

/// The reachable branch: the daemon answers, every tile has real content.
#[tokio::test]
async fn overview_renders_no_mutating_control() {
    let sock = spawn_canned_daemon("overview-readonly", replies_for_tiles());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = spawn_panel(state_for_socket_with_caps(
        sock,
        CapabilitySnapshot::fully_capable(),
    ))
    .await;
    assert_overview_renders_no_mutating_control(&base, "daemon reachable").await;
}

/// The degraded branch: no daemon at all, so Overview falls back to the unit
/// states it reads straight from systemd. That is the branch that makes
/// Overview honest in recovery mode — and it is also the branch most tempting
/// to hang a restart button off, since the daemon being down is exactly when
/// you want one. It goes on Dev ▸ Recovery and System ▸ Services instead.
#[tokio::test]
async fn overview_renders_no_mutating_control_with_the_daemon_down() {
    let base = spawn_panel(state_for_socket_with_caps(
        std::path::PathBuf::from("/tmp/tvshp-no-such-socket.sock"),
        CapabilitySnapshot::unreachable(),
    ))
    .await;
    assert_overview_renders_no_mutating_control(&base, "daemon down, recovery mode").await;
}

/// Every tile is a whole-tile link to the page that now OWNS its subject —
/// the point of the tiles, and the thing that quietly rots when a page moves.
/// Pins the map itself, and re-checks each target against `route_table()`:
/// `no_page_renders_an_unregistered_target_*` already does that for the
/// degraded and bare-node renders but cannot reach here, because those
/// harnesses have no daemon and so only ever see the degraded branch.
#[tokio::test]
async fn overview_tiles_link_to_the_page_that_owns_each_subject() {
    let sock = spawn_canned_daemon("overview-deep-links", replies_for_tiles());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket_with_caps(sock, CapabilitySnapshot::fully_capable());

    let mut html = pages::dashboard::render_tiles(&state).await;
    html.push_str(&pages::dashboard::render_services_tile(&state).await);

    // Tile subject -> the page that owns it after the IA move. The Updates
    // tile is the one fragment not in `html`: it polls on its own 300s
    // cadence, so its link is listed here and checked against the table below
    // with the rest of the map.
    let owners = [
        ("Input daemon", "/devices/controllers"),
        ("Build", "/dev/recovery"),
        ("System", "/system/processes"),
        ("Resources", "/system/processes"),
        ("Temperatures", "/system/processes"),
        ("Storage", "/system/processes"),
        ("Controllers", "/devices/controllers"),
        ("Units", "/system/services"),
        ("System services", "/system/services"),
        ("Updates", "/system/updates"),
    ];
    for (tile, owner) in owners {
        if tile != "Updates" {
            assert!(
                html.contains(&format!("<a class=\"tile\" href=\"{owner}\"")),
                "the {tile} tile must deep-link to {owner}: {html}"
            );
        }
        let table = route_table();
        let row = declaring_row(&table, owner).unwrap_or_else(|| {
            panic!("the {tile} tile links to {owner}, which is not a route at all")
        });
        assert!(
            !row.dangerous,
            "a read-only Overview must not deep-link into the dangerous set: {owner}"
        );
    }

    // The reverse direction: nothing on Overview links anywhere else. A tile
    // pointing at a pre-IA path would otherwise pass the loop above unnoticed.
    let expected: BTreeSet<String> = owners.iter().map(|(_, o)| o.to_string()).collect();
    for target in link_targets(&html) {
        assert!(
            expected.contains(&target),
            "Overview renders a link to {target}, which owns no tile's subject — \
             every tile link is the page that owns it: {html}"
        );
    }
}

/// The empty state — `[panel].managed_units` is the default (empty) on every
/// node today, so this is what the tile actually shows in production. An empty
/// card would read as "no services", which is a different and false claim; the
/// tile says nothing is *configured* and names the key that fills it.
#[tokio::test]
async fn overview_system_services_tile_explains_an_empty_allowlist() {
    let state = hermetic_state();
    let html = pages::dashboard::render_services_tile(&state).await;
    assert!(
        html.contains("none configured"),
        "the empty state must say nothing is configured, not render blank: {html}"
    );
    assert!(
        html.contains("[panel].managed_units"),
        "and must name the config key that fills it: {html}"
    );
    assert!(
        html.contains(r#"href="/system/services""#),
        "the tile is still a link to the page that owns the subject: {html}"
    );
}

/// A row per configured unit, labelled by the operator's own key. The unit's
/// *state* is whatever this machine says — what is pinned is that every
/// configured unit gets a row with a status dot, since a silently-dropped unit
/// on a health screen is worse than no health screen.
#[tokio::test]
async fn overview_system_services_tile_renders_a_row_per_managed_unit() {
    let state = state_with(managed(&[
        ("sshd", "sshd.service", "system"),
        ("network", "NetworkManager.service", "system"),
        ("pipewire", "pipewire.service", "user"),
    ]));
    let html = pages::dashboard::render_services_tile(&state).await;
    for key in ["sshd", "network", "pipewire"] {
        assert!(
            html.contains(&format!("</span>{key}: ")),
            "expected a status row for the configured {key}: {html}"
        );
    }
    assert_eq!(
        html.matches(r#"<span class="dot "#).count(),
        3,
        "one dot per configured unit, each paired with its status word: {html}"
    );
    assert!(
        !html.contains("none configured"),
        "the empty state must not render alongside real rows: {html}"
    );
}

/// With no daemon, Overview still answers the two questions systemd alone can
/// answer: are the tv-shell units up, and are the managed units up. Both
/// fragments are exec-only, which is why the group stays in the recovery-mode
/// drawer at all (`docs/PANEL_IA.md` § Capability gating).
#[tokio::test]
async fn overview_still_reports_unit_state_with_the_daemon_down() {
    let state = state_with_caps(
        managed(&[("sshd", "sshd.service", "system")]),
        CapabilitySnapshot::unreachable(),
    );

    let tiles = pages::dashboard::render_tiles(&state).await;
    assert!(
        tiles.to_lowercase().contains("unreachable"),
        "the degraded branch must say so: {tiles}"
    );
    assert!(
        tiles.contains("Units (via systemd)") && tiles.contains("daemon:"),
        "the degraded branch reads unit state straight from systemd: {tiles}"
    );

    let services = pages::dashboard::render_services_tile(&state).await;
    assert!(
        services.contains("</span>sshd: "),
        "the system-services tile is exec-only and unaffected by the daemon: {services}"
    );
}

/// The three fragments render into ONE grid. The grid is declared once, on the
/// page; each fragment emits bare tiles into a `display: contents` slot. While
/// each fragment carried a `.tile-grid` of its own, the separately-polled
/// Updates tile landed alone on a row instead of flowing in beside its
/// neighbours.
#[tokio::test]
async fn the_overview_tile_fragments_share_one_grid() {
    let sock = spawn_canned_daemon("overview-one-grid", replies_for_tiles());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = spawn_panel(state_for_socket_with_caps(
        sock,
        CapabilitySnapshot::fully_capable(),
    ))
    .await;
    let c = client();

    let page = c
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        page.matches(r#"class="tile-grid""#).count(),
        1,
        "the page declares exactly one grid: {page}"
    );
    assert_eq!(
        page.matches("tile-slot").count(),
        3,
        "one `display: contents` slot per poll target: {page}"
    );

    for path in OVERVIEW_ROUTES.iter().skip(1) {
        let body = c
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            !body.contains("tile-grid"),
            "{path} must emit bare tiles — a grid of its own is what put the \
             Updates tile on a row by itself: {body}"
        );
    }
}

/// One class for a tile's headline value, used the same way everywhere. The
/// Input Daemon tile used to carry `.big` (1.2rem/600) while the equivalent
/// line on Build/System/Resources was plain body text, so it rendered bold and
/// much larger than its neighbours and wrapped to two lines at 1440px.
#[tokio::test]
async fn every_overview_tile_headline_uses_the_same_class() {
    let sock = spawn_canned_daemon("overview-typography", replies_for_tiles());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket_with_caps(sock, CapabilitySnapshot::fully_capable());
    let html = pages::dashboard::render_tiles(&state).await;

    assert!(
        !html.contains(r#"class="big""#),
        "`.big` was the one-off that made Input Daemon's line an outlier: {html}"
    );
    // Input daemon, Build, System, Resources — the four tiles whose body leads
    // with a single value. Temperatures/Storage/Controllers/Units are lists and
    // deliberately have no headline.
    assert_eq!(
        html.matches(r#"class="tile-value""#).count(),
        4,
        "each single-value tile has exactly one headline line: {html}"
    );
}

/// The recovery banner must give the RIGHT advice for each failure mode.
///
/// Both modes gate identically (empty set, recovery tier only), but the
/// operator instruction is opposite. The realistic refusal trigger is a panel
/// binary newer than the on-device daemon: telling that operator to "restart
/// the panel once the daemon is back" describes a daemon that is already back,
/// and it would stay wrong forever.
#[tokio::test]
async fn the_recovery_banner_distinguishes_a_down_node_from_an_old_one() {
    let c = client();

    let down = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        CapabilitySnapshot::unreachable(),
    ))
    .await;
    let body = c
        .get(format!("{down}/"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("Recovery mode"), "{body}");
    assert!(
        body.contains("once the daemon is\n  back") || body.contains("once the daemon is back"),
        "an unreachable node's advice is to wait and restart the panel: {body}"
    );
    assert!(
        !body.contains("older than this panel"),
        "an unreachable node is not a version-skew problem: {body}"
    );

    let refused = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        CapabilitySnapshot::refused("unknown command"),
    ))
    .await;
    let body = c
        .get(format!("{refused}/"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("Recovery mode"), "{body}");
    assert!(
        body.contains("older than this panel"),
        "a refusal must be diagnosed as version skew: {body}"
    );
    assert!(
        body.contains("restarting the panel will not help")
            || body.contains("restarting the panel will not\n  help"),
        "the refusal banner must contradict the restart advice, not repeat it: {body}"
    );
    assert!(
        body.contains("unknown command"),
        "the node's own message must reach the operator: {body}"
    );
}

/// **The nav/route drift gate** (the half `allows()` alone does not give).
///
/// Route registration and the nav share the *predicate* — both ask
/// [`CapabilitySnapshot::allows`] — but not the *assignment*: `NavPage.gate` is
/// hand-typed in `capabilities.rs` while a page's real gate is the
/// `build_router` block it sits in, which `route_table()` mirrors. Nothing
/// stops those two diverging, and the dangerous direction is silent: move a
/// page to a stricter gate, leave the nav on the looser one, and the drawer or
/// sub-nav renders a link to a route that was never registered.
///
/// Walks groups → pages, so it covers both nav levels: a group's drawer href is
/// always one of its own pages' hrefs (`Chrome::new` picks the first registered
/// one), so pinning every page pins every drawer target too.
#[test]
fn nav_items_agree_with_the_route_table_they_link_to() {
    let table = route_table();
    for group in crate::capabilities::NAV {
        assert!(
            !group.pages.is_empty(),
            "nav group {} declares no pages — it could never render",
            group.key
        );
        for page in group.pages {
            let row = table
                .iter()
                .find(|r| r.declared == page.href && r.method == Get)
                .unwrap_or_else(|| {
                    panic!(
                        "the nav links to {} ({} ▸ {}), which `route_table()` declares \
                         no GET route for — the link would 404",
                        page.href, group.label, page.label
                    )
                });
            assert_eq!(
                row.gate.ident(),
                page.gate.ident(),
                "nav page {} is gated on Gate::{} but its page is registered under \
                 Gate::{} — the nav would render a link to an unregistered route (or \
                 hide a page that exists)",
                page.href,
                page.gate.ident(),
                row.gate.ident()
            );
            assert!(
                !row.dangerous,
                "nav page {} points at a route in the dangerous set; the nav has no \
                 `allow_dangerous` input, so it cannot honor that gate",
                page.href
            );
        }
    }
}

/// The two-level chrome as `base.html` actually renders it: a drawer of groups
/// on every page, a sub-nav only where the active group has two or more
/// registered pages, and the daemon dot in the drawer footer (not the sub-nav).
///
/// `Chrome`'s own shape is pinned in `capabilities::tests`; this is the
/// template half, which is where a `{% for %}` over the wrong collection would
/// show up.
#[tokio::test]
async fn base_html_renders_the_drawer_and_gates_the_subnav_on_group_size() {
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        caps_with(every_gated_feature()),
    ))
    .await;
    let c = client();
    let get = |path: &'static str| {
        let c = c.clone();
        let base = base.clone();
        async move {
            c.get(format!("{base}{path}"))
                .bearer_auth(TEST_TOKEN)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    };

    let overview = get("/").await;
    for group in crate::capabilities::NAV {
        assert!(
            overview.contains(&format!(r#"data-group="{}""#, group.key)),
            "the drawer is missing the {} group: {overview}",
            group.key
        );
    }
    assert!(
        overview.contains(r#"class="drawer-link active""#),
        "the active group must be marked in the drawer: {overview}"
    );
    assert!(
        !overview.contains(r#"class="subnav""#),
        "Overview is a single-page group — it must render no sub-nav bar at all: \
         {overview}"
    );
    assert!(
        overview.contains(r#"<div class="drawer-footer">"#)
            && overview.contains(r#"id="daemon-status""#),
        "the daemon dot lives in the drawer footer: {overview}"
    );

    let processes = get("/system/processes").await;
    assert!(
        processes.contains(r#"class="subnav""#),
        "System has four registered pages, so it gets a sub-nav bar: {processes}"
    );
    // All four System pages, in the spec's order (`docs/PANEL_IA.md` § System).
    for page in [
        "/system/services",
        "/system/processes",
        "/system/updates",
        "/system/logs",
    ] {
        assert!(
            processes.contains(&format!(r#"<a href="{page}""#)),
            "the sub-nav must list every registered page of the active group, \
             missing {page}: {processes}"
        );
    }
    assert!(
        !processes.contains(r#"<a href="/shell/widgets""#),
        "the sub-nav must NOT list another group's pages: {processes}"
    );
}

/// **Every pre-IA path still answers**, and answers with a redirect rather than
/// a 404 — for whoever bookmarked it or typed it from memory.
///
/// Each redirect is registered in the same `build_router` block as its target,
/// which is the property under test in both directions: with the gate open the
/// old path 303s (never 404s), and with it closed the old path 404s exactly
/// like the page it points at, instead of forwarding to something that is not
/// there.
#[tokio::test]
async fn the_pre_ia_paths_redirect_when_their_target_is_registered() {
    const REDIRECTS: [(&str, &str); 10] = [
        ("/dashboard", "/"),
        ("/processes", "/system/processes"),
        ("/logs", "/system/logs"),
        ("/settings", "/shell/appearance"),
        ("/widgets", "/shell/widgets"),
        ("/media", "/shell/appearance"),
        ("/tools", "/remote/navigation"),
        ("/controllers", "/devices/controllers"),
        ("/cec", "/devices/cec"),
        ("/dev", "/dev/recovery"),
    ];

    let table = route_table();
    let gate_of = |path: &str| {
        table
            .iter()
            .find(|r| r.declared == path && r.method == Get)
            .unwrap_or_else(|| panic!("{path} is missing from route_table()"))
            .gate
    };

    // Every redirect must sit in the same block as its target, or one of the
    // two assertions below would be asserting nothing.
    for (old, new) in REDIRECTS {
        assert_eq!(
            gate_of(old).ident(),
            gate_of(new).ident(),
            "{old} redirects to {new} from a different registration block — it \
             could outlive its target and forward to a 404"
        );
    }

    let open = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        caps_with(every_gated_feature()),
    ))
    .await;
    let closed = spawn_panel(state_with_caps(
        cfg_authenticated(true),
        CapabilitySnapshot::unreachable(),
    ))
    .await;
    let c = client();
    let caps_down = CapabilitySnapshot::unreachable();

    for (old, new) in REDIRECTS {
        let resp = c
            .get(format!("{open}{old}"))
            .bearer_auth(TEST_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            303,
            "{old} must still answer — a moved page needs a forwarding address"
        );
        assert_eq!(
            resp.headers().get("location").unwrap(),
            new,
            "{old} forwards to the wrong place"
        );

        let status = c
            .get(format!("{closed}{old}"))
            .bearer_auth(TEST_TOKEN)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        let expected = if caps_down.allows(gate_of(old)) {
            303
        } else {
            404
        };
        assert_eq!(
            status,
            expected,
            "{old} (Gate::{}) must track its target exactly with the handshake \
             failed — a redirect that outlives its page forwards to a 404",
            gate_of(old).ident()
        );
    }
}

/// Pull every literal link/form target out of a rendered page.
///
/// Rendered HTML, so no `{{ }}` survives — path parameters arrive already
/// substituted (`/system/services/restart/tv-shell-input`), which is why matching
/// back to a declaration is segment-wise below.
fn link_targets(html: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for attr in ["hx-post=\"", "hx-get=\"", "href=\""] {
        let mut rest = html;
        while let Some(i) = rest.find(attr) {
            rest = &rest[i + attr.len()..];
            let Some(end) = rest.find('"') else { break };
            let raw = &rest[..end];
            rest = &rest[end..];
            if !raw.starts_with('/') {
                continue;
            }
            // Query strings and fragments address the same route.
            let path = raw.split(['?', '#']).next().unwrap_or(raw);
            out.insert(path.to_string());
        }
    }
    out
}

/// Resolve a concrete request path back to the `route_table()` row that
/// declares it, treating a `{placeholder}` segment as a wildcard.
fn declaring_row<'a>(table: &'a [RouteSpec], path: &str) -> Option<&'a RouteSpec> {
    table.iter().find(|r| {
        let declared: Vec<&str> = r.declared.split('/').collect();
        let actual: Vec<&str> = path.split('/').collect();
        declared.len() == actual.len()
            && declared
                .iter()
                .zip(&actual)
                .all(|(d, a)| (d.starts_with('{') && d.ends_with('}')) || d == a)
    })
}

/// **The rendered-affordance gate.** Fetch every page a capability set
/// registers, and assert that every link and form target in the returned HTML
/// is itself a route registered under that same set.
///
/// This is the invariant `build_router`'s doc comment states — *"the panel
/// never renders a button that 404s"* — enforced instead of asserted. The nav
/// gate above only covers the drawer and sub-nav; this covers in-page
/// affordances, which is where the interesting version of the bug lives: a page
/// in one tier (Shell ▸ Apps, `settings_store`) carrying a form for a route in
/// another (`/shell/apps/webapp/*`, `Gate::WebApps`).
async fn assert_no_page_renders_an_unregistered_target(caps: CapabilitySnapshot, label: &str) {
    let allow_dangerous = true;
    let table = route_table();
    let base = spawn_panel(state_with_caps(
        cfg_authenticated(allow_dangerous),
        caps.clone(),
    ))
    .await;
    let c = client();

    let registered = |row: &RouteSpec| caps.allows(row.gate) && (!row.dangerous || allow_dangerous);

    for page in table
        .iter()
        .filter(|r| r.method == Get && r.access == Authenticated && !r.exec_backed && registered(r))
    {
        let resp = c
            .get(format!("{base}{}", page.request))
            .bearer_auth(TEST_TOKEN)
            .send()
            .await
            .unwrap();
        if resp.status() != 200 {
            continue;
        }
        let body = resp.text().await.unwrap();
        if !body.contains('<') {
            continue;
        }
        for target in link_targets(&body) {
            // Static assets and off-router anchors are not routes under test.
            if target.starts_with("/assets/") {
                continue;
            }
            let Some(row) = declaring_row(&table, &target) else {
                panic!(
                    "[{label}] {} renders a link to {target}, which is not a route \
                     `route_table()` declares at all",
                    page.request
                )
            };
            assert!(
                registered(row),
                "[{label}] {} renders an affordance targeting {target}, which is \
                 behind Gate::{} and is NOT registered under this capability set — \
                 the panel would render a button that 404s",
                page.request,
                row.gate.ident()
            );
        }
    }
}

/// The full set: every page, every affordance, all registered.
#[tokio::test]
async fn no_page_renders_an_unregistered_target_with_the_full_set() {
    assert_no_page_renders_an_unregistered_target(
        caps_with(every_gated_feature()),
        "full capability set",
    )
    .await;
}

/// The failed handshake: only the recovery pages exist, and none of them may
/// link into a tier that was gated away. This is the state the Dashboard tiles
/// regressed in — the tile poll's live `reachable` flag is not the startup
/// snapshot, so a daemon that came back after a failed handshake made the
/// tiles render `/controllers` links against a router that has no such route.
#[tokio::test]
async fn no_page_renders_an_unregistered_target_in_recovery_mode() {
    assert_no_page_renders_an_unregistered_target(
        CapabilitySnapshot::unreachable(),
        "failed handshake",
    )
    .await;
}

/// A node that answers but declares nothing the panel gates on — the shape a
/// non-Linux sidecar takes. The node-tier pages exist while `controllers` does
/// not, which is exactly the pairing that once put two 404 buttons on the
/// Tools page.
#[tokio::test]
async fn no_page_renders_an_unregistered_target_for_a_bare_node() {
    assert_no_page_renders_an_unregistered_target(
        caps_with(BTreeSet::new()),
        "handshake ok, no features",
    )
    .await;
}

/// The bearer path works, and a wrong token does not.
#[tokio::test]
async fn bearer_token_is_accepted_and_a_wrong_one_is_rejected() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let c = client();

    let ok = c
        .get(format!("{base}/nav/daemon-status"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);

    let bad = c
        .get(format!("{base}/nav/daemon-status"))
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 401);
}

/// The login form mints a session cookie with the hardened flags, and that
/// cookie then authenticates a request.
#[tokio::test]
async fn login_sets_a_hardened_session_cookie_that_authenticates() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let c = client();

    let resp = c
        .post(format!("{base}/login"))
        .form(&[("token", TEST_TOKEN)])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("location").unwrap(), "/");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(
        !cookie.contains("Secure"),
        "Secure is deliberately omitted (plain HTTP on the LAN): {cookie}"
    );

    let jar = cookie.split(';').next().unwrap().to_string();
    let authed = c
        .get(format!("{base}/nav/daemon-status"))
        .header("cookie", jar)
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status().as_u16(), 200);
}

/// A wrong token at the login form gets 401 and mints no cookie.
#[tokio::test]
async fn login_with_a_wrong_token_sets_no_cookie() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let resp = client()
        .post(format!("{base}/login"))
        .form(&[("token", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    assert!(resp.headers().get("set-cookie").is_none());
}

/// htmx must never be handed an HTML login page — it would be swapped into
/// whatever target the caller declared (e.g. the nav status dot).
#[tokio::test]
async fn htmx_request_gets_a_401_not_a_login_page() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let resp = client()
        .get(format!("{base}/nav/daemon-status"))
        .header("HX-Request", "true")
        .header("accept", "text/html, */*")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("<form") && !body.contains("<!DOCTYPE"),
        "htmx must not receive an HTML login page: {body}"
    );
}

/// A browser navigation is redirected to the login form.
#[tokio::test]
async fn browser_navigation_is_redirected_to_login() {
    let base = spawn_panel(state_with(cfg_authenticated(false))).await;
    let resp = client()
        .get(format!("{base}/system/processes"))
        .header("accept", "text/html,application/xhtml+xml")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

/// Fail closed: auth on (`[panel].token_file` set) with no resolvable token
/// rejects everything, including a request that presents an empty credential.
#[tokio::test]
async fn auth_enabled_with_no_token_rejects_everything() {
    let cfg = AppConfig {
        panel_token_file: Some("~/.config/tv-shell/panel-token".to_string()),
        panel_token: None,
        ..AppConfig::default()
    };
    let base = spawn_panel(state_with(cfg)).await;
    let c = client();
    for path in ["/", "/overview", "/nav/daemon-status"] {
        let status = c
            .get(format!("{base}{path}"))
            .bearer_auth("")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(status, 401, "{path} must fail closed");
    }
}

/// No `[panel].token_file` ⇒ auth off ⇒ the loopback dev experience is
/// unchanged (this is the state the panel shipped in, and is only permitted
/// on a loopback bind — see `config::AppConfig::validate`).
#[tokio::test]
async fn auth_disabled_serves_without_credentials() {
    let base = spawn_panel(state_with(AppConfig::default())).await;
    let status = client()
        .get(format!("{base}/nav/daemon-status"))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);
}

/// S2 — the confused-deputy fix, proven end to end: an unauthenticated
/// request to a bridge-backed panel route is rejected BEFORE `BridgeClient`
/// gets to attach the daemon's `[http].token_file` bearer. The stand-in
/// daemon bridge counts inbound connections; it must see zero.
#[tokio::test]
async fn unauthenticated_request_never_reaches_the_daemon_bridge() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bridge_addr = listener.local_addr().unwrap();
    {
        let hits = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                hits.fetch_add(1, Ordering::SeqCst);
                use tokio::io::AsyncWriteExt;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
    }

    let cfg = AppConfig {
        panel_token_file: Some("~/.config/tv-shell/panel-token".to_string()),
        panel_token: Some(TEST_TOKEN.to_string()),
        http_bridge_base: Some(format!("http://{bridge_addr}")),
        http_token: Some("DAEMON-SECRET".to_string()),
        ..AppConfig::default()
    };
    let base = spawn_panel(state_with(cfg)).await;
    let c = client();

    // Two bridge-backed routes: the screenshot PNG proxy (GET) and the log
    // view (GET, `dev_logs`). The screenshot *page* is not one — it renders
    // without calling the bridge, which is why the proxy is probed here. Both call `BridgeClient`, which attaches the daemon
    // token on every request it makes.
    for path in ["/dev/screenshot/image", "/system/logs/view"] {
        let status = c
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(status, 401, "{path} must be gated");
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the daemon bridge must not be dialed for an unauthenticated request — \
         the panel would be laundering the daemon's bearer token (S2)"
    );

    // Control: the same route WITH credentials does reach the bridge, so the
    // zero above is the auth layer's doing and not a broken bridge client.
    let ok = c
        .get(format!("{base}/system/logs/view"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    assert!(hits.load(Ordering::SeqCst) > 0);
}

/// S3 — the refusal must happen before the listener binds. `main` resolves the
/// config (which validates) strictly before `TcpListener::bind`, so an
/// aborted startup can never have opened the port.
#[test]
fn startup_refusal_precedes_the_listener_bind() {
    let src = include_str!("main.rs");
    let load = src
        .find("config::load()?")
        .expect("main must resolve the config");
    let bind = src
        .find("TcpListener::bind")
        .expect("main must bind a listener");
    assert!(
        load < bind,
        "config::load() (which refuses an insecure bind) must run before TcpListener::bind"
    );
}

// ---------------------------------------------------------------------------
// settings.json key ⇄ consumer attribution gate (#416)
// ---------------------------------------------------------------------------
//
// The failure mode this closes has shipped twice, and #416 found a third case:
// a `settings.json` key gets a rendered control — here in [`SCHEMA`], or on a
// QML settings page — and NOTHING anywhere reads it. The control saves, the
// daemon persists, the UI reports success, and the preference does nothing.
// `overscan` was that for its whole life until #416 wired it into `shell.qml`.
//
// Same shape as `route_table_matches_main_rs_declarations`, deliberately:
//
//   * the KEY LIST is machine-derived from `SettingsStore.qml`'s `_schema`,
//     the single declaration site;
//   * the CONTROL surfaces are machine-derived too — [`SCHEMA`] for the panel,
//     a scan of `shell/settings/*.qml` for the shell;
//   * the one hand-maintained thing is [`settings_consumer_table`], and even
//     that is verified against the file it names, so an attribution cannot
//     outlive the consumer it points at.
//
// Adding a key with a control and no consumer therefore cannot pass: there is
// no row to write for it that the gate accepts. Declaring one legitimately
// consumer-free requires [`ReadBy::Nobody`] with a written justification,
// which is visible in review instead of silent.

/// Where a `settings.json` key is actually read, and the proof it still is.
struct Attribution {
    /// Repo-relative path of the consuming file.
    file: &'static str,
    /// A literal that must appear in `file`. Pick the expression that reads the
    /// key (or the construct that acts on it) — NOT the bare key name, which a
    /// stale doc comment would satisfy.
    needle: &'static str,
    /// Where the effect lands, when that is a different file from `file`.
    /// Documentation for the reader; not enforced.
    effect: &'static str,
}

/// How a `settings.json` key earns the control that writes it.
enum ReadBy {
    /// The daemon reads the key and acts on it.
    Daemon(Attribution),
    /// QML reads the key at render/apply time.
    Qml(Attribution),
    /// Deliberately has NO consumer. Legal ONLY for a key that renders no
    /// control on either surface; the string is the justification.
    ///
    /// Unconstructed today — every key in the table has a consumer, which is
    /// the point of #416. It stays because it is the escape hatch the gate is
    /// designed around: a future render-only key needs a VISIBLE, justified
    /// row here rather than a silent pass, and deleting the variant would
    /// leave a contributor with no legal way to declare one.
    #[allow(dead_code)]
    Nobody(&'static str),
}

fn daemon(file: &'static str, needle: &'static str, effect: &'static str) -> ReadBy {
    ReadBy::Daemon(Attribution {
        file,
        needle,
        effect,
    })
}

fn qml(file: &'static str, needle: &'static str, effect: &'static str) -> ReadBy {
    ReadBy::Qml(Attribution {
        file,
        needle,
        effect,
    })
}

/// THE table: one row per key `SettingsStore.qml` declares, naming what reads
/// it. `every_settings_key_has_a_named_consumer` asserts this is EXACTLY that
/// key set, and that every named file still contains its needle.
fn settings_consumer_table() -> Vec<(&'static str, ReadBy)> {
    vec![
        // ── Appearance ──────────────────────────────────────────────────────
        (
            "themeMode",
            qml(
                "shell/components/Theme.qml",
                "SettingsStore.themeMode",
                "drives the whole palette + the `auto` schedule timer",
            ),
        ),
        (
            "autoThemeDarkStart",
            qml(
                "shell/components/Theme.qml",
                "SettingsStore.autoThemeDarkStart",
                "the `auto` themeMode flip-to-dark hour",
            ),
        ),
        (
            "autoThemeLightStart",
            qml(
                "shell/components/Theme.qml",
                "SettingsStore.autoThemeLightStart",
                "the `auto` themeMode flip-to-light hour",
            ),
        ),
        (
            "reduceMotion",
            qml(
                "shell/components/Theme.qml",
                "SettingsStore.reduceMotion",
                "every animation duration in shell/components/lib/ collapses to 0",
            ),
        ),
        (
            "textScale",
            qml(
                "shell/components/Theme.qml",
                "SettingsStore.textScale",
                "multiplies every Theme.font* tier",
            ),
        ),
        (
            "wallpaperPath",
            qml(
                "shell/components/HomeScreen.qml",
                "SettingsStore.wallpaperPath",
                "the home-screen wallpaper Image source + visibility",
            ),
        ),
        (
            "widgets",
            qml(
                "shell/widgets/lib/WidgetRegistry.qml",
                "SettingsStore.widget(",
                "per-widget enabled/order/size for every home-screen widget",
            ),
        ),
        // ── Input ───────────────────────────────────────────────────────────
        (
            "controllerDebug",
            qml(
                "shell/components/ShellLayout.qml",
                "Theme.controllerDebug",
                "maps the debug overlay and the key-event tracing",
            ),
        ),
        (
            "rumbleEnabled",
            daemon(
                "daemon/src/input/mod.rs",
                "rumble_enabled_from",
                "gates every daemon-fired rumble; refreshed on set-config",
            ),
        ),
        (
            "keyBindings",
            daemon(
                "daemon/src/config.rs",
                "apply_binding_overrides",
                "overrides the daemon's action->button table",
            ),
        ),
        // ── Display ─────────────────────────────────────────────────────────
        (
            "hdrEnabled",
            qml(
                "shell/settings/DisplaySettings.qml",
                "applyHdr(SettingsStore.hdrEnabled)",
                "hyprctl keyword monitor, with/without the `cm,hdr` suffix",
            ),
        ),
        (
            "nightLightEnabled",
            qml(
                "shell/settings/DisplaySettings.qml",
                "applyNightLightSetting(SettingsStore.nightLightEnabled",
                "spawns/kills hyprsunset",
            ),
        ),
        (
            "nightLightTemp",
            qml(
                "shell/settings/DisplaySettings.qml",
                "SettingsStore.nightLightTemp)",
                "the -t argument handed to hyprsunset",
            ),
        ),
        (
            "overscan",
            qml(
                "shell/shell.qml",
                "Components.SettingsStore.overscan",
                "insets the shell PanelWindow's content rect per axis (#416)",
            ),
        ),
        (
            "autoDimEnabled",
            qml(
                "shell/components/DimOverlay.qml",
                "SettingsStore.autoDimEnabled",
                "arms the OLED auto-dim overlay",
            ),
        ),
        (
            "autoDimDelayMinutes",
            qml(
                "shell/components/DimOverlay.qml",
                "SettingsStore.autoDimDelayMinutes",
                "the auto-dim idle timer interval",
            ),
        ),
        // ── Power ───────────────────────────────────────────────────────────
        (
            "sleepTimerMinutes",
            qml(
                "shell/components/AutoSuspendController.qml",
                "SettingsStore.sleepTimerMinutes",
                "the auto-suspend timer interval and its running gate",
            ),
        ),
        (
            "wakeOnController",
            qml(
                "shell/shell.qml",
                "Components.SettingsStore.wakeOnController",
                "gates avController.wake() on controller activity (#130)",
            ),
        ),
        // ── Audio ───────────────────────────────────────────────────────────
        //
        // Both are read by the store itself: it owns the boot-time re-apply
        // Processes, because nothing else re-asserts them after a reboot
        // (WirePlumber and PipeWire both revert). The needle is the Process id,
        // not the key, so deleting the re-apply fails this gate.
        (
            "defaultSink",
            qml(
                "shell/components/SettingsStore.qml",
                "startupSinkApply",
                "wpctl set-default at shell startup (#131)",
            ),
        ),
        (
            "audioCardProfile",
            qml(
                "shell/components/SettingsStore.qml",
                "startupCardProfileApply",
                "pactl set-card-profile at shell startup (#234)",
            ),
        ),
        // ── CEC ─────────────────────────────────────────────────────────────
        (
            "cecFocusOnStartup",
            daemon(
                "daemon/src/cec.rs",
                "cec_focus_on_startup(&",
                "runs the wake/claim-active-source sequence once at daemon start",
            ),
        ),
        (
            "cecFocusOnWake",
            daemon(
                "daemon/src/cec.rs",
                "cec_focus_on_wake(&",
                "claims active source on resume from sleep",
            ),
        ),
        (
            "cecAutoSwitchOnPowerOn",
            daemon(
                "daemon/src/cec.rs",
                "cec_auto_switch_on_power_on(",
                "switches the TV/AVR input when a device powers on (#415)",
            ),
        ),
        (
            "cecDefaultInput",
            daemon(
                "daemon/src/cec.rs",
                "cec_default_input(",
                "the logical address the auto-switch selects (#415)",
            ),
        ),
        (
            "cecDeviceNames",
            qml(
                "shell/settings/AVControlSettings.qml",
                "SettingsStore.cecDeviceNames[",
                "friendly-name override per logical address; panel/src/pages/cec.rs reads it too",
            ),
        ),
        // ── Apps ────────────────────────────────────────────────────────────
        (
            "prewarmApps",
            qml(
                "shell/shell.qml",
                "Components.SettingsStore.prewarmApps",
                "handed to AppLifecycleManager, which silently launches them at login (#238)",
            ),
        ),
        (
            "webApps",
            qml(
                "shell/settings/WebAppsSettings.qml",
                "SettingsStore.webApps",
                "lists the daemon-owned registry; daemon/src/webapps.rs is the writer",
            ),
        ),
    ]
}

/// Blank whole-line `//` comments, preserving byte offsets, so a commented-out
/// schema row can never be parsed as a live one. Separate from
/// `blank_comment_lines` only because that one's panic message names `main.rs`.
fn blank_qml_comment_lines(src: &str) -> String {
    src.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                " ".repeat(l.len())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One `_schema` row from `SettingsStore.qml`.
struct QmlSettingKey {
    key: String,
    /// `noSave: true` — a daemon-owned key the store mirrors read-only and
    /// never sends back in a `set-config` payload.
    no_save: bool,
}

/// `SettingsStore.qml` verbatim. `include_str!` rather than a runtime read so a
/// moved or renamed file is a COMPILE error, not a gate that quietly stops
/// guarding anything.
const SETTINGS_STORE_QML: &str = include_str!("../../shell/components/SettingsStore.qml");

/// Parse the `_schema` table out of `SettingsStore.qml`. Strict on purpose: a
/// row this cannot read is a key the gate cannot check, so it panics rather
/// than skipping.
fn parse_settings_store_schema(src: &str) -> Vec<QmlSettingKey> {
    const MARKER: &str = "readonly property var _schema: [";
    let src = blank_qml_comment_lines(src);
    let open = src
        .find(MARKER)
        .expect("SettingsStore.qml must declare `readonly property var _schema: [`")
        + MARKER.len();

    // Walk to the `]` that closes the array (depth 1 on entry).
    let mut depth = 1i32;
    let mut close = None;
    for (i, c) in src[open..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &src[open..close.expect("SettingsStore.qml: unterminated `_schema` array")];

    // Each row is one brace-delimited object, and none of them nests braces.
    let mut rows: Vec<&str> = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in body.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                assert!(
                    depth >= 0,
                    "SettingsStore.qml: unbalanced `}}` in `_schema`"
                );
                if depth == 0 {
                    rows.push(&body[start..i]);
                }
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "SettingsStore.qml: unterminated `_schema` row");
    assert_eq!(
        rows.len(),
        body.matches("key:").count(),
        "SettingsStore.qml: the `_schema` parser found {} rows but {} `key:` fields — a row \
         it cannot see is a key this gate cannot check",
        rows.len(),
        body.matches("key:").count()
    );

    rows.iter()
        .map(|row| {
            const K: &str = "key: \"";
            let at = row.find(K).unwrap_or_else(|| {
                panic!("SettingsStore.qml: `_schema` row with no `key:` — {row}")
            }) + K.len();
            let end = at
                + row[at..].find('"').unwrap_or_else(|| {
                    panic!("SettingsStore.qml: unterminated key literal — {row}")
                });
            QmlSettingKey {
                key: row[at..end].to_string(),
                no_save: row.contains("noSave: true"),
            }
        })
        .collect()
}

/// The repo root — `panel/`'s parent. The gate reaches outside the crate on
/// purpose: `settings.json` is one contract spanning the shell, the daemon and
/// the panel, so a gate that could only see `panel/` would miss most of it.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR (panel/) must have a parent")
        .to_path_buf()
}

/// Every key a QML settings page renders a control for: any `shell/settings/`
/// page that reads `SettingsStore.<key>` or calls its `set<Key>(` setter.
fn qml_control_keys(keys: &[String]) -> std::collections::BTreeSet<String> {
    let dir = repo_root().join("shell/settings");
    let mut sources: Vec<String> = Vec::new();
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("readdir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("qml") {
            sources.push(
                std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display())),
            );
        }
    }
    assert!(
        !sources.is_empty(),
        "no .qml settings pages under {} — the control scan would pass vacuously",
        dir.display()
    );

    let mut out = std::collections::BTreeSet::new();
    for key in keys {
        let mut setter = String::from("SettingsStore.set");
        let mut chars = key.chars();
        if let Some(first) = chars.next() {
            setter.extend(first.to_uppercase());
            setter.push_str(chars.as_str());
        }
        setter.push('(');
        let read = format!("SettingsStore.{key}");
        if sources
            .iter()
            .any(|s| s.contains(&read) || s.contains(&setter))
        {
            out.insert(key.clone());
        }
    }
    out
}

/// The panel's typed schema must stay the QML store's schema minus the two
/// daemon-owned mirrors — the "KEEP IN SYNC" note above [`SCHEMA`] with an
/// assertion behind it. Drift here is how a key ends up editable on one
/// surface and invisible on the other.
#[test]
fn panel_settings_schema_matches_the_qml_settings_store() {
    let qml_keys = parse_settings_store_schema(SETTINGS_STORE_QML);

    let expected: Vec<&str> = qml_keys
        .iter()
        .filter(|k| !k.no_save)
        .map(|k| k.key.as_str())
        .collect();
    let actual: Vec<&str> = crate::pages::settings::SCHEMA
        .iter()
        .map(|f| f.key)
        .collect();

    for key in &expected {
        assert!(
            actual.contains(key),
            "SettingsStore.qml declares `{key}` but panel SCHEMA does not — the panel cannot \
             edit a key it has no field for. Add it to SCHEMA, or mark it `noSave` in the QML \
             schema if the daemon owns it."
        );
    }
    for key in &actual {
        assert!(
            expected.contains(key),
            "panel SCHEMA has a field for `{key}` but SettingsStore.qml's `_schema` does not \
             declare it — the store will drop the value on its next read-back."
        );
    }

    // `noSave` keys are the daemon-owned mirrors, and the panel must agree they
    // are read-only rather than render a typed input that cannot stick.
    for k in qml_keys.iter().filter(|k| k.no_save) {
        assert!(
            crate::pages::settings::DAEMON_OWNED_KEYS.contains(&k.key.as_str()),
            "`{}` is `noSave` in SettingsStore.qml (daemon-owned) but is not in the panel's \
             DAEMON_OWNED_KEYS, so the panel would offer to write a key the store never sends",
            k.key
        );
    }
}

/// THE gate. Every key `SettingsStore.qml` declares must have a row in
/// [`settings_consumer_table`]; every row must name a file that still contains
/// its needle; and a key declared consumer-free must render NO control on
/// either surface.
///
/// This is what stops the failure mode that has now shipped three times: a
/// control that saves a key nothing reads, so the UI reports an effect that
/// never happens.
#[test]
fn every_settings_key_has_a_named_consumer() {
    let declared: Vec<String> = parse_settings_store_schema(SETTINGS_STORE_QML)
        .into_iter()
        .map(|k| k.key)
        .collect();
    let table = settings_consumer_table();

    // 1. The table is exactly the declared key set.
    for key in &declared {
        assert!(
            table.iter().any(|(k, _)| k == key),
            "settings key `{key}` is declared in SettingsStore.qml but has no row in \
             settings_consumer_table(). Add one naming what reads it — or, if nothing does \
             and nothing should, ReadBy::Nobody with the reason (and remove its \
             controls)."
        );
    }
    for (key, _) in &table {
        assert!(
            declared.iter().any(|d| d == key),
            "settings_consumer_table() has a row for `{key}`, which SettingsStore.qml no \
             longer declares — drop the row"
        );
    }

    // 2. Where a control for each key is rendered.
    let panel_controls: Vec<&str> = crate::pages::settings::SCHEMA
        .iter()
        .map(|f| f.key)
        .collect();
    let shell_controls = qml_control_keys(&declared);

    // 3. Each attribution must still be true of the file it names.
    let root = repo_root();
    for (key, read_by) in &table {
        let attribution = match read_by {
            // The classification is load-bearing, not decorative: a `Daemon`
            // row must name daemon (or protocol) source and a `Qml` row must
            // name a .qml file, so a key cannot be filed under the wrong side
            // of the IPC boundary and still pass.
            ReadBy::Daemon(a) => {
                assert!(
                    a.file.starts_with("daemon/") || a.file.starts_with("protocol/"),
                    "`{key}` is classified daemon-consumed but attributed to {}, which is not                      daemon source. Use ReadBy::Qml for a shell-side consumer.",
                    a.file
                );
                a
            }
            ReadBy::Qml(a) => {
                assert!(
                    a.file.ends_with(".qml"),
                    "`{key}` is classified QML-consumed but attributed to {}, which is not a                      .qml file. Use ReadBy::Daemon for a daemon-side consumer.",
                    a.file
                );
                a
            }
            ReadBy::Nobody(why) => {
                assert!(
                    !why.trim().is_empty(),
                    "`{key}` is declared read-by-nobody with an empty justification"
                );
                let panel = panel_controls.contains(key);
                let shell = shell_controls.contains(*key);
                assert!(
                    !panel && !shell,
                    "`{key}` is declared read-by-nobody ({why}) but renders a control \
                     (panel SCHEMA: {panel}, shell/settings: {shell}). That is the exact bug \
                     this gate exists for — the UI would report an effect nothing applies. \
                     Wire it, or remove the control."
                );
                continue;
            }
        };

        let path = root.join(attribution.file);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "`{key}` is attributed to {}, which cannot be read ({e}). Point the row at the \
                 file that consumes the key.",
                attribution.file
            )
        });
        assert!(
            src.contains(attribution.needle),
            "`{key}` is attributed to {}, which no longer contains `{}`. Either the consumer \
             moved (update the row) or it was deleted — in which case the control that writes \
             `{key}` now reports an effect nothing applies. Effect was: {}",
            attribution.file,
            attribution.needle,
            attribution.effect
        );
    }

    // 4. Sanity: the control scan must actually find controls, or assertion 3's
    //    Nobody arm would be toothless.
    assert!(
        shell_controls.len() > 10,
        "the shell settings-page control scan found only {} keys — it has probably stopped \
         matching, which would let a read-by-nobody key keep a live control",
        shell_controls.len()
    );
}

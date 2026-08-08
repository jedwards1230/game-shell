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
use crate::ipc::IpcClient;
use crate::pages;
use crate::state::AppState;

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
        ipc: IpcClient::new(sock),
        bridge: BridgeClient::new(None, None),
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
/// (good for a single `IpcClient::command` call), the real Settings/Widgets
/// flows make TWO separate connections per page load or save (each
/// `IpcClient` request opens its own connection — see `ipc.rs`'s doc
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
/// `IpcClient` at the returned socket path.
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
    Arc::new(AppState {
        cfg: AppConfig::default(),
        ipc: IpcClient::new(sock),
        bridge: BridgeClient::new(None, None),
        recovery: Recovery::new(),
        updates: crate::updates::UpdatesState::default(),
    })
}

#[tokio::test]
async fn settings_page_renders_current_config() {
    let (sock, _received) = spawn_config_daemon(
        "settings-page",
        r#"{"themeMode":"light","rumbleEnabled":false}"#,
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::settings::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.contains("light"),
        "settings page must render the current themeMode value: {html}"
    );
}

#[tokio::test]
async fn settings_save_sends_expected_patch() {
    let (sock, received) = spawn_config_daemon("settings-save", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);

    let mut form: HashMap<String, String> = HashMap::new();
    form.insert("themeMode".to_string(), "light".to_string());
    form.insert("rumbleEnabled".to_string(), "on".to_string()); // checked
    form.insert("wallpaperPath".to_string(), "/home/u/wall.png".to_string());
    // StrList textarea: blank + padded lines must be dropped, order kept.
    form.insert(
        "prewarmApps".to_string(),
        "tv.plex.PlexHTPC\r\n\r\n  com.spotify.Client  \n".to_string(),
    );
    // controllerDebug intentionally absent from the form -> must become
    // explicit `false`, not be omitted.

    let html = pages::settings::render_save(&state, &form).await;
    assert!(
        html.to_lowercase().contains("saved"),
        "expected ok result: {html}"
    );

    let sent = received.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one set-config call: {sent:?}"
    );
    let patch: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    assert_eq!(patch["themeMode"], "light");
    assert_eq!(patch["rumbleEnabled"], true);
    assert_eq!(patch["controllerDebug"], false);
    assert_eq!(patch["wallpaperPath"], "/home/u/wall.png");
    assert_eq!(
        patch["prewarmApps"],
        serde_json::json!(["tv.plex.PlexHTPC", "com.spotify.Client"])
    );
    assert!(
        patch.get("webApps").is_none(),
        "webApps is daemon-owned and must never appear in a Settings save patch: {patch}"
    );
    assert!(
        patch.get("keyBindings").is_none(),
        "keyBindings must never appear in a Settings save patch: {patch}"
    );
    assert!(
        patch.get("perGameBindings").is_none(),
        "perGameBindings must never appear in a Settings save patch: {patch}"
    );
    assert!(
        patch.get("perPlayerBindings").is_none(),
        "perPlayerBindings must never appear in a Settings save patch: {patch}"
    );
    assert!(
        patch.get("widgets").is_none(),
        "widgets must never appear in a Settings save patch: {patch}"
    );
}

#[tokio::test]
async fn settings_page_renders_pretty_printed_raw_json() {
    let (sock, _received) = spawn_config_daemon(
        "settings-raw-pretty-render",
        r#"{"themeMode":"dark","rumbleEnabled":true}"#,
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::settings::render_page(&state).await;
    assert!(
        html.contains("{\n"),
        "expected the raw JSON escape hatch to be pretty-printed (multi-line): {html}"
    );
}

#[tokio::test]
async fn settings_raw_pretty_input_is_sent_compact() {
    // The textarea round-trips a pretty-printed, multi-line document — the
    // set-config call it triggers must still be a single compact line.
    let (sock, received) = spawn_config_daemon("settings-raw-compact", "{}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let pretty = "{\n  \"themeMode\": \"light\"\n}";
    let html = pages::settings::render_save_raw(&state, pretty).await;
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
async fn settings_raw_rejects_malformed_json() {
    // No daemon needed: malformed JSON must be rejected before any IPC call.
    let state = hermetic_state();
    let html = pages::settings::render_save_raw(&state, "not json").await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("invalid"),
        "expected an error marker for malformed JSON: {html}"
    );
}

#[tokio::test]
async fn settings_raw_rejects_non_object_json() {
    let state = hermetic_state();
    let html = pages::settings::render_save_raw(&state, "[1,2,3]").await;
    assert!(
        html.to_lowercase().contains("invalid") || html.to_lowercase().contains("object"),
        "expected an error marker for a non-object JSON body: {html}"
    );
}

#[tokio::test]
async fn settings_page_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::settings::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "settings page must show an unreachable marker when the daemon is down: {html}"
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
async fn media_page_degrades_when_daemon_unreachable() {
    // The daemon owns wallpaperPath and the web-app registry, so with it down
    // the page must still render (200 + honest banner) rather than 500 — the
    // wallpaper FILES are local and still listable.
    let state = hermetic_state();
    let html = pages::media::render_page(&state).await;
    assert!(!html.is_empty());
    assert!(
        html.to_lowercase().contains("unreachable"),
        "media page must show an unreachable marker when the daemon is down: {html}"
    );
    // Both sections still render their shells.
    assert!(html.contains("Wallpapers"), "missing wallpapers section");
    assert!(html.contains("Web apps"), "missing web apps section");
}

#[tokio::test]
async fn media_webapp_add_relays_a_compact_json_body() {
    // The panel must not validate/allocate ids itself — it relays name+url and
    // lets the daemon (the registry's sole writer) do the work.
    let (sock, received) = spawn_recording_daemon(
        "media-webapp-add",
        r#"{"id":"youtube","name":"YouTube","url":"https://youtube.com/tv","wmClass":"tvshell-youtube"}"#,
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let _ =
        pages::media::render_webapp_add(&state, "  YouTube  ", " https://youtube.com/tv ").await;
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
// M3: Tools console
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_intent_rejects_whitespace_without_ipc() {
    // Validation must fail before any IPC call — no daemon needed.
    let state = hermetic_state();
    let html = pages::tools::render_intent(&state, "settings audio").await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

#[tokio::test]
async fn tools_intent_degrades_when_daemon_unreachable() {
    let state = hermetic_state();
    let html = pages::tools::render_intent(&state, "home").await;
    assert!(
        html.to_lowercase().contains("unreachable"),
        "expected a daemon-unreachable marker: {html}"
    );
}

#[tokio::test]
async fn tools_key_rejects_unknown_key_without_ipc() {
    let state = hermetic_state();
    let html = pages::tools::render_key(&state, "north").await;
    assert!(
        html.to_lowercase().contains("unknown key"),
        "expected an unknown-key error: {html}"
    );
}

#[tokio::test]
async fn tools_net_ping_rejects_whitespace_in_host() {
    let state = hermetic_state();
    let html = pages::tools::render_net_ping(&state, "1.1.1.1 extra", None).await;
    assert!(
        html.to_lowercase().contains("whitespace"),
        "expected a whitespace validation error: {html}"
    );
}

#[tokio::test]
async fn tools_net_ping_rejects_out_of_range_count() {
    let state = hermetic_state();
    let html = pages::tools::render_net_ping(&state, "1.1.1.1", Some("99")).await;
    assert!(
        html.contains("1 and 10"),
        "expected a count-range validation error: {html}"
    );
}

#[tokio::test]
async fn tools_net_throughput_rejects_path_separator_in_iface() {
    let state = hermetic_state();
    let html = pages::tools::render_net_throughput(&state, "../etc").await;
    assert!(
        html.to_lowercase().contains("invalid interface"),
        "expected an invalid-interface error: {html}"
    );
}

#[tokio::test]
async fn tools_bt_action_rejects_unknown_action() {
    let state = hermetic_state();
    let html = pages::tools::render_bt_action(&state, "AA:BB:CC:DD:EE:FF", "reboot").await;
    assert!(
        html.to_lowercase().contains("unknown bluetooth action"),
        "expected an unknown-action error: {html}"
    );
}

#[tokio::test]
async fn tools_sys_status_json_roundtrip() {
    let mut replies = HashMap::new();
    replies.insert(
        "sys-status",
        r#"{"os":"Test OS","kernel":"1.2.3","hostname":"h","uptime":"1h"}"#,
    );
    let sock = spawn_canned_daemon("tools-sys-status", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::tools::run_line(&state, "sys-status").await;
    assert!(
        html.contains("Test OS"),
        "expected the pretty-printed sys-status JSON: {html}"
    );
}

#[tokio::test]
async fn tools_bt_power_status_bare_text_roundtrip() {
    let mut replies = HashMap::new();
    replies.insert("bt-power-status", "bt:on");
    let sock = spawn_canned_daemon("tools-bt-power", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::tools::run_line(&state, "bt-power-status").await;
    assert!(
        html.contains("bt:on"),
        "expected the bare-text reply: {html}"
    );
}

#[tokio::test]
async fn tools_raw_error_reply_roundtrip() {
    let mut replies = HashMap::new();
    replies.insert("sys-metrics", "error:input-runtime-down");
    let sock = spawn_canned_daemon("tools-raw-error", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::tools::render_raw(&state, "sys-metrics").await;
    assert!(
        html.to_lowercase().contains("input-runtime-down"),
        "expected the daemon's error message: {html}"
    );
}

#[tokio::test]
async fn tools_raw_warns_on_guarded_command() {
    let mut replies = HashMap::new();
    replies.insert("grab", "ok");
    let sock = spawn_canned_daemon("tools-raw-warn", replies);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = state_for_socket(sock);
    let html = pages::tools::render_raw(&state, "grab").await;
    assert!(
        html.to_lowercase().contains("guarded"),
        "expected a warning banner for a guarded command: {html}"
    );
}

#[tokio::test]
async fn tools_raw_rejects_empty_command() {
    let state = hermetic_state();
    let html = pages::tools::render_raw(&state, "   ").await;
    assert!(
        html.to_lowercase().contains("empty"),
        "expected an empty-command validation error: {html}"
    );
}

// ---------------------------------------------------------------------------
// M3: Processes page
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
}

#[tokio::test]
async fn processes_restart_rejects_unknown_unit_key() {
    let state = hermetic_state();
    let html = pages::processes::render_restart(&state, "bogus").await;
    assert!(
        html.to_lowercase().contains("unknown"),
        "expected an unknown-unit-key error: {html}"
    );
}

#[tokio::test]
async fn processes_page_renders_system_updates_section() {
    let state = hermetic_state();
    let html = pages::processes::render_page(&state).await;
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
}

// ---------------------------------------------------------------------------
// M3: Dev screenshot viewer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dev_screenshot_capture_degrades_when_bridge_not_configured() {
    let state = hermetic_state();
    let html = pages::dev::render_screenshot_capture(&state).await;
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
    /// Part of the S5 root-equivalent set — registered only under
    /// `[panel].allow_dangerous = true`.
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
        dangerous: true,
        exec_backed: true,
    }
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
/// exactly the set of routes `main.rs` declares — so adding an ungated route
/// without adding it here fails the suite.
fn route_table() -> Vec<RouteSpec> {
    vec![
        r("/", "/", Get, Authenticated),
        r("/dashboard", "/dashboard", Get, Authenticated),
        r("/dashboard/tiles", "/dashboard/tiles", Get, Authenticated),
        r(
            "/dashboard/updates-tile",
            "/dashboard/updates-tile",
            Get,
            Authenticated,
        ),
        r("/processes", "/processes", Get, Authenticated),
        // Deliberately an unknown unit key: the handler rejects it before
        // touching `systemctl`, so this row is inert even if probed.
        recovery(
            "/processes/restart/{key}",
            "/processes/restart/not-a-unit",
            Post,
        ),
        r(
            "/processes/updates/refresh",
            "/processes/updates/refresh",
            Post,
            Authenticated,
        ),
        r(
            "/processes/updates/job",
            "/processes/updates/job",
            Get,
            Authenticated,
        ),
        r("/settings", "/settings", Get, Authenticated),
        r("/settings/save", "/settings/save", Post, Authenticated),
        r("/settings/raw", "/settings/raw", Post, Authenticated),
        r("/widgets", "/widgets", Get, Authenticated),
        r("/widgets/save", "/widgets/save", Post, Authenticated),
        r(
            "/widgets/reorder/{id}/up",
            "/widgets/reorder/plex/up",
            Post,
            Authenticated,
        ),
        r(
            "/widgets/reorder/{id}/down",
            "/widgets/reorder/plex/down",
            Post,
            Authenticated,
        ),
        r("/tools", "/tools", Get, Authenticated),
        r("/tools/intent", "/tools/intent", Post, Authenticated),
        r("/tools/key", "/tools/key", Post, Authenticated),
        r("/tools/apps/list", "/tools/apps/list", Post, Authenticated),
        r(
            "/tools/apps/launch",
            "/tools/apps/launch",
            Post,
            Authenticated,
        ),
        r(
            "/tools/apps/recents",
            "/tools/apps/recents",
            Post,
            Authenticated,
        ),
        r(
            "/tools/bt/power-status",
            "/tools/bt/power-status",
            Post,
            Authenticated,
        ),
        r(
            "/tools/bt/power-on",
            "/tools/bt/power-on",
            Post,
            Authenticated,
        ),
        r(
            "/tools/bt/power-off",
            "/tools/bt/power-off",
            Post,
            Authenticated,
        ),
        r(
            "/tools/bt/scan-on",
            "/tools/bt/scan-on",
            Post,
            Authenticated,
        ),
        r(
            "/tools/bt/scan-off",
            "/tools/bt/scan-off",
            Post,
            Authenticated,
        ),
        r("/tools/bt/list", "/tools/bt/list", Post, Authenticated),
        r("/tools/bt/action", "/tools/bt/action", Post, Authenticated),
        r(
            "/tools/net/status",
            "/tools/net/status",
            Post,
            Authenticated,
        ),
        r(
            "/tools/net/wifi-list",
            "/tools/net/wifi-list",
            Post,
            Authenticated,
        ),
        r(
            "/tools/net/wifi-rescan",
            "/tools/net/wifi-rescan",
            Post,
            Authenticated,
        ),
        r(
            "/tools/net/throughput",
            "/tools/net/throughput",
            Post,
            Authenticated,
        ),
        r("/tools/net/ping", "/tools/net/ping", Post, Authenticated),
        r(
            "/tools/power/can-suspend",
            "/tools/power/can-suspend",
            Post,
            Authenticated,
        ),
        r(
            "/tools/power/battery",
            "/tools/power/battery",
            Post,
            Authenticated,
        ),
        r(
            "/tools/sys/status",
            "/tools/sys/status",
            Post,
            Authenticated,
        ),
        r(
            "/tools/sys/metrics",
            "/tools/sys/metrics",
            Post,
            Authenticated,
        ),
        r(
            "/tools/sys/storage",
            "/tools/sys/storage",
            Post,
            Authenticated,
        ),
        r(
            "/tools/sys/build-info",
            "/tools/sys/build-info",
            Post,
            Authenticated,
        ),
        r(
            "/tools/sys/controllerdb-status",
            "/tools/sys/controllerdb-status",
            Post,
            Authenticated,
        ),
        r(
            "/tools/sys/controllerdb-refresh",
            "/tools/sys/controllerdb-refresh",
            Post,
            Authenticated,
        ),
        r("/controllers", "/controllers", Get, Authenticated),
        r(
            "/controllers/grab",
            "/controllers/grab",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/release",
            "/controllers/release",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/handoff",
            "/controllers/handoff",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/pad/battery",
            "/controllers/pad/battery",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/pad/rumble-status",
            "/controllers/pad/rumble-status",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/pad/rumble",
            "/controllers/pad/rumble",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/input-devices",
            "/controllers/input-devices",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/bindings/set",
            "/controllers/bindings/set",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/bindings/capture",
            "/controllers/bindings/capture",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/bindings/capture-cancel",
            "/controllers/bindings/capture-cancel",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/active-game/set",
            "/controllers/active-game/set",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/active-game/clear",
            "/controllers/active-game/clear",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/controllerdb/status",
            "/controllers/controllerdb/status",
            Post,
            Authenticated,
        ),
        r(
            "/controllers/controllerdb/refresh",
            "/controllers/controllerdb/refresh",
            Post,
            Authenticated,
        ),
        r("/cec", "/cec", Get, Authenticated),
        r("/cec/scan", "/cec/scan", Post, Authenticated),
        r("/cec/device", "/cec/device", Post, Authenticated),
        r(
            "/cec/active-source",
            "/cec/active-source",
            Post,
            Authenticated,
        ),
        r("/cec/power-on", "/cec/power-on", Post, Authenticated),
        r("/cec/power-off", "/cec/power-off", Post, Authenticated),
        r("/media", "/media", Get, Authenticated),
        r(
            "/media/wallpaper/upload",
            "/media/wallpaper/upload",
            Post,
            Authenticated,
        ),
        r(
            "/media/wallpaper/select",
            "/media/wallpaper/select",
            Post,
            Authenticated,
        ),
        r(
            "/media/wallpaper/delete",
            "/media/wallpaper/delete",
            Post,
            Authenticated,
        ),
        r(
            "/media/wallpaper/file",
            "/media/wallpaper/file",
            Get,
            Authenticated,
        ),
        r(
            "/media/webapp/add",
            "/media/webapp/add",
            Post,
            Authenticated,
        ),
        r(
            "/media/webapp/remove",
            "/media/webapp/remove",
            Post,
            Authenticated,
        ),
        r("/cec/test", "/cec/test", Post, Authenticated),
        r("/cec/osd-name", "/cec/osd-name", Post, Authenticated),
        // Recovery, deliberately NOT part of the dangerous set: restarting a
        // wedged daemon is the reason the panel exists.
        recovery(
            "/cec/recover/restart-daemon",
            "/cec/recover/restart-daemon",
            Post,
        ),
        r("/logs", "/logs", Get, Authenticated),
        r("/logs/view", "/logs/view", Get, Authenticated),
        r("/dev", "/dev", Get, Authenticated),
        // Recovery, deliberately NOT part of the dangerous set: these restart
        // the very same units `POST /processes/restart/{key}` restarts.
        recovery("/dev/restart-daemon", "/dev/restart-daemon", Post),
        recovery("/dev/restart-shell", "/dev/restart-shell", Post),
        r("/dev/screenshot", "/dev/screenshot", Get, Authenticated),
        r(
            "/dev/screenshot/capture",
            "/dev/screenshot/capture",
            Post,
            Authenticated,
        ),
        r(
            "/nav/daemon-status",
            "/nav/daemon-status",
            Get,
            Authenticated,
        ),
        // ── the four auth exemptions ──
        r("/login", "/login", Get, Public),
        r("/login", "/login", Post, Public),
        r("/assets/htmx.min.js", "/assets/htmx.min.js", Get, Public),
        r("/assets/style.css", "/assets/style.css", Get, Public),
        // ── the S5 root-equivalent set ──
        danger("/dev/deploy", Post),
        danger("/dev/build", Post),
        danger("/dev/reboot", Post),
        danger("/dev/suspend", Post),
        danger("/processes/updates/apply", Post),
        danger("/tools/raw", Post),
    ]
}

/// Parse every `.route("<path>", get|post` declaration out of `main.rs`.
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
fn parse_declared_routes(src: &str) -> Vec<(String, Method)> {
    /// The offending declaration, clipped, for the panic message.
    fn snippet(s: &str) -> String {
        s.chars().take(80).collect::<String>().replace('\n', " ")
    }

    let mut out = Vec::new();
    let mut rest = src;
    while let Some(idx) = rest.find(".route(") {
        rest = &rest[idx + ".route(".len()..];
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
        out.push((path.to_string(), method));
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
#[test]
fn route_table_matches_main_rs_declarations() {
    let declared = parse_declared_routes(include_str!("main.rs"));
    let table = route_table();

    let declared_paths: BTreeSet<&str> = declared.iter().map(|(p, _)| p.as_str()).collect();
    let table_paths: BTreeSet<&str> = table.iter().map(|r| r.declared).collect();
    assert_eq!(
        declared_paths, table_paths,
        "main.rs declares a route the auth route table doesn't cover (or vice versa) — \
         add it to `route_table()` with its access class"
    );

    // Stronger than the path-set check: also pin the (path, method) pairs, so
    // adding a second method to an existing path can't slip through.
    let mut declared_pairs: Vec<(&str, Method)> =
        declared.iter().map(|(p, m)| (p.as_str(), *m)).collect();
    let mut table_pairs: Vec<(&str, Method)> =
        table.iter().map(|r| (r.declared, r.method)).collect();
    declared_pairs.sort_unstable();
    table_pairs.sort_unstable();
    assert_eq!(declared_pairs, table_pairs);

    assert_eq!(
        declared.len(),
        90,
        "expected 88 pre-existing routes + GET/POST /login"
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
/// units as `POST /processes/restart/{key}` — is deliberately absent.
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
            "/dev/deploy",
            "/dev/reboot",
            "/dev/suspend",
            "/processes/updates/apply",
            "/tools/raw",
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

/// The unit-restart routes: recovery, always registered, never in the S5 set.
/// `/dev/restart-{daemon,shell}` and `/processes/restart/{key}` hit the same
/// two systemd units, so gating one and leaving the other open bought nothing.
const RECOVERY_ROUTES: [&str; 4] = [
    "/processes/restart/{key}",
    "/cec/recover/restart-daemon",
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

fn state_with(cfg: AppConfig) -> SharedState {
    let sock = std::path::PathBuf::from(format!(
        "/tmp/tvshp-router-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ));
    let bridge = BridgeClient::new(cfg.http_bridge_base.clone(), cfg.http_token.clone());
    Arc::new(AppState {
        cfg,
        ipc: IpcClient::new(sock),
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
        "/processes/restart/not-a-unit",
        "/cec/recover/restart-daemon",
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
        .get(format!("{base}/dashboard"))
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
    for path in ["/", "/dashboard", "/nav/daemon-status"] {
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

    // Two bridge-backed routes: the screenshot proxy (GET) and the log view
    // (GET, `dev_logs`). Both call `BridgeClient`, which attaches the daemon
    // token on every request it makes.
    for path in ["/dev/screenshot", "/logs/view"] {
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
        .get(format!("{base}/logs/view"))
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

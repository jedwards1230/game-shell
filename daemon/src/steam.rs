//! Steam library proxy for the home-screen Steam widget.
//!
//! Mirrors [`crate::plex`]: a stateless, cross-platform handler served from
//! `ipc.rs` like `list-apps`, not a Linux-only actor. The shell's `SteamWidget`
//! polls the `steam-library` IPC command and renders poster cards for the
//! installed Steam games on the gaming PC; selecting one fires `steam-launch
//! <appid>` (also handled here) and then starts the existing Moonlight stream.
//!
//! The actual enumeration + launch live in `tv-shell-host` — a thin
//! cross-platform sidecar that runs **on the gaming PC** (a *different machine*;
//! see `docs/HOST_SETUP.md`). This daemon module is an **HTTP client** to that
//! host: it proxies the host's `GET /library` / `GET /status` / `POST /launch` /
//! `POST /open-bpm` / `POST /quit` / `POST /sleep` over the LAN via the reusable
//! [`crate::sidecar::Sidecar`] client. It does **not** spawn, supervise, or
//! restart the host — the host is deployed independently on the gaming PC.
//!
//! **Config** lives in `~/.config/tv-shell/config.toml` under `[steam]`:
//! - `url`        — tv-shell-host base, e.g. `http://192.0.2.1:47995`
//! - `token_file` — path to a `0600` file holding the host's bearer token
//!   (`TV_SHELL_HOST_TOKEN`). The token lives only in the daemon's config;
//!   QML never reads it.
//!
//! The reply carries a `status` field from the shared [`crate::service_health`]
//! vocabulary (`disabled`/`ok`/`unreachable`/`error`) so the widget tells a down
//! host apart from an empty library. When unconfigured the command returns
//! `{"status":"disabled",…}` so the widget collapses to nothing.
//!
//! Host responses are deserialized into the shared [`tv_shell_protocol`] types
//! (`LibraryResponse` / `StatusResponse` / `QuitResponse` / `SleepResponse`), so
//! the daemon↔host JSON contract is single-sourced and can't silently drift. An
//! action endpoint whose *body* carries the decision (`/quit`, `/sleep` — both
//! answer HTTP 200 whether they acted or refused) is read with `post_json`, never
//! the body-discarding `post`. The response *normalizer*
//! ([`normalize_library`]) is a pure function, unit-tested on every platform; only
//! the live fetch needs a reachable host.

use crate::daemon_config::SteamHostConfig;
use crate::service_health::ServiceStatus;
use crate::sidecar::{url_host, Sidecar};
use serde_json::{json, Value};
use tv_shell_protocol::{
    LibraryEntry, LibraryResponse, QuitResponse, SleepResponse, StatusResponse,
};

/// Recently-played rail cap. The rows only show a handful at 4K.
const RECENT_LIMIT: usize = 12;

/// `/status` is a tiny JSON; refuse a body advertising more than this (a
/// misconfigured/compromised host could otherwise stream a huge body into memory
/// — the request timeout alone doesn't cap size).
const MAX_STATUS_BODY: u64 = 64 * 1024;

/// `/library` is legitimately larger (one entry per game); 10 MiB covers ~1000
/// games while still refusing a rogue host's unbounded body.
const MAX_LIBRARY_BODY: u64 = 10 * 1024 * 1024;

/// `/sleep` answers `{ok, reason}` — tiny, like `/status`. Same rationale for the
/// cap: a misconfigured/compromised host must not be able to stream an unbounded
/// body into memory.
const MAX_SLEEP_BODY: u64 = 64 * 1024;

/// `/quit` answers `{ok, appid, reason}` — tiny, like `/sleep`. Same rationale
/// for the cap.
const MAX_QUIT_BODY: u64 = 64 * 1024;

/// The ACTIVE Steam host: the configured host whose name matches the persisted
/// `steamServer` selection (settings.json, written by `steam-set-host`), else
/// the first configured host. `None` when no hosts are configured.
pub(crate) fn active_host() -> Option<SteamHostConfig> {
    let hosts = crate::daemon_config::global().steam_hosts();
    let selected = crate::config::steam_server(&crate::config::settings_path());
    pick_host(hosts, selected.as_deref())
}

/// Pure selection rule for [`active_host`]: the host named `selected` when it
/// exists, else the first. A stale/unknown selection (host removed from
/// config.toml) degrades to the first host rather than breaking the widget.
fn pick_host(hosts: Vec<SteamHostConfig>, selected: Option<&str>) -> Option<SteamHostConfig> {
    if let Some(name) = selected {
        if let Some(h) = hosts.iter().find(|h| h.name == name) {
            return Some(h.clone());
        }
    }
    hosts.into_iter().next()
}

/// Resolve the Steam sidecar (base URL + bearer token) for the ACTIVE host, or
/// `None` when the widget is unconfigured (no hosts, or no resolvable token).
///
/// `pub(crate)` so [`crate::service_health`] can reuse the same resolution for the
/// reachability probe — one source of truth for "is Steam configured?".
pub(crate) fn sidecar() -> Option<Sidecar> {
    let cfg = crate::daemon_config::global();
    let host = active_host()?;
    // steam_token_for() is Result; a hard error or missing token here just means
    // "widget not configured".
    let token = cfg.steam_token_for(&host).ok().flatten()?;
    Sidecar::from_parts(&host.url, &token)
}

/// IPC entry point for `steam-library`. Returns
/// `{"status":<status>,"recentlyPlayed":[…],"allGames":[…]}` where `status` is
/// the shared [`crate::service_health::ServiceStatus`] vocabulary.
///
/// A lightweight `GET /status` reachability probe runs first; only on `Ok` is the
/// library fetched, so a down host yields empty rails *with a status the widget
/// can render*. Unconfigured ⇒ `disabled` (widget collapses).
pub async fn handle_steam_library() -> String {
    // The active host's name-part rides every reply (`"host"`) so the widget
    // knows WHOM it is checking — and whom the Wake card should target — without
    // a config of its own. Resolved before the sidecar so even a token-less
    // (⇒ disabled) but host-selected config still reports the host.
    let host = active_host().as_ref().and_then(|h| url_host(&h.url));
    let Some(sc) = sidecar() else {
        return empty_library(ServiceStatus::Disabled, host);
    };

    // Reachability gate — `GET /status` is small and authenticated, so its HTTP
    // status classifies cleanly (401 → Error/bad token, 5xx → Unreachable). We
    // also read its body to capture `running_appid` (the foreground game on the
    // host) and `streaming` (an active Moonlight/Sunshine stream) so the widget
    // can badge the "Playing" card and reflect stream state without a second
    // request.
    let (probe, running_appid, streaming) = fetch_status(&sc).await;
    if probe != ServiceStatus::Ok {
        return empty_library(probe, host);
    }

    match sc
        .get_json::<LibraryResponse>("/library", MAX_LIBRARY_BODY)
        .await
    {
        Ok(resp) => normalize_library(&resp, sc.base(), running_appid, streaming),
        Err(e) => {
            tracing::debug!("steam-library fetch failed: {e}");
            empty_library(ServiceStatus::Unreachable, host)
        }
    }
}

/// Empty-rails reply for a non-`Ok` library state (`disabled`/`unreachable`/
/// `error`): no running game, not streaming. The shape matches the populated
/// reply so the widget binds the same fields either way — including `host`
/// (the active host's name-part, or null), which the Wake card needs precisely
/// when the host is down.
fn empty_library(status: ServiceStatus, host: Option<String>) -> String {
    json!({
        "status": status.as_str(),
        "recentlyPlayed": [],
        "allGames": [],
        "runningAppid": Value::Null,
        "streaming": false,
        "host": host,
    })
    .to_string()
}

/// `GET /status` on the sidecar: returns the reachability classification plus the
/// foreground-game id and stream flag parsed from the same response (one request
/// serves both the probe and the metadata the widget badges with). The body is
/// deserialized into [`StatusResponse`]; a too-large or unparseable body degrades
/// to `(status, None, false)` so the caller still gets a clean status to surface.
/// `running_appid` 0 is treated as "nothing running" (an unaddressable id).
async fn fetch_status(sc: &Sidecar) -> (ServiceStatus, Option<u32>, bool) {
    let (status, body) = sc.get_classified("/status", MAX_STATUS_BODY).await;
    if status != ServiceStatus::Ok {
        return (status, None, false);
    }
    match body.and_then(|b| serde_json::from_str::<StatusResponse>(&b).ok()) {
        Some(s) => (status, s.running_appid.filter(|&id| id != 0), s.streaming),
        None => (status, None, false),
    }
}

/// IPC entry point for `steam-launch <appid>`. POSTs `{appid}` to the host's
/// `/launch` and returns `ok` / `error:*`. The Moonlight stream start stays in
/// QML — this only kicks off the game on the host.
pub async fn handle_steam_launch(appid: u32) -> String {
    let Some(sc) = sidecar() else {
        return crate::protocol::resp_error("steam not configured");
    };
    match sc.post("/launch", Some(&launch_body(appid))).await {
        Ok(()) => crate::protocol::resp_ok(),
        Err(e) => {
            tracing::debug!("steam-launch {appid} failed: {e}");
            crate::protocol::resp_error(&format!("steam-launch failed: {e}"))
        }
    }
}

/// IPC entry point for `steam-bigpicture`. POSTs to the host's `/open-bpm` (no
/// body) to reset Steam to the Big Picture HOME screen, and returns a compact-JSON
/// status object (`{"status":"ok"}` / `{"status":"error","reason":…}`). Mirrors
/// [`handle_steam_launch`] but lands on the BPM home rather than a game's page;
/// the Moonlight stream start stays in QML. Degrades gracefully when the host is
/// unconfigured (`disabled`) or unreachable (`error`), exactly like steam-launch.
pub async fn handle_steam_bigpicture() -> String {
    let Some(sc) = sidecar() else {
        return json!({
            "status": ServiceStatus::Disabled.as_str(),
            "reason": "steam not configured",
        })
        .to_string();
    };
    match sc.post("/open-bpm", None).await {
        Ok(()) => json!({ "status": ServiceStatus::Ok.as_str() }).to_string(),
        Err(e) => {
            tracing::debug!("steam-bigpicture failed: {e}");
            json!({
                "status": ServiceStatus::Error.as_str(),
                "reason": format!("steam-bigpicture failed: {e}"),
            })
            .to_string()
        }
    }
}

/// IPC entry point for `steam-quit <appid>`. POSTs `{appid}` to the host's `/quit`
/// to gracefully terminate the running game (SIGTERM to its process group — like
/// Steam's Stop button). Degrades to `disabled` when steam is unconfigured.
///
/// Three outcomes, deliberately distinguishable by a QML consumer — the same
/// shape [`handle_steam_suspend`] uses:
/// - quit sent → `{"status":"ok"}`
/// - REFUSED   → `{"status":"error","reason":"<host's reason>","refused":true}`
/// - failed    → `{"status":"error","reason":"steam-quit failed: …","refused":false}`
///
/// The refusal is the interesting one. The host answers HTTP **200** with
/// `{"ok":false,"appid":…,"reason":"not running"}` when no matching game process
/// exists, so it can't be told from success by status code alone — hence
/// [`crate::sidecar::Sidecar::post_json`] rather than the body-discarding `post`,
/// which used to flatten both into `{"status":"ok"}` and report a quit that
/// quit nothing. The host's reason is passed through **verbatim**, and `refused`
/// lets a caller branch without string-matching it.
pub async fn handle_steam_quit(appid: u32) -> String {
    let Some(sc) = sidecar() else {
        return json!({
            "status": ServiceStatus::Disabled.as_str(),
            "reason": "steam not configured",
        })
        .to_string();
    };
    match sc
        .post_json::<QuitResponse>("/quit", Some(&launch_body(appid)), MAX_QUIT_BODY)
        .await
    {
        Ok(resp) => quit_reply(&resp),
        Err(e) => {
            tracing::debug!("steam-quit {appid} failed: {e}");
            json!({
                "status": ServiceStatus::Error.as_str(),
                "reason": format!("steam-quit failed: {e}"),
                "refused": false,
            })
            .to_string()
        }
    }
}

/// Pure reply builder for [`handle_steam_quit`] — maps the host's
/// [`QuitResponse`] onto the daemon's compact-JSON status shape, mirroring
/// [`suspend_reply`]. A refusal with no `reason` (a host that answered but said
/// nothing) still reports as refused, with a generic message, rather than being
/// mistaken for success.
fn quit_reply(resp: &QuitResponse) -> String {
    if resp.ok {
        return json!({ "status": ServiceStatus::Ok.as_str() }).to_string();
    }
    let reason = resp.reason.as_deref().unwrap_or("the host quit nothing");
    json!({
        "status": ServiceStatus::Error.as_str(),
        "reason": reason,
        "refused": true,
    })
    .to_string()
}

/// IPC entry point for `steam-suspend`. POSTs to the host's `/sleep` (no body) to
/// put the gaming PC to sleep, and returns a compact-JSON status object. Mirrors
/// [`handle_steam_quit`]; degrades to `disabled` when steam is unconfigured.
///
/// Three outcomes, deliberately distinguishable by a QML consumer:
/// - accepted  → `{"status":"ok"}`
/// - REFUSED   → `{"status":"error","reason":"<host's reason>","refused":true}`
/// - failed    → `{"status":"error","reason":"steam-suspend failed: …","refused":false}`
///
/// The refusal is the interesting one. The host answers HTTP **200** with
/// `{"ok":false,"reason":…}` when suspending would strand a running game or a
/// live stream, so it can't be told from success by status code alone — hence
/// [`crate::sidecar::Sidecar::post_json`] rather than the body-discarding `post`.
/// The host's reason is passed through **verbatim** (it is written to be shown to
/// a person), and `refused` lets a caller branch without string-matching it.
pub async fn handle_steam_suspend() -> String {
    let Some(sc) = sidecar() else {
        return json!({
            "status": ServiceStatus::Disabled.as_str(),
            "reason": "steam not configured",
        })
        .to_string();
    };
    match sc
        .post_json::<SleepResponse>("/sleep", None, MAX_SLEEP_BODY)
        .await
    {
        Ok(resp) => suspend_reply(&resp),
        Err(e) => {
            tracing::debug!("steam-suspend failed: {e}");
            json!({
                "status": ServiceStatus::Error.as_str(),
                "reason": format!("steam-suspend failed: {e}"),
                "refused": false,
            })
            .to_string()
        }
    }
}

/// Pure reply builder for [`handle_steam_suspend`] — maps the host's
/// [`SleepResponse`] onto the daemon's compact-JSON status shape. A refusal with
/// no `reason` (a host that answered but said nothing) still reports as refused,
/// with a generic message, rather than being mistaken for success.
fn suspend_reply(resp: &SleepResponse) -> String {
    if resp.ok {
        return json!({ "status": ServiceStatus::Ok.as_str() }).to_string();
    }
    let reason = resp
        .reason
        .as_deref()
        .unwrap_or("the host refused to suspend");
    json!({
        "status": ServiceStatus::Error.as_str(),
        "reason": reason,
        "refused": true,
    })
    .to_string()
}

/// IPC entry point for `steam-hosts`. Lists the configured tv-shell-host
/// sidecars and which one is active:
/// `{"status":"ok","active":<name>,"hosts":[{"name":…,"host":…}]}`, or
/// `{"status":"disabled","active":null,"hosts":[]}` when none are configured.
/// Each entry's `host` is the URL's name-part (IP/hostname) — for display and
/// as the widget's WoL target; the full URL (and token) never leave the daemon.
pub fn handle_steam_hosts() -> String {
    let hosts = crate::daemon_config::global().steam_hosts();
    let selected = crate::config::steam_server(&crate::config::settings_path());
    hosts_reply(hosts, selected.as_deref())
}

/// Pure reply builder for [`handle_steam_hosts`] — unit-tests on any host.
fn hosts_reply(hosts: Vec<SteamHostConfig>, selected: Option<&str>) -> String {
    let active = pick_host(hosts.clone(), selected).map(|h| h.name);
    let entries: Vec<Value> = hosts
        .iter()
        .map(|h| {
            json!({
                "name": h.name,
                "host": url_host(&h.url),
            })
        })
        .collect();
    let status = if entries.is_empty() {
        ServiceStatus::Disabled
    } else {
        ServiceStatus::Ok
    };
    json!({
        "status": status.as_str(),
        "active": active,
        "hosts": entries,
    })
    .to_string()
}

/// IPC entry point for `steam-set-host <name>`. Persists the sidecar selection
/// (settings.json `steamServer`) after validating it names a configured host;
/// the next `steam-library` poll and health probe use the new host. Blocking
/// file I/O — dispatch via `spawn_blocking` (see `ipc.rs`).
pub fn handle_steam_set_host(name: &str) -> String {
    let hosts = crate::daemon_config::global().steam_hosts();
    if !hosts.iter().any(|h| h.name == name) {
        return crate::protocol::resp_error(&format!("unknown steam host {name:?}"));
    }
    match crate::config::set_config(
        &crate::config::settings_path(),
        &json!({ "steamServer": name }),
    ) {
        Ok(_) => crate::protocol::resp_ok(),
        Err(e) => crate::protocol::resp_error(&format!("steam-set-host failed: {e}")),
    }
}

/// The `{appid}` POST body for `/launch` and `/quit`, built from the shared
/// [`tv_shell_protocol::LaunchRequest`] so the request shape is single-sourced
/// with the host (which deserializes the same type). Serializes to `{"appid":N}`.
fn launch_body(appid: u32) -> Value {
    serde_json::to_value(tv_shell_protocol::LaunchRequest { appid })
        .expect("LaunchRequest always serializes")
}

/// Normalize a [`LibraryResponse`] into the widget's two rails: `recentlyPlayed`
/// (sorted by `last_played` desc, top [`RECENT_LIMIT`]) and `allGames` (sorted by
/// name). `running_appid` (the host's foreground game, or `None`) is passed
/// through to the reply's `runningAppid` so the widget can badge the "Playing"
/// card; `streaming` (whether a stream is active on the host) is passed through to
/// `streaming`. `base` is the host base URL (already trailing-slash-trimmed by the
/// [`Sidecar`]) used to build each card's `localArt` fallback URL
/// (`{base}/art/{appid}`). Pure function — no I/O — so it unit-tests anywhere. An
/// empty library yields empty rails (but `status:ok`, matching an
/// installed-but-empty library).
pub fn normalize_library(
    resp: &LibraryResponse,
    base: &str,
    running_appid: Option<u32>,
    streaming: bool,
) -> String {
    let running = running_appid.map(Value::from).unwrap_or(Value::Null);

    // Project each entry to the widget shape and keep only installed games (a
    // partially-downloaded title isn't launchable yet). `card` returns None for a
    // zero appid (an unaddressable entry) so it's skipped rather than emitted as a
    // broken card.
    let mut cards: Vec<Value> = resp
        .games
        .iter()
        .filter(|e| e.installed)
        .filter_map(|e| card(e, base))
        .collect();

    // allGames: by name (case-insensitive).
    cards.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("").to_lowercase();
        let bn = b["name"].as_str().unwrap_or("").to_lowercase();
        an.cmp(&bn)
    });
    let all_games = cards.clone();

    // recentlyPlayed: by last_played desc, drop never-played, cap.
    let mut recent: Vec<Value> = cards
        .into_iter()
        .filter(|c| c["lastPlayed"].as_u64().unwrap_or(0) > 0)
        .collect();
    recent.sort_by(|a, b| {
        let al = a["lastPlayed"].as_u64().unwrap_or(0);
        let bl = b["lastPlayed"].as_u64().unwrap_or(0);
        bl.cmp(&al)
    });
    recent.truncate(RECENT_LIMIT);

    json!({
        "status": ServiceStatus::Ok.as_str(),
        "recentlyPlayed": recent,
        "allGames": all_games,
        "runningAppid": running,
        "streaming": streaming,
        // The host's name-part (from the base URL) — whom this library belongs
        // to. The widget shows/wakes THIS host, never a separately-configured one.
        "host": url_host(base),
    })
    .to_string()
}

/// Map one host [`LibraryEntry`] to the widget card shape `{appid,name,art,
/// localArt,headerArt,lastPlayed}`. The `art`/`headerArt` poster + header URLs are
/// built off the appid against Steam's public CDN (no token), and `localArt`
/// points at the host's own `/art/{appid}` endpoint (`{base}/art/{appid}`) which
/// serves the on-disk cache art for titles the CDN is missing — QML uses it as a
/// fallback between `art` and `headerArt`. All three bind to `Image.source`.
fn card(entry: &LibraryEntry, base: &str) -> Option<Value> {
    // A zero appid can't address Steam's CDN or our /art endpoint, so skip the
    // entry (filter_map drops the None) rather than emit a broken card pointing at
    // `.../apps/0/...`.
    let appid = entry.appid;
    if appid == 0 {
        return None;
    }
    Some(json!({
        "appid": appid,
        "name": entry.name,
        "art": library_art_url(appid),
        "localArt": local_art_url(base, appid),
        "headerArt": header_art_url(appid),
        "lastPlayed": entry.last_played,
    }))
}

/// Portrait library poster (600×900). Verified HTTP 200 on Steam's CDN.
fn library_art_url(appid: u32) -> String {
    format!("https://steamcdn-a.akamaihd.net/steam/apps/{appid}/library_600x900.jpg")
}

/// 16:9 header image — the QML fallback when the portrait poster 404s (older
/// titles may lack `library_600x900`).
fn header_art_url(appid: u32) -> String {
    format!("https://steamcdn-a.akamaihd.net/steam/apps/{appid}/header.jpg")
}

/// Local portrait art served by the host's public `/art/{appid}` endpoint. Used
/// when Steam's CDN lacks library art for a (usually newer) title — the art is
/// still cached on the gaming PC. `base` is already trailing-slash-trimmed.
fn local_art_url(base: &str, appid: u32) -> String {
    format!("{base}/art/{appid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host base URL for tests — matches the [`Sidecar`]'s already-trimmed shape.
    const BASE: &str = "http://host:47995";

    fn entry(appid: u32, name: &str, last_played: Option<u64>, installed: bool) -> LibraryEntry {
        LibraryEntry {
            appid,
            name: name.to_string(),
            last_played,
            size_on_disk: None,
            installed,
        }
    }

    fn resp(games: Vec<LibraryEntry>) -> LibraryResponse {
        LibraryResponse { games }
    }

    fn parse(out: &str) -> Value {
        serde_json::from_str(out).unwrap()
    }

    fn host_cfg(name: &str, url: &str) -> SteamHostConfig {
        SteamHostConfig {
            name: name.to_string(),
            url: url.to_string(),
            token_file: None,
            mac: None,
        }
    }

    #[test]
    fn pick_host_prefers_selected_name() {
        let hosts = vec![host_cfg("a", "http://a:1"), host_cfg("b", "http://b:1")];
        assert_eq!(pick_host(hosts.clone(), Some("b")).unwrap().name, "b");
        // Unknown/stale selection degrades to the first host.
        assert_eq!(pick_host(hosts.clone(), Some("gone")).unwrap().name, "a");
        assert_eq!(pick_host(hosts, None).unwrap().name, "a");
        assert!(pick_host(vec![], Some("a")).is_none());
    }

    #[test]
    fn hosts_reply_lists_names_and_active() {
        let hosts = vec![
            host_cfg("desktop-1", "http://192.0.2.1:47995"),
            host_cfg("desktop-2", "http://192.0.2.2:47995"),
        ];
        let out = parse(&hosts_reply(hosts, Some("desktop-2")));
        assert_eq!(out["status"], "ok");
        assert_eq!(out["active"], "desktop-2");
        let entries = out["hosts"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "desktop-1");
        assert_eq!(entries[0]["host"], "192.0.2.1");
        // URLs and tokens never leave the daemon.
        assert!(entries[0].get("url").is_none());
        assert!(entries[0].get("token_file").is_none());
    }

    #[test]
    fn suspend_reply_ok_when_host_accepted() {
        let out = parse(&suspend_reply(&SleepResponse {
            ok: true,
            reason: None,
        }));
        assert_eq!(out["status"], "ok");
        // The accepted shape matches steam-quit's exactly — no extra fields.
        assert!(out.get("reason").is_none());
        assert!(out.get("refused").is_none());
    }

    #[test]
    fn suspend_reply_surfaces_the_hosts_refusal_verbatim() {
        // The refusal reason is written for a person; it must reach the caller
        // unmangled, and `refused` must let QML branch without string-matching.
        let out = parse(&suspend_reply(&SleepResponse {
            ok: false,
            reason: Some("a game is running on the host".into()),
        }));
        assert_eq!(out["status"], "error");
        assert_eq!(out["reason"], "a game is running on the host");
        assert_eq!(out["refused"], true);
    }

    #[test]
    fn suspend_reply_refusal_without_a_reason_is_still_a_refusal() {
        // A host that answered `{"ok":false}` with no reason must NOT be read as
        // success; it degrades to a generic message, still flagged refused.
        let out = parse(&suspend_reply(&SleepResponse::default()));
        assert_eq!(out["status"], "error");
        assert_eq!(out["refused"], true);
        assert!(!out["reason"].as_str().unwrap().is_empty());
    }

    #[test]
    fn quit_reply_ok_when_the_host_signalled_the_game() {
        let out = parse(&quit_reply(&QuitResponse {
            ok: true,
            appid: 730,
            reason: None,
        }));
        assert_eq!(out["status"], "ok");
        // The accepted shape matches steam-suspend's exactly — no extra fields.
        assert!(out.get("reason").is_none());
        assert!(out.get("refused").is_none());
    }

    #[test]
    fn quit_reply_surfaces_not_running_instead_of_faking_success() {
        // The bug this closes: the host says "I quit nothing", HTTP 200, and the
        // daemon used to answer `{"status":"ok"}`. The refusal must reach the
        // caller with the host's reason verbatim and a `refused` flag QML can
        // branch on without string-matching.
        let out = parse(&quit_reply(&QuitResponse {
            ok: false,
            appid: 252950,
            reason: Some("not running".into()),
        }));
        assert_eq!(out["status"], "error");
        assert_eq!(out["reason"], "not running");
        assert_eq!(out["refused"], true);
    }

    #[test]
    fn quit_reply_refusal_without_a_reason_is_still_a_refusal() {
        // A host that answered `{"ok":false}` with no reason must NOT be read as
        // success; it degrades to a generic message, still flagged refused. This
        // is also what a `{}` body deserializes to (ok defaults false).
        let out = parse(&quit_reply(&QuitResponse::default()));
        assert_eq!(out["status"], "error");
        assert_eq!(out["refused"], true);
        assert!(!out["reason"].as_str().unwrap().is_empty());
    }

    #[test]
    fn hosts_reply_disabled_when_unconfigured() {
        let out = parse(&hosts_reply(vec![], None));
        assert_eq!(out["status"], "disabled");
        assert!(out["active"].is_null());
        assert_eq!(out["hosts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn library_replies_carry_host() {
        // Populated reply: host = the base URL's name-part.
        let out = parse(&normalize_library(&resp(vec![]), BASE, None, false));
        assert_eq!(out["host"], "host");
        // Empty (down/disabled) reply: host passes through — the Wake card needs
        // it exactly when the host is unreachable.
        let down = parse(&empty_library(
            ServiceStatus::Unreachable,
            Some("192.0.2.7".into()),
        ));
        assert_eq!(down["host"], "192.0.2.7");
        let disabled = parse(&empty_library(ServiceStatus::Disabled, None));
        assert!(disabled["host"].is_null());
    }

    #[test]
    fn empty_library_is_ok_empty() {
        let out = parse(&normalize_library(&resp(vec![]), BASE, None, false));
        assert_eq!(out["status"], "ok");
        assert_eq!(out["recentlyPlayed"].as_array().unwrap().len(), 0);
        assert_eq!(out["allGames"].as_array().unwrap().len(), 0);
        assert!(out["runningAppid"].is_null());
        assert_eq!(out["streaming"], false);
    }

    #[test]
    fn default_library_is_ok_empty() {
        // A default (no games) LibraryResponse — what a `{}` body deserializes to —
        // yields empty rails with status ok.
        let out = parse(&normalize_library(
            &LibraryResponse::default(),
            BASE,
            None,
            false,
        ));
        assert_eq!(out["status"], "ok");
        assert_eq!(out["allGames"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn running_appid_passed_through() {
        let out = parse(&normalize_library(
            &resp(vec![entry(730, "CS2", Some(0), true)]),
            BASE,
            Some(730),
            false,
        ));
        assert_eq!(out["runningAppid"], 730);
        // And passes through on the empty-games path too.
        let empty = parse(&normalize_library(
            &LibraryResponse::default(),
            BASE,
            Some(440),
            false,
        ));
        assert_eq!(empty["runningAppid"], 440);
    }

    #[test]
    fn streaming_passed_through() {
        // streaming flows through on both the populated and empty-games paths.
        let out = parse(&normalize_library(
            &resp(vec![entry(730, "CS2", Some(0), true)]),
            BASE,
            None,
            true,
        ));
        assert_eq!(out["streaming"], true);
        let empty = parse(&normalize_library(
            &LibraryResponse::default(),
            BASE,
            None,
            true,
        ));
        assert_eq!(empty["streaming"], true);
    }

    #[test]
    fn all_games_sorted_by_name_and_art_built() {
        let out = parse(&normalize_library(
            &resp(vec![
                entry(620, "Portal 2", Some(0), true),
                entry(400, "Portal", Some(0), true),
            ]),
            BASE,
            None,
            false,
        ));
        let all = out["allGames"].as_array().unwrap();
        assert_eq!(all.len(), 2);
        // Sorted: "Portal" before "Portal 2".
        assert_eq!(all[0]["name"], "Portal");
        assert_eq!(all[1]["name"], "Portal 2");
        // Art URLs built off appid.
        assert_eq!(
            all[0]["art"],
            "https://steamcdn-a.akamaihd.net/steam/apps/400/library_600x900.jpg"
        );
        assert_eq!(
            all[0]["headerArt"],
            "https://steamcdn-a.akamaihd.net/steam/apps/400/header.jpg"
        );
        // localArt points at the host's public /art/{appid} endpoint.
        assert_eq!(all[0]["localArt"], "http://host:47995/art/400");
        assert_eq!(all[1]["localArt"], "http://host:47995/art/620");
    }

    #[test]
    fn recently_played_sorted_desc_and_filtered() {
        let out = parse(&normalize_library(
            &resp(vec![
                entry(1, "Old", Some(100), true),
                entry(2, "Newest", Some(300), true),
                entry(3, "Never", Some(0), true),
                entry(4, "Mid", Some(200), true),
            ]),
            BASE,
            None,
            false,
        ));
        let recent = out["recentlyPlayed"].as_array().unwrap();
        // Never-played dropped; rest by last_played desc.
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0]["name"], "Newest");
        assert_eq!(recent[1]["name"], "Mid");
        assert_eq!(recent[2]["name"], "Old");
    }

    #[test]
    fn uninstalled_games_excluded() {
        let out = parse(&normalize_library(
            &resp(vec![
                entry(1, "Installed", Some(0), true),
                entry(2, "Downloading", Some(0), false),
            ]),
            BASE,
            None,
            false,
        ));
        let all = out["allGames"].as_array().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["name"], "Installed");
    }

    #[test]
    fn zero_appid_skipped() {
        // A zero appid is unaddressable (no CDN/`/art` URL), so it's dropped even
        // when installed.
        let out = parse(&normalize_library(
            &resp(vec![
                entry(0, "Bogus", Some(0), true),
                entry(10, "Real", Some(0), true),
            ]),
            BASE,
            None,
            false,
        ));
        let all = out["allGames"].as_array().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["name"], "Real");
    }

    #[test]
    fn recently_played_capped() {
        // appids start at 1 — appid 0 is unaddressable and is skipped, so use real
        // (non-zero) ids to exercise the cap with a full 30-game library.
        let many: Vec<LibraryEntry> = (1..31)
            .map(|i| entry(i, &format!("G{i}"), Some(1000 + i as u64), true))
            .collect();
        let out = parse(&normalize_library(&resp(many), BASE, None, false));
        assert_eq!(
            out["recentlyPlayed"].as_array().unwrap().len(),
            RECENT_LIMIT
        );
        // allGames is uncapped.
        assert_eq!(out["allGames"].as_array().unwrap().len(), 30);
    }
}

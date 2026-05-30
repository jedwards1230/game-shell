//! Network READ subsystem (Phase 3): a long-lived async actor owning a single
//! `zbus::Connection` to the system bus, talking to NetworkManager. It answers
//! request/response queries over an `mpsc` of [`NetReq`] and pushes `net:*`
//! [`Event`]s onto the shared broadcast bus.
//!
//! READ ONLY: connectivity/SSID/AP-list/primary state via zbus. Wi-Fi **join**
//! deliberately stays an `nmcli device wifi connect` shell-out in the QML
//! (NetworkManager's `AddAndActivateConnection` is a deeply nested variant map);
//! this module never activates connections.
//!
//! Linux-only (system D-Bus); `main.rs` declares it under
//! `#[cfg(target_os = "linux")]`. Single-owner discipline: the `run` loop owns
//! the `Connection` and any signal/property streams; nothing is held across an
//! `.await` behind a lock.

use crate::protocol::{self, Event};
use crate::state::Reply;
use anyhow::{Context, Result};
use futures::stream::StreamExt;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zbus::Connection;

// ---------------------------------------------------------------------------
// D-Bus proxy definitions (NetworkManager). Hand-written `#[zbus::proxy]`
// traits — only the properties/methods/signals we read are declared.
// ---------------------------------------------------------------------------

/// Root NetworkManager object: connectivity + the primary/active connections.
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// Overall connectivity: 1 none, 2 portal, 3 limited, 4 full (0 unknown).
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;

    /// Object path of the primary active connection (`/` when none).
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;

    /// All currently-active connection object paths.
    #[zbus(property)]
    fn active_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// All network device object paths.
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

/// A network device (per-device object path).
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    /// NM device type: 1 ethernet, 2 wifi, ... (see NMDeviceType).
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;

    /// Kernel interface name (e.g. `wlan0`).
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
}

/// The wireless facet of a Wi-Fi device.
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wireless {
    /// Currently-associated access point (`/` when none).
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;

    /// All access points seen in the most recent scan.
    #[zbus(property)]
    fn access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Request a fresh scan. `options` is an (empty) `a{sv}` map.
    fn request_scan(
        &self,
        options: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;
}

/// A single Wi-Fi access point.
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    /// SSID as raw bytes (decoded with UTF-8-lossy at the call site).
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    /// Signal strength 0..=100.
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;

    /// AP capability flags (NM80211ApFlags): bit 0 = privacy/WEP.
    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;

    /// WPA (RSN-less) security flags (NM80211ApSecurityFlags).
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;

    /// RSN (WPA2/WPA3) security flags (NM80211ApSecurityFlags).
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;
}

/// An active connection (used to render `{name,type,device}` for status).
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    /// Human-readable connection id (the "NAME" column in `nmcli`).
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    /// Connection type, e.g. `802-3-ethernet` / `802-11-wireless`.
    #[zbus(property)]
    fn type_(&self) -> zbus::Result<String>;

    /// Devices this connection is bound to.
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

// ---------------------------------------------------------------------------
// Requests / wire shapes.
// ---------------------------------------------------------------------------

/// Requests from the IPC server to the Network actor. Each carries a `oneshot`
/// reply with a fully-formatted wire string.
#[derive(Debug)]
pub enum NetReq {
    /// `net-status` -> compact JSON object
    /// `{connectivity,primaryType,hasWifi,ipv4,activeConnections}`.
    Status(Reply),
    /// `net-wifi-list` -> compact JSON array of `{ssid,signal,security,inUse}`.
    WifiList(Reply),
    /// `net-wifi-rescan` -> `ok` / `error:*` (NetworkManager `RequestScan`).
    WifiRescan(Reply),
}

/// Map NetworkManager `Connectivity` (u32) to the wire state word.
fn connectivity_word(c: u32) -> &'static str {
    match c {
        1 => "none",
        2 => "portal",
        3 => "limited",
        4 => "full",
        _ => "unknown",
    }
}

/// Map an AP's flag triple to a coarse security label the QML understands.
///
/// NM80211ApSecurityFlags bits used:
/// - `KEY_MGMT_PSK`   = 0x100 (WPA/WPA2 personal)
/// - `KEY_MGMT_802_1X`= 0x200 (enterprise)
/// - `KEY_MGMT_SAE`   = 0x400 (WPA3 personal)
/// - `KEY_MGMT_OWE`   = 0x800 / 0x1000 (enhanced open)
///
/// `rsn_flags` are WPA2/WPA3, `wpa_flags` are legacy WPA1; AP `flags` bit 0 is
/// WEP/privacy. We pick the strongest indicated scheme.
fn security_label(ap_flags: u32, wpa_flags: u32, rsn_flags: u32) -> &'static str {
    const KEY_MGMT_802_1X: u32 = 0x200;
    const KEY_MGMT_SAE: u32 = 0x400;
    const KEY_MGMT_OWE: u32 = 0x800;
    const KEY_MGMT_OWE_TM: u32 = 0x1000;
    const PRIVACY: u32 = 0x1; // NM80211ApFlags::PRIVACY

    if rsn_flags & KEY_MGMT_802_1X != 0 || wpa_flags & KEY_MGMT_802_1X != 0 {
        return "WPA-Enterprise";
    }
    if rsn_flags & KEY_MGMT_SAE != 0 {
        return "WPA3";
    }
    // OWE (enhanced open) before the generic `rsn_flags != 0` WPA2 fallthrough,
    // otherwise an OWE-only AP would be mislabelled "WPA2".
    if rsn_flags & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0 {
        return "OWE";
    }
    if rsn_flags != 0 {
        return "WPA2";
    }
    if wpa_flags != 0 {
        return "WPA";
    }
    if ap_flags & PRIVACY != 0 {
        return "WEP";
    }
    "Open"
}

// ---------------------------------------------------------------------------
// Actor entry point.
// ---------------------------------------------------------------------------

/// Run the Network actor until `rx` is closed.
///
/// Owns one `zbus::Connection`, services [`NetReq`]s against NetworkManager,
/// and pushes `net:connectivity` / `net:wifi` / `net:primary` events onto
/// `events_tx`. Never panics: if NetworkManager (or the system bus) is absent,
/// it still drains `rx`, replying `error:*` to each query, and skips the signal
/// streams.
pub async fn run(
    mut rx: mpsc::Receiver<NetReq>,
    events_tx: broadcast::Sender<Event>,
) -> Result<()> {
    let conn = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("network: system bus unavailable ({e}); replying error to all requests");
            while let Some(req) = rx.recv().await {
                let reply = reply_of(req);
                let _ = reply.send(protocol::resp_error("NetworkManager unavailable"));
            }
            return Ok(());
        }
    };

    // Spawn the property-change watcher on the same connection. It owns its own
    // proxy + streams (single-owner; no shared mutable state with the request
    // loop). If NM is missing the proxy build fails and the watcher just exits.
    let watch_conn = conn.clone();
    let watch_tx = events_tx.clone();
    let watcher = tokio::spawn(async move {
        if let Err(e) = watch_signals(watch_conn, watch_tx).await {
            tracing::warn!("network: signal watcher stopped: {e}");
        }
    });

    tracing::info!("network actor started (NetworkManager)");

    while let Some(req) = rx.recv().await {
        match req {
            NetReq::Status(reply) => {
                let _ = reply.send(status(&conn).await);
            }
            NetReq::WifiList(reply) => {
                let _ = reply.send(wifi_list(&conn).await);
            }
            NetReq::WifiRescan(reply) => {
                let _ = reply.send(wifi_rescan(&conn).await);
            }
        }
    }

    watcher.abort();
    tracing::info!("network actor stopped");
    Ok(())
}

/// Extract the `oneshot` reply from any request (used on the bus-absent path).
fn reply_of(req: NetReq) -> Reply {
    match req {
        NetReq::Status(r) | NetReq::WifiList(r) | NetReq::WifiRescan(r) => r,
    }
}

// ---------------------------------------------------------------------------
// Query handlers (each returns a ready-to-send wire string, never panics).
// ---------------------------------------------------------------------------

/// Build the `net-status` compact-JSON object. On any D-Bus failure, degrades
/// to a best-effort object with `connectivity:"unknown"` rather than erroring.
async fn status(conn: &Connection) -> String {
    match status_inner(conn).await {
        Ok(json) => json,
        Err(e) => {
            tracing::debug!("network: status query failed: {e}");
            // Degrade gracefully: an "unknown" object keeps the QML page usable.
            json!({
                "connectivity": "unknown",
                "primaryType": "",
                "hasWifi": false,
                "ipv4": "",
                "activeConnections": [],
            })
            .to_string()
        }
    }
}

async fn status_inner(conn: &Connection) -> Result<String> {
    let nm = NetworkManagerProxy::new(conn)
        .await
        .context("build NetworkManager proxy")?;

    let connectivity = connectivity_word(nm.connectivity().await.unwrap_or(0)).to_string();

    // Primary connection's type (e.g. 802-3-ethernet). `/` means none.
    let primary_path = nm
        .primary_connection()
        .await
        .unwrap_or_else(|_| empty_path());
    let primary_type = if is_real_path(&primary_path) {
        active_connection_type(conn, &primary_path)
            .await
            .unwrap_or_default()
    } else {
        String::new()
    };

    // hasWifi := any device whose DeviceType == 2 (wifi).
    let has_wifi = match nm.devices().await {
        Ok(paths) => any_wifi_device(conn, &paths).await,
        Err(_) => false,
    };

    // Active connections rendered as {name,type,device}. Best-effort per entry.
    let mut active = Vec::new();
    if let Ok(paths) = nm.active_connections().await {
        for p in paths {
            if let Ok(entry) = active_connection_entry(conn, &p).await {
                active.push(entry);
            }
        }
    }

    // IPv4 addresses: an `ip` shell-out is explicitly acceptable (only nmcli
    // *reads* must move to D-Bus). Simpler than walking IP4Config.AddressData.
    let ipv4 = read_ipv4().await;

    Ok(json!({
        "connectivity": connectivity,
        "primaryType": primary_type,
        "hasWifi": has_wifi,
        "ipv4": ipv4,
        "activeConnections": active,
    })
    .to_string())
}

/// Build the `net-wifi-list` compact-JSON array of `{ssid,signal,security,inUse}`,
/// deduped by SSID (keep strongest) and sorted by signal descending.
async fn wifi_list(conn: &Connection) -> String {
    match wifi_list_inner(conn).await {
        Ok(json) => json,
        Err(e) => {
            tracing::debug!("network: wifi-list query failed: {e}");
            "[]".to_string()
        }
    }
}

async fn wifi_list_inner(conn: &Connection) -> Result<String> {
    let nm = NetworkManagerProxy::new(conn)
        .await
        .context("build NetworkManager proxy")?;
    let device_paths = nm.devices().await.context("list devices")?;

    // Collect, dedupe by SSID keeping the strongest, then sort by signal desc.
    let mut best: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();

    for dev_path in device_paths {
        // Only Wi-Fi devices (DeviceType == 2).
        let Ok(dev) = device_proxy(conn, &dev_path).await else {
            continue;
        };
        if dev.device_type().await.unwrap_or(0) != 2 {
            continue;
        }

        let Ok(wireless) = wireless_proxy(conn, &dev_path).await else {
            continue;
        };
        let active_ap = wireless
            .active_access_point()
            .await
            .unwrap_or_else(|_| empty_path());
        let ap_paths = match wireless.access_points().await {
            Ok(p) => p,
            Err(_) => continue,
        };

        for ap_path in ap_paths {
            let Ok(ap) = access_point_proxy(conn, &ap_path).await else {
                continue;
            };
            let ssid_bytes = ap.ssid().await.unwrap_or_default();
            if ssid_bytes.is_empty() {
                continue; // hidden / broadcast-suppressed SSID
            }
            let ssid = String::from_utf8_lossy(&ssid_bytes).into_owned();
            let signal = ap.strength().await.unwrap_or(0) as i64;
            let security = security_label(
                ap.flags().await.unwrap_or(0),
                ap.wpa_flags().await.unwrap_or(0),
                ap.rsn_flags().await.unwrap_or(0),
            );
            let in_use = is_real_path(&active_ap) && ap_path.as_str() == active_ap.as_str();

            let entry = json!({
                "ssid": ssid,
                "signal": signal,
                "security": security,
                "inUse": in_use,
            });

            // Dedup by SSID. The active (in-use) AP always wins so the UI keeps
            // its `inUse:true` marker even if a same-SSID AP has a stronger
            // signal; among non-active APs the stronger signal wins.
            let replace = match best.get(&ssid) {
                None => true,
                Some(existing) => {
                    let existing_in_use = existing
                        .get("inUse")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let existing_signal =
                        existing.get("signal").and_then(|v| v.as_i64()).unwrap_or(0);
                    if existing_in_use {
                        false // never overwrite the active AP
                    } else if in_use {
                        true // a non-active entry yields to the active one
                    } else {
                        signal > existing_signal // both non-active: stronger wins
                    }
                }
            };
            if replace {
                best.insert(ssid, entry);
            }
        }
    }

    let mut list: Vec<serde_json::Value> = best.into_values().collect();
    list.sort_by(|a, b| {
        let sa = a.get("signal").and_then(|v| v.as_i64()).unwrap_or(0);
        let sb = b.get("signal").and_then(|v| v.as_i64()).unwrap_or(0);
        sb.cmp(&sa)
    });

    Ok(serde_json::Value::Array(list).to_string())
}

/// `net-wifi-rescan`: ask every Wi-Fi device to rescan. `ok` if at least one
/// scan was requested, else `error:*`.
async fn wifi_rescan(conn: &Connection) -> String {
    match wifi_rescan_inner(conn).await {
        Ok(true) => protocol::resp_ok(),
        Ok(false) => protocol::resp_error("no wifi device"),
        Err(e) => {
            tracing::debug!("network: rescan failed: {e}");
            protocol::resp_error("rescan failed")
        }
    }
}

async fn wifi_rescan_inner(conn: &Connection) -> Result<bool> {
    let nm = NetworkManagerProxy::new(conn)
        .await
        .context("build NetworkManager proxy")?;
    let device_paths = nm.devices().await.context("list devices")?;

    let mut requested = false;
    for dev_path in device_paths {
        let Ok(dev) = device_proxy(conn, &dev_path).await else {
            continue;
        };
        if dev.device_type().await.unwrap_or(0) != 2 {
            continue;
        }
        let Ok(wireless) = wireless_proxy(conn, &dev_path).await else {
            continue;
        };
        // RequestScan throws if a scan is already running; that's fine — treat
        // it as "requested" so the UI doesn't surface a spurious error.
        match wireless
            .request_scan(std::collections::HashMap::new())
            .await
        {
            Ok(()) => requested = true,
            Err(e) => {
                tracing::debug!("network: RequestScan on {} failed: {e}", dev_path.as_str());
                requested = true; // already-scanning / rate-limited: still "ok"
            }
        }
    }
    Ok(requested)
}

// ---------------------------------------------------------------------------
// Signal watcher: connectivity + primary-connection changes.
// ---------------------------------------------------------------------------

/// Watch NetworkManager property-changed signals and fan `net:*` events onto
/// the broadcast bus. Owns its own proxy and the two property streams.
async fn watch_signals(conn: Connection, events_tx: broadcast::Sender<Event>) -> Result<()> {
    let nm = NetworkManagerProxy::new(&conn)
        .await
        .context("build NetworkManager proxy for signals")?;

    let mut conn_changes = nm.receive_connectivity_changed().await;
    let mut primary_changes = nm.receive_primary_connection_changed().await;

    loop {
        tokio::select! {
            Some(change) = conn_changes.next() => {
                if let Ok(value) = change.get().await {
                    let word = connectivity_word(value).to_string();
                    let _ = events_tx.send(Event::NetConnectivity(word));
                    // A connectivity change usually means the Wi-Fi/primary
                    // picture moved; emit a fresh net:wifi snapshot too.
                    let snapshot = status(&conn).await;
                    let _ = events_tx.send(Event::NetWifi(snapshot));
                }
            }
            Some(change) = primary_changes.next() => {
                if let Ok(path) = change.get().await {
                    let id = if is_real_path(&path) {
                        active_connection_id(&conn, &path).await.unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let _ = events_tx.send(Event::NetPrimary(id));
                }
            }
            else => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Proxy builders / small helpers (each scoped to one object path).
// ---------------------------------------------------------------------------

// Each proxy is built with an owned `ObjectPath<'static>` (cloned from the
// supplied `OwnedObjectPath`, which converts to `ObjectPath<'static>`), so the
// returned proxy borrows nothing from the caller's path.
async fn device_proxy(conn: &Connection, path: &OwnedObjectPath) -> Result<DeviceProxy<'static>> {
    DeviceProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await
        .map_err(Into::into)
}

async fn wireless_proxy(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Result<WirelessProxy<'static>> {
    WirelessProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await
        .map_err(Into::into)
}

async fn access_point_proxy(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Result<AccessPointProxy<'static>> {
    AccessPointProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await
        .map_err(Into::into)
}

async fn active_connection_proxy(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Result<ActiveConnectionProxy<'static>> {
    ActiveConnectionProxy::builder(conn)
        .path(path.clone())?
        .build()
        .await
        .map_err(Into::into)
}

/// Type string (e.g. `802-3-ethernet`) of an active-connection object path.
async fn active_connection_type(conn: &Connection, path: &OwnedObjectPath) -> Result<String> {
    let ac = active_connection_proxy(conn, path).await?;
    Ok(ac.type_().await.unwrap_or_default())
}

/// Human-readable id of an active-connection object path.
async fn active_connection_id(conn: &Connection, path: &OwnedObjectPath) -> Result<String> {
    let ac = active_connection_proxy(conn, path).await?;
    Ok(ac.id().await.unwrap_or_default())
}

/// `{name,type,device}` for an active connection (device = first iface name).
async fn active_connection_entry(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Result<serde_json::Value> {
    let ac = active_connection_proxy(conn, path).await?;
    let name = ac.id().await.unwrap_or_default();
    let ty = ac.type_().await.unwrap_or_default();
    let device = match ac.devices().await {
        Ok(devs) => match devs.first() {
            Some(dp) => match device_proxy(conn, dp).await {
                Ok(d) => d.interface().await.unwrap_or_default(),
                Err(_) => String::new(),
            },
            None => String::new(),
        },
        Err(_) => String::new(),
    };
    Ok(json!({ "name": name, "type": ty, "device": device }))
}

/// True if any device path is a Wi-Fi device (DeviceType == 2).
async fn any_wifi_device(conn: &Connection, paths: &[OwnedObjectPath]) -> bool {
    for p in paths {
        if let Ok(dev) = device_proxy(conn, p).await {
            if dev.device_type().await.unwrap_or(0) == 2 {
                return true;
            }
        }
    }
    false
}

/// Read non-loopback IPv4 addresses via the `ip` tool (an `ip` shell-out for
/// IPs is explicitly acceptable; only `nmcli` *reads* must go through D-Bus).
async fn read_ipv4() -> String {
    let output = tokio::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
        .await;
    let stdout = match output {
        Ok(o) if o.status.success() => o.stdout,
        _ => return String::new(),
    };
    let text = String::from_utf8_lossy(&stdout);
    let mut lines = Vec::new();
    for line in text.lines() {
        // `<n>: <iface> inet <addr>/<pfx> ...`
        let mut fields = line.split_whitespace();
        let iface = fields.nth(1).unwrap_or("");
        // skip to the token after `inet`
        let mut addr = "";
        let mut prev = "";
        for tok in line.split_whitespace() {
            if prev == "inet" {
                addr = tok;
                break;
            }
            prev = tok;
        }
        if iface.is_empty() || addr.is_empty() || addr.starts_with("127.") {
            continue;
        }
        let ip = addr.split('/').next().unwrap_or(addr);
        lines.push(format!("{iface}: {ip}"));
        if lines.len() >= 3 {
            break;
        }
    }
    lines.join("\n")
}

/// An empty/`/` object path (NetworkManager's "no object" sentinel).
/// `ObjectPath::default()` is the root path `/`.
fn empty_path() -> OwnedObjectPath {
    ObjectPath::default().into()
}

/// True unless the path is the `/` "no object" sentinel.
fn is_real_path(path: &OwnedObjectPath) -> bool {
    path.as_str() != "/"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectivity_words() {
        assert_eq!(connectivity_word(0), "unknown");
        assert_eq!(connectivity_word(1), "none");
        assert_eq!(connectivity_word(2), "portal");
        assert_eq!(connectivity_word(3), "limited");
        assert_eq!(connectivity_word(4), "full");
        assert_eq!(connectivity_word(99), "unknown");
    }

    #[test]
    fn security_labels() {
        // Open: no flags.
        assert_eq!(security_label(0, 0, 0), "Open");
        // WEP: privacy bit only.
        assert_eq!(security_label(0x1, 0, 0), "WEP");
        // WPA2: any rsn_flags (PSK = 0x100).
        assert_eq!(security_label(0x1, 0, 0x100), "WPA2");
        // WPA3: rsn SAE.
        assert_eq!(security_label(0x1, 0, 0x400), "WPA3");
        // WPA1: wpa_flags only.
        assert_eq!(security_label(0x1, 0x100, 0), "WPA");
        // Enterprise overrides personal.
        assert_eq!(security_label(0x1, 0, 0x200), "WPA-Enterprise");
        assert_eq!(security_label(0x1, 0x200, 0), "WPA-Enterprise");
    }
}

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
use std::collections::HashMap;
use tokio::sync::{broadcast, mpsc};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
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

/// The wired facet of an Ethernet device — Speed in Mb/s (0 when unknown/not linked).
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wired",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wired {
    /// Link speed in Mb/s; 0 when the link is down or speed is unknown.
    #[zbus(property)]
    fn speed(&self) -> zbus::Result<u32>;
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

    /// Object path of the IP4Config for this active connection.
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
}

/// IPv4 configuration object — gateway and nameservers.
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip4Config {
    /// Gateway IP address string, `""` when none.
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;

    /// Structured nameserver data: each entry is `{address: <ip-string>, ...}`.
    /// Present on NM >= 1.6; older NM may not expose this property.
    #[zbus(property)]
    fn nameserver_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;

    /// Legacy nameserver list as packed big-endian u32 values.
    /// Fallback when `NameserverData` is empty or unavailable.
    #[zbus(property)]
    fn nameservers(&self) -> zbus::Result<Vec<u32>>;
}

// ---------------------------------------------------------------------------
// Requests / wire shapes.
// ---------------------------------------------------------------------------

/// Requests from the IPC server to the Network actor. Each carries a `oneshot`
/// reply with a fully-formatted wire string.
#[derive(Debug)]
pub enum NetReq {
    /// `net-status` -> compact JSON object
    /// `{connectivity,primaryType,hasWifi,ipv4,gateway,dns,activeConnections}`.
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

/// Decode a big-endian packed u32 nameserver (legacy NM `Nameservers` property)
/// into a dotted-quad IPv4 string.
fn decode_nameserver_u32(raw: u32) -> String {
    let b = raw.to_be_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
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
                "gateway": "",
                "dns": [],
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

    // gateway + DNS: read from the primary connection's IP4Config. Best-effort.
    let (gateway, dns) = if is_real_path(&primary_path) {
        read_gateway_dns(conn, &primary_path).await
    } else {
        (String::new(), Vec::new())
    };

    // Active connections rendered as {name,type,device,speed}. Best-effort per entry.
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
        "gateway": gateway,
        "dns": dns,
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

async fn wired_proxy(conn: &Connection, path: &OwnedObjectPath) -> Result<WiredProxy<'static>> {
    WiredProxy::builder(conn)
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

async fn ip4_config_proxy(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Result<Ip4ConfigProxy<'static>> {
    Ip4ConfigProxy::builder(conn)
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

/// `{name,type,device,speed}` for an active connection.
/// `speed` is Mb/s from the Device.Wired interface; 0 for non-wired or unknown.
async fn active_connection_entry(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Result<serde_json::Value> {
    let ac = active_connection_proxy(conn, path).await?;
    let name = ac.id().await.unwrap_or_default();
    let ty = ac.type_().await.unwrap_or_default();
    let (device, speed) = match ac.devices().await {
        Ok(devs) => match devs.first() {
            Some(dp) => {
                // One device proxy serves both reads: the interface name, and the
                // device type that gates the Wired facet below.
                let (iface, is_ethernet) = match device_proxy(conn, dp).await {
                    Ok(d) => (
                        d.interface().await.unwrap_or_default(),
                        // DeviceType == 1 is Ethernet (same NMDeviceType literal
                        // the Wi-Fi paths above compare against 2).
                        d.device_type().await.unwrap_or(0) == 1,
                    ),
                    Err(_) => (String::new(), false),
                };
                // Speed lives on `Device.Wired`, which exists ONLY on an Ethernet
                // device. Building the proxy unconditionally made zbus attempt a
                // `GetAll` on that interface for EVERY active connection — and
                // `lo` (loopback) is an active connection on a normal wired host,
                // so every poll logged
                //   Failed to populate properties cache via GetAll: ...
                //   No such interface "org.freedesktop.NetworkManager.Device.Wired".
                //   Property change streams will not produce values.
                // at warn level (176 hits in 90 minutes on the reference box).
                // The read itself was already safe — `unwrap_or(0)` swallowed the
                // error — so this only removes the noise and a pointless D-Bus
                // round trip; a real Ethernet device still reports its speed.
                let speed = if is_ethernet {
                    match wired_proxy(conn, dp).await {
                        Ok(w) => w.speed().await.unwrap_or(0),
                        Err(_) => 0,
                    }
                } else {
                    0
                };
                (iface, speed)
            }
            None => (String::new(), 0u32),
        },
        Err(_) => (String::new(), 0u32),
    };
    Ok(json!({ "name": name, "type": ty, "device": device, "speed": speed }))
}

/// Read gateway and DNS from the primary active connection's IP4Config.
/// Returns `("", [])` on any failure (best-effort, never errors).
async fn read_gateway_dns(
    conn: &Connection,
    primary_path: &OwnedObjectPath,
) -> (String, Vec<String>) {
    let ac = match active_connection_proxy(conn, primary_path).await {
        Ok(p) => p,
        Err(_) => return (String::new(), Vec::new()),
    };

    let ip4_path = match ac.ip4_config().await {
        Ok(p) => p,
        Err(_) => return (String::new(), Vec::new()),
    };

    if !is_real_path(&ip4_path) {
        return (String::new(), Vec::new());
    }

    let ip4 = match ip4_config_proxy(conn, &ip4_path).await {
        Ok(p) => p,
        Err(_) => return (String::new(), Vec::new()),
    };

    let gateway = ip4.gateway().await.unwrap_or_default();

    // Prefer structured NameserverData (NM >= 1.6); fall back to legacy u32 list.
    let dns = {
        let mut addrs = Vec::new();
        match ip4.nameserver_data().await {
            Ok(entries) if !entries.is_empty() => {
                for entry in entries {
                    if let Some(addr_val) = entry.get("address") {
                        if let Ok(s) = <&str>::try_from(addr_val) {
                            addrs.push(s.to_string());
                        }
                    }
                }
            }
            _ => {
                // Fall back to legacy Nameservers (Vec<u32>, big-endian).
                if let Ok(raw_list) = ip4.nameservers().await {
                    for raw in raw_list {
                        addrs.push(decode_nameserver_u32(raw));
                    }
                }
            }
        }
        addrs
    };

    (gateway, dns)
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

    #[test]
    fn decode_nameserver_u32_standard() {
        // 8.8.8.8 big-endian = 0x08080808
        assert_eq!(decode_nameserver_u32(0x08080808), "8.8.8.8");
        // 1.1.1.1 big-endian = 0x01010101
        assert_eq!(decode_nameserver_u32(0x01010101), "1.1.1.1");
        // 192.0.2.1 big-endian = 0xC0000201 (RFC 5737 documentation address)
        assert_eq!(decode_nameserver_u32(0xC0000201), "192.0.2.1");
        // 0.0.0.0
        assert_eq!(decode_nameserver_u32(0x00000000), "0.0.0.0");
        // 255.255.255.255
        assert_eq!(decode_nameserver_u32(0xFFFFFFFF), "255.255.255.255");
    }
}

//! Wake-on-LAN: send a magic packet to a streaming host.
//!
//! The home-screen Steam row replaces its posters with a single "Wake <host>"
//! card when the streaming host is unreachable; activating it sends `wol <host>`
//! over the IPC socket, which lands here. We then fire a standard Wake-on-LAN
//! magic packet (6×0xFF + 16× the host's MAC) as a UDP broadcast on port 9.
//!
//! The hard part is resolving the host's MAC *while it is asleep*: the kernel
//! ARP/neighbor entry goes STALE and may be evicted entirely once the host stops
//! answering. Three sources are consulted, in this order (see [`pick_mac`]):
//!
//!   1. **A statically configured MAC** — `[[steam.hosts]] mac` in `config.toml`.
//!      Authoritative, needs no discovery at all, and is the only source that
//!      survives "the host has been asleep for days AND the cache is cold" — the
//!      exact hole that made a wake impossible before.
//!   2. **The live neighbor table** (`ip neigh show`) — the host is normally
//!      online, and thus present in that table, shortly before it sleeps, so this
//!      keeps the cache warm for the wake that comes *after*. A hit is persisted.
//!   3. **The persisted cache** — the learned `host → MAC` mapping, stored beside
//!      `settings.json` (`~/.config/tv-shell/host-macs.json`, NOT inside the
//!      user-authored config).
//!
//! Sources 2 and 3 are the pre-existing behavior and are unchanged when no `mac`
//! is configured.
//!
//! Waking is normally **reactive** (the QML `WakeCard` sends `wol <host>`), but
//! `[steam].wake_active_host_on_start` (default **off**) additionally fires one
//! packet at the ACTIVE host — and only that one — at daemon startup and on a
//! `steam-set-host` switch. See [`wake_active_host_if_enabled`].
//!
//! Cross-platform: the magic-packet build + the `ip neigh` parse are pure
//! functions unit-tested on every platform; only the live `ip neigh` shell-out
//! and the UDP send touch the system (and degrade gracefully off-Linux / when
//! `ip` is absent).

use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;

/// A parsed Ethernet MAC address (6 octets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    /// Parse a colon- or dash-separated MAC (`aa:bb:cc:dd:ee:ff`). Returns `None`
    /// for the wrong octet count or a non-hex octet. Case-insensitive.
    pub fn parse(s: &str) -> Option<Mac> {
        let mut octets = [0u8; 6];
        let mut count = 0;
        for part in s.split([':', '-']) {
            if count >= 6 {
                return None; // too many octets
            }
            octets[count] = u8::from_str_radix(part.trim(), 16).ok()?;
            count += 1;
        }
        if count == 6 {
            Some(Mac(octets))
        } else {
            None
        }
    }

    /// Canonical lowercase colon-separated rendering (`aa:bb:cc:dd:ee:ff`).
    pub fn to_canonical(self) -> String {
        let b = self.0;
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

/// Build a standard Wake-on-LAN magic packet: 6 bytes of 0xFF followed by the
/// target MAC repeated 16 times — 102 bytes total. Pure; unit-tested.
pub fn magic_packet(mac: Mac) -> Vec<u8> {
    let mut packet = Vec::with_capacity(102);
    packet.extend_from_slice(&[0xFF; 6]);
    for _ in 0..16 {
        packet.extend_from_slice(&mac.0);
    }
    packet
}

/// Parse `ip neigh show` output, returning a `host-ip → MAC` map for every line
/// that carries a resolvable `lladdr`. Lines without an `lladdr` (FAILED /
/// INCOMPLETE entries) are skipped. Pure; unit-tested.
///
/// A neighbor line looks like:
/// `192.0.2.10 dev eth0 lladdr aa:bb:cc:dd:ee:ff REACHABLE`
/// (or `... STALE`, `... DELAY`, etc.). The first token is the peer IP.
pub fn parse_ip_neigh(output: &str) -> HashMap<String, Mac> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let mut toks = line.split_whitespace();
        let Some(ip) = toks.next() else {
            continue;
        };
        // Scan the remaining tokens for `lladdr <mac>`.
        let mut mac = None;
        let rest: Vec<&str> = toks.collect();
        for w in rest.windows(2) {
            if w[0] == "lladdr" {
                mac = Mac::parse(w[1]);
                break;
            }
        }
        if let Some(mac) = mac {
            map.insert(ip.to_string(), mac);
        }
    }
    map
}

/// Path to the learned `host → MAC` cache: a sibling of `settings.json`
/// (`~/.config/tv-shell/host-macs.json`), NOT inside the user-authored config
/// so we never clobber hand-edited settings.
fn mac_cache_path() -> PathBuf {
    let mut p = crate::config::settings_path();
    p.set_file_name("host-macs.json");
    p
}

/// Load the persisted `host → MAC-string` cache. A missing/corrupt file is an
/// empty map (best-effort).
fn load_cache() -> HashMap<String, String> {
    let path = mac_cache_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Persist the `host → MAC-string` cache (best-effort; errors are logged, not
/// fatal — a failed cache write just means the next wake re-learns from `ip
/// neigh`).
fn save_cache(cache: &HashMap<String, String>) {
    let path = mac_cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string(cache) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                tracing::debug!("wol: failed to write MAC cache {}: {e}", path.display());
            }
        }
        Err(e) => tracing::debug!("wol: failed to serialize MAC cache: {e}"),
    }
}

/// Resolve `host` to an IPv4 string. If `host` already parses as an IP it's
/// returned verbatim; otherwise the std resolver is consulted and the first IPv4
/// is taken. Returns `None` when the host can't be resolved to an IPv4.
fn resolve_ipv4(host: &str) -> Option<String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(_) => Some(host.to_string()),
            // An IPv6 literal can't be matched against the IPv4 neighbor table.
            IpAddr::V6(_) => None,
        };
    }
    // Hostname: resolve via the std resolver. Append a dummy port so
    // `to_socket_addrs` works, then take the first IPv4.
    let addrs = (host, 0u16).to_socket_addrs().ok()?;
    for addr in addrs {
        if let IpAddr::V4(v4) = addr.ip() {
            return Some(v4.to_string());
        }
    }
    None
}

/// Run `ip neigh show` and return its stdout, or `None` if the command is
/// unavailable / fails. Isolated so the rest of the resolution logic stays pure.
fn ip_neigh_output() -> Option<String> {
    let out = std::process::Command::new("ip")
        .args(["neigh", "show"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// The MAC-source precedence rule, as a pure function: **configured → neighbor
/// table → cache**.
///
/// `neighbor` and `cached` are taken lazily so a configured MAC costs neither the
/// `ip neigh` shell-out nor the cache file read — the configured value is
/// authoritative, there is nothing a live lookup could add.
///
/// An unparseable `configured` string falls through to discovery rather than
/// failing the wake: [`crate::daemon_config::DaemonConfig::validate`] already
/// rejects those at startup, so reaching here with one means the daemon was
/// started some other way, and degrading to the old behavior beats refusing.
///
/// Pure with respect to its inputs — unit-tested, including that a configured MAC
/// short-circuits both fallbacks.
fn pick_mac(
    configured: Option<&str>,
    neighbor: impl FnOnce() -> Option<Mac>,
    cached: impl FnOnce() -> Option<Mac>,
) -> Option<Mac> {
    configured
        .and_then(Mac::parse)
        .or_else(neighbor)
        .or_else(cached)
}

/// Resolve `host` → MAC via [`pick_mac`]'s precedence. `host` is the original
/// IP/hostname the shell passed; `ipv4` is its resolved IPv4 string (the key used
/// to match the neighbor table); `configured` is the `[[steam.hosts]] mac` pinned
/// for this host, if any. Returns `None` when no source has a MAC.
fn resolve_mac(host: &str, ipv4: &str, configured: Option<&str>) -> Option<Mac> {
    pick_mac(
        configured,
        || neighbor_mac(host, ipv4),
        || cached_mac(host, ipv4),
    )
}

/// Live neighbor-table lookup, warming the persisted cache on a hit. The host is
/// normally online (and thus in the neighbor table) shortly before it goes to
/// sleep, so this keeps the cache warm for the wake that happens *after*.
fn neighbor_mac(host: &str, ipv4: &str) -> Option<Mac> {
    let output = ip_neigh_output()?;
    let mac = parse_ip_neigh(&output).get(ipv4).copied()?;
    // Warm the cache under both the resolved IPv4 and the original host string,
    // so a later wake keyed by either resolves.
    let mut cache = load_cache();
    cache.insert(ipv4.to_string(), mac.to_canonical());
    cache.insert(host.to_string(), mac.to_canonical());
    save_cache(&cache);
    Some(mac)
}

/// Persisted-cache fallback (the host may already be asleep / evicted from the
/// neighbor table). Keyed by the resolved IPv4 first, then the original host
/// string — both are written on a neighbor hit.
fn cached_mac(host: &str, ipv4: &str) -> Option<Mac> {
    let cache = load_cache();
    cache
        .get(ipv4)
        .or_else(|| cache.get(host))
        .and_then(|s| Mac::parse(s))
}

/// The statically configured Wake-on-LAN MAC for `host`, matched against the
/// `[[steam.hosts]]` roster by the host-part of each entry's URL (the same
/// name-part the shell shows and passes to `wol <host>`). `None` when no entry
/// matches or the matching entry pins no MAC.
///
/// Pure over its inputs — unit-tested without touching the global config.
fn configured_mac_for<'a>(
    hosts: &'a [crate::daemon_config::SteamHostConfig],
    host: &str,
) -> Option<&'a str> {
    hosts
        .iter()
        .find(|h| crate::sidecar::url_host(&h.url).as_deref() == Some(host))
        .and_then(|h| h.mac.as_deref())
}

/// Send a magic packet for `mac` as a UDP broadcast on port 9. Broadcasts to the
/// global broadcast address `255.255.255.255` (and is best-effort — the OS routes
/// it onto the LAN). Returns `Ok(())` on a successful send.
fn send_magic_packet(mac: Mac) -> std::io::Result<()> {
    let packet = magic_packet(mac);
    // Bind to any local IPv4 address/port for the outbound broadcast.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let dest = SocketAddr::from((Ipv4Addr::BROADCAST, 9));
    socket.send_to(&packet, dest)?;
    Ok(())
}

/// JSON success reply for a sent wake.
fn ok_json(mac: Mac) -> String {
    json!({"status": "ok", "mac": mac.to_canonical()}).to_string()
}

/// JSON error reply with a short machine-readable `reason`.
fn err_json(reason: &str) -> String {
    json!({"status": "error", "reason": reason}).to_string()
}

/// IPC entry point for `wol <host>`. Resolves the host's MAC (configured →
/// neighbor table → cache), then fires the magic packet. Returns a compact-JSON
/// reply: `{"status":"ok","mac":"…"}` on success, or
/// `{"status":"error","reason":"…"}` (`no-host`, `no-ip`, `no-mac`, or
/// `send-failed`) on failure.
pub async fn handle_wol(host: &str) -> String {
    // Defensive: an empty host shouldn't reach here (the parser routes those to
    // `WolUsage`), but guard anyway.
    if host.is_empty() {
        return err_json("no-host");
    }
    // Read the pinned MAC (if any) from the typed config before crossing the
    // blocking boundary — it's a cheap in-memory lookup on the global config.
    let configured =
        configured_mac_for(&crate::daemon_config::global().steam_hosts(), host).map(str::to_string);
    // The resolution + UDP send are blocking syscalls; run them off the reactor.
    let host = host.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let Some(ipv4) = resolve_ipv4(&host) else {
            return err_json("no-ip");
        };
        let Some(mac) = resolve_mac(&host, &ipv4, configured.as_deref()) else {
            return err_json("no-mac");
        };
        match send_magic_packet(mac) {
            Ok(()) => ok_json(mac),
            Err(e) => {
                tracing::debug!("wol: send failed for {host}: {e}");
                err_json("send-failed")
            }
        }
    })
    .await;
    result.unwrap_or_else(|e| {
        tracing::debug!("wol: join error: {e}");
        err_json("send-failed")
    })
}

/// Proactive Wake-on-LAN for the **active** Steam host, gated on
/// `[steam].wake_active_host_on_start` (default off).
///
/// Called at daemon startup and after a successful `steam-set-host` — the two
/// moments where the shell is about to start polling a host it may have just
/// selected, and where finding it asleep costs the user a manual Wake press.
///
/// Deliberately targets ONLY the active host resolved by
/// [`crate::steam::active_host`], never the whole `[[steam.hosts]]` roster:
/// broadcasting at every configured machine would wake boxes nobody asked for
/// (including, on a dual-boot host, the OS that isn't selected).
///
/// **Fail-soft by construction**: it reuses [`handle_wol`], which never panics
/// and always returns a status string, and the caller spawns it fire-and-forget —
/// so a disabled flag, an unconfigured host, an unresolvable MAC, or a failed
/// send is at most a log line. It can never block startup or an IPC reply.
///
/// `trigger` is a short label for the log line (`"startup"` / `"steam-set-host"`).
pub async fn wake_active_host_if_enabled(trigger: &'static str) {
    if !crate::daemon_config::global()
        .steam
        .wake_active_host_on_start
    {
        return;
    }
    let Some(host) = crate::steam::active_host() else {
        tracing::debug!("wol: proactive wake ({trigger}) skipped — no active steam host");
        return;
    };
    let Some(name) = crate::sidecar::url_host(&host.url) else {
        tracing::debug!(
            "wol: proactive wake ({trigger}) skipped — steam host {:?} has no host-part in its url",
            host.name
        );
        return;
    };
    let reply = handle_wol(&name).await;
    tracing::info!("wol: proactive wake ({trigger}) for {name}: {reply}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mac_colon_and_dash() {
        assert_eq!(
            Mac::parse("aa:bb:cc:dd:ee:ff"),
            Some(Mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]))
        );
        assert_eq!(
            Mac::parse("AA-BB-CC-DD-EE-FF"),
            Some(Mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]))
        );
        // Mixed case + leading zeros.
        assert_eq!(
            Mac::parse("00:1A:2b:3C:4d:5E"),
            Some(Mac([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]))
        );
    }

    #[test]
    fn rejects_malformed_mac() {
        assert_eq!(Mac::parse(""), None);
        assert_eq!(Mac::parse("aa:bb:cc:dd:ee"), None); // too few
        assert_eq!(Mac::parse("aa:bb:cc:dd:ee:ff:00"), None); // too many
        assert_eq!(Mac::parse("zz:bb:cc:dd:ee:ff"), None); // non-hex
    }

    #[test]
    fn mac_to_canonical_is_lowercase_colon() {
        assert_eq!(
            Mac([0xaa, 0xbb, 0xcc, 0x00, 0x0e, 0xff]).to_canonical(),
            "aa:bb:cc:00:0e:ff"
        );
    }

    #[test]
    fn magic_packet_is_102_bytes_with_header_and_repetitions() {
        let mac = Mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        // First 6 bytes are the 0xFF sync header.
        assert_eq!(&packet[0..6], &[0xFF; 6]);
        // Followed by 16 copies of the MAC.
        for i in 0..16 {
            let start = 6 + i * 6;
            assert_eq!(&packet[start..start + 6], &mac.0, "repetition {i}");
        }
    }

    #[test]
    fn parses_ip_neigh_matching_host_to_mac() {
        let output = "\
192.0.2.10 dev eth0 lladdr aa:bb:cc:dd:ee:ff REACHABLE
192.0.2.20 dev eth0 lladdr 11:22:33:44:55:66 STALE
192.0.2.30 dev eth0  FAILED
192.0.2.40 dev eth0 lladdr 77:88:99:aa:bb:cc DELAY
";
        let table = parse_ip_neigh(output);
        assert_eq!(
            table.get("192.0.2.10").copied(),
            Some(Mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]))
        );
        // STALE entries still carry a usable lladdr.
        assert_eq!(
            table.get("192.0.2.20").copied(),
            Some(Mac([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]))
        );
        // FAILED entry (no lladdr) is skipped.
        assert!(!table.contains_key("192.0.2.30"));
        // DELAY entry resolves.
        assert_eq!(
            table.get("192.0.2.40").copied(),
            Some(Mac([0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc]))
        );
    }

    #[test]
    fn parses_ip_neigh_empty_and_garbage() {
        assert!(parse_ip_neigh("").is_empty());
        assert!(parse_ip_neigh("garbage line with no lladdr").is_empty());
    }

    #[test]
    fn resolve_ipv4_passes_through_literal_v4() {
        assert_eq!(resolve_ipv4("192.0.2.1").as_deref(), Some("192.0.2.1"));
    }

    #[test]
    fn resolve_ipv4_rejects_v6_literal() {
        assert_eq!(resolve_ipv4("::1"), None);
    }

    // --- MAC-source precedence (configured → neighbor → cache) ---

    const CONFIGURED: Mac = Mac([0x01, 0x01, 0x01, 0x01, 0x01, 0x01]);
    const NEIGHBOR: Mac = Mac([0x02, 0x02, 0x02, 0x02, 0x02, 0x02]);
    const CACHED: Mac = Mac([0x03, 0x03, 0x03, 0x03, 0x03, 0x03]);

    #[test]
    fn pick_mac_prefers_configured_and_skips_both_fallbacks() {
        // A configured MAC wins AND must short-circuit discovery entirely — the
        // `ip neigh` shell-out and the cache read are the expensive parts, and
        // there is nothing they could add to an authoritative value.
        let mut neighbor_called = false;
        let mut cache_called = false;
        let got = pick_mac(
            Some("01:01:01:01:01:01"),
            || {
                neighbor_called = true;
                Some(NEIGHBOR)
            },
            || {
                cache_called = true;
                Some(CACHED)
            },
        );
        assert_eq!(got, Some(CONFIGURED));
        assert!(!neighbor_called, "neighbor table must not be consulted");
        assert!(!cache_called, "MAC cache must not be consulted");
    }

    #[test]
    fn pick_mac_falls_back_to_neighbor_when_unconfigured() {
        // The pre-existing behavior, unchanged when no `mac` is configured.
        let mut cache_called = false;
        let got = pick_mac(
            None,
            || Some(NEIGHBOR),
            || {
                cache_called = true;
                Some(CACHED)
            },
        );
        assert_eq!(got, Some(NEIGHBOR));
        assert!(!cache_called, "cache is only the last resort");
    }

    #[test]
    fn pick_mac_falls_back_to_cache_when_neighbor_misses() {
        // The "host already asleep, evicted from the neighbor table" path.
        assert_eq!(pick_mac(None, || None, || Some(CACHED)), Some(CACHED));
    }

    #[test]
    fn pick_mac_none_when_every_source_is_empty() {
        // This is the acute gap a configured MAC closes: aged-out neighbor entry
        // + cold cache ⇒ no MAC ⇒ `{"status":"error","reason":"no-mac"}`.
        assert_eq!(pick_mac(None, || None, || None), None);
    }

    #[test]
    fn pick_mac_ignores_unparseable_configured_and_degrades_to_discovery() {
        // validate() rejects these at startup, so this is belt-and-braces: a bad
        // configured value must not be worse than having configured nothing.
        assert_eq!(
            pick_mac(Some("not-a-mac"), || Some(NEIGHBOR), || Some(CACHED)),
            Some(NEIGHBOR)
        );
        assert_eq!(pick_mac(Some(""), || None, || Some(CACHED)), Some(CACHED));
    }

    #[test]
    fn pick_mac_accepts_dash_separated_and_uppercase_configured() {
        assert_eq!(
            pick_mac(Some("01-01-01-01-01-01"), || None, || None),
            Some(CONFIGURED)
        );
        assert_eq!(
            pick_mac(Some("01:01:01:01:01:01"), || None, || None),
            Some(CONFIGURED)
        );
    }

    // --- configured-MAC lookup by host ---

    fn host_cfg(name: &str, url: &str, mac: Option<&str>) -> crate::daemon_config::SteamHostConfig {
        crate::daemon_config::SteamHostConfig {
            name: name.to_string(),
            url: url.to_string(),
            token_file: None,
            mac: mac.map(str::to_string),
        }
    }

    #[test]
    fn configured_mac_matches_host_by_url_host_part() {
        let hosts = vec![
            host_cfg(
                "linux",
                "http://192.0.2.10:47995",
                Some("aa:bb:cc:dd:ee:ff"),
            ),
            host_cfg(
                "windows",
                "http://192.0.2.20:47995",
                Some("11:22:33:44:55:66"),
            ),
        ];
        // `wol <host>` passes the URL's name-part, which is what we match on.
        assert_eq!(
            configured_mac_for(&hosts, "192.0.2.10"),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(
            configured_mac_for(&hosts, "192.0.2.20"),
            Some("11:22:33:44:55:66")
        );
        // An unknown host, and an entry that pins no MAC, both yield None
        // (discovery still applies).
        assert_eq!(configured_mac_for(&hosts, "192.0.2.99"), None);
        assert_eq!(configured_mac_for(&[], "192.0.2.10"), None);
        let no_mac = vec![host_cfg("linux", "http://192.0.2.10:47995", None)];
        assert_eq!(configured_mac_for(&no_mac, "192.0.2.10"), None);
    }

    #[test]
    fn configured_mac_matches_hostname_urls_too() {
        let hosts = vec![host_cfg(
            "gaming-pc",
            "http://gaming-pc:47995/",
            Some("aa:bb:cc:dd:ee:ff"),
        )];
        assert_eq!(
            configured_mac_for(&hosts, "gaming-pc"),
            Some("aa:bb:cc:dd:ee:ff")
        );
        // The selector is the URL's HOST part, not the entry's `name`.
        assert_eq!(configured_mac_for(&hosts, "http://gaming-pc:47995"), None);
    }
}

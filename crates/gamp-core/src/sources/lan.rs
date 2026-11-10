//! Plex (GDM) and Jellyfin/Emby servers found on the local network.

use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::*;

/// A media server that answered a LAN search.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Found {
    /// "plex", "jellyfin", or "emby".
    pub kind: String,
    pub name: String,
    pub url: String,
    pub id: String,
}

#[derive(Default)]
struct Cache {
    servers: Vec<Found>,
    searched: Option<Instant>,
    searching: bool,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Looks again in the background, at most every few minutes.
pub fn refresh_soon() {
    refresh(false);
}

pub fn refresh(force: bool) {
    {
        let mut f = cache().lock().unwrap();
        if f.searching || (!force && f.searched.is_some_and(|t| t.elapsed() < Duration::from_secs(180))) {
            return;
        }
        f.searching = true;
    }
    tokio::spawn(async {
        let servers = discover().await;
        let mut f = cache().lock().unwrap();
        // A server that missed one search keeps its place.
        for s in servers {
            f.servers.retain(|o| o.id != s.id);
            f.servers.push(s);
        }
        f.servers.sort_by_key(|a| a.name.to_lowercase());
        f.searched = Some(Instant::now());
        f.searching = false;
    });
}

pub fn searching() -> bool {
    cache().lock().unwrap().searching
}

/// What's been found and isn't already added as an account.
pub async fn found() -> Vec<Found> {
    let servers = cache().lock().unwrap().servers.clone();
    let accounts = super::accounts().await;
    servers.into_iter().filter(|s| !is_added(s, &accounts)).collect()
}

const PLEX_PORT: u16 = 32400;
const MEDIA_PORT: u16 = 8096;

struct Probe {
    kind: &'static str,
    message: &'static [u8],
    targets: &'static [&'static str],
    multicast: bool,
}

// One socket per question, so a reply is tagged with who we asked.
const PROBES: &[Probe] = &[
    Probe {
        kind: "plex",
        message: b"M-SEARCH * HTTP/1.1\r\n\r\n",
        targets: &["239.0.0.250:32414", "255.255.255.255:32414"],
        multicast: true,
    },
    Probe { kind: "jellyfin", message: b"who is JellyfinServer?", targets: &["255.255.255.255:7359"], multicast: false },
    Probe { kind: "emby", message: b"who is EmbyServer?", targets: &["255.255.255.255:7359"], multicast: false },
];

fn udp_socket(addr: Option<std::net::Ipv4Addr>, multicast: bool) -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_broadcast(true)?;
    let ip = addr.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
    if multicast {
        if let Some(a) = addr {
            sock.set_multicast_if_v4(&a)?;
        }
        sock.set_multicast_ttl_v4(2)?;
    }
    sock.bind(&std::net::SocketAddr::from((ip, 0)).into())?;
    sock.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(sock.into())
}

fn ipv4_ifaces() -> Vec<std::net::Ipv4Addr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(a) => Some(a),
            _ => None,
        })
        .collect()
}

/// Ask every network this machine is on, then read answers for a few seconds.
async fn discover() -> Vec<Found> {
    // iOS needs a special Apple entitlement for multicast.
    if cfg!(target_os = "ios") {
        return Vec::new();
    }
    let mut sockets = Vec::new();
    for addr in ipv4_ifaces().into_iter().map(Some).chain([None]) {
        for probe in PROBES {
            match udp_socket(addr, probe.multicast) {
                Ok(sock) => {
                    for _ in 0..2 {
                        for target in probe.targets {
                            let _ = sock.send_to(probe.message, *target).await;
                        }
                    }
                    sockets.push((probe.kind, sock));
                }
                Err(e) => log::debug!("LAN: no {} search on {addr:?}: {e}", probe.kind),
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let listen = sockets.iter().map(|(kind, sock)| async move {
        let mut found = Vec::new();
        let mut buf = [0u8; 2048];
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(Ok((n, from))) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else { break };
            if let Some(server) = read_reply(kind, &buf[..n], from.ip()) {
                found.push(server);
            }
        }
        found
    });
    let mut out = Vec::new();
    for server in futures_util::future::join_all(listen).await.into_iter().flatten() {
        if !out.iter().any(|o: &Found| o.id == server.id) {
            out.push(server);
        }
    }
    log::info!("LAN: {} answered", out.len());
    out
}

fn read_reply(kind: &str, packet: &[u8], sender: IpAddr) -> Option<Found> {
    match kind {
        "plex" => parse_gdm(packet, sender),
        "jellyfin" | "emby" => parse_announce(packet, sender, kind),
        _ => None,
    }
}

fn http_url(sender: IpAddr, port: u16) -> String {
    format!("http://{}", std::net::SocketAddr::new(sender, port))
}

/// `host:port`, host in lowercase, port written out (80 for http, 443 for https).
fn host_port(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    Some((parsed.host_str()?.to_ascii_lowercase(), parsed.port_or_known_default()?))
}

/// True when this server is already an account: same host and port on `url`
/// or any `alt_urls`, or (Plex) the same machine id in `user_id`.
fn is_added(found: &Found, accounts: &[Account]) -> bool {
    let place = host_port(&found.url);
    accounts.iter().any(|acc| {
        let same_place = place.is_some()
            && std::iter::once(acc.url.as_str()).chain(acc.alt_urls.iter().map(String::as_str)).any(|url| host_port(url) == place);
        let same_plex = found.kind == "plex" && acc.kind == Kind::Plex && !found.id.is_empty() && acc.user_id == found.id;
        same_place || same_plex
    })
}

fn http_ok(status: &str) -> bool {
    let mut parts = status.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(version), Some(code)) => version.get(..5).is_some_and(|v| v.eq_ignore_ascii_case("http/")) && code == "200",
        _ => false,
    }
}

/// A Plex GDM reply. `None` when it isn't a media-server answer.
fn parse_gdm(packet: &[u8], sender: IpAddr) -> Option<Found> {
    let text = String::from_utf8_lossy(packet).replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = text.lines();
    if !http_ok(lines.next()?.trim()) {
        return None;
    }
    let mut name = String::new();
    let mut id = String::new();
    let mut port = None;
    let mut content_type = String::new();
    for line in lines {
        let Some((k, v)) = line.trim().split_once(':') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "content-type" if content_type.is_empty() => content_type = v.to_owned(),
            "name" if name.is_empty() => name = v.to_owned(),
            "resource-identifier" if id.is_empty() => id = v.to_owned(),
            "port" if port.is_none() => port = v.parse::<u16>().ok().filter(|&p| p > 0),
            _ => {}
        }
    }
    if !content_type.to_ascii_lowercase().contains("plex/media-server") || (name.is_empty() && id.is_empty()) {
        return None;
    }
    let url = http_url(sender, port.unwrap_or(PLEX_PORT));
    if id.is_empty() {
        id = url.clone();
    }
    if name.is_empty() {
        name = "Plex".to_owned();
    }
    Some(Found { kind: "plex".into(), name, url, id })
}

fn json_str(v: &Value, name: &str) -> Option<String> {
    let obj = v.as_object()?;
    let val = obj.iter().find(|(k, _)| k.eq_ignore_ascii_case(name))?.1;
    let text = match val {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if text.is_empty() { None } else { Some(text) }
}

/// A Jellyfin or Emby discovery reply. `kind` is which question it answered.
fn parse_announce(packet: &[u8], sender: IpAddr, kind: &str) -> Option<Found> {
    let v: Value = serde_json::from_slice(packet).ok()?;
    if !v.is_object() {
        return None;
    }
    let url = match json_str(&v, "Address") {
        Some(address) => address,
        None => http_url(sender, MEDIA_PORT),
    };
    let id = json_str(&v, "Id").unwrap_or_else(|| url.clone());
    let name = json_str(&v, "Name").unwrap_or_else(|| match kind {
        "emby" => "Emby".to_owned(),
        _ => "Jellyfin".to_owned(),
    });
    Some(Found { kind: kind.to_owned(), name, url, id })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn account(kind: Kind, url: &str, alts: &[&str], user_id: &str) -> Account {
        Account { kind, url: url.to_owned(), alt_urls: alts.iter().copied().map(str::to_owned).collect(), user_id: user_id.to_owned(), ..Default::default() }
    }

    #[test]
    fn gdm_headers() {
        let pkt = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\nResource-Identifier: 0123abcd\r\nname:   DiskStation  \r\nPORT: 32401\r\nVersion: 1.40.0\r\n\r\n";
        let found = parse_gdm(pkt, ip(192, 168, 1, 5)).unwrap();
        assert_eq!(found.kind, "plex");
        assert_eq!(found.name, "DiskStation");
        assert_eq!(found.url, "http://192.168.1.5:32401");
        assert_eq!(found.id, "0123abcd");

        // Missing port, odd content-type case, line feeds only.
        let bare = b"HTTP/1.1 200 OK\ncontent-type: PLEX/MEDIA-SERVER\nName: Office\nResource-Identifier: abc\n";
        let found = parse_gdm(bare, ip(10, 0, 0, 2)).unwrap();
        assert_eq!(found.url, "http://10.0.0.2:32400");
        assert_eq!(found.name, "Office");
        assert_eq!(found.id, "abc");

        let bad_port = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\nName: X\r\nResource-Identifier: id\r\nPort: no\r\n";
        assert_eq!(parse_gdm(bad_port, ip(10, 0, 0, 2)).unwrap().url, "http://10.0.0.2:32400");
        let zero = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\nName: X\r\nResource-Identifier: id\r\nPort: 0\r\n";
        assert_eq!(parse_gdm(zero, ip(10, 0, 0, 2)).unwrap().url, "http://10.0.0.2:32400");

        // Name only: the id falls back to the url.
        let name_only = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\nName: Only\r\n";
        let found = parse_gdm(name_only, ip(192, 168, 1, 5)).unwrap();
        assert_eq!(found.name, "Only");
        assert_eq!(found.id, "http://192.168.1.5:32400");

        assert!(parse_gdm(b"not a reply", ip(1, 2, 3, 4)).is_none());
        assert!(parse_gdm(b"HTTP/1.0 404 No\r\nContent-Type: plex/media-server\r\nName: X\r\nResource-Identifier: a\r\n", ip(1, 2, 3, 4)).is_none());
        assert!(parse_gdm(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nName: X\r\nResource-Identifier: a\r\n", ip(1, 2, 3, 4)).is_none());
        assert!(parse_gdm(b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\n\r\n", ip(1, 2, 3, 4)).is_none());
    }

    #[test]
    fn jellyfin_json() {
        let pkt = br#"{"Address":"http://192.168.1.5:8096","Id":"abc","Name":"media-pc","EndpointAddress":null}"#;
        let found = parse_announce(pkt, ip(10, 0, 0, 8), "jellyfin").unwrap();
        assert_eq!(
            found,
            Found { kind: "jellyfin".into(), name: "media-pc".into(), url: "http://192.168.1.5:8096".into(), id: "abc".into() }
        );

        let lower = br#"{"address":"http://192.168.1.8:8096","id":"xyz","name":"box"}"#;
        let found = parse_announce(lower, ip(10, 0, 0, 1), "emby").unwrap();
        assert_eq!(found.kind, "emby");
        assert_eq!(found.name, "box");
        assert_eq!(found.url, "http://192.168.1.8:8096");
        assert_eq!(found.id, "xyz");

        let no_addr = br#"{"Id":"abc","Name":"media-pc"}"#;
        let found = parse_announce(no_addr, ip(192, 168, 1, 9), "jellyfin").unwrap();
        assert_eq!(found.url, "http://192.168.1.9:8096");
        assert_eq!(found.id, "abc");
        assert_eq!(found.name, "media-pc");

        let no_id = br#"{"Name":"media-pc","Address":null}"#;
        let found = parse_announce(no_id, ip(192, 168, 1, 9), "emby").unwrap();
        assert_eq!(found.kind, "emby");
        assert_eq!(found.url, "http://192.168.1.9:8096");
        assert_eq!(found.id, found.url);

        assert!(parse_announce(b"not json", ip(1, 1, 1, 1), "jellyfin").is_none());
        assert!(parse_announce(b"{", ip(1, 1, 1, 1), "emby").is_none());
        assert!(parse_announce(b"[]", ip(1, 1, 1, 1), "jellyfin").is_none());
        assert!(parse_announce(b"", ip(1, 1, 1, 1), "jellyfin").is_none());
    }

    #[test]
    fn already_added() {
        let plex = Found { kind: "plex".into(), name: "DiskStation".into(), url: "http://192.168.1.5:32400".into(), id: "0123abcd".into() };
        assert!(is_added(&plex, &[account(Kind::Plex, "http://192.168.1.5:32400/", &[], "")]));
        assert!(is_added(&plex, &[account(Kind::Subsonic, "http://10.0.0.1:1", &["http://192.168.1.5:32400"], "")]));

        let https = Found { kind: "jellyfin".into(), name: "nas".into(), url: "https://nas:443".into(), id: "j".into() };
        assert!(is_added(&https, &[account(Kind::Jellyfin, "https://nas", &[], "user")]));
        let https_plain = Found { kind: "emby".into(), name: "nas".into(), url: "https://nas".into(), id: "e".into() };
        assert!(is_added(&https_plain, &[account(Kind::Jellyfin, "https://NAS:443/web", &[], "")]));

        let http_default = Found { kind: "jellyfin".into(), name: "nas".into(), url: "http://nas:80".into(), id: "h".into() };
        assert!(is_added(&http_default, &[account(Kind::Jellyfin, "http://NAS", &[], "")]));

        // A different port is a different server, even when the Plex id is someone else's.
        let other_port = Found { kind: "plex".into(), name: "DiskStation".into(), url: "http://192.168.1.5:32401".into(), id: "other".into() };
        assert!(!is_added(&other_port, &[account(Kind::Plex, "http://192.168.1.5:32400/", &[], "0123abcd")]));

        // Plex stores the machine id in user_id, so a second address still counts.
        let away = Found { kind: "plex".into(), name: "Away".into(), url: "http://10.1.1.5:32400".into(), id: "0123abcd".into() };
        assert!(is_added(&away, &[account(Kind::Plex, "http://192.168.1.5:32400", &[], "0123abcd")]));
        assert!(!is_added(&away, &[account(Kind::Jellyfin, "http://10.9.9.9:8096", &[], "0123abcd")]));

        let blank = Found { kind: "plex".into(), name: "x".into(), url: "http://1.2.3.4:32400".into(), id: String::new() };
        assert!(!is_added(&blank, &[account(Kind::Plex, "http://9.9.9.9:1", &[], "")]));
        assert!(!is_added(&plex, &[]));
    }
}

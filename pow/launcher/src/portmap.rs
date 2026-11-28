//! Port mapping for the local node's P2P port.
//!
//! A VPN's port forwarding is assigned on the provider's side; the machine cannot read it from the OS. Gateways expose it
//! through one of the standard local protocols, tried in order:
//!
//! - **NAT-PMP** (RFC 6886): a UDP request to the gateway on port 5351 for a TCP mapping.
//! - **PCP** (RFC 6887): the successor to NAT-PMP, same port, a MAP request with a nonce.
//! - **UPnP-IGD**: SSDP discovery + the WANIPConnection/WANPPPConnection AddPortMapping SOAP action (mostly home routers;
//!   some VPN gateways offer it too).
//!
//! NAT-PMP/PCP mappings can be short-lived, so the app renews one while it runs. When none of the three answers (a provider
//! with a private API and no local mapping protocol), the page asks the user for the port shown by their VPN app.
//!
//! The code is provider-agnostic: it asks whichever gateway owns the route, using only standard protocols.

use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

/// NAT-PMP and PCP both use UDP port 5351.
const MAPPING_PORT: u16 = 5351;
/// Ask for a minute: many gateways grant short-lived mappings, and a minute is enough for the app to renew within it.
const LIFETIME: u32 = 60;
/// UPnP's permanent lease; a router that only accepts finite leases still accepts 3600 on most firmware.
const UPNP_LEASE: u32 = 3600;

#[derive(Clone, Debug, Serialize)]
pub struct Mapping {
    /// The gateway that granted the mapping (the VPN's tunnel gateway when a VPN owns the default route).
    pub gateway: String,
    /// The node's local P2P port.
    pub internal_port: u16,
    /// The public port the gateway forwards to `internal_port`.
    pub external_port: u16,
    /// Seconds the mapping is valid for; 0 means permanent (UPnP).
    pub lifetime: u32,
    /// `NAT-PMP`, `PCP` or `UPnP-IGD`.
    pub method: String,
    /// How to renew it; not part of the page's data.
    #[serde(skip)]
    renew: Renew,
}

#[derive(Clone, Debug)]
enum Renew {
    NatPmp,
    Pcp { nonce: [u8; 12], suggested_external: u16 },
    Upnp { control_url: String, service: String, internal_ip: Ipv4Addr, external_port: u16 },
}

/// The default route's gateway, preferring a VPN interface when one has a route. On Linux this is the VPN gateway when the
/// VPN owns the default route, so a mapping request reaches the provider's port-forwarding server; with no VPN it is the
/// router (NAT-PMP/PCP/UPnP on the local network).
pub fn default_gateway() -> Result<Ipv4Addr, String> {
    #[cfg(target_os = "linux")]
    {
        let table = std::fs::read_to_string("/proc/net/route").map_err(|e| format!("cannot read the routing table: {e}"))?;
        let vpn_iface = |iface: &str| ["tun", "wg", "ppp", "tailscale", "utun"].iter().any(|p| iface.starts_with(p));
        let mut vpn: Option<(u32, Ipv4Addr)> = None;
        let mut default: Option<(u32, Ipv4Addr)> = None;
        for line in table.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 {
                continue;
            }
            // /proc/net/route prints IPv4 addresses as the four bytes in little-endian hex (010011AC is 172.17.0.1).
            let Ok(raw) = u32::from_str_radix(f[2], 16) else { continue };
            let gw = Ipv4Addr::from(raw.swap_bytes());
            if gw.is_unspecified() {
                continue;
            }
            let Ok(metric) = f[6].parse::<u32>() else { continue };
            // A VPN's route (a WireGuard/OpenVPN interface, or the split 0.0.0.0/1) is where the mapping server lives.
            // Prefer it over a LAN default route even when the VPN replaced only half the default route.
            if vpn_iface(f[0]) && vpn.as_ref().is_none_or(|(m, _)| metric < *m) {
                vpn = Some((metric, gw));
            }
            if f[1] == "00000000" && default.as_ref().is_none_or(|(m, _)| metric < *m) {
                default = Some((metric, gw));
            }
        }
        vpn.or(default).map(|(_, gw)| gw).ok_or_else(|| "no IPv4 route with a gateway: is this machine online?".into())
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("route").args(["-n", "get", "default"]).output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(v) = line.trim().strip_prefix("gateway:") {
                if let Ok(ip) = v.trim().parse::<Ipv4Addr>() {
                    return Ok(ip);
                }
            }
        }
        Err("no default IPv4 gateway".into())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("automatic port detection is implemented on Linux and macOS; enter the port shown by your VPN app".into())
    }
}

/// Tries NAT-PMP, then PCP, then UPnP-IGD on the default gateway. The first one that answers wins.
pub fn detect(internal_port: u16) -> Result<Mapping, String> {
    if internal_port < 1024 {
        return Err("the local P2P port must be 1024 or higher".into());
    }
    let gateway = default_gateway()?;
    let mut errors: Vec<String> = Vec::new();
    match natpmp_map(gateway, internal_port, 0) {
        Ok(m) => return Ok(m),
        Err(e) => errors.push(format!("NAT-PMP: {e}")),
    }
    match pcp_map(gateway, internal_port, 0) {
        Ok(m) => return Ok(m),
        Err(e) => errors.push(format!("PCP: {e}")),
    }
    match upnp_map(gateway, internal_port) {
        Ok(m) => return Ok(m),
        Err(e) => errors.push(format!("UPnP-IGD: {e}")),
    }
    Err(format!(
        "No port-mapping service answered on {gateway} ({}). Your VPN may not expose the forwarded port through NAT-PMP, PCP or UPnP; enter the port shown by your VPN app.",
        errors.join("; ")
    ))
}

/// Renews a mapping, asking for the same public port where the protocol allows it.
pub fn renew(m: &Mapping) -> Result<Mapping, String> {
    match &m.renew {
        Renew::NatPmp => {
            let gw: Ipv4Addr = m.gateway.parse().map_err(|_| "the saved gateway is not an IPv4 address".to_string())?;
            natpmp_map(gw, m.internal_port, m.external_port)
        }
        Renew::Pcp { nonce, suggested_external } => {
            let gw: Ipv4Addr = m.gateway.parse().map_err(|_| "the saved gateway is not an IPv4 address".to_string())?;
            pcp_map_with(gw, m.internal_port, *suggested_external, *nonce)
        }
        Renew::Upnp { control_url, service, internal_ip, external_port } => {
            let gateway: Ipv4Addr = m.gateway.parse().map_err(|_| "the saved gateway is not an IPv4 address".to_string())?;
            upnp_add(control_url, service, *internal_ip, m.internal_port, *external_port, gateway)
        }
    }
}

/// How often the app renews: half the granted lifetime, at least every 20 s, at most every 30 min. A permanent UPnP mapping
/// is refreshed every 30 min anyway (routers reboot and drop their table).
pub fn renew_every(m: &Mapping) -> Duration {
    let lifetime = if m.lifetime == 0 { 3600 } else { m.lifetime };
    Duration::from_secs((lifetime / 2).clamp(20, 1800) as u64)
}

// ---- NAT-PMP ------------------------------------------------------------------------------------------------------------

fn natpmp_map(gateway: Ipv4Addr, internal_port: u16, suggested_external_port: u16) -> Result<Mapping, String> {
    let mut m = natpmp_request(gateway, internal_port, suggested_external_port)?;
    m.method = "NAT-PMP".into();
    m.renew = Renew::NatPmp;
    Ok(m)
}

fn natpmp_request(gateway: Ipv4Addr, internal_port: u16, suggested_external_port: u16) -> Result<Mapping, String> {
    if internal_port < 1024 {
        return Err("the local P2P port must be 1024 or higher".into());
    }
    let sock = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| format!("cannot open a UDP socket: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(400))).map_err(|e| e.to_string())?;
    let req = natpmp_request_bytes(internal_port, suggested_external_port, LIFETIME);
    let dest = SocketAddrV4::new(gateway, MAPPING_PORT);
    let mut last = String::new();
    for _ in 0..3 {
        if let Err(e) = sock.send_to(&req, dest) {
            last = e.to_string();
            continue;
        }
        let mut buf = [0u8; 64];
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if from.ip() == IpAddr::V4(gateway) => {
                let mut m = natpmp_parse(&buf[..n], internal_port)?;
                m.gateway = gateway.to_string();
                return Ok(m);
            }
            Ok((_, from)) => last = format!("answer from {from}, not the gateway {gateway}"),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => last = "no answer".into(),
            Err(e) => last = e.to_string(),
        }
    }
    let detail = if last == "no answer" { String::new() } else { format!(" ({last})") };
    Err(format!("the gateway did not answer{detail}"))
}

fn natpmp_request_bytes(internal_port: u16, suggested_external_port: u16, lifetime: u32) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0] = 0; // version
    b[1] = 2; // opcode: map TCP
    // b[2..4]: reserved, 0
    b[4..6].copy_from_slice(&internal_port.to_be_bytes());
    b[6..8].copy_from_slice(&suggested_external_port.to_be_bytes());
    b[8..12].copy_from_slice(&lifetime.to_be_bytes());
    b
}

fn natpmp_parse(buf: &[u8], expected_internal: u16) -> Result<Mapping, String> {
    if buf.len() < 16 {
        return Err(format!("short NAT-PMP answer ({} bytes)", buf.len()));
    }
    if buf[0] != 0 {
        return Err(format!("unsupported NAT-PMP version {}", buf[0]));
    }
    if buf[1] != 130 {
        return Err(format!("unexpected NAT-PMP opcode {}", buf[1]));
    }
    let result = u16::from_be_bytes([buf[2], buf[3]]);
    if result != 0 {
        let why = match result {
            1 => "the gateway does not support this NAT-PMP version",
            2 => "the gateway refused the mapping (not authorized)",
            3 => "network failure",
            4 => "out of resources",
            5 => "unsupported opcode",
            _ => "unknown error",
        };
        return Err(format!("mapping refused: {why} (code {result})"));
    }
    let internal = u16::from_be_bytes([buf[8], buf[9]]);
    let external = u16::from_be_bytes([buf[10], buf[11]]);
    let lifetime = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
    if internal != expected_internal {
        return Err(format!("the gateway answered for port {internal}, not {expected_internal}"));
    }
    if external == 0 {
        return Err("the gateway returned no public port".into());
    }
    Ok(Mapping { gateway: String::new(), internal_port: internal, external_port: external, lifetime, method: String::new(), renew: Renew::NatPmp })
}

// ---- PCP ----------------------------------------------------------------------------------------------------------------

fn pcp_map(gateway: Ipv4Addr, internal_port: u16, suggested_external_port: u16) -> Result<Mapping, String> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("cannot generate a PCP nonce: {e}"))?;
    pcp_map_with(gateway, internal_port, suggested_external_port, nonce)
}

fn pcp_map_with(gateway: Ipv4Addr, internal_port: u16, suggested_external_port: u16, nonce: [u8; 12]) -> Result<Mapping, String> {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| format!("cannot open a UDP socket: {e}"))?;
    sock.connect(SocketAddrV4::new(gateway, MAPPING_PORT)).map_err(|e| format!("cannot reach {gateway}: {e}"))?;
    let client_ip = match sock.local_addr().map_err(|e| e.to_string())?.ip() {
        IpAddr::V4(ip) => ip,
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    };
    let req = pcp_request_bytes(internal_port, suggested_external_port, nonce, client_ip);
    sock.set_read_timeout(Some(Duration::from_millis(400))).map_err(|e| e.to_string())?;
    let mut last = String::new();
    for _ in 0..3 {
        if let Err(e) = sock.send(&req) {
            last = e.to_string();
            continue;
        }
        let mut buf = [0u8; 128];
        match sock.recv(&mut buf) {
            Ok(n) => {
                let mut m = pcp_parse(&buf[..n], internal_port, &nonce)?;
                m.gateway = gateway.to_string();
                m.method = "PCP".into();
                m.renew = Renew::Pcp { nonce, suggested_external: m.external_port };
                return Ok(m);
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => last = "no answer".into(),
            Err(e) => last = e.to_string(),
        }
    }
    let detail = if last == "no answer" { String::new() } else { format!(" ({last})") };
    Err(format!("the gateway did not answer{detail}"))
}

fn pcp_request_bytes(internal_port: u16, suggested_external_port: u16, nonce: [u8; 12], client_ip: Ipv4Addr) -> [u8; 60] {
    let mut b = [0u8; 60];
    b[0] = 2; // version
    b[1] = 1; // R=0, opcode MAP
    // b[2..4]: reserved
    b[4..8].copy_from_slice(&LIFETIME.to_be_bytes());
    // Client IP, IPv4-mapped IPv6 (::ffff:a.b.c.d)
    b[18] = 0xff;
    b[19] = 0xff;
    b[20..24].copy_from_slice(&client_ip.octets());
    b[24..36].copy_from_slice(&nonce);
    b[36] = 6; // TCP
    // b[37..40]: reserved
    b[40..42].copy_from_slice(&internal_port.to_be_bytes());
    b[42..44].copy_from_slice(&suggested_external_port.to_be_bytes());
    // b[44..60]: suggested external IP, all zero
    b
}

fn pcp_parse(buf: &[u8], expected_internal: u16, nonce: &[u8; 12]) -> Result<Mapping, String> {
    if buf.len() < 60 {
        return Err(format!("short PCP answer ({} bytes)", buf.len()));
    }
    if buf[0] != 2 {
        return Err(format!("unsupported PCP version {}", buf[0]));
    }
    if buf[1] != 0x81 {
        return Err(format!("unexpected PCP opcode {}", buf[1]));
    }
    let result = buf[3];
    if result != 0 {
        let why = match result {
            1 => "unsupported PCP version",
            2 => "not authorized",
            3 => "malformed request",
            4 => "unsupported opcode",
            5 => "unsupported option",
            6 => "malformed option",
            7 => "network failure",
            8 => "out of resources",
            9 => "unsupported protocol",
            10 => "quota exceeded",
            11 => "cannot provide an external address",
            _ => "unknown error",
        };
        return Err(format!("mapping refused: {why} (code {result})"));
    }
    let lifetime = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if &buf[24..36] != nonce {
        return Err("the answer does not carry our nonce".into());
    }
    if buf[36] != 6 {
        return Err("the answer is not for TCP".into());
    }
    let internal = u16::from_be_bytes([buf[40], buf[41]]);
    let external = u16::from_be_bytes([buf[42], buf[43]]);
    if internal != expected_internal {
        return Err(format!("the gateway answered for port {internal}, not {expected_internal}"));
    }
    if external == 0 {
        return Err("the gateway returned no public port".into());
    }
    Ok(Mapping { gateway: String::new(), internal_port: internal, external_port: external, lifetime, method: String::new(), renew: Renew::Pcp { nonce: *nonce, suggested_external: external } })
}

// ---- UPnP-IGD -----------------------------------------------------------------------------------------------------------

fn upnp_map(gateway: Ipv4Addr, internal_port: u16) -> Result<Mapping, String> {
    let location = upnp_discover()?;
    let xml = ureq::get(&location).timeout(Duration::from_secs(3)).call().map_err(|e| format!("cannot read {location}: {e}"))?.into_string().map_err(|e| e.to_string())?;
    let (service, control) = upnp_service(&xml).ok_or_else(|| "the gateway description has no WANIPConnection/WANPPPConnection service".to_string())?;
    let control_url = join_url(&location, &control);
    let internal_ip = local_ip_towards(gateway)?;
    // Try the node's own port first; if the router has it taken, try random ports until one is free.
    let mut candidates = vec![internal_port];
    for _ in 0..12 {
        let mut r = [0u8; 2];
        if getrandom::getrandom(&mut r).is_err() {
            break;
        }
        let p = 20000 + (u16::from_be_bytes(r) % 40000);
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    }
    let mut last = String::new();
    for external in candidates {
        match upnp_add(&control_url, &service, internal_ip, internal_port, external, gateway) {
            Ok(m) => return Ok(m),
            Err(e) => {
                last = e;
                if !last.contains("718") && !last.contains("conflict") {
                    break;
                }
            }
        }
    }
    Err(last)
}

fn upnp_discover() -> Result<String, String> {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| format!("cannot open a UDP socket: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(700))).map_err(|e| e.to_string())?;
    for st in ["urn:schemas-upnp-org:device:InternetGatewayDevice:1", "upnp:rootdevice"] {
        let msg = format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n");
        sock.send_to(msg.as_bytes(), ("239.255.255.250", 1900)).map_err(|e| format!("SSDP search failed: {e}"))?;
    }
    for _ in 0..3 {
        let mut buf = [0u8; 4096];
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                for line in text.lines() {
                    if line.len() > 9 && line[..9].eq_ignore_ascii_case("location:") {
                        let location = line[9..].trim();
                        if location.starts_with("http://") || location.starts_with("https://") {
                            return Ok(location.to_string());
                        }
                    }
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("no SSDP answer (UPnP may be disabled on the gateway)".into())
}

fn upnp_service(xml: &str) -> Option<(String, String)> {
    let mut pos = 0;
    while let Some(start) = xml[pos..].find("<service>") {
        let start = pos + start;
        let end = xml[start..].find("</service>").map(|e| start + e + "</service>".len())?;
        let block = &xml[start..end];
        if let (Some(service), Some(control)) = (tag(block, "serviceType"), tag(block, "controlURL")) {
            if service.contains("WANIPConnection") || service.contains("WANPPPConnection") {
                return Some((service.trim().to_string(), control.trim().to_string()));
            }
        }
        pos = end;
    }
    None
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

fn join_url(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    let Some(scheme_end) = base.find("://") else { return path.to_string() };
    let after_scheme = scheme_end + 3;
    let authority_end = base[after_scheme..].find('/').map(|i| after_scheme + i).unwrap_or(base.len());
    let origin = &base[..authority_end];
    if path.starts_with('/') {
        format!("{origin}{path}")
    } else {
        match base[authority_end..].find('/').map(|i| authority_end + i) {
            Some(slash) => format!("{}{path}", &base[..=slash]),
            None => format!("{origin}/{path}"),
        }
    }
}

fn local_ip_towards(gateway: Ipv4Addr) -> Result<Ipv4Addr, String> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| format!("cannot open a UDP socket: {e}"))?;
    s.connect(SocketAddrV4::new(gateway, 53)).map_err(|e| format!("cannot route to {gateway}: {e}"))?;
    match s.local_addr().map_err(|e| e.to_string())?.ip() {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(_) => Err("the route to the gateway is IPv6; UPnP-IGD needs IPv4".into()),
    }
}

fn upnp_add(control_url: &str, service: &str, internal_ip: Ipv4Addr, internal_port: u16, external_port: u16, gateway: Ipv4Addr) -> Result<Mapping, String> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body><u:AddPortMapping xmlns:u="{service}">
<NewRemoteHost></NewRemoteHost><NewExternalPort>{external_port}</NewExternalPort><NewProtocol>TCP</NewProtocol>
<NewInternalPort>{internal_port}</NewInternalPort><NewInternalClient>{internal_ip}</NewInternalClient>
<NewEnabled>1</NewEnabled><NewPortMappingDescription>SM64Coin P2P</NewPortMappingDescription><NewLeaseDuration>{UPNP_LEASE}</NewLeaseDuration>
</u:AddPortMapping></s:Body></s:Envelope>"#
    );
    let action = format!("\"{service}#AddPortMapping\"");
    let res = ureq::post(control_url)
        .timeout(Duration::from_secs(4))
        .set("Content-Type", "text/xml; charset=\"utf-8\"")
        .set("SOAPAction", &action)
        .send_string(&body);
    match res {
        Ok(r) => {
            let _ = r.into_string();
            Ok(Mapping {
                gateway: gateway.to_string(),
                internal_port,
                external_port,
                lifetime: UPNP_LEASE,
                method: "UPnP-IGD".into(),
                renew: Renew::Upnp { control_url: control_url.to_string(), service: service.to_string(), internal_ip, external_port },
            })
        }
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            let upnp = tag(&text, "errorCode").unwrap_or("");
            let why = tag(&text, "errorDescription").or_else(|| tag(&text, "faultstring")).unwrap_or("the gateway refused the mapping");
            Err(format!("HTTP {code} (UPnP {upnp}): {why}"))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn natpmp_request_encodes_the_tcp_mapping() {
        let b = natpmp_request_bytes(16480, 0, 60);
        assert_eq!(&b[..2], &[0, 2]);
        assert_eq!(u16::from_be_bytes([b[4], b[5]]), 16480);
        assert_eq!(u16::from_be_bytes([b[6], b[7]]), 0);
        assert_eq!(u32::from_be_bytes([b[8], b[9], b[10], b[11]]), 60);
    }

    #[test]
    fn natpmp_response_carries_the_public_port() {
        let mut b = [0u8; 16];
        b[1] = 130;
        b[4..8].copy_from_slice(&7u32.to_be_bytes());
        b[8..10].copy_from_slice(&16480u16.to_be_bytes());
        b[10..12].copy_from_slice(&32123u16.to_be_bytes());
        b[12..16].copy_from_slice(&60u32.to_be_bytes());
        let m = natpmp_parse(&b, 16480).unwrap();
        assert_eq!((m.internal_port, m.external_port, m.lifetime), (16480, 32123, 60));
        b[3] = 2;
        assert!(natpmp_parse(&b, 16480).unwrap_err().contains("refused"));
        b[3] = 0;
        b[9] = 1;
        assert!(natpmp_parse(&b, 16480).unwrap_err().contains("not 16480"));
    }

    #[test]
    fn a_fake_natpmp_gateway_answers_the_mapping() {
        let server = match UdpSocket::bind(("127.0.0.1", MAPPING_PORT)) {
            Ok(s) => s,
            Err(_) => return,
        };
        let t = thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            assert_eq!(n, 12);
            let internal = u16::from_be_bytes([buf[4], buf[5]]);
            let mut answer = [0u8; 16];
            answer[1] = 130;
            answer[8..10].copy_from_slice(&internal.to_be_bytes());
            answer[10..12].copy_from_slice(&45678u16.to_be_bytes());
            answer[12..16].copy_from_slice(&60u32.to_be_bytes());
            server.send_to(&answer, from).unwrap();
        });
        let m = natpmp_request(Ipv4Addr::LOCALHOST, 16480, 0).unwrap();
        t.join().unwrap();
        assert_eq!((m.external_port, m.lifetime), (45678, 60));
    }

    #[test]
    fn pcp_request_and_response_use_the_same_nonce() {
        let nonce = [7u8; 12];
        let req = pcp_request_bytes(16480, 0, nonce, Ipv4Addr::new(192, 168, 1, 5));
        assert_eq!(&req[..2], &[2, 1]);
        assert_eq!(u32::from_be_bytes([req[4], req[5], req[6], req[7]]), 60);
        assert_eq!(&req[18..24], &[0xff, 0xff, 192, 168, 1, 5]);
        assert_eq!(&req[24..36], &nonce);
        assert_eq!(req[36], 6);
        assert_eq!(u16::from_be_bytes([req[40], req[41]]), 16480);
        let mut res = [0u8; 60];
        res[0] = 2;
        res[1] = 0x81;
        res[4..8].copy_from_slice(&60u32.to_be_bytes());
        res[24..36].copy_from_slice(&nonce);
        res[36] = 6;
        res[40..42].copy_from_slice(&16480u16.to_be_bytes());
        res[42..44].copy_from_slice(&32123u16.to_be_bytes());
        let m = pcp_parse(&res, 16480, &nonce).unwrap();
        assert_eq!((m.internal_port, m.external_port), (16480, 32123));
        res[3] = 8;
        assert!(pcp_parse(&res, 16480, &nonce).unwrap_err().contains("resources"));
    }

    #[test]
    fn upnp_description_and_url_join() {
        let xml = r#"<root><device><serviceList><service>
            <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
            <controlURL>/ctl/IPConn</controlURL></service></serviceList></device></root>"#;
        let (service, control) = upnp_service(xml).unwrap();
        assert!(service.contains("WANIPConnection"));
        assert_eq!(control, "/ctl/IPConn");
        assert_eq!(join_url("http://192.168.1.1:5000/rootDesc.xml", "/ctl/IPConn"), "http://192.168.1.1:5000/ctl/IPConn");
        assert_eq!(join_url("http://192.168.1.1:5000/rootDesc.xml", "IPConn"), "http://192.168.1.1:5000/IPConn");
    }

    #[test]
    fn upnp_add_sends_the_mapping_to_the_gateway() {
        let server = match tiny_http::Server::http("127.0.0.1:0") {
            Ok(s) => s,
            Err(_) => return,
        };
        let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
        let t = thread::spawn(move || {
            let mut req = server.recv().unwrap();
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            assert!(body.contains("<NewExternalPort>32123</NewExternalPort>"), "{body}");
            assert!(body.contains("<NewInternalPort>16480</NewInternalPort>"), "{body}");
            assert!(body.contains("<NewInternalClient>192.168.1.5</NewInternalClient>"), "{body}");
            assert!(body.contains("AddPortMapping"), "{body}");
            req.respond(tiny_http::Response::from_string("<ok/>")).unwrap();
        });
        let m = upnp_add(
            &format!("http://127.0.0.1:{port}/ctl"),
            "urn:schemas-upnp-org:service:WANIPConnection:1",
            Ipv4Addr::new(192, 168, 1, 5),
            16480,
            32123,
            Ipv4Addr::LOCALHOST,
        )
        .unwrap();
        t.join().unwrap();
        assert_eq!((m.external_port, m.internal_port, m.method.as_str()), (32123, 16480, "UPnP-IGD"));
    }
}

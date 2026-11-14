//! The client's address behind a reverse proxy, without trusting headers from anyone else.

use std::net::IpAddr;

use axum::http::HeaderMap;

/// An IPv4 or IPv6 network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Cidr> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max)?,
            None => max,
        };
        Some(Cidr { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            v4 => v4,
        };
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

/// Security review M5: the network of `ip` that the per-address limits count as one client: its first `v4` bits
/// (an IPv4-mapped IPv6 address is IPv4), its first `v6` bits for IPv6 (64 by default: one home or one server gets a
/// whole /64, and counting its single addresses gave one client 2^64 of them).
pub fn client_key(ip: IpAddr, v4: u8, v6: u8) -> IpAddr {
    let ip = match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => {
            let bits = u32::from(v4.min(32));
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            IpAddr::V4((u32::from(a) & mask).into())
        }
        IpAddr::V6(a) => {
            let bits = u32::from(v6.min(128));
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            IpAddr::V6((u128::from(a) & mask).into())
        }
    }
}

/// The client address: the socket peer, unless the peer is a trusted proxy; then the right-most address of
/// `X-Forwarded-For` that is not itself a trusted proxy (the left part of that header is client-controlled). Every
/// `X-Forwarded-For` line counts, in order (security review L-S4: a proxy that adds its own line after the client's
/// would otherwise leave the client's line to decide).
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[Cidr]) -> IpAddr {
    if !trusted.iter().any(|c| c.contains(peer)) {
        return peer;
    }
    let mut lines = Vec::new();
    for v in headers.get_all("x-forwarded-for") {
        match v.to_str() {
            Ok(s) => lines.push(s),
            Err(_) => return peer,
        }
    }
    if lines.is_empty() {
        return peer;
    }
    let xff = lines.join(",");
    for part in xff.rsplit(',').take(16) {
        match part.trim().parse::<IpAddr>() {
            Ok(ip) if trusted.iter().any(|c| c.contains(ip)) => continue,
            Ok(ip) => return ip,
            Err(_) => return peer,
        }
    }
    peer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_and_forwarded_for() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains("10.1.2.3".parse().unwrap()));
        assert!(c.contains("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert!(Cidr::parse("::1").unwrap().contains("::1".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("nonsense").is_none());
        let trusted = vec![Cidr::parse("127.0.0.1").unwrap()];
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 203.0.113.9".parse().unwrap());
        let proxy: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(client_ip(proxy, &h, &trusted), "203.0.113.9".parse::<IpAddr>().unwrap());
        // An untrusted peer's header is ignored.
        let other: IpAddr = "198.51.100.1".parse().unwrap();
        assert_eq!(client_ip(other, &h, &trusted), other);
        h.insert("x-forwarded-for", "garbage".parse().unwrap());
        assert_eq!(client_ip(proxy, &h, &trusted), proxy);
    }

    /// L-S4: a proxy that appends its own `X-Forwarded-For` line after the client's: the last line counts.
    #[test]
    fn every_forwarded_for_line_counts() {
        let trusted = vec![Cidr::parse("127.0.0.1").unwrap()];
        let proxy: IpAddr = "127.0.0.1".parse().unwrap();
        let mut h = HeaderMap::new();
        h.append("x-forwarded-for", "6.6.6.6".parse().unwrap());
        h.append("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(client_ip(proxy, &h, &trusted), "203.0.113.9".parse::<IpAddr>().unwrap());
        h.append("x-forwarded-for", "127.0.0.1, 198.51.100.4, 127.0.0.1".parse().unwrap());
        assert_eq!(
            client_ip(proxy, &h, &trusted),
            "198.51.100.4".parse::<IpAddr>().unwrap()
        );
    }

    /// M5: one IPv6 /64 is one client; IPv4 addresses stay themselves unless the operator widens them.
    #[test]
    fn client_keys_group_a_prefix() {
        let k = |s: &str, v4, v6| client_key(s.parse().unwrap(), v4, v6).to_string();
        assert_eq!(k("2001:db8:7:7::1", 32, 64), "2001:db8:7:7::");
        assert_eq!(k("2001:db8:7:7:ffff:1:2:3", 32, 64), "2001:db8:7:7::");
        assert_eq!(k("2001:db8:7:8::1", 32, 64), "2001:db8:7:8::");
        assert_eq!(k("2001:db8:7:7::1", 32, 128), "2001:db8:7:7::1");
        assert_eq!(k("198.51.100.20", 32, 64), "198.51.100.20");
        assert_eq!(k("198.51.100.20", 24, 64), "198.51.100.0");
        assert_eq!(k("::ffff:198.51.100.20", 32, 64), "198.51.100.20");
    }
}

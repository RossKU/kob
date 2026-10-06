//! Proxy-aware client identity.
//!
//! The socket peer is the client unless it is a trusted proxy (the operator's reverse proxy or
//! CDN). Only then is the forwarding header consulted, and for `X-Forwarded-For` the right-most
//! address that is not itself a trusted proxy is used: everything to its left was supplied by the
//! client or by earlier hops and can be spoofed. Any malformed or missing header falls back to the
//! peer address.

use axum::http::HeaderMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are turned into plain IPv4.
pub fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// The key clients are rate limited by: IPv4 as is, IPv6 collapsed to its first `prefix_bits` bits (64: one subscriber /64;
/// 48 groups a whole site, for operators facing an attacker that owns a larger prefix and rotates addresses through it).
pub fn rate_key(ip: IpAddr, prefix_bits: u8) -> IpAddr {
    match unmap(ip) {
        IpAddr::V6(v6) => {
            let bits = u32::from(prefix_bits.clamp(16, 128));
            let mask = if bits >= 128 { !0u128 } else { !0u128 << (128 - bits) };
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
        }
        v4 => v4,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse `a.b.c.d/len`, `x::y/len`, or a bare address (a host route).
    pub fn parse(s: &str) -> Result<Cidr, String> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.parse().map_err(|_| format!("invalid address in `{s}`"))?;
        let addr = unmap(addr);
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) => p.parse::<u8>().map_err(|_| format!("invalid prefix length in `{s}`"))?,
        };
        if prefix > max {
            return Err(format!("prefix length {prefix} too large in `{s}`"));
        }
        Ok(Cidr { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, unmap(ip)) {
            (IpAddr::V4(n), IpAddr::V4(a)) => mask_eq_v4(n, a, self.prefix),
            (IpAddr::V6(n), IpAddr::V6(a)) => mask_eq_v6(n, a, self.prefix),
            _ => false,
        }
    }
}

fn mask_eq_v4(n: Ipv4Addr, a: Ipv4Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
    u32::from(n) & mask == u32::from(a) & mask
}

fn mask_eq_v6(n: Ipv6Addr, a: Ipv6Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix as u32) };
    u128::from(n) & mask == u128::from(a) & mask
}

#[derive(Debug, Clone, Default)]
pub struct TrustedProxies {
    nets: Vec<Cidr>,
}

impl TrustedProxies {
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        Ok(TrustedProxies { nets: entries.iter().map(|e| Cidr::parse(e)).collect::<Result<_, _>>()? })
    }

    pub fn is_trusted(&self, ip: IpAddr) -> bool {
        self.nets.iter().any(|n| n.contains(ip))
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ClientIpResolver {
    proxies: TrustedProxies,
    header: String,
}

impl ClientIpResolver {
    pub fn new(proxies: TrustedProxies, header: &str) -> Self {
        ClientIpResolver { proxies, header: header.trim().to_ascii_lowercase() }
    }

    /// The address to attribute a request to.
    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = unmap(peer);
        if self.proxies.is_empty() || !self.proxies.is_trusted(peer) {
            return peer;
        }
        let values: Vec<&str> = headers.get_all(self.header.as_str()).iter().filter_map(|v| v.to_str().ok()).collect();
        if values.is_empty() {
            return peer;
        }
        if self.header == "x-forwarded-for" {
            // Right to left across all header lines (a proxy may append its own line).
            let entries: Vec<&str> = values.iter().flat_map(|v| v.split(',')).map(str::trim).collect();
            for e in entries.iter().rev() {
                match parse_ip(e) {
                    None => return peer,
                    Some(ip) if self.proxies.is_trusted(ip) => continue,
                    Some(ip) => return ip,
                }
            }
            peer
        } else if values.len() == 1 {
            parse_ip(values[0].trim()).unwrap_or(peer)
        } else {
            peer
        }
    }
}

/// `1.2.3.4`, `::1`, or `[::1]`; ports are not accepted (a malformed entry must not be trusted).
fn parse_ip(s: &str) -> Option<IpAddr> {
    let s = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(s);
    s.parse::<IpAddr>().ok().map(unmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn resolver(proxies: &[&str], header: &str) -> ClientIpResolver {
        let p = TrustedProxies::parse(&proxies.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        ClientIpResolver::new(p, header)
    }

    fn hdrs(name: &'static str, values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(name, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn cidr_v4_v6() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains(ip("10.255.1.1")));
        assert!(!c.contains(ip("11.0.0.1")));
        assert!(!c.contains(ip("::1")));
        let host = Cidr::parse("192.168.1.5").unwrap();
        assert!(host.contains(ip("192.168.1.5")));
        assert!(!host.contains(ip("192.168.1.6")));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains(ip("2001:db8:1::5")));
        assert!(!v6.contains(ip("2001:db9::1")));
        // v4-mapped addresses match v4 networks
        assert!(c.contains(ip("::ffff:10.1.2.3")));
        let all = Cidr::parse("0.0.0.0/0").unwrap();
        assert!(all.contains(ip("8.8.8.8")));
        assert!(Cidr::parse("10.0.0.0/33").is_err());
        assert!(Cidr::parse("nonsense").is_err());
        assert!(Cidr::parse("::/129").is_err());
    }

    #[test]
    fn untrusted_peer_ignores_header() {
        let r = resolver(&["10.0.0.0/8"], "x-forwarded-for");
        let h = hdrs("x-forwarded-for", &["1.2.3.4"]);
        assert_eq!(r.resolve(ip("203.0.113.9"), &h), ip("203.0.113.9"));
    }

    #[test]
    fn no_trusted_proxies_means_peer() {
        let r = resolver(&[], "x-forwarded-for");
        let h = hdrs("x-forwarded-for", &["1.2.3.4"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
    }

    #[test]
    fn xff_rightmost_untrusted_wins_over_spoofed_left() {
        let r = resolver(&["10.0.0.0/8"], "x-forwarded-for");
        // client spoofed 9.9.9.9, real client 198.51.100.7 appended by the trusted proxy chain
        let h = hdrs("x-forwarded-for", &["9.9.9.9, 198.51.100.7, 10.0.0.2"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("198.51.100.7"));
        // multiple header lines
        let h = hdrs("x-forwarded-for", &["9.9.9.9", "198.51.100.7, 10.0.0.2"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("198.51.100.7"));
    }

    #[test]
    fn xff_malformed_or_missing_falls_back() {
        let r = resolver(&["10.0.0.0/8"], "x-forwarded-for");
        assert_eq!(r.resolve(ip("10.0.0.1"), &HeaderMap::new()), ip("10.0.0.1"));
        let h = hdrs("x-forwarded-for", &["198.51.100.7, garbage"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
        let h = hdrs("x-forwarded-for", &["10.0.0.5, 10.0.0.6"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
        let h = hdrs("x-forwarded-for", &["198.51.100.7:1234"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
    }

    #[test]
    fn single_value_header() {
        let r = resolver(&["10.0.0.1"], "CF-Connecting-IP");
        let h = hdrs("cf-connecting-ip", &["198.51.100.7"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("198.51.100.7"));
        let h = hdrs("cf-connecting-ip", &["bad"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
        let h = hdrs("cf-connecting-ip", &["1.1.1.1", "2.2.2.2"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
        let h = hdrs("cf-connecting-ip", &["[2001:db8::1]"]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("2001:db8::1"));
    }

    #[test]
    fn v6_prefix_is_configurable() {
        let (a, b) = (ip("2001:db8:1:2::1"), ip("2001:db8:1:3::1"));
        assert_ne!(rate_key(a, 64), rate_key(b, 64));
        assert_eq!(rate_key(a, 48), rate_key(b, 48), "one /48 site is one client");
        assert_ne!(rate_key(a, 48), rate_key(ip("2001:db8:2:2::1"), 48));
        assert_eq!(rate_key(a, 128), a, "a full prefix keeps the address");
    }

    #[test]
    fn v6_keys_collapse_to_64() {
        assert_eq!(rate_key(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd"), 64), rate_key(ip("2001:db8:1:2::1"), 64));
        assert_ne!(rate_key(ip("2001:db8:1:3::1"), 64), rate_key(ip("2001:db8:1:2::1"), 64));
        assert_eq!(rate_key(ip("::ffff:1.2.3.4"), 64), ip("1.2.3.4"));
    }
}

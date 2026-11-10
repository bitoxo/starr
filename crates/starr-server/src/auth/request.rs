//! Facts about the incoming request that security decisions rely on: client
//! IP (throttling), scheme (cookie names and flags) and origin (CSRF).

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::{HeaderMap, header};
use url::Url;

static WARNED_NO_FORWARDED_FOR: AtomicBool = AtomicBool::new(false);
static WARNED_UNTRUSTED_PEER: AtomicBool = AtomicBool::new(false);

const FORWARDED_HEADERS: [&str; 2] = ["x-forwarded-for", "x-forwarded-proto"];

/// Whether a socket peer may be the reverse proxy whose `X-Forwarded-*`
/// headers Starr trusts with `STARR_BEHIND_PROXY=true`: loopback, private
/// IPv4 (RFC 1918), link-local and IPv6 unique local addresses. That covers
/// a proxy on the host or in a Docker network; a client on the internet that
/// reaches the port directly is none of these.
#[must_use]
pub fn is_trusted_proxy_peer(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Remove `X-Forwarded-For` and `X-Forwarded-Proto` unless they come from a
/// trusted proxy peer (see [`is_trusted_proxy_peer`]); without a known peer
/// they are removed too. Called for every request when Starr is behind a
/// proxy, before anything reads them.
pub fn drop_untrusted_forwarded(headers: &mut HeaderMap, peer: Option<IpAddr>) {
    if peer.is_some_and(is_trusted_proxy_peer) {
        return;
    }
    let mut dropped = false;
    for name in FORWARDED_HEADERS {
        dropped |= headers.remove(name).is_some();
    }
    if dropped && !WARNED_UNTRUSTED_PEER.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            peer = ?peer,
            "STARR_BEHIND_PROXY is set but X-Forwarded-* came from a peer that is not a loopback or private address; ignored (publish port 7979 only to the proxy)"
        );
    }
}

/// The client IP used for throttling and session records.
///
/// Without a proxy this is the socket peer. With `STARR_BEHIND_PROXY=true`
/// Starr trusts exactly one proxy hop (and only from a loopback or private
/// peer: [`drop_untrusted_forwarded`] removes the header otherwise): the proxy appends the address it saw to
/// `X-Forwarded-For`, so the **rightmost** entry is the only one the client
/// cannot forge; everything left of it is client-supplied and ignored. Multiple
/// `X-Forwarded-For` headers are read as one list in order. An entry may carry
/// a port (`203.0.113.7:4711`, `[2001:db8::1]:4711`). If the header is missing
/// or its rightmost entry is not an address, the socket peer is used and a
/// warning is logged once (the proxy is probably misconfigured).
#[must_use]
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr, behind_proxy: bool) -> IpAddr {
    if behind_proxy {
        if let Some(ip) = rightmost_forwarded_for(headers) {
            return ip;
        }
        if !WARNED_NO_FORWARDED_FOR.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "STARR_BEHIND_PROXY is set but a request had no usable X-Forwarded-For; using the socket address (check the proxy configuration)"
            );
        }
    }
    peer.ip().to_canonical()
}

fn rightmost_forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    let last = rightmost_entry(headers, "x-forwarded-for")?;
    parse_forwarded_addr(last).map(|ip| ip.to_canonical())
}

/// Rightmost non-empty entry of a comma-separated header that may be repeated.
fn rightmost_entry<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .rfind(|entry| !entry.is_empty())
}

/// `ip`, `ipv4:port`, `[ipv6]` or `[ipv6]:port`.
fn parse_forwarded_addr(entry: &str) -> Option<IpAddr> {
    if let Some(rest) = entry.strip_prefix('[') {
        let (inner, _) = rest.split_once(']')?;
        return inner.parse().ok();
    }
    if let Ok(ip) = entry.parse() {
        return Some(ip);
    }
    let (host, port) = entry.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    host.parse::<std::net::Ipv4Addr>().ok().map(IpAddr::V4)
}

/// Key for per-IP login throttling. IPv6 clients usually control a whole
/// /64, so IPv6 addresses are grouped by that prefix; IPv4 stays exact.
#[must_use]
pub fn throttle_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        IpAddr::V4(v4) => v4.to_string(),
    }
}

/// Whether the browser talks to us over https. Starr itself serves plain
/// http, so this is only true behind a proxy that says so in the rightmost
/// `X-Forwarded-Proto` entry.
#[must_use]
pub fn is_https(headers: &HeaderMap, behind_proxy: bool) -> bool {
    behind_proxy
        && rightmost_entry(headers, "x-forwarded-proto")
            .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

/// Whether a mutating request's `Origin` is allowed (CSRF defence).
///
/// `Origin` must equal the origin of `STARR_PUBLIC_URL` or of one of
/// `STARR_ADDITIONAL_ORIGINS`. Without a public URL (development only; setup
/// refuses to run) an `Origin` naming the same host and port as the `Host`
/// header is accepted too. A missing or `null` `Origin` is rejected: browsers
/// always send it on `POST`, and non-browser clients can set it.
#[must_use]
pub fn origin_allowed(
    headers: &HeaderMap,
    public_url: Option<&Url>,
    additional_origins: &[Url],
) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if public_url
        .into_iter()
        .chain(additional_origins)
        .any(|url| origin == url.origin().ascii_serialization())
    {
        return true;
    }
    public_url.is_none() && same_host(headers, origin)
}

fn same_host(headers: &HeaderMap, origin: &str) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = Url::parse(origin) else {
        return false;
    };
    let Some(origin_host) = origin.host_str() else {
        return false;
    };
    let authority = match origin.port() {
        Some(port) => format!("{origin_host}:{port}"),
        None => origin_host.to_owned(),
    };
    matches!(origin.scheme(), "http" | "https") && authority.eq_ignore_ascii_case(host)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn peer() -> SocketAddr {
        "10.0.0.2:51000".parse().unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn socket_peer_is_used_without_proxy_even_if_header_is_sent() {
        let h = headers(&[("x-forwarded-for", "203.0.113.7")]);
        assert_eq!(client_ip(&h, peer(), false), ip("10.0.0.2"));
    }

    #[test]
    fn rightmost_forwarded_entry_is_used_behind_proxy() {
        let h = headers(&[("x-forwarded-for", "1.1.1.1, 203.0.113.7")]);
        assert_eq!(client_ip(&h, peer(), true), ip("203.0.113.7"));
        // Spoofed header plus the proxy's own header: still the rightmost.
        let h = headers(&[
            ("x-forwarded-for", "6.6.6.6"),
            ("x-forwarded-for", "198.51.100.4"),
        ]);
        assert_eq!(client_ip(&h, peer(), true), ip("198.51.100.4"));
    }

    #[test]
    fn forwarded_entries_may_carry_ports_and_brackets() {
        for (entry, expected) in [
            ("203.0.113.7:4711", "203.0.113.7"),
            ("[2001:db8::1]", "2001:db8::1"),
            ("[2001:db8::1]:4711", "2001:db8::1"),
            ("2001:db8::1", "2001:db8::1"),
        ] {
            let mut h = HeaderMap::new();
            h.insert("x-forwarded-for", HeaderValue::from_str(entry).unwrap());
            assert_eq!(client_ip(&h, peer(), true), ip(expected), "{entry}");
        }
    }

    #[test]
    fn invalid_or_missing_forwarded_for_falls_back_to_peer() {
        for h in [
            headers(&[]),
            headers(&[("x-forwarded-for", "1.1.1.1, garbage")]),
            headers(&[("x-forwarded-for", "1.1.1.1:notaport")]),
        ] {
            assert_eq!(client_ip(&h, peer(), true), ip("10.0.0.2"));
        }
    }

    #[test]
    fn ipv4_mapped_addresses_are_canonical() {
        let h = headers(&[("x-forwarded-for", "::ffff:203.0.113.7")]);
        assert_eq!(client_ip(&h, peer(), true), ip("203.0.113.7"));
    }

    #[test]
    fn only_loopback_and_private_peers_are_trusted_as_proxies() {
        for trusted in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.10",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fc12::1",
            "fe80::1",
            "::ffff:172.18.0.5",
        ] {
            assert!(is_trusted_proxy_peer(ip(trusted)), "{trusted}");
        }
        for untrusted in [
            "203.0.113.7",
            "172.32.0.1",
            "100.64.0.1",
            "8.8.8.8",
            "2001:db8::1",
            "::ffff:203.0.113.7",
        ] {
            assert!(!is_trusted_proxy_peer(ip(untrusted)), "{untrusted}");
        }
    }

    #[test]
    fn forwarded_headers_from_untrusted_peers_are_dropped() {
        let mut h = headers(&[
            ("x-forwarded-for", "198.51.100.4"),
            ("x-forwarded-proto", "https"),
        ]);
        drop_untrusted_forwarded(&mut h, Some(ip("10.0.0.2")));
        assert!(h.contains_key("x-forwarded-for") && h.contains_key("x-forwarded-proto"));
        drop_untrusted_forwarded(&mut h, Some(ip("203.0.113.9")));
        assert!(!h.contains_key("x-forwarded-for") && !h.contains_key("x-forwarded-proto"));
        let mut h = headers(&[("x-forwarded-for", "198.51.100.4")]);
        drop_untrusted_forwarded(&mut h, None);
        assert!(!h.contains_key("x-forwarded-for"));
    }

    #[test]
    fn ipv6_throttle_keys_cover_the_slash_64() {
        assert_eq!(
            throttle_key(ip("2001:db8:1:2:aaaa::1")),
            "2001:db8:1:2::/64"
        );
        assert_eq!(
            throttle_key(ip("2001:db8:1:2:bbbb::9")),
            "2001:db8:1:2::/64"
        );
        assert_eq!(throttle_key(ip("203.0.113.7")), "203.0.113.7");
    }

    #[test]
    fn https_only_when_the_trusted_proxy_says_so() {
        let h = headers(&[("x-forwarded-proto", "http, https")]);
        assert!(is_https(&h, true));
        assert!(!is_https(&h, false));
        let h = headers(&[("x-forwarded-proto", "https, http")]);
        assert!(!is_https(&h, true));
        assert!(!is_https(&headers(&[]), true));
    }

    #[test]
    fn origin_must_match_public_url_or_additional_origins() {
        let url = Url::parse("https://starr.example.com").unwrap();
        let extra = [Url::parse("http://192.168.1.10:7979").unwrap()];
        for ok in ["https://starr.example.com", "http://192.168.1.10:7979"] {
            let h = headers(&[("origin", ok)]);
            assert!(origin_allowed(&h, Some(&url), &extra), "{ok}");
        }
        for origin in [
            "https://evil.example.com",
            "http://starr.example.com",
            "https://starr.example.com:8443",
            "http://192.168.1.10",
            "null",
        ] {
            let h = headers(&[("origin", origin), ("host", "starr.example.com")]);
            assert!(!origin_allowed(&h, Some(&url), &extra), "{origin}");
        }
        assert!(!origin_allowed(&headers(&[]), Some(&url), &extra));
    }

    #[test]
    fn without_public_url_origin_must_match_host() {
        let ok = headers(&[
            ("origin", "http://localhost:5173"),
            ("host", "localhost:5173"),
        ]);
        assert!(origin_allowed(&ok, None, &[]));
        let other_port = headers(&[
            ("origin", "http://localhost:5173"),
            ("host", "localhost:7979"),
        ]);
        assert!(!origin_allowed(&other_port, None, &[]));
        let foreign = headers(&[
            ("origin", "https://evil.example"),
            ("host", "localhost:7979"),
        ]);
        assert!(!origin_allowed(&foreign, None, &[]));
        let no_host = headers(&[("origin", "http://localhost:7979")]);
        assert!(!origin_allowed(&no_host, None, &[]));
    }
}

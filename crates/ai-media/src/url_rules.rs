//! Which URLs a provider may point us at.
//!
//! A hosted provider hands back a link; the link is untrusted input. We follow it only when it is
//! https (plain http only for a loopback host the caller has allowed), carries no user-info, and
//! does not name a private, link-local or otherwise internal address. Names are not resolved
//! here, so a hostname that later resolves to an internal address is not caught; the transfer
//! still sends no credentials and only keeps bytes that look like real media.
//!
//! Names are compared without a trailing dot (`localhost.` is `localhost`), and an IPv6 address
//! that carries an IPv4 one (mapped, compatible, NAT64, 6to4) is judged by the IPv4 inside it.

use craft_ai_core::{AiError, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use url::{Host, Url};

fn unsafe_link(why: &str) -> AiError {
    AiError::new(ErrorKind::Provider, format!("media link refused: {why}"))
}

/// Checks one URL. `allow_loopback` permits `http` and `https` to a loopback host.
pub fn check(url: &Url, allow_loopback: bool) -> Result<(), AiError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(unsafe_link("it carries credentials"));
    }
    let loopback = is_loopback(url);
    match url.scheme() {
        "https" => {}
        "http" if allow_loopback && loopback => {}
        "http" => return Err(unsafe_link("it is not https")),
        _ => return Err(unsafe_link("it is not an http link")),
    }
    if loopback {
        return if allow_loopback {
            Ok(())
        } else {
            Err(unsafe_link("it points at this computer"))
        };
    }
    match url.host() {
        Some(Host::Ipv4(ip)) if internal_v4(ip) => {
            Err(unsafe_link("it points at a private address"))
        }
        Some(Host::Ipv6(ip)) if internal_v6(ip) => {
            Err(unsafe_link("it points at a private address"))
        }
        Some(Host::Domain(name)) if private_name(name) => {
            Err(unsafe_link("it points at a private name"))
        }
        Some(_) => Ok(()),
        None => Err(unsafe_link("it has no host")),
    }
}

/// [`check`], and for a host name also looks it up and refuses a name that points at this
/// computer or a private network (`evil.example` with an A record of `127.0.0.1`).
///
/// This is a look before the connection, not a guarantee: a server that answers differently the
/// second time (DNS rebinding) is not caught. It stops the ordinary case, a name set up to aim a
/// provider's link at something inside the network. A lookup that fails is refused too.
pub fn check_resolved(url: &Url, allow_loopback: bool) -> Result<(), AiError> {
    check(url, allow_loopback)?;
    let Some(Host::Domain(name)) = url.host() else {
        return Ok(());
    };
    // A loopback name was already judged (and allowed or refused) by `check`.
    if is_loopback(url) {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = (name, port)
        .to_socket_addrs()
        .map_err(|_| AiError::new(ErrorKind::Network, "could not look up the media host"))?;
    if addrs.map(|a| a.ip()).any(internal_addr) {
        return Err(unsafe_link("its name points at a private address"));
    }
    Ok(())
}

fn internal_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => internal_v4(v4),
        IpAddr::V6(v6) => internal_v6(v6),
    }
}

/// Resolves a `Location` header against the current link. A redirect may not leave https; the
/// caller still runs [`check`] on the result before connecting.
pub fn redirect_target(current: &Url, location: &str) -> Result<Url, AiError> {
    let next = current
        .join(location)
        .map_err(|_| AiError::new(ErrorKind::Provider, "media link redirects nowhere"))?;
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(unsafe_link("a redirect leaves https"));
    }
    Ok(next)
}

/// A host name without its root dot: `localhost.` and `localhost` are the same name.
fn bare(name: &str) -> &str {
    name.trim_end_matches('.')
}

/// Names that only mean something on a private network.
fn private_name(name: &str) -> bool {
    let name = bare(name).to_ascii_lowercase();
    [
        "local",
        "internal",
        "localdomain",
        "lan",
        "intranet",
        "corp",
        "home",
        "home.arpa",
        "private",
    ]
    .iter()
    .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => {
            let name = bare(name).to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost")
        }
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback() || embedded_v4(ip).is_some_and(|v4| v4.is_loopback())
        }
        None => false,
    }
}

/// The IPv4 address an IPv6 one carries, for the forms that route to it: IPv4-mapped
/// (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::/96`) and 6to4
/// (`2002:aabb:ccdd::/48`).
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = ip.segments();
    let from = |hi: u16, lo: u16| {
        let [a, b] = hi.to_be_bytes();
        let [c, d] = lo.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    // `::` and `::1` are handled by the loopback and unspecified checks.
    let compatible = s[..6].iter().all(|x| *x == 0) && (s[6] != 0 || s[7] > 1);
    let nat64 = s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|x| *x == 0);
    if compatible || nat64 {
        Some(from(s[6], s[7]))
    } else if s[0] == 0x2002 {
        Some(from(s[1], s[2]))
    } else {
        None
    }
}

fn internal_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        || (a == 198 && (18..20).contains(&b))
        || a == 0
        || a >= 240
}

fn internal_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return internal_v4(v4);
    }
    let [first, second, ..] = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        // Documentation (2001:db8::/32) and Teredo (2001::/32) have no business as media hosts.
        || (first == 0x2001 && (second == 0xdb8 || second == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str, loopback: bool) -> bool {
        check(&Url::parse(url).unwrap(), loopback).is_ok()
    }

    #[test]
    fn ordinary_https_links_pass() {
        assert!(ok("https://cdn.example.com/a.png?sig=abc", false));
        assert!(ok("https://8.8.8.8/a.png", false));
        assert!(!ok("https://203.0.113.9/a.png", false)); // documentation range
    }

    #[test]
    fn plain_http_and_other_schemes_are_refused() {
        assert!(!ok("http://cdn.example.com/a.png", false));
        assert!(!ok("http://cdn.example.com/a.png", true));
        assert!(!ok("ftp://cdn.example.com/a.png", false));
        assert!(!ok("file:///etc/passwd", true));
    }

    #[test]
    fn a_redirect_resolves_relative_links_and_never_leaves_https() {
        let here = Url::parse("https://cdn.example.com/a/b.png").unwrap();
        assert_eq!(
            redirect_target(&here, "/c.png").unwrap().as_str(),
            "https://cdn.example.com/c.png"
        );
        assert!(redirect_target(&here, "http://cdn.example.com/c.png").is_err());
        assert!(redirect_target(&here, "http://127.0.0.1/c.png").is_err());
        assert!(redirect_target(&here, "").is_ok()); // same link; the hop limit ends any loop
        let plain = Url::parse("http://127.0.0.1/a").unwrap();
        assert!(redirect_target(&plain, "https://cdn.example.com/c.png").is_ok());
    }

    #[test]
    fn credentials_in_the_link_are_refused() {
        assert!(!ok("https://user:pw@cdn.example.com/a.png", false));
        assert!(!ok("https://user@cdn.example.com/a.png", false));
    }

    #[test]
    fn internal_addresses_are_refused() {
        for u in [
            "https://127.0.0.1/a",
            "https://localhost/a",
            "https://[::1]/a",
            "https://10.0.0.5/a",
            "https://192.168.1.2/a",
            "https://172.16.0.1/a",
            "https://169.254.169.254/latest/meta-data",
            "https://100.64.0.1/a",
            "https://0.0.0.0/a",
            "https://[fd00::1]/a",
            "https://[fe80::1]/a",
            "https://[::ffff:10.0.0.1]/a",
            "https://printer.local/a",
            "https://db.internal/a",
            // A root dot is the same name.
            "https://localhost./a",
            "https://LocalHost./a",
            "https://metadata.google.internal./a",
            "https://printer.local./a",
            "https://nas.lan/a",
            "https://foo.localhost./a",
            // IPv6 forms that carry an IPv4 address.
            "https://[::7f00:1]/a",
            "https://[::a00:1]/a",
            "https://[64:ff9b::a9fe:a9fe]/a",
            "https://[64:ff9b::7f00:1]/a",
            "https://[2002:a9fe:a9fe::1]/a",
            "https://[2002:7f00:1::1]/a",
            "https://[::ffff:7f00:1]/a",
            "https://[fec0::1]/a",
            "https://[ff02::1]/a",
            "https://[2001:db8::1]/a",
            "https://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/a",
            // Other reserved IPv4 ranges.
            "https://192.0.0.1/a",
            "https://198.18.0.1/a",
            "https://224.0.0.1/a",
        ] {
            assert!(!ok(u, false), "{u}");
        }
    }

    #[test]
    fn an_address_a_name_resolves_to_is_judged_like_a_literal_one() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "::1",
            "fd00::1",
            "::ffff:192.168.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(internal_addr(bad.parse().unwrap()), "{bad}");
        }
        for good in ["8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!internal_addr(good.parse().unwrap()), "{good}");
        }
        // Addresses and loopback names need no lookup.
        assert!(check_resolved(&Url::parse("https://8.8.8.8/a").unwrap(), false).is_ok());
        assert!(check_resolved(&Url::parse("http://localhost:8080/a").unwrap(), true).is_ok());
        assert!(check_resolved(&Url::parse("https://localhost/a").unwrap(), false).is_err());
    }

    #[test]
    fn public_addresses_inside_the_wrapped_forms_still_pass() {
        // The IPv4 inside decides, so a public one stays reachable.
        for u in [
            "https://[64:ff9b::808:808]/a",
            "https://[2002:808:808::1]/a",
            "https://[2606:4700:4700::1111]/a",
            "https://notlocalhost.example.com/a",
            "https://localhost.example.com/a",
            "https://corporate.example.com/a",
            "https://cdn.homepage.example/a",
        ] {
            assert!(ok(u, false), "{u}");
        }
    }

    #[test]
    fn loopback_is_only_allowed_when_the_caller_says_so() {
        assert!(ok("http://127.0.0.1:8080/a.png", true));
        assert!(ok("http://localhost:8080/a.png", true));
        assert!(ok("http://localhost.:8080/a.png", true));
        assert!(!ok("http://127.0.0.1:8080/a.png", false));
        // Allowing loopback does not allow the rest of the private ranges.
        assert!(!ok("http://10.0.0.5/a.png", true));
        assert!(!ok("https://169.254.169.254/a", true));
    }
}

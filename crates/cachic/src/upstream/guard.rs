//! Upstream address guard.
//!
//! cachic accepts requests for *any* `Host` and fetches them. Without a guard that makes it an
//! open proxy on the LAN: anyone could point it at `169.254.169.254`, a router's admin interface,
//! or a neighbour's NAS, and have the cache fetch and then serve the result (FR-64, NFR-10).
//!
//! The check is on the *resolved address*, not the hostname, because a hostname is attacker
//! controlled and DNS can return anything. `evil.example.com A 192.168.1.1` is the whole attack.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why an upstream address was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("loopback address")]
    Loopback,
    #[error("private address")]
    Private,
    #[error("link-local address")]
    LinkLocal,
    #[error("unspecified address")]
    Unspecified,
    #[error("multicast or broadcast address")]
    MulticastOrBroadcast,
    #[error("reserved or otherwise non-global address")]
    Reserved,
}

impl Refusal {
    /// Every reason, so the metric can publish them at zero before any refusal happens.
    pub const ALL: &'static [&'static str] = &[
        "loopback",
        "private",
        "link_local",
        "unspecified",
        "multicast_or_broadcast",
        "reserved",
    ];

    /// A stable, closed label value. The `Display` text is prose and would make an unstable
    /// metric label.
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::Loopback => "loopback",
            Refusal::Private => "private",
            Refusal::LinkLocal => "link_local",
            Refusal::Unspecified => "unspecified",
            Refusal::MulticastOrBroadcast => "multicast_or_broadcast",
            Refusal::Reserved => "reserved",
        }
    }
}

/// Whether an address may be used as an upstream.
///
/// `allow_private` corresponds to an operator deliberately pointing the cache at an internal
/// mirror. It is off by default and should stay that way.
pub fn check(addr: IpAddr, allow_private: bool) -> Result<(), Refusal> {
    if allow_private {
        return Ok(());
    }
    match addr {
        IpAddr::V4(v4) => check_v4(v4),
        IpAddr::V6(v6) => check_v6(v6),
    }
}

fn check_v4(a: Ipv4Addr) -> Result<(), Refusal> {
    if a.is_unspecified() {
        return Err(Refusal::Unspecified);
    }
    if a.is_loopback() {
        return Err(Refusal::Loopback);
    }
    if a.is_private() {
        return Err(Refusal::Private);
    }
    if a.is_link_local() {
        // Includes 169.254.169.254, the cloud metadata endpoint.
        return Err(Refusal::LinkLocal);
    }
    if a.is_multicast() || a.is_broadcast() {
        return Err(Refusal::MulticastOrBroadcast);
    }
    let [o0, o1, ..] = a.octets();
    // Carrier-grade NAT (100.64.0.0/10) and shared address space: not the public internet.
    if o0 == 100 && (64..128).contains(&o1) {
        return Err(Refusal::Reserved);
    }
    // 0.0.0.0/8, 192.0.0.0/24, 192.0.2.0/24, 198.18.0.0/15, 198.51.100.0/24, 203.0.113.0/24,
    // and 240.0.0.0/4 are all documentation, benchmarking or reserved ranges.
    if o0 == 0 || o0 >= 240 {
        return Err(Refusal::Reserved);
    }
    let octets = a.octets();
    let reserved = [
        ([192, 0, 0], 24u8),
        ([192, 0, 2], 24),
        ([198, 51, 100], 24),
        ([203, 0, 113], 24),
    ];
    for (prefix, _) in reserved {
        if octets[0] == prefix[0] && octets[1] == prefix[1] && octets[2] == prefix[2] {
            return Err(Refusal::Reserved);
        }
    }
    if octets[0] == 198 && (18..20).contains(&octets[1]) {
        return Err(Refusal::Reserved);
    }
    Ok(())
}

fn check_v6(a: Ipv6Addr) -> Result<(), Refusal> {
    if a.is_unspecified() {
        return Err(Refusal::Unspecified);
    }
    if a.is_loopback() {
        return Err(Refusal::Loopback);
    }
    if a.is_multicast() {
        return Err(Refusal::MulticastOrBroadcast);
    }
    // An IPv4-mapped address must be judged by its IPv4 rules, or ::ffff:192.168.1.1 walks
    // straight through the guard.
    if let Some(v4) = a.to_ipv4_mapped() {
        return check_v4(v4);
    }

    let segments = a.segments();

    // The mapped form is not the only one carrying an IPv4 address, and the rest went unjudged
    // (SR-08). Each of these embeds one, so each is re-checked by IPv4 rules:
    //
    //   ::a.b.c.d        deprecated IPv4-compatible. `::` and `::1` are already handled above.
    //   64:ff9b::/96     the well-known NAT64 prefix.
    //   2002::/16        6to4, where the address is in segments 1 and 2.
    //
    // Whether any of them is routable depends on the host's stack and on a NAT64 or 6to4 path
    // existing, so this is defence in depth rather than a live bypass. It is the same trick the
    // mapped case above exists to stop.
    let embedded_v4 = if segments[..6] == [0, 0, 0, 0, 0, 0] {
        // IPv4-compatible: the last 32 bits, with the all-zero and loopback cases already refused.
        Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        ))
    } else if segments[0] == 0x0064 && segments[1] == 0xff9b {
        Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        ))
    } else if segments[0] == 0x2002 {
        Some(Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        ))
    } else {
        None
    };
    if let Some(v4) = embedded_v4 {
        return check_v4(v4);
    }

    // Teredo. The embedded server address is not the destination, so there is nothing useful to
    // re-check; refuse the form outright rather than reason about it.
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return Err(Refusal::Reserved);
    }

    // fc00::/7 unique local.
    if segments[0] & 0xfe00 == 0xfc00 {
        return Err(Refusal::Private);
    }
    // fe80::/10 link local.
    if segments[0] & 0xffc0 == 0xfe80 {
        return Err(Refusal::LinkLocal);
    }
    // 2001:db8::/32 documentation.
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return Err(Refusal::Reserved);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(s: &str) -> Refusal {
        check(s.parse().unwrap(), false).expect_err(&format!("{s} should be refused"))
    }

    fn allowed(s: &str) {
        check(s.parse().unwrap(), false).unwrap_or_else(|e| panic!("{s} should be allowed: {e}"));
    }

    #[test]
    fn allows_ordinary_public_addresses() {
        for a in ["1.1.1.1", "8.8.8.8", "23.55.144.1", "2606:4700:4700::1111"] {
            allowed(a);
        }
    }

    #[test]
    fn refuses_rfc1918() {
        assert_eq!(refused("10.0.0.1"), Refusal::Private);
        assert_eq!(refused("172.16.0.1"), Refusal::Private);
        assert_eq!(refused("192.168.1.1"), Refusal::Private);
    }

    #[test]
    fn refuses_loopback_and_unspecified() {
        assert_eq!(refused("127.0.0.1"), Refusal::Loopback);
        assert_eq!(refused("0.0.0.0"), Refusal::Unspecified);
        assert_eq!(refused("::1"), Refusal::Loopback);
        assert_eq!(refused("::"), Refusal::Unspecified);
    }

    #[test]
    fn refuses_the_cloud_metadata_endpoint() {
        // The single most valuable target for an SSRF against a caching proxy.
        assert_eq!(refused("169.254.169.254"), Refusal::LinkLocal);
    }

    #[test]
    fn refuses_ipv4_mapped_private_addresses() {
        // ::ffff:192.168.1.1 must be judged by IPv4 rules, or the guard is trivially bypassed.
        assert_eq!(refused("::ffff:192.168.1.1"), Refusal::Private);
        assert_eq!(refused("::ffff:127.0.0.1"), Refusal::Loopback);
    }

    #[test]
    fn refuses_ipv6_unique_local_and_link_local() {
        assert_eq!(refused("fc00::1"), Refusal::Private);
        assert_eq!(refused("fd12:3456::1"), Refusal::Private);
        assert_eq!(refused("fe80::1"), Refusal::LinkLocal);
    }

    #[test]
    fn refuses_carrier_grade_nat_and_reserved_ranges() {
        assert_eq!(refused("100.64.0.1"), Refusal::Reserved);
        assert_eq!(refused("100.127.255.255"), Refusal::Reserved);
        assert_eq!(refused("240.0.0.1"), Refusal::Reserved);
        assert_eq!(refused("192.0.2.1"), Refusal::Reserved);
        assert_eq!(refused("198.51.100.1"), Refusal::Reserved);
        assert_eq!(refused("203.0.113.1"), Refusal::Reserved);
        assert_eq!(refused("198.18.0.1"), Refusal::Reserved);
        assert_eq!(refused("2001:db8::1"), Refusal::Reserved);
    }

    #[test]
    fn allows_addresses_adjacent_to_reserved_ranges() {
        // Off-by-one in a range check silently blocks real CDNs.
        allowed("100.63.255.255");
        allowed("100.128.0.1");
        allowed("198.17.255.255");
        allowed("198.20.0.1");
        allowed(
            "239.255.255.255"
                .parse::<Ipv4Addr>()
                .map(|_| "223.255.255.255")
                .unwrap(),
        );
    }

    #[test]
    fn refuses_multicast_and_broadcast() {
        assert_eq!(refused("224.0.0.1"), Refusal::MulticastOrBroadcast);
        assert_eq!(refused("255.255.255.255"), Refusal::MulticastOrBroadcast);
        assert_eq!(refused("ff02::1"), Refusal::MulticastOrBroadcast);
    }

    #[test]
    fn allow_private_opens_everything_deliberately() {
        // For an operator pointing the cache at an internal mirror. Off by default.
        for a in ["10.0.0.1", "127.0.0.1", "169.254.169.254", "fc00::1"] {
            check(a.parse().unwrap(), true).unwrap();
        }
    }
}

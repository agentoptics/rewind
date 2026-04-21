//! SSRF guard for user-supplied outbound URLs.
//!
//! See `docs/security-audit.md` §CRITICAL-01. Used by `export_otel` to reject
//! endpoints that resolve to private/reserved IP ranges (RFC 1918, link-local,
//! loopback, unspecified, multicast) before the outbound request is made.
//!
//! ## DNS rebinding
//!
//! This guard resolves the hostname once and validates the resulting IP(s).
//! The downstream HTTP client (opentelemetry-otlp via reqwest/tonic) may
//! re-resolve at connection time, so a malicious resolver could still rebind
//! to a private IP between validation and connection. Fully closing that gap
//! requires pinning the resolver, which isn't supported by the current
//! `opentelemetry-otlp` API surface. The remaining rebinding window is
//! documented as a known limitation; the single-resolution check still blocks
//! the common case of an attacker directly targeting `169.254.169.254` or
//! `localhost`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Reject an endpoint URL if it's malformed, non-HTTP(S), or resolves to any
/// non-public-unicast IP.
///
/// Returns the same URL on success so callers can chain. Returns a
/// user-facing error string on failure — safe to put in an HTTP response.
pub fn validate_export_endpoint(url: &str) -> Result<&str, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Endpoint must start with http:// or https://".to_string());
    }

    let (host, port) = parse_host_port(url)
        .ok_or_else(|| "Endpoint URL is malformed".to_string())?;

    // Refuse IP literals that are not public unicast. IP literals skip DNS
    // resolution entirely, so we validate the literal directly.
    if let Ok(ip) = host.parse::<IpAddr>() {
        reject_if_blocked(ip)?;
        return Ok(url);
    }

    // Resolve the hostname. Reject if any resolved IP is blocked — callers
    // shouldn't be talking to names that round-robin between public and
    // internal hosts.
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("Failed to resolve endpoint host '{host}': {e}"))?
        .collect();

    if addrs.is_empty() {
        return Err(format!("Endpoint host '{host}' resolved to no addresses"));
    }

    for addr in &addrs {
        reject_if_blocked(addr.ip())?;
    }

    Ok(url)
}

fn reject_if_blocked(ip: IpAddr) -> Result<(), String> {
    if is_blocked(ip) {
        return Err(format!(
            "Endpoint resolves to blocked IP {ip} (private/loopback/link-local/reserved). \
             SSRF protection: only public unicast targets are allowed. \
             See docs/security-audit.md §CRITICAL-01."
        ));
    }
    Ok(())
}

/// True when an IP is NOT a public-unicast address and should be refused.
fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    // Cover cloud metadata endpoints, local networks, loopback, etc.
    ip.is_private()                   // 10/8, 172.16/12, 192.168/16
        || ip.is_loopback()           // 127/8
        || ip.is_link_local()         // 169.254/16 (AWS/GCP/Azure metadata)
        || ip.is_unspecified()        // 0.0.0.0
        || ip.is_multicast()          // 224/4
        || ip.is_broadcast()          // 255.255.255.255
        || ip.is_documentation()      // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || is_shared_address_space(ip) // 100.64/10 (carrier-grade NAT)
        || is_benchmarking(ip)        // 198.18/15
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()                   // ::1
        || ip.is_unspecified()         // ::
        || ip.is_multicast()           // ff00::/8
        || is_unique_local_v6(ip)      // fc00::/7
        || is_link_local_v6(ip)        // fe80::/10
        || is_v4_mapped(ip)            // ::ffff:0:0/96 — check the embedded v4
        || is_v4_compatible_deprecated(ip) // ::/96 (deprecated but dangerous)
        || is_documentation_v6(ip)     // 2001:db8::/32
}

fn is_shared_address_space(ip: Ipv4Addr) -> bool {
    // 100.64.0.0/10 (RFC 6598)
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0b1100_0000) == 0b0100_0000
}

fn is_benchmarking(ip: Ipv4Addr) -> bool {
    // 198.18.0.0/15 (RFC 2544)
    let o = ip.octets();
    o[0] == 198 && (o[1] == 18 || o[1] == 19)
}

fn is_unique_local_v6(ip: Ipv6Addr) -> bool {
    // fc00::/7
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn is_link_local_v6(ip: Ipv6Addr) -> bool {
    // fe80::/10
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn is_v4_mapped(ip: Ipv6Addr) -> bool {
    // ::ffff:a.b.c.d — recurse into the embedded v4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    false
}

fn is_v4_compatible_deprecated(ip: Ipv6Addr) -> bool {
    // ::a.b.c.d (deprecated ipv4-compatible form). Covers ::1 loopback too,
    // but is_loopback catches that separately.
    let segs = ip.segments();
    if segs[0..6] == [0, 0, 0, 0, 0, 0] && segs[6] != 0 {
        // Don't double-reject ::1 (handled by is_loopback), but recurse on the
        // embedded v4 address for all others.
        let v4 = Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            segs[6] as u8,
            (segs[7] >> 8) as u8,
            segs[7] as u8,
        );
        return is_blocked_v4(v4);
    }
    false
}

fn is_documentation_v6(ip: Ipv6Addr) -> bool {
    // 2001:db8::/32
    let segs = ip.segments();
    segs[0] == 0x2001 && segs[1] == 0x0db8
}

/// Extract (host, port) from a URL string. Minimal parser — we only need host
/// and port, not a full URL crate dependency.
///
/// Returns None for malformed URLs (empty host, missing scheme separator, etc.).
fn parse_host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let (authority, _) = rest.split_once('/').unwrap_or((rest, ""));
    let authority = authority.split('?').next().unwrap_or(authority);
    let authority = authority.split('#').next().unwrap_or(authority);

    // Strip userinfo@ if present (rare but legal)
    let authority = match authority.rsplit_once('@') {
        Some((_, rest)) => rest,
        None => authority,
    };

    let default_port: u16 = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };

    // IPv6 literal: [::1]:port or [::1]
    if let Some(rest) = authority.strip_prefix('[') {
        let (addr, port_part) = rest.split_once(']')?;
        if addr.is_empty() {
            return None;
        }
        let port = if let Some(p) = port_part.strip_prefix(':') {
            p.parse::<u16>().ok()?
        } else if port_part.is_empty() {
            default_port
        } else {
            return None;
        };
        return Some((addr.to_string(), port));
    }

    // IPv4 or hostname
    if let Some((host, port_str)) = authority.rsplit_once(':') {
        // Make sure this isn't an IPv6 without brackets (which we refuse)
        if host.contains(':') {
            return None;
        }
        let port = port_str.parse::<u16>().ok()?;
        if host.is_empty() {
            return None;
        }
        return Some((host.to_string(), port));
    }

    if authority.is_empty() {
        return None;
    }
    Some((authority.to_string(), default_port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn blocked(s: &str) -> bool {
        is_blocked(IpAddr::from_str(s).unwrap())
    }

    #[test]
    fn blocks_cloud_metadata_v4() {
        assert!(blocked("169.254.169.254"), "AWS/GCP/Azure metadata");
    }

    #[test]
    fn blocks_rfc1918_ranges() {
        assert!(blocked("10.0.0.1"));
        assert!(blocked("10.255.255.255"));
        assert!(blocked("172.16.0.1"));
        assert!(blocked("172.31.255.254"));
        assert!(blocked("192.168.0.1"));
        assert!(blocked("192.168.255.255"));
    }

    #[test]
    fn blocks_loopback_and_unspecified() {
        assert!(blocked("127.0.0.1"));
        assert!(blocked("127.255.255.254"));
        assert!(blocked("0.0.0.0"));
        assert!(blocked("::1"));
        assert!(blocked("::"));
    }

    #[test]
    fn blocks_link_local() {
        assert!(blocked("169.254.0.1"));
        assert!(blocked("169.254.169.254"));
        assert!(blocked("fe80::1"));
    }

    #[test]
    fn blocks_multicast_and_broadcast() {
        assert!(blocked("224.0.0.1"));
        assert!(blocked("239.255.255.255"));
        assert!(blocked("255.255.255.255"));
        assert!(blocked("ff02::1"));
    }

    #[test]
    fn blocks_shared_and_benchmarking() {
        assert!(blocked("100.64.0.1"));
        assert!(blocked("100.127.255.254"));
        assert!(blocked("198.18.0.1"));
        assert!(blocked("198.19.255.254"));
    }

    #[test]
    fn blocks_unique_local_v6() {
        assert!(blocked("fc00::1"));
        assert!(blocked("fd00::1"));
    }

    #[test]
    fn blocks_v4_mapped_v6() {
        assert!(blocked("::ffff:127.0.0.1"));
        assert!(blocked("::ffff:169.254.169.254"));
        assert!(blocked("::ffff:10.0.0.1"));
    }

    #[test]
    fn allows_public_unicast_v4() {
        assert!(!blocked("8.8.8.8"));
        assert!(!blocked("1.1.1.1"));
        assert!(!blocked("151.101.1.1")); // Fastly
    }

    #[test]
    fn allows_public_unicast_v6() {
        assert!(!blocked("2606:4700:4700::1111")); // Cloudflare
        assert!(!blocked("2001:4860:4860::8888")); // Google
    }

    #[test]
    fn blocks_documentation_ranges() {
        assert!(blocked("192.0.2.1"));
        assert!(blocked("198.51.100.1"));
        assert!(blocked("203.0.113.1"));
        assert!(blocked("2001:db8::1"));
    }

    // ── URL parser ──

    #[test]
    fn parses_simple_http_url() {
        assert_eq!(parse_host_port("http://example.com").unwrap(), ("example.com".to_string(), 80));
        assert_eq!(parse_host_port("https://example.com").unwrap(), ("example.com".to_string(), 443));
    }

    #[test]
    fn parses_explicit_port() {
        assert_eq!(parse_host_port("http://example.com:8080").unwrap(), ("example.com".to_string(), 8080));
        assert_eq!(parse_host_port("https://example.com:4317/v1/traces").unwrap(), ("example.com".to_string(), 4317));
    }

    #[test]
    fn parses_ipv4_literal() {
        assert_eq!(parse_host_port("http://169.254.169.254/latest/").unwrap(), ("169.254.169.254".to_string(), 80));
    }

    #[test]
    fn parses_ipv6_literal_with_port() {
        assert_eq!(parse_host_port("http://[::1]:4317").unwrap(), ("::1".to_string(), 4317));
    }

    #[test]
    fn parses_ipv6_literal_no_port() {
        assert_eq!(parse_host_port("https://[2001:4860:4860::8888]/x").unwrap(), ("2001:4860:4860::8888".to_string(), 443));
    }

    #[test]
    fn strips_userinfo() {
        assert_eq!(parse_host_port("https://user:pass@example.com:8443/x").unwrap(), ("example.com".to_string(), 8443));
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_host_port("not-a-url").is_none());
        assert!(parse_host_port("ftp://example.com").is_none(), "only http/https allowed");
        assert!(parse_host_port("https://").is_none());
        assert!(parse_host_port("https://:8080").is_none());
    }

    // ── End-to-end ──

    #[test]
    fn e2e_rejects_aws_metadata_literal() {
        let err = validate_export_endpoint("http://169.254.169.254/latest/meta-data/")
            .expect_err("should reject AWS metadata");
        assert!(err.contains("169.254.169.254"), "error should name the IP: {err}");
        assert!(err.contains("SSRF"), "error should mention SSRF: {err}");
    }

    #[test]
    fn e2e_rejects_loopback() {
        assert!(validate_export_endpoint("http://127.0.0.1:9000").is_err());
        assert!(validate_export_endpoint("http://localhost:9000").is_err());
        assert!(validate_export_endpoint("http://[::1]:9000").is_err());
    }

    #[test]
    fn e2e_rejects_non_http_scheme() {
        let err = validate_export_endpoint("ftp://example.com").unwrap_err();
        assert!(err.contains("http://"), "error should mention scheme requirement: {err}");
    }

    #[test]
    fn e2e_rejects_rfc1918() {
        assert!(validate_export_endpoint("http://10.0.0.1:4318/v1/traces").is_err());
        assert!(validate_export_endpoint("https://192.168.1.1").is_err());
        assert!(validate_export_endpoint("http://172.16.5.5:4317").is_err());
    }

    #[test]
    fn e2e_rejects_malformed() {
        assert!(validate_export_endpoint("http://").is_err());
        assert!(validate_export_endpoint("not-a-url").is_err());
    }

    // Live DNS test: accepts a public hostname. Marked #[ignore] because CI
    // runs in sandboxes that may block DNS or the wider internet.
    #[test]
    #[ignore]
    fn e2e_allows_public_host() {
        validate_export_endpoint("https://example.com").unwrap();
    }
}

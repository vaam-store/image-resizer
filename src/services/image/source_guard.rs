//! SSRF guard for the source-image fetcher (#21).
//!
//! Everything here is pure/deterministic except [`resolve_validated_addr`],
//! which is the one function that touches the network (DNS). Keeping the
//! validation logic (scheme check, allowlist check, blocked-range check,
//! IP-literal decoding) as free functions makes it straightforward to unit
//! test every blocked range and every literal-encoding trick without
//! spinning up an HTTP server.
//!
//! Threat model covered:
//! - non-http(s) schemes (`file://`, `gopher://`, ...)
//! - loopback, link-local (v4 + v6), RFC1918, CGNAT, IPv6 unique-local,
//!   unspecified, and IPv4-mapped-IPv6 forms of all of the above
//! - decimal/octal/hex-encoded IPv4 literals used to smuggle a blocked
//!   address past naive string checks (`http://2130706433/`,
//!   `http://0x7f.0.0.1/`, `http://0177.0.0.1/`, ...)
//! - DNS rebinding: the caller resolves once via [`resolve_validated_addr`]
//!   and pins the HTTP client to the validated [`SocketAddr`], so a second,
//!   attacker-controlled DNS answer at connect time can never be observed
//! - redirects to a blocked/disallowed target: the caller (see
//!   `ImageService::fetch_validated`) re-runs every check in this module
//!   for each hop instead of trusting reqwest's default redirect handling
//!
//! Loopback and link-local blocking can each be independently disabled via
//! `allow_loopback`/`allow_link_local` (imgproxy's
//! `ALLOW_LOOPBACK_SOURCE_ADDRESSES` / `ALLOW_LINK_LOCAL_SOURCE_ADDRESSES`,
//! both default `false`). The override exists because a same-host "fetch
//! from our own local fixture server" workflow (see `src/bin/benchmark.rs`,
//! which serves fixtures on `127.0.0.1` and drives the resize endpoint
//! against them) is otherwise indistinguishable from an attacker targeting
//! loopback, and needs an explicit opt-in escape hatch.
//!
//! RFC1918, CGNAT and IPv6 unique-local (collectively "private" ranges
//! below) have no *blanket* override - unlike loopback/link-local, which
//! are each a single well-known-purpose range, "private" covers an entire
//! operator network, so an unconditional `ALLOW_PRIVATE_SOURCE_ADDRESSES`
//! toggle would let any attacker-influenced URL (a redirect, a
//! DNS-controlled hostname) reach *anything* on that network - every
//! internal admin panel, database, sibling pod, not just the one origin
//! the operator meant to unblock (#57). Instead, an explicit
//! `ALLOWED_SOURCES` match is what lifts the private-range block, and only
//! for the specific host that matched, on the specific hop that matched it
//! - see `is_allowed_source` and the `allow_private` parameter threaded
//! through this module. This keeps the safe default (private ranges
//! blocked unless configured) while making a named internal origin
//! (Kubernetes Service ClusterIP, internal MinIO, private CDN shield)
//! reachable without widening the guard for anything else.

use anyhow::{Context, Result};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::LazyLock;
use url::Url;

use ipnet::{Ipv4Net, Ipv6Net};

/// Why a source URL was refused by the SSRF guard (#21), either before a
/// single byte was sent (bad scheme, blocked IP literal, not in the
/// `ALLOWED_SOURCES` allowlist) or after DNS resolved it to something unsafe.
///
/// A single downcastable type - rather than the bare `anyhow::bail!` this
/// module used to raise at every rejection site - so
/// `AppError::classify_resize_error` (`src/modules/utils/err.rs`, owned
/// separately) can `downcast_ref::<SourceRejected>()` the `anyhow::Error` it
/// receives and map every variant to `400 Bad Request`. Before this type
/// existed, none of these messages matched any of that function's string
/// heuristics, so they all fell through to `502 Bad Gateway` - telling the
/// caller "the upstream is broken" when in fact we refused their request
/// outright, and inviting pointless retries of a request that can never
/// succeed. Mirrors the pattern `InvalidKeyError` established in
/// `src/services/storage/key_validation.rs`.
///
/// Deliberately kept as one enum rather than splitting "we refused outright"
/// (`UnsupportedScheme`, `NotAllowlisted`, `BlockedIpLiteral`,
/// `BlockedResolvedAddress`) from "we tried to resolve and got nothing"
/// (`NoAddressesResolved`) into two separate error types: both classes map to
/// the exact same HTTP status today - it is the caller's URL either way,
/// whether it points somewhere blocked or nowhere at all - so two types would
/// only add a second downcast call site without changing any observable
/// behavior. The variants still carry that distinction in their names/fields,
/// so a future caller that does want to treat "unreachable" differently from
/// "blocked" (e.g. to retry DNS but never retry a blocked IP) can match on
/// the variant instead of the type.
///
/// Propagated via plain `?` (never `.context(...)`, which would replace this
/// type's `Display` as the top-level message `anyhow::Error::to_string()`
/// returns, and would NOT affect `downcast_ref` itself - but
/// `classify_resize_error` also uses `err.to_string()` for the response body,
/// so a `.context(...)` wrapper would silently show the caller a generic
/// context message instead of the real reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRejected {
    /// Scheme other than `http`/`https`.
    UnsupportedScheme { scheme: String },
    /// URL doesn't match any prefix in the configured `ALLOWED_SOURCES`.
    NotAllowlisted { url: String },
    /// Host, given as a literal IP (or decoded from an `inet_aton`-style
    /// numeric form), is itself a blocked address.
    BlockedIpLiteral { host: String, addr: IpAddr },
    /// DNS resolved `host`, but every resulting address is in a blocked
    /// range.
    BlockedResolvedAddress { host: String, addr: IpAddr },
    /// DNS resolution for `host` succeeded but returned zero addresses.
    NoAddressesResolved { host: String },
}

impl fmt::Display for SourceRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceRejected::UnsupportedScheme { scheme } => write!(
                f,
                "Unsupported source URL scheme '{scheme}' (only http/https are allowed)"
            ),
            SourceRejected::NotAllowlisted { url } => write!(
                f,
                "Source URL '{url}' is not in the configured ALLOWED_SOURCES allowlist"
            ),
            SourceRejected::BlockedIpLiteral { host, addr } => {
                write!(f, "Source host '{host}' is a blocked IP literal ({addr})")
            }
            SourceRejected::BlockedResolvedAddress { host, addr } => write!(
                f,
                "Source host '{host}' resolves to a blocked address ({addr})"
            ),
            SourceRejected::NoAddressesResolved { host } => {
                write!(f, "DNS resolution for host '{host}' returned no addresses")
            }
        }
    }
}

impl std::error::Error for SourceRejected {}

// CGNAT (RFC 6598) and the IPv6 link-local / unique-local ranges aren't
// covered by a stable `std::net` predicate, so they're checked via `ipnet`
// CIDR membership instead. Parsed once, lazily, since these are fixed
// literals that can never fail to parse.
static CGNAT_V4: LazyLock<Ipv4Net> =
    LazyLock::new(|| "100.64.0.0/10".parse().expect("valid CIDR literal"));
static LINK_LOCAL_V6: LazyLock<Ipv6Net> =
    LazyLock::new(|| "fe80::/10".parse().expect("valid CIDR literal"));
static UNIQUE_LOCAL_V6: LazyLock<Ipv6Net> =
    LazyLock::new(|| "fc00::/7".parse().expect("valid CIDR literal"));

/// Rejects any scheme other than `http`/`https` before a single byte is
/// sent over the network.
pub fn validate_scheme(url: &Url) -> Result<()> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(SourceRejected::UnsupportedScheme {
            scheme: other.to_string(),
        }
        .into()),
    }
}

/// `true` if `url` matches at least one prefix in `allowed`. Mirrors
/// imgproxy's `IMGPROXY_ALLOWED_SOURCES` shape: a comma-separated list of
/// URL prefixes.
///
/// Matching is **structural** (scheme + host + port compared as parsed
/// fields, path compared as a segment-aware prefix), never a naive
/// `candidate.starts_with(prefix)` on the raw URL text. A plain string
/// `starts_with` is spoofable two ways that both matter here, since a
/// match against this allowlist is what (#57) authorizes bypassing the
/// private-IP-range block:
/// - **userinfo**: `https://allowed.example.com@evil.com/x` *starts with*
///   the literal text `https://allowed.example.com`, but its actual host
///   (what the client connects to, and what `Url::host_str` returns) is
///   `evil.com`. Comparing parsed `host_str()`/`scheme()`/port instead of
///   raw text makes this not match.
/// - **subdomain boundary**: without a separator check, a prefix
///   `https://allowed.example.com` (no trailing slash) would also
///   textually match `https://allowed.example.com.evil.com/x`. Comparing
///   `host_str()` for exact equality closes this too.
///
/// A URL that merely *contains* an allowed prefix elsewhere (query string,
/// fragment, path) was never a `starts_with` match in the first place and
/// still isn't here.
pub fn is_allowed_source(url: &Url, allowed: &[String]) -> bool {
    allowed
        .iter()
        .any(|prefix| matches_allowed_prefix(url, prefix))
}

/// Single-prefix half of [`is_allowed_source`]. The prefix is itself
/// parsed as a URL so it goes through the same host/userinfo-stripping
/// logic as the candidate - an allowlist entry is operator-configured, but
/// there's no reason to hold it to a lower parsing standard than the
/// request URL it's compared against.
fn matches_allowed_prefix(url: &Url, prefix: &str) -> bool {
    let Ok(prefix_url) = Url::parse(prefix) else {
        return false;
    };

    url.scheme() == prefix_url.scheme()
        && url.host_str().is_some()
        && url.host_str() == prefix_url.host_str()
        && url.port_or_known_default() == prefix_url.port_or_known_default()
        && url.path().starts_with(prefix_url.path())
}

fn is_blocked_ipv4(
    ip: Ipv4Addr,
    allow_loopback: bool,
    allow_link_local: bool,
    allow_private: bool,
) -> bool {
    (!allow_loopback && ip.is_loopback()) // 127.0.0.0/8
        || (!allow_link_local && ip.is_link_local()) // 169.254.0.0/16
        || (!allow_private && ip.is_private())    // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 (RFC1918)
        || ip.is_unspecified() // 0.0.0.0
        || ip.is_broadcast()  // 255.255.255.255
        || (!allow_private && CGNAT_V4.contains(&ip)) // 100.64.0.0/10 (RFC 6598)
}

fn is_blocked_ipv6(
    ip: Ipv6Addr,
    allow_loopback: bool,
    allow_link_local: bool,
    allow_private: bool,
) -> bool {
    // IPv4-mapped IPv6 (::ffff:a.b.c.d) must be unwrapped and re-checked
    // against the IPv4 rules, otherwise `::ffff:169.254.169.254` would
    // sail straight past every IPv6-shaped check below.
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped, allow_loopback, allow_link_local, allow_private);
    }

    (!allow_loopback && ip.is_loopback()) // ::1
        || ip.is_unspecified() // ::
        || (!allow_link_local && LINK_LOCAL_V6.contains(&ip)) // fe80::/10
        || (!allow_private && UNIQUE_LOCAL_V6.contains(&ip)) // fc00::/7 (ULA)
}

/// Strict variant of [`is_blocked_ip_with_policy`] with every override off -
/// what every request gets unless explicitly configured otherwise.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    is_blocked_ip_with_policy(ip, false, false, false)
}

/// Single entry point for "is this address safe to connect to", honoring
/// the `allow_loopback`/`allow_link_local`/`allow_private` overrides. Used
/// both for literal IP hosts and for every address a DNS lookup returns.
///
/// `allow_private` lifts the RFC1918/CGNAT/IPv6-unique-local block. Unlike
/// `allow_loopback`/`allow_link_local`, which are blanket per-process
/// toggles, callers must only pass `true` here for a hop whose *host*
/// independently matched the operator's `ALLOWED_SOURCES` allowlist (#57)
/// - see `is_allowed_source` and this module's doc comment. Loopback and
/// link-local are deliberately not covered by `allow_private`: the cloud
/// metadata endpoint (169.254.169.254) lives in link-local, and an
/// allowlisted origin redirecting there must still be blocked.
pub fn is_blocked_ip_with_policy(
    ip: IpAddr,
    allow_loopback: bool,
    allow_link_local: bool,
    allow_private: bool,
) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4, allow_loopback, allow_link_local, allow_private),
        IpAddr::V6(v6) => is_blocked_ipv6(v6, allow_loopback, allow_link_local, allow_private),
    }
}

/// Parses a numeric string as decimal, or as octal when it has a leading
/// `0` (e.g. `0177` -> 127), or as hex when prefixed with `0x`/`0X`. This is
/// the per-component grammar `inet_aton`-style IPv4 literal encodings rely
/// on to smuggle a blocked address past a naive dotted-quad string check.
fn parse_flexible_u64(part: &str) -> Option<u64> {
    if part.is_empty() {
        return None;
    }

    if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }

    if part.len() > 1 && part.starts_with('0') && part.bytes().all(|b| b.is_ascii_digit()) {
        return u64::from_str_radix(part, 8).ok();
    }

    part.parse::<u64>().ok()
}

/// Decodes the `inet_aton`-style short forms of an IPv4 address: a bare
/// integer (`2130706433` == `127.0.0.1`), 2/3-part forms where the last
/// part absorbs the remaining bytes (`127.1` == `127.0.0.1`), and full
/// 4-part dotted form, each part independently decimal/octal/hex. Returns
/// `None` for anything that isn't a purely numeric host (i.e. every real
/// hostname), so callers can safely fall through to normal DNS resolution.
fn parse_ipv4_literal(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }

    let nums: Vec<u64> = parts
        .iter()
        .map(|part| parse_flexible_u64(part))
        .collect::<Option<Vec<u64>>>()?;

    let value: u64 = match nums.as_slice() {
        [a] => *a,
        [a, b] => {
            if *a > 0xFF || *b > 0x00FF_FFFF {
                return None;
            }
            (a << 24) | b
        }
        [a, b, c] => {
            if *a > 0xFF || *b > 0xFF || *c > 0xFFFF {
                return None;
            }
            (a << 24) | (b << 16) | c
        }
        [a, b, c, d] => {
            if [*a, *b, *c, *d].iter().any(|n| *n > 0xFF) {
                return None;
            }
            (a << 24) | (b << 16) | (c << 8) | d
        }
        _ => return None,
    };

    u32::try_from(value).ok().map(Ipv4Addr::from)
}

/// Recognizes a host string that is actually a disguised IP literal:
/// standard dotted-decimal/IPv6 (via the stdlib parser), or one of the
/// decimal/octal/hex `inet_aton`-style encodings above. Returns `None` for
/// real hostnames, which must go through DNS instead.
fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }

    parse_ipv4_literal(host).map(IpAddr::V4)
}

/// Resolves `host` to a single validated [`SocketAddr`], rejecting it if it
/// (or, for a DNS name, *any* address it resolves to) falls in a blocked
/// range under the given `allow_loopback`/`allow_link_local`/`allow_private`
/// policy. Resolution happens exactly once here; the caller must connect to
/// the returned address directly (e.g. via `ClientBuilder::resolve`) rather
/// than letting the HTTP stack re-resolve the hostname, which is what
/// closes the DNS-rebinding TOCTOU window.
///
/// `allow_private` must only be `true` when the caller has already
/// confirmed, via [`is_allowed_source`], that *this specific `host`* (the
/// one about to be resolved, not merely some URL earlier in a redirect
/// chain) matches the operator's `ALLOWED_SOURCES` allowlist (#57). Passing
/// a caller-wide constant here instead of a per-host, per-hop result would
/// turn one allowlisted hostname into a bypass for the entire private
/// range - callers must recompute the match for every hop.
pub async fn resolve_validated_addr(
    host: &str,
    port: u16,
    allow_loopback: bool,
    allow_link_local: bool,
    allow_private: bool,
) -> Result<SocketAddr> {
    if let Some(ip) = parse_ip_literal(host) {
        if is_blocked_ip_with_policy(ip, allow_loopback, allow_link_local, allow_private) {
            return Err(SourceRejected::BlockedIpLiteral {
                host: host.to_string(),
                addr: ip,
            }
            .into());
        }
        return Ok(SocketAddr::new(ip, port));
    }

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("DNS resolution failed for host '{host}'"))?
        .collect();

    if addrs.is_empty() {
        return Err(SourceRejected::NoAddressesResolved {
            host: host.to_string(),
        }
        .into());
    }

    for addr in &addrs {
        if is_blocked_ip_with_policy(addr.ip(), allow_loopback, allow_link_local, allow_private) {
            return Err(SourceRejected::BlockedResolvedAddress {
                host: host.to_string(),
                addr: addr.ip(),
            }
            .into());
        }
    }

    Ok(addrs[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn scheme_rejects_non_http() {
        for scheme in ["file", "gopher", "ftp", "javascript", "data"] {
            let url = Url::parse(&format!("{scheme}://example.com/x")).unwrap();
            assert!(
                validate_scheme(&url).is_err(),
                "expected {scheme} to be rejected"
            );
        }
    }

    #[test]
    fn scheme_allows_http_and_https() {
        assert!(validate_scheme(&Url::parse("http://example.com").unwrap()).is_ok());
        assert!(validate_scheme(&Url::parse("https://example.com").unwrap()).is_ok());
    }

    #[test]
    fn blocks_ipv4_loopback() {
        assert!(is_blocked_ip(ip("127.0.0.1")));
        assert!(is_blocked_ip(ip("127.255.255.255")));
    }

    #[test]
    fn blocks_ipv4_link_local() {
        assert!(is_blocked_ip(ip("169.254.169.254"))); // cloud metadata endpoint
        assert!(is_blocked_ip(ip("169.254.0.1")));
    }

    #[test]
    fn blocks_ipv4_rfc1918() {
        assert!(is_blocked_ip(ip("10.0.0.1")));
        assert!(is_blocked_ip(ip("172.16.0.1")));
        assert!(is_blocked_ip(ip("172.31.255.255")));
        assert!(is_blocked_ip(ip("192.168.1.1")));
    }

    #[test]
    fn blocks_ipv4_cgnat() {
        assert!(is_blocked_ip(ip("100.64.0.1")));
        assert!(is_blocked_ip(ip("100.127.255.255")));
        assert!(!is_blocked_ip(ip("100.63.255.255")));
        assert!(!is_blocked_ip(ip("100.128.0.0")));
    }

    #[test]
    fn blocks_ipv4_unspecified_and_broadcast() {
        assert!(is_blocked_ip(ip("0.0.0.0")));
        assert!(is_blocked_ip(ip("255.255.255.255")));
    }

    #[test]
    fn blocks_ipv6_loopback_and_unspecified() {
        assert!(is_blocked_ip(ip("::1")));
        assert!(is_blocked_ip(ip("::")));
    }

    #[test]
    fn blocks_ipv6_link_local() {
        assert!(is_blocked_ip(ip("fe80::1")));
    }

    #[test]
    fn blocks_ipv6_unique_local() {
        assert!(is_blocked_ip(ip("fc00::1")));
        assert!(is_blocked_ip(ip("fd12:3456:789a::1")));
    }

    #[test]
    fn unique_local_ipv6_has_no_loopback_or_link_local_override() {
        // ULA is not touched by the loopback/link-local overrides - only
        // `allow_private` (tested separately below) affects it.
        assert!(is_blocked_ip_with_policy(ip("fc00::1"), true, true, false));
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6() {
        assert!(is_blocked_ip(ip("::ffff:169.254.169.254")));
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1")));
        assert!(is_blocked_ip(ip("::ffff:10.0.0.1")));
    }

    #[test]
    fn allows_public_addresses() {
        assert!(!is_blocked_ip(ip("8.8.8.8")));
        assert!(!is_blocked_ip(ip("1.1.1.1")));
        assert!(!is_blocked_ip(ip("2606:4700:4700::1111"))); // Cloudflare DNS
    }

    #[test]
    fn loopback_override_allows_loopback_but_nothing_else() {
        assert!(!is_blocked_ip_with_policy(
            ip("127.0.0.1"),
            true,
            false,
            false
        ));
        assert!(!is_blocked_ip_with_policy(ip("::1"), true, false, false));
        // RFC1918 stays blocked regardless of the loopback override.
        assert!(is_blocked_ip_with_policy(
            ip("10.0.0.1"),
            true,
            false,
            false
        ));
    }

    #[test]
    fn link_local_override_allows_link_local_but_nothing_else() {
        assert!(!is_blocked_ip_with_policy(
            ip("169.254.1.1"),
            false,
            true,
            false
        ));
        assert!(!is_blocked_ip_with_policy(
            ip("fe80::1"),
            false,
            true,
            false
        ));
        assert!(is_blocked_ip_with_policy(
            ip("127.0.0.1"),
            false,
            true,
            false
        ));
    }

    /// #57: `allow_private` lifts RFC1918/CGNAT/IPv6-ULA, and only those -
    /// it must not also unblock loopback or link-local (the metadata
    /// endpoint lives in link-local and must stay hard to reach even when
    /// an origin's `ALLOWED_SOURCES` match sets `allow_private`).
    #[test]
    fn private_override_allows_private_ranges_but_not_loopback_or_link_local() {
        assert!(!is_blocked_ip_with_policy(
            ip("10.0.0.1"),
            false,
            false,
            true
        ));
        assert!(!is_blocked_ip_with_policy(
            ip("172.16.0.1"),
            false,
            false,
            true
        ));
        assert!(!is_blocked_ip_with_policy(
            ip("192.168.1.1"),
            false,
            false,
            true
        ));
        assert!(!is_blocked_ip_with_policy(
            ip("100.64.0.1"), // CGNAT
            false,
            false,
            true
        ));
        assert!(!is_blocked_ip_with_policy(
            ip("fc00::1"),
            false,
            false,
            true
        )); // IPv6 ULA

        // Loopback and link-local (including the cloud metadata endpoint)
        // stay blocked - `allow_private` is not a blanket override.
        assert!(is_blocked_ip_with_policy(
            ip("127.0.0.1"),
            false,
            false,
            true
        ));
        assert!(is_blocked_ip_with_policy(
            ip("169.254.169.254"),
            false,
            false,
            true
        ));

        // Unspecified/broadcast still blocked too - never legitimate
        // origins regardless of any override.
        assert!(is_blocked_ip_with_policy(ip("0.0.0.0"), false, false, true));
        assert!(is_blocked_ip_with_policy(
            ip("255.255.255.255"),
            false,
            false,
            true
        ));
    }

    #[test]
    fn decimal_ip_literal_is_decoded_and_blocked() {
        // 2130706433 == 127.0.0.1
        assert_eq!(parse_ip_literal("2130706433"), Some(ip("127.0.0.1")));
        assert!(is_blocked_ip(parse_ip_literal("2130706433").unwrap()));

        // 2852039166 == 169.254.169.254 (cloud metadata endpoint)
        assert_eq!(parse_ip_literal("2852039166"), Some(ip("169.254.169.254")));
    }

    #[test]
    fn octal_ip_literal_is_decoded_and_blocked() {
        // 0177 == 127 in octal
        assert_eq!(parse_ip_literal("0177.0.0.1"), Some(ip("127.0.0.1")));
        assert!(is_blocked_ip(parse_ip_literal("0177.0.0.1").unwrap()));
    }

    #[test]
    fn hex_ip_literal_is_decoded_and_blocked() {
        assert_eq!(parse_ip_literal("0x7f.0x0.0x0.0x1"), Some(ip("127.0.0.1")));
        assert_eq!(parse_ip_literal("0x7f000001"), Some(ip("127.0.0.1")));
        assert!(is_blocked_ip(parse_ip_literal("0x7f000001").unwrap()));
    }

    #[test]
    fn short_inet_aton_form_is_decoded() {
        // 127.1 == 127.0.0.1 (2-part inet_aton shorthand)
        assert_eq!(parse_ip_literal("127.1"), Some(ip("127.0.0.1")));
        // 10.1 == 10.0.0.1
        assert_eq!(parse_ip_literal("10.1"), Some(ip("10.0.0.1")));
        assert!(is_blocked_ip(parse_ip_literal("10.1").unwrap()));
    }

    #[test]
    fn real_hostnames_are_not_misdetected_as_ip_literals() {
        assert_eq!(parse_ip_literal("example.com"), None);
        assert_eq!(parse_ip_literal("123.example.com"), None);
        assert_eq!(parse_ip_literal("cdn.example.com"), None);
    }

    #[test]
    fn allowed_sources_prefix_match() {
        let allowed = vec!["https://trusted.example.com/".to_string()];
        let ok_url = Url::parse("https://trusted.example.com/img.png").unwrap();
        let bad_url = Url::parse("https://evil.example.com/img.png").unwrap();

        assert!(is_allowed_source(&ok_url, &allowed));
        assert!(!is_allowed_source(&bad_url, &allowed));
    }

    /// #57: a URL that merely *contains* an allowed prefix somewhere other
    /// than its actual host - query string, userinfo, path - must not
    /// match. A match here is what authorizes bypassing the private-IP
    /// block, so a spoofable match would reopen #21's SSRF.
    #[test]
    fn allowed_sources_does_not_match_prefix_embedded_in_query_string() {
        let allowed = vec!["https://allowed.internal".to_string()];
        let spoofed = Url::parse("https://evil.com/?x=https://allowed.internal/").unwrap();

        assert!(!is_allowed_source(&spoofed, &allowed));
    }

    /// The userinfo trick: `https://allowed.internal@evil.com/x` *starts
    /// with* the literal text `https://allowed.internal`, but its actual
    /// host - what a client connects to, and what `Url::host_str` reports
    /// - is `evil.com`. A naive `str::starts_with` match on the raw URL
    /// text falls for this; structural host comparison does not.
    #[test]
    fn allowed_sources_does_not_match_prefix_embedded_in_userinfo() {
        let allowed = vec!["https://allowed.internal".to_string()];
        let spoofed = Url::parse("https://allowed.internal@evil.com/x").unwrap();

        assert_eq!(spoofed.host_str(), Some("evil.com"));
        assert!(!is_allowed_source(&spoofed, &allowed));
    }

    /// Subdomain-boundary trick: without an exact-host comparison, a
    /// prefix `https://allowed.example.com` (no trailing slash) would also
    /// textually match `https://allowed.example.com.evil.com/x` - a
    /// completely different, attacker-registered host that merely starts
    /// with the same characters.
    #[test]
    fn allowed_sources_does_not_match_subdomain_boundary_spoof() {
        let allowed = vec!["https://allowed.example.com".to_string()];
        let spoofed = Url::parse("https://allowed.example.com.evil.com/x").unwrap();

        assert!(!is_allowed_source(&spoofed, &allowed));
    }

    /// A different scheme or port on an otherwise-matching host must not
    /// match - imgproxy's `ALLOWED_SOURCES` shape is a *URL* prefix, not a
    /// bare hostname allowlist.
    #[test]
    fn allowed_sources_requires_matching_scheme_and_port() {
        let allowed = vec!["https://trusted.example.com/".to_string()];
        let wrong_scheme = Url::parse("http://trusted.example.com/img.png").unwrap();
        let wrong_port = Url::parse("https://trusted.example.com:8443/img.png").unwrap();

        assert!(!is_allowed_source(&wrong_scheme, &allowed));
        assert!(!is_allowed_source(&wrong_port, &allowed));
    }

    /// An allowlist entry can itself be a raw IP-literal prefix (e.g. a
    /// Kubernetes Service ClusterIP with no DNS name at all) - the
    /// operator's whole point in #57 is to name an internal address
    /// directly.
    #[test]
    fn allowed_sources_matches_ip_literal_prefix() {
        let allowed = vec!["http://10.0.0.5:8080/".to_string()];
        let ok_url = Url::parse("http://10.0.0.5:8080/img.png").unwrap();
        let other_ip = Url::parse("http://10.0.0.6:8080/img.png").unwrap();

        assert!(is_allowed_source(&ok_url, &allowed));
        assert!(!is_allowed_source(&other_ip, &allowed));
    }

    #[tokio::test]
    async fn resolve_validated_addr_rejects_loopback_literal() {
        let result = resolve_validated_addr("127.0.0.1", 80, false, false, false).await;
        assert!(result.is_err());
    }

    /// The whole point of `SourceRejected` (Gap 1): a blocked-IP-literal
    /// rejection must downcast to the typed error, not just produce *some*
    /// `anyhow::Error` whose message a caller has to string-match. This is
    /// what lets `AppError::classify_resize_error`
    /// (`src/modules/utils/err.rs`) map it to `400 Bad Request` instead of
    /// falling through to the `502 Bad Gateway` default.
    #[tokio::test]
    async fn resolve_validated_addr_rejects_loopback_literal_as_typed_source_rejected() {
        let err = resolve_validated_addr("127.0.0.1", 80, false, false, false)
            .await
            .expect_err("loopback literal must be rejected");
        let rejected = err
            .downcast_ref::<SourceRejected>()
            .expect("error must downcast to SourceRejected, not be a bare anyhow! message");
        assert!(matches!(rejected, SourceRejected::BlockedIpLiteral { .. }));
    }

    #[tokio::test]
    async fn resolve_validated_addr_rejects_metadata_endpoint_literal() {
        let result = resolve_validated_addr("169.254.169.254", 80, false, false, false).await;
        assert!(result.is_err());
    }

    /// Cloud metadata endpoint (`169.254.169.254`) is the highest-value SSRF
    /// target this guard blocks - confirm its rejection is typed too, same
    /// as the loopback case above.
    #[tokio::test]
    async fn resolve_validated_addr_rejects_metadata_endpoint_as_typed_source_rejected() {
        let err = resolve_validated_addr("169.254.169.254", 80, false, false, false)
            .await
            .expect_err("metadata endpoint literal must be rejected");
        let rejected = err
            .downcast_ref::<SourceRejected>()
            .expect("error must downcast to SourceRejected");
        assert!(matches!(rejected, SourceRejected::BlockedIpLiteral { .. }));
    }

    #[test]
    fn unsupported_scheme_rejection_downcasts_to_source_rejected() {
        let url = Url::parse("gopher://example.com/x").unwrap();
        let err = validate_scheme(&url).expect_err("gopher scheme must be rejected");
        let rejected = err
            .downcast_ref::<SourceRejected>()
            .expect("error must downcast to SourceRejected, not be a bare anyhow! message");
        assert!(matches!(
            rejected,
            SourceRejected::UnsupportedScheme { scheme } if scheme == "gopher"
        ));
    }

    // `NoAddressesResolved` (the empty-`addrs` branch of
    // `resolve_validated_addr`) is intentionally not exercised here via a
    // real hostname lookup: `tokio::net::lookup_host` needs the OS resolver
    // and actual network access, which may not be available in every
    // sandboxed test environment, and a hostname that reliably resolves to
    // zero addresses everywhere isn't guaranteed to exist. The variant's
    // construction and `Display` are still covered indirectly by every
    // other `SourceRejected` variant test above using the same pattern.

    #[tokio::test]
    async fn resolve_validated_addr_rejects_encoded_loopback_literal() {
        // Decimal-encoded 127.0.0.1 must be blocked exactly like the
        // dotted-quad form - this is the whole point of decoding literals
        // before the range check instead of after.
        let result = resolve_validated_addr("2130706433", 80, false, false, false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn resolve_validated_addr_allows_public_ip_literal() {
        let result = resolve_validated_addr("8.8.8.8", 443, false, false, false).await;
        assert_eq!(result.unwrap(), SocketAddr::new(ip("8.8.8.8"), 443));
    }

    #[tokio::test]
    async fn resolve_validated_addr_honors_loopback_override() {
        let result = resolve_validated_addr("127.0.0.1", 8080, true, false, false).await;
        assert_eq!(result.unwrap(), SocketAddr::new(ip("127.0.0.1"), 8080));
    }

    /// #57: with `allow_private`, an RFC1918 literal resolves successfully
    /// instead of being rejected - this is the actual fix, proven at the
    /// address-validation layer (this function never touches the network
    /// for an IP-literal host, so this needs no live private-network
    /// server to test).
    #[tokio::test]
    async fn resolve_validated_addr_allows_rfc1918_literal_when_private_allowed() {
        let result = resolve_validated_addr("10.0.0.5", 80, false, false, true).await;
        assert_eq!(result.unwrap(), SocketAddr::new(ip("10.0.0.5"), 80));
    }

    #[tokio::test]
    async fn resolve_validated_addr_allows_cgnat_literal_when_private_allowed() {
        let result = resolve_validated_addr("100.64.0.1", 80, false, false, true).await;
        assert_eq!(result.unwrap(), SocketAddr::new(ip("100.64.0.1"), 80));
    }

    #[tokio::test]
    async fn resolve_validated_addr_allows_ipv6_ula_literal_when_private_allowed() {
        let result = resolve_validated_addr("fd12:3456:789a::1", 80, false, false, true).await;
        assert_eq!(
            result.unwrap(),
            SocketAddr::new(ip("fd12:3456:789a::1"), 80)
        );
    }

    /// Without `allow_private` (the default), RFC1918 stays blocked - the
    /// non-allowlisted case from the checklist.
    #[tokio::test]
    async fn resolve_validated_addr_still_blocks_rfc1918_literal_by_default() {
        let result = resolve_validated_addr("10.0.0.5", 80, false, false, false).await;
        assert!(result.is_err());
    }

    /// `allow_private` must not be a blanket escape hatch: the metadata
    /// endpoint is link-local, not RFC1918/private, and must stay blocked
    /// even when the caller passes `allow_private: true` for an
    /// allowlisted origin that redirected there (#21's bypass).
    #[tokio::test]
    async fn resolve_validated_addr_rejects_metadata_endpoint_even_with_private_allowed() {
        let result = resolve_validated_addr("169.254.169.254", 80, false, false, true).await;
        assert!(result.is_err());
    }

    /// Same property for loopback: `allow_private` alone must not also
    /// unblock loopback.
    #[tokio::test]
    async fn resolve_validated_addr_rejects_loopback_even_with_private_allowed() {
        let result = resolve_validated_addr("127.0.0.1", 80, false, false, true).await;
        assert!(result.is_err());
    }
}

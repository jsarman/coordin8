//! Opt-in allowlist for TransactionMgr participant endpoints.
//!
//! `Enlist` stores a caller-supplied `host:port` that the coordinator later
//! dials during 2PC, attaching its outbound service token when JWT auth is on.
//! Without a guard any caller can aim the Djinn at arbitrary internal hosts
//! (SSRF) or harvest a valid token by enlisting an endpoint it controls.
//!
//! `COORDIN8_TXN_PARTICIPANT_ALLOW` is a comma-separated list of entries:
//!
//! - exact hostname: `space`
//! - wildcard-suffix hostname: `*.internal` (matches `a.internal`, not `internal`)
//! - IPv4 / IPv6 literal: `10.0.0.5`, `::1`
//! - CIDR: `10.0.0.0/8`, `fd00::/8` (matches IP-literal endpoints only)
//!
//! A non-CIDR entry may carry a `:port` suffix (`space:9006`, `[::1]:9006`) to
//! restrict the port; no port means any port. Unset/empty allows everything.
//!
//! Matching is on the literal host string in the endpoint. **No DNS
//! resolution is performed**: allowing a hostname trusts whatever it resolves
//! to, and a CIDR entry never matches a hostname endpoint.

use std::net::IpAddr;

use coordin8_core::Error;

pub const ALLOW_ENV_VAR: &str = "COORDIN8_TXN_PARTICIPANT_ALLOW";

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPattern {
    Exact(String),
    /// Stored without the leading `*.`; matches `<label>.<suffix>`.
    Suffix(String),
    Ip(IpAddr),
    Cidr(IpAddr, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    host: HostPattern,
    port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    Name(String),
    Ip(IpAddr),
}

#[derive(Debug, Clone, Default)]
pub struct ParticipantAllowlist {
    /// Empty = allow all.
    entries: Vec<Entry>,
}

fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn canon(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

fn parse_port(p: &str) -> Result<u16, String> {
    match p.parse::<u16>() {
        Ok(n) if n != 0 => Ok(n),
        _ => Err(format!("invalid port '{p}'")),
    }
}

/// Split `host[:port]` / `[v6][:port]` / bare-v6. Returns (host, port).
fn split_host_port(s: &str) -> Result<(&str, Option<&str>), String> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or("missing ']'")?;
        return match tail {
            "" => Ok((host, None)),
            t => match t.strip_prefix(':') {
                Some(p) => Ok((host, Some(p))),
                None => Err("unexpected text after ']'".into()),
            },
        };
    }
    match s.matches(':').count() {
        0 => Ok((s, None)),
        1 => {
            let (h, p) = s.split_once(':').unwrap();
            Ok((h, Some(p)))
        }
        _ => Ok((s, None)), // bare IPv6 literal
    }
}

fn parse_host(h: &str) -> Result<Host, String> {
    if h.is_empty() {
        return Err("empty host".into());
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        return Ok(Host::Ip(canon(ip)));
    }
    let lower = h.to_ascii_lowercase();
    if !valid_hostname(&lower) {
        return Err(format!("invalid host '{h}'"));
    }
    Ok(Host::Name(lower))
}

fn pattern_of(host: Host) -> HostPattern {
    match host {
        Host::Name(n) => HostPattern::Exact(n),
        Host::Ip(ip) => HostPattern::Ip(ip),
    }
}

fn parse_entry(raw: &str) -> Result<Entry, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty entry".into());
    }
    if let Some((addr, len)) = raw.split_once('/') {
        let ip: IpAddr = addr
            .parse()
            .map_err(|_| format!("invalid CIDR address '{addr}'"))?;
        let len: u8 = len
            .parse()
            .map_err(|_| format!("invalid CIDR prefix length '{len}'"))?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if len > max {
            return Err(format!("CIDR prefix length {len} exceeds {max}"));
        }
        return Ok(Entry {
            host: HostPattern::Cidr(ip, len),
            port: None,
        });
    }
    let (host, port) = split_host_port(raw)?;
    let port = port.map(parse_port).transpose()?;
    if let Some(suffix) = host.strip_prefix("*.") {
        let suffix = suffix.to_ascii_lowercase();
        if !valid_hostname(&suffix) {
            return Err(format!("invalid wildcard host '{host}'"));
        }
        return Ok(Entry {
            host: HostPattern::Suffix(suffix),
            port,
        });
    }
    if host.contains('*') {
        return Err(format!(
            "'{host}': wildcards are only supported as a leading '*.'"
        ));
    }
    Ok(Entry {
        host: pattern_of(parse_host(host)?),
        port,
    })
}

fn cidr_contains(net: IpAddr, len: u8, ip: IpAddr) -> bool {
    let len = len as u32;
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(n) & mask == u32::from(i) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(i)) => {
            let mask = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len)
            };
            u128::from(n) & mask == u128::from(i) & mask
        }
        _ => false,
    }
}

impl Entry {
    fn matches(&self, host: &Host, port: u16) -> bool {
        if self.port.is_some_and(|p| p != port) {
            return false;
        }
        match (&self.host, host) {
            (HostPattern::Exact(e), Host::Name(h)) => e == h,
            (HostPattern::Suffix(s), Host::Name(h)) => {
                h.len() > s.len() + 1
                    && h.ends_with(s.as_str())
                    && h[..h.len() - s.len()].ends_with('.')
            }
            (HostPattern::Ip(a), Host::Ip(b)) => canon(*a) == *b,
            (HostPattern::Cidr(n, l), Host::Ip(b)) => cidr_contains(*n, *l, *b),
            _ => false,
        }
    }
}

/// Parse an endpoint, which must be strictly `host:port` / `[v6]:port`.
fn parse_endpoint(endpoint: &str) -> Result<(Host, u16), String> {
    let bad = |why: &str| format!("participant_endpoint '{endpoint}' must be host:port ({why})");
    // Bare IPv6 without brackets is ambiguous as an endpoint.
    if endpoint.matches(':').count() > 1 && !endpoint.starts_with('[') {
        return Err(bad("IPv6 literals must be bracketed"));
    }
    let (host, port) = split_host_port(endpoint).map_err(|e| bad(&e))?;
    let port = port.ok_or_else(|| bad("missing port"))?;
    let port = parse_port(port).map_err(|e| bad(&e))?;
    let host = parse_host(host).map_err(|e| bad(&e))?;
    Ok((host, port))
}

impl ParticipantAllowlist {
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// True when no restriction is configured.
    pub fn is_allow_all(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parse a comma-separated spec. Empty/blank = allow all. Any invalid
    /// entry is an error naming the entry.
    pub fn parse(spec: &str) -> Result<Self, String> {
        if spec.trim().is_empty() {
            return Ok(Self::allow_all());
        }
        let mut entries = Vec::new();
        for raw in spec.split(',') {
            entries.push(
                parse_entry(raw)
                    .map_err(|e| format!("{ALLOW_ENV_VAR}: bad entry '{}': {e}", raw.trim()))?,
            );
        }
        Ok(Self { entries })
    }

    /// Read [`ALLOW_ENV_VAR`]; unset/empty = allow all. Fail-fast on invalid.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var(ALLOW_ENV_VAR) {
            Ok(v) => Self::parse(&v),
            Err(_) => Ok(Self::allow_all()),
        }
    }

    /// Additionally allow one exact `host:port` endpoint (e.g. bundled mode's
    /// own Space participant). No-op if the list is allow-all.
    pub fn with_implicit_endpoint(mut self, endpoint: &str) -> Result<Self, String> {
        if self.is_allow_all() {
            return Ok(self);
        }
        let (host, port) = parse_endpoint(endpoint)?;
        self.entries.push(Entry {
            host: pattern_of(host),
            port: Some(port),
        });
        Ok(self)
    }

    /// Validate well-formedness (always) and the allowlist (if configured).
    pub fn check(&self, endpoint: &str) -> Result<(), Error> {
        let (host, port) = parse_endpoint(endpoint).map_err(Error::InvalidArgument)?;
        if self.is_allow_all() || self.entries.iter().any(|e| e.matches(&host, port)) {
            Ok(())
        } else {
            Err(Error::PermissionDenied(format!(
                "participant endpoint '{endpoint}' is not permitted by {ALLOW_ENV_VAR}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn al(s: &str) -> ParticipantAllowlist {
        ParticipantAllowlist::parse(s).unwrap()
    }
    fn ok(a: &ParticipantAllowlist, e: &str) -> bool {
        a.check(e).is_ok()
    }

    #[test]
    fn unset_or_blank_allows_all() {
        assert!(ok(&al(""), "anything.example:1"));
        assert!(ok(&al("  "), "10.1.2.3:99"));
        assert!(ParticipantAllowlist::allow_all().is_allow_all());
    }

    #[test]
    fn malformed_endpoint_is_invalid_argument_even_when_allow_all() {
        for bad in [
            "",
            "nohost",
            "host:",
            ":80",
            "host:0",
            "host:99999",
            "a b:1",
            "::1:80",
            "[::1]",
            "[::1",
        ] {
            assert!(
                matches!(al("").check(bad), Err(Error::InvalidArgument(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn exact_host_and_port() {
        let a = al("space, Other:9006");
        assert!(ok(&a, "space:9006"));
        assert!(ok(&a, "SPACE:1"));
        assert!(ok(&a, "other:9006"));
        assert!(!ok(&a, "other:9007"));
        assert!(!ok(&a, "evil:9006"));
        assert!(matches!(
            a.check("evil:9006"),
            Err(Error::PermissionDenied(_))
        ));
    }

    #[test]
    fn wildcard_suffix() {
        let a = al("*.internal");
        assert!(ok(&a, "a.internal:1"));
        assert!(ok(&a, "a.b.internal:1"));
        assert!(!ok(&a, "internal:1"));
        assert!(!ok(&a, "xinternal:1"));
        assert!(!ok(&a, "a.internal.evil.com:1"));
        assert!(!ok(&a, "10.0.0.1:1"));
        let p = al("*.internal:9006");
        assert!(ok(&p, "a.internal:9006"));
        assert!(!ok(&p, "a.internal:80"));
    }

    #[test]
    fn ip_literals() {
        let a = al("10.0.0.5, ::1, [fd00::7]:9006");
        assert!(ok(&a, "10.0.0.5:80"));
        assert!(!ok(&a, "10.0.0.6:80"));
        assert!(ok(&a, "[::1]:80"));
        assert!(ok(&a, "[fd00::7]:9006"));
        assert!(!ok(&a, "[fd00::7]:9007"));
        assert!(ok(&a, "[::ffff:10.0.0.5]:80"));
    }

    #[test]
    fn cidr_v4_and_v6() {
        let a = al("10.0.0.0/8, fd00::/8, 192.168.1.128/25");
        assert!(ok(&a, "10.200.1.1:1"));
        assert!(!ok(&a, "11.0.0.1:1"));
        assert!(ok(&a, "[fd12::1]:1"));
        assert!(!ok(&a, "[fe80::1]:1"));
        assert!(ok(&a, "192.168.1.200:1"));
        assert!(!ok(&a, "192.168.1.5:1"));
        // CIDR never matches a hostname (no DNS resolution)
        assert!(!ok(&a, "internal.example:1"));
        assert!(ok(&al("0.0.0.0/0"), "1.2.3.4:5"));
        assert!(ok(&al("10.1.2.3/32"), "10.1.2.3:5"));
    }

    #[test]
    fn invalid_entries_rejected_at_parse() {
        for bad in [
            "space,,x",
            "10.0.0.0/33",
            "fd00::/129",
            "10.0.0.0/x",
            "300.1.1.1/8",
            "host:0",
            "host:70000",
            "*",
            "*.",
            "a*.b",
            "bad host",
            "*.a:b",
            "[::1",
            "10.0.0.0/8:80",
        ] {
            assert!(ParticipantAllowlist::parse(bad).is_err(), "{bad}");
        }
        let e = ParticipantAllowlist::parse("ok,10.0.0.0/99").unwrap_err();
        assert!(e.contains("10.0.0.0/99") && e.contains(ALLOW_ENV_VAR));
    }

    #[test]
    fn implicit_endpoint_added_only_when_restricted() {
        let a = al("*.internal")
            .with_implicit_endpoint("djinn-host:9006")
            .unwrap();
        assert!(ok(&a, "djinn-host:9006"));
        assert!(!ok(&a, "djinn-host:9007"));
        assert!(ok(&a, "x.internal:1"));
        let all = al("").with_implicit_endpoint("djinn-host:9006").unwrap();
        assert!(all.is_allow_all());
    }
}

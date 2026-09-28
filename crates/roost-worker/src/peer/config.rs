//! The operator's peer transport settings, resolved once at boot and shared by
//! the terminal and attachment peer owners: whether direct peers run at all,
//! the one address their sockets bind, and the UDP port range. Resolved by
//! `runtime::boot` (`WorkerBoot::resolve`); read by `runtime::owners`. Ports
//! `parseTerminalPeer{Enabled,BindAddress,PortRange}` of v2
//! `apps/worker/src/host/config.ts`.

use std::net::IpAddr;

use roost_host::{EnvSource, HostPlatform};

pub const TERMINAL_PEER_ENABLED_ENV: &str = "ROOST_TERMINAL_PEER_ENABLED";
pub const TERMINAL_PEER_BIND_ADDRESS_ENV: &str = "ROOST_TERMINAL_PEER_BIND_ADDRESS";
pub const TERMINAL_PEER_PORT_RANGE_ENV: &str = "ROOST_TERMINAL_PEER_PORT_RANGE";

const PORT_RANGE_REFUSAL: &str =
    "ROOST_TERMINAL_PEER_PORT_RANGE must be min-max with decimal ports from 1024 to 65535";

/// v2 `WorkerConfig.terminalPeer{Enabled,BindAddress,PortRange}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerTransportConfig {
    pub enabled: bool,
    pub bind_address: Option<IpAddr>,
    pub port_range: Option<(u16, u16)>,
}

/// The refusal a malformed setting boots into, worded as v2 throws it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PeerConfigError(pub &'static str);

impl PeerTransportConfig {
    pub fn resolve(env: &dyn EnvSource, platform: HostPlatform) -> Result<Self, PeerConfigError> {
        Ok(Self {
            enabled: parse_enabled(env.get(TERMINAL_PEER_ENABLED_ENV).as_deref(), platform)?,
            bind_address: parse_bind_address(env.get(TERMINAL_PEER_BIND_ADDRESS_ENV).as_deref())?,
            port_range: parse_port_range(env.get(TERMINAL_PEER_PORT_RANGE_ENV).as_deref())?,
        })
    }
}

/// Unset runs peers on the platforms v2 shipped them on; `0`/`1` otherwise.
pub fn parse_enabled(value: Option<&str>, platform: HostPlatform) -> Result<bool, PeerConfigError> {
    match value {
        None => Ok(matches!(
            platform,
            HostPlatform::Linux | HostPlatform::MacOs
        )),
        Some("0") => Ok(false),
        Some("1") if platform == HostPlatform::Windows => Err(PeerConfigError(
            "ROOST_TERMINAL_PEER_ENABLED=1 is unsupported on Windows",
        )),
        Some("1") => Ok(true),
        Some(_) => Err(PeerConfigError(
            "ROOST_TERMINAL_PEER_ENABLED must be exactly 0 or 1",
        )),
    }
}

/// A literal unicast address: IPv4 with a first octet in 1..=223, or IPv6
/// that is neither `::` nor `ff…`, and whose mapped IPv4 form (if any) is
/// unicast by the same rule (v2 `isUnicastIpv6`).
pub fn parse_bind_address(value: Option<&str>) -> Result<Option<IpAddr>, PeerConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let unicast_v4 = |first_octet: u8| (1..224).contains(&first_octet);
    let address = value
        .parse::<IpAddr>()
        .ok()
        .filter(|address| match address {
            IpAddr::V4(v4) => unicast_v4(v4.octets()[0]),
            IpAddr::V6(v6) => {
                let canonical = v6.to_string();
                canonical != "::"
                    && !canonical.starts_with("ff")
                    && v6
                        .to_ipv4_mapped()
                        .is_none_or(|v4| unicast_v4(v4.octets()[0]))
            }
        });
    match address {
        Some(address) => Ok(Some(address)),
        None => Err(PeerConfigError(
            "ROOST_TERMINAL_PEER_BIND_ADDRESS must be a literal unicast IPv4 or IPv6 address",
        )),
    }
}

/// `min-max`, both decimal without a leading zero, 1024 ≤ min ≤ max ≤ 65535.
pub fn parse_port_range(value: Option<&str>) -> Result<Option<(u16, u16)>, PeerConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let (min, max) = value
        .split_once('-')
        .ok_or(PeerConfigError(PORT_RANGE_REFUSAL))?;
    let port = |text: &str| -> Result<u32, PeerConfigError> {
        let canonical = !text.is_empty()
            && !text.starts_with('0')
            && text.len() <= 5
            && text.bytes().all(|byte| byte.is_ascii_digit());
        if !canonical {
            return Err(PeerConfigError(PORT_RANGE_REFUSAL));
        }
        text.parse()
            .map_err(|_| PeerConfigError(PORT_RANGE_REFUSAL))
    };
    let (min, max) = (port(min)?, port(max)?);
    if min < 1024 || max > 65_535 || min > max {
        return Err(PeerConfigError(PORT_RANGE_REFUSAL));
    }
    let as_port =
        |value: u32| u16::try_from(value).map_err(|_| PeerConfigError(PORT_RANGE_REFUSAL));
    Ok(Some((as_port(min)?, as_port(max)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_follows_v2_platform_defaults_and_spelling() {
        assert_eq!(parse_enabled(None, HostPlatform::Linux), Ok(true));
        assert_eq!(parse_enabled(None, HostPlatform::Windows), Ok(false));
        assert_eq!(parse_enabled(Some("0"), HostPlatform::MacOs), Ok(false));
        assert!(parse_enabled(Some("true"), HostPlatform::Linux).is_err());
        assert!(parse_enabled(Some("1"), HostPlatform::Windows).is_err());
    }

    #[test]
    fn bind_addresses_must_be_literal_unicast() {
        assert_eq!(
            parse_bind_address(Some("127.0.0.1")),
            Ok(Some("127.0.0.1".parse().unwrap()))
        );
        assert_eq!(
            parse_bind_address(Some("fd7a::1")),
            Ok(Some("fd7a::1".parse().unwrap()))
        );
        for refused in ["0.0.0.0", "224.0.0.1", "localhost", "ff02::1", "::"] {
            assert!(parse_bind_address(Some(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn port_ranges_are_bounded_decimal_pairs() {
        assert_eq!(
            parse_port_range(Some("41000-41001")),
            Ok(Some((41000, 41001)))
        );
        for refused in [
            "1023-2000",
            "2000-1999",
            "02000-3000",
            "2000-70000",
            "2000",
            "a-b",
        ] {
            assert!(parse_port_range(Some(refused)).is_err(), "{refused}");
        }
    }
}

//! Parse Tailscale's local status and Serve configuration without trusting it.
//!
//! The access controller uses these checks before every Serve mutation.
//! Unknown configuration is treated as a conflict rather than overwritten.

use serde_json::Value;
use url::Url;

use super::TailscaleError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Mapping {
    Free,
    Compatible,
    Conflict,
    Funnel,
}

pub(super) fn node_name(status: &Value) -> Result<String, TailscaleError> {
    match status.get("BackendState").and_then(Value::as_str) {
        Some("Running") => (),
        Some("NeedsMachineAuth") => return Err(TailscaleError::NeedsApproval),
        Some("Starting") => return Err(TailscaleError::Starting),
        _ => return Err(TailscaleError::NotLoggedIn),
    }
    let name = status
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)
        .ok_or(TailscaleError::MissingDnsName)?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if name.len() > 253
        || !name.ends_with(".ts.net")
        || name.split('.').any(|part| {
            part.is_empty()
                || part.len() > 63
                || part.starts_with('-')
                || part.ends_with('-')
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(TailscaleError::MissingDnsName);
    }
    let domains = status
        .get("CertDomains")
        .and_then(Value::as_array)
        .ok_or(TailscaleError::CertificatesUnavailable)?;
    if !domains
        .iter()
        .filter_map(Value::as_str)
        .any(|domain| domain.trim_end_matches('.').eq_ignore_ascii_case(&name))
    {
        return Err(TailscaleError::CertificatesUnavailable);
    }
    Ok(name)
}

fn entry<'a>(
    object: &'a Value,
    field: &str,
    key: &str,
) -> Result<Option<&'a Value>, TailscaleError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let map = value.as_object().ok_or(TailscaleError::InvalidResponse)?;
    let mut matches = map
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(key));
    let result = matches.next().map(|(_, value)| value);
    if matches.next().is_some() {
        return Err(TailscaleError::InvalidResponse);
    }
    Ok(result)
}

fn target_matches(value: &str, port: u16) -> bool {
    let text = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let Ok(url) = Url::parse(&text) else {
        return false;
    };
    url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        && url.port_or_known_default() == Some(port)
        && url.path() == "/"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

pub(super) fn exact_proxy(
    config: &Value,
    hostname: &str,
    proxy: &str,
) -> Result<bool, TailscaleError> {
    let key = format!("{hostname}:443");
    let Some(web) = entry(config, "Web", &key)? else {
        return Ok(false);
    };
    Ok(web.pointer("/Handlers/~1/Proxy").and_then(Value::as_str) == Some(proxy))
}

pub(super) fn mapping(
    config: &Value,
    hostname: &str,
    port: u16,
) -> Result<Mapping, TailscaleError> {
    if !(config.is_object() || config.is_null()) {
        return Err(TailscaleError::InvalidResponse);
    }
    let key = format!("{hostname}:443");
    if let Some(funnels) = config.get("AllowFunnel").filter(|value| !value.is_null()) {
        let funnels = funnels.as_object().ok_or(TailscaleError::InvalidResponse)?;
        for (name, enabled) in funnels {
            if name.to_ascii_lowercase().ends_with(":443") {
                match enabled.as_bool() {
                    Some(true) => return Ok(Mapping::Funnel),
                    Some(false) => (),
                    None => return Err(TailscaleError::InvalidResponse),
                }
            }
        }
    }
    let tcp = entry(config, "TCP", "443")?;
    if let Some(tcp) = tcp {
        let Some(tcp) = tcp.as_object() else {
            return Err(TailscaleError::InvalidResponse);
        };
        if tcp.get("HTTP").and_then(Value::as_bool) == Some(true)
            || tcp
                .get("TCPForward")
                .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
            || tcp
                .get("HTTPS")
                .is_some_and(|value| value.as_bool() != Some(true))
        {
            return Ok(Mapping::Conflict);
        }
        if tcp
            .keys()
            .any(|key| !matches!(key.as_str(), "HTTPS" | "HTTP" | "TCPForward"))
        {
            return Ok(Mapping::Conflict);
        }
    }
    let Some(web) = entry(config, "Web", &key)? else {
        return Ok(Mapping::Free);
    };
    let Some(handlers) = web.get("Handlers").and_then(Value::as_object) else {
        return Err(TailscaleError::InvalidResponse);
    };
    let Some(root) = handlers.get("/") else {
        return Ok(Mapping::Free);
    };
    let Some(root) = root.as_object() else {
        return Err(TailscaleError::InvalidResponse);
    };
    if tcp
        .and_then(|value| value.get("HTTPS"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Ok(Mapping::Conflict);
    }
    if root.len() == 1
        && root
            .get("Proxy")
            .and_then(Value::as_str)
            .is_some_and(|target| target_matches(target, port))
    {
        Ok(Mapping::Compatible)
    } else {
        Ok(Mapping::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognizes_only_exact_private_https_mapping() {
        let config = json!({"TCP":{"443":{"HTTPS":true}},"Web":{"node.example.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4113"},"/other":{"Text":"x"}}}}});
        assert_eq!(
            mapping(&config, "node.example.ts.net", 4113).ok(),
            Some(Mapping::Compatible)
        );
        assert_eq!(
            mapping(&config, "node.example.ts.net", 4114).ok(),
            Some(Mapping::Conflict)
        );
        let mut public = config;
        public["AllowFunnel"] = json!({"node.example.ts.net:443":true});
        assert_eq!(
            mapping(&public, "node.example.ts.net", 4113).ok(),
            Some(Mapping::Funnel)
        );
        assert!(!target_matches("http://localhost:4113/?secret=x", 4113));
    }

    #[test]
    fn refuses_funnel_on_another_hostname_and_tcp_forward() {
        let public = json!({"AllowFunnel":{"other.example.ts.net:443":true}});
        assert_eq!(
            mapping(&public, "node.example.ts.net", 4113).ok(),
            Some(Mapping::Funnel)
        );
        let tcp = json!({"TCP":{"443":{"TCPForward":"localhost:22"}}});
        assert_eq!(
            mapping(&tcp, "node.example.ts.net", 4113).ok(),
            Some(Mapping::Conflict)
        );
    }

    #[test]
    fn login_states_do_not_imply_authorization() {
        assert!(matches!(
            node_name(&json!({"BackendState":"NeedsMachineAuth"})),
            Err(TailscaleError::NeedsApproval)
        ));
        assert!(matches!(
            node_name(&json!({"BackendState":"NeedsLogin"})),
            Err(TailscaleError::NotLoggedIn)
        ));
        assert!(matches!(
            node_name(&json!({"BackendState":"Starting"})),
            Err(TailscaleError::Starting)
        ));
    }

    #[test]
    fn requires_own_certificate_name() {
        let mut status = json!({"BackendState":"Running","Self":{"DNSName":"node.example.ts.net."},"CertDomains":["other.example.ts.net"]});
        assert!(matches!(
            node_name(&status),
            Err(TailscaleError::CertificatesUnavailable)
        ));
        status["CertDomains"] = json!(["node.example.ts.net"]);
        assert_eq!(
            node_name(&status).ok().as_deref(),
            Some("node.example.ts.net")
        );
    }
}

//! Port of `packages/ai/src/utils/node-http-proxy.ts` @ pi `9841914`
//! (v0.85.0+; `a63fb12c1` / #8737 fixed the NO_PROXY matching: a bare domain
//! exempts both the root domain **and** subdomains, without the legacy
//! suffix bug that also exempted `notexample.com`).
//!
//! Upstream consumes this for the adapters that bypass standard fetch
//! (Bedrock / Codex build raw Node HTTP clients); everything else rides the
//! coding-agent global `EnvHttpProxyAgent`. rpi's transport is reqwest
//! everywhere, so this module's decision logic is wired into every adapter
//! client builder via [`crate::api::http_client`]: reqwest's built-in env
//! proxying is disabled (`.no_proxy()`, semantics not upstream-alignable)
//! and an explicit proxy is injected with **these** semantics. The aligned
//! behavior is *which hosts bypass the proxy* — the transport mechanism
//! difference (reqwest explicit `Proxy` vs Node agents) is noted here, not a
//! deviation.
//!
//! rpi-env mirror note: `rpi/src/core/environment.rs` owns the `RPI_*`
//! naming layer; this module deliberately reads the **raw** process-level
//! `HTTP(S)_PROXY`/`NO_PROXY`/`ALL_PROXY` names (JS truthiness: empty string
//! counts as unset), mirroring upstream `getProxyEnv` exactly — provider
//! scoped overrides (`ProviderEnv`) win, then the process environment.

use crate::types::ProviderEnv;

/// `DEFAULT_PROXY_PORTS` (node-http-proxy.ts:5-12).
const DEFAULT_PROXY_PORTS: &[(&str, u16)] = &[
    ("ftp", 21),
    ("gopher", 70),
    ("http", 80),
    ("https", 443),
    ("ws", 80),
    ("wss", 443),
];

/// `UNSUPPORTED_PROXY_PROTOCOL_MESSAGE`.
pub const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE: &str = "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";

/// `getProxyEnv` (node-http-proxy.ts:14-22): scoped env overrides (lowercase
/// first, then uppercase) before the process environment, JS truthiness
/// (empty values are unset).
fn proxy_env(key: &str, env: Option<&ProviderEnv>) -> Option<String> {
    let lowercase = key.to_ascii_lowercase();
    let uppercase = key.to_ascii_uppercase();
    let truthy = |value: Option<String>| value.filter(|value| !value.is_empty());
    truthy(env.and_then(|map| map.get(&lowercase).cloned()))
        .or_else(|| truthy(env.and_then(|map| map.get(&uppercase).cloned())))
        .or_else(|| truthy(std::env::var(&lowercase).ok()))
        .or_else(|| truthy(std::env::var(&uppercase).ok()))
}

/// `stripBrackets` — `[::1]` → `::1`.
fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

/// `parseNoProxyEntry` (:41-72): `host` (brackets stripped for IPv6) and an
/// optional `port` (0 = any). `None` skips empty entries.
fn parse_no_proxy_entry(entry: &str) -> Option<(String, u16)> {
    let trimmed = entry.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('[')
        && let Some(closing_bracket) = trimmed.find(']')
    {
        let host = trimmed[1..closing_bracket].to_owned();
        let rest = &trimmed[closing_bracket + 1..];
        if let Some(port_text) = rest.strip_prefix(':') {
            let port = port_text.parse::<u16>().unwrap_or(0);
            return Some((host, port));
        }
        return Some((host, 0));
    }

    // More than one colon and no brackets: a bare IPv6 address.
    if trimmed.contains(':') && trimmed.split(':').count() > 2 {
        return Some((trimmed, 0));
    }

    if let Some(colon_index) = trimmed.rfind(':')
        && colon_index == trimmed.find(':').expect("rfind implies find")
    {
        let host = trimmed[..colon_index].to_owned();
        if let Ok(port) = trimmed[colon_index + 1..].parse::<u16>() {
            return Some((host, port));
        }
    }

    Some((trimmed, 0))
}

/// `shouldProxyHostname` (:74-114): true when the target must go through the
/// proxy — every `NO_PROXY` entry must fail to match (an entry matches when
/// its domain equals the host or is a subdomain of it, and the entry's port
/// — if any — equals the target port).
pub fn should_proxy_hostname(hostname: &str, port: u16, env: Option<&ProviderEnv>) -> bool {
    let Some(no_proxy) = proxy_env("no_proxy", env) else {
        return true;
    };
    let no_proxy = no_proxy.to_ascii_lowercase();
    if no_proxy.is_empty() {
        return true;
    }
    if no_proxy == "*" {
        return false;
    }

    let lowercased_host = hostname.to_ascii_lowercase();
    let normalized_target_host = strip_brackets(&lowercased_host);

    no_proxy
        .split(|c: char| c == ',' || c.is_whitespace())
        .all(|entry| {
            let Some((host, entry_port)) = parse_no_proxy_entry(entry) else {
                return true;
            };

            if entry_port != 0 && entry_port != port {
                return true;
            }

            // `*.` strips two chars; a leading `.` or bare `*` strips one.
            let domain = strip_brackets(&host);
            let domain = if let Some(rest) = domain.strip_prefix("*.") {
                rest
            } else if let Some(rest) = domain.strip_prefix('.') {
                rest
            } else if let Some(rest) = domain.strip_prefix('*') {
                rest
            } else {
                domain
            };

            if domain.is_empty() {
                return true;
            }

            if normalized_target_host == domain {
                return false;
            }

            if normalized_target_host
                .strip_suffix(&format!(".{domain}"))
                .is_some()
            {
                return false;
            }

            true
        })
}

/// `getProxyForUrl` (:116-135): the proxy URL for a target, or `None` for a
/// direct connection. Scheme-less values are prefixed with the target
/// protocol.
fn proxy_for_url(target_url: &url::Url, env: Option<&ProviderEnv>) -> Option<String> {
    let protocol = target_url.scheme();
    let host_raw = target_url.host_str()?;
    let hostname = strip_brackets(host_raw);
    let port = target_url.port().unwrap_or_else(|| {
        DEFAULT_PROXY_PORTS
            .iter()
            .find(|(scheme, _)| *scheme == protocol)
            .map(|(_, port)| *port)
            .unwrap_or(0)
    });
    if !should_proxy_hostname(hostname, port, env) {
        return None;
    }

    let protocol_key = format!("{protocol}_proxy");
    let proxy = proxy_env(&protocol_key, env).or_else(|| proxy_env("all_proxy", env))?;
    if proxy.contains("://") {
        Some(proxy)
    } else {
        Some(format!("{protocol}://{proxy}"))
    }
}

/// `resolveHttpProxyUrlForTarget` (:141-159): parse and validate the proxy
/// URL. `Ok(None)` connects directly; `Err` carries the upstream error text
/// (invalid URL / unsupported SOCKS-PAC protocol).
pub fn resolve_http_proxy_for_target(
    target_url: &str,
    env: Option<&ProviderEnv>,
) -> Result<Option<url::Url>, String> {
    let Some(proxy) = (|| {
        let parsed = url::Url::parse(target_url).ok()?;
        if parsed.scheme().is_empty() {
            return None;
        }
        proxy_for_url(&parsed, env)
    })() else {
        return Ok(None);
    };

    let proxy_url =
        url::Url::parse(&proxy).map_err(|error| format!("Invalid proxy URL {proxy:?}: {error}"))?;

    if proxy_url.scheme() != "http" && proxy_url.scheme() != "https" {
        return Err(format!(
            "{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {}:",
            proxy_url.scheme()
        ));
    }

    Ok(Some(proxy_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(pairs: &[(&str, &str)]) -> ProviderEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    // Upstream test env keys are process-env scoped; rpi tests use the
    // scoped ProviderEnv (same resolution order — scoped wins).

    /// "respects NO_PROXY exclusions".
    #[test]
    fn respects_no_proxy_exclusions() {
        let env = scoped(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com"),
        ]);
        assert_eq!(
            resolve_http_proxy_for_target(
                "https://bedrock-runtime.us-east-1.amazonaws.com",
                Some(&env)
            ),
            Ok(None)
        );
    }

    /// "resolves HTTP and HTTPS proxy URLs".
    #[test]
    fn resolves_proxy_urls() {
        let env = scoped(&[("HTTPS_PROXY", "http://proxy.example:8080")]);
        let resolved = resolve_http_proxy_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            Some(&env),
        )
        .expect("ok")
        .expect("proxy");
        assert_eq!(resolved.as_str(), "http://proxy.example:8080/");
    }

    /// "prefers scoped proxy env aliases before process env aliases" — the
    /// scoped map wins over any same-named process variable.
    #[test]
    fn prefers_scoped_env() {
        // Scoped lowercase beats scoped uppercase; both beat process env.
        let env = scoped(&[("https_proxy", "http://scoped-lower.example:8080")]);
        let resolved = resolve_http_proxy_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            Some(&env),
        )
        .expect("ok")
        .expect("proxy");
        assert_eq!(resolved.as_str(), "http://scoped-lower.example:8080/");

        let env = scoped(&[("HTTPS_PROXY", "http://scoped-upper.example:8080")]);
        let resolved = resolve_http_proxy_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            Some(&env),
        )
        .expect("ok")
        .expect("proxy");
        assert_eq!(resolved.as_str(), "http://scoped-upper.example:8080/");
    }

    /// "rejects SOCKS and PAC proxy URLs explicitly".
    #[test]
    fn rejects_socks_and_pac() {
        let env = scoped(&[("HTTPS_PROXY", "socks5://proxy.example:1080")]);
        let error = resolve_http_proxy_for_target(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            Some(&env),
        )
        .expect_err("socks rejected");
        assert!(
            error.starts_with(UNSUPPORTED_PROXY_PROTOCOL_MESSAGE),
            "error: {error}"
        );
    }

    /// "handles subdomain wildcards, IPv6, and ports in NO_PROXY" — the
    /// #8737 matrix.
    #[test]
    fn no_proxy_matrix() {
        let env = scoped(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            (
                "NO_PROXY",
                "example.com, .wildcard.org, *.star.net, ::1, [2001:db8::1], 127.0.0.1:8080",
            ),
        ]);
        let direct = |url: &str| resolve_http_proxy_for_target(url, Some(&env)) == Ok(None);
        let proxied = |url: &str| {
            resolve_http_proxy_for_target(url, Some(&env))
                .expect("ok")
                .is_some()
        };

        // Bare domain: root + subdomains, but not lookalikes.
        assert!(direct("https://example.com"));
        assert!(direct("https://api.example.com"));
        assert!(proxied("https://notexample.com"));

        // `.domain` and `*.domain` prefixes.
        assert!(direct("https://wildcard.org"));
        assert!(direct("https://api.wildcard.org"));
        assert!(direct("https://star.net"));
        assert!(direct("https://api.star.net"));

        // IPv6 bare and bracketed.
        assert!(direct("https://[::1]:80"));
        assert!(direct("https://[2001:db8::1]"));

        // Port-scoped entry: only the matching port is exempt.
        assert!(direct("https://127.0.0.1:8080"));
        assert!(proxied("https://127.0.0.1:3000"));
    }

    /// `*` exempts everything; empty entries are skipped; empty env proxies
    /// directly.
    #[test]
    fn star_and_empty_entries() {
        let env = scoped(&[("NO_PROXY", "*")]);
        assert_eq!(
            resolve_http_proxy_for_target("https://any.host", Some(&env)),
            Ok(None)
        );

        let env = scoped(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "  , ,, "),
        ]);
        assert!(
            resolve_http_proxy_for_target("https://any.host", Some(&env))
                .expect("ok")
                .is_some()
        );
    }

    /// Scheme-less proxy values get the target protocol prefixed; `ALL_PROXY`
    /// is the fallback; `HTTP_PROXY` drives http targets.
    #[test]
    fn scheme_backfill_and_fallbacks() {
        let env = scoped(&[("HTTPS_PROXY", "proxy.example:8080")]);
        let resolved = resolve_http_proxy_for_target("https://api.example.com", Some(&env))
            .expect("ok")
            .expect("proxy");
        assert_eq!(resolved.as_str(), "https://proxy.example:8080/");

        let env = scoped(&[("ALL_PROXY", "http://all.example:8080")]);
        let resolved = resolve_http_proxy_for_target("https://api.example.com", Some(&env))
            .expect("ok")
            .expect("proxy");
        assert_eq!(resolved.as_str(), "http://all.example:8080/");

        let env = scoped(&[("HTTP_PROXY", "http://http-proxy.example:8080")]);
        let resolved = resolve_http_proxy_for_target("http://api.example.com", Some(&env))
            .expect("ok")
            .expect("proxy");
        assert_eq!(resolved.as_str(), "http://http-proxy.example:8080/");
        // An https target does not use HTTP_PROXY.
        assert_eq!(
            resolve_http_proxy_for_target("https://api.example.com", Some(&env)),
            Ok(None)
        );
    }

    /// Default ports participate in NO_PROXY port matching (https → 443).
    #[test]
    fn default_port_matching() {
        let env = scoped(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "api.example.com:443"),
        ]);
        assert_eq!(
            resolve_http_proxy_for_target("https://api.example.com", Some(&env)),
            Ok(None)
        );
        // Explicit other port still proxies.
        assert!(
            resolve_http_proxy_for_target("https://api.example.com:8443", Some(&env))
                .expect("ok")
                .is_some()
        );
    }
}

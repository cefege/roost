//! One reader for an installed service definition's `ROOST_*` environment, for
//! both shapes a POSIX install writes: a launchd `EnvironmentVariables`
//! dictionary on macOS and `Environment=` lines in a systemd `--user` unit on
//! Linux. Called by status/collect.rs, which resolves the front door, the
//! coordinator's own bind, its database and its stamped SPA dist from here.
//!
//! It parses rather than reads `process.env` on purpose. The definitions a
//! deploy writes are the install's own record of what it was told, and the
//! shell that runs `roost status` is a different shell from the one that
//! installed the services — so the definition is the only place the answer
//! survives. It is also the reason a remote deploy must not adopt this
//! process's ambient identity: see DEPLOY_IDENTITY_ENV_FLAGS in the deploy
//! group.

use std::collections::BTreeMap;

use roost_host::HostPlatform;

/// The keys an install may carry. Both parsers accept exactly this set, so a
/// secret a deploy deliberately refused to write (a bootstrap token) is not
/// read back out of a definition, and an unrelated `Environment=` line in the
/// same unit cannot become part of a status readout.
const ROOST_KEY_PREFIX: &str = "ROOST_";
/// The one non-`ROOST_` key an install carries: the commit the unit was built
/// from. `roost status` does not read it, but the deploy paths write it beside
/// the identity keys and a parser that silently dropped it would report a
/// different unit from the one on disk.
const GIT_SHA_KEY: &str = "GIT_SHA";

pub type InstalledEnvironment = BTreeMap<String, String>;

/// Every `ROOST_*`/`GIT_SHA` entry in an installed definition. An unreadable
/// or unrecognised definition yields an empty map rather than an error: status
/// must still report a machine whose unit file was hand-edited into something
/// unparseable, and refusing to print would hide the one fact that matters.
pub fn parse_installed_environment(
    definition: &str,
    platform: HostPlatform,
) -> InstalledEnvironment {
    match platform {
        HostPlatform::MacOs => parse_launch_agent_keys(definition),
        HostPlatform::Linux => parse_systemd_environment(definition),
        // v3 ships Linux and macOS only, and `supported_host_platform` refuses a
        // Windows host before any command runs. The arm exists so the refusal is
        // the loader's decision rather than this match's.
        HostPlatform::Windows => InstalledEnvironment::new(),
    }
}

/// One declared value, with installed-but-empty treated as undeclared.
/// Installed units carry `Environment="ROOST_COORDINATOR_PUBLIC_URL="` when
/// the operator cleared the front door, and an empty string that reads as a
/// configured URL would send status to `https://`.
pub fn declared_value<'a>(environment: &'a InstalledEnvironment, name: &str) -> Option<&'a str> {
    environment
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

fn is_install_key(name: &str) -> bool {
    // `GIT_SHA` is a whole key, not a family, so it is matched exactly; the
    // `ROOST_` family is matched by prefix but never on the bare prefix, which
    // is not a key.
    name == GIT_SHA_KEY
        || (name.starts_with(ROOST_KEY_PREFIX) && name.len() > ROOST_KEY_PREFIX.len())
}

fn parse_systemd_environment(definition: &str) -> InstalledEnvironment {
    let mut out = InstalledEnvironment::new();
    for line in definition.lines() {
        let Some(entry) = line.strip_prefix("Environment=") else {
            continue;
        };
        // One entry per line, in either the legacy unquoted form or the
        // canonical quoted one. A line carrying two entries is skipped rather
        // than half-parsed: systemd accepts it, the installer has never
        // written it, and guessing which half is the value is how a wrong
        // coordinator URL gets installed.
        let entry = match entry.strip_prefix('"') {
            Some(quoted) => match quoted.split_once('"') {
                Some((pair, _rest)) => pair,
                None => continue,
            },
            None => entry.trim(),
        };
        let Some((name, raw)) = entry.split_once('=') else {
            continue;
        };
        if !is_install_key(name) {
            continue;
        }
        out.insert(name.to_string(), unescape_systemd(raw.trim_end()));
    }
    out
}

/// `%%` is systemd's own escape for a literal `%`. A backslash introduces one of
/// `\ " n r t`; anything else is the character itself with the backslash
/// dropped, which is what the v2 parser did and what keeps a Windows-style path
/// in a unit file from reading as a run of escape sequences.
fn unescape_systemd(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        match character {
            '%' if chars.as_str().starts_with('%') => {
                chars.next();
                out.push('%');
            }
            '\\' => match chars.next() {
                Some(escaped) => out.push(convert_escape(escaped)),
                None => out.push('\\'),
            },
            other => out.push(other),
        }
    }
    out
}

fn convert_escape(escaped: char) -> char {
    match escaped {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        // An unrecognised escape is the character, not the backslash and the
        // character. Keeping the backslash would put a character in the value
        // that no writer meant and no reader expects.
        other => other,
    }
}

fn parse_launch_agent_keys(definition: &str) -> InstalledEnvironment {
    let mut out = InstalledEnvironment::new();
    let mut rest = definition;
    while let Some(open) = rest.find("<key>") {
        let after_key = &rest[open + "<key>".len()..];
        let Some(close) = after_key.find("</key>") else {
            break;
        };
        let name = &after_key[..close];
        let tail = &after_key[close + "</key>".len()..];
        // A key's value is the `<string>` that follows it DIRECTLY. A key whose
        // next element is a nested `<dict>` or `<array>` — `EnvironmentVariables`
        // above all — has no scalar value of its own, and taking the first
        // `<string>` inside that nested element both reads it under the wrong
        // name and consumes it, so every variable inside the dictionary is then
        // invisible. On a macOS install that is every variable: a deploy could
        // not reuse a prior install's coordinator URL, and `roost status` could
        // not resolve the bind it was about to probe.
        let value_start = match tail.find("<string>") {
            Some(offset) if tail[..offset].trim().is_empty() => offset,
            _ => {
                rest = tail;
                continue;
            }
        };
        let after_value = &tail[value_start + "<string>".len()..];
        // The value runs to the next `<`, which is what the TypeScript regex
        // `([^<]*)` did: an XML entity is fine, a nested tag is not, and a
        // LaunchAgent value is neither.
        let value_end = after_value.find('<').unwrap_or(after_value.len());
        if is_install_key(name) {
            out.insert(name.to_string(), unescape_xml(&after_value[..value_end]));
        }
        rest = &after_value[value_end..];
    }
    out
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::{declared_value, is_install_key, parse_installed_environment, unescape_systemd};
    use roost_host::HostPlatform;

    const UNIT: &str = r#"[Unit]
Description=roost3-coord

[Service]
Environment="ROOST_COORDINATOR_BIND=127.0.0.1:4113"
Environment=ROOST_WEB_PUBLIC_URL=https://roost.example.com
Environment="ROOST_WEB_DIST_PATH=/srv/roost/releases/current/web"
Environment="ROOST_COORDINATOR_PUBLIC_URL="
ExecStart=/usr/local/bin/roost coord
"#;

    const PLIST: &str = r#"<?xml version="1.0"?>
<dict>
  <key>ROOST_COORDINATOR_BIND</key>
  <string>127.0.0.1:4113</string>
  <key>ROOST_WEB_DIST_PATH</key>
  <string>/Users/x/roost/releases/&amp;current/web</string>
</dict>
"#;

    #[test]
    fn reads_a_quoted_systemd_entry() {
        let parsed = parse_installed_environment(UNIT, HostPlatform::Linux);
        assert_eq!(
            declared_value(&parsed, "ROOST_COORDINATOR_BIND"),
            Some("127.0.0.1:4113")
        );
    }

    #[test]
    fn reads_an_unquoted_systemd_entry() {
        let parsed = parse_installed_environment(UNIT, HostPlatform::Linux);
        assert_eq!(
            declared_value(&parsed, "ROOST_WEB_PUBLIC_URL"),
            Some("https://roost.example.com")
        );
    }

    #[test]
    fn a_declared_but_empty_entry_declares_nothing() {
        // `Environment="ROOST_COORDINATOR_PUBLIC_URL="` is how an install
        // records "the operator cleared the front door". Reading it as a URL
        // would make status probe `https://`.
        let parsed = parse_installed_environment(UNIT, HostPlatform::Linux);
        assert_eq!(
            declared_value(&parsed, "ROOST_COORDINATOR_PUBLIC_URL"),
            None
        );
    }

    #[test]
    fn reads_a_launch_agent_pair_and_unescapes_xml() {
        let parsed = parse_installed_environment(PLIST, HostPlatform::MacOs);
        assert_eq!(
            declared_value(&parsed, "ROOST_WEB_DIST_PATH"),
            Some("/Users/x/roost/releases/&current/web")
        );
    }

    #[test]
    fn the_two_shapes_do_not_read_each_other() {
        assert!(parse_installed_environment(UNIT, HostPlatform::MacOs).is_empty());
        assert!(parse_installed_environment(PLIST, HostPlatform::Linux).is_empty());
    }

    #[test]
    fn systemd_escapes_unescape() {
        assert_eq!(unescape_systemd("100%%/a\\nb"), "100%/a\nb");
        assert_eq!(unescape_systemd("a\\qb"), "aqb");
    }

    #[test]
    fn only_install_keys_are_kept() {
        assert!(is_install_key("ROOST_COORDINATOR_BIND"));
        assert!(is_install_key("GIT_SHA"));
        assert!(!is_install_key("ROOST_"));
        assert!(!is_install_key("PATH"));
    }
}

//! The two escaping rules a systemd unit needs, and why there are two.
//!
//! systemd quoting is per-directive, not per-file. `ExecStart=` is a command
//! line and `Environment=` is a quoted key/value list, so both accept and for a
//! path with a space need double quotes. `WorkingDirectory=`, `StandardOutput=`
//! and `StandardError=` take the RAW value: the quotes become part of the path,
//! so `WorkingDirectory=` fails `path is not absolute` and the unit refuses to
//! start, and a quoted output specifier is discarded SILENTLY, leaving a
//! service that came up healthy with no log file anywhere.
//!
//! One uniform escape helper for every interpolated value is therefore the
//! wrong shape, and it is the shape that bricks a unit while looking like the
//! conservative choice.

use roost_host::{ProtocolError, ProtocolResult};

/// The characters that would let a value end its own directive and start a
/// new one. A newline is the only one of these that can appear in a
/// well-formed path, and the reason quoting is what a value needs at all.
const DIRECTIVE_FORGING: [char; 3] = ['\n', '\r', '"'];

/// Escape a value for a directive systemd parses as a quoted command line:
/// `ExecStart=` and `Environment=`.
pub fn quoted_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '%' => escaped.push_str("%%"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Render a value for a directive that takes it raw. The forging characters are
/// refused outright — they are the whole reason quoting was ever wanted here —
/// and `%` is doubled so the value can never be read as a systemd specifier.
pub fn raw_path_value(value: &str, directive: &str) -> ProtocolResult<String> {
    if let Some(character) = value
        .chars()
        .find(|candidate| DIRECTIVE_FORGING.contains(candidate))
    {
        return Err(ProtocolError::new(
            directive,
            format!("{directive}={value:?} contains {character:?}, which would forge a directive"),
        ));
    }
    Ok(value.replace('%', "%%"))
}

/// One `Environment="KEY=VALUE"` line. The value is quoted and the key is not:
/// a key is a Rust-side constant everywhere it is used, and a name that needed
/// quoting would be a name nobody can find with `grep`.
pub fn environment_directive(key: &str, value: &str) -> String {
    format!("Environment=\"{key}={}\"", quoted_value(value))
}

#[cfg(test)]
mod tests {
    use super::{environment_directive, quoted_value, raw_path_value};

    #[test]
    fn a_quoted_value_escapes_what_would_end_the_quoting() {
        assert_eq!(quoted_value("/tmp/a b"), "/tmp/a b");
        assert_eq!(quoted_value(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(quoted_value("a\nb"), "a\\nb");
        assert_eq!(quoted_value("100%"), "100%%");
    }

    #[test]
    fn a_raw_value_doubles_percent_and_refuses_to_forge_a_directive() {
        assert_eq!(
            raw_path_value("/var/log/100%x", "WorkingDirectory").unwrap(),
            "/var/log/100%%x"
        );
        for forged in ["/tmp\nExecStart=/bin/sh", "/tmp\rX=1", "/tmp\"x"] {
            assert!(raw_path_value(forged, "WorkingDirectory").is_err());
        }
    }

    #[test]
    fn an_environment_line_keeps_the_equals_inside_the_quotes() {
        assert_eq!(
            environment_directive("ROOST_TRUST_PROXY", "true"),
            "Environment=\"ROOST_TRUST_PROXY=true\""
        );
        assert_eq!(
            environment_directive("ROOST_DIAG", "0"),
            "Environment=\"ROOST_DIAG=0\""
        );
    }
}

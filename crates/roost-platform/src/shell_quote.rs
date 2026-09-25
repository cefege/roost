//! Canonical POSIX shell quoting, for deploy commands and anything else that
//! builds a command line for a remote shell. One quoter for the whole
//! product so local and remote command construction cannot diverge, and
//! byte-stable because recovery tooling compares generated commands exactly.

/// Wrap `value` in single quotes, escaping an embedded single quote with the
/// classic close-escape-open idiom.
pub fn posix_shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::posix_shell_quote;

    #[test]
    fn a_quote_becomes_close_escape_open() {
        assert_eq!(posix_shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(posix_shell_quote("plain"), "'plain'");
    }

    #[test]
    fn the_caller_shapes_a_deploy_passes_come_back_whole() {
        for (value, expected) in [
            (
                "/home/ubuntu/.local/share/roost/releases/worker",
                "'/home/ubuntu/.local/share/roost/releases/worker'",
            ),
            ("$HOME/RoostWorkerV2", "'$HOME/RoostWorkerV2'"),
            (
                "/tmp/space dir/plist path.plist",
                "'/tmp/space dir/plist path.plist'",
            ),
            ("com.roost.worker-v2", "'com.roost.worker-v2'"),
            (
                "back`tick`and$(substituted)",
                "'back`tick`and$(substituted)'",
            ),
            ("semi;colon|pipe&amp", "'semi;colon|pipe&amp'"),
            ("*glob[chars]?", "'*glob[chars]?'"),
            ("double\"quotes", "'double\"quotes'"),
            ("", "''"),
        ] {
            assert_eq!(posix_shell_quote(value), expected, "value was {value}");
        }
    }

    #[test]
    fn every_quote_in_the_value_is_escaped_not_just_the_first() {
        assert_eq!(posix_shell_quote("'a'"), "''\"'\"'a'\"'\"''");
    }
}

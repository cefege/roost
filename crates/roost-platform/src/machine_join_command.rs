//! The one command a new machine runs to join the fleet: the door it dials, the
//! one-shot grant it spends, and the name it appears under, as a single line a
//! terminal can be pasted into.
//!
//! Owned here because more than one surface prints it — `roost add-machine`
//! mints a grant against a coordinator database, and the machines pane mints
//! one against a running coordinator — and a second template would let the two
//! mint grants the other cannot spend. Depends on `shell_quote` and nothing
//! else: no coordinator, no database, no environment and no async, so the
//! quoting can be proved without anything to talk to.

use crate::shell_quote::{posix_shell_quote, powershell_single_quote};

/// The script a new machine runs, in the order its own usage text shows it. The
/// same URL `install.sh` documents, so the command printed here and the command
/// the script describes cannot drift apart silently.
pub const INSTALL_SCRIPT_URL: &str = "https://raw.githubusercontent.com/cefege/roost/v3/install.sh";

/// The Windows installer a new Windows machine runs from PowerShell.
pub const INSTALL_POWERSHELL_URL: &str =
    "https://raw.githubusercontent.com/cefege/roost/v3/install.ps1";

/// The door the new machine's worker dials.
pub const COORDINATOR_URL_ENV: &str = "ROOST_COORDINATOR_URL";

/// The one-shot enrollment grant, spent by the script's first successful run.
pub const BOOTSTRAP_TOKEN_ENV: &str = "ROOST_BOOTSTRAP_TOKEN";

/// The name the machine appears under. Unset, the script lets the machine name
/// itself from the key it generates.
pub const WORKER_LABEL_ENV: &str = "ROOST_WORKER_LABEL";

/// The command a new machine runs, over three plain strings.
///
/// EVERY VALUE IS EXACTLY ONE SHELL WORD, and that is the whole property: a
/// grant is a credential being pasted into somebody's shell, so a machine name
/// carrying a quote, a `;` or a `$(…)` must not be able to turn an enrollment
/// command into something else. An empty label contributes nothing at all,
/// because an empty assignment is a blank name the script would have to guess at.
pub fn machine_join_command(
    coordinator_url: &str,
    bootstrap_token: &str,
    worker_label: &str,
) -> String {
    let label_setting = if worker_label.is_empty() {
        String::new()
    } else {
        format!(" {WORKER_LABEL_ENV}={}", posix_shell_quote(worker_label))
    };
    let url_setting = format!(
        "{COORDINATOR_URL_ENV}={}",
        posix_shell_quote(coordinator_url)
    );
    let grant_setting = format!(
        "{BOOTSTRAP_TOKEN_ENV}={}",
        posix_shell_quote(bootstrap_token)
    );
    format!("curl -fsSL {INSTALL_SCRIPT_URL} | {url_setting} {grant_setting}{label_setting} bash")
}

/// The PowerShell command a new Windows machine runs: the same three values as
/// [`machine_join_command`], each a single-quoted literal, set in the process
/// environment `install.ps1` reads.
pub fn machine_join_command_powershell(
    coordinator_url: &str,
    bootstrap_token: &str,
    worker_label: &str,
) -> String {
    let label_setting = if worker_label.is_empty() {
        String::new()
    } else {
        format!(
            " $env:{WORKER_LABEL_ENV}={};",
            powershell_single_quote(worker_label)
        )
    };
    format!(
        "$env:{COORDINATOR_URL_ENV}={}; $env:{BOOTSTRAP_TOKEN_ENV}={};{label_setting} irm {INSTALL_POWERSHELL_URL} | iex",
        powershell_single_quote(coordinator_url),
        powershell_single_quote(bootstrap_token),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        BOOTSTRAP_TOKEN_ENV, COORDINATOR_URL_ENV, WORKER_LABEL_ENV, machine_join_command,
        machine_join_command_powershell,
    };

    /// The literal assigned to `$env:<name>`, read back the way PowerShell
    /// reads a single-quoted string, and what follows it.
    fn powershell_assignment<'a>(command: &'a str, name: &str) -> (String, &'a str) {
        let opening = format!("$env:{name}='");
        let start = command
            .find(&opening)
            .unwrap_or_else(|| panic!("{name} is assigned: {command}"))
            + opening.len();
        let mut value = String::new();
        let mut rest = &command[start..];
        loop {
            let quote = rest
                .find('\'')
                .unwrap_or_else(|| panic!("the literal closes: {command}"));
            value.push_str(&rest[..quote]);
            if rest[quote + 1..].starts_with('\'') {
                value.push('\'');
                rest = &rest[quote + 2..];
            } else {
                return (value, &rest[quote + 1..]);
            }
        }
    }

    #[test]
    fn a_hostile_label_stays_inside_one_powershell_literal() {
        let label = "box'; rm -r C:\\ ; '$(whoami)";
        let command =
            machine_join_command_powershell("https://roost.example.com", "roost_bt_x", label);
        let (value, rest) = powershell_assignment(&command, WORKER_LABEL_ENV);
        assert_eq!(value, label);
        assert!(rest.starts_with("; irm "), "{command}");
        assert!(command.ends_with(" | iex"), "{command}");
    }

    #[test]
    fn an_unnamed_windows_machine_sets_no_label() {
        let command =
            machine_join_command_powershell("https://roost.example.com", "roost_bt_x", "");
        assert!(!command.contains(WORKER_LABEL_ENV), "{command}");
        assert_eq!(
            powershell_assignment(&command, BOOTSTRAP_TOKEN_ENV).0,
            "roost_bt_x"
        );
        assert_eq!(
            powershell_assignment(&command, COORDINATOR_URL_ENV).0,
            "https://roost.example.com"
        );
    }

    #[test]
    fn an_unnamed_machine_gets_a_command_with_no_label_to_guess_at() {
        let command = machine_join_command("https://roost.example.com", "roost_bt_x", "");
        let url_setting = format!("{COORDINATOR_URL_ENV}=");
        let grant_setting = format!("{BOOTSTRAP_TOKEN_ENV}=");
        assert!(!command.contains(WORKER_LABEL_ENV), "{command}");
        assert!(command.contains(&url_setting), "{command}");
        assert!(command.contains(&grant_setting), "{command}");
        assert!(command.starts_with("curl -fsSL "), "{command}");
        assert!(command.ends_with(" bash"), "{command}");
    }

    /// The quoting IS the property. A label carrying a substitution, a quote and
    /// a command separator must still reach the shell as one word, which is read
    /// back here the way a POSIX shell reads it rather than compared as text.
    #[test]
    fn a_hostile_label_is_one_shell_word_and_not_a_command() {
        let url = "https://roost.example.com";
        let token = "roost_bt_deadbeef";
        let label = "box $(id) 'quoted'; rm -rf ~";
        let command = machine_join_command(url, token, label);

        let assignments = command
            .split(" | ")
            .nth(1)
            .and_then(|tail| tail.strip_suffix(" bash"))
            .unwrap_or_else(|| panic!("the command pipes the script into a shell: {command}"));
        let words = shell_words(assignments);
        let mut values = std::collections::BTreeMap::new();
        for word in &words {
            let (name, value) = word
                .split_once('=')
                .unwrap_or_else(|| panic!("every assignment is NAME=VALUE: {word:?}"));
            values.insert(name.to_owned(), value.to_owned());
        }
        assert_eq!(
            words.len(),
            3,
            "three assignments and nothing else, so the payload introduced no fourth word: \
             {words:?}"
        );
        assert_eq!(values[WORKER_LABEL_ENV], label);
        assert_eq!(values[COORDINATOR_URL_ENV], url);
        assert_eq!(values[BOOTSTRAP_TOKEN_ENV], token);
    }

    /// The words of a POSIX shell command line, expanded the way a shell
    /// expands them.
    ///
    /// Single quotes are literal; double quotes are literal except a backslash
    /// before a quote or another backslash; a backslash outside quotes escapes
    /// the next character; and whitespace outside quotes separates. It is the
    /// `'"'"'` reading — a double-quoted single quote is one word, so
    /// `'it'"'"'s'` is `it's` rather than two quoted runs with a `"` between
    /// them — and a line it cannot account for fails the test instead of being
    /// guessed at, because a guess is the thing under test.
    fn shell_words(line: &str) -> Vec<String> {
        fn closing_quote(characters: &[char], from: usize, quote: char, line: &str) -> usize {
            let mut cursor = from;
            while cursor < characters.len() && characters[cursor] != quote {
                cursor += 1;
            }
            assert!(
                cursor < characters.len(),
                "the line ends inside a quoted word: {line:?}"
            );
            cursor
        }
        let characters: Vec<char> = line.chars().collect();
        let mut words: Vec<String> = Vec::new();
        let mut word = String::new();
        let mut index = 0;
        while index < characters.len() {
            match characters[index] {
                character if character.is_whitespace() => {
                    if !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    index += 1;
                }
                '\'' | '"' => {
                    let end = closing_quote(&characters, index + 1, characters[index], line);
                    word.extend(characters[index + 1..end].iter());
                    index = end + 1;
                }
                '\\' => {
                    let escaped = characters
                        .get(index + 1)
                        .unwrap_or_else(|| panic!("the line ends on a backslash: {line:?}"));
                    word.push(*escaped);
                    index += 2;
                }
                other => {
                    word.push(other);
                    index += 1;
                }
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        words
    }
}

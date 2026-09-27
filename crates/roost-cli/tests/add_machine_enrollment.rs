//! `roost add-machine` reads the door from the installed coordinator
//! definition, refuses when there is no door, refuses a platform v3 does not
//! ship, and prints a command whose quoting a shell cannot reinterpret.
//!
//! Every assertion here is a fact an operator or a script observes: which
//! origin the new machine would dial, what the refusal says, and what the
//! printed line contains. Nothing asserts that a function called another
//! function.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_cli::quickstart::add_machine::{
    EnrollmentPlatform, dial_url, installed_coordinator, shell_enrollment_command,
};
use roost_cli::services::definition_text::render_definition;
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec};
use roost_cli::status::service_definition::InstalledEnvironment;
use roost_host::coord_config_loader::{
    ENV_COORDINATOR_DB, ENV_COORDINATOR_PUBLIC_URL, ENV_WEB_PUBLIC_URL,
};
use roost_host::{HostPlatform, MapEnv};

/// A throwaway tree holding one account's worth of install paths.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-add-machine-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway tree is created");
        Self { root }
    }

    fn environment(&self) -> MapEnv {
        let text = |relative: &str| self.root.join(relative).display().to_string();
        MapEnv::new()
            .with("HOME", &text("home"))
            .with(
                roost_host::COORD_UNIT_ENV,
                &text("unit/roost3-coord.service"),
            )
            .with(roost_host::COORD_DATA_DIR_ENV, &text("data/coord"))
    }

    /// Render and install a coordinator definition carrying `settings`, through
    /// the same renderer and the same path the install uses.
    fn install_coordinator(&self, platform: HostPlatform, settings: &[(&str, &str)]) {
        let env = self.environment();
        let decided: std::collections::BTreeMap<String, String> = settings
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        let install_env = roost_cli::deploy::apply_release::install_environment(&env, &decided);
        let program = self.root.join("versions/v3/bin/roost");
        let spec = ServiceSpec::resolve(ServiceRole::Coordinator, &install_env, platform, &program)
            .expect("a coordinator spec resolves against a complete environment");
        let definition = render_definition(&spec, platform).expect("the definition renders");
        let unit = roost_host::coord_service_path(&env, platform).expect("the unit path resolves");
        std::fs::create_dir_all(unit.parent().expect("a parent")).expect("the unit dir exists");
        std::fs::write(&unit, definition).expect("the definition is installed");
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn empty_installed() -> InstalledEnvironment {
    InstalledEnvironment::new()
}

#[test]
fn the_door_comes_from_the_installed_definition_and_the_shell_cannot_override_it() {
    let tree = TempTree::new("installed-wins");
    tree.install_coordinator(
        HostPlatform::Linux,
        &[
            (ENV_COORDINATOR_PUBLIC_URL, "https://api.installed.example"),
            (ENV_WEB_PUBLIC_URL, "https://web.installed.example"),
            (ENV_COORDINATOR_DB, "/var/lib/roost/coordinator.sqlite"),
        ],
    );
    let ambient = MapEnv::new().with(
        roost_worker::runtime::boot::ENV_COORDINATOR_URL,
        "https://somewhere-else.example",
    );

    let installed = installed_coordinator(&tree.environment(), HostPlatform::Linux);
    assert!(
        !installed.is_empty(),
        "an installed unit is read back through the same parser `roost status` uses"
    );
    let dialed = dial_url(&installed, &ambient).expect("a declared door resolves");
    assert_eq!(
        dialed, "https://api.installed.example",
        "the installed definition is authoritative; a shell that exported a different door must \
         not enroll the next machine somewhere else"
    );
}

#[test]
fn a_machine_with_no_installed_coordinator_has_no_door_to_enrol_against() {
    let tree = TempTree::new("no-install");
    let installed = installed_coordinator(&tree.environment(), HostPlatform::Linux);
    assert!(
        installed.is_empty(),
        "an account with no coordinator unit declares nothing"
    );
    assert_eq!(
        dial_url(&installed, &tree.environment()),
        None,
        "Roost derives no door, so there is nothing to print"
    );
}

#[test]
fn a_declared_loopback_door_is_refused_because_another_machine_cannot_reach_it() {
    // A worker dialing `http://127.0.0.1` from another machine reaches that
    // machine's own loopback, which is nothing.
    let installed: InstalledEnvironment = [(
        ENV_COORDINATOR_PUBLIC_URL.to_string(),
        "http://127.0.0.1:4113".to_string(),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        dial_url(&installed, &MapEnv::new()),
        None,
        "a plaintext, loopback declaration is not a door another machine can dial"
    );
}

#[test]
fn windows_is_refused_at_the_argument_and_the_refusal_explains_why() {
    let refusal = EnrollmentPlatform::from_name("windows")
        .expect_err("v3 has no Windows host install to enroll");
    assert!(
        refusal.contains("macos or linux"),
        "the refusal names what is accepted: {refusal}"
    );
    assert!(
        refusal.contains("no Windows host install"),
        "the refusal explains why rather than just rejecting: {refusal}"
    );
    assert_eq!(
        EnrollmentPlatform::from_name("macos").expect("macos is offered"),
        EnrollmentPlatform::Macos
    );
    assert_eq!(
        EnrollmentPlatform::from_name("linux").expect("linux is offered"),
        EnrollmentPlatform::Linux
    );
}

#[test]
fn the_printed_command_quotes_every_value_it_hands_the_shell() {
    let command = shell_enrollment_command(
        "https://roost.example.com",
        "roost_bt_deadbeef",
        "build box",
    );
    assert!(command.starts_with("curl -fsSL https://"), "{command}");
    assert!(command.ends_with(" bash"), "{command}");
    assert!(
        command.contains("ROOST_COORDINATOR_URL='https://roost.example.com'"),
        "{command}"
    );
    assert!(
        command.contains("ROOST_BOOTSTRAP_TOKEN='roost_bt_deadbeef'"),
        "{command}"
    );
    assert!(
        command.contains("ROOST_WORKER_LABEL='build box'"),
        "{command}"
    );
}

#[test]
fn a_hostile_url_or_label_cannot_escape_its_quotes_into_the_shell() {
    let url = "https://a.example'; rm -rf ~; echo '";
    let label = "box'; touch /tmp/pwned; '";
    let command = shell_enrollment_command(url, "roost_bt_x", label);

    // The property is what a POSIX shell would DO with the line, so the words
    // are read the way a shell reads them. A whitespace split cannot answer
    // this: the payload's spaces sit inside quotes, so a split tears one
    // assignment into fragments that look like separate words and proves
    // nothing, while a reader that ignored quoting would call `rm -rf ~` a
    // word when the shell never sees one.
    let assignments = command
        .split("curl -fsSL ")
        .nth(1)
        .expect("the command names the script")
        // `… | NAME=VALUE … bash`: the shell's own words are the SECOND half.
        // The first is the script URL, which is not an assignment.
        .split(" | ")
        .nth(1)
        .expect("the command pipes the script into a shell")
        .split(" bash")
        .next()
        .expect("the command ends with bash")
        .trim();
    let words = shell_words(assignments);

    let mut values = std::collections::BTreeMap::new();
    for (raw, plain) in &words {
        let (name, spelling) = raw
            .split_once('=')
            .unwrap_or_else(|| panic!("every assignment is NAME=VALUE: {raw:?}"));
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
            "every assignment names a variable: {raw:?}"
        );
        assert!(
            spelling.starts_with('\'') && spelling.ends_with('\''),
            "every value is exactly one single-quoted word, whatever it carries: {raw:?}"
        );
        // The same first `=` separates name from value in the expanded word,
        // because the quotes that made the value one shell word were removed
        // from it and the separator was not.
        let value = plain
            .split_once('=')
            .map_or(plain.as_str(), |(_, value)| value);
        values.insert(name.to_string(), value.to_string());
    }
    assert_eq!(
        values.get("ROOST_COORDINATOR_URL").map(String::as_str),
        Some(url),
        "the hostile URL reaches the shell as one word, characters and all"
    );
    assert_eq!(
        values.get("ROOST_WORKER_LABEL").map(String::as_str),
        Some(label)
    );
    assert_eq!(
        values.get("ROOST_BOOTSTRAP_TOKEN").map(String::as_str),
        Some("roost_bt_x")
    );
    assert_eq!(
        words.len(),
        3,
        "three assignments and nothing else, so the payload introduced no fourth word: {words:?}"
    );
}

/// The words of a POSIX shell command line as a shell would expand them,
/// paired with the exact text of each word.
///
/// Single quotes are literal, a backslash outside quotes escapes the next
/// character, and whitespace outside quotes separates. This is the `'\''`
/// reading: the close-quote, the escaped quote and the open-quote are three
/// separate things, and a reader that treats the four characters as a token
/// inside one quoted run puts the quote in the wrong place.
///
/// Not a general parser: it is a reader for one generated line, and it refuses
/// a line it cannot account for rather than guessing at it.
fn shell_words(line: &str) -> Vec<(String, String)> {
    let characters: Vec<char> = line.chars().collect();
    let mut words = Vec::new();
    let mut raw = String::new();
    let mut plain = String::new();
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            character if character.is_whitespace() => {
                if !raw.is_empty() {
                    words.push((std::mem::take(&mut raw), std::mem::take(&mut plain)));
                }
                index += 1;
            }
            '\'' => {
                let opened = index;
                index += 1;
                let start = index;
                while index < characters.len() && characters[index] != '\'' {
                    index += 1;
                }
                assert!(
                    index < characters.len(),
                    "the line ends inside a quoted word: {line:?}"
                );
                plain.extend(characters[start..index].iter());
                raw.extend(characters[opened..=index].iter());
                index += 1;
            }
            '\\' => {
                let opened = index;
                index += 1;
                let escaped = characters
                    .get(index)
                    .unwrap_or_else(|| panic!("the line ends on a backslash: {line:?}"));
                plain.push(*escaped);
                raw.extend(characters[opened..=index].iter());
                index += 1;
            }
            other => {
                plain.push(other);
                raw.push(other);
                index += 1;
            }
        }
    }
    if !raw.is_empty() {
        words.push((raw, plain));
    }
    words
}

#[test]
fn an_unnamed_machine_gets_a_command_with_no_label_to_guess_at() {
    let command = shell_enrollment_command("https://roost.example.com", "roost_bt_x", "");
    assert!(!command.contains("ROOST_WORKER_LABEL"), "{command}");
    assert!(command.contains("ROOST_COORDINATOR_URL="), "{command}");
}

/// The installed definition really is the only place a door is read from: a
/// tree with a unit file that declares nothing yields no door, whatever the
/// shell says, and the refusal names the three variables it looked for.
#[test]
fn a_unit_that_declares_no_door_leaves_the_shell_as_the_only_remaining_source() {
    let tree = TempTree::new("blank-unit");
    let unit = roost_host::coord_service_path(&tree.environment(), HostPlatform::Linux)
        .expect("the unit path resolves");
    std::fs::create_dir_all(unit.parent().expect("a parent")).expect("the unit dir exists");
    std::fs::write(&unit, "# nothing declared\n").expect("a blank unit is installed");

    let installed = installed_coordinator(&tree.environment(), HostPlatform::Linux);
    assert!(
        installed.is_empty(),
        "a unit with no ROOST_ entry declares nothing"
    );
    let ambient = MapEnv::new().with(ENV_WEB_PUBLIC_URL, "https://only-in-the-shell.example");
    assert_eq!(
        dial_url(&installed, &ambient),
        Some("https://only-in-the-shell.example".to_string()),
        "a host with no installed definition falls back to the shell, which is the documented \
         precedence"
    );
    assert_eq!(
        dial_url(&installed, &MapEnv::new()),
        None,
        "and with neither there is nothing to print"
    );
}

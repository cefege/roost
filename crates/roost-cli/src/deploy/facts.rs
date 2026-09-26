//! What the target says about itself, answered by the target's own binary.
//! Called by the deploy command through the hidden `roost __remote-facts`, and
//! by nothing else; depends on `roost-host`'s path resolution and on the status
//! group's own installed-definition reader.
//!
//! This exists to delete a whole class of arithmetic. A deploying box that
//! derived the target's definition path from its own idea of the target's home
//! and platform would be right until an operator overrode `ROOST_SERVICE_DIR` —
//! and then it would read a file that is not the install, resolve paths that are
//! not the install's, and write a definition somewhere the install never looks.
//! So the deploying box asks, and the answer comes from the release it just
//! staged, running with the target's own environment.
//!
//! One JSON line, hidden, no arguments. Its fields are the facts a deploy cannot
//! proceed without and cannot safely guess.

use roost_host::{EnvSource, HostPlatform, ProtocolError};
use serde::{Deserialize, Serialize};

use crate::services::service_spec::ServiceRole;
use crate::status::service_definition::{InstalledEnvironment, parse_installed_environment};

/// The facts, as the target reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFacts {
    /// Always 1. A target whose facts this build cannot read is a target whose
    /// paths this build would guess.
    pub schema: u32,
    /// The target's platform, as `roost-host` names it.
    pub platform: String,
    /// The target's resolved home directory.
    pub home: String,
    /// The label the target's worker service is installed under.
    pub worker_label: String,
    /// The installed worker definition, absolute.
    pub definition_path: String,
    /// The directory this install keeps releases under, resolved the way the
    /// install resolves it.
    pub release_root: String,
    /// The service directory that holds the machine transaction and the deploy
    /// journal.
    pub service_dir: String,
    /// The `roost` the installed worker definition runs, absolute. A keeper
    /// refresh holds this machine's transaction through the INSTALLED release
    /// rather than through a staged one, because it changes no release.
    pub installed_program: String,
    /// Every `ROOST_*` entry the installed definition carries, so a deploy can
    /// reuse a prior install's coordinator URL and identity without an operator
    /// re-exporting them.
    pub installed_environment: InstalledEnvironment,
}

/// The schema this build reads and writes.
pub const FACTS_SCHEMA: u32 = 1;

/// Read the facts from this machine, for this install.
pub fn read(env: &dyn EnvSource, platform: HostPlatform) -> Result<RemoteFacts, ProtocolError> {
    let definition_path = ServiceRole::Worker.definition_path(env, platform)?;
    let installed = std::fs::read_to_string(&definition_path)
        .ok()
        .map(|definition| parse_installed_environment(&definition, platform))
        .unwrap_or_default();
    let release_root = roost_host::roost_versions_dir(env, platform)?;
    // The installed program is read out of the installed definition rather than
    // derived from the default paths, because an operator who moved the release
    // root moved it there.
    let installed_program = std::fs::read_to_string(&definition_path)
        .ok()
        .and_then(|definition| {
            crate::deploy::installed::installed_release_dir(&definition, platform)
        })
        .map(|release| {
            release
                .join(crate::deploy::apply_release::RELEASE_BIN_DIR)
                .join(crate::deploy::apply_release::ROOST_PROGRAM)
                .display()
                .to_string()
        })
        .unwrap_or_default();
    Ok(RemoteFacts {
        schema: FACTS_SCHEMA,
        platform: platform.as_str().to_string(),
        home: env
            .home_dir()
            .map(|home| home.display().to_string())
            .unwrap_or_default(),
        worker_label: ServiceRole::Worker.service_label(env, platform)?,
        definition_path: definition_path.display().to_string(),
        release_root: release_root.display().to_string(),
        service_dir: roost_host::roost_service_dir(env, platform)?
            .display()
            .to_string(),
        installed_program,
        installed_environment: installed,
    })
}

/// A POSIX snippet that runs one of the hidden target-side subcommands from the
/// release the target has INSTALLED, locating it from the target's own
/// definition.
///
/// This exists for exactly one caller: keeper maintenance, which changes no
/// release and so has no staged one to name. It is shell rather than a path sent
/// from here because the deploying box does not know where this install keeps
/// its program — that is the fact the subcommand exists to report, so asking for
/// it by asking for the path is circular. The locator is deliberately small and
/// reads the two formats' own conventions: `ExecStart="…"` is quoted because the
/// writer quotes that directive, and a launchd agent's program is the first
/// absolute `<string>` in its argument array.
///
/// Exit 66 is "there is no installed worker here", which the caller reports as a
/// first install rather than as a failure.
pub fn installed_launcher(subcommand: &str) -> String {
    let quoted = roost_platform::posix_shell_quote(subcommand);
    format!(
        "set -e; \
         find_roost() {{ \
           for spec in \
             \"${{ROOST_WORKER_UNIT:-}}\" \
             \"$HOME/.config/systemd/user/${{ROOST_WORKER_AGENT_LABEL:-roost3-worker}}.service\" \
             \"$HOME/Library/LaunchAgents/${{ROOST_WORKER_AGENT_LABEL:-com.roost.worker-v3}}.plist\"; do \
             test -n \"$spec\" || continue; test -f \"$spec\" || continue; \
             program=$(sed -n 's/^ExecStart=\"\\([^\"]*\\)\".*/\\1/p' \"$spec\" | head -1); \
             if test -z \"$program\"; then \
               program=$(sed -n 's|.*<string>\\(/[^<]*\\)</string>.*|\\1|p' \"$spec\" | head -1); \
             fi; \
             if test -n \"$program\" && test -x \"$program\"; then printf '%s\\n' \"$program\"; return 0; fi; \
           done; return 1; \
         }}; \
         program=$(find_roost) || exit 66; \
         exec \"$program\" {quoted}"
    )
}

/// Encode for the wire: one line, no trailing newline.
pub fn encode(facts: &RemoteFacts) -> Result<String, String> {
    serde_json::to_string(facts).map_err(|error| error.to_string())
}

/// Read the facts out of a remote command's stdout.
pub fn decode(output: &str) -> Result<RemoteFacts, String> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(FACTS_PREFIX))
        .ok_or_else(|| {
            format!(
                "the target did not report what it is; its last output was: {}",
                if output.trim().is_empty() {
                    "(nothing)"
                } else {
                    output.trim()
                }
            )
        })?;
    let facts: RemoteFacts = serde_json::from_str(&line[FACTS_PREFIX.len()..])
        .map_err(|error| format!("the target's facts are unreadable: {error}"))?;
    if facts.schema != FACTS_SCHEMA {
        return Err(format!(
            "the target reported facts schema {} and this build reads {FACTS_SCHEMA}; its roost is \
             not the release this deploy staged",
            facts.schema
        ));
    }
    Ok(facts)
}

/// The marker the target prefixes its facts line with.
pub const FACTS_PREFIX: &str = "RoostFacts=";

/// What the installed-release launcher exits with when this machine has no worker
/// installed. Not 66 by taste: it is the launcher's own "could not find it" code,
/// and a caller that reported it as a transport failure would tell an operator
/// their ssh is broken when their machine is simply fresh.
pub const NO_INSTALLED_RELEASE: i32 = 66;

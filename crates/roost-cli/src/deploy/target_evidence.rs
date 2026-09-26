//! The one probe a deploy runs on a target to decide whether staging is safe,
//! and what its output means. Called by the deploy command; depends on
//! `roost-platform`'s POSIX quoting and on the service probe's own vocabulary,
//! and on nothing else in the deploy group.
//!
//! The whole evidence rule exists because the two obvious gates describe the
//! wrong machine. Gating on the coordinator's registry row plus a `test -e`
//! against the service definition both look conservative and both are about the
//! coordinator's knowledge rather than the host's: a stale row is a statement
//! about what the coordinator last heard, and a file that exists says nothing
//! about whether the service manager ever loaded it, whether a worker process is
//! alive, or whether a keeper is still holding PTYs. So the target is asked, and
//! it stages only on positive proof of emptiness.
//!
//! Every unknown fails closed. An unreachable service manager reads exactly like
//! a stopped one in its own output — on darwin because `launchctl print` fails
//! the same way for both, on Linux because `systemctl show` exits 0 for a unit it
//! has never heard of — so reachability is asked separately and the two answers
//! are never collapsed. A keeper SOCKET FILE is deliberately not evidence at all:
//! it outlives the keeper that created it, so it can neither prove nor disprove
//! anything the process counts do not.

use roost_host::HostPlatform;
use roost_platform::posix_shell_quote;
use roost_worker::runtime::boot::KEEPER_SOCKET_NAME;

/// What the target reported about itself.
///
/// Absence is only ever read from a positive observation: a `false` in
/// `service_observed` or `processes_observed` means the probe could not tell,
/// which is never a licence to stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetEvidence {
    /// The transport worked and the definition question was answered.
    pub service_observed: bool,
    /// A worker service definition is installed on the target.
    pub service_installed: bool,
    /// The service manager answered at all.
    pub service_state_observed: bool,
    /// The service manager says the worker is running with a live pid.
    pub service_running: bool,
    /// The process counts were collected.
    pub processes_observed: bool,
    /// How many keeper processes the target is running.
    pub keeper_processes: u32,
    /// How many channel processes those keepers are parenting — one per live
    /// PTY.
    pub keeper_channel_processes: u32,
}

/// The one probe command the target runs, and what its output means.
///
/// The order is the design. The two checks that need no extra tooling are
/// emitted before the process probe, so a host with no `pgrep` still reports
/// what it can and is refused for the part it could not. And `pgrep -f` sees
/// this deploy's own ssh command line, so the socket name's leading character is
/// bracketed into a character class: the pattern then cannot match the literal
/// text that carries it.
pub fn target_worker_evidence_command(platform: HostPlatform, label: &str, spec: &str) -> String {
    let state = service_state_command_text(platform, label);
    let domain_query = roost_cli_domain_query(platform);
    let pattern = bracketed_pattern(KEEPER_SOCKET_NAME);
    format!(
        "spec={spec}; \
         case \"$spec\" in /*) service=\"$spec\";; *) service=\"$HOME/$spec\";; esac; \
         if test -e \"$service\" || test -L \"$service\"; \
         then echo RoostServiceInstalled=yes; else echo RoostServiceInstalled=no; fi; \
         service_output=$( {state} 2>&1 ); service_status=$?; \
         printf '%s\\n' \"$service_output\"; \
         if test \"$service_status\" -eq 0{domain_query}; \
         then echo RoostServiceState=observed; fi; \
         command -v pgrep >/dev/null 2>&1 || exit 0; \
         keeper_pids=$(pgrep -f {pattern} || true); \
         echo \"RoostKeeperProcesses=$(printf '%s' \"$keeper_pids\" | grep -c . || true)\"; \
         keeper_list=$(printf '%s' \"$keeper_pids\" | tr '\\n' ',' | sed 's/,*$//'); \
         if test -z \"$keeper_list\"; then keeper_children=0; \
         else keeper_children=$(pgrep -P \"$keeper_list\" | grep -c . || true); fi; \
         echo \"RoostKeeperChannelProcesses=$keeper_children\"; \
         echo RoostTargetEvidence=complete",
        spec = posix_shell_quote(spec),
        state = state,
        domain_query = domain_query,
        pattern = posix_shell_quote(&pattern),
    )
}

fn service_state_command_text(platform: HostPlatform, label: &str) -> String {
    render_argv(&crate::status::service_probe::service_state_command(
        label, platform,
    ))
}

fn roost_cli_domain_query(platform: HostPlatform) -> &'static str {
    match platform {
        HostPlatform::MacOs => " || launchctl print-disabled \"gui/$(id -u)\" >/dev/null 2>&1",
        _ => "",
    }
}

/// Render argv as one shell fragment, with every element quoted. A remote
/// command is necessarily a shell string; this is where that string stops being
/// a source of injection.
pub fn render_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|element| posix_shell_quote(element))
        .collect::<Vec<String>>()
        .join(" ")
}

/// The keeper's socket name as a pattern that cannot match the command line
/// carrying it.
///
/// The name already ends in `.sock`, so nothing is appended: an extra suffix here
/// would make the pattern match nothing at all, and a probe that counts zero
/// keepers because its pattern is wrong is a probe that stages onto a live
/// machine.
fn bracketed_pattern(name: &str) -> String {
    let mut characters = name.chars();
    let first = characters.next().unwrap_or('m');
    // Every remaining character that a `pgrep -f` pattern would read as syntax is
    // escaped, the dot included: an unescaped `.` matches any character, so a
    // pattern for `mux-keeper.sock` would also match a directory that merely
    // starts with the same letters.
    let rest: String = characters
        .map(|character| {
            if r"\.[]{}()*+?^$|".contains(character) {
                format!("\\{character}")
            } else {
                character.to_string()
            }
        })
        .collect();
    format!("[{first}]{rest}")
}

/// Read the probe's output into evidence.
pub fn parse_target_evidence(exit: i32, output: &str, platform: HostPlatform) -> TargetEvidence {
    let field = |name: &str| -> Option<String> {
        output.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_string())
        })
    };
    let transport_ok = exit == 0;
    let installed = field("RoostServiceInstalled");
    let keepers = field("RoostKeeperProcesses");
    let children = field("RoostKeeperChannelProcesses");
    let complete = field("RoostTargetEvidence").as_deref() == Some("complete");
    let counts = keepers
        .as_deref()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    TargetEvidence {
        service_observed: transport_ok && installed.is_some(),
        service_installed: installed.as_deref() == Some("yes"),
        service_state_observed: transport_ok
            && field("RoostServiceState").as_deref() == Some("observed"),
        service_running: transport_ok
            && crate::status::service_probe::service_is_running(output, platform),
        processes_observed: transport_ok && complete && keepers.is_some() && children.is_some(),
        keeper_processes: counts,
        keeper_channel_processes: children
            .as_deref()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0),
    }
}

/// What the evidence decides: whether the registry's refusal stands, and the
/// sentence that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceVerdict {
    /// Nothing on the target is there for a staged release to destroy.
    Permitted(String),
    /// The refusal stands.
    Refused(String),
}

/// Decide the registry refusal against the target's own evidence.
///
/// Every `Refused` arm names what was not proven rather than asserting the
/// opposite, because "the target could not prove that no keeper is holding
/// channels" and "a keeper is holding channels" call for different operator
/// actions and only one of them is a bug.
pub fn installed_service_verdict(
    refusal: &str,
    host: &str,
    evidence: TargetEvidence,
) -> EvidenceVerdict {
    if !evidence.service_observed {
        return EvidenceVerdict::Refused(format!(
            "{refusal}; {host} did not report whether a worker service is installed"
        ));
    }
    if !evidence.service_installed {
        return EvidenceVerdict::Permitted(format!("no worker service is installed on {host}"));
    }
    if !evidence.service_state_observed {
        return EvidenceVerdict::Refused(format!(
            "{refusal}; the service manager on {host} did not report the worker's state"
        ));
    }
    if evidence.service_running {
        return EvidenceVerdict::Refused(format!(
            "{refusal}; the worker service on {host} is running and can prove admission"
        ));
    }
    if !evidence.processes_observed {
        return EvidenceVerdict::Refused(format!(
            "{refusal}; {host} could not prove that no keeper is holding channels"
        ));
    }
    if evidence.keeper_channel_processes > 0 {
        return EvidenceVerdict::Refused(format!(
            "{refusal}; a keeper on {host} still holds {} channel process(es)",
            evidence.keeper_channel_processes
        ));
    }
    EvidenceVerdict::Permitted(format!(
        "the worker service on {host} is installed but not running, and its {} keeper process(es) \
         hold no channel",
        evidence.keeper_processes
    ))
}

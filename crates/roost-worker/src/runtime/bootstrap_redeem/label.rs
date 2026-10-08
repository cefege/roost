//! The name this machine answers to in the fleet, and the order the three
//! sources that can supply one are consulted in. Called by
//! [`super::enroll`], which is the only caller in the product; the ordering is
//! spelled out here because it is a regression, not a preference.
//!
//! The machine's OWN name outranks the shell's `HOSTNAME` because macOS does
//! not set the shell's by default. An ordering that consulted `HOSTNAME` first
//! fell through to the same final fallback on every macOS host at once, and
//! every worker in a fleet registered as the literal string `"worker"`.
//! Ports v2 `apps/worker/src/host/install.ts`.

use roost_host::{EnvSource, HostPlatform};

use super::EnrollmentError;

/// The label the fleet shows this machine under.
///
/// Not `roost_host::WORKER_LABEL_ENV`: that one overrides the worker's launchd
/// or systemd UNIT name, and `roost-host` says so where it declares its own.
/// This is the name the machine answers to everywhere else.
pub const ENV_WORKER_LABEL: &str = "ROOST_WORKER_LABEL";

/// The shell's own name for the host: the last resort, and deliberately not
/// the first.
const HOSTNAME_ENV: &str = "HOSTNAME";

/// Where the machine's own name comes from.
///
/// A trait for the same reason [`crate::host::identity::IdentitySources`] is
/// one: the answer a label falls back to is a fact about the box the test did
/// not run on, and reading it on every test would make a label test a
/// hostname test.
pub(super) trait LabelSources {
    /// The host name of the machine under `platform`, or `None` when it has
    /// none to give.
    fn host_name(&self, platform: HostPlatform) -> Option<String>;
}

/// The label source that reads this host.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct HostLabelSources;

impl LabelSources for HostLabelSources {
    fn host_name(&self, platform: HostPlatform) -> Option<String> {
        match platform {
            // The kernel's own node name, which is exactly what the hostname
            // syscall returns, exported through a file of fixed size.
            HostPlatform::Linux => std::fs::read_to_string("/proc/sys/kernel/hostname").ok(),
            // macOS keeps no such file. `hostname(1)` prints the same answer;
            // it is spawned rather than linked because the syscall is `unsafe`
            // and this workspace forbids unsafe.
            HostPlatform::MacOs => std::process::Command::new("/bin/hostname")
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok()),
            // Windows exports the NetBIOS computer name to every process.
            HostPlatform::Windows => std::env::var("COMPUTERNAME")
                .ok()
                .filter(|name| !name.trim().is_empty()),
        }
    }
}

/// The label this worker registers under.
///
/// `ROOST_WORKER_LABEL` wins, then the machine's own name, then `HOSTNAME`.
///
/// Every value is trimmed, and one that is empty once trimmed counts as unset
/// at every step, the way [`WorkerBoot::resolve`] already reads an empty path.
/// So an operator who exported `ROOST_WORKER_LABEL=` — which is what an unset
/// shell variable writes into a systemd `Environment=` line — gets the
/// machine's name rather than a blank row in the sidebar.
///
/// There is no fourth fallback. A host that can answer none of the three has
/// no name to give, and registering it under the literal `"worker"` is the
/// fleet-wide collision the ordering above exists to prevent, so that is a
/// refusal an operator can read.
///
/// [`WorkerBoot::resolve`]: crate::runtime::boot::WorkerBoot::resolve
pub(super) fn resolve_worker_label(
    env: &dyn EnvSource,
    platform: HostPlatform,
    sources: &dyn LabelSources,
) -> Result<String, EnrollmentError> {
    if let Some(label) = named(env.get(ENV_WORKER_LABEL)) {
        return Ok(label);
    }
    if let Some(name) = named(sources.host_name(platform)) {
        return Ok(name);
    }
    named(env.get(HOSTNAME_ENV)).ok_or(EnrollmentError::NoLabel)
}

/// A value that is there and says something, or nothing.
///
/// Trimmed, because surrounding whitespace is never part of a value here:
/// `/proc/sys/kernel/hostname` and `hostname(1)` both end their answer with a
/// newline, and a name taken raw registered ovh1 as `"ovh1-8c32g\n"`, which
/// broke the line of every readout that printed it.
pub(super) fn named(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use roost_host::{EnvSource, HostPlatform, MapEnv};

    use super::{ENV_WORKER_LABEL, HostLabelSources, LabelSources, named, resolve_worker_label};
    use crate::runtime::bootstrap_redeem::EnrollmentError;

    /// A host with a name the test chooses, or with none at all.
    struct FakeHost(Option<&'static str>);

    impl LabelSources for FakeHost {
        fn host_name(&self, _platform: HostPlatform) -> Option<String> {
            self.0.map(str::to_owned)
        }
    }

    fn label_of(
        env: &dyn EnvSource,
        host: Option<&'static str>,
    ) -> Result<String, EnrollmentError> {
        resolve_worker_label(env, HostPlatform::MacOs, &FakeHost(host))
    }

    #[test]
    fn the_operator_label_outranks_the_machine_name() {
        let env = MapEnv::new()
            .with(ENV_WORKER_LABEL, "studio-mac")
            .with("HOSTNAME", "mikes-air.local");
        assert_eq!(
            label_of(&env, Some("mikes-air")),
            Ok("studio-mac".to_string())
        );
    }

    #[test]
    fn the_machine_name_outranks_the_shell_hostname() {
        // The macOS regression: the shell's HOSTNAME is unset by default
        // there, and an ordering that consulted it first left a whole fleet
        // under one name.
        let env = MapEnv::new().with("HOSTNAME", "stale-import");
        assert_eq!(
            label_of(&env, Some("mike-m5-air")),
            Ok("mike-m5-air".to_string())
        );
    }

    #[test]
    fn the_shell_hostname_is_the_last_resort_not_the_first() {
        let env = MapEnv::new().with("HOSTNAME", "build-box");
        assert_eq!(label_of(&env, None), Ok("build-box".to_string()));
    }

    #[test]
    fn an_empty_value_is_no_value_at_every_step() {
        let env = MapEnv::new()
            .with(ENV_WORKER_LABEL, "")
            .with("HOSTNAME", "");
        assert_eq!(
            label_of(&env, Some("mike-m5-air")),
            Ok("mike-m5-air".to_string())
        );
        assert_eq!(label_of(&env, None), Err(EnrollmentError::NoLabel));
    }

    #[test]
    fn a_host_with_no_name_at_all_is_refused_rather_than_called_worker() {
        assert_eq!(
            label_of(&MapEnv::new(), None),
            Err(EnrollmentError::NoLabel)
        );
    }

    #[test]
    fn a_machine_name_is_registered_without_the_newline_its_source_ends_with() {
        assert_eq!(
            label_of(&MapEnv::new(), Some("ovh1-8c32g\n")),
            Ok("ovh1-8c32g".to_string())
        );
        let padded = MapEnv::new().with(ENV_WORKER_LABEL, " studio-mac\n");
        assert_eq!(label_of(&padded, None), Ok("studio-mac".to_string()));
        let shell_named = MapEnv::new().with("HOSTNAME", "build-box");
        assert_eq!(
            label_of(&shell_named, Some("\n")),
            Ok("build-box".to_string()),
            "a host name that is only a newline is no name, not a blank label"
        );
    }

    #[test]
    fn this_host_can_name_itself_and_windows_reads_its_computer_name() {
        let platform = roost_host::supported_host_platform().expect("a supported host");
        let name = HostLabelSources.host_name(platform).expect("a host name");
        assert!(!name.trim().is_empty(), "an empty host name is no name");
        assert_eq!(
            HostLabelSources.host_name(HostPlatform::Windows),
            std::env::var("COMPUTERNAME")
                .ok()
                .filter(|name| !name.trim().is_empty())
        );
    }

    #[test]
    fn an_empty_value_is_dropped_wherever_it_arrives() {
        assert_eq!(named(Some(String::new())), None);
        assert_eq!(named(Some(" \n".to_string())), None);
        assert_eq!(named(None), None);
        assert_eq!(named(Some(" x\n".to_string())), Some("x".to_string()));
    }
}

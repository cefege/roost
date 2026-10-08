//! Choosing the definition format a platform reads, and proving a rendered one
//! is whole before it replaces the installed file: a systemd unit, a launchd
//! plist, or a Windows Scheduled Task's launcher script. Called by install.rs
//! and by the deploy transaction; the renderers are reached only from here.
//!
//! Both managers read the file they were pointed at, and both report nothing
//! for a definition they never loaded: a coordinator simply never appears, and
//! a worker never appears. So a staged definition is proved complete and then
//! renamed over the target, never redirected into.

use roost_host::{HostPlatform, ProtocolResult};

use crate::services::launchd_plist::{plist_is_complete, render_launchd_plist};
use crate::services::scheduled_task::{launcher_is_complete, render_launcher};
use crate::services::service_spec::ServiceSpec;
use crate::services::systemd_unit::{
    render_systemd_unit, require_absolute_program, unit_is_complete,
};

/// The definition text for `spec` in the format `platform` reads.
pub fn render_definition(spec: &ServiceSpec, platform: HostPlatform) -> ProtocolResult<String> {
    // A program path that is not absolute is refused for every platform: a
    // unit resolves `ExecStart` through the service's `PATH`, a plist resolves
    // a relative path against whatever launchd's working directory happened to
    // be, and a launcher against the task's.
    require_absolute_program(spec)?;
    match platform {
        HostPlatform::Linux => render_systemd_unit(spec),
        HostPlatform::MacOs => render_launchd_plist(spec),
        HostPlatform::Windows => render_launcher(spec),
    }
}

/// Whether `text` is a whole definition rather than a prefix of one.
pub fn definition_is_complete(text: &str, platform: HostPlatform) -> bool {
    match platform {
        HostPlatform::Linux => unit_is_complete(text),
        HostPlatform::MacOs => plist_is_complete(text),
        HostPlatform::Windows => launcher_is_complete(text),
    }
}

/// The modes an installed definition file carries. A definition can name a
/// bootstrap grant, so it is never world-readable even though a unit is
/// usually.
pub const DEFINITION_MODE: u32 = 0o600;

/// The name a rendered definition is given beside its target while it is being
/// proved. It ends in a per-process suffix so two installs on one machine
/// cannot stage onto the same path.
pub fn staging_name(file_name: &str, attempt: u32) -> String {
    format!(".{file_name}.staged.{}.{attempt}", std::process::id())
}

//! The environment entries an installed service definition carries, split into
//! the operator's choices — which a later deploy preserves — and the one-shot
//! authorizations, which a later deploy retires. Called by service_spec.rs,
//! which builds the definition; nothing else spells these names.
//!
//! An entry another crate owns is imported from that owner rather than
//! restated, so a rename cannot land in the writer only. The two entries with
//! no owner yet — the door's origin allowlist and the address the fleet reaches
//! this machine at — are declared here once, and are why this file exists.

use std::path::Path;

use roost_observability::diag::{DIAG_ENABLED_ENV, DIAG_ENABLED_VALUE};
use roost_platform::{AGENT_CONVERSATION_RESTORE_ENV, KEEPER_FORCE_LIVE_RETIRE_ENV};
use roost_protocol::local_ui_door::WORKER_LOCAL_UI_BIND_ENV;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

/// The home directory. Every definition carries it because a service manager
/// starts a process with a near-empty environment, and a service that cannot
/// find its home cannot resolve any default path.
pub const ENV_HOME: &str = "HOME";

/// The search path a started service's own children run with.
pub const ENV_PATH: &str = "PATH";

/// The address the rest of the fleet reaches this machine at. Carried only
/// when the operator set one; a worker derives a reachable address otherwise.
pub const ENV_REACHABLE_ADDR: &str = "ROOST_REACHABLE_ADDR";

/// The name this machine is enrolled under, as the rest of the fleet sees it.
///
/// Deliberately not `roost-host`'s `ROOST_WORKER_AGENT_LABEL`, which overrides
/// the service identity the platform reads: this one names the machine to the
/// fleet and that one names the unit, and an install that conflated them would
/// rename a machine by renaming a service.
pub const ENV_WORKER_LABEL: &str = "ROOST_WORKER_LABEL";

/// The origins allowed to call this worker's loopback door.
pub const ENV_WORKER_LOCAL_UI_ALLOWED_ORIGINS: &str = "ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS";

/// The one-shot pairing grant that enrolls a machine.
pub const ENV_BOOTSTRAP_TOKEN: &str = "ROOST_BOOTSTRAP_TOKEN";

/// The authorizations that reach a service exactly once. A definition still
/// carrying one would re-authorize the destruction it names on every later
/// restart, so a deploy strips them rather than reinstalling them.
pub const ONE_SHOT_AUTHORIZATIONS: [&str; 2] = [ENV_BOOTSTRAP_TOKEN, KEEPER_FORCE_LIVE_RETIRE_ENV];

/// The worker's settings an operator chose and a redeploy must preserve. The
/// conversation-restore opt-in is here rather than in the grant list precisely
/// because it is the opposite: the operator's answer outlives the deploy that
/// carried it.
pub const WORKER_CHOSEN_ENTRIES: [&str; 5] = [
    ENV_COORDINATOR_URL,
    WORKER_LOCAL_UI_BIND_ENV,
    ENV_WORKER_LOCAL_UI_ALLOWED_ORIGINS,
    AGENT_CONVERSATION_RESTORE_ENV,
    ENV_REACHABLE_ADDR,
];

/// True when `name` is an authorization no redeploy may carry forward.
pub fn is_one_shot_authorization(name: &str) -> bool {
    ONE_SHOT_AUTHORIZATIONS.contains(&name)
}

/// The search path an installed service runs with: the account's own `bin`
/// first, so a tool the operator installed the way they installed Roost is the
/// one a shell inside the service finds.
pub fn default_service_path(home: &Path) -> String {
    format!(
        "{}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        home.join(".local/bin").display()
    )
}

/// The diagnostic firehose, off unless the operator turned it on. The accepted
/// value is the observability crate's, not a second spelling of "on".
pub const DIAGNOSTIC_ENV: (&str, &str) = (DIAG_ENABLED_ENV, DIAG_ENABLED_VALUE);

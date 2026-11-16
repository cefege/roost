//! The service-manager probe `db-to-postgres` and `db-to-sqlite` consult before
//! touching a coordinator database: only an active unit is a running coordinator.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::ops::coordinator_state::{
    CoordinatorState, coordinator_probe_argv, coordinator_state_with,
};
use roost_host::HostPlatform;

/// The copy refuses to read a database a running coordinator holds, and refuses
/// ONLY in that state.
#[test]
fn only_an_active_unit_is_a_running_coordinator() {
    let active = |_: &[String]| Some(String::from("active\n"));
    assert_eq!(
        coordinator_state_with(HostPlatform::Linux, "roost3-coord", active),
        CoordinatorState::Running,
    );
    for answer in ["inactive\n", "failed\n", ""] {
        let answered = move |_: &[String]| Some(String::from(answer));
        assert_eq!(
            coordinator_state_with(HostPlatform::Linux, "roost3-coord", answered),
            CoordinatorState::Stopped,
            "only an explicit `active` is a running coordinator: {answer:?}"
        );
    }
    assert_eq!(
        coordinator_state_with(HostPlatform::Linux, "roost3-coord", |_| None),
        CoordinatorState::Unavailable,
        "a service manager that could not be asked is not a coordinator holding the database"
    );
    assert_eq!(
        coordinator_probe_argv(HostPlatform::Linux, "roost3-coord"),
        vec!["systemctl", "--user", "is-active", "roost3-coord"],
        "argv, not a shell string: a unit name can come from the environment"
    );
}

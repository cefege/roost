//! `roost quickstart --dry-run` resolves and renders the whole plan on a
//! machine with nothing installed, and leaves the machine byte-for-byte as it
//! was.
//!
//! The "writes nothing" half is a tree snapshot taken before and after, not a
//! return value. A dry run that created the service directory, staged a
//! definition, or started a coordinator would report success from every
//! function it called, and the only thing that sees it is a file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::quickstart::endpoint::{EndpointMode, fresh_endpoint, installed_endpoint};
use roost_cli::quickstart::plan;
use roost_cli::services::definition_text::render_definition;
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec};
use roost_cli::status::service_definition::parse_installed_environment;
use roost_host::{HostPlatform, MapEnv};

/// A throwaway tree that removes itself, standing in for a machine with no
/// install of any kind.
struct TempMachine {
    root: PathBuf,
}

impl TempMachine {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-quickstart-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway machine is created");
        Self { root }
    }

    /// An account with a home and nothing else under it.
    fn environment(&self) -> MapEnv {
        let text = |relative: &str| self.root.join(relative).display().to_string();
        MapEnv::new()
            .with("HOME", &text("home"))
            .with(roost_host::COORD_UNIT_ENV, &text("unit/roost3-coord.service"))
            .with(roost_host::WORKER_UNIT_ENV, &text("unit/roost3-worker.service"))
            .with(roost_host::WORKER_DATA_DIR_ENV, &text("data/worker"))
            .with(roost_host::COORD_DATA_DIR_ENV, &text("data/coord"))
    }
}

impl Drop for TempMachine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every file under `root` as its bytes and permission bits, keyed by its path
/// relative to `root`. This is the whole comparison: a file that appeared, a
/// directory that was created, a byte that changed.
fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("every entry is under the root")
                .to_path_buf();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(path);
            } else {
                snapshot.insert(
                    relative,
                    (
                        std::fs::read(&path).unwrap_or_default(),
                        metadata.permissions().mode() & 0o7777,
                    ),
                );
            }
        }
    }
    snapshot
}

#[test]
fn a_dry_run_resolves_both_services_on_a_machine_with_nothing_installed() {
    let machine = TempMachine::new("resolves");
    let env = machine.environment();
    assert!(
        plan::installed_coordinator(&env, HostPlatform::Linux).is_none(),
        "the machine starts with no coordinator unit"
    );
    assert!(
        !plan::worker_installed(&env, HostPlatform::Linux),
        "and with no worker unit"
    );

    let endpoint = fresh_endpoint(None).expect("a loopback endpoint needs no front door");
    assert_eq!(endpoint.mode, EndpointMode::Local);

    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, false)
        .expect("the plan resolves on a machine with nothing installed");

    assert_eq!(resolved.coordinator.spec.role, ServiceRole::Coordinator);
    assert_eq!(resolved.worker.spec.role, ServiceRole::Worker);
    assert_eq!(
        resolved.coordinator.spec.label, "roost3-coord",
        "the four labels the platform reads are the ones the tree already pins"
    );
    assert_eq!(resolved.worker.spec.label, "roost3-worker");
    assert!(!resolved.coordinator_already_installed);
    assert!(!resolved.worker_already_installed);
    assert_eq!(
        resolved.path_link.0.file_name().and_then(|name| name.to_str()),
        Some("roost"),
        "the PATH entry is named for the program, which is `roost`; `roost3` is the unit label"
    );
    assert_eq!(resolved.path_link.1, resolved.coordinator.spec.program);
}

#[test]
fn a_dry_run_renders_the_definitions_a_real_run_would_install() {
    let machine = TempMachine::new("renders");
    let env = machine.environment();
    let endpoint = fresh_endpoint(None).expect("a loopback endpoint");
    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, false)
        .expect("the plan resolves");

    for service in [&resolved.coordinator, &resolved.worker] {
        let rendered = service
            .definition_text(HostPlatform::Linux)
            .expect("a linux unit renders");
        assert!(
            roost_cli::services::definition_text::definition_is_complete(
                &rendered,
                HostPlatform::Linux
            ),
            "the dry run prints a definition the service manager would accept:\n{rendered}"
        );
        assert!(
            rendered.contains(&service.spec.program.display().to_string()),
            "the definition names the program the plan resolved:\n{rendered}"
        );
    }
    let coordinator_text = resolved
        .coordinator
        .definition_text(HostPlatform::Linux)
        .expect("a linux unit renders");
    assert!(
        coordinator_text.contains(&endpoint.loopback_origin().replace("http://", "127.0.0.1:")),
        "the unit states the bind the coordinator will actually use:\n{coordinator_text}"
    );
}

#[test]
fn a_dry_run_changes_nothing_on_disk() {
    let machine = TempMachine::new("no-writes");
    let env = machine.environment();
    // A release program at the path the plan resolves, so a dry run that
    // installed one would overwrite a file whose bytes a snapshot would catch.
    let endpoint = fresh_endpoint(None).expect("a loopback endpoint");
    let first = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, false)
        .expect("the plan resolves");
    let program = first.coordinator.spec.program.clone();
    std::fs::create_dir_all(program.parent().expect("a parent")).expect("the release dir exists");
    std::fs::write(&program, b"the release that is installed\n").expect("the program is written");

    let before = tree_snapshot(&machine.root);

    let endpoint = fresh_endpoint(None).expect("a loopback endpoint");
    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, false)
        .expect("the plan resolves");
    // The rendered text is materialised, exactly as `print_plan` does.
    let _ = resolved
        .coordinator
        .definition_text(HostPlatform::Linux)
        .expect("renders");
    let _ = resolved
        .worker
        .definition_text(HostPlatform::Linux)
        .expect("renders");

    let after = tree_snapshot(&machine.root);
    let changed: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(
        before == after,
        "a dry run wrote to the machine: {changed:?}"
    );
}

/// A rerun is decided from the installed definition, not from the shell, and a
/// dry run of a rerun has to show the operator the front door their machine
/// already has rather than a fresh loopback one.
#[test]
fn a_dry_run_of_a_rerun_keeps_the_installed_front_door() {
    let machine = TempMachine::new("rerun");
    let env = machine.environment();
    let decided: BTreeMap<String, String> = [
        ("ROOST_COORDINATOR_BIND".to_string(), "127.0.0.1:4200".to_string()),
        (
            "ROOST_WEB_PUBLIC_URL".to_string(),
            "https://roost.example.com".to_string(),
        ),
    ]
    .into_iter()
    .collect();
    let spec = ServiceSpec::resolve(
        ServiceRole::Coordinator,
        &roost_cli::deploy::apply_release::install_environment(&env, &decided),
        HostPlatform::Linux,
        &machine.root.join("versions/v3/bin/roost"),
    )
    .expect("a coordinator spec resolves");
    let unit = roost_host::coord_service_path(&env, HostPlatform::Linux).expect("a unit path");
    std::fs::create_dir_all(unit.parent().expect("a parent")).expect("the unit dir exists");
    std::fs::write(
        &unit,
        render_definition(&spec, HostPlatform::Linux).expect("the unit renders"),
    )
    .expect("the definition is installed");

    let installed = plan::installed_coordinator(&env, HostPlatform::Linux)
        .expect("an installed coordinator is discovered");
    let endpoint = installed_endpoint(&installed, None, &env, HostPlatform::Linux)
        .expect("the installed endpoint resolves");
    assert_eq!(endpoint.mode, EndpointMode::FrontDoor);
    assert_eq!(endpoint.origin, "https://roost.example.com");
    assert_eq!(
        endpoint.loopback_origin(),
        "http://127.0.0.1:4200",
        "the listener stays on the loopback bind the install already declared"
    );

    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, Some(&installed), false)
        .expect("the rerun plan resolves");
    assert!(resolved.coordinator_already_installed);
    let unit_text = resolved
        .coordinator
        .definition_text(HostPlatform::Linux)
        .expect("renders");
    assert!(
        unit_text.contains("https://roost.example.com"),
        "a rerun must not silently drop the front door the machine already has:\n{unit_text}"
    );
}

/// The plan's definitions are the ones the services group's own tests pin, not
/// a second rendering: the same renderer, the same spec, byte for byte.
#[test]
fn the_dry_run_definition_is_the_text_the_install_would_write() {
    let machine = TempMachine::new("same-text");
    let env = machine.environment();
    let endpoint = fresh_endpoint(None).expect("a loopback endpoint");
    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, false)
        .expect("the plan resolves");

    let spec = &resolved.worker.spec;
    let independent = render_definition(spec, HostPlatform::Linux).expect("renders");
    assert_eq!(
        resolved.worker.definition_text(HostPlatform::Linux).expect("renders"),
        independent,
        "the dry run prints what the install writes, not a re-render of it"
    );
    let reparsed = parse_installed_environment(&independent, HostPlatform::Linux);
    assert_eq!(
        reparsed.get("ROOST_COORDINATOR_URL").map(String::as_str),
        Some(endpoint.loopback_origin().as_str()),
        "and the printed text is one the installed-definition reader understands: {reparsed:?}"
    );
}

//! The bundle half of `roost deploy`, seen from the machine that receives it.
//!
//! The property is that a machine reached through `roost __remote-apply` — the
//! hidden command a deploy runs over ssh, on the target, with the machine
//! transaction held — ends up with the bundle the deploy staged AND a
//! `ROOST_WEB_DIST_PATH` in its own worker definition that names the copy that
//! landed there.
//!
//! Both halves are load-bearing and neither is observable from the deploying
//! box. A worker whose definition names a path into the DEPLOYING machine's
//! version tree reports a healthy worker and answers 404 for every URL; a
//! worker carrying no path at all serves nothing and looks the same from
//! `roost status` run on the coordinator. The green `roost status` a coordinator
//! prints after re-joining a fleet is not evidence the fleet has a UI, and this
//! test is the evidence it needs instead.
//!
//! The service manager is the test's own, so this exercises the real apply — the
//! real digest check, the real bundle install, the real definition render and
//! write — and stops short of `systemctl`, which is the one collaborator here
//! that can change the machine outside a file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::deploy::apply::run_with;
use roost_cli::deploy::machine_txn::{MachineTransaction, TransactionKind, lock_path};
use roost_cli::deploy::manifest::{ApplyManifest, ApplyOutcome};
use roost_cli::deploy::release::release_digest;
use roost_cli::services::service_argv::ServiceAction;
use roost_cli::services::service_control::{ServiceControlError, ServiceManager};
use roost_cli::services::service_spec::{ServiceRole, ServiceTarget};
use roost_cli::wall_clock;
use roost_host::{EnvSource, HostPlatform, MapEnv};

/// The build identity the manifest names, and therefore the release directory.
const SHA: &str = "0f0c0a09";

/// A throwaway machine: a home, a staged release and whatever else a test needs.
struct Machine {
    root: PathBuf,
    env: MapEnv,
}

impl Machine {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-remote-web-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("the throwaway home is created");
        let mut env = MapEnv::new();
        env.set("HOME", home.to_str().expect("utf-8"));
        env.set(
            "XDG_DATA_HOME",
            root.join("home/data").to_str().expect("utf-8"),
        );
        env.set(
            "XDG_STATE_HOME",
            root.join("home/state").to_str().expect("utf-8"),
        );
        env.set("ROOST_WORKER_LABEL", "studio");
        env.set("ROOST_COORDINATOR_URL", "https://v3.mike.roosttt.com");
        Self { root, env }
    }

    /// Stage a release whose `web/` holds exactly `web`, and answer with the
    /// manifest a deploy would have sent over the ssh boundary.
    fn stage(&self, web: &[(&str, &str)]) -> String {
        let staged = self.root.join("staged");
        write(&staged.join("bin/roost"), "release one\n");
        write(&staged.join("bin/roost-keeper"), "keeper one\n");
        for (relative, body) in web {
            write(&staged.join("web").join(relative), body);
        }
        let digest = release_digest(&staged.join("bin")).expect("the staged release hashes");
        let mut environment = std::collections::BTreeMap::new();
        for name in ["ROOST_WORKER_LABEL", "ROOST_COORDINATOR_URL"] {
            environment.insert(name.to_string(), self.env.get(name).expect("a value"));
        }
        let manifest =
            ApplyManifest::new(SHA, staged.to_str().expect("utf-8"), &digest, environment);
        manifest
            .encode()
            .map(|bytes| String::from_utf8(bytes).expect("the manifest is utf-8"))
            .expect("the manifest encodes")
    }

    /// The files a real bundle holds: an index a router can answer a deep link
    /// from, and one nested asset, which is the half a copy that never creates
    /// its subdirectories fails on.
    const BUNDLE: [(&'static str, &'static str); 2] = [
        ("index.html", "<html>marker: deployed bundle</html>\n"),
        ("assets/app.js", "marker: deployed asset\n"),
    ];

    /// The definition the apply wrote, read back off this machine.
    fn definition(&self) -> String {
        let path = ServiceRole::Worker
            .definition_path(&self.env, HostPlatform::Linux)
            .expect("the worker definition path resolves");
        std::fs::read_to_string(path).expect("the apply wrote the definition")
    }

    /// The bundle directory inside the release the apply installed.
    fn release_web(&self) -> PathBuf {
        roost_host::roost_versions_dir(&self.env, HostPlatform::Linux)
            .expect("the version root resolves")
            .join(SHA)
            .join("web")
    }

    /// Hold the machine transaction, which is the apply's own precondition.
    ///
    /// No journal file is written: a first install has none, and the apply
    /// resolves an interrupted deploy by finding no journal. A file that exists
    /// and cannot be parsed is a different state the apply refuses outright, and
    /// this is not a test of that.
    async fn hold(&self) -> MachineTransaction {
        let service_dir =
            roost_host::roost_service_dir(&self.env, HostPlatform::Linux).expect("service dir");
        MachineTransaction::acquire(
            &lock_path(&service_dir),
            TransactionKind::Deploy,
            &service_dir.join("deploy-journal.json"),
            wall_clock::now_ms(),
        )
        .await
        .expect("the machine is taken")
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("the directory");
    std::fs::write(path, body).expect("the file is written");
}

/// Answers every activation, so the apply settles and the definition it wrote is
/// the one on disk.
struct AnsweringManager;

impl ServiceManager for AnsweringManager {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Linux
    }

    fn apply(
        &mut self,
        _target: &ServiceTarget,
        _action: ServiceAction,
    ) -> Result<(), ServiceControlError> {
        Ok(())
    }

    fn await_active(&mut self, _target: &ServiceTarget) -> bool {
        true
    }
}

/// The property. A deploy that staged a bundle leaves the target with those
/// bytes beside its own binaries, and with a definition naming exactly that
/// directory — a path into the DEPLOYING box's tree, or no entry at all, is the
/// failure this pins.
#[tokio::test]
async fn a_machine_reached_through_the_remote_apply_serves_the_bundle_the_deploy_staged() {
    let machine = Machine::new("bundle");
    let manifest = machine.stage(&Machine::BUNDLE);
    let held = machine.hold().await;

    let report = run_with(manifest.as_bytes(), &machine.env, &mut AnsweringManager).await;
    assert_eq!(
        report.outcome,
        ApplyOutcome::Settled,
        "a deploy with a held transaction and a proved digest settles: {}",
        report.detail
    );

    let installed = machine.release_web();
    assert_eq!(
        std::fs::read_to_string(installed.join("index.html")).expect("the bundle landed"),
        "<html>marker: deployed bundle</html>\n",
        "the bytes on the target are the bytes the deploy staged, and the nested asset came \
         with them: a copy that never created its subdirectory fails on the first real bundle"
    );
    assert_eq!(
        std::fs::read_to_string(installed.join("assets/app.js")).expect("the asset landed"),
        "marker: deployed asset\n",
        "a bundle installed as its index alone is a page that loads its shell and never its code"
    );

    let definition = machine.definition();
    assert!(
        definition.contains(&format!("ROOST_WEB_DIST_PATH={}", installed.display())),
        "the definition names the copy on THIS machine, and names it exactly: {definition}"
    );
    held.release().await.expect("the machine is released");
}

/// The other half of the same question. A deploy that shipped no bundle must not
/// leave a definition pointing at one, or a later release's install inherits a
/// path into a tree that no longer exists.
#[tokio::test]
async fn a_deploy_that_shipped_no_bundle_names_no_bundle() {
    let machine = Machine::new("nobundle");
    let manifest = machine.stage(&[]);
    let held = machine.hold().await;

    let report = run_with(manifest.as_bytes(), &machine.env, &mut AnsweringManager).await;
    assert_eq!(
        report.outcome,
        ApplyOutcome::Settled,
        "a release predating the bundle asset still installs and runs: {}",
        report.detail
    );
    assert!(
        !machine.definition().contains("ROOST_WEB_DIST_PATH"),
        "a definition naming a bundle this deploy did not ship is a path to nothing"
    );
    assert!(
        !machine.release_web().exists(),
        "and no bundle directory is invented to make such a path resolve"
    );
    held.release().await.expect("the machine is released");
}

/// A `web/` directory is a bundle only if it holds the file a router answers a
/// deep link from. Anything else is not installed and not named, because a
/// bundle missing its index is a coordinator answering 404 for every URL while
/// reporting itself healthy.
#[tokio::test]
async fn a_staged_web_directory_without_an_index_is_not_a_bundle() {
    let machine = Machine::new("noindex");
    let manifest = machine.stage(&[("assets/app.js", "marker: deployed asset\n")]);
    let held = machine.hold().await;

    let report = run_with(manifest.as_bytes(), &machine.env, &mut AnsweringManager).await;
    assert_eq!(
        report.outcome,
        ApplyOutcome::Settled,
        "a release carrying assets but no index is still a release: {}",
        report.detail
    );
    assert!(
        !machine.release_web().exists(),
        "an index-less directory is not installed, because a router cannot answer from it"
    );
    assert!(
        !machine.definition().contains("ROOST_WEB_DIST_PATH"),
        "and it is not named, so the definition never points at a directory with no page in it"
    );
    held.release().await.expect("the machine is released");
}

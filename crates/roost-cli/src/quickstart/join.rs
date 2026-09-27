//! `roost join` — install and register this machine's worker from a one-shot
//! grant. Called by the crate's dispatcher. Depends on the deploy group's
//! identity proof for what a joined worker is allowed to be built from, and on
//! the services group's install for everything that puts a definition on disk.
//!
//! **The grant is a credential and is read from the environment, once.** It is
//! never printed, never logged, and never written anywhere except the
//! installed worker definition — which is where the worker's own boot reads it
//! to redeem itself, and which the next install strips, which is what makes it
//! one-shot. The two variables are named in the refusal when one is missing,
//! because the operator reading that message is holding a one-liner on another
//! machine and needs to know which half of it did not arrive.
//!
//! **A joined worker is refused on a dirty tree, and the refusal names the way
//! out.** The build identity a worker stamps into its heartbeat is what the
//! coordinator's fleet roster compares, and a worker that reports a build it is
//! not running can never earn keeper-update admission — so a fleet becomes
//! quietly unupdatable rather than visibly broken. Unlike a deploy, a join does
//! not accept the dirty stamp: enrollment is the moment a machine's identity is
//! first asserted, and there is no earlier moment to have got it wrong.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use roost_host::{EnvSource, HostPlatform};
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::{ROOST_PROGRAM, install_environment};
use crate::deploy::codes;
use crate::deploy::identity::{ALLOW_DIRTY_ENV, DIRTY_SUFFIX, local_git_sha_or_die};
use crate::quickstart::install::{
    LocalPrograms, deploy_local_definition, install_programs, prepare_service_directories,
    report_change, report_rotation, service_dir,
};
use crate::services::install::release_bin_dir;
use crate::services::service_environment::{ENV_BOOTSTRAP_TOKEN, ENV_WORKER_LABEL};
use crate::services::service_spec::{ServiceRole, ServiceSpec};

/// The checkout a join proves its build identity against, when the operator
/// names one. A compiled binary has no checkout of its own, and the variable is
/// how a machine that installed from a tarball says which tree it is enrolling
/// as. Unset, the working directory is the tree.
pub const SOURCE_ROOT_ENV: &str = "ROOST_SOURCE_ROOT";

/// What the coordinator this machine is joining is told, in the terms the
/// worker's own boot reads.
///
/// The two one-shot values live in the DECIDED map, and
/// [`ServiceSpec::with_decided_one_shots`] is the only thing that arms them
/// into a definition. A plain resolve refuses them on purpose — an ambient
/// environment must never hand a machine a credential — so the arming is
/// explicit at exactly the one site that means to arm one.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCredentials {
    /// The coordinator the worker dials.
    pub coordinator_url: String,
    /// The one-shot grant, held only long enough to arm it.
    pub bootstrap_token: String,
    /// The name the fleet will know this machine by, when one was declared.
    pub label: Option<String>,
}

impl std::fmt::Debug for JoinCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JoinCredentials")
            .field("coordinator_url", &self.coordinator_url)
            .field("bootstrap_token", &"redacted")
            .field("label", &self.label)
            .finish()
    }
}

impl JoinCredentials {
    /// The decided environment one worker's definition is resolved from.
    pub fn decided_settings(&self) -> BTreeMap<String, String> {
        let mut decided = BTreeMap::new();
        decided.insert(
            ENV_COORDINATOR_URL.to_string(),
            self.coordinator_url.clone(),
        );
        decided.insert(
            ENV_BOOTSTRAP_TOKEN.to_string(),
            self.bootstrap_token.clone(),
        );
        if let Some(label) = &self.label {
            decided.insert(ENV_WORKER_LABEL.to_string(), label.clone());
        }
        decided
    }
}

/// Read the two variables a join needs, naming whichever is missing.
pub fn read_credentials(env: &dyn EnvSource) -> Result<JoinCredentials, CommandFailure> {
    let coordinator_url =
        declared(env, ENV_COORDINATOR_URL).ok_or_else(|| missing_variable(ENV_COORDINATOR_URL))?;
    let bootstrap_token =
        declared(env, ENV_BOOTSTRAP_TOKEN).ok_or_else(|| missing_variable(ENV_BOOTSTRAP_TOKEN))?;
    Ok(JoinCredentials {
        coordinator_url,
        bootstrap_token,
        label: declared(env, ENV_WORKER_LABEL),
    })
}

fn missing_variable(name: &str) -> CommandFailure {
    CommandFailure::generic(format!(
        "{name} is not set. The enrollment command from `roost add-machine --platform macos` or \
         `roost add-machine --platform linux`, run on your coordinator, sets both it and the \
         other one; paste that whole command here rather than exporting half of it."
    ))
}

/// A variable that is set and not empty. A blank value is not a value: it is
/// the shape an unset variable takes in a shell that exported it anyway, and a
/// worker installed against an empty coordinator URL dials nothing while
/// reporting itself as joined.
fn declared(env: &dyn EnvSource, name: &str) -> Option<String> {
    env.get(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The checkout a join builds its identity from.
pub fn source_root(env: &dyn EnvSource) -> Result<PathBuf, CommandFailure> {
    if let Some(declared_root) = declared(env, SOURCE_ROOT_ENV) {
        return Ok(PathBuf::from(declared_root));
    }
    std::env::current_dir().map_err(|error| {
        CommandFailure::generic(format!(
            "the working directory could not be read, so there is no source tree to prove this \
             machine's build from; set {SOURCE_ROOT_ENV} to the checkout instead: {error}"
        ))
    })
}

/// The build this machine's worker will be stamped with, refusing a dirty
/// checkout even when the operator allowed one elsewhere.
///
/// `ROOST_ALLOW_DIRTY=1` makes a deploy stamp `<sha>-dirty` and carry on. A
/// join may not: the stamp is the identity the fleet roster compares, and a
/// machine whose first assertion of identity is already wrong is a machine
/// whose drift badge is permanently wrong with no way to tell which build it
/// was.
pub async fn joined_build_sha(source_root: &Path) -> Result<String, CommandFailure> {
    let stamp = local_git_sha_or_die(source_root).await?;
    if stamp.ends_with(DIRTY_SUFFIX) {
        return Err(CommandFailure::new(
            codes::IDENTITY_UNPROVED,
            format!(
                "a joined worker requires a clean committed source snapshot, and {} has \
                 uncommitted changes. Commit them first. If you understand that the fleet will \
                 record this machine as {stamp} and can never afterwards tell which build it is \
                 running, set {ALLOW_DIRTY_ENV}=1 for this command alone.",
                source_root.display()
            ),
        ));
    }
    Ok(stamp)
}

/// The worker definition this machine will be enrolled with, resolved from the
/// decided environment and armed with the one grant the caller supplied.
pub fn worker_spec(
    env: &dyn EnvSource,
    platform: HostPlatform,
    bin_dir: &Path,
    credentials: &JoinCredentials,
) -> Result<ServiceSpec, CommandFailure> {
    let decided = credentials.decided_settings();
    let install_env = install_environment(env, &decided);
    let program = bin_dir.join(ROOST_PROGRAM);
    let resolved = ServiceSpec::resolve(ServiceRole::Worker, &install_env, platform, &program)?;
    Ok(resolved.with_decided_one_shots(&decided))
}

/// Install and register this machine's worker from a one-shot grant.
pub async fn run(env: &dyn EnvSource) -> Result<ExitCode, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    let credentials = read_credentials(env)?;
    let root = source_root(env)?;
    let build_sha = joined_build_sha(&root).await?;

    let service_dir = service_dir(env, platform)?;
    let programs = LocalPrograms::of_this_process()?;
    let bin_dir = release_bin_dir(env, platform)?;
    install_programs(&programs, &bin_dir)?;

    let spec = worker_spec(env, platform, &bin_dir, &credentials)?;
    prepare_service_directories(&spec)?;
    let outcome = deploy_local_definition(&spec, platform, &service_dir).await?;
    report_change(&outcome, "joiner installed the worker definition");

    report_rotation(ServiceRole::Worker, env, platform);

    info!(build_sha = %build_sha, label = %spec.label, "join settled");
    println!("Joined {}.", spec.label);
    println!("  build:   {build_sha}");
    println!("  dials:   {}", credentials.coordinator_url);
    println!("  program: {}", spec.program.display());
    println!("  check:   roost status");
    eprintln!(
        "The enrollment grant was carried into {} and is spent on this machine's first boot. The \
         next install strips it.",
        spec.definition_path.display()
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::{JoinCredentials, declared, read_credentials, worker_spec};
    use crate::services::definition_text::render_definition;
    use crate::services::service_environment::{ENV_BOOTSTRAP_TOKEN, ENV_WORKER_LABEL};
    use roost_host::{HostPlatform, MapEnv};
    use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

    fn environment(pairs: &[(&str, &str)]) -> MapEnv {
        pairs
            .iter()
            .fold(MapEnv::new(), |env, (key, value)| env.with(key, value))
    }

    fn credentials(pairs: &[(&str, &str)]) -> JoinCredentials {
        read_credentials(&environment(pairs)).expect("the two variables are present")
    }

    #[test]
    fn a_missing_coordinator_url_is_named_on_its_own() {
        let failure = read_credentials(&environment(&[])).expect_err("neither variable is set");
        assert!(
            failure
                .message
                .contains(&format!("{ENV_COORDINATOR_URL} is not set")),
            "{failure}"
        );
        assert!(
            !failure
                .message
                .contains(&format!("{ENV_BOOTSTRAP_TOKEN} is not set")),
            "{failure}"
        );
    }

    #[test]
    fn a_missing_grant_is_named_even_when_the_url_is_present() {
        let failure = read_credentials(&environment(&[(ENV_COORDINATOR_URL, "https://a.example")]))
            .expect_err("the grant is missing");
        assert!(
            failure
                .message
                .contains(&format!("{ENV_BOOTSTRAP_TOKEN} is not set")),
            "{failure}"
        );
    }

    #[test]
    fn a_variable_set_to_nothing_is_treated_as_missing() {
        assert!(
            declared(
                &environment(&[(ENV_BOOTSTRAP_TOKEN, "   ")]),
                ENV_BOOTSTRAP_TOKEN
            )
            .is_none()
        );
        assert!(
            declared(
                &environment(&[(ENV_BOOTSTRAP_TOKEN, "roost_bt_x")]),
                ENV_BOOTSTRAP_TOKEN
            )
            .is_some()
        );
    }

    #[test]
    fn the_decided_map_carries_the_grant_and_declares_no_label_it_was_not_given() {
        let settings = credentials(&[
            (ENV_COORDINATOR_URL, "https://a.example"),
            (ENV_BOOTSTRAP_TOKEN, "roost_bt_x"),
        ])
        .decided_settings();
        assert_eq!(
            settings.get(ENV_COORDINATOR_URL).map(String::as_str),
            Some("https://a.example")
        );
        assert_eq!(
            settings.get(ENV_BOOTSTRAP_TOKEN).map(String::as_str),
            Some("roost_bt_x")
        );
        assert!(
            !settings.contains_key(ENV_WORKER_LABEL),
            "an unnamed machine declares no label"
        );
    }

    #[test]
    fn a_debug_rendering_of_the_credentials_never_carries_the_grant() {
        let rendered = format!(
            "{:?}",
            credentials(&[
                (ENV_COORDINATOR_URL, "https://a.example"),
                (ENV_BOOTSTRAP_TOKEN, "roost_bt_secret"),
            ])
        );
        assert!(!rendered.contains("roost_bt_secret"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    /// The grant has to reach the rendered unit, or a joined machine installs a
    /// worker that can never redeem itself. This is the observable end of the
    /// arming chain: not "the decided map has the key" but "the definition a
    /// service manager will read says it".
    #[test]
    fn the_armed_definition_carries_the_grant_and_the_decided_coordinator_url() {
        let root = std::env::temp_dir().join(format!("roost-join-spec-{}", std::process::id()));
        let bin_dir = root.join("versions/build/bin");
        std::fs::create_dir_all(&bin_dir).expect("the bin directory exists");
        let env = environment(&[
            ("HOME", root.to_str().expect("utf-8")),
            (
                roost_host::COORD_UNIT_ENV,
                root.join("unit/roost3-coord.service")
                    .to_str()
                    .expect("utf-8"),
            ),
            (
                roost_host::WORKER_UNIT_ENV,
                root.join("unit/roost3-worker.service")
                    .to_str()
                    .expect("utf-8"),
            ),
            (
                roost_host::WORKER_DATA_DIR_ENV,
                root.join("data").to_str().expect("utf-8"),
            ),
        ]);
        let spec = worker_spec(
            &env,
            HostPlatform::Linux,
            &bin_dir,
            &credentials(&[
                (ENV_COORDINATOR_URL, "https://a.example"),
                (ENV_BOOTSTRAP_TOKEN, "roost_bt_secret"),
            ]),
        )
        .expect("a worker spec resolves");
        let rendered = render_definition(&spec, HostPlatform::Linux)
            .expect("the definition renders for this platform");
        assert!(rendered.contains("roost_bt_secret"), "{rendered}");
        assert!(rendered.contains("https://a.example"), "{rendered}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

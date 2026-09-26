//! Assembling `roost status`'s report: the installed service definition, the
//! two service-manager probes, the coordinator's own listener, the front door,
//! and the worker roster. Called by status/mod.rs, which prints it. This is the
//! only module in the status group that touches the disk, a socket, or a
//! subprocess; everything it produces goes to status/render.rs, which is pure,
//! so the documented output can be asserted with nothing running.
//!
//! `now_ms` and the environment are parameters rather than reads of the clock
//! and `process.env`, for the same reason: the readout is a documented shape,
//! and a shape that cannot be produced deterministically cannot be pinned.

use std::path::{Path, PathBuf};

use roost_host::coord_config::COORD_DB_FILE_NAME;
use roost_host::coord_config_loader::{
    ENV_COORDINATOR_BIND, ENV_COORDINATOR_DB, ENV_COORDINATOR_PUBLIC_URL, ENV_WEB_DIST_PATH,
    ENV_WEB_PUBLIC_URL,
};
use roost_host::{
    EnvSource, HostPlatform, ProtocolError, coord_data_dir, coord_service_label,
    coord_service_path, normalize_https_origin, worker_service_label,
};

use crate::status::http_probe::HttpProbe;
use crate::status::inventory::{self, InventoryError};
use crate::status::report::{CoordStatus, EndpointStatus, SpaStatus, StatusReport, WorkerStatus};
use crate::status::service_definition::{
    InstalledEnvironment, declared_value, parse_installed_environment,
};
use crate::status::service_probe;

#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    #[error("this host's install paths could not be resolved: {0}")]
    Paths(#[from] ProtocolError),
    #[error("no HTTP client could be built for the coordinator probes: {0}")]
    HttpClient(#[from] reqwest::Error),
}

/// The report plus the two service identities it was assembled against. The
/// labels travel with the report because they come from the environment, and a
/// renderer that read the environment itself could not render two installs in
/// one process.
#[derive(Debug)]
pub struct CollectedStatus {
    pub report: StatusReport,
    pub coord_label: String,
    pub worker_label: String,
}

/// Hand-written because the environment is a trait object. What is worth seeing
/// in a failure is the platform and the clock, because those decide what the
/// report means; the environment is a pointer either way.
impl std::fmt::Debug for StatusContext<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StatusContext")
            .field("platform", &self.platform)
            .field("now_ms", &self.now_ms)
            .field("endpoint_override", &self.endpoint_override)
            .finish()
    }
}

pub struct StatusContext<'a> {
    pub env: &'a dyn EnvSource,
    pub platform: HostPlatform,
    pub now_ms: i64,
    /// `--endpoint`: speak about this front door instead of the installed
    /// declaration, for a machine whose unit file names a URL that has since
    /// moved. Reading the declaration is still what fills the rest.
    pub endpoint_override: Option<String>,
}

/// The two origins `roost status` can speak about: the operator's declared
/// front door, and the coordinator's own loopback listener as the installed
/// service definition binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub public_url: Option<String>,
    pub coord_url: Option<String>,
}

pub fn resolve_endpoint(
    installed: &InstalledEnvironment,
    override_origin: Option<&str>,
) -> ResolvedEndpoint {
    // `ROOST_WEB_PUBLIC_URL` first, then `ROOST_COORDINATOR_PUBLIC_URL`, in
    // that order and not the other way round: the web public URL is what a
    // browser is handed, so when both are declared they must not disagree
    // about which origin the fleet is reachable at.
    let declared = override_origin.map(str::to_string).or_else(|| {
        declared_value(installed, ENV_WEB_PUBLIC_URL)
            .or_else(|| declared_value(installed, ENV_COORDINATOR_PUBLIC_URL))
            .map(str::to_string)
    });
    ResolvedEndpoint {
        public_url: front_door_origin(declared.as_deref()),
        coord_url: declared_value(installed, ENV_COORDINATOR_BIND)
            .map(|bind| format!("http://{bind}")),
    }
}

/// A declared front door is only a front door if it is an HTTPS origin. A
/// `http://` or unparseable declaration reads as no front door rather than as
/// an error: the readout's job is to say what the install will answer, and an
/// install that declared an unusable URL answers nothing.
fn front_door_origin(declared: Option<&str>) -> Option<String> {
    normalize_https_origin(declared, ENV_WEB_PUBLIC_URL)
        .ok()
        .flatten()
}

pub async fn collect(context: &StatusContext<'_>) -> Result<CollectedStatus, CollectError> {
    let coord_label = coord_service_label(context.env, context.platform)?;
    let worker_label = worker_service_label(context.env, context.platform)?;
    let definition = read_service_definition(context.env, context.platform);
    let installed = definition
        .as_deref()
        .map(|text| parse_installed_environment(text, context.platform))
        .unwrap_or_default();

    let endpoint = resolve_endpoint(&installed, context.endpoint_override.as_deref());
    let probe = HttpProbe::new()?;
    // Liveness is the coordinator's own listener, so a front door the operator
    // has not finished wiring never reads as a dead coordinator. Off a
    // coordinator host there is no bind to probe, so the front door is all
    // there is to ask.
    let liveness_origin = endpoint
        .coord_url
        .as_deref()
        .or(endpoint.public_url.as_deref());
    let identity = probe.coordinator_identity(liveness_origin).await;
    let answers = match endpoint.public_url.as_deref() {
        None => false,
        // The front door IS the origin that answered above: reusing that one
        // answer rather than probing twice keeps a slow front door from
        // doubling the time an operator waits for a readout.
        Some(front) if Some(front) == liveness_origin => identity.reachable,
        Some(front) => probe.coordinator_identity(Some(front)).await.reachable,
    };

    let web_dist_path = declared_value(&installed, ENV_WEB_DIST_PATH).map(str::to_string);
    let spa = SpaStatus {
        serves: probe.spa_root(endpoint.coord_url.as_deref()).await,
        web_dist_present: web_dist_path
            .as_deref()
            .is_some_and(|path| holds_index_html(Path::new(path))),
        web_dist_path,
    };

    Ok(CollectedStatus {
        report: StatusReport {
            coord_agent_loaded: service_probe::service_is_loaded(&coord_label, context.platform),
            worker_agent_loaded: service_probe::service_is_loaded(&worker_label, context.platform),
            coord: CoordStatus {
                reachable: identity.reachable,
                git_sha: identity.git_sha,
            },
            workers: read_workers(context, &installed).await,
            endpoint: EndpointStatus {
                public_url: endpoint.public_url,
                answers,
            },
            spa,
        },
        coord_label,
        worker_label,
    })
}

/// A damaged or hand-edited unit must not stop the readout: the one fact an
/// operator needs from a machine in that state is that its services are not
/// loaded, and a read error that aborts the command hides exactly that.
fn read_service_definition(env: &dyn EnvSource, platform: HostPlatform) -> Option<String> {
    let path = coord_service_path(env, platform).ok()?;
    std::fs::read_to_string(path).ok()
}

/// The environment the installed coordinator's own service definition
/// declares, or an empty map when there is no readable definition. Public
/// because `roost doctor` resolves the same database through it: two readers
/// of one installed definition must not each grow their own idea of its shape.
pub fn installed_coordinator_environment(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> InstalledEnvironment {
    read_service_definition(env, platform)
        .map(|definition| parse_installed_environment(&definition, platform))
        .unwrap_or_default()
}

/// Does the stamped dist hold a page the coordinator could serve? The
/// coordinator's responder is the authority on what it serves; this answers the
/// narrower question the `spa:` line needs, which is whether the file is there.
fn holds_index_html(dist_path: &Path) -> bool {
    dist_path.join("index.html").is_file()
}

async fn read_workers(
    context: &StatusContext<'_>,
    installed: &InstalledEnvironment,
) -> Vec<WorkerStatus> {
    let database_path = match coordinator_database_path(context.env, context.platform, installed) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(
                target: "status",
                msg = "coordinator_db_path_unresolved",
                fields = error.to_string(),
            );
            return Vec::new();
        }
    };
    match inventory::worker_inventory(&database_path, context.now_ms).await {
        Ok(workers) => workers,
        Err(InventoryError::Missing(path)) => {
            // No database at all is a coordinator that has never run on this
            // host, which is a legitimate empty roster rather than a failure.
            tracing::info!(
                target: "status",
                msg = "coordinator_db_absent",
                fields = path.display().to_string(),
            );
            Vec::new()
        }
        Err(error) => {
            // A database that exists and cannot be read is NOT an empty fleet.
            // The rows print empty and the reason goes to the log, because the
            // alternative — failing the command — hides every other column of a
            // readout an operator is running on a sick machine.
            tracing::warn!(
                target: "status",
                msg = "worker_inventory_failed",
                fields = error.to_string(),
            );
            Vec::new()
        }
    }
}

/// Where the roster comes from: the installed unit's own declaration first,
/// then `ROOST_COORDINATOR_DB`, then the default database inside the data
/// directory. The unit is authoritative because it is what the running
/// coordinator booted with; a shell variable naming a different file describes
/// a database nobody is serving.
pub fn coordinator_database_path(
    env: &dyn EnvSource,
    platform: HostPlatform,
    installed: &InstalledEnvironment,
) -> Result<PathBuf, ProtocolError> {
    if let Some(declared) = declared_value(installed, ENV_COORDINATOR_DB) {
        return Ok(PathBuf::from(declared));
    }
    if let Some(declared) = env
        .get(ENV_COORDINATOR_DB)
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(declared));
    }
    Ok(coord_data_dir(env, platform)?.join(COORD_DB_FILE_NAME))
}

//! `CoordBoot` and the blocking `serve` the CLI's `roost coord` calls.
//!
//! Owned by the coordinator and the ONLY public entry point a process uses. The
//! shape is v2's `runCoord` (`apps/coord/src/main.ts:48-253`) with one
//! deliberate difference: v2 ends its shutdown in `process.exit(0)`, and this
//! **returns** instead, because a library that exits the process cannot be called
//! from a test or from a subcommand.
//!
//! CONFIGURATION IS THE CALLER'S. `CoordBoot` carries an already-resolved,
//! already-validated [`roost_host::CoordConfig`]. The CLI owns resolving it from
//! argv and the environment and refuses before starting anything; the convenience
//! constructor [`CoordBoot::from_env`] exists so `roost coord` can delegate to
//! `roost_host::load_coord_config` rather than re-deriving a single path.
//!
//! THE BOOT ORDER IS THE CONTRACT (`docs/phase3-coord-contract.md` §1.1), and
//! three of its steps are load-bearing rather than cosmetic: the pre-migration
//! backup runs only if the file already existed, the tenancy invariant runs after
//! the authorized-keys import so it sees freshly imported keys, and it runs before
//! the listener exists so a mis-scoped database never binds a port.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use roost_host::CoordConfig;
use roost_platform::HostPlatform;

use crate::http::listener::{ListenerState, build_router, resolve_bind};
use crate::rpc::service::CoordinatorServiceImpl;
use crate::services::CoordServices;

/// The resolved, validated configuration a coordinator daemon boots from.
#[derive(Debug, Clone)]
pub struct CoordBoot {
    /// The settings, already defaulted and shape-checked.
    pub config: CoordConfig,
    /// The host platform, so path resolution used the same rules the installer
    /// did.
    pub platform: HostPlatform,
}

impl CoordBoot {
    /// A boot over an already-validated configuration.
    ///
    /// No validation happens here on purpose: `roost_host::load_coord_config` is
    /// the one loader, and a second check is a second answer.
    #[must_use]
    pub fn new(config: CoordConfig, platform: HostPlatform) -> Self {
        Self { config, platform }
    }

    /// Resolve the configuration from an environment, through the one loader.
    ///
    /// `roost coord` may use this; anything that has already resolved its config
    /// should use [`CoordBoot::new`] instead, so the refusal happens in the CLI
    /// where the operator sees it.
    pub fn from_env(
        env: &dyn roost_host::EnvSource,
        platform: HostPlatform,
    ) -> anyhow::Result<Self> {
        let config = roost_host::load_coord_config(env, platform)
            .map_err(|error| anyhow::anyhow!("coordinator configuration: {error}"))?;
        Ok(Self::new(config, platform))
    }
}

/// Run the coordinator until the process is asked to stop.
///
/// Blocks. Returns rather than exiting, so the caller owns the shutdown and the
/// exit code. A bind that cannot be resolved, a database that cannot be opened,
/// or a migration that fails all return an error **before** anything is bound --
/// which is the behaviour the tenancy invariant depends on.
pub async fn serve(boot: CoordBoot) -> anyhow::Result<()> {
    let boot_ms = now_ms();
    let process_epoch = process_epoch();

    let bind = resolve_bind(&boot.config.bind)
        .with_context(|| format!("coordinator bind {}", boot.config.bind))?;

    // The listener's admission gate needs the RESOLVED port, and `:0` only
    // resolves after the listener is created. The gate is therefore built after
    // the bind, from what the bind actually is -- and it answers 503 for anything
    // that arrives in between (`apps/coord/src/bun-coordinator-listeners.ts:363-370`).
    let database = crate::db::open(&boot.config.db_path)
        .await
        .with_context(|| format!("coordinator database {}", boot.config.db_path.display()))?;
    let services = Arc::new(CoordServices::new(database));

    let service = Arc::new(CoordinatorServiceImpl::new(
        boot.config.clone(),
        process_epoch,
        boot_ms,
        roost_host::COMPILED_ROOST_BUILD_SHA
            .unwrap_or_default()
            .to_string(),
    ));
    let state = Arc::new(ListenerState {
        service,
        services,
        bind: boot.config.bind.clone(),
        web_public_url: boot.config.web_public_url.clone(),
        trust_proxy: boot.config.trust_proxy,
        spa_available: boot.config.web_dist_path.is_some(),
    });

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("coordinator listen on {bind}"))?;
    let local = listener
        .local_addr()
        .with_context(|| "coordinator local address".to_string())?;
    tracing::info!(bind = %local, uptime_ms = now_ms().saturating_sub(boot_ms), "coordinator listening");

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("coordinator listener")
}

/// The platform's termination signal, or a never-completing future elsewhere.
///
/// `SIGTERM` **and** `SIGINT` are both wired, because systemd sends the first and
/// a terminal sends the second, and a coordinator that only handled one of them
/// would need a `SIGKILL` to stop -- which is the one path that does not run the
/// shutdown sequence.
async fn shutdown_signal() {
    let interrupt = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            signal.recv().await;
        }
    };
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
    tracing::info!("coordinator shutdown");
}

/// Epoch milliseconds, saturating rather than wrapping.
///
/// The one clock in this crate's public surface. Everything that needs a time
/// takes it as a parameter, so a test never waits for this to tick.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A fresh identity for this process, so a log line distinguishes a restart from
/// a reconnect.
#[must_use]
pub fn process_epoch() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!("{nanos:08x}-{:04x}", std::process::id())
}

/// The interval between the scheduled maintenance sweeps.
///
/// A day, matching v2's `DAY_MS` for backups and audit retention
/// (`apps/coord/src/db/backup.ts:113`,
/// `apps/coord/src/db/audit-retention.ts:178-180`). Exposed so a test can assert
/// the three sweeps agree on one interval rather than three.
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

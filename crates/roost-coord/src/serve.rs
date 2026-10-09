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
//! the listener exists so a mis-scoped database never binds a port. Steps 2-7 are
//! [`prepare_coordinator_database`]; `db::open` owns 2-4.
//!
//! The build SHA reported by `MiscHealth` is resolved once here, through
//! `roost_host::build_identity` -- the port of `apps/coord/src/git-sha.ts`,
//! minus its live `git rev-parse`, since a compiled binary names its own commit.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use roost_host::CoordConfig;
use roost_platform::HostPlatform;

use crate::agents::status_push::PushTransitions;
use crate::auth::cf_access::{cloudflare_access_configured, install_cloudflare_jwks};
use crate::auth::cf_access_keyring::RsaJwks;
use crate::auth::self_hosted_tenant::SelfHostedTenant;
use crate::coord_core::CoordCore;
use crate::coord_core::boot_facts::BootFacts;
use crate::coord_core::seams::CoordTerminal;
use crate::db::CoordDb;
use crate::http::listener::{ListenerState, build_router};
use crate::http::spa::SpaMount;
use crate::push::PushRuntime;
use crate::push::dispatch::ActiveTerminalViewers;
use crate::push::transport::WebPushTransport;
use crate::rpc::service::CoordinatorServiceImpl;
use crate::services::CoordServices;
use crate::shutdown::{SHUTDOWN_DRAIN_TIMEOUT, ShutdownDrain, shutdown_signal};

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

    let bind = crate::http::bind::resolve_bind(&boot.config.bind)
        .with_context(|| format!("coordinator bind {}", boot.config.bind))?;
    let (database, tenant) = prepare_coordinator_database(&boot.config, boot_ms).await?;

    // The boot facts are filled from what boot already established -- the
    // tenancy scope the invariant above just enforced, the config the caller
    // resolved, and this process's identity. Every handler reads them from
    // `core.services.boot` rather than carrying its own copy, so a fact cannot
    // be right in one domain and absent in another.
    let boot_facts = BootFacts {
        tenant: Some(tenant.clone()),
        config: Some(Arc::new(boot.config.clone())),
        process_epoch: process_epoch.clone(),
        boot_ms,
    };

    // The Access key ring is installed ONCE, here, and only when Access is
    // configured. Without this line the gate is still fail-closed — an absent
    // ring refuses every assertion rather than believing one — but a FRONTED
    // coordinator refuses every pairing request it was built to authenticate,
    // with nothing anywhere saying the ring was never installed.
    if cloudflare_access_configured(&boot.config) {
        install_cloudflare_jwks(Arc::new(RsaJwks::default()))?;
    }
    let services = Arc::new(CoordServices::booted(database, boot_facts));

    // The push runtime is built from the tenancy scope and the operator's
    // allowlist, both of which are boot-order facts: a push surface with no
    // dashboard has nowhere to scope a subscription row, and an empty
    // allowlist is how the operator switches the whole surface off.
    let push = PushRuntime::new(
        tenant.dashboard_id.clone(),
        boot.config.push_allowed_origins.clone(),
    );
    // Agent transitions reach a phone through the production Web Push
    // transport, which signs with the SAME VAPID store `PushGetConfig` hands
    // the browser, and a device already viewing the terminal is not told
    // (`push-dispatch.ts:92`). Installed once, before any worker can report.
    let web_push: Arc<dyn crate::push::transport::PushNotificationTransport> = Arc::new(
        WebPushTransport::new(services.db.clone(), push.vapid_keys().clone())
            .context("web push client")?,
    );
    services
        .agents
        .status
        .install_push_delivery(Arc::new(PushTransitions::new(
            services.db.pool().clone(),
            push.allowed_origins().to_vec(),
            Arc::clone(&services.views) as Arc<dyn ActiveTerminalViewers>,
            Arc::clone(&web_push),
        )));
    // Long shell commands and programs' own notifications reach a phone the
    // same way. Held for the life of `serve`: dropping them stops the pushes.
    let _session_pushes = crate::push::session_push::subscribe_session_pushes(
        &services,
        push.allowed_origins(),
        web_push,
    );

    let terminal = terminal_seams(&services);
    let core = CoordCore::with_terminal_and_push(Arc::clone(&services), terminal, push);

    let service = Arc::new(CoordinatorServiceImpl::new(
        core,
        boot.config.clone(),
        process_epoch,
        boot_ms,
        roost_host::build_identity(&roost_host::ProcessEnv::new()).build_sha,
    ));
    // Chosen ONCE, here, and reported either way. A missing build otherwise
    // presents only as a 404 on every page, which reads like an edge or DNS
    // fault rather than as a path the operator misspelled (`main.ts:113-120`).
    let spa = Arc::new(SpaMount::from_dist_path(
        boot.config.web_dist_path.as_deref(),
    ));
    match spa.root() {
        Some(root) => tracing::info!(web_dist_path = %root.display(), "spa source: disk"),
        None => tracing::error!(
            web_dist_path = ?boot.config.web_dist_path,
            "spa source missing: every page request answers 404"
        ),
    }
    let state = Arc::new(ListenerState {
        service,
        services,
        bind: boot.config.bind.clone(),
        web_public_url: boot.config.web_public_url.clone(),
        trust_proxy: boot.config.trust_proxy,
        spa,
        draining: Arc::default(),
    });

    // Cloned: the maintenance schedulers below need the same services, and a
    // backup scheduled before the port is accepting would compete with the very
    // startup it protects.
    let mounted = build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("coordinator listen on {bind}"))?;
    let local = listener
        .local_addr()
        .with_context(|| "coordinator local address".to_string())?;
    // The admission gate's allowlist names the RESOLVED port, so it cannot be
    // built until the OS has reported which port this bind got. Between the
    // bind and this line the gate answers `503 listener unavailable` to every
    // request rather than guessing a port -- and `local.port()` is 0 only for
    // the instant between the two.
    mounted.publish_bound_port(local.port());
    tracing::info!(bind = %local, uptime_ms = now_ms().saturating_sub(boot_ms), "coordinator listening");

    // Boot step 9 (contract §1.1): maintenance, scheduled AFTER the listener.
    // v2's ordering note is "the listeners must start before maintenance and
    // signal wiring" -- a backup that ran before the port was accepting would
    // compete with the very startup it is meant to protect.
    crate::maintenance::backup::spawn_scheduled_backups(state.services.db.clone());
    crate::maintenance::audit_retention::spawn_audit_retention(
        state.services.db.clone(),
        boot.config.audit_retention_days,
    );
    // The terminal view lease sweep (v2 arms it when the hub is built,
    // `terminal-view-hub.ts:256`), and the view and title hubs' release of
    // closed sessions, held for as long as this coordinator serves.
    crate::terminal_view::spawn_view_sweep(Arc::clone(&state.services.views));
    let _view_release = state
        .services
        .views
        .subscribe_session_close(&state.services.buses);
    let _title_release = state
        .services
        .titles
        .subscribe_session_close(&state.services.buses);
    let _terminal_signals_release = state
        .services
        .terminal_signals
        .subscribe_session_close(&state.services.buses);

    // Boot step 9, the pair-request half: a sweep that reclaims a request whose
    // deadline passed while this coordinator was DOWN. It runs before its first
    // sleep, so the reclaim is at boot rather than a minute later -- a live
    // pair request past its expiry is a credential until something notices.
    //
    // The SENDER is held here and the receiver is what the sweep consumes, and
    // the stop is two halves rather than one: dropping the sender tells a sweep
    // blocked in `changed()` to return, and `stop()` then waits out the tick
    // already in flight. A sweep that cannot be stopped is a leak with a name;
    // one stopped without waiting logs after its owner is gone. Passing `None`
    // here would be the unstoppable sweep, and is for tests only.
    let (pair_shutdown, pair_stopped) = tokio::sync::watch::channel(false);
    let pair_retention = crate::auth::pairing::spawn_pair_request_retention(
        Arc::clone(&state.services),
        pair_stopped,
    );

    // Nagle holds a small write for the peer's delayed ACK (~40 ms), clumping a
    // keystroke's echo frame behind later acknowledgements on every socket.
    let listener = axum::serve::ListenerExt::tap_io(listener, |tcp| {
        if let Err(error) = tcp.set_nodelay(true) {
            tracing::warn!(%error, "an accepted coordinator socket refused TCP_NODELAY");
        }
    });
    let drain = ShutdownDrain::new(Arc::clone(&state.draining), SHUTDOWN_DRAIN_TIMEOUT);
    let served = axum::serve(
        listener,
        mounted
            .router
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(drain.requested(shutdown_signal()));
    let served = drain.bound(served.into_future()).await;

    drop(pair_shutdown);
    pair_retention.stop().await;

    // A drain the bound cut short is a clean exit: the open sockets belong to
    // workers and browsers that redial.
    served.map_or(Ok(()), |outcome| outcome.context("coordinator listener"))
}

/// Boot steps 2-7 (contract §1.1): everything that must hold before a listener
/// exists.
///
/// Opens the database (pragmas, the pre-migration backup, migrations and their
/// validation all live in `db::open`), imports the authorized-keys file, enforces
/// the self-hosted tenant invariant and runs the startup janitor, in v2's order
/// (`main.ts:60-86`). `serve` is the one production caller; it is public so the
/// order is provable without binding a port.
pub async fn prepare_coordinator_database(
    config: &CoordConfig,
    now_ms: i64,
) -> anyhow::Result<(CoordDb, SelfHostedTenant)> {
    let database = crate::db::open(&config.database)
        .await
        .with_context(|| format!("coordinator database {:?}", config.database))?;

    // Step 5, BEFORE the tenancy invariant, so the invariant sees the keys the
    // operator's file just added (`main.ts:73-83`).
    import_authorized_keys_at_boot(&database, &config.authorized_keys_path, now_ms).await;

    // Step 6: a mis-scoped database must never bind a port, so a throw here is
    // the correct outcome rather than a late failure.
    let tenant = crate::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, now_ms)
        .await
        .with_context(|| "self-hosted tenant invariant")?;
    tracing::info!(
        backend = database.backend().label(),
        account_id = %tenant.account_id,
        organization_id = %tenant.organization_id,
        dashboard_id = %tenant.dashboard_id,
        "self-hosted tenant ready"
    );

    // Step 7: the startup janitor. It DELETES closed sessions and never touches
    // an open row -- a live terminal must not be deleted by a janitor -- and it
    // reports rather than erroring, so no context wrapper belongs here.
    let janitor = crate::maintenance::startup_janitor::run_startup_janitor(&database).await;
    tracing::info!(
        deleted_sessions = janitor.deleted_sessions,
        "startup janitor complete"
    );
    Ok((database, tenant))
}

/// Import the operator's authorized-keys file if it exists.
///
/// A failure is a warning and boot continues, as v2's does (`main.ts:73-80`):
/// the keys already in the database still authenticate, and refusing to boot
/// over one unreadable file would take every paired device down with it.
async fn import_authorized_keys_at_boot(database: &CoordDb, path: &Path, now_ms: i64) {
    if !path.exists() {
        tracing::debug!(path = %path.display(), "authorized_keys file absent: nothing to import");
        return;
    }
    match crate::auth::authorized_keys::import_authorized_keys(database, path, now_ms).await {
        Ok(count) => tracing::info!(count, path = %path.display(), "authorized_keys_imported"),
        Err(error) => tracing::warn!(
            error = %error,
            path = %path.display(),
            "authorized_keys_import_failed"
        ),
    }
}

/// The terminal collaborators a booted coordinator hands the workers domain.
///
/// BOTH ARE REAL. `ByteHub` is the route index and `TerminalViewHub` implements
/// `TerminalViewLifecycle` (`terminal_view/mod.rs:338`), so the second
/// collaborator is no longer `NoTerminalSeams`.
///
/// The no-op stays in the tree for tests that genuinely want a coordinator with
/// no terminal, and its own doc is why the wiring is asserted rather than
/// assumed: "two no-op types invite a caller to wire one and forget the other."
/// The two are behaviourally identical on a coordinator with no sessions — a
/// page renders the same either way — so **no behavioural test can tell them
/// apart**, and the only thing that keeps the no-op correct for tests and wrong
/// for production is `terminal_seam_wiring_is_the_real_hubs`.
pub fn terminal_seams(services: &CoordServices) -> CoordTerminal {
    // THE CONCRETE TYPES, not `Arc<dyn Trait>`. That is what lets
    // `CoordTerminal` name the collaborator it was given, and the names are the
    // whole point: the log line a worker retirement produces has to say which
    // hub was consulted, and a `Debug` that printed the container's own name
    // for both fields said neither.
    CoordTerminal::new(services.byte_hub.clone(), services.views.clone())
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

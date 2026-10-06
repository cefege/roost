// The one tenancy invariant, enforced before the listener exists.
//
// A self-hosted deployment means exactly one account, one organization and one
// dashboard. v2 enforces that in `ensureSelfHostedTenant`
// (`apps/coord/src/auth/self-hosted-tenant.ts:350`) and calls it from `main`
// with the comment "this is the only tenancy invariant and it must hold before
// any RPC runs". The `serve` path here had documented that boot order and
// implemented none of it, so a database with two accounts, or with a dashboard
// belonging to another organization, would have bound a port and answered RPCs.
//
// Every read and write below goes through ONE `BEGIN IMMEDIATE` transaction, so
// two coordinators racing at boot cannot each observe an empty topology and
// each build one. Passing the pool instead would make the transaction
// decorative, and a decorative transaction is worse than none: it reads as
// proof of atomicity that nothing provides.
//
// ONE BRANCH IS DEFENCE-IN-DEPTH, NOT A LIVE PATH. `db::open` sets
// `foreign_keys(true)`, so a dashboard cannot name an organization that does
// not exist, and the "at most one organization" rule fires first for any
// database that does have two. `dashboard belongs to another organization`
// is therefore reachable only through a connection with the pragma off -- a
// repair tool, or a raw session on the file. It is kept because that is a
// real thing that happens to a restored database, and because a check that
// costs one comparison is cheaper than the incident it prevents. It is NOT
// testable through `CoordDb::open`, and `tests/self_hosted_tenant.rs` says so
// where a reader would otherwise waste an hour trying.
//
// ONE MORE BRANCH IS A SETTING'S SCOPE, and it is the only one that is a live
// path. `app_settings` holds one coordinator-global key at the NULL scope and
// every other key at a dashboard's, and no SQL constraint can say which is
// which — so `require_valid_setting_scopes` says it, and `roost import-v2` is
// how a row the previous product wrote arrives carrying the wrong one.

use roost_protocol::{ProtocolError, ProtocolResult};
use sqlx::{Any, Transaction};

use crate::db::{CoordDb, SqlBuilder};

/// The three ids a self-hosted deployment is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHostedTenant {
    /// The single account.
    pub account_id: String,
    /// The single organization.
    pub organization_id: String,
    /// The single dashboard.
    pub dashboard_id: String,
}

impl SelfHostedTenant {
    /// The dashboard a request is scoped to when it names none.
    #[must_use]
    pub fn default_dashboard(&self) -> &str {
        &self.dashboard_id
    }
}

/// Why the database is not a self-hosted deployment.
fn refuse(reason: &str) -> ProtocolError {
    ProtocolError::new("coord.self_hosted_tenant", reason)
}

/// Insert the account, organization, dashboard and both memberships.
async fn create_topology(
    transaction: &mut Transaction<'_, Any>,
    now_ms: i64,
) -> ProtocolResult<SelfHostedTenant> {
    let account_id = format!("acct_{now_ms}");
    let organization_id = format!("org_{now_ms}");
    let dashboard_id = format!("dash_{now_ms}");

    let statements: [(&str, Vec<String>, Vec<i64>); 5] = [
        (
            "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
             VALUES ($1, 'local@roost.invalid', 'active', $2)",
            vec![account_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO organizations (id, slug, name, status, created_at_ms) \
             VALUES ($1, 'personal', 'Personal', 'active', $2)",
            vec![organization_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO organization_memberships \
               (organization_id, account_id, role, created_at_ms) \
             VALUES ($1, $2, 'owner', $3)",
            vec![organization_id.clone(), account_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
             VALUES ($1, $2, 'default', 'Personal', 'active', $3)",
            vec![dashboard_id.clone(), organization_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO dashboard_memberships \
               (dashboard_id, account_id, role, created_at_ms) \
             VALUES ($1, $2, 'admin', $3)",
            vec![dashboard_id.clone(), account_id.clone()],
            vec![now_ms],
        ),
    ];
    for (sql, texts, numbers) in statements {
        let mut query = sqlx::query(sql);
        for text in texts {
            query = query.bind(text);
        }
        for number in numbers {
            query = query.bind(number);
        }
        query
            .execute(&mut **transaction)
            .await
            .map_err(|error| refuse(&format!("create self-hosted topology: {error}")))?;
    }

    Ok(SelfHostedTenant {
        account_id,
        organization_id,
        dashboard_id,
    })
}

/// The organization membership must be exactly this account, as owner.
async fn require_owner_membership(
    transaction: &mut Transaction<'_, Any>,
    organization_id: &str,
    account_id: &str,
) -> ProtocolResult<()> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT organization_id, account_id FROM organization_memberships LIMIT 2")
            .fetch_all(&mut **transaction)
            .await
            .map_err(|error| refuse(&format!("organization memberships: {error}")))?;
    if rows.len() != 1 || rows[0].0 != organization_id || rows[0].1 != account_id {
        return Err(refuse("organization owner membership is incomplete"));
    }
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT role FROM organization_memberships WHERE organization_id = $1 AND account_id = $2",
    )
    .bind(organization_id)
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| refuse(&format!("organization membership role: {error}")))?;
    if roles.as_slice() != ["owner"] {
        return Err(refuse("organization owner membership is incomplete"));
    }
    Ok(())
}

/// The dashboard membership must be exactly this account, as admin.
async fn require_admin_membership(
    transaction: &mut Transaction<'_, Any>,
    dashboard_id: &str,
    account_id: &str,
) -> ProtocolResult<()> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT dashboard_id, account_id FROM dashboard_memberships LIMIT 2")
            .fetch_all(&mut **transaction)
            .await
            .map_err(|error| refuse(&format!("dashboard memberships: {error}")))?;
    if rows.len() != 1 || rows[0].0 != dashboard_id || rows[0].1 != account_id {
        return Err(refuse("dashboard admin membership is incomplete"));
    }
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT role FROM dashboard_memberships WHERE dashboard_id = $1 AND account_id = $2",
    )
    .bind(dashboard_id)
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| refuse(&format!("dashboard membership role: {error}")))?;
    if roles.as_slice() != ["admin"] {
        return Err(refuse("dashboard admin membership is incomplete"));
    }
    Ok(())
}

/// Inspect the topology, creating it when the database is empty.
async fn inspect_or_create(
    transaction: &mut Transaction<'_, Any>,
    now_ms: i64,
) -> ProtocolResult<SelfHostedTenant> {
    let accounts: Vec<(String, String)> = sqlx::query_as("SELECT id, status FROM accounts LIMIT 2")
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| refuse(&format!("accounts: {error}")))?;
    let organizations: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM organizations LIMIT 2")
            .fetch_all(&mut **transaction)
            .await
            .map_err(|error| refuse(&format!("organizations: {error}")))?;
    let dashboard_rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, organization_id, status FROM dashboards LIMIT 2")
            .fetch_all(&mut **transaction)
            .await
            .map_err(|error| refuse(&format!("dashboards: {error}")))?;

    if accounts.len() > 1 {
        return Err(refuse("multiple accounts"));
    }
    if organizations.len() > 1 {
        return Err(refuse("multiple organizations"));
    }
    if dashboard_rows.len() > 1 {
        return Err(refuse("multiple dashboards"));
    }

    if accounts.is_empty() {
        return create_topology(transaction, now_ms).await;
    }

    let (account_id, account_status) = accounts[0].clone();
    let (organization_id, organization_status) = organizations
        .first()
        .cloned()
        .ok_or_else(|| refuse("identity topology is partial"))?;
    let (dashboard_id, dashboard_organization, dashboard_status) = dashboard_rows
        .first()
        .cloned()
        .ok_or_else(|| refuse("identity topology is partial"))?;

    if account_status != "active" {
        return Err(refuse("account is inactive"));
    }
    if organization_status != "active" {
        return Err(refuse("organization is inactive"));
    }
    if dashboard_status != "active" {
        return Err(refuse("dashboard is inactive"));
    }
    if dashboard_organization != organization_id {
        return Err(refuse("dashboard belongs to another organization"));
    }
    require_owner_membership(transaction, &organization_id, &account_id).await?;
    require_admin_membership(transaction, &dashboard_id, &account_id).await?;
    require_valid_setting_scopes(transaction, &dashboard_id).await?;
    Ok(SelfHostedTenant {
        account_id,
        organization_id,
        dashboard_id,
    })
}

/// The `app_settings` keys that belong to the COORDINATOR rather than to a
/// dashboard, and so live at the `dashboard_id IS NULL` scope.
///
/// One list, and a second global key is one line here. Every other key in the
/// table is read at one dashboard's scope (`agents::config`,
/// `diagnostics::transcription`), so a key missing from this list is a
/// dashboard-scoped key, and an unscoped row for one of those is drift no
/// reader can see.
const GLOBAL_SETTING_KEYS: &[&str] = &[crate::push::vapid::VAPID_SETTING_KEY];

/// Every `app_settings` row has to sit at the scope its reader uses.
///
/// A coordinator-global key stored at a dashboard scope is the shape that
/// breaks push silently: `push::vapid` reads and writes the NULL scope and
/// nowhere else, so the scoped copy is unreachable from every code path that
/// exists, the NULL-scoped read finds nothing, and the next `PushGetConfig`
/// MINTS A SECOND IDENTITY. Every subscription already in `push_subscriptions`
/// was signed by the first one, so an install that was being notified stops
/// being notified and reports no reason. The reverse shape is quieter still: a
/// key read at a dashboard scope and stored unscoped reads as unset, and the
/// operator's agent config or transcription key is gone with no error.
///
/// v2 refused both at boot and that refusal is the fail-safe; without it a
/// mis-scoped row is admitted and the next occurrence is a support ticket.
/// `roost import-v2` is how one reaches a v3 database, because it carries
/// `app_settings` across verbatim.
///
/// "outside this dashboard" is defence-in-depth and not a live path, for the
/// reason the module header gives for the organization check: `db::open`
/// enforces foreign keys and the at-most-one-dashboard rule has already run, so
/// a row can only name a dashboard that does not exist through a connection with
/// the pragma off. It is kept because a check that costs one comparison is
/// cheaper than the incident it prevents.
async fn require_valid_setting_scopes(
    transaction: &mut Transaction<'_, Any>,
    dashboard_id: &str,
) -> ProtocolResult<()> {
    let mut scoped_global = SqlBuilder::new(
        "SELECT key, dashboard_id FROM app_settings \
          WHERE dashboard_id IS NOT NULL AND key IN (",
    );
    push_global_keys(&mut scoped_global);
    scoped_global.push(") LIMIT 1");
    if let Some((key, scope)) = scoped_global
        .build_query_as::<(String, String)>()
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| refuse(&format!("app_settings scopes: {error}")))?
    {
        return Err(refuse(&format!(
            "app_settings key {key} is coordinator-global and is stored scoped to \
             dashboard {scope}; every code path that reads it requires the NULL \
             scope, so the value is unreachable and the identity will be minted \
             again on first use"
        )));
    }

    let mut dashboard_scoped =
        SqlBuilder::new("SELECT key, dashboard_id FROM app_settings WHERE key NOT IN (");
    push_global_keys(&mut dashboard_scoped);
    dashboard_scoped.push(") AND (dashboard_id IS NULL OR dashboard_id <> ");
    dashboard_scoped.push_bind(dashboard_id);
    dashboard_scoped.push(") LIMIT 1");
    if let Some((key, scope)) = dashboard_scoped
        .build_query_as::<(String, Option<String>)>()
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| refuse(&format!("app_settings scopes: {error}")))?
    {
        let described = scope.as_deref().unwrap_or("no dashboard");
        return Err(refuse(&format!(
            "app_settings key {key} is a dashboard-scoped key and is scoped to \
             {described}; it is read at one dashboard's scope, so the row is \
             unreachable"
        )));
    }
    Ok(())
}

/// Bind every coordinator-global key into an `IN (...)` that is still open.
fn push_global_keys(statement: &mut SqlBuilder) {
    let mut keys = statement.separated(", ");
    for key in GLOBAL_SETTING_KEYS {
        keys.push_bind(*key);
    }
}

/// Enforce the invariant, and hand back the ids the process is scoped to.
pub async fn ensure_self_hosted_tenant(
    database: &CoordDb,
    now_ms: i64,
) -> ProtocolResult<SelfHostedTenant> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(|error| refuse(&format!("begin: {error}")))?;
    let tenant = inspect_or_create(&mut transaction, now_ms).await?;
    transaction
        .commit()
        .await
        .map_err(|error| refuse(&format!("commit: {error}")))?;
    Ok(tenant)
}

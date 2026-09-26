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

use roost_protocol::{ProtocolError, ProtocolResult};
use sqlx::{Sqlite, Transaction};

use crate::db::CoordDb;

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
    transaction: &mut Transaction<'_, Sqlite>,
    now_ms: i64,
) -> ProtocolResult<SelfHostedTenant> {
    let account_id = format!("acct_{now_ms}");
    let organization_id = format!("org_{now_ms}");
    let dashboard_id = format!("dash_{now_ms}");

    let statements: [(&str, Vec<String>, Vec<i64>); 5] = [
        (
            "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
             VALUES (?, 'local@roost.invalid', 'active', ?)",
            vec![account_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO organizations (id, slug, name, status, created_at_ms) \
             VALUES (?, 'personal', 'Personal', 'active', ?)",
            vec![organization_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO organization_memberships \
               (organization_id, account_id, role, created_at_ms) \
             VALUES (?, ?, 'owner', ?)",
            vec![organization_id.clone(), account_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
             VALUES (?, ?, 'default', 'Personal', 'active', ?)",
            vec![dashboard_id.clone(), organization_id.clone()],
            vec![now_ms],
        ),
        (
            "INSERT INTO dashboard_memberships \
               (dashboard_id, account_id, role, created_at_ms) \
             VALUES (?, ?, 'admin', ?)",
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
    transaction: &mut Transaction<'_, Sqlite>,
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
        "SELECT role FROM organization_memberships WHERE organization_id = ? AND account_id = ?",
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
    transaction: &mut Transaction<'_, Sqlite>,
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
        "SELECT role FROM dashboard_memberships WHERE dashboard_id = ? AND account_id = ?",
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
    transaction: &mut Transaction<'_, Sqlite>,
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

    Ok(SelfHostedTenant {
        account_id,
        organization_id,
        dashboard_id,
    })
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

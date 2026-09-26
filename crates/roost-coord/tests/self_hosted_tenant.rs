// The self-hosted tenancy invariant, against a real migrated database.
//
// The invariant is the one thing that stands between a mis-scoped database and
// a bound port, so these tests assert the refusals, not just the happy path:
// v2's rule is that this runs before the listener exists precisely so a
// database with two accounts never answers an RPC.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::db::CoordDb;
use sqlx::AssertSqlSafe;

/// A migrated database in a directory that removes itself.
struct TenantFixture {
    database: CoordDb,
    #[allow(dead_code)]
    root: PathBuf,
}

impl TenantFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-tenant-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        Self { database, root }
    }

    async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.database.pool())
            .await
            .expect("the statement applies");
    }

    async fn count(&self, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
            .fetch_one(self.database.pool())
            .await
            .expect("a count")
    }
}

impl Drop for TenantFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn an_empty_database_gains_exactly_one_of_each_thing() {
    let fixture = TenantFixture::new("empty").await;

    let tenant = ensure_self_hosted_tenant(&fixture.database, 1_000)
        .await
        .expect("an empty database is a self-hosted deployment to create");

    assert!(!tenant.account_id.is_empty());
    assert!(!tenant.organization_id.is_empty());
    assert_eq!(tenant.default_dashboard(), tenant.dashboard_id.as_str());
    assert_eq!(fixture.count("accounts").await, 1);
    assert_eq!(fixture.count("organizations").await, 1);
    assert_eq!(fixture.count("dashboards").await, 1);
    assert_eq!(fixture.count("organization_memberships").await, 1);
    assert_eq!(fixture.count("dashboard_memberships").await, 1);
}

#[tokio::test]
async fn running_it_twice_is_idempotent_and_keeps_the_same_ids() {
    let fixture = TenantFixture::new("twice").await;

    let first = ensure_self_hosted_tenant(&fixture.database, 1_000)
        .await
        .expect("the first run creates the topology");
    let second = ensure_self_hosted_tenant(&fixture.database, 2_000)
        .await
        .expect("the second run finds it already there");

    assert_eq!(first, second, "a boot must not mint a second dashboard");
    assert_eq!(fixture.count("accounts").await, 1);
    assert_eq!(fixture.count("dashboards").await, 1);
}

/// The refusal that matters: two accounts is not a self-hosted deployment, and
/// a coordinator that bound a port anyway would serve two tenants' rows from
/// one process.
#[tokio::test]
async fn a_second_account_is_refused_rather_than_served() {
    let fixture = TenantFixture::new("two-accounts").await;
    ensure_self_hosted_tenant(&fixture.database, 1_000)
        .await
        .expect("the first run creates the topology");
    fixture
        .exec(
            "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
              VALUES ('acct_other', 'other@roost.invalid', 'active', 1)",
        )
        .await;

    let refusal = ensure_self_hosted_tenant(&fixture.database, 2_000)
        .await
        .expect_err("two accounts is not self-hosted");

    assert!(
        refusal.to_string().contains("multiple accounts"),
        "the refusal must name the rule, got: {refusal}"
    );
}

/// A second organization is refused, and it is refused BEFORE the
/// dashboard-ownership rule -- which is why the ownership rule cannot be
/// reached from a test that adds an organization to make one foreign. See the
/// module header on `self_hosted_tenant`: with `foreign_keys(true)` that rule
/// is defence-in-depth, not a live path.
#[tokio::test]
async fn a_second_organization_is_refused_before_ownership_is_considered() {
    let fixture = TenantFixture::new("two-orgs").await;
    let tenant = ensure_self_hosted_tenant(&fixture.database, 1_000)
        .await
        .expect("the first run creates the topology");
    fixture
        .exec(
            "INSERT INTO organizations (id, slug, name, status, created_at_ms) \
              VALUES ('org_other', 'other', 'Other', 'active', 1)",
        )
        .await;
    fixture
        .exec(&format!(
            "UPDATE dashboards SET organization_id = 'org_other' WHERE id = '{}'",
            tenant.dashboard_id
        ))
        .await;

    let refusal = ensure_self_hosted_tenant(&fixture.database, 2_000)
        .await
        .expect_err("two organizations is not self-hosted");

    assert!(
        refusal.to_string().contains("multiple organizations"),
        "the refusal must name the rule, got: {refusal}"
    );
}

#[tokio::test]
async fn an_inactive_account_is_refused() {
    let fixture = TenantFixture::new("inactive").await;
    let tenant = ensure_self_hosted_tenant(&fixture.database, 1_000)
        .await
        .expect("the first run creates the topology");
    fixture
        .exec(&format!(
            "UPDATE accounts SET status = 'disabled' WHERE id = '{}'",
            tenant.account_id
        ))
        .await;

    let refusal = ensure_self_hosted_tenant(&fixture.database, 2_000)
        .await
        .expect_err("a disabled account cannot act");

    assert!(
        refusal.to_string().contains("account is inactive"),
        "the refusal must name the rule, got: {refusal}"
    );
}

#[tokio::test]
async fn a_missing_membership_is_a_partial_topology_and_is_refused() {
    let fixture = TenantFixture::new("no-membership").await;
    ensure_self_hosted_tenant(&fixture.database, 1_000)
        .await
        .expect("the first run creates the topology");
    fixture.exec("DELETE FROM dashboard_memberships").await;

    let refusal = ensure_self_hosted_tenant(&fixture.database, 2_000)
        .await
        .expect_err("a dashboard with no admin is not a deployment");

    assert!(
        refusal
            .to_string()
            .contains("dashboard admin membership is incomplete"),
        "the refusal must name the rule, got: {refusal}"
    );
}

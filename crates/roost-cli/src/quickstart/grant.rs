//! Minting a one-shot enrollment grant on the machine that hosts the
//! coordinator, and holding the live bearer so nothing can print it by
//! accident. Called by `quickstart` (the local worker and the paired browser)
//! and by `add-machine` (the next machine to enroll). Depends on
//! `roost-coord` for the database and for the single-tenant invariant, and on
//! the deploy journal's own digest helper; it writes one row and holds one
//! string.
//!
//! **The live grant is a credential and is treated as one.** Only its SHA-256
//! digest is ever persisted, exactly as the coordinator stores it, and the live
//! value leaves this module through [`OneShotGrant::expose`] and nowhere else.
//! `Debug` prints `redacted` rather than the value, so a grant cannot reach a
//! log line through a derived format, a `?`-propagated error, or a struct a
//! caller printed while debugging. `docs/phase6-cli-contract.md` says
//! quickstart "never prints or logs the one-shot grant", and that is why this
//! type exists rather than a bare `String`.
//!
//! **This mints a coordinator value from the CLI, and that is a fork — waiting
//! on one merge.** `roost_coord::auth::bootstrap_tokens::mint_host_bootstrap_token`
//! already exists, on `v3-coord` at `edac76e2`, mirroring v2's
//! `apps/coord/src/auth/bootstrap-tokens.ts`. **This worktree has not merged
//! `v3-coord`, so the module is not here and the call cannot be written yet.**
//! Until it is, the bearer format, the `bootstrap_tokens` column list, the
//! 24-hour lifetime and the digest are four hand-restated copies of
//! coordinator values living in a second crate, and they are restated here only
//! so the CLI half is complete and compiling the day the merge lands. When it
//! does, the whole of [`mint_host_grant`] collapses to that one call and this
//! note with it. The tenancy invariant is already the coordinator's:
//! `ensure_self_hosted_tenant` is called, not reimplemented.

use std::fmt;

use roost_host::DatabaseLocation;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::services::deploy_journal::sha256_hex;

/// What a minted grant will authorize, which is the value the coordinator's
/// `bootstrap_tokens.kind` column constrains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// Enrolls a machine's worker.
    Worker,
    /// Pairs one browser.
    Browser,
}

impl GrantKind {
    /// The value the coordinator's schema accepts.
    pub const fn as_str(self) -> &'static str {
        match self {
            GrantKind::Worker => "worker",
            GrantKind::Browser => "browser",
        }
    }
}

/// How long a minted grant stays acceptable. It is a long window because the
/// thing it authorizes is a human carrying a laptop to another machine; it is
/// bounded because a grant is one-shot, and a one-shot that never expires is a
/// password.
pub const GRANT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// The stand-in a `--dry-run` prints in place of a real grant.
///
/// A dry run must render the definition a real run would write, and the
/// difference between the two is exactly one value: a bearer nobody may have.
/// Printing a plausible-looking `roost_bt_` string would be a credential-shaped
/// lie in a plan an operator reads before committing, so the placeholder is
/// named as one and the definition says which.
pub const PLACEHOLDER_BEARER: &str = "<one-shot grant minted at install time>";

/// Bytes of entropy behind a bearer. 24 bytes is 192 bits, which no amount of
/// offline guessing reaches; the value exists so a future change of length is
/// a deliberate act rather than a truncation nobody notices.
const BEARER_ENTROPY_BYTES: usize = 24;

/// The prefix every Roost enrollment bearer carries, so one is recognisable in
/// a log a human is reading without being printed into one on purpose.
const BEARER_PREFIX: &str = "roost_bt_";

/// A live one-shot grant. Constructed only by [`mint_host_grant`], and the only
/// way to read the value is [`expose`](OneShotGrant::expose).
pub struct OneShotGrant {
    bearer: String,
    expires_at_ms: i64,
}

/// Deliberately not derived: a `Debug` that printed the bearer would defeat
/// every `#[derive(Debug)]` upstream of it, and the temptation to add one is
/// exactly how a credential reaches a log.
impl fmt::Debug for OneShotGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OneShotGrant")
            .field("bearer", &"redacted")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl OneShotGrant {
    /// The live bearer, for the one line an operator must copy.
    pub fn expose(&self) -> &str {
        &self.bearer
    }

    /// When the coordinator will stop accepting it, in milliseconds since the
    /// epoch. Reported so an operator knows how long the command they are about
    /// to paste stays valid.
    pub fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }
}

/// The digest the coordinator stores for `bearer`.
///
/// Public because it is also the lookup key for proving a grant was claimed,
/// and because a caller that hashes a value to compare it must not invent a
/// second hash.
pub fn grant_digest(bearer: &str) -> String {
    sha256_hex(bearer.as_bytes())
}

/// Mint a grant against the coordinator's own database.
///
/// A SQLite database must already exist. `roost_coord::db::open` creates and
/// migrates a file that is not there, which is right for the coordinator's boot
/// and wrong here: a mistyped or derived path would produce a brand-new empty
/// database, the tenant would be created in it, and the grant would be printed
/// into an enrollment command that can never be redeemed — with a junk
/// database left behind. An existing file is the only proof that this is the
/// database the running coordinator is actually using. A Postgres URL is the
/// one the coordinator itself was given, so it needs no such proof.
pub async fn mint_host_grant(
    database: &DatabaseLocation,
    kind: GrantKind,
    label: &str,
    now_ms: i64,
) -> Result<OneShotGrant, CommandFailure> {
    let described = describe_database(database);
    if let DatabaseLocation::SqliteFile(path) = database
        && !path.is_file()
    {
        return Err(codes::refuse(
            codes::NO_COORDINATOR_URL,
            format!(
                "the coordinator database {described} does not exist, so this host is not a \
                 coordinator; run this command on the machine the coordinator is installed on"
            ),
        ));
    }
    let bearer = format!("{BEARER_PREFIX}{}", random_hex()?);
    let digest = grant_digest(&bearer);
    let expires_at_ms = now_ms + GRANT_TTL_MS;

    let opened = roost_coord::db::open(database).await.map_err(|error| {
        codes::refuse(
            codes::NO_COORDINATOR_URL,
            format!("the coordinator database {described} could not be opened: {error}"),
        )
    })?;
    let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&opened, now_ms)
        .await
        .map_err(|error| {
            codes::refuse(
                codes::NO_COORDINATOR_URL,
                format!(
                    "this coordinator's tenant could not be resolved from {described}: {error}"
                ),
            )
        })?;
    sqlx::query(
        "INSERT INTO bootstrap_tokens (\
           token_hash, account_id, dashboard_id, kind, label,\
           created_at_ms, expires_at_ms, used_at_ms, used_by_fp, minted_by_fp)\
         VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, NULL, NULL)",
    )
    .bind(&digest)
    .bind(&tenant.account_id)
    .bind(&tenant.dashboard_id)
    .bind(kind.as_str())
    .bind(label)
    .bind(now_ms)
    .bind(expires_at_ms)
    .execute(opened.pool())
    .await
    .map_err(|error| {
        codes::refuse(
            codes::NO_COORDINATOR_URL,
            format!("the {kind:?} grant could not be recorded in {described}: {error}"),
        )
    })?;

    Ok(OneShotGrant {
        bearer,
        expires_at_ms,
    })
}

/// The database as an operator-facing message names it: the file path, or the
/// backend alone — a Postgres URL carries a password and never reaches stderr.
pub(crate) fn describe_database(database: &DatabaseLocation) -> String {
    match database {
        DatabaseLocation::SqliteFile(path) => path.display().to_string(),
        DatabaseLocation::Postgres(_) => {
            "(the Postgres server in ROOST_COORDINATOR_DATABASE_URL)".to_owned()
        }
    }
}

/// Bytes from the operating system's CSPRNG, hex-encoded, through `getrandom`
/// so every platform has one spelling. A hand-rolled PRNG standing in for a
/// CSPRNG is the one substitution that must never be made for tidiness.
fn random_hex() -> Result<String, CommandFailure> {
    let mut bytes = [0_u8; BEARER_ENTROPY_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| {
        CommandFailure::generic(format!(
            "this machine's entropy source could not be read: {error}"
        ))
    })?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use roost_host::DatabaseLocation;

    use super::{
        BEARER_PREFIX, GRANT_TTL_MS, GrantKind, grant_digest, mint_host_grant, random_hex,
    };
    use crate::deploy::codes;
    use crate::services::deploy_journal::sha256_hex;

    /// A throwaway tree that removes itself, so a grant test never writes into
    /// a database a real coordinator is using.
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(case: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "roost-grant-{}-{case}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&root).expect("the throwaway tree is created");
            Self { root }
        }

        fn database(&self) -> PathBuf {
            self.root.join("coordinator.sqlite")
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A database created and migrated by the coordinator's own boot, which is
    /// the only database a grant is ever minted into.
    async fn coordinator_database(tree: &TempTree) -> DatabaseLocation {
        let database = DatabaseLocation::SqliteFile(tree.database());
        roost_coord::db::open(&database)
            .await
            .expect("the coordinator database is created and migrated");
        database
    }

    #[tokio::test]
    async fn a_minted_grant_is_stored_as_a_digest_and_the_bearer_is_the_only_live_copy() {
        let tree = TempTree::new("digest-only");
        let database = coordinator_database(&tree).await;
        let grant = mint_host_grant(
            &database,
            GrantKind::Worker,
            "add-machine",
            1_700_000_000_000,
        )
        .await
        .expect("a grant is minted");

        let bearer = grant.expose().to_string();
        assert!(bearer.starts_with(BEARER_PREFIX), "{bearer}");
        assert_eq!(grant.expires_at_ms(), 1_700_000_000_000 + GRANT_TTL_MS);

        let pool = roost_coord::db::open(&database).await.expect("reopened");
        let rows: Vec<(String, String, String, i64)> =
            sqlx::query_as("SELECT token_hash, kind, label, expires_at_ms FROM bootstrap_tokens")
                .fetch_all(pool.pool())
                .await
                .expect("the grant row is readable");
        assert_eq!(rows.len(), 1, "one mint writes one row");
        let (hash, kind, label, expires_at_ms) = &rows[0];
        assert_eq!(
            hash,
            &grant_digest(&bearer),
            "the stored value is the digest"
        );
        assert_ne!(hash, &bearer, "the bearer itself is never stored");
        assert_eq!(kind, "worker");
        assert_eq!(label, "add-machine");
        assert_eq!(*expires_at_ms, 1_700_000_000_000 + GRANT_TTL_MS);
    }

    #[tokio::test]
    async fn two_mints_are_two_distinct_bearers() {
        let tree = TempTree::new("distinct");
        let database = coordinator_database(&tree).await;
        let first = mint_host_grant(&database, GrantKind::Worker, "a", 1_700_000_000_000)
            .await
            .expect("minted");
        let second = mint_host_grant(&database, GrantKind::Browser, "b", 1_700_000_000_000)
            .await
            .expect("minted");
        assert_ne!(first.expose(), second.expose());
        assert_ne!(grant_digest(first.expose()), grant_digest(second.expose()));
    }

    #[tokio::test]
    async fn a_debug_rendering_never_carries_the_bearer() {
        let tree = TempTree::new("redacted");
        let database = coordinator_database(&tree).await;
        let grant = mint_host_grant(&database, GrantKind::Browser, "quickstart-browser", 1)
            .await
            .expect("minted");
        let rendered = format!("{grant:?}");
        assert!(!rendered.contains(grant.expose()), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[tokio::test]
    async fn a_database_that_does_not_exist_is_refused_rather_than_created() {
        let tree = TempTree::new("absent");
        let path = tree.database();
        let failure = mint_host_grant(
            &DatabaseLocation::SqliteFile(path.clone()),
            GrantKind::Worker,
            "add-machine",
            1,
        )
        .await
        .expect_err("a database that is not there is refused");
        assert_eq!(failure.code, codes::NO_COORDINATOR_URL);
        assert!(
            failure.message.contains(&path.display().to_string()),
            "{failure}"
        );
        assert!(
            !path.exists(),
            "a refused mint must not leave a database behind"
        );
    }

    #[test]
    fn the_digest_is_the_same_sha256_the_deploy_journal_owns() {
        assert_eq!(
            grant_digest("roost_bt_example"),
            sha256_hex(b"roost_bt_example")
        );
    }

    #[test]
    fn a_draw_is_ninety_six_bits_of_hex_and_is_not_repeated() {
        let first = random_hex().expect("entropy is readable");
        assert_eq!(first.len(), 48, "{first}");
        let second = random_hex().expect("entropy is readable");
        assert_ne!(first, second);
    }
}

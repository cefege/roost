//! What a v2 import copies, in what order, and what a re-run does about it.
//! Called by `import_v2::copy`, which does the I/O. Depends on nothing but
//! itself, so every decision here is decidable without a database; the tests
//! are `crates/roost-cli/tests/import_v2_plan.rs`.
//!
//! **THE SCOPE IS A LIST, NOT A NEGATION.** `roost import-v2` exists to carry
//! paired browsers across the cutover, and the tables below are the ones that
//! decide whether a browser is still paired. Everything else in a v2 database
//! describes machines, sessions or history belonging to the v2 product, and
//! the ones with the most rows are the ones that must never be enumerated:
//! `events` and `audit_log` are ~1.5 M rows of a product being replaced. Naming
//! what is copied is therefore the safety property — a table nobody thought
//! about is simply not in this list, rather than being excluded by a rule
//! somebody has to remember to extend.

/// The schema-qualified name of the attached v2 database.
///
/// One name, so a statement cannot be half-qualified: `main` is the v3 install
/// being written and `src` is the v2 database being read, and a statement that
/// named neither would be a statement about whichever happened to be open.
pub const SOURCE_SCHEMA: &str = "src";

/// Which rows of a table an import takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSelection {
    /// Every row.
    Every,
    /// Only rows whose `fingerprint` is one an account paired.
    ///
    /// `authorized_keys` holds two kinds of key. A paired browser has a row in
    /// `account_devices`; a machine does not — its fingerprint is its
    /// `workers.fp`. Copying the machine keys would enrol five keys that
    /// belong to no paired browser and can never be presented by one, so the
    /// fleet would show five authenticators no human ever paired. On the
    /// database this was written for that is 26 of 31 rows, and the filter is
    /// the difference between the right number and a plausible-looking one.
    OnAccountDevice,
}

/// What a re-run does with a table that is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// A row the v3 install already has wins.
    ///
    /// Right for anything an operator can edit in v3, and `app_settings` is
    /// the case that matters — it holds the Deepgram key, the VAPID keypair
    /// and the agent settings, so an operator who already changed one in v3
    /// must not have it reverted by re-running an import.
    KeepExisting,
    /// Insert the revocations v3 is missing, then delete the rows they cover.
    ///
    /// Ordered that way on purpose: the coordinator's schema has a trigger
    /// refusing to insert a key that is already revoked, so the revocations
    /// cannot be in place while keys are being copied, and the deletions must
    /// be in place before anything can present a revoked key again.
    ApplyRevocations,
}

/// One table's contribution to an import.
#[derive(Debug, Clone, Copy)]
pub struct TableSpec {
    /// The table's name, which is also what the report line is about.
    pub table: &'static str,
    /// Which rows are taken.
    pub selection: RowSelection,
    /// What a re-run does.
    pub refresh: Refresh,
}

/// The tables an import copies, in the order it must copy them.
///
/// **THE ORDER IS FUNCTIONAL, NOT COSMETIC.** Foreign keys are enforced. An
/// identity has to exist before a membership names it; a key has to exist
/// before the device row that references its fingerprint; a dashboard has to
/// exist before the `app_settings` row scoped to it. Reading this list as an
/// unordered set would produce a constraint failure halfway through the one
/// transaction that owns the whole identity topology.
pub const TABLES: &[TableSpec] = &[
    TableSpec {
        table: "accounts",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "account_identities",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "organizations",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "organization_memberships",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "dashboards",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "dashboard_memberships",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "authorized_keys",
        selection: RowSelection::OnAccountDevice,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "account_devices",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
    TableSpec {
        table: "authorized_key_revocations",
        selection: RowSelection::Every,
        refresh: Refresh::ApplyRevocations,
    },
    TableSpec {
        table: "app_settings",
        selection: RowSelection::Every,
        refresh: Refresh::KeepExisting,
    },
];

/// How a target database answers to an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportMode {
    /// The target has no account: this install has never existed, so every
    /// row is written.
    ///
    /// The emptiness is what makes writing them verbatim safe. A target that
    /// had anything in it would be somebody else's install.
    FirstRun,
    /// The target already holds this install: bring it up to date without
    /// undoing anything v3 decided since.
    Refresh,
}

/// Why a target cannot be imported into, in the words the operator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeRefusal {
    /// The v3 database holds an account, and it is not the one being imported.
    ///
    /// A refusal rather than a merge: two accounts in one coordinator is a
    /// state v3 does not have, and the honest repair is for the operator to
    /// say which install this machine is.
    DifferentInstall {
        /// The account the v3 database already holds.
        installed: String,
        /// The account the v2 database holds.
        imported: String,
    },
    /// The v3 database holds more than one account, so it is not a v3 install
    /// this command understands.
    NotSelfHosted {
        /// How many accounts there are.
        accounts: usize,
    },
}

/// Decide how to import into a target holding `installed_accounts`.
///
/// The source's account is the authority for which install this is: carrying
/// a different account's rows into a live v3 install would replace the
/// identity every one of its other rows refers to.
pub fn decide_mode(
    installed_accounts: &[String],
    imported_account: &str,
) -> Result<ImportMode, ModeRefusal> {
    match installed_accounts {
        [] => Ok(ImportMode::FirstRun),
        [only] if only == imported_account => Ok(ImportMode::Refresh),
        [only] => Err(ModeRefusal::DifferentInstall {
            installed: only.clone(),
            imported: imported_account.to_string(),
        }),
        many => Err(ModeRefusal::NotSelfHosted {
            accounts: many.len(),
        }),
    }
}

/// The primary key a table is matched and counted on.
///
/// Named per table rather than read out of `PRAGMA` at runtime, because a key
/// discovered dynamically is a second schema to keep in step, and because
/// three of these tables are keyed on something other than `id` — asking them
/// for an `id` column is a query that fails, which is how a wrong guess gets
/// caught rather than shipped.
#[must_use]
pub fn key_columns(table: &str) -> &'static [&'static str] {
    match table {
        "account_identities" => &["issuer", "subject"],
        "organization_memberships" => &["organization_id", "account_id"],
        "dashboard_memberships" => &["dashboard_id", "account_id"],
        "app_settings" => &["dashboard_id", "key"],
        "authorized_keys" | "account_devices" | "authorized_key_revocations" => &["fingerprint"],
        _ => &["id"],
    }
}

/// The device filter as a `WHERE` body, over `alias` when the query aliases
/// its source rows and bare when it does not.
///
/// `joiner` is the keyword the predicate hangs off — `" WHERE "` or
/// `" AND "` — and it is an argument rather than a constant because the two
/// shapes are not interchangeable: a `WHERE` shape reused where an `AND`
/// belongs produces a query that RUNS and counts the wrong set, which is the
/// failure this whole file exists to prevent. A body with no keyword at all
/// produces `FROM src.authorized_keysfingerprint IN (...)`, which is at least
/// a syntax error somebody would notice.
fn device_predicate(selection: RowSelection, alias: Option<&str>, joiner: &str) -> String {
    if selection == RowSelection::Every {
        return String::new();
    }
    let column = match alias {
        Some(alias) => format!("{alias}.fingerprint"),
        None => String::from("fingerprint"),
    };
    format!("{joiner}{column} IN (SELECT fingerprint FROM {SOURCE_SCHEMA}.account_devices)")
}

/// The `INSERT … SELECT` that copies one table from the attached source.
///
/// `OR IGNORE` rather than a bare `INSERT` on both paths, because a first run
/// into an empty target has nothing to ignore and a re-run must not fail on
/// the rows it already has.
///
/// The column list is the TARGET's, read from its own schema, and that is the
/// point of the call. Selecting the source's own columns would make a schema
/// drift invisible in the direction that loses data: a column v3 added would
/// simply be absent from the list, and the row would be written without it.
/// Naming v3's columns and asking the source for them inverts that — a column
/// the source lacks is a query that fails, out loud, inside a transaction that
/// rolls back, rather than a row that quietly loses a field.
#[must_use]
pub fn copy_sql(table: &str, columns: &[String], selection: RowSelection) -> String {
    format!(
        "INSERT OR IGNORE INTO main.{table} ({columns}) SELECT {columns} FROM \
         {SOURCE_SCHEMA}.{table}{device_predicate}",
        columns = columns.join(", "),
        device_predicate = device_predicate(selection, None, " WHERE "),
    )
}

/// How many rows of the source this table takes, for the report.
#[must_use]
pub fn source_count_sql(table: &str, selection: RowSelection) -> String {
    format!(
        "SELECT count(*) FROM {SOURCE_SCHEMA}.{table}{device_predicate}",
        device_predicate = device_predicate(selection, None, " WHERE "),
    )
}

/// How many of those rows the target already has, for the report.
///
/// `IS` rather than `=` on every key, because `app_settings.dashboard_id` is
/// nullable and `NULL = NULL` is not true in SQL: the dashboard-less setting
/// is a real row, and an `=` comparison would report it as absent forever and
/// re-insert it on every run.
#[must_use]
pub fn already_present_sql(table: &str, selection: RowSelection) -> String {
    let keys = key_columns(table)
        .iter()
        .map(|key| format!("t.{key} IS s.{key}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "SELECT count(*) FROM {SOURCE_SCHEMA}.{table} s WHERE EXISTS \
         (SELECT 1 FROM main.{table} t WHERE {keys}){device_predicate}",
        device_predicate = device_predicate(selection, Some("s"), " AND "),
    )
}

/// The account ids a database holds, for [`decide_mode`].
#[must_use]
pub fn accounts_sql() -> &'static str {
    "SELECT id FROM main.accounts"
}

/// The single account a v2 database holds, for the import to carry across.
#[must_use]
pub fn source_accounts_sql() -> &'static str {
    "SELECT id FROM src.accounts"
}

/// The deletes a revocation sweep performs, once the revocation rows are in.
///
/// Two statements, devices before keys: a device row references its key row,
/// so deleting the key first would fail the foreign key on the device and
/// remove neither.
#[must_use]
pub fn revocation_deletes() -> &'static [&'static str] {
    &[
        "DELETE FROM main.account_devices WHERE fingerprint IN \
         (SELECT fingerprint FROM main.authorized_key_revocations)",
        "DELETE FROM main.authorized_keys WHERE fingerprint IN \
         (SELECT fingerprint FROM main.authorized_key_revocations)",
    ]
}

/// The statement that names a table's columns, for reading the TARGET's schema.
///
/// The target, deliberately. The plan states that any column v3 has and the
/// source lacks must fail loudly, and that only works if the column list comes
/// from the side being written.
///
/// The schema is a PREFIX here rather than part of the argument: SQLite's
/// table-valued pragma takes the schema as the qualifier, and passing
/// `'main.accounts'` as the table name matches nothing and returns an EMPTY
/// column list — which then produces an `INSERT ... ()` that is a syntax error
/// far from its cause.
#[must_use]
pub fn columns_sql(table: &str) -> String {
    format!("SELECT name FROM main.pragma_table_info('{table}') ORDER BY cid")
}

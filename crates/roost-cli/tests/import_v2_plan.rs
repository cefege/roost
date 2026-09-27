//! The decisions `roost import-v2` makes before it touches a database: which
//! tables, in which order, with which filter, and what a re-run does about
//! each. Every one of them is a question with a wrong answer that only shows
//! up on a production cutover, so they are decided here with no database at
//! all.
//!
//! The behaviour that needs a real SQLite — that the filter really excludes
//! the machine keys, that a re-run really applies a new revocation, that a dry
//! run really writes nothing — is in `tests/import_v2_copy.rs`, against a
//! fixture built from the coordinator's own migration. This file is the part
//! that is a list, an order and a mode, and those are all readable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::import_v2::plan::{
    ImportMode, ModeRefusal, Refresh, RowSelection, SOURCE_SCHEMA, TABLES, accounts_sql,
    already_present_sql, columns_sql, copy_sql, decide_mode, key_columns, revocation_deletes,
    source_accounts_sql, source_count_sql,
};

const COLUMNS: &[String] = &[String::new()];

fn position(table: &str) -> usize {
    TABLES
        .iter()
        .position(|spec| spec.table == table)
        .unwrap_or_else(|| panic!("{table} must be in the import list"))
}

#[test]
fn an_empty_target_is_a_first_run_and_a_matching_account_is_a_refresh() {
    assert_eq!(
        decide_mode(&[], "acct-1").expect("an empty target is importable"),
        ImportMode::FirstRun
    );
    assert_eq!(
        decide_mode(&["acct-1".to_string()], "acct-1").expect("the same install is importable"),
        ImportMode::Refresh
    );
}

#[test]
fn a_target_holding_another_install_is_refused_by_name() {
    let refusal =
        decide_mode(&["acct-2".to_string()], "acct-1").expect_err("two installs are not one");
    assert_eq!(
        refusal,
        ModeRefusal::DifferentInstall {
            installed: "acct-2".to_string(),
            imported: "acct-1".to_string(),
        },
        "the refusal names both accounts: importing acct-1 into a database that already holds \
         acct-2 would rewrite the identity every other row refers to"
    );
}

#[test]
fn a_target_that_is_not_a_v3_install_is_refused_rather_than_merged_into() {
    let refusal =
        decide_mode(&["a".to_string(), "b".to_string()], "a").expect_err("not self-hosted");
    assert_eq!(refusal, ModeRefusal::NotSelfHosted { accounts: 2 });
}

/// The order is the property. A dependency copied after the row that needs it
/// is a constraint failure inside the transaction that owns the whole identity
/// topology, and the failure names a foreign key rather than the ordering that
/// caused it.
#[test]
fn every_copied_table_is_copied_after_everything_it_references() {
    for (table, needs) in [
        ("account_identities", "accounts"),
        ("organization_memberships", "accounts"),
        ("organization_memberships", "organizations"),
        ("dashboards", "organizations"),
        ("dashboard_memberships", "dashboards"),
        ("account_devices", "authorized_keys"),
        ("app_settings", "dashboards"),
    ] {
        assert!(
            position(needs) < position(table),
            "{table} is copied before the {needs} row it references, so the insert cannot satisfy \
             its foreign key"
        );
    }
    assert!(
        position("authorized_keys") < position("authorized_key_revocations"),
        "a revocation in place would make the coordinator's own trigger refuse the keys"
    );
}

/// The five machine keys are the whole reason the filter exists, and it is the
/// one thing a reviewer must be able to check by reading the list.
#[test]
fn only_the_keys_table_filters_and_every_statement_about_it_applies_the_filter() {
    for spec in TABLES {
        let expected = if spec.table == "authorized_keys" {
            RowSelection::OnAccountDevice
        } else {
            RowSelection::Every
        };
        assert_eq!(
            spec.selection, expected,
            "{}: a filter nobody expected is a scope nobody reviewed",
            spec.table
        );
    }
    for sql in [
        copy_sql("authorized_keys", COLUMNS, RowSelection::OnAccountDevice),
        source_count_sql("authorized_keys", RowSelection::OnAccountDevice),
        already_present_sql("authorized_keys", RowSelection::OnAccountDevice),
    ] {
        assert!(
            sql.contains(&format!(
                "fingerprint IN (SELECT fingerprint FROM {SOURCE_SCHEMA}.account_devices)"
            )),
            "a statement that counts without filtering reports a total the copy does not produce, \
             so the machine keys would be reported as copied when they were not: {sql}"
        );
    }
}

/// The aliased and unaliased spellings must name the same predicate, or the
/// "already present" count is measuring a different set from the copy.
#[test]
fn the_filter_is_the_same_predicate_aliased_and_unaliased() {
    let aliased = already_present_sql("authorized_keys", RowSelection::OnAccountDevice);
    assert!(
        aliased.contains("s.fingerprint IN (SELECT fingerprint FROM src.account_devices)"),
        "{aliased}"
    );
    let unaliased = source_count_sql("authorized_keys", RowSelection::OnAccountDevice);
    assert!(
        unaliased.ends_with("WHERE fingerprint IN (SELECT fingerprint FROM src.account_devices)"),
        "the unaliased form must not carry the alias into a query that has none: {unaliased}"
    );
}

#[test]
fn a_re_run_edits_nothing_v3_owns_and_applies_every_revocation() {
    for spec in TABLES {
        let expected = if spec.table == "authorized_key_revocations" {
            Refresh::ApplyRevocations
        } else {
            Refresh::KeepExisting
        };
        assert_eq!(spec.refresh, expected, "{}: refresh policy", spec.table);
    }
    let deletes = revocation_deletes();
    assert_eq!(deletes.len(), 2, "a device row and the key row it names");
    assert!(
        deletes[0].contains("account_devices") && deletes[1].contains("authorized_keys"),
        "devices before keys: a device row references its key, so deleting the key first fails \
         the foreign key and removes neither"
    );
}

/// Three tables are keyed on something other than `id` and three have no `id`
/// column at all, so a key assumed to be `id` is wrong in a way only a query
/// would catch.
#[test]
fn every_table_is_matched_on_its_real_primary_key() {
    assert_eq!(key_columns("accounts"), &["id"]);
    assert_eq!(
        key_columns("app_settings"),
        &["dashboard_id", "key"],
        "app_settings is composite, so counting it on one column reports a wrong total"
    );
    for keyed_on_fingerprint in [
        "authorized_keys",
        "account_devices",
        "authorized_key_revocations",
    ] {
        assert_eq!(
            key_columns(keyed_on_fingerprint),
            &["fingerprint"],
            "{keyed_on_fingerprint} is keyed on fingerprint and has no id column at all"
        );
    }
    assert!(
        already_present_sql("app_settings", RowSelection::Every)
            .contains("t.dashboard_id IS s.dashboard_id AND t.key IS s.key"),
        "IS rather than =, because app_settings.dashboard_id is nullable and NULL = NULL is not \
         true: a dashboard-less setting would be re-inserted on every run"
    );
}

/// The column list has to come from the side being WRITTEN, or a column v3
/// added would be dropped in the one direction that loses data.
#[test]
fn the_column_list_is_read_from_the_target_and_the_copy_names_both_schemas() {
    let columns = columns_sql("app_settings");
    assert!(
        columns.contains("main.pragma_table_info('app_settings')"),
        "the schema is a PREFIX, not part of the argument: passing 'main.app_settings' as the \
         table name matches nothing and yields an empty column list, which then produces an \
         `INSERT ... ()` that is a syntax error far from its cause: {columns}"
    );
    assert!(
        !columns.contains("src."),
        "the source is never the schema authority: {columns}"
    );
    let copy = copy_sql(
        "accounts",
        &["id".to_string(), "status".to_string()],
        RowSelection::Every,
    );
    assert!(
        copy.starts_with(
            "INSERT OR IGNORE INTO main.accounts (id, status) SELECT id, status FROM src.accounts"
        ),
        "{copy}"
    );
}

/// A statement that named neither schema would be a statement about whichever
/// database happened to be attached, which is how a copy ends up reading its
/// own output.
#[test]
fn every_statement_names_the_database_it_means() {
    assert_eq!(accounts_sql(), "SELECT id FROM main.accounts");
    assert_eq!(source_accounts_sql(), "SELECT id FROM src.accounts");
    for sql in [
        source_count_sql("dashboards", RowSelection::Every),
        already_present_sql("dashboards", RowSelection::Every),
    ] {
        assert!(sql.contains(SOURCE_SCHEMA), "{sql}");
    }
    for sql in revocation_deletes() {
        assert!(sql.contains("main."), "{sql}");
    }
}

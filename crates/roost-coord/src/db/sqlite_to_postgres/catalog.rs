//! What a copy from SQLite to Postgres moves: the target's tables in an order
//! every foreign key accepts, each table's columns and their Postgres types,
//! and which columns are identities whose sequences must follow the copied ids.
//!
//! Read from the Postgres catalog after `db::open` migrated it, and checked
//! against the SQLite file's own schema, so a drift between the two migration
//! sets is a refusal before any row moves. Called by `sqlite_to_postgres`.

use std::collections::{BTreeMap, HashSet};

use sqlx::Row as _;
use sqlx::postgres::PgConnection;

use super::TransferError;
use crate::db::CoordDb;

/// The Postgres column types the coordinator schema uses, and so the only ones
/// a copy knows how to carry. A new type in a migration is a refusal naming it,
/// not a guess at its encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// `BIGINT`: every SQLite integer.
    BigInt,
    /// `TEXT`.
    Text,
    /// `BYTEA`: every SQLite blob.
    Bytea,
}

impl ColumnKind {
    fn from_postgres(data_type: &str) -> Option<Self> {
        match data_type {
            "bigint" => Some(Self::BigInt),
            "text" => Some(Self::Text),
            "bytea" => Some(Self::Bytea),
            _ => None,
        }
    }

    /// The array type an `UNNEST` parameter of this column binds as.
    #[must_use]
    pub fn array_cast(self) -> &'static str {
        match self {
            Self::BigInt => "bigint[]",
            Self::Text => "text[]",
            Self::Bytea => "bytea[]",
        }
    }
}

/// One column of one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    /// The column name, unquoted.
    pub name: String,
    /// Its Postgres type.
    pub kind: ColumnKind,
    /// Whether it is `GENERATED … AS IDENTITY`, whose sequence must be moved
    /// past the largest copied value.
    pub identity: bool,
}

/// One table to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlan {
    /// The table name, unquoted.
    pub name: String,
    /// Its columns, in the target's ordinal order.
    pub columns: Vec<TableColumn>,
}

/// The migration bookkeeping table each side keeps for itself.
const MIGRATIONS_TABLE: &str = "_sqlx_migrations";

/// Every table the target holds, parents before children.
pub async fn read_target_plan(target: &mut PgConnection) -> Result<Vec<TablePlan>, TransferError> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind = 'r' AND n.nspname = current_schema() AND c.relname <> $1 \
         ORDER BY c.oid",
    )
    .bind(MIGRATIONS_TABLE)
    .fetch_all(&mut *target)
    .await?;
    let edges: Vec<(String, String)> = sqlx::query_as(
        "SELECT child.relname::text, parent.relname::text FROM pg_constraint k \
         JOIN pg_class child ON child.oid = k.conrelid \
         JOIN pg_class parent ON parent.oid = k.confrelid \
         JOIN pg_namespace n ON n.oid = child.relnamespace \
         WHERE k.contype = 'f' AND n.nspname = current_schema()",
    )
    .fetch_all(&mut *target)
    .await?;
    let column_rows = sqlx::query(
        "SELECT table_name::text, column_name::text, data_type::text, is_identity::text \
         FROM information_schema.columns WHERE table_schema = current_schema() \
         ORDER BY table_name, ordinal_position",
    )
    .fetch_all(&mut *target)
    .await?;

    let mut columns: BTreeMap<String, Vec<TableColumn>> = BTreeMap::new();
    for row in column_rows {
        let table: String = row.try_get(0)?;
        let name: String = row.try_get(1)?;
        let data_type: String = row.try_get(2)?;
        let identity: String = row.try_get(3)?;
        let Some(kind) = ColumnKind::from_postgres(&data_type) else {
            if table == MIGRATIONS_TABLE {
                continue;
            }
            return Err(TransferError::SchemaMismatch(format!(
                "{table}.{name} is {data_type}, a type this copy does not carry"
            )));
        };
        columns.entry(table).or_default().push(TableColumn {
            name,
            kind,
            identity: identity == "YES",
        });
    }
    let ordered = order_parents_first(&names, &edges)?;
    Ok(ordered
        .into_iter()
        .map(|name| TablePlan {
            columns: columns.remove(&name).unwrap_or_default(),
            name,
        })
        .collect())
}

/// `tables` reordered so every foreign key's parent precedes its child; ties
/// keep the given (creation) order, so the copy is deterministic.
///
/// A self-reference is ignored: one multi-row insert checks it at statement
/// end. A cycle across tables is refused, because no insert order satisfies it.
pub fn order_parents_first(
    tables: &[String],
    edges: &[(String, String)],
) -> Result<Vec<String>, TransferError> {
    let mut emitted: HashSet<&str> = HashSet::new();
    let mut ordered = Vec::with_capacity(tables.len());
    while ordered.len() < tables.len() {
        let next = tables.iter().find(|table| {
            !emitted.contains(table.as_str())
                && edges.iter().all(|(child, parent)| {
                    child != *table
                        || parent == *table
                        || emitted.contains(parent.as_str())
                        || !tables.contains(parent)
                })
        });
        let Some(table) = next else {
            let stuck: Vec<&str> = tables
                .iter()
                .map(String::as_str)
                .filter(|table| !emitted.contains(table))
                .collect();
            return Err(TransferError::SchemaMismatch(format!(
                "foreign keys form a cycle among {}",
                stuck.join(", ")
            )));
        };
        emitted.insert(table.as_str());
        ordered.push(table.clone());
    }
    Ok(ordered)
}

/// Refuse a source whose tables or columns differ from the target's: both
/// sides were migrated by this build, so a difference is a build whose two
/// migration sets disagree, and copying across it would drop data silently.
pub async fn verify_source_matches(
    source: &CoordDb,
    plan: &[TablePlan],
) -> Result<(), TransferError> {
    let source_tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND name <> $1 ORDER BY name",
    )
    .bind(MIGRATIONS_TABLE)
    .fetch_all(source.pool())
    .await?;
    let target_tables: HashSet<&str> = plan.iter().map(|table| table.name.as_str()).collect();
    let source_set: HashSet<&str> = source_tables.iter().map(String::as_str).collect();
    if source_set != target_tables {
        let mut only_source: Vec<&str> = source_set.difference(&target_tables).copied().collect();
        let mut only_target: Vec<&str> = target_tables.difference(&source_set).copied().collect();
        only_source.sort_unstable();
        only_target.sort_unstable();
        return Err(TransferError::SchemaMismatch(format!(
            "tables only in SQLite: [{}]; only in Postgres: [{}]",
            only_source.join(", "),
            only_target.join(", ")
        )));
    }
    for table in plan {
        let source_columns: HashSet<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info($1)")
                .bind(&table.name)
                .fetch_all(source.pool())
                .await?
                .into_iter()
                .collect();
        let target_columns: HashSet<String> = table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect();
        if source_columns != target_columns {
            let mut differing: Vec<&String> = source_columns
                .symmetric_difference(&target_columns)
                .collect();
            differing.sort_unstable();
            return Err(TransferError::SchemaMismatch(format!(
                "{} columns differ between SQLite and Postgres: {differing:?}",
                table.name
            )));
        }
    }
    Ok(())
}

/// `name` as a double-quoted SQL identifier.
#[must_use]
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::{order_parents_first, quote_identifier};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn edge(child: &str, parent: &str) -> (String, String) {
        (child.to_owned(), parent.to_owned())
    }

    #[test]
    fn a_child_created_before_its_parent_is_copied_after_it() {
        // `workers` references `dashboards`, which the migration creates later.
        let ordered = order_parents_first(
            &names(&["workers", "sessions", "dashboards", "accounts"]),
            &[
                edge("workers", "dashboards"),
                edge("sessions", "workers"),
                edge("dashboards", "accounts"),
            ],
        )
        .expect("an acyclic schema orders");
        assert_eq!(
            ordered,
            names(&["accounts", "dashboards", "workers", "sessions"])
        );
    }

    #[test]
    fn unrelated_tables_keep_creation_order() {
        let ordered = order_parents_first(&names(&["b", "a", "c"]), &[]).expect("orders");
        assert_eq!(ordered, names(&["b", "a", "c"]));
    }

    #[test]
    fn a_self_reference_does_not_block_its_table() {
        let ordered =
            order_parents_first(&names(&["tree"]), &[edge("tree", "tree")]).expect("orders");
        assert_eq!(ordered, names(&["tree"]));
    }

    #[test]
    fn a_cycle_is_refused_by_name() {
        let error = order_parents_first(
            &names(&["a", "b", "free"]),
            &[edge("a", "b"), edge("b", "a")],
        )
        .expect_err("a cycle has no insert order");
        let message = error.to_string();
        assert!(message.contains("a, b"), "{message}");
        assert!(!message.contains("free"), "{message}");
    }

    #[test]
    fn identifiers_are_quoted_with_embedded_quotes_doubled() {
        assert_eq!(quote_identifier("workspaces"), "\"workspaces\"");
        assert_eq!(quote_identifier("we\"ird"), "\"we\"\"ird\"");
    }
}

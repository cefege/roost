//! One table's rows, column by column, and the single `INSERT … SELECT FROM
//! UNNEST(…)` that lands a whole batch of them in Postgres in one round trip.
//!
//! Called by `sqlite_to_postgres` for every table in the plan; the column
//! types come from `catalog`. Values are decoded from SQLite by the TARGET's
//! type, so a value stored under another storage class is a refusal naming
//! its table and column rather than a silent coercion.

use sqlx::any::AnyRow;
use sqlx::postgres::PgConnection;
use sqlx::{AssertSqlSafe, Postgres, Row as _};

use super::TransferError;
use super::catalog::{ColumnKind, TablePlan, quote_identifier};

/// One column's values for the current batch, typed for its array parameter.
#[derive(Debug)]
enum ColumnValues {
    BigInt(Vec<Option<i64>>),
    Boolean(Vec<Option<bool>>),
    Text(Vec<Option<String>>),
    Bytea(Vec<Option<Vec<u8>>>),
}

impl ColumnValues {
    fn empty(kind: ColumnKind, capacity: usize) -> Self {
        match kind {
            ColumnKind::BigInt => Self::BigInt(Vec::with_capacity(capacity)),
            ColumnKind::Boolean => Self::Boolean(Vec::with_capacity(capacity)),
            ColumnKind::Text => Self::Text(Vec::with_capacity(capacity)),
            ColumnKind::Bytea => Self::Bytea(Vec::with_capacity(capacity)),
        }
    }

    fn push_from(&mut self, row: &AnyRow, index: usize) -> Result<(), sqlx::Error> {
        match self {
            Self::BigInt(values) => values.push(row.try_get(index)?),
            Self::Boolean(values) => {
                let value: Option<i64> = row.try_get(index)?;
                values.push(
                    value
                        .map(|integer| match integer {
                            0 => Ok(false),
                            1 => Ok(true),
                            _ => Err(sqlx::Error::Decode(Box::new(std::io::Error::other(
                                "SQLite boolean is not 0 or 1",
                            )))),
                        })
                        .transpose()?,
                );
            }
            Self::Text(values) => values.push(row.try_get(index)?),
            Self::Bytea(values) => values.push(row.try_get(index)?),
        }
        Ok(())
    }

    fn clear(&mut self) {
        match self {
            Self::BigInt(values) => values.clear(),
            Self::Boolean(values) => values.clear(),
            Self::Text(values) => values.clear(),
            Self::Bytea(values) => values.clear(),
        }
    }
}

/// A table's batch under construction, and the statement that flushes it.
#[derive(Debug)]
pub struct TableBatch<'plan> {
    table: &'plan TablePlan,
    insert: String,
    columns: Vec<ColumnValues>,
    rows: usize,
}

impl<'plan> TableBatch<'plan> {
    /// An empty batch for `table`, holding up to `capacity` rows.
    #[must_use]
    pub fn new(table: &'plan TablePlan, capacity: usize) -> Self {
        Self {
            table,
            insert: unnest_insert(table),
            columns: table
                .columns
                .iter()
                .map(|column| ColumnValues::empty(column.kind, capacity))
                .collect(),
            rows: 0,
        }
    }

    /// Rows waiting to be flushed.
    #[must_use]
    pub fn pending_rows(&self) -> usize {
        self.rows
    }

    /// Add one source row, read in the plan's column order.
    pub fn push(&mut self, row: &AnyRow) -> Result<(), TransferError> {
        for (index, (values, column)) in
            self.columns.iter_mut().zip(&self.table.columns).enumerate()
        {
            values
                .push_from(row, index)
                .map_err(|error| TransferError::Value {
                    table: self.table.name.clone(),
                    column: column.name.clone(),
                    reason: error.to_string(),
                })?;
        }
        self.rows += 1;
        Ok(())
    }

    /// Insert every waiting row in one statement and start the next batch.
    pub async fn flush(&mut self, target: &mut PgConnection) -> Result<u64, TransferError> {
        if self.rows == 0 {
            return Ok(0);
        }
        let mut statement = sqlx::query::<Postgres>(AssertSqlSafe(self.insert.clone()));
        for values in &self.columns {
            statement = match values {
                ColumnValues::BigInt(values) => statement.bind(values),
                ColumnValues::Boolean(values) => statement.bind(values),
                ColumnValues::Text(values) => statement.bind(values),
                ColumnValues::Bytea(values) => statement.bind(values),
            };
        }
        let inserted = statement.execute(&mut *target).await?.rows_affected();
        for values in &mut self.columns {
            values.clear();
        }
        self.rows = 0;
        Ok(inserted)
    }
}

/// `SELECT` of the plan's columns from the SQLite table, in plan order.
#[must_use]
pub fn source_select(table: &TablePlan) -> String {
    format!(
        "SELECT {} FROM {}",
        column_list(table),
        quote_identifier(&table.name)
    )
}

/// `INSERT INTO t (a, b) SELECT * FROM UNNEST($1::bigint[], $2::text[])`.
fn unnest_insert(table: &TablePlan) -> String {
    let arrays: Vec<String> = table
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("${}::{}", index + 1, column.kind.array_cast()))
        .collect();
    format!(
        "INSERT INTO {} ({}) SELECT * FROM UNNEST({})",
        quote_identifier(&table.name),
        column_list(table),
        arrays.join(", ")
    )
}

fn column_list(table: &TablePlan) -> String {
    table
        .columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ")
}

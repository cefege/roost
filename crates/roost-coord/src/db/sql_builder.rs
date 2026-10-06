//! Building one statement whose shape depends on the request — an `IN (...)`
//! list, an optional column, a multi-row `VALUES` — with `$n` placeholders.
//!
//! Owned by `crate::db`; every dynamically shaped statement in this crate is
//! built here. It exists because `sqlx::QueryBuilder<Any>` renders each bound
//! value as `?`, which SQLite accepts and Postgres refuses as a syntax error;
//! `$n` is the one spelling both backends parse. The surface mirrors the
//! subset of `QueryBuilder` this crate used, so a call site reads the same.

use std::fmt::{Display, Write as _};

use sqlx::any::{AnyArguments, AnyRow};
use sqlx::query::{Query, QueryAs, QueryScalar};
use sqlx::{Any, Arguments as _, AssertSqlSafe, Encode, FromRow, Type};

/// A statement under construction: its text and the values bound so far.
#[derive(Default)]
pub struct SqlBuilder {
    sql: String,
    arguments: AnyArguments,
    placeholders: usize,
}

/// The text and the placeholder count; bound values are a caller's data and
/// stay out of a log line.
impl std::fmt::Debug for SqlBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqlBuilder")
            .field("sql", &self.sql)
            .field("placeholders", &self.placeholders)
            .finish_non_exhaustive()
    }
}

impl SqlBuilder {
    /// A statement beginning with `sql`.
    #[must_use]
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            ..Self::default()
        }
    }

    /// Append literal SQL. Never a caller's value: those go through
    /// [`Self::push_bind`].
    pub fn push(&mut self, sql: impl Display) -> &mut Self {
        let _ = write!(self.sql, "{sql}");
        self
    }

    /// Bind `value` and append its `$n` placeholder.
    ///
    /// The placeholder is written even if the value fails to encode, so the
    /// statement then carries one fewer value than placeholders and the
    /// database refuses it when it runs — the failure surfaces at the same
    /// call as any other statement error. (The `Any` encoders for the integer,
    /// text and byte types this crate binds cannot fail.)
    pub fn push_bind<'value, T>(&mut self, value: T) -> &mut Self
    where
        T: Encode<'value, Any> + Type<Any>,
    {
        let _ = self.arguments.add(value);
        self.placeholders += 1;
        let _ = write!(self.sql, "${}", self.placeholders);
        self
    }

    /// Push items with `separator` between them; see [`Separated`].
    pub fn separated(&mut self, separator: &'static str) -> Separated<'_> {
        Separated {
            builder: self,
            separator,
            first: true,
        }
    }

    /// Append `VALUES (…), (…)` with one parenthesized row per item, each
    /// row's columns pushed by `push_row` through a comma-separated
    /// [`Separated`].
    pub fn push_values<I, F>(&mut self, rows: I, mut push_row: F) -> &mut Self
    where
        I: IntoIterator,
        F: FnMut(Separated<'_>, I::Item),
    {
        self.push("VALUES ");
        for (index, item) in rows.into_iter().enumerate() {
            if index > 0 {
                self.push(", ");
            }
            self.push("(");
            push_row(self.separated(", "), item);
            self.push(")");
        }
        self
    }

    /// The statement, to execute.
    pub fn build(self) -> Query<'static, Any, AnyArguments> {
        sqlx::query_with(AssertSqlSafe(self.sql), self.arguments)
    }

    /// The statement, decoding each row as `Row`.
    pub fn build_query_as<Row>(self) -> QueryAs<'static, Any, Row, AnyArguments>
    where
        Row: for<'row> FromRow<'row, AnyRow>,
    {
        sqlx::query_as_with(AssertSqlSafe(self.sql), self.arguments)
    }

    /// The statement, decoding each row's first column as `Value`.
    pub fn build_query_scalar<Value>(self) -> QueryScalar<'static, Any, Value, AnyArguments>
    where
        (Value,): for<'row> FromRow<'row, AnyRow>,
    {
        sqlx::query_scalar_with(AssertSqlSafe(self.sql), self.arguments)
    }
}

/// Items pushed with a separator between them: the separator goes before every
/// item but the first. The `_unseparated` variants continue the current item.
#[derive(Debug)]
pub struct Separated<'builder> {
    builder: &'builder mut SqlBuilder,
    separator: &'static str,
    first: bool,
}

impl Separated<'_> {
    /// Start a new item with literal SQL.
    pub fn push(&mut self, sql: impl Display) -> &mut Self {
        self.separate();
        self.builder.push(sql);
        self
    }

    /// Continue the current item with literal SQL.
    pub fn push_unseparated(&mut self, sql: impl Display) -> &mut Self {
        self.builder.push(sql);
        self
    }

    /// Start a new item with a bound value.
    pub fn push_bind<'value, T>(&mut self, value: T) -> &mut Self
    where
        T: Encode<'value, Any> + Type<Any>,
    {
        self.separate();
        self.builder.push_bind(value);
        self
    }

    /// Continue the current item with a bound value.
    pub fn push_bind_unseparated<'value, T>(&mut self, value: T) -> &mut Self
    where
        T: Encode<'value, Any> + Type<Any>,
    {
        self.builder.push_bind(value);
        self
    }

    fn separate(&mut self) {
        if !self.first {
            self.builder.push(self.separator);
        }
        self.first = false;
    }
}

#[cfg(test)]
mod tests {
    use super::SqlBuilder;

    #[test]
    fn every_bound_value_is_a_numbered_placeholder() {
        let mut statement = SqlBuilder::new("UPDATE t SET ");
        {
            let mut columns = statement.separated(", ");
            columns
                .push("a")
                .push_unseparated(" = ")
                .push_bind_unseparated(1_i64);
            columns
                .push("b")
                .push_unseparated(" = ")
                .push_bind_unseparated("x");
        }
        statement.push(" WHERE id IN (");
        {
            let mut ids = statement.separated(", ");
            for id in ["p", "q"] {
                ids.push_bind(id);
            }
        }
        statement.push(")");
        assert_eq!(
            statement.sql,
            "UPDATE t SET a = $1, b = $2 WHERE id IN ($3, $4)"
        );
    }

    #[test]
    fn values_rows_are_parenthesized_and_comma_joined() {
        let mut statement = SqlBuilder::new("INSERT INTO t (a, b) ");
        statement.push_values([(1_i64, "x"), (2_i64, "y")], |mut row, (a, b)| {
            row.push_bind(a).push_bind(b);
        });
        assert_eq!(
            statement.sql,
            "INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4)"
        );
    }
}

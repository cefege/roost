//! Binding a list of values into `IN (...)`, the one portable spelling of "any
//! of these ids" on both backends.
//!
//! Owned by `crate::db`; every statement that filters on a caller-sized id list
//! builds it here. SQLite's `json_each` has no Postgres twin, and Postgres'
//! `= ANY($1)` needs an array type the `Any` driver cannot bind, so the list is
//! one placeholder per value — chunked, because each backend caps a statement's
//! placeholders (SQLite 32766, Postgres 65535).

use sqlx::{Any, Encode, Type};

use super::SqlBuilder;

/// The most values one `IN (...)` list binds. A caller with more runs one
/// statement per chunk; an empty list runs none.
pub const IN_LIST_CHUNK: usize = 500;

/// Append `(<bind>, <bind>, …)` for `values` to `builder`.
///
/// The caller owns the chunking (`values.chunks(IN_LIST_CHUNK)`) and the empty
/// case: `IN ()` is a syntax error on Postgres.
pub fn push_in_list<'value, T>(builder: &mut SqlBuilder, values: &'value [T])
where
    &'value T: Encode<'value, Any> + Type<Any>,
{
    builder.push("(");
    let mut separated = builder.separated(", ");
    for value in values {
        separated.push_bind(value);
    }
    builder.push(")");
}

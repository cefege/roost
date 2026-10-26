//! Scan order/cursor resolution (port of `src/storage/scan.ts` @ `abe508e1b`).

use crate::errors::{StorageError, StorageResult};
use crate::types::{Cursor, ScanOrder};

/// Where a built-in storage scan starts: its order and the last ID a previous
/// page returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStart {
    /// Effective order.
    pub order: ScanOrder,
    /// Last ID a previous page returned.
    pub after: Option<u64>,
}

/// Resolve a scan's order and position. A cursor continues in the order it was
/// created with, whether the query repeats that order or omits it; a different
/// `order` throws. A cursor without an order continues in the scan's default
/// order.
pub fn scan_start(
    requested: Option<ScanOrder>,
    cursor: Option<&Cursor>,
    fallback: ScanOrder,
) -> StorageResult<ScanStart> {
    let Some(cursor) = cursor else {
        return Ok(ScanStart {
            order: requested.unwrap_or(fallback),
            after: None,
        });
    };
    if cursor.after > crate::storage::MAX_SAFE_INTEGER {
        return Err(StorageError::InvalidCursor);
    }
    let order = cursor.order.unwrap_or(fallback);
    if let Some(requested) = requested
        && requested != order
    {
        return Err(StorageError::CursorOrder {
            stored: scan_order_text(order).to_string(),
            requested: scan_order_text(requested).to_string(),
        });
    }
    Ok(ScanStart {
        order,
        after: Some(cursor.after),
    })
}

fn scan_order_text(order: ScanOrder) -> &'static str {
    match order {
        ScanOrder::Ascending => "ascending",
        ScanOrder::Descending => "descending",
    }
}

/// Indexes of sorted `ids` in scan order, after the cursor's ID when there is one.
pub(crate) fn scan_indexes<T: Copy>(
    ids: &[T],
    start: ScanStart,
    id_of: impl Fn(T) -> u64,
) -> Vec<usize> {
    match start.order {
        ScanOrder::Ascending => {
            let first = match start.after {
                None => 0,
                Some(after) => ids.partition_point(|id| id_of(*id) <= after),
            };
            (first..ids.len()).collect()
        }
        ScanOrder::Descending => {
            let end = match start.after {
                None => ids.len(),
                Some(after) => ids.partition_point(|id| id_of(*id) < after),
            };
            (0..end).rev().collect()
        }
    }
}

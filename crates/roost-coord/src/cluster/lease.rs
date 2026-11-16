//! The `coord_leases` row shape and the epoch/expiry arithmetic every lease
//! holder shares: the cluster link owner and the login poller.

/// One row of `coord_leases`.
#[derive(Debug)]
pub struct LeaseRow {
    pub owner: String,
    pub epoch: u64,
    pub expires_ms: i64,
}

/// The epoch `owner` takes the lease at, or `None` while another owner's
/// lease is live. The same owner re-acquiring bumps the epoch, so whatever an
/// earlier incarnation of it still has in flight is fenced off.
#[must_use]
pub fn next_lease_epoch(current: Option<&LeaseRow>, owner: &str, now_ms: i64) -> Option<u64> {
    match current {
        None => Some(1),
        Some(row) if row.owner == owner || row.expires_ms <= now_ms => {
            Some(row.epoch.saturating_add(1))
        }
        Some(_) => None,
    }
}

/// The expiry a lease taken or renewed at `now_ms` carries.
#[must_use]
pub fn lease_expiry(now_ms: i64, ttl_ms: u64) -> i64 {
    now_ms.saturating_add(i64::try_from(ttl_ms).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::{LeaseRow, next_lease_epoch};

    fn row(owner: &str, epoch: u64, expires_ms: i64) -> LeaseRow {
        LeaseRow {
            owner: owner.into(),
            epoch,
            expires_ms,
        }
    }

    #[test]
    fn a_live_foreign_lease_is_refused_and_an_expired_one_is_taken_at_the_next_epoch() {
        assert_eq!(next_lease_epoch(None, "a", 10), Some(1));
        assert_eq!(next_lease_epoch(Some(&row("b", 4, 100)), "a", 10), None);
        assert_eq!(next_lease_epoch(Some(&row("b", 4, 5)), "a", 10), Some(5));
    }

    #[test]
    fn the_same_owner_reacquiring_bumps_the_epoch() {
        assert_eq!(next_lease_epoch(Some(&row("a", 7, 100)), "a", 10), Some(8));
    }
}

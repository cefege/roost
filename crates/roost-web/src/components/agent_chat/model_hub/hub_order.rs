//! Row order of the models hub, ported from OMP `sortModelItems`: a role the
//! model leads ranks first (skipped while searching), then most recently used,
//! then provider name, priority (lower first), version (higher first), and the
//! date suffix or `-latest` marker, falling back to id. Pure over plain keys so
//! the list component and its tests share one ordering.

use std::cmp::Ordering;

/// The fields the ordering reads from one model row.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderKey<'a> {
    pub selector: &'a str,
    pub provider: &'a str,
    pub id: &'a str,
    /// Index of the first role this model leads, in role order; `None` when no role does.
    pub role_rank: Option<usize>,
    /// Position in the recently used list; `None` when never used here.
    pub mru_index: Option<usize>,
    pub priority: Option<i32>,
}

/// Sorts `keys` in place. `skip_role_rank` is set while searching, so the
/// match quality decides instead of role assignments.
pub fn sort_order_keys(keys: &mut [OrderKey<'_>], skip_role_rank: bool) {
    keys.sort_by(|left, right| compare_keys(left, right, skip_role_rank));
}

fn compare_keys(left: &OrderKey<'_>, right: &OrderKey<'_>, skip_role_rank: bool) -> Ordering {
    if !skip_role_rank {
        let by_role = rank_or_max(left.role_rank).cmp(&rank_or_max(right.role_rank));
        if by_role != Ordering::Equal {
            return by_role;
        }
    }
    let by_mru = rank_or_max(left.mru_index).cmp(&rank_or_max(right.mru_index));
    if by_mru != Ordering::Equal {
        return by_mru;
    }
    let by_provider = left
        .provider
        .to_lowercase()
        .cmp(&right.provider.to_lowercase());
    if by_provider != Ordering::Equal {
        return by_provider;
    }
    let by_priority = left
        .priority
        .unwrap_or(i32::MAX)
        .cmp(&right.priority.unwrap_or(i32::MAX));
    if by_priority != Ordering::Equal {
        return by_priority;
    }
    let by_version = version_numbers(right.id).cmp(&version_numbers(left.id));
    if by_version != Ordering::Equal {
        return by_version;
    }
    compare_recency(left.id, right.id)
}

fn rank_or_max(rank: Option<usize>) -> usize {
    rank.unwrap_or(usize::MAX)
}

/// The numbers in a model id, in order: `claude-sonnet-4-5` is `[4, 5]`, so a
/// newer version compares greater.
fn version_numbers(id: &str) -> Vec<u64> {
    id.split(|character: char| !character.is_ascii_digit())
        .filter(|run| !run.is_empty() && run.len() < 8)
        .filter_map(|run| run.parse().ok())
        .collect()
}

/// The `YYYYMMDD` suffix or `-latest` marker: models with recency come first,
/// `-latest` first among them, then newer dates, then id.
fn compare_recency(left_id: &str, right_id: &str) -> Ordering {
    let left_date = date_suffix(left_id);
    let right_date = date_suffix(right_id);
    let left_latest = left_id.ends_with("-latest");
    let right_latest = right_id.ends_with("-latest");
    let left_recent = left_latest || left_date.is_some();
    let right_recent = right_latest || right_date.is_some();
    if left_recent != right_recent {
        return if left_recent {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    if !left_recent {
        return left_id.to_lowercase().cmp(&right_id.to_lowercase());
    }
    if left_latest != right_latest {
        return if left_latest {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    right_date.cmp(&left_date)
}

fn date_suffix(id: &str) -> Option<&str> {
    let (_, tail) = id.rsplit_once('-')?;
    (tail.len() == 8 && tail.bytes().all(|byte| byte.is_ascii_digit())).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key<'a>(provider: &'a str, id: &'a str) -> OrderKey<'a> {
        OrderKey {
            selector: id,
            provider,
            id,
            role_rank: None,
            mru_index: None,
            priority: None,
        }
    }

    #[test]
    fn role_then_mru_then_provider_order() {
        let mut keys = vec![
            key("zeta", "a-1"),
            OrderKey {
                role_rank: Some(0),
                ..key("zeta", "b-1")
            },
            OrderKey {
                mru_index: Some(0),
                ..key("alpha", "c-1")
            },
        ];
        sort_order_keys(&mut keys, false);
        assert_eq!(keys[0].id, "b-1");
        assert_eq!(keys[1].id, "c-1");
        assert_eq!(keys[2].id, "a-1");
    }

    #[test]
    fn newer_version_and_latest_sort_first() {
        let mut keys = vec![
            key("anthropic", "claude-sonnet-4-5"),
            key("anthropic", "claude-sonnet-5-5"),
            key("anthropic", "claude-opus-latest"),
        ];
        sort_order_keys(&mut keys, true);
        assert_eq!(keys[0].id, "claude-sonnet-5-5");
        assert_eq!(keys[1].id, "claude-sonnet-4-5");
    }

    #[test]
    fn skipping_role_rank_ignores_roles() {
        let mut keys = vec![
            key("p", "b"),
            OrderKey {
                role_rank: Some(0),
                ..key("p", "a")
            },
        ];
        sort_order_keys(&mut keys, true);
        assert_eq!(keys[0].id, "a");
    }
}

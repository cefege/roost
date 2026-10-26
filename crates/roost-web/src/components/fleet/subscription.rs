//! The fleet page's live-metrics subscription as a pure diff. The client event
//! replaces the whole set, so the page sends only when the desired set differs
//! from what it last sent.

use std::collections::BTreeSet;

use roost_protocol::wire::WorkerFp;

/// The set to subscribe to now, sorted and deduplicated, or `None` when it
/// matches `last_sent` exactly.
pub fn plan_subscription(
    last_sent: &[WorkerFp],
    online: impl IntoIterator<Item = WorkerFp>,
) -> Option<Vec<WorkerFp>> {
    let desired: Vec<WorkerFp> = online
        .into_iter()
        .collect::<BTreeSet<WorkerFp>>()
        .into_iter()
        .collect();
    (desired.as_slice() != last_sent).then_some(desired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(digit: char) -> WorkerFp {
        match WorkerFp::try_from(digit.to_string().repeat(64)) {
            Ok(fp) => fp,
            Err(_) => unreachable!("64 repeated hex digits are a valid fingerprint"),
        }
    }

    #[test]
    fn duplicates_collapse_and_order_is_stable() {
        let planned =
            plan_subscription(&[], [fingerprint('b'), fingerprint('a'), fingerprint('b')]);
        assert_eq!(planned, Some(vec![fingerprint('a'), fingerprint('b')]));
    }

    #[test]
    fn unchanged_roster_sends_nothing() {
        let sent = vec![fingerprint('a'), fingerprint('b')];
        assert_eq!(
            plan_subscription(&sent, [fingerprint('b'), fingerprint('a')]),
            None
        );
    }

    #[test]
    fn a_machine_going_online_sends_the_whole_new_set() {
        let sent = vec![fingerprint('a')];
        assert_eq!(
            plan_subscription(&sent, [fingerprint('a'), fingerprint('c')]),
            Some(vec![fingerprint('a'), fingerprint('c')])
        );
    }

    #[test]
    fn the_last_machine_going_offline_sends_an_empty_set() {
        let sent = vec![fingerprint('a')];
        assert_eq!(plan_subscription(&sent, []), Some(Vec::new()));
    }

    #[test]
    fn nothing_online_and_nothing_sent_sends_nothing() {
        assert_eq!(plan_subscription(&[], []), None);
    }
}

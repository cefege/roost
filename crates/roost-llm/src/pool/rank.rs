//! Orders eligible credentials by rate-limit headroom and known primary usage.
//! The pool calls this after preserving a usable sticky credential.

use crate::credentials::StoredCredential;
use crate::usage::UsageReport;

pub(super) fn rank(
    left: &StoredCredential,
    right: &StoredCredential,
    reports: &[UsageReport],
) -> std::cmp::Ordering {
    let report_for = |id| reports.iter().find(|report| report.credential_id == id);
    let hot = |report: Option<&UsageReport>| {
        report.is_some_and(|report| {
            report
                .windows
                .iter()
                .any(|window| window.used_fraction >= 0.90)
        })
    };
    let primary_usage = |report: Option<&UsageReport>| {
        report
            .and_then(|report| {
                report
                    .windows
                    .iter()
                    .find(|window| matches!(window.name.as_str(), "5h" | "primary"))
            })
            .map(|window| window.used_fraction)
            .filter(|fraction| fraction.is_finite())
    };
    let left_report = report_for(left.id);
    let right_report = report_for(right.id);
    let usage_order = match (primary_usage(left_report), primary_usage(right_report)) {
        (Some(left_usage), Some(right_usage)) => left_usage.total_cmp(&right_usage),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    hot(left_report).cmp(&hot(right_report)).then(usage_order)
}

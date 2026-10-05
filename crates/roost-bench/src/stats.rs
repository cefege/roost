//! Summary statistics over one metric's samples. Called by `report`.

use serde::Serialize;

/// The summary every report row is built from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Summary {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

/// Nearest-rank percentile of an ascending slice; `q` in `(0, 1]`.
pub fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.clamp(1, sorted.len()) - 1).copied()
}

/// `None` for no samples. Non-finite samples are dropped.
pub fn summarize(values: &[f64]) -> Option<Summary> {
    let mut sorted: Vec<f64> = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    Some(Summary {
        n,
        p50: percentile(&sorted, 0.50)?,
        p95: percentile(&sorted, 0.95)?,
        max: *sorted.last()?,
        mean: sorted.iter().sum::<f64>() / n as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::{percentile, summarize};

    #[test]
    fn nearest_rank_picks_an_observed_sample() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&sorted, 0.50), Some(2.0));
        assert_eq!(percentile(&sorted, 0.95), Some(4.0));
        assert_eq!(percentile(&sorted, 1.0), Some(4.0));
        assert_eq!(percentile(&[7.0], 0.50), Some(7.0));
    }

    #[test]
    fn no_samples_is_no_summary() {
        assert_eq!(percentile(&[], 0.50), None);
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&[f64::NAN]), None);
    }

    #[test]
    fn summary_sorts_unordered_input() {
        let summary = summarize(&[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(
            summary.map(|summary| (summary.p50, summary.max, summary.mean)),
            Some((2.0, 4.0, 2.5))
        );
    }
}

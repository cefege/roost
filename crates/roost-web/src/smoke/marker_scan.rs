//! Marker accounting over painted rows: every `${prefix}<N>`, its depth, loss,
//! duplication and render-order inversions. Native; the DOM row read is
//! `smoke::dom`, the caller `smoke::backdoor` (`markerScan`) and the harness
//! (`runRenderStress`). Ports the `markerScan` fold of
//! `apps/web/src/smoke/smokeTerminalRenderProbes.ts:93-146`.

use std::collections::BTreeMap;

use serde::Serialize;

/// `Number.MAX_SAFE_INTEGER`: a marker past it is skipped, as v2 skips it.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Every `${prefix}<digits>` in `text`, left to right, exactly as a global
/// `RegExp(escape(prefix) + "(\\d+)", "g")` finds them: a prefix with no digit
/// after it is not a match, and the search resumes one character later.
pub fn prefixed_markers(text: &str, prefix: &str) -> Vec<u64> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor <= text.len() {
        let Some(offset) = text[cursor..].find(prefix) else {
            break;
        };
        let start = cursor + offset;
        let digits_start = start + prefix.len();
        let digits = text[digits_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits == 0 {
            cursor = start + text[start..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let end = digits_start + digits;
        if let Ok(value) = text[digits_start..end].parse::<u64>()
            && value <= MAX_SAFE_INTEGER
        {
            found.push(value);
        }
        cursor = end;
    }
    found
}

/// What `markerScan()` reports (`SmokeMarkerScan` in `smokeTypes.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmokeMarkerScan {
    /// Every occurrence.
    pub total: u64,
    /// Distinct markers.
    pub unique: u64,
    /// Smallest marker, 0 when none.
    pub min: u64,
    /// Largest marker, 0 when none.
    pub max: u64,
    /// Markers seen more than once, ascending.
    pub duplicated: Vec<u64>,
    /// Markers absent from `[min, max]`.
    pub missing: u64,
    /// Render-order drops.
    pub out_of_order: u64,
    /// The first displaced marker, -1 when none.
    pub first_inversion: i64,
}

/// Scan painted rows in render order.
pub fn scan_painted_rows<'a>(
    rows: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> SmokeMarkerScan {
    let sequence: Vec<u64> = rows
        .into_iter()
        .flat_map(|row| prefixed_markers(row, prefix))
        .collect();
    let tally = MarkerTally::of(&sequence);
    let first_inversion = sequence
        .windows(2)
        .find(|pair| pair[1] < pair[0])
        .map_or(-1, |pair| i64::try_from(pair[1]).unwrap_or(i64::MAX));
    SmokeMarkerScan {
        total: sequence.len() as u64,
        unique: tally.unique(),
        min: tally.min,
        max: tally.max,
        duplicated: tally.duplicated(),
        missing: tally.missing(),
        out_of_order: inversions(&sequence),
        first_inversion,
    }
}

/// Occurrence counts of a marker sequence, shared by the painted and the
/// retained scans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerTally {
    counts: BTreeMap<u64, u64>,
    /// Smallest marker, 0 when none.
    pub min: u64,
    /// Largest marker, 0 when none.
    pub max: u64,
}

impl MarkerTally {
    /// Tally a sequence.
    pub fn of(sequence: &[u64]) -> Self {
        let mut counts = BTreeMap::new();
        for marker in sequence {
            *counts.entry(*marker).or_insert(0) += 1;
        }
        let min = counts.keys().next().copied().unwrap_or(0);
        let max = counts.keys().next_back().copied().unwrap_or(0);
        Self { counts, min, max }
    }

    /// Distinct markers.
    pub fn unique(&self) -> u64 {
        self.counts.len() as u64
    }

    /// Markers seen more than once, ascending.
    pub fn duplicated(&self) -> Vec<u64> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(marker, _)| *marker)
            .collect()
    }

    /// Markers absent from `[min, max]`: counted, not walked, because a stray
    /// large marker would otherwise make the walk the length of its value.
    pub fn missing(&self) -> u64 {
        if self.counts.is_empty() {
            return 0;
        }
        (self.max - self.min + 1) - self.unique()
    }
}

/// Count render-order drops in a sequence.
pub fn inversions(sequence: &[u64]) -> u64 {
    sequence.windows(2).filter(|pair| pair[1] < pair[0]).count() as u64
}

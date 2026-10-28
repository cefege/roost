//! Recorded token and cost totals shared by transcript and inspection responses.
//! The context reading is the latest model prompt, not accumulated session input.
//! This contract is I/O-free and serde-compatible with existing transcript snapshots.

use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    /// The most recent model call's own prompt size (input plus cache reads
    /// and writes), not summed across the session: how full the context
    /// window is right now, for the header's usage ring. `0` before the
    /// first call, or for an entry recorded before this field existed.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub context_tokens: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

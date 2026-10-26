//! Native inspection of the Durable compaction policy and retained usage.
//! The shared kernel owns summarization, head changes and recovery. These
//! helpers keep the existing context inspector aligned with its configuration.

use crate::history::active_window;
use crate::records::Entry;

/// Headroom kept below the context window before compaction triggers.
pub(crate) const HEADROOM_TOKENS: u64 = 16_384;
/// Recent history kept verbatim after a compaction.
pub(crate) const KEEP_TOKENS: u64 = 20_000;

pub(crate) fn latest_compaction_usage_tokens(entries: &[(u64, Entry)]) -> Option<u64> {
    let (_, window) = active_window(entries);
    window.iter().rev().find_map(|(_, entry)| match entry {
        Entry::Assistant { usage, .. } if usage.input + usage.output > 0 => {
            Some(usage.input + usage.cache_read + usage.cache_write + usage.output)
        }
        _ => None,
    })
}

pub(crate) fn compaction_trigger_tokens(context_window: u64) -> u64 {
    context_window.saturating_sub(HEADROOM_TOKENS)
}

#[cfg(test)]
mod tests {
    use super::latest_compaction_usage_tokens;
    use crate::records::Entry;
    use roost_llm::Usage;

    #[test]
    fn automatic_compaction_usage_includes_output_tokens() {
        let assistant = Entry::Assistant {
            item_id: "assistant".into(),
            blocks: Vec::new(),
            usage: Usage {
                input: 100,
                output: 25,
                cache_read: 10,
                cache_write: 5,
            },
            model: None,
            metadata: None,
        };
        assert_eq!(latest_compaction_usage_tokens(&[(1, assistant)]), Some(140));
    }
}

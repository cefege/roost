use roost_llm::{Catalog, Usage};
use roost_protocol::wire::agent_chat::{ModelRef, UsageTotals};

use super::usage_totals;
use crate::records::Entry;

fn assistant(input: u64, output: u64) -> (u64, Entry) {
    (
        0,
        Entry::Assistant {
            item_id: "a".into(),
            blocks: Vec::new(),
            usage: Usage {
                input,
                output,
                cache_read: 0,
                cache_write: 0,
            },
            model: Some(ModelRef {
                provider: "test".into(),
                model_id: "model".into(),
            }),
            metadata: None,
        },
    )
}

#[test]
fn context_tokens_holds_only_the_latest_calls_own_prompt_size() {
    let catalog = Catalog::builtin();
    let entries = [assistant(100, 10), assistant(40, 5)];
    let totals = usage_totals(&catalog, &entries, &UsageTotals::default());
    assert_eq!(totals.context_tokens, 40, "not the 140-token sum");
    assert_eq!(totals.input_tokens, 140, "the session total still sums");
    assert_eq!(totals.output_tokens, 15);
}

#[test]
fn a_call_with_no_usage_keeps_the_last_real_calls_context_size() {
    let catalog = Catalog::builtin();
    let entries = [assistant(100, 10), assistant(0, 0)];
    let totals = usage_totals(&catalog, &entries, &UsageTotals::default());
    assert_eq!(
        totals.context_tokens, 100,
        "a failed or stopped call must not zero the ring"
    );
}

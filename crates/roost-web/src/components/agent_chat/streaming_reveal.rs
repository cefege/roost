//! The reply being written, revealed the way OMP reveals it: whatever text
//! has arrived is shown a few characters per frame, 30 frames a second,
//! catching up within about eight frames however much is waiting. A burst of
//! tokens therefore reads as steady typing instead of a jump. Mounted by
//! `transcript` for the one block still streaming; finished text renders whole.

use dioxus::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

use super::markdown::markdown_to_safe_html;

/// One display frame at OMP's 30 fps reveal clock.
const FRAME_MS: u64 = 33;
/// The fewest characters a frame reveals while text is waiting.
const MIN_STEP: usize = 3;
/// A backlog is revealed over this many frames.
const CATCHUP_FRAMES: usize = 8;

/// How many more characters (graphemes) the next frame shows.
pub(super) fn reveal_step(backlog: usize) -> usize {
    MIN_STEP.max(backlog.div_ceil(CATCHUP_FRAMES)).min(backlog)
}

/// `text` cut after its first `count` graphemes.
pub(super) fn grapheme_prefix(text: &str, count: usize) -> &str {
    text.grapheme_indices(true)
        .nth(count)
        .map_or(text, |(offset, _)| &text[..offset])
}

#[component]
pub(super) fn StreamingText(text: String) -> Element {
    let total = text.graphemes(true).count();
    // Text already on screen when this mounts (a reload mid-reply) is not
    // replayed; only what arrives afterwards is paced.
    let mut shown = use_signal(|| total);
    let mut arrived = use_signal(|| total);
    use_effect(use_reactive((&total,), move |(total,)| arrived.set(total)));
    use_future(move || async move {
        loop {
            crate::components::terminal::dom::sleep_ms(FRAME_MS).await;
            let target = *arrived.peek();
            let current = *shown.peek();
            if current < target {
                shown.set(current + reveal_step(target - current));
            }
        }
    });
    let visible = grapheme_prefix(&text, shown().min(total));
    rsx! {
        div {
            class: "agent-chat__markdown",
            "data-streaming": "true",
            dangerous_inner_html: markdown_to_safe_html(visible),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{grapheme_prefix, reveal_step};

    #[test]
    fn a_frame_reveals_at_least_three_and_clears_a_backlog_in_eight() {
        assert_eq!(reveal_step(2), 2);
        assert_eq!(reveal_step(10), 3);
        assert_eq!(reveal_step(800), 100);
        assert_eq!(grapheme_prefix("héllo wörld", 3), "hél");
        assert_eq!(grapheme_prefix("hi", 9), "hi");
    }
}

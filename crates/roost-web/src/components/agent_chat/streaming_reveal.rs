//! The reply being written, revealed the way OMP reveals it: whatever text
//! has arrived is shown a few characters per frame, 30 frames a second,
//! catching up within about eight frames however much is waiting. A burst of
//! tokens therefore reads as steady typing instead of a jump. Mounted by
//! `transcript` for every assistant text block; one finished at mount renders whole.

use dioxus::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

use super::markdown::markdown_to_safe_html;
use crate::components::ui::Markdown;

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

/// One assistant text block. While `live` (the block still streaming) or
/// still behind what arrived, it reveals at OMP's pace; the stream ending or
/// a tool call landing after it does not drop the backlog on screen at once.
/// A block that is already finished when it mounts renders whole.
#[component]
pub(super) fn StreamingText(text: String, live: bool) -> Element {
    let total = text.graphemes(true).count();
    // A live block starts from the first character, as OMP's `begin` resets
    // to zero: the provider's first chunk is often a whole sentence. A reload
    // mid-reply catches up within CATCHUP_FRAMES.
    let mut shown = use_signal(|| if live { 0 } else { total });
    let mut arrived = use_signal(|| total);
    let mut still_live = use_signal(|| live);
    use_effect(use_reactive((&total, &live), move |(total, live)| {
        arrived.set(total);
        still_live.set(live);
    }));
    use_future(move || async move {
        loop {
            let target = *arrived.peek();
            let current = *shown.peek();
            if current < target {
                shown.set(current + reveal_step(target - current));
            } else if !*still_live.peek() {
                break;
            }
            crate::components::terminal::dom::sleep_ms(FRAME_MS).await;
        }
    });
    let revealed = shown().min(total);
    let typing = live || revealed < total;
    let visible = grapheme_prefix(&text, revealed);
    rsx! {
        Markdown {
            class: "[&[data-streaming]>:last-child]:after:inline-block [&[data-streaming]>:last-child]:after:h-4 [&[data-streaming]>:last-child]:after:w-2 [&[data-streaming]>:last-child]:after:ml-1 [&[data-streaming]>:last-child]:after:rounded-xs [&[data-streaming]>:last-child]:after:align-text-bottom [&[data-streaming]>:last-child]:after:bg-current [&[data-streaming]>:last-child]:after:content-['']",
            "data-streaming": typing.then_some("true"),
            html: markdown_to_safe_html(visible),
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

//! `PhoneSheetContent`: a bottom sheet with a top-only sheet radius. The operator
//! chose the pi-pocket phone look, where a bottom sheet rounds only its top
//! corners. It wraps the stock `SheetContent` (which already draws the scrim
//! behind the panel) and adds `--radius-sheet` on the top edge only. The stock
//! `Sheet` recipes stay untouched beside it.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::focus_scope::AutoFocusRequest;
use crate::components::ui::sheet::{SheetContent, SheetSide};

const PHONE_SHEET_CLASS: &str = "rounded-t-sheet";

#[component]
pub fn PhoneSheetContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default = true)] show_close_button: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    let merged = tw_merge!(PHONE_SHEET_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        SheetContent {
            side: SheetSide::Bottom,
            class: merged,
            test_id,
            show_close_button,
            on_open_auto_focus,
            on_close_auto_focus,
            {children}
        }
    }
}

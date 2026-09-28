//! `Card`: the flat content surface for settings and product screens. Ported
//! from `apps/web/src/components/Settings/md/Card.tsx`; the settings panes and
//! the machine and agent cards compose it. `tokens.css` owns the variants.
//!
//! The header renders only when there is something to put in it, so a card
//! without a title, supporting line or trailing action is its body and nothing
//! else — no empty header band.

use dioxus::prelude::*;

use super::class_list::class_list;

/// The three presentations `tokens.css` declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CardVariant {
    /// The base `.md-card`.
    #[default]
    Filled,
    /// `md-card--elevated`.
    Elevated,
    /// `md-card--outlined`.
    Outlined,
}

/// The card's class attribute.
pub fn card_class(variant: CardVariant, class: Option<&str>) -> String {
    let variant_class = match variant {
        CardVariant::Filled => "",
        CardVariant::Elevated => "md-card--elevated",
        CardVariant::Outlined => "md-card--outlined",
    };
    class_list(["md-card", variant_class, class.unwrap_or("")])
}

/// The heading column's layout: title over supporting line, allowed to shrink.
const CARD_HEADING_STYLE: &str =
    "display: flex; flex-direction: column; gap: 4px; min-width: 0; flex: 1;";

/// A content card.
#[component]
pub fn Card(
    title: Option<String>,
    supporting: Option<String>,
    trailing: Option<Element>,
    #[props(default)] variant: CardVariant,
    class: Option<String>,
    style: Option<String>,
    test_id: Option<String>,
    children: Element,
) -> Element {
    let has_header = title.is_some() || supporting.is_some() || trailing.is_some();
    rsx! {
        section {
            class: card_class(variant, class.as_deref()),
            style,
            "data-testid": test_id,
            if has_header {
                header { class: "md-card__header",
                    div { style: CARD_HEADING_STYLE,
                        if let Some(title) = title {
                            h2 { class: "md-card__title", {title} }
                        }
                        if let Some(supporting) = supporting {
                            p { class: "md-card__supporting", {supporting} }
                        }
                    }
                    if let Some(trailing) = trailing {
                        {trailing}
                    }
                }
            }
            {children}
        }
    }
}

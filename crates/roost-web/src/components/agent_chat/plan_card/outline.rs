//! Section navigation for the plan review document.
//! PlanCard supplies the Markdown and selected section; the outline uses the
//! same parser options as the renderer and never treats code fences as headings.

use std::rc::Rc;

use dioxus::prelude::*;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use super::super::markdown::parser_options;
use crate::components::ui::{
    SidenavMenu, SidenavMenuButton, SidenavMenuButtonSize, SidenavMenuItem,
};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlanSection {
    title: String,
    level: HeadingLevel,
}

pub(super) fn sections(content: &str) -> Vec<PlanSection> {
    let mut sections = Vec::new();
    let mut current: Option<PlanSection> = None;
    for event in Parser::new_ext(content, parser_options()) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some(PlanSection {
                    title: String::new(),
                    level,
                });
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(section) = current.as_mut() {
                    section.title.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(section) = current.as_mut() {
                    section.title.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(mut section) = current.take() {
                    if section.title.trim().is_empty() {
                        section.title = "Untitled section".to_owned();
                    }
                    sections.push(section);
                }
            }
            _ => {}
        }
    }
    sections
}

#[component]
pub(super) fn PlanOutline(
    sections: Rc<[PlanSection]>,
    selected: usize,
    on_select: EventHandler<usize>,
) -> Element {
    let shallowest = sections
        .iter()
        .map(|section| section.level as usize)
        .min()
        .unwrap_or(1);
    rsx! {
        nav {
            class: "hidden min-h-0 w-40 shrink-0 overflow-y-auto overscroll-y-auto border-r border-border pr-2 md:block",
            "aria-label": "Plan sections",
            SidenavMenu {
                for (index, section) in sections.iter().enumerate() {
                    SidenavMenuItem {
                        key: "{index}",
                        class: if section.level as usize > shallowest { "pl-2" } else { "" },
                        SidenavMenuButton {
                            size: SidenavMenuButtonSize::Sm,
                            aria_current: if selected == index { "page" } else { "false" },
                            title: section.title.clone(),
                            onclick: move |_| on_select.call(index),
                            span { "{section.title}" }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn scroll_to_section(document_id: &str, index: usize) {
    use wasm_bindgen::JsCast;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(viewport) = document.get_element_by_id(document_id) else {
        return;
    };
    let Ok(headings) = viewport.query_selector_all("h1, h2, h3, h4, h5, h6") else {
        return;
    };
    let Some(heading) = headings
        .item(index as u32)
        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let top = heading.get_bounding_client_rect().top() - viewport.get_bounding_client_rect().top()
        + f64::from(viewport.scroll_top());
    viewport.scroll_to_with_x_and_y(f64::from(viewport.scroll_left()), top);
}

#[cfg(test)]
mod tests {
    use super::sections;

    #[test]
    fn outline_follows_rendered_headings_not_fenced_examples() {
        let outline = sections(
            "# **Context** and `scope`\n\n```md\n## Not a section\n```\n\nApproach\n--------\n\n### [Verify](https://example.com)\n",
        );
        assert_eq!(
            outline
                .iter()
                .map(|section| section.title.as_str())
                .collect::<Vec<_>>(),
            ["Context and scope", "Approach", "Verify"]
        );
        assert_eq!(
            outline
                .iter()
                .map(|section| section.level as usize)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn leading_rules_do_not_hide_rendered_setext_headings() {
        let outline = sections("---\nOverview\n---\n# Implementation\n");
        assert_eq!(
            outline
                .iter()
                .map(|section| section.title.as_str())
                .collect::<Vec<_>>(),
            ["Overview", "Implementation"]
        );
    }
}

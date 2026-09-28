//! The `.cell-row` tree walks behind `WebLinkHost`: the rows a mutation batch
//! touched, row membership, the rows inside a scope, and the text-node walk
//! (document order) the linkifier reads. `links::dom` feeds mutation
//! records here; `dom::host` calls the rest.
//! Ports the DOM reads of `apps/web/src/renderer/terminal-links.scan.ts` and
//! `terminal-links.dom.ts`.

use js_sys::Array;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, MutationRecord, Node, Text};

/// v2 `ROW_SELECTOR`: the class the row painter stamps on every row.
const ROW_SELECTOR: &str = ".cell-row";

/// The rows a mutation batch touched: each record's own row, plus every row an
/// added node is or contains. Character data names only its row.
pub(in crate::links::dom) fn touched_rows(records: &Array) -> Vec<Element> {
    let mut rows = Vec::new();
    for record in records
        .iter()
        .filter_map(|value| value.dyn_into::<MutationRecord>().ok())
    {
        rows.extend(record.target().as_ref().and_then(row_of));
        if record.type_() == "characterData" {
            continue;
        }
        let added = record.added_nodes();
        for node in (0..added.length()).filter_map(|index| added.item(index)) {
            match node.dyn_ref::<HtmlElement>() {
                None => rows.extend(row_of(&node)),
                Some(element) if element.matches(ROW_SELECTOR).unwrap_or(false) => {
                    rows.push(element.clone().unchecked_into());
                }
                Some(element) => push_rows_within(element, &mut rows),
            }
        }
    }
    rows
}

/// The `.cell-row` holding `node`, or `node` itself when it is one.
fn row_of(node: &Node) -> Option<Element> {
    let element = match node.dyn_ref::<HtmlElement>() {
        Some(html) => html.clone().unchecked_into::<Element>(),
        None => node.parent_element()?,
    };
    element.closest(ROW_SELECTOR).ok().flatten()
}

pub(super) fn is_row(element: &Element) -> bool {
    element.matches(ROW_SELECTOR).unwrap_or(false)
}

pub(super) fn push_rows_within(scope: &Element, rows: &mut Vec<Element>) {
    let Ok(found) = scope.query_selector_all(ROW_SELECTOR) else {
        return;
    };
    rows.extend(
        (0..found.length()).filter_map(|index| found.item(index)?.dyn_into::<Element>().ok()),
    );
}

/// Every descendant text node in document order, as a `SHOW_TEXT` walk yields.
pub(super) fn collect_text_nodes(node: &Node, texts: &mut Vec<Text>) {
    let mut child = node.first_child();
    while let Some(current) = child {
        match current.dyn_ref::<Text>() {
            Some(text) => texts.push(text.clone()),
            None => collect_text_nodes(&current, texts),
        }
        child = current.next_sibling();
    }
}

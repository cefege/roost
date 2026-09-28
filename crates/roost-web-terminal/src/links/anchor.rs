//! Applies validated terminal links to renderer-owned row DOM without reparsing
//! it: authoring an anchor's target attributes, resolving a painted anchor back
//! to its target, and linkifying changed rows from `links::detect` segments.
//! The scanner supplies the rows and the attachment reuses the same authoring
//! before a user opens a producer-painted link. All of it runs over `LinkDom`,
//! which `links::dom` implements for the browser and tests implement natively.
//! Ports `apps/web/src/renderer/terminal-links.dom.ts`.

use super::activation::{LinkModifierKey, link_title};
use super::detect::{PaintedLink, RowLinkInput, RowLinkSegment, compute_row_links};
use crate::cell_row::{
    LINK_KEY_ATTR, ROW_COLUMNS_ATTR, ROW_HAS_LINKS_ATTR, TERMINAL_LINK_CLASS,
    TERMINAL_LINK_TARGET_ATTR,
};
use crate::link_target::{
    ResolveFile, TerminalLinkTarget, classify_terminal_link_target, is_worker_file_href,
};

/// Marks a row the linkifier already wrapped. Renderer updates replace row
/// elements, so the mark identifies only an unchanged row and keeps a
/// mutation-driven rescan from nesting anchors.
pub const SCANNED_ATTR: &str = "data-linkified";

/// The DOM operations the link attachment performs, one DOM call each.
pub trait LinkDom {
    /// An element handle; equality is identity.
    type Element: Clone + PartialEq;
    /// A text node handle; equality is identity.
    type Text: Clone + PartialEq;

    /// `getAttribute`.
    fn attribute(&self, element: &Self::Element, name: &str) -> Option<String>;
    /// `setAttribute`.
    fn set_attribute(&self, element: &Self::Element, name: &str, value: &str);
    /// `removeAttribute`.
    fn remove_attribute(&self, element: &Self::Element, name: &str);
    /// `textContent`.
    fn text_content(&self, element: &Self::Element) -> String;
    /// `childNodes`: each child's `textContent` length in UTF-16 code units,
    /// with the child itself when it is an element.
    fn child_nodes(&self, element: &Self::Element) -> Vec<(Option<Self::Element>, usize)>;
    /// `replaceWith(...childNodes)`: dissolve an element into its children.
    fn unwrap_element(&self, element: &Self::Element);
    /// Every descendant text node in document order (`SHOW_TEXT` walk).
    fn text_nodes(&self, root: &Self::Element) -> Vec<Self::Text>;
    /// A text node's live length in UTF-16 code units.
    fn text_length(&self, text: &Self::Text) -> usize;
    /// `document.createElement("a")`.
    fn create_anchor(&self) -> Self::Element;
    /// Move `start..end` into `anchor`: `surroundContents` inside one node,
    /// `extractContents` + `insertNode` across two. A DOM refusal is dropped:
    /// the replacement row's scan recovers it.
    fn wrap_range(
        &self,
        anchor: &Self::Element,
        start: (&Self::Text, usize),
        end: (&Self::Text, usize),
        same_node: bool,
    );
}

/// Author `anchor` for `target`, keeping the exact terminal-authored target
/// beside the route so a later click reclassifies instead of trusting it.
pub fn apply_terminal_anchor_target<D: LinkDom>(
    dom: &D,
    anchor: &D::Element,
    raw_target: &str,
    target: &TerminalLinkTarget,
    hint: Option<&str>,
    modifier_key: LinkModifierKey,
) {
    dom.set_attribute(anchor, TERMINAL_LINK_TARGET_ATTR, raw_target);
    dom.set_attribute(anchor, "tabindex", "-1");
    dom.set_attribute(anchor, "draggable", "false");
    dom.set_attribute(anchor, "title", &link_title(modifier_key, target.display()));
    match target {
        TerminalLinkTarget::External { href, display } => {
            dom.set_attribute(anchor, "href", href);
            dom.set_attribute(anchor, "target", "_blank");
            dom.set_attribute(anchor, "rel", "noopener noreferrer");
            dom.remove_attribute(anchor, "data-kind");
            dom.set_attribute(anchor, "data-hint", hint.unwrap_or(display));
        }
        TerminalLinkTarget::WorkerFile { href, display, .. } => {
            dom.set_attribute(anchor, "data-kind", "file");
            dom.remove_attribute(anchor, "target");
            dom.remove_attribute(anchor, "rel");
            match href {
                Some(href) => dom.set_attribute(anchor, "href", href),
                None => dom.remove_attribute(anchor, "href"),
            }
            let fallback = format!("Open {display}");
            dom.set_attribute(anchor, "data-hint", hint.unwrap_or(&fallback));
        }
    }
}

/// The target an anchor opens, reclassified from its terminal-authored target
/// (else its `href`). A file target no resolver could route is no target.
pub fn resolve_terminal_anchor_target<D: LinkDom>(
    dom: &D,
    anchor: &D::Element,
    resolve_file: Option<ResolveFile<'_>>,
) -> Option<TerminalLinkTarget> {
    let raw_target = dom
        .attribute(anchor, TERMINAL_LINK_TARGET_ATTR)
        .or_else(|| dom.attribute(anchor, "href"))
        .filter(|raw| !raw.is_empty())?;
    match classify_terminal_link_target(&raw_target, resolve_file)? {
        TerminalLinkTarget::WorkerFile { href: None, .. } => None,
        target => Some(target),
    }
}

/// v2 `terminalRowColumns`: the stamped grid occupancy, or `-1` for a row
/// with no readable stamp — a sentinel that never equals a real width, so an
/// unstamped row never joins the next.
pub fn terminal_row_columns(stamped: Option<&str>) -> i64 {
    stamped.and_then(js_parse_int).unwrap_or(-1)
}

/// JS `parseInt(value, 10)`: leading whitespace, an optional sign, then the
/// leading digits; `None` where JS answers `NaN`. Saturates where JS would
/// answer a huge float, which compares the same way.
pub fn js_parse_int(value: &str) -> Option<i64> {
    let trimmed = value.trim_start_matches(|character: char| {
        crate::link_target::is_js_whitespace(u32::from(character))
    });
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digit_count = digits.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let magnitude = digits[..digit_count].bytes().fold(0i64, |total, digit| {
        total.saturating_mul(10).saturating_add(i64::from(digit - b'0'))
    });
    Some(if negative { -magnitude } else { magnitude })
}

/// One painted producer anchor that survived validation, with its element.
struct PaintedAnchor<E> {
    element: E,
    link: PaintedLink,
}

/// Resolve producer links before detection sees them. An invalid target
/// dissolves back to terminal text instead of blocking an inferred match.
fn collect_painted_links<D: LinkDom>(
    dom: &D,
    row: &D::Element,
    resolve_file: Option<ResolveFile<'_>>,
    modifier_key: LinkModifierKey,
) -> Vec<PaintedAnchor<D::Element>> {
    if dom.attribute(row, ROW_HAS_LINKS_ATTR).is_none() {
        return Vec::new();
    }
    let mut painted = Vec::new();
    let mut offset = 0;
    for (child, length) in dom.child_nodes(row) {
        if let Some(element) = child.filter(|element| dom.attribute(element, LINK_KEY_ATTR).is_some()) {
            let raw_target = dom
                .attribute(&element, TERMINAL_LINK_TARGET_ATTR)
                .or_else(|| dom.attribute(&element, "href"))
                .unwrap_or_default();
            match classify_terminal_link_target(&raw_target, resolve_file) {
                None | Some(TerminalLinkTarget::WorkerFile { href: None, .. }) => {
                    dom.unwrap_element(&element);
                }
                Some(target) => {
                    apply_terminal_anchor_target(dom, &element, &raw_target, &target, None, modifier_key);
                    let key = dom.attribute(&element, LINK_KEY_ATTR).unwrap_or_default();
                    let link = PaintedLink { start: offset, end: offset + length, uri: raw_target, key };
                    painted.push(PaintedAnchor { element, link });
                }
            }
        }
        offset += length;
    }
    if painted.is_empty() {
        dom.remove_attribute(row, ROW_HAS_LINKS_ATTR);
    }
    painted
}

/// Linkify one soft-wrap group (or any run) of rows: validate painted anchors,
/// detect on the joined text, then wrap each unscanned row's segments, last
/// first so earlier offsets stay valid, dissolving painted anchors a segment
/// supersedes.
pub fn linkify_terminal_rows<D: LinkDom>(
    dom: &D,
    rows: &[D::Element],
    cols: i64,
    resolve_file: Option<ResolveFile<'_>>,
    github_owner_repo: Option<&str>,
    modifier_key: LinkModifierKey,
) {
    let mut painted = Vec::with_capacity(rows.len());
    let mut inputs = Vec::with_capacity(rows.len());
    for row in rows {
        let links = collect_painted_links(dom, row, resolve_file, modifier_key);
        let stamped = dom.attribute(row, ROW_COLUMNS_ATTR);
        inputs.push(RowLinkInput {
            text: dom.text_content(row),
            columns: terminal_row_columns(stamped.as_deref()),
            links: links.iter().map(|anchor| anchor.link.clone()).collect(),
        });
        painted.push(links);
    }
    let segments = compute_row_links(&inputs, cols, resolve_file, github_owner_repo);
    let mut by_row: Vec<(usize, Vec<RowLinkSegment>)> = Vec::new();
    for segment in segments {
        match by_row.iter_mut().find(|(row, _)| *row == segment.row) {
            Some((_, list)) => list.push(segment),
            None => by_row.push((segment.row, vec![segment])),
        }
    }
    for (row_index, row_segments) in by_row {
        let row = &rows[row_index];
        if dom.attribute(row, SCANNED_ATTR).is_some() {
            continue;
        }
        for anchor in &painted[row_index] {
            let superseded = row_segments
                .iter()
                .any(|segment| segment.start < anchor.link.end && anchor.link.start < segment.end);
            if superseded {
                dom.unwrap_element(&anchor.element);
            }
        }
        let mut nodes = Vec::new();
        let mut offset = 0;
        for text in dom.text_nodes(row) {
            let length = dom.text_length(&text);
            nodes.push((text, offset, offset + length));
            offset += length;
        }
        for segment in row_segments.iter().rev() {
            wrap_terminal_range(dom, &nodes, segment, modifier_key);
        }
        dom.set_attribute(row, SCANNED_ATTR, "1");
    }
}

/// Wrap one segment. The target is rebuilt from the segment, never trusted:
/// a file segment must carry its source and a minted worker route, anything
/// else must classify as external.
fn wrap_terminal_range<D: LinkDom>(
    dom: &D,
    nodes: &[(D::Text, usize, usize)],
    segment: &RowLinkSegment,
    modifier_key: LinkModifierKey,
) {
    let (raw_target, target, hint) = match &segment.file {
        Some(file) => {
            if file.source.is_empty() || !is_worker_file_href(&segment.url) {
                return;
            }
            let target = TerminalLinkTarget::WorkerFile {
                raw_path: file.source.clone(),
                line: None,
                file_authority: None,
                href: Some(segment.url.clone()),
                display: file.source.clone(),
            };
            (file.source.as_str(), target, Some(file.hint.as_str()))
        }
        None => match classify_terminal_link_target(&segment.url, None) {
            Some(target @ TerminalLinkTarget::External { .. }) => (segment.url.as_str(), target, None),
            _ => return,
        },
    };
    let (start, end) = (segment.start, segment.end);
    let start_node = nodes.iter().position(|(_, node_start, node_end)| *node_end > start && *node_start <= start);
    let end_node = nodes.iter().position(|(_, node_start, node_end)| *node_end >= end && *node_start < end);
    let (Some(start_node), Some(end_node)) = (start_node, end_node) else {
        return;
    };
    let (start_text, start_base, _) = &nodes[start_node];
    let (end_text, end_base, _) = &nodes[end_node];
    let (start_offset, end_offset) = (start - start_base, end - end_base);
    // A live renderer write can shorten a text node between detection and
    // wrapping; skipping lets the replacement row receive the next scan.
    if start_offset > dom.text_length(start_text) || end_offset > dom.text_length(end_text) {
        return;
    }
    let anchor = dom.create_anchor();
    dom.set_attribute(&anchor, "class", TERMINAL_LINK_CLASS);
    apply_terminal_anchor_target(dom, &anchor, raw_target, &target, hint, modifier_key);
    dom.wrap_range(&anchor, (start_text, start_offset), (end_text, end_offset), start_node == end_node);
}

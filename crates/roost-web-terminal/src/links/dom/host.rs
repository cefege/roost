//! `WebLinkHost`: the live DOM behind the `LinkDom`, `LinkScanHost` and
//! `LinkHost` seams, and the mutation-record reading that names touched rows.
//! Every judgement stays in `links::{anchor,scan,attachment}`; this forwards.
//! Ports the DOM reads and writes of `apps/web/src/renderer/terminal-links.ts`,
//! `terminal-links.dom.ts` and `terminal-links.scan.ts`.

mod rows;

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Set;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    Document, Element, EventTarget, HtmlElement, IdleRequestOptions, MutationObserver,
    MutationObserverInit, Text, Window,
};

use super::LinkCallbacks;
use crate::element_style::{set_style_property, style_property_of};
use crate::links::anchor::LinkDom;
use crate::links::attachment::{LinkHost, LinkListener, js_round};
use crate::links::scan::{FrameCallback, LinkScanHost, RowSet};

use rows::{collect_text_nodes, is_row, push_rows_within};

pub(super) use rows::touched_rows;

/// The browser a pane's link attachment runs in.
pub(super) struct WebLinkHost {
    pub(super) container: Element,
    pub(super) window: Window,
    pub(super) document: Document,
    /// Shared with the attachment that owns the teardown, so unregistering a
    /// callback never needs the state borrow a re-entrant callback holds.
    pub(super) callbacks: Rc<LinkCallbacks>,
    pub(super) observer: Option<MutationObserver>,
    pub(super) hint: RefCell<Option<Element>>,
}

/// Rows by identity in a JS `Set`, so membership stays O(1) on a full repaint.
pub(super) struct JsRowSet(Set);

impl RowSet<Element> for JsRowSet {
    fn insert(&mut self, row: &Element) {
        self.0.add(row.as_ref());
    }
    fn remove(&mut self, row: &Element) {
        self.0.delete(row.as_ref());
    }
    fn contains(&self, row: &Element) -> bool {
        self.0.has(row.as_ref())
    }
    fn clear(&mut self) {
        self.0.clear();
    }
    fn len(&self) -> usize {
        self.0.size() as usize
    }
    fn rows(&self) -> Vec<Element> {
        let mut rows = Vec::with_capacity(self.len());
        self.0
            .for_each(&mut |value, _, _| rows.push(value.unchecked_into()));
        rows
    }
}

impl WebLinkHost {
    fn listener_target(&self, listener: LinkListener) -> &EventTarget {
        if listener.on_window() {
            self.window.as_ref()
        } else {
            self.container.as_ref()
        }
    }
}

impl LinkDom for WebLinkHost {
    type Element = Element;
    type Text = Text;

    fn attribute(&self, element: &Element, name: &str) -> Option<String> {
        element.get_attribute(name)
    }
    fn set_attribute(&self, element: &Element, name: &str, value: &str) {
        let _ = element.set_attribute(name, value);
    }
    fn remove_attribute(&self, element: &Element, name: &str) {
        let _ = element.remove_attribute(name);
    }
    fn text_content(&self, element: &Element) -> String {
        element.text_content().unwrap_or_default()
    }
    fn child_nodes(&self, element: &Element) -> Vec<(Option<Element>, usize)> {
        let nodes = element.child_nodes();
        (0..nodes.length())
            .filter_map(|index| nodes.item(index))
            .map(|node| {
                let length = node
                    .text_content()
                    .map_or(0, |text| text.encode_utf16().count());
                let html = node
                    .dyn_ref::<HtmlElement>()
                    .map(|html| html.clone().unchecked_into());
                (html, length)
            })
            .collect()
    }
    fn unwrap_element(&self, element: &Element) {
        let Some(parent) = element.parent_node() else {
            return;
        };
        while let Some(child) = element.first_child() {
            if parent.insert_before(&child, Some(element)).is_err() {
                return;
            }
        }
        element.remove();
    }
    fn text_nodes(&self, root: &Element) -> Vec<Text> {
        let mut texts = Vec::new();
        collect_text_nodes(root, &mut texts);
        texts
    }
    fn text_length(&self, text: &Text) -> usize {
        text.length() as usize
    }
    fn create_anchor(&self) -> Option<Element> {
        self.document.create_element("a").ok()
    }
    fn wrap_range(
        &self,
        anchor: &Element,
        start: (&Text, usize),
        end: (&Text, usize),
        same_node: bool,
    ) {
        let Ok(range) = self.document.create_range() else {
            return;
        };
        let offset = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        if range.set_start(start.0, offset(start.1)).is_err()
            || range.set_end(end.0, offset(end.1)).is_err()
        {
            return;
        }
        // A concurrent boundary mutation is recovered by the replacement row's scan.
        let _ = if same_node {
            range.surround_contents(anchor)
        } else {
            range
                .extract_contents()
                .and_then(|fragment| anchor.append_child(&fragment))
                .and_then(|_| range.insert_node(anchor))
        };
    }
}

impl LinkScanHost for WebLinkHost {
    type Rows = JsRowSet;

    fn new_row_set(&self) -> JsRowSet {
        JsRowSet(Set::new(&JsValue::UNDEFINED))
    }
    fn is_page_visible(&self) -> bool {
        !self.document.hidden()
    }
    fn cell_cols_property(&self) -> String {
        style_property_of(&self.container, "--cell-cols")
    }
    fn hot_rows(&self) -> Vec<Element> {
        let mut rows = Vec::new();
        let newest_block = self
            .container
            .query_selector(".cell-scrollback")
            .ok()
            .flatten()
            .and_then(|scrollback| scrollback.last_element_child());
        let viewport = self
            .container
            .query_selector(".cell-viewport")
            .ok()
            .flatten();
        for scope in newest_block.iter().chain(viewport.iter()) {
            push_rows_within(scope, &mut rows);
        }
        rows
    }
    fn previous_row(&self, row: &Element) -> Option<Element> {
        if let Some(sibling) = row.previous_element_sibling().filter(is_row) {
            return Some(sibling);
        }
        let parent = row.parent_element()?;
        let scope = if parent.class_list().contains("cell-block") {
            parent.previous_element_sibling()
        } else if parent.class_list().contains("cell-viewport") {
            parent
                .parent_element()
                .and_then(|root| root.query_selector(".cell-scrollback").ok().flatten())
                .and_then(|scrollback| scrollback.last_element_child())
        } else {
            None
        };
        scope?.last_element_child().filter(is_row)
    }
    fn next_row(&self, row: &Element) -> Option<Element> {
        if let Some(sibling) = row.next_element_sibling().filter(is_row) {
            return Some(sibling);
        }
        let parent = row.parent_element()?;
        if !parent.class_list().contains("cell-block") {
            return None;
        }
        let scope = parent.next_element_sibling().or_else(|| {
            parent
                .parent_element()
                .and_then(|scrollback| scrollback.parent_element())
                .and_then(|root| root.query_selector(".cell-viewport").ok().flatten())
        });
        scope?.first_element_child().filter(is_row)
    }
    fn is_connected(&self, row: &Element) -> bool {
        row.is_connected()
    }
    fn request_idle_callback(&self, timeout_ms: u32) -> Option<u32> {
        let options = IdleRequestOptions::new();
        options.set_timeout(timeout_ms);
        let callback = self.callbacks.idle_scan.as_ref().unchecked_ref();
        self.window
            .request_idle_callback_with_options(callback, &options)
            .ok()
    }
    fn cancel_idle_callback(&self, handle: u32) {
        self.window.cancel_idle_callback(handle);
    }
    fn request_animation_frame(&self, callback: FrameCallback) -> u32 {
        let closure = match callback {
            FrameCallback::Scan => &self.callbacks.scan_frame,
            FrameCallback::ActivationScan => &self.callbacks.activation_frame,
        };
        match self
            .window
            .request_animation_frame(closure.as_ref().unchecked_ref())
        {
            Ok(handle) => u32::try_from(handle).unwrap_or(0),
            Err(_) => {
                tracing::warn!(target: "terminal_links", ?callback, "the browser refused a link scan frame");
                0
            }
        }
    }
    fn cancel_animation_frame(&self, handle: u32) {
        if let Ok(handle) = i32::try_from(handle) {
            let _ = self.window.cancel_animation_frame(handle);
        }
    }
    fn observe_mutations(&self, observe: bool) {
        let Some(observer) = &self.observer else {
            return;
        };
        if !observe {
            observer.disconnect();
            return;
        }
        let init = MutationObserverInit::new();
        init.set_child_list(true);
        init.set_character_data(true);
        init.set_subtree(true);
        if observer
            .observe_with_options(&self.container, &init)
            .is_err()
        {
            tracing::warn!(target: "terminal_links", "the browser refused the link mutation observer");
        }
    }
    fn listen_for_visibility(&self, listen: bool) {
        let target: &EventTarget = self.document.as_ref();
        let callback = self.callbacks.visibility.as_ref().unchecked_ref();
        let _ = if listen {
            target.add_event_listener_with_callback("visibilitychange", callback)
        } else {
            target.remove_event_listener_with_callback("visibilitychange", callback)
        };
    }
}

impl LinkHost for WebLinkHost {
    fn container(&self) -> Element {
        self.container.clone()
    }
    fn add_listener(&self, listener: LinkListener) {
        let Some(callback) = self.callbacks.listener(listener) else {
            return;
        };
        let _ = self
            .listener_target(listener)
            .add_event_listener_with_callback(listener.event_type(), callback);
    }
    fn remove_listener(&self, listener: LinkListener) {
        let Some(callback) = self.callbacks.listener(listener) else {
            return;
        };
        let _ = self
            .listener_target(listener)
            .remove_event_listener_with_callback(listener.event_type(), callback);
    }
    fn show_hint(&self, anchor: &Element, text: &str) {
        let mut hint = self.hint.borrow_mut();
        if hint.is_none() {
            let (Ok(created), Some(body)) =
                (self.document.create_element("div"), self.document.body())
            else {
                return;
            };
            created.set_class_name("wterm-link-hint");
            let _ = body.append_child(&created);
            *hint = Some(created);
        }
        let Some(hint) = hint.as_ref() else {
            return;
        };
        hint.set_text_content(Some(text));
        let rect = anchor.get_bounding_client_rect();
        set_style_property(hint, "left", &format!("{}px", js_round(rect.left())));
        set_style_property(hint, "top", &format!("{}px", js_round(rect.bottom() + 4.0)));
        set_style_property(hint, "display", "block");
    }
    fn hide_hint(&self) {
        if let Some(hint) = self.hint.borrow().as_ref() {
            set_style_property(hint, "display", "none");
        }
    }
    fn remove_hint(&self) {
        if let Some(hint) = self.hint.borrow_mut().take() {
            hint.remove();
        }
    }
    fn click_detached_anchor(&self, anchor: &Element) {
        let (Some(body), Some(html)) = (self.document.body(), anchor.dyn_ref::<HtmlElement>())
        else {
            return;
        };
        let _ = body.append_child(anchor);
        html.click();
        anchor.remove();
    }
}

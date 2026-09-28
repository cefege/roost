//! `web_sys::Element` as the renderer's element: the one place the renderer's
//! DOM reads and writes reach a browser.
//!
//! Every judgement stays in `CellGridRenderer`; this file only forwards. The
//! two properties the stable `Element` surface lacks — inline `style` and the
//! double-valued `scrollTop` — go through `element_style`, which owns them.

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, Node};

use crate::cell_renderer_dom::{DomResult, DomSetupError};
use crate::element_style::{
    remove_style_property, scroll_top_of, set_scroll_top_of, set_style_property,
};
use crate::render_element::{ElementRect, RenderElement};

fn as_node(element: &Element) -> &Node {
    AsRef::<Node>::as_ref(element)
}

impl RenderElement for Element {
    fn create_element(&self, tag: &str) -> DomResult<Self> {
        self.owner_document()
            .and_then(|document| document.create_element(tag).ok())
            .ok_or_else(|| DomSetupError::RefusedTag {
                tag: tag.to_string(),
            })
    }

    fn append_child(&self, child: &Self) {
        let _ = Node::append_child(as_node(self), as_node(child));
    }

    fn insert_before(&self, child: &Self, reference: Option<&Self>) {
        let _ = Node::insert_before(as_node(self), as_node(child), reference.map(as_node));
    }

    fn remove(&self) {
        if let Some(parent) = as_node(self).parent_node() {
            let _ = parent.remove_child(as_node(self));
        }
    }

    fn replace_with(&self, replacement: &Self) {
        if let Some(parent) = as_node(self).parent_node() {
            let _ = parent.replace_child(as_node(replacement), as_node(self));
        }
    }

    fn clear_children(&self) {
        self.set_inner_html("");
    }

    fn parent(&self) -> Option<Self> {
        self.parent_element()
    }

    fn child_count(&self) -> u32 {
        self.child_element_count()
    }

    fn child_at(&self, index: u32) -> Option<Self> {
        self.children().item(index)
    }

    fn first_child(&self) -> Option<Self> {
        self.first_element_child()
    }

    fn class_name(&self) -> String {
        Element::class_name(self)
    }

    fn set_class_name(&self, name: &str) {
        Element::set_class_name(self, name);
    }

    fn add_class(&self, name: &str) {
        let _ = self.class_list().add_1(name);
    }

    fn toggle_class(&self, name: &str, on: bool) {
        let _ = self.class_list().toggle_with_force(name, on);
    }

    fn attribute(&self, name: &str) -> Option<String> {
        self.get_attribute(name)
    }

    fn set_attribute(&self, name: &str, value: &str) {
        let _ = Element::set_attribute(self, name, value);
    }

    fn remove_attribute(&self, name: &str) {
        let _ = Element::remove_attribute(self, name);
    }

    fn set_text(&self, text: &str) {
        self.set_text_content(Some(text));
    }

    fn set_style(&self, property: &str, value: &str) {
        set_style_property(self, property, value);
    }

    fn remove_style(&self, property: &str) {
        remove_style_property(self, property);
    }

    fn scroll_top(&self) -> f64 {
        scroll_top_of(self)
    }

    fn set_scroll_top(&self, value: f64) {
        set_scroll_top_of(self, value);
    }

    fn scroll_height(&self) -> f64 {
        f64::from(Element::scroll_height(self))
    }

    fn client_height(&self) -> f64 {
        f64::from(Element::client_height(self))
    }

    fn offset_top(&self) -> f64 {
        self.dyn_ref::<HtmlElement>()
            .map_or(0.0, |html| f64::from(html.offset_top()))
    }

    fn bounding_rect(&self) -> ElementRect {
        let rect = self.get_bounding_client_rect();
        ElementRect {
            left: rect.left(),
            top: rect.top(),
            width: rect.width(),
            height: rect.height(),
        }
    }

    fn is_connected(&self) -> bool {
        as_node(self).is_connected()
    }

    fn now_ms(&self) -> f64 {
        web_sys::window()
            .and_then(|window| window.performance())
            .map_or(0.0, |performance| performance.now())
    }
}

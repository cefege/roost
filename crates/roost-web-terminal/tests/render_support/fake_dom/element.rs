//! `RenderElement` for the fake element: the renderer's whole DOM surface,
//! with structural calls that MOVE a placed node exactly as the DOM does, and
//! the scroll/box reads derived from `FakeEl`'s layout model.

use std::rc::Rc;

use roost_web_terminal::cell_renderer_dom::DomResult;
use roost_web_terminal::{ElementRect, RenderElement};

use super::{CELL_PX, FakeEl, PAD_TOP, PANE_PX, ROW_PX};

impl RenderElement for FakeEl {
    fn create_element(&self, tag: &str) -> DomResult<Self> {
        Ok(Self::new(tag))
    }

    fn append_child(&self, child: &Self) {
        child.detach_from_parent();
        child.0.borrow_mut().parent = Some(Rc::downgrade(&self.0));
        self.0.borrow_mut().children.push(child.clone());
    }

    fn insert_before(&self, child: &Self, reference: Option<&Self>) {
        child.detach_from_parent();
        let index = reference.and_then(|reference| {
            self.0
                .borrow()
                .children
                .iter()
                .position(|existing| existing == reference)
        });
        child.0.borrow_mut().parent = Some(Rc::downgrade(&self.0));
        let mut node = self.0.borrow_mut();
        match index {
            Some(index) => node.children.insert(index, child.clone()),
            None => node.children.push(child.clone()),
        }
    }

    fn remove(&self) {
        self.detach_from_parent();
    }

    fn replace_with(&self, replacement: &Self) {
        let Some(parent) = self.parent_el() else {
            return;
        };
        replacement.detach_from_parent();
        let mut node = parent.0.borrow_mut();
        if let Some(index) = node.children.iter().position(|child| child == self) {
            node.children[index] = replacement.clone();
            replacement.0.borrow_mut().parent = Some(Rc::downgrade(&parent.0));
            self.0.borrow_mut().parent = None;
        }
    }

    fn clear_children(&self) {
        let children = std::mem::take(&mut self.0.borrow_mut().children);
        for child in children {
            child.0.borrow_mut().parent = None;
        }
    }

    fn parent(&self) -> Option<Self> {
        self.parent_el()
    }

    fn child_count(&self) -> u32 {
        u32::try_from(self.0.borrow().children.len()).unwrap_or(u32::MAX)
    }

    fn child_at(&self, index: u32) -> Option<Self> {
        self.0.borrow().children.get(index as usize).cloned()
    }

    fn class_name(&self) -> String {
        self.0.borrow().class_name.clone()
    }

    fn set_class_name(&self, name: &str) {
        self.0.borrow_mut().class_name = name.to_string();
    }

    fn add_class(&self, name: &str) {
        if !self.has_class(name) {
            let mut node = self.0.borrow_mut();
            if !node.class_name.is_empty() {
                node.class_name.push(' ');
            }
            node.class_name.push_str(name);
        }
    }

    fn toggle_class(&self, name: &str, on: bool) {
        if on {
            self.add_class(name);
            return;
        }
        let mut node = self.0.borrow_mut();
        node.class_name = node
            .class_name
            .split_whitespace()
            .filter(|class| *class != name)
            .collect::<Vec<_>>()
            .join(" ");
    }

    fn attribute(&self, name: &str) -> Option<String> {
        self.0.borrow().attrs.get(name).cloned()
    }

    fn set_attribute(&self, name: &str, value: &str) {
        self.0
            .borrow_mut()
            .attrs
            .insert(name.to_string(), value.to_string());
    }

    fn remove_attribute(&self, name: &str) {
        self.0.borrow_mut().attrs.remove(name);
    }

    fn set_text(&self, text: &str) {
        self.clear_children();
        self.0.borrow_mut().text = text.to_string();
    }

    fn set_style(&self, property: &str, value: &str) {
        self.0
            .borrow_mut()
            .style
            .insert(property.to_string(), value.to_string());
    }

    fn remove_style(&self, property: &str) {
        self.0.borrow_mut().style.remove(property);
    }

    fn scroll_top(&self) -> f64 {
        self.0.borrow().scroll_top
    }

    fn set_scroll_top(&self, value: f64) {
        let mut node = self.0.borrow_mut();
        node.scroll_top = node.next_scroll_top_write_result.take().unwrap_or(value);
        node.scroll_top_writes += 1;
    }

    fn scroll_height(&self) -> f64 {
        PAD_TOP + self.painted_rows() * ROW_PX
    }

    fn client_height(&self) -> f64 {
        self.0.borrow().client_height
    }

    fn offset_top(&self) -> f64 {
        if self.class_name() != "cell-scrollback" {
            return PAD_TOP;
        }
        let spacer = self.parent_el().and_then(|parent| {
            parent
                .children()
                .into_iter()
                .find(|child| child.class_name() == "cell-sb-spacer")
        });
        PAD_TOP + spacer.map_or(0.0, |spacer| spacer.style_px("height"))
    }

    fn bounding_rect(&self) -> ElementRect {
        let class_name = self.class_name();
        let height = if class_name == "cell-row" {
            ROW_PX
        } else {
            self.painted_rows() * ROW_PX
        };
        let width = if class_name == "cell-viewport" || class_name == "cell-scrollback" {
            self.grid_cols() * CELL_PX
        } else {
            PANE_PX
        };
        ElementRect {
            left: 0.0,
            top: self.client_top(),
            width,
            height,
        }
    }

    fn is_connected(&self) -> bool {
        !self.root().0.borrow().disconnected
    }

    fn now_ms(&self) -> f64 {
        self.root().0.borrow().now_ms
    }
}

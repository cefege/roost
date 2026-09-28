//! An in-memory `RenderElement` with the layout model v2's renderer tripwire
//! suite used (`apps/web/tests/helpers/cellRendererFakeDom.ts`): scroll space
//! is derived from the painted rows and the reserved placeholders, so a test
//! can tell browser geometry from the renderer's one conditional scroll writer.
//!
//! Node identity is what the suite asserts, so a handle is an `Rc` and equality
//! is pointer identity, exactly like a DOM reference.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::{Rc, Weak};

mod element;

/// `.wterm` padding-top.
pub const PAD_TOP: f64 = 12.0;
/// One `.cell-row` line box.
pub const ROW_PX: f64 = 16.0;
/// One column advance (1ch in the display font).
pub const CELL_PX: f64 = 8.0;
/// The pane's client width — wider than the grid, so rows letterbox.
pub const PANE_PX: f64 = 1000.0;

#[derive(Default)]
struct FakeNode {
    tag: String,
    class_name: String,
    children: Vec<FakeEl>,
    parent: Option<Weak<RefCell<FakeNode>>>,
    style: BTreeMap<String, String>,
    attrs: BTreeMap<String, String>,
    text: String,
    scroll_top: f64,
    next_scroll_top_write_result: Option<f64>,
    scroll_top_writes: u32,
    client_height: f64,
    disconnected: bool,
    now_ms: f64,
    clear_children_calls: u32,
    /// `(spacer height, root scrollHeight)` right after the last wipe.
    last_wipe: Option<(f64, f64)>,
    rect_override: Option<roost_web_terminal::ElementRect>,
}

/// One fake element. Cloning clones the reference.
#[derive(Clone)]
pub struct FakeEl(Rc<RefCell<FakeNode>>);

impl PartialEq for FakeEl {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for FakeEl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let node = self.0.borrow();
        write!(formatter, "<{} class={:?}>", node.tag, node.class_name)
    }
}

impl FakeEl {
    /// A detached element of `tag`, with the fake's 500px client height.
    pub fn new(tag: &str) -> Self {
        Self(Rc::new(RefCell::new(FakeNode {
            tag: tag.to_string(),
            client_height: 500.0,
            ..FakeNode::default()
        })))
    }

    /// A fresh pane container (`makeContainer`).
    pub fn container() -> Self {
        Self::new("div")
    }

    /// The element's tag.
    pub fn tag(&self) -> String {
        self.0.borrow().tag.clone()
    }

    /// The element children, in order.
    pub fn children(&self) -> Vec<FakeEl> {
        self.0.borrow().children.clone()
    }

    /// Whether the class list holds `name`.
    pub fn has_class(&self, name: &str) -> bool {
        self.0
            .borrow()
            .class_name
            .split_whitespace()
            .any(|class| class == name)
    }

    /// One inline style property.
    pub fn style(&self, property: &str) -> Option<String> {
        self.0.borrow().style.get(property).cloned()
    }

    /// One inline style property parsed as pixels, or zero.
    pub fn style_px(&self, property: &str) -> f64 {
        self.style(property)
            .and_then(|value| value.trim_end_matches("px").parse().ok())
            .unwrap_or(0.0)
    }

    /// The subtree's text, like `Node.textContent`.
    pub fn text_content(&self) -> String {
        let node = self.0.borrow();
        if node.children.is_empty() {
            return node.text.clone();
        }
        node.children.iter().map(FakeEl::text_content).collect()
    }

    /// A direct setup write that is NOT counted as an application write.
    pub fn set_scroll_top_raw(&self, value: f64) {
        self.0.borrow_mut().scroll_top = value;
    }

    /// How many application `scrollTop` writes landed since the last reset.
    pub fn scroll_top_writes(&self) -> u32 {
        self.0.borrow().scroll_top_writes
    }

    /// Forget the counted writes.
    pub fn reset_scroll_top_writes(&self) {
        self.0.borrow_mut().scroll_top_writes = 0;
    }

    /// Make the next write land somewhere else, as a browser clamp does.
    pub fn set_next_scroll_top_write_result(&self, value: Option<f64>) {
        self.0.borrow_mut().next_scroll_top_write_result = value;
    }

    /// Resize the box.
    pub fn set_client_height(&self, value: f64) {
        self.0.borrow_mut().client_height = value;
    }

    /// Mark the element as removed from, or returned to, the document.
    pub fn set_connected(&self, connected: bool) {
        self.0.borrow_mut().disconnected = !connected;
    }

    /// How many times the renderer wiped this element's children.
    pub fn clear_children_calls(&self) -> u32 {
        self.0.borrow().clear_children_calls
    }

    /// The spacer's reserved height and the pane's scroll height at the instant
    /// this element's children were last wiped.
    pub fn last_wipe(&self) -> Option<(f64, f64)> {
        self.0.borrow().last_wipe
    }

    /// Make the box read back as `rect`, as a detached or zero-size layout does.
    pub fn set_bounding_rect_override(&self, rect: Option<roost_web_terminal::ElementRect>) {
        self.0.borrow_mut().rect_override = rect;
    }

    /// Advance the document clock.
    pub fn set_now_ms(&self, now_ms: f64) {
        self.0.borrow_mut().now_ms = now_ms;
    }

    /// Insert `child` at `index` among the children, as an injected fault.
    pub fn splice_child(&self, index: usize, child: &FakeEl) {
        child.detach_from_parent();
        child.0.borrow_mut().parent = Some(Rc::downgrade(&self.0));
        self.0.borrow_mut().children.insert(index, child.clone());
    }

    /// Rows this element contributes to scroll space: a spacer or gap by its
    /// reserved height, a row as one, anything else by its children.
    pub fn painted_rows(&self) -> f64 {
        let class_name = self.0.borrow().class_name.clone();
        if class_name == "cell-sb-spacer" || class_name == "cell-sb-gap" {
            return self.style_px("height") / ROW_PX;
        }
        if class_name == "cell-row" {
            return 1.0;
        }
        self.children().iter().map(FakeEl::painted_rows).sum()
    }

    fn parent_el(&self) -> Option<FakeEl> {
        self.0
            .borrow()
            .parent
            .as_ref()
            .and_then(Weak::upgrade)
            .map(FakeEl)
    }

    fn detach_from_parent(&self) {
        if let Some(parent) = self.parent_el() {
            parent.0.borrow_mut().children.retain(|child| child != self);
        }
        self.0.borrow_mut().parent = None;
    }

    fn root(&self) -> FakeEl {
        let mut root = self.clone();
        while let Some(parent) = root.parent_el() {
            root = parent;
        }
        root
    }

    fn grid_cols(&self) -> f64 {
        self.root()
            .style("--cell-cols")
            .and_then(|value| value.parse().ok())
            .unwrap_or(80.0)
    }

    /// The container's own box starts at 0; a child starts below the painted
    /// height of its preceding siblings, plus the pane's padding, minus how far
    /// the scroller has been scrolled.
    fn client_top(&self) -> f64 {
        let Some(parent) = self.parent_el() else {
            return 0.0;
        };
        let mut top = parent.client_top();
        if parent.parent_el().is_none() {
            top += PAD_TOP - parent.0.borrow().scroll_top;
        }
        for sibling in parent.children() {
            if sibling == *self {
                break;
            }
            top += sibling.painted_rows() * ROW_PX;
        }
        top
    }
}

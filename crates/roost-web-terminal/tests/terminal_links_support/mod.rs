//! A fake browser for the link attachment, ported from the harnesses of
//! `apps/web/tests/terminal-links.dom.test.ts` and
//! `apps/web/tests/renderer/terminal-links.activation.dom.test.ts`: a small
//! node tree with text nodes, a queued animation-frame list with REAL cancel,
//! no idle callbacks, and counters for every listener and observer.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use roost_web_terminal::links::{
    FrameCallback, LinkDom, LinkHost, LinkListener, LinkScanHost, RowSet,
};

#[derive(Default)]
struct NodeData {
    /// `None` for a text node.
    tag: Option<String>,
    text: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Node>,
    parent: Option<Weak<RefCell<NodeData>>>,
}

/// One element or text node; equality is identity.
#[derive(Clone)]
pub struct Node(Rc<RefCell<NodeData>>);

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Node {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "<{:?} {:?}>",
            self.0.borrow().tag,
            self.text_content()
        )
    }
}

impl Node {
    pub fn element(tag: &str) -> Self {
        Self(Rc::new(RefCell::new(NodeData {
            tag: Some(tag.to_string()),
            ..NodeData::default()
        })))
    }
    pub fn text(data: &str) -> Self {
        Self(Rc::new(RefCell::new(NodeData {
            text: data.to_string(),
            ..NodeData::default()
        })))
    }
    pub fn tag(&self) -> Option<String> {
        self.0.borrow().tag.clone()
    }
    pub fn attr(&self, name: &str) -> Option<String> {
        self.0.borrow().attrs.get(name).cloned()
    }
    pub fn set_attr(&self, name: &str, value: &str) {
        self.0
            .borrow_mut()
            .attrs
            .insert(name.to_string(), value.to_string());
    }
    pub fn children(&self) -> Vec<Node> {
        self.0.borrow().children.clone()
    }
    pub fn parent(&self) -> Option<Node> {
        self.0
            .borrow()
            .parent
            .as_ref()
            .and_then(Weak::upgrade)
            .map(Node)
    }
    pub fn append(&self, child: &Node) {
        child.0.borrow_mut().parent = Some(Rc::downgrade(&self.0));
        self.0.borrow_mut().children.push(child.clone());
    }
    pub fn text_content(&self) -> String {
        let node = self.0.borrow();
        match node.tag {
            None => node.text.clone(),
            Some(_) => node.children.iter().map(Node::text_content).collect(),
        }
    }
    fn replace_in_parent(&self, replacement: Vec<Node>) {
        let Some(parent) = self.parent() else {
            return;
        };
        for node in &replacement {
            node.0.borrow_mut().parent = Some(Rc::downgrade(&parent.0));
        }
        let mut data = parent.0.borrow_mut();
        if let Some(index) = data.children.iter().position(|child| child == self) {
            data.children.splice(index..=index, replacement);
        }
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn utf16_split(text: &str, at: usize) -> (String, String) {
    let units: Vec<u16> = text.encode_utf16().collect();
    (
        String::from_utf16_lossy(&units[..at]),
        String::from_utf16_lossy(&units[at..]),
    )
}

/// Rows by identity, in insertion order.
#[derive(Default)]
pub struct FakeRows(Vec<Node>);

impl RowSet<Node> for FakeRows {
    fn insert(&mut self, row: &Node) {
        if !self.0.contains(row) {
            self.0.push(row.clone());
        }
    }
    fn remove(&mut self, row: &Node) {
        self.0.retain(|held| held != row);
    }
    fn contains(&self, row: &Node) -> bool {
        self.0.contains(row)
    }
    fn clear(&mut self) {
        self.0.clear();
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn rows(&self) -> Vec<Node> {
        self.0.clone()
    }
}

#[derive(Default)]
pub struct Page {
    pub container: Option<Node>,
    pub hidden: Cell<bool>,
    pub cell_cols: RefCell<String>,
    /// The viewport's rows, once the test installs a viewport.
    pub viewport_rows: RefCell<Option<Vec<Node>>>,
    /// How many times the scanner read the viewport's rows.
    pub viewport_reads: Cell<u32>,
    pub frames: RefCell<Vec<(u32, FrameCallback)>>,
    next_frame: Cell<u32>,
    pub observe_calls: Cell<u32>,
    pub disconnect_calls: Cell<u32>,
    pub visibility_listeners: Cell<u32>,
    pub listeners: RefCell<Vec<LinkListener>>,
    pub hint: RefCell<Option<String>>,
    pub clicked: RefCell<Vec<Option<String>>>,
}

/// The fake host: a handle, so the test keeps one while the attachment owns one.
#[derive(Clone)]
pub struct FakeLinkHost(pub Rc<Page>);

impl FakeLinkHost {
    pub fn new(cols: &str) -> Self {
        let page = Page {
            container: Some(Node::element("div")),
            ..Page::default()
        };
        *page.cell_cols.borrow_mut() = cols.to_string();
        Self(Rc::new(page))
    }
    pub fn container_node(&self) -> Node {
        self.0
            .container
            .clone()
            .expect("the harness always has a container")
    }
    pub fn listener_count(&self, listener: LinkListener) -> usize {
        self.0
            .listeners
            .borrow()
            .iter()
            .filter(|held| **held == listener)
            .count()
    }
    pub fn is_listening(&self, listener: LinkListener) -> bool {
        self.listener_count(listener) > 0
    }
    /// Remove and return the oldest queued frame.
    pub fn take_next_frame(&self) -> Option<FrameCallback> {
        let mut frames = self.0.frames.borrow_mut();
        (!frames.is_empty()).then(|| frames.remove(0).1)
    }
    pub fn frame_handles(&self) -> Vec<u32> {
        self.0
            .frames
            .borrow()
            .iter()
            .map(|(handle, _)| *handle)
            .collect()
    }
}

impl LinkDom for FakeLinkHost {
    type Element = Node;
    type Text = Node;

    fn attribute(&self, element: &Node, name: &str) -> Option<String> {
        element.attr(name)
    }
    fn set_attribute(&self, element: &Node, name: &str, value: &str) {
        element.set_attr(name, value);
    }
    fn remove_attribute(&self, element: &Node, name: &str) {
        element.0.borrow_mut().attrs.remove(name);
    }
    fn text_content(&self, element: &Node) -> String {
        element.text_content()
    }
    fn child_nodes(&self, element: &Node) -> Vec<(Option<Node>, usize)> {
        element
            .children()
            .into_iter()
            .map(|child| {
                let length = utf16_len(&child.text_content());
                (child.tag().is_some().then_some(child), length)
            })
            .collect()
    }
    fn unwrap_element(&self, element: &Node) {
        element.replace_in_parent(element.children());
    }
    fn text_nodes(&self, root: &Node) -> Vec<Node> {
        let mut texts = Vec::new();
        for child in root.children() {
            match child.tag() {
                None => texts.push(child),
                Some(_) => texts.extend(self.text_nodes(&child)),
            }
        }
        texts
    }
    fn text_length(&self, text: &Node) -> usize {
        utf16_len(&text.text_content())
    }
    fn create_anchor(&self) -> Option<Node> {
        Some(Node::element("a"))
    }
    /// `surroundContents` within one text node; across two, both must share a
    /// parent, as every row this harness builds does.
    fn wrap_range(
        &self,
        anchor: &Node,
        start: (&Node, usize),
        end: (&Node, usize),
        same_node: bool,
    ) {
        let (before, rest) = utf16_split(&start.0.text_content(), start.1);
        if same_node {
            let (selected, after) = utf16_split(&rest, end.1 - start.1);
            anchor.append(&Node::text(&selected));
            let mut replacement = Vec::new();
            replacement.extend((!before.is_empty()).then(|| Node::text(&before)));
            replacement.push(anchor.clone());
            replacement.extend((!after.is_empty()).then(|| Node::text(&after)));
            start.0.replace_in_parent(replacement);
            return;
        }
        let Some(parent) = start.0.parent() else {
            return;
        };
        let siblings = parent.children();
        let (Some(first), Some(last)) = (
            siblings.iter().position(|node| node == start.0),
            siblings.iter().position(|node| node == end.0),
        ) else {
            return;
        };
        let (end_selected, after) = utf16_split(&end.0.text_content(), end.1);
        anchor.append(&Node::text(&rest));
        for middle in &siblings[first + 1..last] {
            anchor.append(middle);
        }
        anchor.append(&Node::text(&end_selected));
        let mut rebuilt: Vec<Node> = siblings[..first].to_vec();
        rebuilt.extend((!before.is_empty()).then(|| Node::text(&before)));
        rebuilt.push(anchor.clone());
        rebuilt.extend((!after.is_empty()).then(|| Node::text(&after)));
        rebuilt.extend(siblings[last + 1..].iter().cloned());
        for node in &rebuilt {
            node.0.borrow_mut().parent = Some(Rc::downgrade(&parent.0));
        }
        parent.0.borrow_mut().children = rebuilt;
    }
}

impl LinkScanHost for FakeLinkHost {
    type Rows = FakeRows;

    fn new_row_set(&self) -> FakeRows {
        FakeRows::default()
    }
    fn is_page_visible(&self) -> bool {
        !self.0.hidden.get()
    }
    fn cell_cols_property(&self) -> String {
        self.0.cell_cols.borrow().clone()
    }
    fn hot_rows(&self) -> Vec<Node> {
        let rows = self.0.viewport_rows.borrow().clone();
        if rows.is_some() {
            self.0.viewport_reads.set(self.0.viewport_reads.get() + 1);
        }
        rows.unwrap_or_default()
    }
    fn previous_row(&self, _row: &Node) -> Option<Node> {
        None
    }
    fn next_row(&self, _row: &Node) -> Option<Node> {
        None
    }
    fn is_connected(&self, _row: &Node) -> bool {
        true
    }
    fn request_idle_callback(&self, _timeout_ms: u32) -> Option<u32> {
        None
    }
    fn cancel_idle_callback(&self, _handle: u32) {}
    fn request_animation_frame(&self, callback: FrameCallback) -> u32 {
        let handle = self.0.next_frame.get() + 1;
        self.0.next_frame.set(handle);
        self.0.frames.borrow_mut().push((handle, callback));
        handle
    }
    fn cancel_animation_frame(&self, handle: u32) {
        self.0
            .frames
            .borrow_mut()
            .retain(|(queued, _)| *queued != handle);
    }
    fn observe_mutations(&self, observe: bool) {
        let counter = if observe {
            &self.0.observe_calls
        } else {
            &self.0.disconnect_calls
        };
        counter.set(counter.get() + 1);
    }
    fn listen_for_visibility(&self, listen: bool) {
        let count = self.0.visibility_listeners.get();
        self.0.visibility_listeners.set(if listen {
            count + 1
        } else {
            count.saturating_sub(1)
        });
    }
}

impl LinkHost for FakeLinkHost {
    fn container(&self) -> Node {
        self.container_node()
    }
    fn add_listener(&self, listener: LinkListener) {
        self.0.listeners.borrow_mut().push(listener);
    }
    fn remove_listener(&self, listener: LinkListener) {
        let mut listeners = self.0.listeners.borrow_mut();
        if let Some(index) = listeners.iter().position(|held| *held == listener) {
            listeners.remove(index);
        }
    }
    fn show_hint(&self, _anchor: &Node, text: &str) {
        *self.0.hint.borrow_mut() = Some(text.to_string());
    }
    fn hide_hint(&self) {
        *self.0.hint.borrow_mut() = None;
    }
    fn remove_hint(&self) {
        *self.0.hint.borrow_mut() = None;
    }
    fn click_detached_anchor(&self, anchor: &Node) {
        self.0.clicked.borrow_mut().push(anchor.attr("href"));
    }
}

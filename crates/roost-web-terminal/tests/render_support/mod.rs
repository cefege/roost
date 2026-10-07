//! The fake DOM, frame builders and painted-history fault injector shared by
//! the renderer's tripwire suite, ported from
//! `apps/web/tests/helpers/cellRendererFakeDom.ts`. A test binary pulls it in
//! with `mod render_support;` (or `#[path = "render_support/mod.rs"]`).

#![allow(dead_code, unused_imports)]

pub mod fake_dom;

use std::sync::Arc;

use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, DEFAULT_COLOR, MouseTracking};
use roost_web_terminal::{CellGridRenderer, RenderElement};

pub use fake_dom::{CELL_PX, FakeEl, PAD_TOP, PANE_PX, ROW_PX};

/// The renderer every DOM test drives.
pub type FakeRenderer = CellGridRenderer<FakeEl>;

/// A fresh container and a renderer mounted in it.
pub fn mount() -> (FakeEl, FakeRenderer) {
    let container = FakeEl::container();
    let renderer = CellGridRenderer::new(&container).expect("the fake DOM creates every element");
    (container, renderer)
}

/// One row whose text is one default-coloured span, or no span when empty.
pub fn row(index: u32, text: &str) -> CellRow {
    let spans: Vec<CellSpan> = if text.is_empty() {
        Vec::new()
    } else {
        vec![CellSpan {
            text: text.to_string(),
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: u32::try_from(text.chars().count()).unwrap_or(u32::MAX),
            link_uri: None,
            link_key: None,
        }]
    };
    CellRow {
        index,
        mark: 0,
        spans: Arc::from(spans),
    }
}

/// `count` rows numbered from `from`, whose text is `r<index>`.
pub fn numbered_rows(count: u32, from: u32) -> Vec<CellRow> {
    (from..from + count)
        .map(|index| row(index, &format!("r{index}")))
        .collect()
}

fn base_frame(cols: u32, rows: u32, full: bool) -> CellGridFrame {
    CellGridFrame {
        stream_id: "test-stream:0".to_string(),
        grid_epoch: "test-grid:0".to_string(),
        cols,
        rows,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        full,
        viewport_rows: Vec::new(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq: 1,
    }
}

/// A viewport-only authoritative full with `scrollback_total` rows reserved.
pub fn full_frame(cols: u32, viewport: Vec<CellRow>, scrollback_total: u64) -> CellGridFrame {
    let rows = u32::try_from(viewport.len()).unwrap_or(u32::MAX);
    CellGridFrame {
        viewport_rows: viewport,
        scrollback_total,
        sb_base: scrollback_total,
        ..base_frame(cols, rows, true)
    }
}

/// A sparse delta carrying `viewport` dirty rows and `append` pushed history.
pub fn delta_frame(
    cols: u32,
    rows: u32,
    viewport: Vec<CellRow>,
    append: Vec<CellRow>,
    seq: u64,
) -> CellGridFrame {
    CellGridFrame {
        viewport_rows: viewport,
        scrollback_append: append,
        base_seq: seq - 1,
        seq,
        ..base_frame(cols, rows, false)
    }
}

/// `seed_held_history_to` with the history's own length as the total.
pub fn seed_held_history<E: RenderElement>(
    renderer: &mut CellGridRenderer<E>,
    cols: u32,
    viewport: Vec<CellRow>,
    history: Vec<CellRow>,
) -> bool {
    let total = history.len() as u64;
    seed_held_history_to(renderer, cols, viewport, history, total)
}

/// Seed painted history the way the stream does: a viewport-only full whose
/// base is the first history row, then a delta appending that history.
pub fn seed_held_history_to<E: RenderElement>(
    renderer: &mut CellGridRenderer<E>,
    cols: u32,
    viewport: Vec<CellRow>,
    history: Vec<CellRow>,
    total: u64,
) -> bool {
    let base_total = history.first().map_or(total, |row| u64::from(row.index));
    if !renderer.apply(&full_frame(cols, viewport.clone(), base_total)) {
        return false;
    }
    if history.is_empty() {
        return true;
    }
    let rows = u32::try_from(viewport.len()).unwrap_or(u32::MAX);
    let mut delta = delta_frame(cols, rows, viewport, history, 2);
    delta.scrollback_total = total;
    renderer.apply(&delta)
}

/// An alternate-screen full: a new semantic grid epoch.
pub fn alt_full_frame(cols: u32, viewport: Vec<CellRow>) -> CellGridFrame {
    CellGridFrame {
        grid_epoch: "test-grid:1".to_string(),
        alt_screen: true,
        ..full_frame(cols, viewport, 0)
    }
}

/// An alternate-screen delta.
pub fn alt_delta_frame(cols: u32, rows: u32, viewport: Vec<CellRow>, seq: u64) -> CellGridFrame {
    CellGridFrame {
        grid_epoch: "test-grid:1".to_string(),
        alt_screen: true,
        ..delta_frame(cols, rows, viewport, Vec::new(), seq)
    }
}

fn child_with_class(container: &FakeEl, class_name: &str) -> FakeEl {
    container
        .children()
        .into_iter()
        .find(|child| child.class_name() == class_name)
        .unwrap_or_else(|| panic!("no .{class_name} child"))
}

/// The history sheet.
pub fn sb_el(container: &FakeEl) -> FakeEl {
    child_with_class(container, "cell-scrollback")
}

/// The live grid host.
pub fn vp_el(container: &FakeEl) -> FakeEl {
    child_with_class(container, "cell-viewport")
}

/// The head spacer.
pub fn spacer_el(container: &FakeEl) -> FakeEl {
    child_with_class(container, "cell-sb-spacer")
}

/// Every child of every block and gap of the history sheet, flattened, so a
/// test asserts ROW identity rather than block packing.
pub fn sb_rows(scrollback: &FakeEl) -> Vec<FakeEl> {
    scrollback
        .children()
        .iter()
        .flat_map(FakeEl::children)
        .collect()
}

/// The viewport's `.cell-row` children.
pub fn vp_rows(container: &FakeEl) -> Vec<FakeEl> {
    vp_el(container)
        .children()
        .into_iter()
        .filter(|child| child.class_name() == "cell-row")
        .collect()
}

/// The painted history row claiming absolute `index`.
pub fn history_node(container: &FakeEl, index: u32) -> FakeEl {
    let wanted = index.to_string();
    sb_rows(&sb_el(container))
        .into_iter()
        .find(|element| element.attribute("data-row-index").as_deref() == Some(wanted.as_str()))
        .unwrap_or_else(|| panic!("no painted history row {index}"))
}

/// Paint a second node claiming an absolute index the DOM already holds —
/// exactly the duplicated-tail corruption, injected below the renderer.
pub fn inject_duplicate_history_node(container: &FakeEl, index: u32) {
    let original = history_node(container, index);
    let block = original.parent().expect("a painted row sits in a block");
    let clone = FakeEl::new("div");
    clone.set_class_name(&original.class_name());
    clone.set_attribute("data-row-index", &index.to_string());
    let span = FakeEl::new("span");
    span.set_text(&original.text_content());
    clone.append_child(&span);
    let position = block
        .children()
        .iter()
        .position(|child| *child == original)
        .expect("the original is a child of its block");
    block.splice_child(position + 1, &clone);
}

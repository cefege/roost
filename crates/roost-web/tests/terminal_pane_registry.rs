//! The session-keyed pane registry: the newest mount wins and a stale mount's
//! unregister cannot orphan its successor. It also pins the preview row pick.

use std::rc::Rc;
use std::sync::Arc;

use roost_protocol::cell::{CellRow, CellSpan};
use roost_web::components::terminal::pane_registry::{PaneRegistry, PaneSurface};
use roost_web::components::terminal::pane_surface::MAX_PREVIEW_ROWS;

struct FakeSurface {
    text: &'static str,
}

impl PaneSurface for FakeSurface {
    fn viewport_text(&self) -> Option<String> {
        Some(self.text.to_owned())
    }
    fn scrollback_text(&self, _max_rows: usize) -> Option<String> {
        Some(String::new())
    }
    fn preview_rows(&self) -> Option<Vec<CellRow>> {
        None
    }
}

fn surface(text: &'static str) -> Rc<dyn PaneSurface> {
    Rc::new(FakeSurface { text })
}

#[test]
fn the_newest_mount_wins_and_a_stale_unregister_cannot_orphan_it() {
    let panes = PaneRegistry::default();
    let first = panes.register("s", surface("first"));
    let second = panes.register("s", surface("second"));
    assert!(second > first);
    assert_eq!(panes.viewport_text("s").as_deref(), Some("second"));
    assert!(
        !panes.unregister("s", first),
        "the replaced mount owns nothing"
    );
    assert!(panes.unregister("s", second));
    assert_eq!(panes.viewport_text("s"), None);
}

fn row(index: u32, text: &str) -> CellRow {
    CellRow {
        index,
        mark: 0,
        spans: Arc::from(vec![CellSpan {
            text: text.to_owned(),
            fg: 0,
            bg: 0,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: u32::try_from(text.chars().count().max(1)).unwrap_or(1),
            link_uri: None,
            link_key: None,
        }]),
    }
}

#[test]
fn a_preview_shows_the_newest_non_blank_rows_oldest_first() {
    use roost_web::components::terminal::pane_surface::preview_rows_of;
    let frame = roost_protocol::cell::CellGridFrame {
        stream_id: "s".to_owned(),
        grid_epoch: "e".to_owned(),
        cols: 80,
        rows: 3,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: roost_protocol::cell::MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        kitty_keyboard_flags: 0,
        full: true,
        viewport_rows: vec![row(0, "top"), row(1, "   "), row(2, "prompt")],
        scrollback_rows: (0..30)
            .map(|index| row(index, &format!("h{index}")))
            .collect(),
        scrollback_append: Vec::new(),
        scrollback_total: 30,
        sb_base: 0,
        base_seq: 0,
        seq: 1,
        image_placements: Some(roost_protocol::cell::no_image_placements()),
    };
    let picked = preview_rows_of(&frame);
    assert_eq!(picked.len(), MAX_PREVIEW_ROWS);
    let text = |row: &CellRow| {
        row.spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
    };
    assert_eq!(text(picked.last().unwrap()), "prompt");
    assert_eq!(
        text(&picked[picked.len() - 2]),
        "top",
        "blank rows are skipped"
    );
    assert_eq!(text(&picked[0]), "h14");
}

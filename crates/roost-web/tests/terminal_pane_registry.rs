//! The session-keyed pane registry: the newest mount wins, a stale mount's
//! unregister cannot orphan its successor, counters survive remounts, and the
//! painted-marker search reads painted rows only. Ports the registry contract
//! of `apps/web/src/renderer/terminalPreview.ts` (`registerRenderer`) and the
//! preview row pick of `renderPreview`.

use std::rc::Rc;
use std::sync::Arc;

use roost_protocol::cell::{CellRow, CellSpan};
use roost_web::components::terminal::pane_registry::{
    PaintedLine, PaneRegistry, PaneSurface, find_marker,
};
use roost_web::components::terminal::pane_surface::{MAX_PREVIEW_ROWS, SurfaceProbe};
use roost_web_terminal::{PaintedRowText, RendererPaintPresentation, RendererPresentationSnapshot};

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
    fn painted_lines(&self) -> Option<Vec<PaintedLine>> {
        Some(vec![PaintedLine {
            row: 7,
            text: self.text.to_owned(),
            in_viewport: true,
        }])
    }
    fn probe(&self) -> Option<SurfaceProbe> {
        None
    }
    fn paint_presentation(&self, _row_limit: Option<usize>) -> Option<RendererPaintPresentation> {
        None
    }
    fn has_painted_scrollback_range(&self, _start: u32, _end: u32) -> bool {
        false
    }
    fn painted_scrollback_range(&self, _start: u32, _end: u32) -> Option<Vec<PaintedRowText>> {
        None
    }
    fn presentation_snapshot(&self) -> Option<RendererPresentationSnapshot> {
        None
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
    assert_eq!(panes.mount_id("s"), Some(second));
    assert!(panes.unregister("s", second));
    assert_eq!(panes.viewport_text("s"), None);
    assert!(panes.sessions().is_empty());
}

#[test]
fn backfill_counters_survive_a_remount() {
    let panes = PaneRegistry::default();
    let mount = panes.register("s", surface("x"));
    panes.note_backfill_request("s");
    panes.unregister("s", mount);
    panes.register("s", surface("y"));
    panes.note_backfill_request("s");
    assert_eq!(panes.counters("s").backfill_requests, 2);
    assert_eq!(panes.counters("never").backfill_requests, 0);
}

#[test]
fn a_painted_marker_is_found_by_row_and_character_column() {
    let panes = PaneRegistry::default();
    panes.register("s", surface("$ printf ROOST_SMOKE_ab12"));
    let hit = panes.find_painted_marker("s", "ROOST_SMOKE_ab12").unwrap();
    assert_eq!((hit.row, hit.column, hit.in_viewport), (7, 9, true));
    assert_eq!(panes.find_painted_marker("s", "absent"), None);
    assert_eq!(
        panes.find_painted_marker("s", ""),
        None,
        "an empty marker matches nothing"
    );
    let wide = [PaintedLine {
        row: 1,
        text: "é marker".to_owned(),
        in_viewport: false,
    }];
    assert_eq!(find_marker(&wide, "marker").map(|hit| hit.column), Some(2));
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

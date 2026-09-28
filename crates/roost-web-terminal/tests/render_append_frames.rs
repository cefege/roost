//! Frame admission on the append-only grid: the painted width is the worker's
//! `cols`, a delta before any full is refused, the alternate screen latches
//! `.alt-active`, and a viewport-only full reserves depth that explicit pages
//! fill in place. Ported from `apps/web/tests/renderer/cellRenderer.append.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    FakeEl, alt_delta_frame, alt_full_frame, delta_frame, full_frame, mount, row, sb_el, sb_rows,
    seed_held_history,
};
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::RenderElement;
use roost_web_terminal::presentation::RendererTerminalModeSnapshot;

fn has_alt_active(container: &FakeEl) -> bool {
    container
        .class_name()
        .split_whitespace()
        .any(|class| class == "alt-active")
}

fn painted_texts(scrollback: &FakeEl) -> Vec<String> {
    sb_rows(scrollback)
        .iter()
        .map(FakeEl::text_content)
        .collect()
}

#[test]
fn the_painted_width_is_pinned_to_the_workers_cols() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v0")], Vec::new());
    assert_eq!(container.style("--cell-cols").as_deref(), Some("80"));
    renderer.apply(&delta_frame(80, 1, vec![row(0, "x")], Vec::new(), 2));
    assert_eq!(container.style("--cell-cols").as_deref(), Some("80"));
}

#[test]
fn a_delta_before_any_full_frame_is_rejected_and_the_next_full_is_accepted() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    assert!(!renderer.apply(&delta_frame(
        80,
        1,
        vec![row(0, "x")],
        vec![row(0, "orphan")],
        1
    )));
    assert_eq!(renderer.canonical_frame_seq(), 0);
    assert!(scrollback.children().is_empty());
    assert!(seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "v0")],
        vec![row(0, "h0")]
    ));
    assert_eq!(scrollback.children().len(), 1);
}

#[test]
fn an_alt_screen_frame_sets_alt_active_and_leaving_alt_clears_it() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v0")], vec![row(0, "h0")]);
    assert!(!has_alt_active(&container));

    renderer.apply(&alt_full_frame(80, vec![row(0, "TUI")]));
    let alt = RendererTerminalModeSnapshot {
        alt_screen: true,
        cursor_keys_app: false,
        bracketed_paste: false,
    };
    let snapshot = renderer.presentation_snapshot();
    assert_eq!(
        (snapshot.canonical_mode, snapshot.reconciled_mode),
        (Some(alt), Some(alt))
    );
    assert!(has_alt_active(&container));

    renderer.apply(&alt_delta_frame(80, 1, vec![row(0, "TUI2")], 3));
    assert!(has_alt_active(&container));

    seed_held_history(&mut renderer, 80, vec![row(0, "back")], vec![row(0, "h0")]);
    assert!(!has_alt_active(&container));
}

#[test]
fn a_viewport_only_full_reserves_depth_and_explicit_pages_fill_the_seam() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    renderer.apply(&full_frame(80, vec![row(0, "v")], 4));
    assert!(sb_rows(&scrollback).is_empty());
    assert!(renderer.insert_history_page(&[row(2, "h2"), row(3, "h3")], false));
    let anchor = renderer.backfill_anchor().unwrap();
    assert_eq!(
        (anchor.sb_base, anchor.grid_epoch.as_str()),
        (2, "test-grid:0")
    );
    let newest_page = sb_rows(&scrollback);
    assert!(renderer.insert_history_page(&[row(0, "h0"), row(1, "h1")], false));
    let all = sb_rows(&scrollback);
    assert_eq!(painted_texts(&scrollback), ["h0", "h1", "h2", "h3"]);
    assert_eq!(all[2..], newest_page[..]);
    assert_eq!(renderer.backfill_anchor().unwrap().sb_base, 0);
    assert!(!renderer.insert_history_page(&[row(0, "stale")], false));
    assert_eq!(sb_rows(&scrollback).len(), 4);
}

#[test]
fn a_delta_after_explicit_backfill_keeps_appending_at_the_same_epoch() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    renderer.apply(&full_frame(80, vec![row(0, "v")], 2));
    assert!(renderer.insert_history_page(&[row(1, "h1")], false));
    renderer.apply(&CellGridFrame {
        scrollback_total: 3,
        ..delta_frame(80, 1, Vec::new(), vec![row(2, "h2")], 2)
    });
    assert_eq!(sb_rows(&scrollback).len(), 2);
    assert_eq!(renderer.backfill_anchor().unwrap().sb_base, 1);
    assert!(renderer.insert_history_page(&[row(0, "h0")], false));
    assert_eq!(painted_texts(&scrollback), ["h0", "h1", "h2"]);
}

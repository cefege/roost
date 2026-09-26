//! The residency pool's own accounting: what a version a snapshot cursor is
//! still walking costs, and what a refusal does to the numbers.
//!
//! Split from `terminal_screen_residency.rs` because that file is about what a
//! screen REPLICA does when the pool says no -- the degrade -- and this one is
//! about the pool's arithmetic underneath. A refusal is only safe to report as
//! a degrade if the refusal itself is exact, so the two are asserted apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::terminal_screen::residency::{SessionCharge, TerminalScreenResidency};
use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, MouseTracking};

const COLS: u32 = 80;
const ROWS: u32 = 24;
const ROW_SPANS: u64 = 1;

fn span() -> CellSpan {
    CellSpan {
        text: "$ ".to_owned(),
        fg: 0,
        bg: 0,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: 2,
        link_uri: None,
        link_key: None,
    }
}

fn frame(seq: u64) -> CellGridFrame {
    CellGridFrame {
        stream_id: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_owned(),
        grid_epoch: "grid-1".to_owned(),
        cols: COLS,
        rows: ROWS,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        full: true,
        viewport_rows: (0..ROWS)
            .map(|row| CellRow {
                index: row,
                spans: vec![span()].into(),
            })
            .collect(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq,
    }
}

#[test]
fn the_residency_pool_accounts_for_a_pinned_version_a_cursor_is_still_walking() {
    let mut pool = TerminalScreenResidency::new(100, 1_000);
    let mut charge = SessionCharge::default();
    let generation = install(&mut pool, &mut charge, frame(1), 24);

    assert!(pool.acquire_source_lease(&mut charge, generation));
    assert!(pool.acquire_source_lease(&mut charge, generation));
    assert_eq!(pool.usage().0, 24);
    // A replacement with the old version pinned charges BOTH, which is what
    // makes an uncharged resident full impossible.
    assert!(pool.replace(&mut charge, frame(2), 0, 24, ROW_SPANS));
    assert!(
        charge.pinned.is_some(),
        "the version a cursor holds stays pinned"
    );
    assert_eq!(
        pool.usage().0,
        48,
        "both versions are charged while both are reachable"
    );

    pool.release_source_lease(&mut charge, generation);
    assert_eq!(pool.usage().0, 48, "one lease is still held");
    pool.release_source_lease(&mut charge, generation);
    assert_eq!(
        pool.usage().0,
        24,
        "the last lease out returns the pinned version"
    );
    assert!(charge.pinned.is_none());
}

#[test]
fn the_pool_refuses_a_version_it_cannot_pay_for_and_changes_nothing() {
    let mut pool = TerminalScreenResidency::new(24, 1_000);
    let mut charge = SessionCharge::default();
    assert!(pool.replace(&mut charge, frame(1), 0, 24, ROW_SPANS));
    let before = pool.usage();

    let mut other = SessionCharge::default();
    assert!(
        !pool.replace(&mut other, frame(1), 0, 24, ROW_SPANS),
        "a full pool refuses the next session"
    );
    assert!(other.current().is_none(), "a refusal installs nothing");
    assert_eq!(
        pool.usage(),
        before,
        "a refusal does not move the accounting"
    );
}

fn install(
    pool: &mut TerminalScreenResidency,
    charge: &mut SessionCharge,
    value: CellGridFrame,
    rows: u64,
) -> u64 {
    assert!(pool.replace(charge, value, 0, rows, ROW_SPANS));
    charge
        .current()
        .expect("a replaced charge holds a cache")
        .generation
}

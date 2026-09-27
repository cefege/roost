//! Painted terminal links: how a click becomes an action, how a soft-wrapped
//! link is re-identified as one, and the scheduling rule that keeps a scan from
//! either deadlocking or replaying retained history.
//!
//! Test names are v2's, from `apps/web/tests/terminal-links.dom.test.ts` and
//! `apps/web/tests/renderer/terminal-links.activation.dom.test.ts`.

use roost_web_terminal::links::{
    DIRTY_ROW_LIMIT, LinkActivation, LinkActivationGesture, LinkArmedHold, LinkModifierKey,
    PaintedChild, PaintedLinkAttributes, PaintedRow, PressWithheld, ScanRequest, ScanSchedule,
    ScannedLink, activate_link, is_link_activation_gesture, is_worker_file_href, link_at_cell,
    region_links, withhold_press,
};
use roost_web_terminal::reader_intent::HoldChange;

/// The one URI the painted-link cases share, so a merge shows up as one link.
const FIRST_TARGET: &str = "https://ex.test/one";

/// v2's stub worker resolver: `/file/W/<path>[#L<line>]`.
fn stub_resolver(path: &str, line: Option<u32>, _authority: Option<&str>) -> Option<String> {
    let path = path.strip_prefix('/').unwrap_or(path);
    Some(match line {
        Some(line) => format!("/file/W/{path}#L{line}"),
        None => format!("/file/W/{path}"),
    })
}

/// No resolver at all, for the arms that must refuse a file target.
type NoResolver = fn(&str, Option<u32>, Option<&str>) -> Option<String>;

fn painted_link(key: &str, target: &str, columns: u32) -> PaintedChild {
    PaintedChild {
        columns,
        link: Some(PaintedLinkAttributes {
            is_terminal_link: true,
            key: Some(key.to_string()),
            target: Some(target.to_string()),
        }),
    }
}

fn row_with_links(children: Vec<PaintedChild>) -> PaintedRow {
    PaintedRow {
        has_links: true,
        children,
    }
}

fn producer_link(target: &str) -> PaintedLinkAttributes {
    PaintedLinkAttributes {
        is_terminal_link: true,
        key: Some("b\u{0}7".to_string()),
        target: Some(target.to_string()),
    }
}

/// Every painted half: `(row, first column, column past the last)`.
fn halves(links: &[ScannedLink]) -> Vec<(u32, u32, u32)> {
    links
        .iter()
        .flat_map(|link| link.halves.iter())
        .map(|half| (half.row, half.first_col, half.end_col))
        .collect()
}

fn gesture(button: i16, ctrl: bool, meta: bool) -> LinkActivationGesture {
    LinkActivationGesture {
        button,
        ctrl,
        meta,
        shift: false,
        alt: false,
    }
}

/// The action a `s/f.ts:9` producer link performs, so two tests agree on it.
fn file_activation() -> LinkActivation {
    LinkActivation::OpenWorkerFile {
        href: "/file/W/s/f.ts#L9".to_string(),
        display: "s/f.ts:9".to_string(),
    }
}

// ── a scanned link re-identifies across a soft wrap ───────────────────────

#[test]
fn a_soft_wrapped_link_whose_halves_land_in_two_rows_is_one_link() {
    // One run key per link, stamped on every painted half: a link longer than the
    // grid comes back from two rows as one entry. Text cannot do this.
    let lead = PaintedChild { columns: 4, link: None };
    let head = row_with_links(vec![lead, painted_link("b\u{0}7", FIRST_TARGET, 36)]);
    let tail = row_with_links(vec![painted_link("b\u{0}7", FIRST_TARGET, 12)]);
    let links = region_links(&[head, tail], 12);

    assert_eq!(links.len(), 1);
    assert_eq!(links[0].key, "b\u{0}7");
    assert_eq!(links[0].raw_target, FIRST_TARGET);
    assert_eq!(halves(&links), vec![(12, 5, 41), (13, 1, 13)]);
    // Either half finds the same link; the cell past it is a plain cell.
    for (row, col) in [(12, 40), (13, 1)] {
        let found = link_at_cell(&links, row, col).map(|link| link.key.as_str());
        assert_eq!(found, Some("b\u{0}7"), "row {row} col {col}");
    }
    assert!(link_at_cell(&links, 13, 13).is_none());
}

#[test]
fn two_links_with_identical_text_and_different_run_keys_stay_two_links() {
    // Per-cell identity: merging these would make one URI silently vanish.
    let other = "https://ex.test/two";
    let one = painted_link("b\u{0}0", FIRST_TARGET, 6);
    let painted = row_with_links(vec![one, painted_link("b\u{0}1", other, 6)]);
    let links = region_links(&[painted], 1);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].raw_target, FIRST_TARGET);
    assert_eq!(links[1].raw_target, other);
    assert_eq!(halves(&links), vec![(1, 1, 7), (1, 7, 13)]);
}

#[test]
fn a_row_without_the_link_marker_contributes_nothing_to_a_scan() {
    // The marker is the one attribute read that skips every held row.
    let child = painted_link("b\u{0}7", FIRST_TARGET, 12);
    let row = row_with_links(vec![child]);
    assert!(region_links(&[PaintedRow { has_links: false, ..row }], 1).is_empty());
}

// ── a scanned link becomes an action ──────────────────────────────────────

#[test]
fn a_producer_painted_file_target_routes_through_the_worker_resolver() {
    let attributes = producer_link("s/f.ts:9");
    assert_eq!(activate_link(&attributes, Some(stub_resolver)), Some(file_activation()));
    // No resolver: refused, not handed on with a null href, because the browser
    // has no route for a path and the app has no worker to ask.
    assert_eq!(activate_link::<NoResolver>(&attributes, None), None);
}

#[test]
fn a_bracketed_ipv6_authority_is_classified_and_activated_without_panicking() {
    // The defect this port already paid for once: a hand-rolled authority
    // parser sliced one byte past the authority and panicked on every bracketed
    // literal WITH a port. Activation delegates to the one classifier and never
    // re-derives the host, so the string survives verbatim.
    for target in ["http://[::1]:4103/x", "https://[fe80::1]:8443/a?b=c#d"] {
        let activation = activate_link(&producer_link(target), Some(stub_resolver));
        assert_eq!(
            activation,
            Some(LinkActivation::OpenExternal {
                href: target.to_string(),
                display: target.to_string(),
            }),
            "{target}"
        );
    }
}

#[test]
fn local_arming_persists_across_link_taps_while_physical_meta_remains_supported() {
    let attributes = producer_link("s/f.ts:9");
    let mut opened: Vec<String> = Vec::new();
    // A bare click never activates — it is the terminal's own text selection.
    // Local arming is pane state, so it opens every tap while it holds; once
    // disarmed, only the physical modifier opens it, and only once more.
    for (armed, modifier) in [
        (false, gesture(0, false, false)),
        (true, gesture(0, false, false)),
        (true, gesture(0, false, false)),
        (false, gesture(0, false, false)),
        (false, gesture(0, false, true)),
    ] {
        if !is_link_activation_gesture(&modifier, armed, LinkModifierKey::Meta) {
            continue;
        }
        if let Some(LinkActivation::OpenWorkerFile { href, .. }) =
            activate_link(&attributes, Some(stub_resolver))
        {
            opened.push(href);
        }
    }
    assert_eq!(opened.len(), 3);
    assert!(opened.iter().all(|href| href == "/file/W/s/f.ts#L9"), "{opened:?}");
    // A custom scheme is not a target at all, however it is activated.
    let refused = producer_link("vscode://file/s/f.ts");
    assert_eq!(activate_link(&refused, Some(stub_resolver)), None);
    assert_eq!(opened.len(), 3, "a refused scheme opens nothing");
}

#[test]
fn physical_ctrl_and_meta_gestures_remain_platform_specific() {
    let mac = LinkModifierKey::for_platform(true);
    let other = LinkModifierKey::for_platform(false);
    let meta = gesture(0, false, true);
    let ctrl = gesture(0, true, false);
    assert_eq!((mac, other), (LinkModifierKey::Meta, LinkModifierKey::Control));
    assert!(is_link_activation_gesture(&meta, false, mac));
    assert!(!is_link_activation_gesture(&ctrl, false, mac));
    assert!(is_link_activation_gesture(&ctrl, false, other));
    assert!(!is_link_activation_gesture(&meta, false, other));
    // A right click and a shifted click are never the gesture, on either.
    assert!(!is_link_activation_gesture(&gesture(2, true, false), false, other));
    let mut shifted = ctrl;
    shifted.shift = true;
    assert!(!is_link_activation_gesture(&shifted, false, other));
}

#[test]
fn only_a_route_this_build_minted_may_be_installed_on_an_internal_anchor() {
    for href in ["/file/W/s/f.ts", "/file/W/s/f.ts#L9"] {
        assert!(is_worker_file_href(href), "{href}");
    }
    let refused = [
        "/file/W/s/f.ts?x=1", "/file/W/s/f.ts#L0", "/file/W/s/f.ts#L09", "/file/W/",
        "/file/W/s/f.ts#section", "/file/W/", "/browse/W/s/f.ts", "https://ex.test/x",
    ];
    for href in refused {
        assert!(!is_worker_file_href(href), "{href}");
    }
}

#[test]
fn armed_and_physical_modifier_terminal_links_bypass_pty_bytes_while_bare_clicks_forward() {
    let mac = LinkModifierKey::Meta;
    let withheld = |over, modifier, armed| withhold_press(over, modifier, armed, mac, false);
    // The physical modifier over a link is the terminal's own gesture. A bare
    // click over the same link falls through, so a mouse-aware TUI keeps its
    // press; compact arming withholds it with no modifier at all.
    assert_eq!(
        withheld(true, &gesture(0, false, true), false),
        Some(PressWithheld::LinkActivation)
    );
    assert_eq!(withheld(true, &gesture(0, false, false), false), None);
    let armed = withheld(true, &gesture(0, false, false), true);
    assert_eq!(armed, Some(PressWithheld::LinkActivation));
    assert_eq!(withheld(false, &gesture(0, false, false), true), None);
    // The middle button is the deck's bring-to-front toggle, never the app's —
    // and it stays the deck's even over a link.
    let middle = withhold_press(false, &gesture(1, false, false), false, mac, true);
    assert_eq!(middle, Some(PressWithheld::DeckMiddleButton));
    let over = withhold_press(true, &gesture(1, false, false), true, mac, true);
    assert_eq!(over, Some(PressWithheld::DeckMiddleButton));
}

/// A hold that is armed AND has the pointer inside: the only state that paints.
fn armed_inside() -> LinkArmedHold {
    let mut hold = LinkArmedHold::default();
    hold.set_armed(true);
    hold.set_pointer_inside(true);
    hold
}

#[test]
fn releaseinteraction_clears_modifier_and_pointer_state_and_releases_a_hold_once() {
    let mut hold = armed_inside();
    assert!(hold.holding());
    assert_eq!(hold.release(), HoldChange::Released);
    // Releasing again changes nothing, and re-arming alone cannot reacquire the
    // hold: the pointer level was cleared with the modifier.
    assert_eq!(hold.release(), HoldChange::Unchanged);
    assert_eq!(hold.set_armed(true), HoldChange::Unchanged);
    assert!(!hold.holding());
}

#[test]
fn a_lost_modifier_keyup_is_healed_by_the_next_pointer_event_which_repaints() {
    let mut hold = armed_inside();
    // No keyup ever arrives; the next pointer event re-derives the level.
    assert_eq!(hold.set_armed(false), HoldChange::Released);
    assert!(!hold.holding());
}

#[test]
fn a_pointer_event_with_the_modifier_still_held_keeps_the_pane_held() {
    let mut hold = armed_inside();
    // Every pointer event re-derives the level in BOTH directions, so a modifier
    // that never sees its keyup still reads as held — which is the heal.
    assert_eq!(hold.set_armed(true), HoldChange::Unchanged);
    assert!(hold.holding());
}

#[test]
fn re_entering_the_pane_without_the_modifier_cannot_revive_the_hold() {
    let mut hold = armed_inside();
    assert_eq!(hold.set_pointer_inside(false), HoldChange::Released);
    assert_eq!(hold.set_armed(false), HoldChange::Unchanged);
    assert_eq!(hold.set_pointer_inside(true), HoldChange::Unchanged);
    assert!(!hold.holding());
}

// ── the scan schedule ─────────────────────────────────────────────────────

/// A schedule run through activation's two frames, with the tail scan done.
fn activated() -> ScanSchedule {
    let mut schedule = ScanSchedule::new();
    schedule.activate();
    schedule.fire();
    schedule.fire();
    schedule
}

#[test]
fn initial_activation_waits_for_paint_then_scans_only_the_current_tail() {
    let mut schedule = ScanSchedule::new();
    assert!(!schedule.is_active());
    // One frame is owed and it scans nothing: a pane that has not painted has
    // nothing to scan, and retained history must not be replayed as dirty rows.
    schedule.activate();
    assert!(schedule.needs_frame());
    assert_eq!(schedule.pending_scan(), None);
    // The paint lands; that frame only ARMS the tail scan.
    assert_eq!(schedule.fire(), ScanRequest::Idle);
    assert_eq!(schedule.pending_scan(), Some(ScanRequest::CurrentTail));
    // The tail scan runs, and nothing is owed afterwards.
    assert_eq!(schedule.fire(), ScanRequest::CurrentTail);
    assert!(!schedule.needs_frame());
    assert_eq!(schedule.pending_scan(), None);
}

#[test]
fn a_dropped_rAF_stuck_latch_is_recovered_by_visibilitychange() {
    let mut schedule = ScanSchedule::new();
    schedule.activate();
    // A browser that DROPPED the post-activation frame leaves the latch armed:
    // no mutation can queue a replacement behind a callback that will never
    // run. The work is armed, but only a live callback can carry it out.
    schedule.note_mutation(3);
    assert!(schedule.needs_frame());
    let armed = schedule.pending_scan();
    assert_eq!(armed, Some(ScanRequest::CurrentTail));
    schedule.page_visible();
    assert_eq!(schedule.pending_scan(), armed, "recovery re-arms, never duplicates");
    assert_eq!(schedule.fire(), ScanRequest::CurrentTail);
    schedule.note_mutation(3);
    assert_eq!(schedule.pending_scan(), Some(ScanRequest::Dirty { rows: 3 }));
}

#[test]
fn a_deferred_rAF_is_cancelled_by_visibilitychange_so_no_scan_runs_twice() {
    let mut schedule = ScanSchedule::new();
    schedule.activate();
    // The frame is merely DEFERRED, still owed. Recovery cancels the stale one
    // and arms a single replacement; a no-op cancel would scan twice.
    schedule.fire();
    assert!(schedule.needs_frame());
    schedule.page_visible();
    assert!(schedule.needs_frame());
    assert_eq!(schedule.fire(), ScanRequest::CurrentTail);
    assert!(!schedule.needs_frame());
    assert_eq!(schedule.pending_scan(), None);
}

#[test]
fn teardown_removes_the_visibility_listener_and_leaves_no_work() {
    let mut schedule = ScanSchedule::new();
    schedule.activate();
    schedule.deactivate();
    // After teardown a visibility flip must not schedule anything.
    schedule.page_visible();
    schedule.note_mutation(3);
    assert!(!schedule.needs_frame());
    assert_eq!(schedule.pending_scan(), None);
    assert!(!schedule.is_active());
}

#[test]
fn inactive_panes_release_scanner_and_modifier_work_then_restore_producer_link_activation() {
    let mut schedule = activated();
    let mut hold = LinkArmedHold::default();
    hold.set_armed(true);
    hold.set_pointer_inside(true);
    assert!(hold.holding());

    schedule.deactivate();
    assert!(!schedule.needs_frame());
    assert_eq!(hold.release(), HoldChange::Released);
    assert!(!hold.holding());

    schedule.activate();
    assert_eq!(schedule.fire(), ScanRequest::Idle);
    assert_eq!(schedule.fire(), ScanRequest::CurrentTail);
    // A producer-painted file link activates again once the pane is back.
    let restored = activate_link(&producer_link("s/f.ts:9"), Some(stub_resolver));
    assert_eq!(restored, Some(file_activation()));
}

#[test]
fn a_streaming_pane_bounds_its_scan_to_the_tail_instead_of_replaying_history() {
    let mut schedule = activated();
    schedule.note_mutation(1);
    assert_eq!(schedule.pending_scan(), Some(ScanRequest::Dirty { rows: 1 }));
    assert_eq!(schedule.fire(), ScanRequest::Dirty { rows: 1 });
    // A history-sized dirty set is what a streaming frame produces every tick.
    let mut streaming = activated();
    streaming.note_mutation(DIRTY_ROW_LIMIT + 1);
    assert_eq!(streaming.pending_scan(), Some(ScanRequest::CurrentTail));
    assert_eq!(streaming.fire(), ScanRequest::CurrentTail);
    streaming.note_mutation(1);
    assert_eq!(streaming.pending_scan(), Some(ScanRequest::Dirty { rows: 1 }));
}

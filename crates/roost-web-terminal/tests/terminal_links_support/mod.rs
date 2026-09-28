//! Shared fixtures for the painted-link suites: the painted row shapes, the
//! stub worker resolver, and the gesture and hold builders the activation cases
//! are written against.
//!
//! These live apart from the cases because a file holding both goes over the
//! cap, and because a scan fixture and an activation fixture are not the same
//! shape. Ported from `apps/web/tests/terminal-links.dom.test.ts`.
#![allow(dead_code)]

use roost_web_terminal::links::{
    LinkActivation, LinkActivationGesture, LinkArmedHold, PaintedChild, PaintedLinkAttributes,
    PaintedRow, ScannedLink,
};

pub const FIRST_TARGET: &str = "https://ex.test/one";

/// v2's stub worker resolver: `/file/W/<path>[#L<line>]`.
pub fn stub_resolver(path: &str, line: Option<u32>, _authority: Option<&str>) -> Option<String> {
    let path = path.strip_prefix('/').unwrap_or(path);
    Some(match line {
        Some(line) => format!("/file/W/{path}#L{line}"),
        None => format!("/file/W/{path}"),
    })
}

/// No resolver at all, for the arms that must refuse a file target.
pub type NoResolver = fn(&str, Option<u32>, Option<&str>) -> Option<String>;

pub fn painted_link(key: &str, target: &str, columns: u32) -> PaintedChild {
    PaintedChild {
        columns,
        link: Some(PaintedLinkAttributes {
            is_terminal_link: true,
            key: Some(key.to_string()),
            target: Some(target.to_string()),
        }),
    }
}

pub fn row_with_links(children: Vec<PaintedChild>) -> PaintedRow {
    PaintedRow {
        has_links: true,
        children,
    }
}

pub fn producer_link(target: &str) -> PaintedLinkAttributes {
    PaintedLinkAttributes {
        is_terminal_link: true,
        key: Some("b\u{0}7".to_string()),
        target: Some(target.to_string()),
    }
}

/// Every painted half: `(row, first column, column past the last)`.
pub fn halves(links: &[ScannedLink]) -> Vec<(u32, u32, u32)> {
    links
        .iter()
        .flat_map(|link| link.halves.iter())
        .map(|half| (half.row, half.first_col, half.end_col))
        .collect()
}

pub fn gesture(button: i16, ctrl: bool, meta: bool) -> LinkActivationGesture {
    LinkActivationGesture {
        button,
        ctrl,
        meta,
        shift: false,
        alt: false,
    }
}

/// The action a `s/f.ts:9` producer link performs, so two tests agree on it.
pub fn file_activation() -> LinkActivation {
    LinkActivation::OpenWorkerFile {
        href: "/file/W/s/f.ts#L9".to_string(),
        display: "s/f.ts:9".to_string(),
    }
}

// ── a scanned link re-identifies across a soft wrap ───────────────────────

/// A hold that is armed AND has the pointer inside: the only state that paints.
pub fn armed_inside() -> LinkArmedHold {
    let mut hold = LinkArmedHold::default();
    hold.set_armed(true);
    hold.set_pointer_inside(true);
    hold
}

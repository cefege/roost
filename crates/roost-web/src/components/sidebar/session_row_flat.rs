//! The flat-density body of a session row: the folder headline and age, the
//! program subtitle, the machine and short path, and the agent status /
//! offline / viewers line. Ports
//! `apps/web/src/components/sidebar/SessionRowFlat.tsx`; `SessionRow` renders it.

use dioxus::prelude::*;
use libero::components::{Flex, Text};

use super::row_chips::ServerChip;
use super::viewers_chip::ViewersChip;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::browse::folder_glyph::FolderGlyph;
use libero::sx::sx;

/// What the flat body shows, resolved by `SessionRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRowFacts {
    /// The session.
    pub session_id: String,
    /// `folder_headline`.
    pub headline: String,
    /// `program_subtitle`.
    pub subtitle: Option<String>,
    /// The relative age.
    pub rel_time: String,
    /// Whether the session is open (its age is since opening, else since closing).
    pub open: bool,
    /// The live cwd.
    pub cwd: String,
    /// The short cwd label.
    pub short_cwd: String,
    /// The machine's short label.
    pub server_label: String,
    /// Whether the machine is reachable.
    pub server_online: bool,
}

/// The flat body.
#[component]
pub fn SessionRowFlat(facts: SessionRowFacts) -> Element {
    let SessionRowFacts {
        session_id,
        headline,
        subtitle,
        rel_time,
        open,
        cwd,
        short_cwd,
        server_label,
        server_online,
    } = facts;
    let body_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex("1")
            .flex_direction("column")
            .gap("xs")
    });
    let line_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("baseline")
            .gap("sm")
    });
    let headline_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .font_size("sm")
            .color("ink")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
    });
    let secondary_sx = use_hook(|| sx().flex_shrink("0").font_size("xs").color("muted"));
    let path_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
            .font_family("monospace")
    });
    let details_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("sm")
            .font_size("xs")
            .color("muted")
    });
    let status_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let mono_sx = use_hook(|| {
        sx().min_width("0")
            .font_family("monospace")
            .font_size("xs")
            .color("muted")
    });
    let server_title = if server_online {
        format!("server: {server_label} — online")
    } else {
        format!("server: {server_label} — offline / not running")
    };
    rsx! {
        Flex { direction: "column", sx: body_sx,
            Flex { direction: "row", sx: line_sx,
                Text { sx: headline_sx, "data-row-text": "true",
                    "data-testid": "session-headline-{session_id}",
                    {headline}
                }
                Text { sx: secondary_sx.clone(), "data-row-text": "true",
                    "data-testid": "session-reltime-{session_id}",
                    title: if open { "Opened" } else { "Closed" },
                    {rel_time}
                }
            }
            if let Some(subtitle) = subtitle {
                Text { sx: mono_sx.clone(), "data-row-text": "true",
                    "data-testid": "session-subtitle-{session_id}",
                    title: subtitle.clone(),
                    {subtitle.clone()}
                }
            }
            Flex { direction: "row", sx: details_sx, "data-row-text": "true",
                ServerChip {
                    online: server_online,
                    label: server_label,
                    title: server_title,
                    test_id: format!("session-server-{session_id}"),
                }
                Flex { direction: "row", sx: path_sx,
                    "data-testid": "session-path-{session_id}",
                    title: cwd,
                    FolderGlyph { size: 11 }
                    Text { sx: mono_sx.clone(), "{short_cwd}" }
                }
            }
            Flex { direction: "row", sx: status_sx,
                AgentStatusIndicator { session_id: session_id.clone() }
                crate::components::terminal_bell_mark::TerminalBellMark { session_id: session_id.clone() }
                crate::components::terminal_signal_marks::TerminalProgressMark { session_id: session_id.clone() }
                crate::components::terminal_signal_marks::TerminalUserVarBadges { session_id: session_id.clone() }
                if !server_online {
                    Text { sx: secondary_sx.clone(),
                        "data-stage": "offline",
                        "data-testid": "session-offline-{session_id}",
                        "offline"
                    }
                }
                ViewersChip { session_id: session_id.clone() }
            }
        }
    }
}

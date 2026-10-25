//! Supporting metadata beneath a terminal's Sidenav link.
//! `SessionRow` renders its headline and age in the destination control; this
//! body keeps machine/path, agent signals and viewers outside that link.

use dioxus::prelude::*;

use super::row_chips::ServerChip;
use super::viewers_chip::ViewersChip;
use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::browse::folder_glyph::FolderGlyph;

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
        cwd,
        short_cwd,
        server_label,
        server_online,
        ..
    } = facts;
    let server_title = if server_online {
        format!("server: {server_label} — online")
    } else {
        format!("server: {server_label} — offline / not running")
    };
    rsx! {
        span { class: "flex min-w-0 flex-col gap-1",
            span { class: "flex min-w-0 items-center gap-2 text-xs text-muted-foreground",
                ServerChip {
                    online: server_online,
                    label: server_label,
                    title: server_title,
                    test_id: format!("session-server-{session_id}"),
                }
                span {
                    class: "flex min-w-0 items-center gap-1 font-mono",
                    "data-testid": "session-path-{session_id}",
                    title: cwd,
                    FolderGlyph { size: 11, class: "shrink-0" }
                    span { class: "min-w-0 truncate", {short_cwd} }
                }
            }
            span { class: "flex flex-wrap items-center gap-1.5",
                AgentStatusIndicator { session_id: session_id.clone() }
                crate::components::terminal_bell_mark::TerminalBellMark { session_id: session_id.clone() }
                crate::components::terminal_signal_marks::TerminalProgressMark { session_id: session_id.clone() }
                crate::components::terminal_signal_marks::TerminalUserVarBadges { session_id: session_id.clone() }
                if !server_online {
                    span {
                        class: "text-xs text-muted-foreground",
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

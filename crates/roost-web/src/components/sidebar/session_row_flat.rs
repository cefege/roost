//! The flat-density body of a session row: the folder headline and age, the
//! program subtitle, the machine and short path, and the agent status /
//! offline / viewers line. Ports
//! `apps/web/src/components/sidebar/SessionRowFlat.tsx`; `SessionRow` renders it.

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
        headline,
        subtitle,
        rel_time,
        open,
        cwd,
        short_cwd,
        server_label,
        server_online,
    } = facts;
    let server_title = if server_online {
        format!("server: {server_label} — online")
    } else {
        format!("server: {server_label} — offline / not running")
    };
    rsx! {
        span { class: "df-flat-body",
            span { class: "df-flat-top",
                span { class: "df-label df-flat-headline", "data-testid": "session-headline-{session_id}", {headline} }
                span {
                    class: "df-flat-time",
                    "data-testid": "session-reltime-{session_id}",
                    title: if open { "Opened" } else { "Closed" },
                    {rel_time}
                }
            }
            if let Some(subtitle) = subtitle {
                span {
                    class: "df-flat-subtitle",
                    "data-testid": "session-subtitle-{session_id}",
                    title: subtitle.clone(),
                    {subtitle.clone()}
                }
            }
            span { class: "df-flat-supporting",
                ServerChip {
                    online: server_online,
                    label: server_label,
                    title: server_title,
                    test_id: format!("session-server-{session_id}"),
                }
                span { class: "df-flat-path", "data-testid": "session-path-{session_id}", title: cwd,
                    FolderGlyph { size: 11, class: "df-flat-folder-icon" }
                    span { class: "df-flat-path-text", {short_cwd} }
                }
            }
            span { class: "df-flat-activity",
                AgentStatusIndicator { session_id: session_id.clone() }
                if !server_online {
                    span { class: "df-stage-text", "data-stage": "offline", "data-testid": "session-offline-{session_id}", "offline" }
                }
                ViewersChip { session_id: session_id.clone() }
            }
        }
    }
}

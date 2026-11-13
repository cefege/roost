//! Canonical OMP journal forks and their authenticated HTML placement.
//! Worker admission binds the durable parent session to its live actor.
//! Coordinator placement replaces journal identities with conversation IDs.

use serde::{Deserialize, Serialize};

/// HTML placement; the source command parser aliases tab to window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPlacement {
    Pane,
    Window,
}

/// Source-created, flushed child journal, never a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkOpen {
    pub session_id: String,
    pub parent_session_id: String,
    pub placement: ForkPlacement,
}

impl ForkOpen {
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 512 {
            return None;
        }
        let request: Self = serde_json::from_str(value).ok()?;
        (session_uuid(&request.session_id)
            && session_uuid(&request.parent_session_id)
            && request.session_id != request.parent_session_id)
            .then_some(request)
    }
}

/// Coordinator-issued child binding delivered only to the admitted browser view.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkTarget {
    pub conversation_id: String,
    pub parent_conversation_id: String,
    pub placement: ForkPlacement,
}

impl ForkTarget {
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 512 {
            return None;
        }
        let target: Self = serde_json::from_str(value).ok()?;
        (conversation_id(&target.conversation_id)
            && conversation_id(&target.parent_conversation_id)
            && target.conversation_id != target.parent_conversation_id)
            .then_some(target)
    }
}

fn session_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn conversation_id(value: &str) -> bool {
    value.starts_with("c-")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_requires_distinct_journal_ids_and_never_accepts_paths() {
        let parent = "019bab01-0000-7000-8000-000000000001";
        let child = "019bab01-0000-7000-8000-000000000002";
        assert!(
            ForkOpen::parse(&format!(
                r#"{{"sessionId":"{child}","parentSessionId":"{parent}","placement":"pane"}}"#
            ))
            .is_some()
        );
        for session in [parent, "/tmp/child.jsonl", "../child", ""] {
            assert!(
                ForkOpen::parse(&format!(
                    r#"{{"sessionId":"{session}","parentSessionId":"{parent}","placement":"pane"}}"#
                ))
                .is_none()
            );
        }
        assert!(
            ForkOpen::parse(&format!(
                r#"{{"sessionId":"{child}","parentSessionId":"{parent}","placement":"tab"}}"#
            ))
            .is_none()
        );
    }
}

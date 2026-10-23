//! Dispatch-time checks for model-issued calls after read-only mode changes.
//! `tool_round` applies these before native worker and harness side effects.
//! The accepted LSP actions share the same list used to construct read-only tools.

use roost_protocol::wire::agent_chat::{LspArgs, TOOL_LSP};

use crate::toolset::{LSP_READ_ACTIONS, is_read_only};

pub(crate) fn allows(name: &str, args: &str) -> bool {
    is_read_only(name) && (name != TOOL_LSP || lsp_action_allowed(args))
}

pub(crate) fn lsp_action_allowed(args: &str) -> bool {
    serde_json::from_str::<LspArgs>(args)
        .ok()
        .and_then(|parsed| {
            serde_json::to_value(parsed.action)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
        })
        .is_some_and(|action| LSP_READ_ACTIONS.contains(&action.as_str()))
}

#[cfg(test)]
mod tests {
    use super::allows;
    use crate::toolset::{TOOL_TASK, TOOL_TODO};
    use roost_protocol::wire::agent_chat::{
        TOOL_BASH, TOOL_EDIT, TOOL_JOB, TOOL_LSP, TOOL_READ, TOOL_WRITE,
    };

    #[test]
    fn read_only_dispatch_rejects_mutations_and_lsp_rename() {
        assert!(!allows(TOOL_BASH, "{}"));
        assert!(!allows(TOOL_JOB, "{}"));
        assert!(!allows(TOOL_EDIT, "{}"));
        assert!(!allows(TOOL_WRITE, "{}"));
        assert!(!allows(TOOL_TASK, "{}"));
        assert!(!allows(TOOL_TODO, "{}"));
        assert!(!allows(TOOL_LSP, r#"{"action":"rename"}"#));
        assert!(allows(TOOL_READ, "{}"));
        assert!(allows(TOOL_LSP, r#"{"action":"diagnostics"}"#));
    }
}

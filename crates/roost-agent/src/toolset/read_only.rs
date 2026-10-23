//! The model-facing tool surface for conversations configured read-only.
//! `toolset::toolset_for` selects it independently of normal and Plan modes.
//! Its worker tools use protocol schemas; its harness tools only read external state.

use super::*;

pub(super) fn read_only_toolset(find_available: bool) -> Toolset {
    let mut specs: Vec<ToolSpec> = worker_tool_specs(true)
        .into_iter()
        .filter(|spec| is_read_only(&spec.name))
        .map(|spec| ToolSpec {
            name: spec.name,
            description: spec.description,
            parameters: spec.parameters,
        })
        .collect();
    if find_available {
        specs.push(find_spec());
    }
    specs.push(crate::web_tools::fetch_spec());
    specs.push(crate::web_tools::web_search_spec());
    let specs = specs
        .into_iter()
        .map(|mut spec| {
            spec.parameters = with_intent_field(spec.parameters);
            spec
        })
        .collect();
    Toolset {
        specs,
        lsp_read_only: true,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn read_only_toolset_contains_only_read_capabilities() {
        let tools = read_only_toolset(true);
        assert_eq!(
            tools.names().iter().map(String::as_str).collect::<Vec<_>>(),
            [
                TOOL_READ,
                TOOL_GREP,
                TOOL_GLOB,
                TOOL_LSP,
                TOOL_FIND,
                TOOL_FETCH,
                TOOL_WEB_SEARCH
            ]
        );
        assert!(tools.lsp_read_only);
        assert!(!tools.offers(TOOL_BASH));
        assert!(!tools.offers(TOOL_JOB));
        assert!(!tools.offers(TOOL_WRITE));
        assert!(!tools.offers(TOOL_EDIT));
        assert!(!tools.offers(TOOL_TASK));
        assert!(!tools.offers(TOOL_TODO));
        assert!(!tools.offers(TOOL_GOAL));
        assert!(!tools.offers(TOOL_MESSAGE));
    }
}

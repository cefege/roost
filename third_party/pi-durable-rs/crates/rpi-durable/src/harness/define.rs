//! Identity/typed constructors of the extension surface
//! (port of `packages/durable/src/harness/define.ts` @ `abe508e1b`).
//!
//! Upstream these are generic identity functions that only influence TypeScript
//! inference. The Rust port keeps them as thin, typed constructors so extension
//! authors can be explicit without a separate literal type.

use std::sync::Arc;

use crate::harness::types::{
    Extension, HookHandlers, HookRegistration, PromptRender, PromptSection, TaskDefinition,
    ToolRegistration, Wrap,
};

/// Identity function that types an extension.
pub fn define_extension(extension: Extension) -> Extension {
    extension
}

/// Identity function that types a tool.
pub fn define_tool(tool: ToolRegistration) -> ToolRegistration {
    tool
}

/// A prompt section; tagged unless `tag` is `Some(false)`.
pub fn section(key: &str, render: PromptRender, tag: Option<bool>) -> PromptSection {
    PromptSection {
        key: key.to_string(),
        render,
        tag: tag.unwrap_or(true),
    }
}

/// Hook handlers for tasks with `task`'s name.
pub fn hook(task: &dyn TaskDefinition, handlers: HookHandlers) -> HookRegistration {
    HookRegistration {
        task: task.name().to_string(),
        handlers,
    }
}

/// Wrap the tool named like `tool` wherever the wrapping extension is selected.
pub fn wrap_tool(
    tool: &ToolRegistration,
    wrapper: Arc<dyn Fn(ToolRegistration) -> ToolRegistration + Send + Sync>,
) -> Wrap {
    Wrap::Tool {
        tool: tool.name().to_string(),
        wrap: wrapper,
    }
}

/// Wrap the section `key` wherever the wrapping extension is selected.
pub fn wrap_section(
    key: &str,
    wrapper: Arc<dyn Fn(PromptSection) -> PromptSection + Send + Sync>,
) -> Wrap {
    Wrap::Section {
        key: key.to_string(),
        wrap: wrapper,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::context::Context;
    use crate::harness::types::{PromptInput, PromptRenderFuture};

    fn render() -> PromptRender {
        Arc::new(
            |_input: PromptInput, _context: Context| -> PromptRenderFuture {
                Box::pin(async { Some("body".to_string()) })
            },
        )
    }

    #[test]
    fn section_defaults_to_tagged() {
        let tagged = section("alpha", render(), None);
        assert!(tagged.tag);
        let raw = section("beta", render(), Some(false));
        assert!(!raw.tag);
        assert_eq!(raw.key, "beta");
    }

    #[test]
    fn wrap_section_targets_its_key() {
        let wrap = wrap_section("alpha", Arc::new(|section: PromptSection| section));
        match wrap {
            Wrap::Section { key, .. } => assert_eq!(key, "alpha"),
            Wrap::Tool { .. } => panic!("expected a section wrap"),
        }
    }
}

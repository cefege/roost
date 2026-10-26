//! Port of `packages/ai/src/utils/transcript.ts` @ pin `19451accd`
//! (#9548, 9e05370b2).
//!
//! Replay and normalization helpers for the transcript's system messages:
//! the leading message is the system prompt, later ones patch it
//! (`content` appends, `sections` replace by name / `null` removes,
//! `toolsAdded`/`toolsRemoved` change the tool set). Replaying every system
//! message in order yields the current prompt and tools.
//!
//! Intentional differences vs the TS original:
//! - [`TranscriptContext`] is branded with a private field instead of a
//!   unique symbol (see `types.rs`).
//! - `declarations_equal` serializes both sides with `serde_json` and
//!   compares the strings; `to_tool_declaration`'s canonical rebuild (same
//!   field order, `constrained_sampling` omitted when absent) makes this
//!   exact, matching the upstream JSON round-trip trick without a deep-equal
//!   dependency.
//! - Section maps are `serde_json::Map<String, Value>` (insertion-ordered
//!   via `preserve_order`), with `Value::Null` playing TS `null`.

use crate::types::{Context, Message, SystemMessage, Tool, ToolReference, TranscriptContext};

/// `createInitialSystemMessage` (transcript.ts:8-24): build the leading
/// system message for a prompt and tool set. Returns `None` when both are
/// empty, so an empty transcript stays empty.
pub fn create_initial_system_message(
    system_prompt: Option<&str>,
    tools: Option<&[Tool]>,
) -> Option<SystemMessage> {
    let has_system_prompt = system_prompt.is_some_and(|prompt| !prompt.is_empty());
    let has_tools = tools.is_some_and(|tools| !tools.is_empty());
    if !has_system_prompt && !has_tools {
        return None;
    }
    Some(SystemMessage {
        role: Default::default(),
        content: crate::types::SystemContent::Text(system_prompt.unwrap_or("").to_owned()),
        sections: None,
        tools_added: has_tools.then(|| tools.unwrap_or_default().to_vec()),
        tools_removed: None,
        timestamp: 0,
    })
}

/// `normalizeContext` (transcript.ts:26-33): fold `Context.system_prompt` and
/// `Context.tools` into a leading system message. This is the only entry
/// point that produces a [`TranscriptContext`]; every provider-facing
/// function expects the result.
pub fn normalize_context(context: &Context) -> TranscriptContext {
    let initial_message =
        create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    let mut messages = Vec::with_capacity(context.messages.len() + 1);
    if let Some(initial) = initial_message {
        messages.push(Message::System(initial));
    }
    messages.extend(context.messages.iter().cloned());
    crate::types::make_transcript_context(messages)
}

/// `getInitialSystemMessage` (transcript.ts:40-43): the leading system
/// message, if the transcript starts with one.
pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
    match messages.first() {
        Some(Message::System(system)) => Some(system),
        _ => None,
    }
}

/// `withoutInitialSystemMessage` (transcript.ts:46-48): drop the leading
/// system message for APIs that carry the prompt outside the message list.
pub fn without_initial_system_message(messages: &[Message]) -> &[Message] {
    match messages.first() {
        Some(Message::System(_)) => &messages[1..],
        _ => messages,
    }
}

/// `getCurrentTools` (transcript.ts:50-58): resolve the tools available
/// after applying every transcript delta in order.
pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
    let mut tools: Vec<(String, Tool)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        if let Some(removed) = &system.tools_removed {
            for tool in removed {
                if let Some(position) = index.remove(&tool.name) {
                    // Order-preserving removal (JS `Map.delete` keeps the
                    // survivors' insertion order; `swap_remove` would move
                    // the tail into the hole and scramble it).
                    tools.remove(position);
                    for slot in index.values_mut() {
                        if *slot > position {
                            *slot -= 1;
                        }
                    }
                }
            }
        }
        if let Some(added) = &system.tools_added {
            for tool in added {
                match index.get(&tool.name) {
                    Some(&position) => tools[position].1 = tool.clone(),
                    None => {
                        index.insert(tool.name.clone(), tools.len());
                        tools.push((tool.name.clone(), tool.clone()));
                    }
                }
            }
        }
    }
    tools.into_iter().map(|(_, tool)| tool).collect()
}

/// `getCurrentSystemMessage` (transcript.ts:66-94): replay every system
/// message into one leading system message holding the current prompt and
/// tools. Later `content` is appended to the base prompt, `sections` are
/// patched by name, and tools are resolved with [`get_current_tools`].
pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut section_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut timestamp: Option<i64> = None;
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        if timestamp.is_none() {
            timestamp = Some(system.timestamp);
        }
        let text = crate::utils::text::content_text_system(&system.content, "\n");
        if !text.is_empty() {
            content.push(text);
        }
        for (name, value) in system.iter_sections() {
            match value {
                None => {
                    if let Some(position) = section_index.remove(name) {
                        // Order-preserving, like the tool removal above.
                        sections.remove(position);
                        for slot in section_index.values_mut() {
                            if *slot > position {
                                *slot -= 1;
                            }
                        }
                    }
                }
                Some(value) => match section_index.get(name) {
                    Some(&position) => sections[position].1 = value.to_owned(),
                    None => {
                        section_index.insert(name.to_owned(), sections.len());
                        sections.push((name.to_owned(), value.to_owned()));
                    }
                },
            }
        }
    }
    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }
    Some(SystemMessage {
        role: Default::default(),
        content: crate::types::SystemContent::Text(content.join("\n\n")),
        sections: (!sections.is_empty()).then(|| {
            let mut map = serde_json::Map::new();
            for (name, value) in sections {
                map.insert(name, serde_json::Value::String(value));
            }
            map
        }),
        tools_added: (!tools.is_empty()).then_some(tools),
        tools_removed: None,
        timestamp: timestamp.unwrap_or(0),
    })
}

/// `getCurrentSystemPrompt` (transcript.ts:96-99): render the current system
/// prompt text after replaying every system message.
pub fn get_current_system_prompt(messages: &[Message]) -> String {
    match get_current_system_message(messages) {
        Some(message) => crate::utils::text::get_system_message_text(&message),
        None => String::new(),
    }
}

/// `collapseSystemMessages` (transcript.ts:101-106): rebuild the transcript
/// for APIs without mid-conversation system messages — the replayed system
/// message leads, and every later system message is dropped.
pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    let head = get_current_system_message(&context.messages);
    let mut messages: Vec<Message> = Vec::with_capacity(context.messages.len());
    if let Some(head) = head {
        messages.push(Message::System(head));
    }
    messages.extend(
        context
            .messages
            .iter()
            .filter(|message| !matches!(message, Message::System(_)))
            .cloned(),
    );
    crate::types::make_transcript_context(messages)
}

/// `resolveTranscript` (transcript.ts:108-112): keep later system messages
/// in place when the model accepts them; otherwise collapse them.
pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: bool,
) -> TranscriptContext {
    if supports_mid_convo_system_messages {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

/// `toToolDeclaration` (transcript.ts:114-122): strip executable and
/// display-only fields from a tool before transcript comparison or
/// persistence. The canonical rebuild (field order fixed,
/// `constrained_sampling` omitted when absent) mirrors the upstream JSON
/// round-trip: comparing serialized declarations is exact.
pub fn to_tool_declaration(tool: &Tool) -> Tool {
    Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        constrained_sampling: tool.constrained_sampling.clone(),
    }
}

/// `declarationsEqual` (transcript.ts:130-134): whether two tools declare
/// the same interface to the model.
pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    serde_json::to_string(&to_tool_declaration(left)).ok()
        == serde_json::to_string(&to_tool_declaration(right)).ok()
}

/// `ToolStateChanges` (transcript.ts:136-139).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolStateChanges {
    pub tools_added: Vec<Tool>,
    pub tools_removed: Vec<ToolReference>,
}

/// `getToolStateChanges` (transcript.ts:141-161): compare two complete tool
/// states. A changed definition is a removal followed by an addition.
pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
    let previous_tools: std::collections::HashMap<&str, &Tool> = previous
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let current_tools: std::collections::HashMap<&str, &Tool> = current
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    ToolStateChanges {
        tools_added: current
            .iter()
            .filter(|tool| match previous_tools.get(tool.name.as_str()) {
                None => true,
                Some(previous_tool) => !declarations_equal(previous_tool, tool),
            })
            .map(to_tool_declaration)
            .collect(),
        tools_removed: previous
            .iter()
            .filter(|tool| match current_tools.get(tool.name.as_str()) {
                None => true,
                Some(current_tool) => !declarations_equal(tool, current_tool),
            })
            .map(|tool| ToolReference {
                name: tool.name.clone(),
            })
            .collect(),
    }
}

/// `getDeclaredTools` (transcript.ts:163-171): every definition referenced by
/// transcript tool state, in first-declaration order.
pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
    let mut definitions: Vec<(String, Tool)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        if let Some(added) = &system.tools_added {
            for tool in added {
                match index.get(&tool.name) {
                    Some(&position) => definitions[position].1 = tool.clone(),
                    None => {
                        index.insert(tool.name.clone(), definitions.len());
                        definitions.push((tool.name.clone(), tool.clone()));
                    }
                }
            }
        }
    }
    definitions.into_iter().map(|(_, tool)| tool).collect()
}

/// `hasToolRedefinitions` (transcript.ts:173-188): whether a tool name was
/// declared twice with different definitions. A transport that can only
/// reference previously declared tools by name cannot replay such a history.
///
/// Deprecated (b271b0a52): no built-in transport needs this anymore —
/// Anthropic expresses redefinitions with inline `tool_definition` blocks.
/// Kept for API compatibility; will be removed in a future release.
#[deprecated(note = "no built-in transport needs this anymore (b271b0a52)")]
pub fn has_tool_redefinitions(messages: &[Message]) -> bool {
    let mut declared: std::collections::HashMap<String, Tool> = std::collections::HashMap::new();
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        if let Some(added) = &system.tools_added {
            for tool in added {
                if let Some(previous) = declared.get(&tool.name)
                    && !declarations_equal(previous, tool)
                {
                    return true;
                }
                declared.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    false
}

/// `hasNonAdditiveToolChanges` (transcript.ts:190-205): whether tool history
/// contains a removal or same-name redeclaration that an addition-only
/// transport cannot replay.
pub fn has_non_additive_tool_changes(messages: &[Message]) -> bool {
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        if system
            .tools_removed
            .as_ref()
            .is_some_and(|removed| !removed.is_empty())
        {
            return true;
        }
        if let Some(added) = &system.tools_added {
            for tool in added {
                if !declared.insert(tool.name.clone()) {
                    return true;
                }
            }
        }
    }
    false
}

/// `TranscriptTools` (transcript.ts:207-216).
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptTools {
    /// Tools sent in the top-level request field.
    pub request_tools: Vec<Tool>,
    /// Whether later system messages carry their own `toolsAdded` as
    /// in-place additions. When false, `request_tools` already holds the
    /// complete current tool set.
    pub anchors_additions: bool,
}

/// `resolveTranscriptTools` (transcript.ts:218-231): split tool declarations
/// between the top-level request field and in-place additions. Transports
/// that can anchor additions at a system message keep the initial tools at
/// the top and load later ones where they appear; that only works when no
/// tool was removed or redeclared, so everything else sends the current
/// tool list.
pub fn resolve_transcript_tools(
    messages: &[Message],
    supports_tool_additions: bool,
) -> TranscriptTools {
    let anchors_additions = supports_tool_additions && !has_non_additive_tool_changes(messages);
    TranscriptTools {
        request_tools: if anchors_additions {
            get_initial_system_message(messages)
                .and_then(|message| message.tools_added.clone())
                .unwrap_or_default()
        } else {
            get_current_tools(messages)
        },
        anchors_additions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({}),
            constrained_sampling: None,
        }
    }

    fn system(content: &str, timestamp: i64) -> Message {
        Message::System(SystemMessage {
            role: Default::default(),
            content: crate::types::SystemContent::Text(content.to_owned()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp,
        })
    }

    #[test]
    fn create_initial_system_message_omits_empty() {
        assert!(create_initial_system_message(None, None).is_none());
        assert!(create_initial_system_message(Some(""), None).is_none());
        let message = create_initial_system_message(Some("p"), Some(&[tool("a")])).unwrap();
        assert_eq!(
            crate::utils::text::content_text_system(&message.content, "\n"),
            "p"
        );
        assert_eq!(message.tools_added.as_deref(), Some(&[tool("a")][..]));
        assert_eq!(message.timestamp, 0);
    }

    #[test]
    fn normalize_context_folds_prompt_and_tools() {
        let context = Context {
            system_prompt: Some("p".to_owned()),
            messages: vec![system("", 1)],
            tools: Some(vec![tool("a")]),
        };
        let transcript = normalize_context(&context);
        assert_eq!(transcript.messages.len(), 2);
        assert!(matches!(transcript.messages[0], Message::System(_)));
        // Empty transcript stays empty.
        let empty = normalize_context(&Context::default());
        assert!(empty.messages.is_empty());
    }

    #[test]
    fn replays_content_sections_and_tools() {
        let mut leading = match system("base", 10) {
            Message::System(mut s) => {
                let mut sections = serde_json::Map::new();
                sections.insert("a".to_owned(), serde_json::json!("<a>1</a>"));
                sections.insert("b".to_owned(), serde_json::json!("<b>1</b>"));
                s.sections = Some(sections);
                s.tools_added = Some(vec![tool("first")]);
                s
            }
            _ => unreachable!(),
        };
        leading.timestamp = 10;
        let mut later = match system("", 14) {
            Message::System(mut s) => {
                let mut sections = serde_json::Map::new();
                sections.insert("a".to_owned(), serde_json::json!("<a>2</a>"));
                sections.insert("b".to_owned(), serde_json::Value::Null);
                sections.insert("c".to_owned(), serde_json::json!("<c>1</c>"));
                s.sections = Some(sections);
                s.tools_removed = Some(vec![ToolReference {
                    name: "first".to_owned(),
                }]);
                s.tools_added = Some(vec![tool("second")]);
                s
            }
            _ => unreachable!(),
        };
        later.timestamp = 14;
        let messages = vec![
            Message::System(leading),
            Message::System(SystemMessage {
                role: Default::default(),
                content: crate::types::SystemContent::Text("also do this".to_owned()),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: 12,
            }),
            Message::System(later),
        ];
        let current = get_current_system_message(&messages).unwrap();
        assert_eq!(
            crate::utils::text::content_text_system(&current.content, "\n"),
            "base\n\nalso do this"
        );
        let section_names: Vec<&str> = current.iter_sections().map(|(name, _)| name).collect();
        assert_eq!(section_names, vec!["a", "c"]);
        assert_eq!(current.section("a"), Some(Some("<a>2</a>")));
        assert_eq!(current.tools_added.as_deref(), Some(&[tool("second")][..]));
        assert_eq!(current.timestamp, 10);
        assert_eq!(
            get_current_system_prompt(&messages),
            "base\n\nalso do this\n\n<a>2</a>\n\n<c>1</c>"
        );
    }

    #[test]
    fn collapse_keeps_only_non_system_after_head() {
        let context = normalize_context(&Context {
            system_prompt: Some("p".to_owned()),
            messages: vec![system("later", 2)],
            tools: Some(vec![tool("a")]),
        });
        let collapsed = collapse_system_messages(&context);
        let roles: Vec<&str> = collapsed
            .messages
            .iter()
            .map(|message| match message {
                Message::System(_) => "system",
                Message::User(_) => "user",
                Message::Assistant(_) => "assistant",
                Message::ToolResult(_) => "toolResult",
            })
            .collect();
        assert_eq!(roles, vec!["system"]);
        assert_eq!(collapsed, collapse_system_messages(&collapsed));
    }

    #[test]
    fn tool_state_changes_detect_definition_change() {
        let mut changed = tool("a");
        changed.description = "different".to_owned();
        let changes = get_tool_state_changes(&[tool("a")], &[changed.clone(), tool("b")]);
        assert_eq!(
            changes.tools_removed,
            vec![ToolReference {
                name: "a".to_owned()
            }]
        );
        assert_eq!(changes.tools_added, vec![changed, tool("b")]);
        assert!(
            get_tool_state_changes(&[tool("a")], &[tool("a")])
                .tools_added
                .is_empty()
        );
    }

    #[test]
    #[allow(deprecated)] // `has_tool_redefinitions` kept for API compatibility (b271b0a52)
    fn non_additive_detection() {
        let with_removed = vec![Message::System(SystemMessage {
            role: Default::default(),
            content: Default::default(),
            sections: None,
            tools_added: None,
            tools_removed: Some(vec![ToolReference {
                name: "x".to_owned(),
            }]),
            timestamp: 0,
        })];
        assert!(has_non_additive_tool_changes(&with_removed));
        let mut changed = tool("x");
        changed.description = "different".to_owned();
        let redeclared = vec![
            Message::System(SystemMessage {
                role: Default::default(),
                content: Default::default(),
                sections: None,
                tools_added: Some(vec![tool("x")]),
                tools_removed: None,
                timestamp: 0,
            }),
            Message::System(SystemMessage {
                role: Default::default(),
                content: Default::default(),
                sections: None,
                tools_added: Some(vec![changed]),
                tools_removed: None,
                timestamp: 1,
            }),
        ];
        assert!(has_non_additive_tool_changes(&redeclared));
        // A same-name redeclaration with a different definition is a
        // redefinition; an identical one is not (upstream Map+equality).
        assert!(has_tool_redefinitions(&redeclared));
    }

    #[test]
    fn removals_preserve_survivor_order() {
        // JS `Map.delete` keeps the survivors' insertion order; a removal in
        // the middle must not reorder the tail (upstream `getCurrentTools` /
        // `getCurrentSystemMessage`).
        let leading = Message::System(SystemMessage {
            role: Default::default(),
            content: Default::default(),
            sections: Some(serde_json::Map::from_iter([
                ("a".to_owned(), serde_json::json!("1")),
                ("b".to_owned(), serde_json::json!("2")),
                ("c".to_owned(), serde_json::json!("3")),
                ("d".to_owned(), serde_json::json!("4")),
            ])),
            tools_added: Some(vec![tool("a"), tool("b"), tool("c"), tool("d")]),
            tools_removed: None,
            timestamp: 1,
        });
        let later = Message::System(SystemMessage {
            role: Default::default(),
            content: Default::default(),
            sections: Some(serde_json::Map::from_iter([(
                "b".to_owned(),
                serde_json::Value::Null,
            )])),
            tools_added: None,
            tools_removed: Some(vec![ToolReference {
                name: "b".to_owned(),
            }]),
            timestamp: 2,
        });
        let messages = vec![leading, later];
        let current_tools = get_current_tools(&messages);
        let names: Vec<&str> = current_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "c", "d"]);
        let current = get_current_system_message(&messages).unwrap();
        let section_names: Vec<&str> = current.iter_sections().map(|(name, _)| name).collect();
        assert_eq!(section_names, vec!["a", "c", "d"]);
    }

    #[test]
    fn resolve_transcript_tools_falls_back_to_current_on_removal() {
        // Transcript-tool-changes shape: with a removal in history an
        // addition-anchoring transport cannot replay, so the request field
        // carries the complete CURRENT set (upstream
        // transcript-tool-changes.test.ts:262, payload-level there — unit
        // here).
        let messages = vec![
            Message::System(SystemMessage {
                role: Default::default(),
                content: Default::default(),
                sections: None,
                tools_added: Some(vec![tool("a"), tool("b")]),
                tools_removed: None,
                timestamp: 0,
            }),
            Message::System(SystemMessage {
                role: Default::default(),
                content: Default::default(),
                sections: None,
                tools_added: Some(vec![tool("c")]),
                tools_removed: Some(vec![ToolReference {
                    name: "b".to_owned(),
                }]),
                timestamp: 1,
            }),
        ];
        let resolved = resolve_transcript_tools(&messages, true);
        assert!(!resolved.anchors_additions);
        let names: Vec<&str> = resolved
            .request_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn resolve_transcript_tools_anchors_or_sends_current() {
        let context = normalize_context(&Context {
            system_prompt: None,
            messages: vec![system("", 1)],
            tools: Some(vec![tool("a")]),
        });
        let anchored = resolve_transcript_tools(&context.messages, true);
        assert!(anchored.anchors_additions);
        assert_eq!(anchored.request_tools, vec![tool("a")]);
        let collapsed = resolve_transcript_tools(&context.messages, false);
        assert!(!collapsed.anchors_additions);
        assert_eq!(collapsed.request_tools, vec![tool("a")]);
    }
}

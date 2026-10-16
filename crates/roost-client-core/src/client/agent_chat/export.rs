//! Markdown export for a browser-visible agent transcript.
//! Owned by the client so the download and other clients share one rendering.
//! It depends only on the protocol transcript and preserves visible answers.

use roost_protocol::wire::agent_chat::{Transcript, TranscriptBlock, TranscriptItem};

/// Renders the user-visible transcript as a self-contained Markdown document.
pub fn transcript_to_markdown(title: &str, transcript: &Transcript) -> String {
    let mut markdown = format!("# {}\n", title.trim());
    let latest_todo_index = transcript
        .items
        .iter()
        .rposition(|item| matches!(item, TranscriptItem::Todo { .. }));
    for (index, item) in transcript.items.iter().enumerate() {
        match item {
            TranscriptItem::User { text, .. } => append_section(&mut markdown, "You", text),
            TranscriptItem::Assistant { blocks, error, .. } => {
                let text = blocks
                    .iter()
                    .filter_map(|block| match block {
                        TranscriptBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !text.is_empty() {
                    append_section(&mut markdown, "Assistant", &text);
                }
                if let Some(error) = error {
                    append_section(&mut markdown, "Error", error);
                }
            }
            TranscriptItem::Notice { title, body, .. } => {
                append_section(&mut markdown, title, body);
            }
            TranscriptItem::Plan { title, content, .. } => {
                append_section(&mut markdown, title, content)
            }
            TranscriptItem::Todo { phases, .. } if latest_todo_index == Some(index) => {
                markdown.push_str("\n## Tasks\n");
                for phase in phases {
                    markdown.push_str(&format!("\n### {}\n", phase.name));
                    for task in &phase.tasks {
                        let marker = match task.status {
                            roost_protocol::wire::agent_chat::TodoStatus::Completed => "x",
                            roost_protocol::wire::agent_chat::TodoStatus::Abandoned => "-",
                            _ => " ",
                        };
                        markdown.push_str(&format!("- [{marker}] {}\n", task.content));
                    }
                }
            }
            TranscriptItem::Todo { .. } => {}
            TranscriptItem::Goal { .. } | TranscriptItem::LoopStatus { .. } => {}
            TranscriptItem::Ask {
                questions, answers, ..
            } => {
                markdown.push_str("\n## Questions\n");
                for question in questions {
                    markdown.push_str(&format!("\n### {}\n", question.question));
                    if let Some(values) =
                        answers.as_ref().and_then(|values| values.get(&question.id))
                    {
                        for value in values {
                            markdown.push_str(&format!("- {value}\n"));
                        }
                    } else {
                        for option in &question.options {
                            markdown.push_str(&format!("- {}\n", option.label));
                        }
                    }
                }
            }
            TranscriptItem::Btw {
                question,
                answer,
                error,
                ..
            } => {
                markdown.push_str(&format!("\n## Side question\n\n> {question}\n"));
                if let Some(answer) = answer {
                    markdown.push_str(&format!("\n{answer}\n"));
                }
                if let Some(error) = error {
                    append_section(&mut markdown, "Error", error);
                }
            }
            TranscriptItem::Tool {
                tool_name,
                output,
                is_error,
                ..
            } if !output.is_empty() => {
                let heading = if *is_error {
                    format!("Tool error: {tool_name}")
                } else {
                    format!("Tool: {tool_name}")
                };
                append_section(&mut markdown, &heading, output);
            }
            TranscriptItem::Tool { .. } => {}
            TranscriptItem::BackgroundJob {
                job_id,
                command,
                output,
                is_error,
                ..
            } => {
                let heading = if *is_error {
                    format!("Background job error: {job_id}")
                } else {
                    format!("Background job: {job_id}")
                };
                append_section(&mut markdown, &heading, &format!("$ {command}\n{output}"));
            }
            TranscriptItem::PeerMessage {
                from,
                broadcast,
                text,
                ..
            } => {
                let scope = if *broadcast {
                    "Broadcast from"
                } else {
                    "Message from"
                };
                append_section(&mut markdown, &format!("{scope} {from}"), text);
            }
            TranscriptItem::Advisory { severity, note, .. } => {
                append_section(&mut markdown, &format!("Advisor · {severity}"), note);
            }
        }
    }
    markdown
}

fn append_section(markdown: &mut String, heading: &str, content: &str) {
    markdown.push_str(&format!("\n## {heading}\n\n{content}\n"));
}

#[cfg(test)]
mod tests {
    use super::transcript_to_markdown;
    use roost_protocol::wire::agent_chat::{
        AgentRunState, Transcript, TranscriptBlock, TranscriptItem, UsageTotals,
    };

    #[test]
    fn exports_visible_user_and_assistant_text() {
        let transcript = Transcript {
            items: vec![
                TranscriptItem::User {
                    id: "u1".into(),
                    text: "Summarize this".into(),
                },
                TranscriptItem::Assistant {
                    id: "a1".into(),
                    blocks: vec![
                        TranscriptBlock::Thinking {
                            text: "private reasoning".into(),
                        },
                        TranscriptBlock::Text {
                            text: "A concise answer.".into(),
                        },
                    ],
                    streaming: false,
                    error: None,
                },
            ],
            run_state: AgentRunState::Idle,
            error: None,
            model: None,
            thinking_level: None,
            mode: None,
            usage: UsageTotals::default(),
            queued: Vec::new(),
        };
        assert_eq!(
            transcript_to_markdown("Research", &transcript),
            "# Research\n\n## You\n\nSummarize this\n\n## Assistant\n\nA concise answer.\n"
        );
    }
}

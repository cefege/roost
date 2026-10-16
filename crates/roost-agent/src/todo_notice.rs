//! The model-facing text of the task list. History replay, the advisor delta
//! and goal mode all render the list through this function, so the model reads
//! the same shape wherever the list appears.

use roost_protocol::wire::agent_chat::TodoPhase;

/// The model-facing form of the current task list.
pub fn todo_message(phases: &[TodoPhase]) -> String {
    let mut rendered = String::from("<todo>\n");
    for phase in phases {
        rendered.push_str(&format!("{}:\n", phase.name));
        for task in &phase.tasks {
            let status = serde_json::to_value(task.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            rendered.push_str(&format!("- [{status}] {}", task.content));
            if let Some(blocker) = &task.blocker {
                rendered.push_str(&format!(" (blocked: {blocker})"));
            }
            rendered.push('\n');
        }
    }
    rendered.push_str("</todo>");
    rendered
}

//! The agent's phased task checklist and disclosure state.
//! Transcript projection supplies committed phases; incomplete work reopens
//! its phase without introducing a second task or execution store.

use dioxus::prelude::*;
use libero::{
    components::{Button, Collapse, Flex, ListItem, Paper, Pictogram, SvgData, Text},
    sx::sx,
};
use roost_protocol::wire::agent_chat::{TodoPhase, TodoStatus};

#[component]
pub fn TodoCard(phases: Vec<TodoPhase>) -> Element {
    let mut expanded = use_signal(|| false);
    let total = phases.iter().map(|phase| phase.tasks.len()).sum::<usize>();
    let closed = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        .count();
    let preview = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .find(|task| task.status == TodoStatus::InProgress || task.status == TodoStatus::Blocked)
        .or_else(|| {
            phases
                .iter()
                .flat_map(|phase| &phase.tasks)
                .find(|task| !matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        });
    let card_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .min_width("0")
    });
    let header_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("sm")
    });
    let toggle_sx = use_hook(|| {
        sx().height("auto")
            .min_width("0")
            .justify_content("flex-start")
            .gap("sm")
            .white_space("normal")
            .text_align("left")
    });
    let preview_sx = use_hook(|| {
        sx().min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .color("muted")
    });
    let blocker_sx = use_hook(|| sx().font_size("xs").color("error"));
    let tasks_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("sm"));
    rsx! {
        Paper { sx: card_sx, "data-testid": "agent-chat-todo",
            Flex { direction: "row", sx: header_sx,
                Button {
                    variant: "text",
                    sx: toggle_sx,
                    "aria-expanded": expanded(),
                    "aria-controls": "agent-chat-todo-toggle",
                    "data-testid": "agent-chat-todo-toggle",
                    onclick: move |_| expanded.toggle(),
                    Pictogram { icon: pictogram_icons_lucide::list_checks::outlined }
                    Text { component: "strong", size: "sm", "To-do {closed}/{total}" }
                    if let Some(task) = preview {
                        Text { component: "span", sx: preview_sx, "{task.content}" }
                        if let Some(blocker) = &task.blocker { Text { component: "span", sx: blocker_sx, "{blocker}" } }
                    } else if total == 0 {
                        Text { component: "span", sx: preview_sx, "No tasks" }
                    } else {
                        Text { component: "span", sx: preview_sx, "All tasks closed" }
                    }
                    Pictogram { icon: if expanded() { pictogram_icons_lucide::chevron_up::outlined } else { pictogram_icons_lucide::chevron_down::outlined } }
                }
            }
            Collapse { id: "agent-chat-todo-toggle", open: expanded(),
                Flex { direction: "column", sx: tasks_sx, role: "list", "aria-label": "Phased task checklist",
                    for phase in phases.iter() {
                        TodoPhaseSection { key: "{phase.name}", phase: phase.clone() }
                    }
                }
            }
        }
    }
}

/// One phase of the checklist. It opens while it holds unfinished work and
/// re-opens when that changes, as the native `details` open attribute did.
#[component]
fn TodoPhaseSection(phase: TodoPhase) -> Element {
    let has_incomplete = phase
        .tasks
        .iter()
        .any(|task| !matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned));
    let mut open = use_signal(|| has_incomplete);
    use_effect(use_reactive((&has_incomplete,), move |(has_incomplete,)| {
        open.set(has_incomplete)
    }));
    let section_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("xs"));
    let toggle_sx = use_hook(|| {
        sx().height("auto")
            .min_width("0")
            .justify_content("flex-start")
            .padding_inline("0")
    });
    let tasks_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("xs"));
    let task_content_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("flex-start")
            .justify_content("space-between")
            .gap("sm")
    });
    let task_text_sx = use_hook(|| sx().min_width("0"));
    let status_sx = use_hook(|| sx().flex_shrink("0").font_size("xs").color("muted"));
    let blocker_sx = use_hook(|| sx().font_size("xs").color("error"));
    let item_sx = use_hook(|| sx().min_width("0"));
    rsx! {
        Flex { direction: "column", sx: section_sx,
            Button {
                variant: "text",
                sx: toggle_sx,
                "aria-expanded": open(),
                onclick: move |_| open.toggle(),
                Text { "{phase.name}" }
            }
            Collapse { id: format!("agent-chat-phase-{}", phase.name), open: open(),
                Flex { direction: "column", sx: tasks_sx,
                    for (index, task) in phase.tasks.iter().enumerate() {
                        ListItem {
                            key: "{index}",
                            sx: item_sx.clone(),
                            icon: Some(rsx! { Pictogram { icon: task_icon(task.status) } }),
                            role: "listitem",
                            Flex { direction: "row", sx: task_content_sx.clone(),
                                Text { component: "span", sx: task_text_sx.clone(), "{task.content}" }
                                Text { component: "span", sx: status_sx.clone(),
                                    "data-status": status_label(task.status).to_ascii_lowercase().replace(' ', "-"),
                                    "{status_label(task.status)}"
                                }
                            }
                            if let Some(blocker) = &task.blocker {
                                Text { component: "span", sx: blocker_sx.clone(), "{blocker}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn task_icon(status: TodoStatus) -> SvgData {
    match status {
        TodoStatus::Pending => pictogram_icons_lucide::circle::outlined,
        TodoStatus::InProgress => pictogram_icons_lucide::loader::outlined,
        TodoStatus::Completed => pictogram_icons_lucide::circle_check::outlined,
        TodoStatus::Abandoned => pictogram_icons_lucide::circle_x::outlined,
        TodoStatus::Blocked => pictogram_icons_lucide::ban::outlined,
    }
}
fn status_label(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "Pending",
        TodoStatus::InProgress => "In progress",
        TodoStatus::Completed => "Completed",
        TodoStatus::Abandoned => "Abandoned",
        TodoStatus::Blocked => "Blocked",
    }
}

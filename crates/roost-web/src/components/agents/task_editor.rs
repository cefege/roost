//! The full form for enqueuing a task: prompt, working directory, machine,
//! priority, and an optional completion check. Ports
//! `apps/web/src/components/agents/TaskEditor.tsx`; the modal wrapper is
//! `agents::queue_task_dialog`.
//!
//! The editor owns the DRAFT and nothing else. Whether it is mounted at all is
//! the caller's decision (`store().shell_dialogs.queue_task`), which is why the
//! prefill arrives as props rather than as a read here: a form that mounts with
//! its fields already seeded has no closed state to reset them from.

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::tasks::EnqueueTask;

use crate::pump::{Pump, use_store};
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Text, TextField, Textarea,
    use_combobox,
};
use libero::sx::sx;
#[path = "task_payload.rs"]
mod task_payload;
use task_payload::{ANY_WORKER_LABEL, DEFAULT_PRIORITY, enqueue_request, worker_options};

/// The test id the queue-task dialog's form is addressed by, shared with the
/// mount test that reads it off the mutation stream.
pub const EDITOR_TEST_ID: &str = "task-editor";

/// The task editor.
#[component]
pub fn TaskEditor(
    default_body: Option<String>,
    default_cwd: Option<String>,
    default_worker_fp: Option<String>,
    on_enqueued: Option<EventHandler<()>>,
    on_cancel: Option<EventHandler<()>>,
    #[props(default = true)] show_cancel: bool,
) -> Element {
    let pump = use_store();
    // `mut` on every one: a control handler closes over the binding by
    // reference and `Signal::set` takes `&mut self`, which is the convention
    // every sibling form component follows (`rename_dialog`, `agent_launcher`).
    let mut body = use_signal(|| default_body.unwrap_or_default());
    let mut cwd = use_signal(|| default_cwd.unwrap_or_default());
    let mut worker_fp = use_signal(|| default_worker_fp.unwrap_or_default());
    let mut priority = use_signal(|| DEFAULT_PRIORITY.to_string());
    let mut completion = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let options = {
        let core = pump.core();
        let core = core.borrow();
        worker_options(&core.store().workers)
    };
    let has_workers = options.len() > 1;

    let submit = {
        let pump = pump.clone();
        EventHandler::new(move |()| {
            let request = enqueue_request(
                body.peek().as_str(),
                cwd.peek().as_str(),
                worker_fp.peek().as_str(),
                priority.peek().as_str(),
                completion.peek().as_str(),
            );
            match request {
                Ok(request) => {
                    error.set(None);
                    submitting.set(true);
                    enqueue(pump.clone(), request, submitting, error, on_enqueued);
                }
                Err(message) => error.set(Some(message)),
            }
        })
    };
    let submit_on_enter = submit;

    let on_body = move |value: String| body.set(value);
    let on_cwd = move |value: String| cwd.set(value);
    let on_priority = move |value: String| priority.set(value);
    let on_completion = move |value: String| completion.set(value);
    let cancel_handler = on_cancel;
    let submit_button = submit;

    let worker_select = use_combobox();
    let field_sx = use_hook(|| sx().width("100%"));
    let editor_sx = use_hook(|| sx().gap("md").min_width("20rem").max_width("100%"));
    let actions_sx = use_hook(|| sx().gap("sm").justify_content("flex-end"));
    rsx! {
    Flex {
            direction: "column",
            sx: editor_sx,
            "data-testid": EDITOR_TEST_ID,
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Named(NamedKey::Enter) && (event.modifiers().meta() || event.modifiers().ctrl())
                {
                    event.prevent_default();
                    submit_on_enter.call(());
                }
            },
            Flex { direction: "column", "data-testid": "task-editor-body",
                Textarea {
                    sx: field_sx.clone(),
                    label: "Prompt",
                    rows: 4,
                    value: body(),
                    oninput: on_body,
                    placeholder: "Describe what the agent should do…",
                }
            }
            Flex { direction: "column", "data-testid": "task-editor-cwd",
                TextField {
                    sx: field_sx.clone(),
                    label: "Working directory",
                    value: cwd(),
                    oninput: on_cwd,
                    placeholder: "/Users/you/code/repo",
                }
            }
            if has_workers {
                Combobox {
                    state: worker_select,
                    select_only: true,
                    "aria-label": "Worker (optional)",
                    selected_index: options.iter().position(|(id, _)| *id == worker_fp()),
                    option_label: |option: (String, String)| option.1.clone(),
                    options: options.clone(),
                    option: {
                        let selected = worker_fp();
                        move |row: ComboboxOptionArgs<(String, String)>| rsx! {
                            ComboboxOption {
                                selected: selected == row.value.0,
                                onpick: move |_| { worker_fp.set(row.value.0.clone()); worker_select.close(); },
                                "{row.value.1}"
                            }
                        }
                    },
                    Button {
                        "data-testid": "task-editor-worker",
                        variant: "outlined",
                        attributes: worker_select.a11y_attributes(),
                        onclick: move |_| worker_select.toggle(),
                        onblur: move |_| worker_select.close(),
                        "{options.iter().find(|(id, _)| *id == worker_fp()).map(|(_, label)| label.as_str()).unwrap_or(ANY_WORKER_LABEL)}"
                    }
                }
            }
            Flex { direction: "column", "data-testid": "task-editor-priority",
                TextField {
                    sx: field_sx.clone(),
                    label: "Priority",
                    value: priority(),
                    oninput: on_priority,
                }
            }
            Flex { direction: "column", "data-testid": "task-editor-completion-check",
                TextField {
                    sx: field_sx,
                    label: "Completion check (shell cmd, optional)",
                    value: completion(),
                    oninput: on_completion,
                    placeholder: "gh pr view --json state -q .state | grep MERGED",
                }
            }
            if let Some(message) = error() {
                Text { color: "error", role: "alert", "{message}" }
            }
            Flex { direction: "row", sx: actions_sx,
                if show_cancel {
                    if let Some(cancel) = cancel_handler {
                        Button {
                            variant: "outlined",
                            aria_label: "Cancel queued task",
                            onclick: move |_| cancel.call(()),
                            "Cancel"
                        }
                    }
                }
                Button {
                    "data-testid": "task-editor-submit",
                    disabled: submitting(),
                    onclick: move |_| submit_button.call(()),
                    if submitting() { "Queuing…" } else { "Queue ⌘↩" }
                }
            }
        }
    }
}

/// Run the enqueue off the render path and answer on the form itself: the owner
/// hears that the task is queued, a refusal lands in the form's own error line
/// with the button live again so it can be retried.
fn enqueue(
    pump: Pump,
    request: EnqueueTask,
    submitting: Signal<bool>,
    error: Signal<Option<String>>,
    enqueued: Option<EventHandler<()>>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut submitting = submitting;
        let mut error = error;
        match pump.rpc().call(&request).await {
            Ok(task) => {
                tracing::info!(target: "shell", task = %task.id, "task enqueued");
                if let Some(enqueued) = enqueued {
                    enqueued.call(());
                }
            }
            Err(refusal) => {
                let message = refusal.to_string();
                tracing::warn!(target: "shell", %message, "task enqueue refused");
                submitting.set(false);
                error.set(Some(message));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, request, submitting, error, enqueued);
}

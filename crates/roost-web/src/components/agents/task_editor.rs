//! The full form for enqueuing a task: prompt, working directory, machine,
//! priority, and an optional completion check. Ports
//! `apps/web/src/components/agents/TaskEditor.tsx`; the modal wrapper is
//! `agents::queue_task_dialog`.
//!
//! The editor owns the DRAFT and nothing else. Whether it is mounted at all is
//! the caller's decision (`store().shell_dialogs.queue_task`), which is why the
//! prefill arrives as props rather than as a read here: a form that mounts with
//! its fields already seeded has no closed state to reset them from.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::tasks::EnqueueTask;
use roost_protocol::wire::Worker;
use serde_json::{Value, json};

use crate::components::md::{Button, ButtonVariant, Select, SelectOption, TextField};
use crate::pump::{Pump, use_store};

/// The option that pins no machine: the coordinator picks one that is reachable.
pub const ANY_WORKER_VALUE: &str = "";
/// What that option says.
pub const ANY_WORKER_LABEL: &str = "— any worker —";

/// What the editor says when the prompt is empty.
pub const BODY_REQUIRED_MESSAGE: &str = "Describe what the agent should do first.";

/// The test id the queue-task dialog's form is addressed by, shared with the
/// mount test that reads it off the mutation stream.
pub const EDITOR_TEST_ID: &str = "task-editor";

/// A body and a folder path read as code.
const MONO_STYLE: &str = "font-family: var(--font-mono);";
/// The form's own column.
const FORM_STYLE: &str = "display: flex; flex-direction: column; \
     gap: var(--md-space-3); min-inline-size: min(360px, 80vw);";
/// The refusal line under the fields.
const ERROR_STYLE: &str = "color: var(--md-sys-color-error); \
     font: var(--md-label-s-weight) var(--md-label-s-size) / var(--md-label-s-line) var(--md-font);";
/// The buttons' row.
const ACTIONS_STYLE: &str = "display: flex; gap: var(--md-space-2); justify-content: flex-end;";

/// The priority the reader has not touched.
const DEFAULT_PRIORITY: i64 = 0;

/// The task document the editor enqueues.
///
/// `cwd` and `worker_fp` are OMITTED rather than sent empty: an empty folder is
/// a folder the coordinator would try to resolve, and "any machine" is the
/// absence of a choice, not a choice of the empty string.
pub fn task_payload(body: &str, cwd: &str, worker_fp: &str, priority: i64) -> Value {
    let mut payload = json!({ "body": body, "priority": priority });
    let trimmed_cwd = cwd.trim();
    if !trimmed_cwd.is_empty() {
        payload["cwd"] = Value::String(trimmed_cwd.to_owned());
    }
    if !worker_fp.is_empty() {
        payload["worker_fp"] = Value::String(worker_fp.to_owned());
    }
    payload
}

/// The completion check the reader typed, or none: blank is not a command.
pub fn completion_check(typed: &str) -> Option<String> {
    let trimmed = typed.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The priority the reader typed, or the default when the field cannot be read
/// as one — an edit in progress such as `-` is not a priority of -1.
pub fn typed_priority(typed: &str) -> i64 {
    typed.trim().parse::<i64>().unwrap_or(DEFAULT_PRIORITY)
}

/// The machine options: no pin, then every machine the registry knows.
pub fn worker_options(workers: &BTreeMap<String, Worker>) -> Vec<SelectOption> {
    let mut options = vec![SelectOption::new(ANY_WORKER_VALUE, ANY_WORKER_LABEL)];
    options.extend(
        workers
            .values()
            .map(|worker| SelectOption::new(worker.fp.as_str(), worker.label.clone())),
    );
    options
}

/// The request a press sends, or the line the form shows instead.
///
/// A blank prompt is refused HERE rather than at the coordinator so the reader
/// sees it in the form they are already looking at; everything else travels.
pub fn enqueue_request(
    body: &str,
    cwd: &str,
    worker_fp: &str,
    priority: &str,
    completion: &str,
) -> Result<EnqueueTask, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err(BODY_REQUIRED_MESSAGE.to_owned());
    }
    Ok(EnqueueTask {
        payload_json: task_payload(body, cwd, worker_fp, typed_priority(priority)).to_string(),
        completion_check: completion_check(completion),
    })
}

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
    let on_worker = move |value: String| worker_fp.set(value);
    let on_priority = move |value: String| priority.set(value);
    let on_completion = move |value: String| completion.set(value);
    let cancel_handler = on_cancel;
    let submit_button = submit;

    rsx! {
        div {
            class: "roost-task-editor",
            "data-testid": EDITOR_TEST_ID,
            style: FORM_STYLE,
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Enter && (event.modifiers().meta() || event.modifiers().ctrl())
                {
                    event.prevent_default();
                    submit_on_enter.call(());
                }
            },
            TextField {
                test_id: "task-editor-body",
                label: "Prompt",
                input_type: Some("textarea".to_owned()),
                rows: Some(4),
                value: body(),
                on_input: on_body,
                placeholder: "Describe what the agent should do…",
            }
            TextField {
                test_id: "task-editor-cwd",
                label: "Working directory",
                value: cwd(),
                on_input: on_cwd,
                placeholder: "/Users/you/code/repo",
                control_style: Some(MONO_STYLE.to_owned()),
            }
            if has_workers {
                Select {
                    test_id: "task-editor-worker",
                    label: "Worker (optional)",
                    value: worker_fp(),
                    options,
                    on_change: on_worker,
                }
            }
            TextField {
                test_id: "task-editor-priority",
                label: "Priority",
                input_type: Some("number".to_owned()),
                value: priority(),
                on_input: on_priority,
                min: Some(-100.0),
                max: Some(100.0),
                style: "inline-size: 8ch;",
            }
            TextField {
                test_id: "task-editor-completion-check",
                label: "Completion check (shell cmd, optional)",
                value: completion(),
                on_input: on_completion,
                placeholder: "gh pr view --json state -q .state | grep MERGED",
                control_style: Some(MONO_STYLE.to_owned()),
            }
            if let Some(message) = error() {
                div {
                    "data-testid": "task-editor-error",
                    role: "alert",
                    style: ERROR_STYLE,
                    {message}
                }
            }
            div { style: ACTIONS_STYLE,
                if show_cancel {
                    if let Some(cancel) = cancel_handler {
                        Button {
                            variant: ButtonVariant::Outline,
                            onclick: move |_| cancel.call(()),
                            "Cancel"
                        }
                    }
                }
                Button {
                    variant: ButtonVariant::Default,
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

#[cfg(test)]
mod tests {
    use super::{
        ANY_WORKER_LABEL, completion_check, enqueue_request, task_payload, typed_priority,
    };
    use serde_json::Value;

    const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

    #[test]
    fn an_untouched_folder_and_machine_are_left_out_of_the_payload() {
        let payload = task_payload("ship it", "   ", "", 0);

        assert_eq!(payload["body"], "ship it");
        assert_eq!(payload["priority"], 0);
        assert!(
            payload.get("cwd").is_none(),
            "an empty folder was sent as a folder; the coordinator resolves it as one"
        );
        assert!(
            payload.get("worker_fp").is_none(),
            "\"any machine\" was sent as the machine named by the empty string"
        );
    }

    #[test]
    fn the_palette_prefill_travels_as_the_folder_the_reader_sits_in() {
        let payload = task_payload("ship it", "  /tmp  ", MACHINE, -5);

        assert_eq!(payload["cwd"], "/tmp");
        assert_eq!(payload["worker_fp"], MACHINE);
        assert_eq!(payload["priority"], -5);
    }

    #[test]
    fn a_blank_completion_check_is_absence_rather_than_an_empty_command() {
        assert_eq!(completion_check("   "), None);
        assert_eq!(
            completion_check("  gh pr view  "),
            Some("gh pr view".to_owned())
        );
    }

    #[test]
    fn a_half_typed_priority_is_the_default_rather_than_a_wrong_number() {
        assert_eq!(typed_priority("-"), 0);
        assert_eq!(typed_priority("  7 "), 7);
        assert_eq!(typed_priority("-12"), -12);
    }

    #[test]
    fn a_blank_prompt_is_refused_before_anything_is_sent() {
        let refused = enqueue_request("   ", "/tmp", MACHINE, "0", "");

        assert_eq!(
            refused.err(),
            Some(super::BODY_REQUIRED_MESSAGE.to_owned()),
            "an empty prompt was sent to the coordinator, which stores it as a task nobody can \
             read — the refusal belongs in the form the reader is looking at"
        );
    }

    #[test]
    fn an_editor_with_a_prompt_and_the_palette_prefill_sends_exactly_that() {
        let request = enqueue_request(" ship it ", " /tmp ", MACHINE, " 3 ", " make test ")
            .expect("a prompt is enough to queue");

        assert_eq!(request.completion_check, Some("make test".to_owned()));
        let payload: Value =
            serde_json::from_str(&request.payload_json).expect("the payload travels as JSON");
        assert_eq!(payload["body"], "ship it");
        assert_eq!(
            payload["cwd"], "/tmp",
            "the folder reached the coordinator with the reader's padding still on it"
        );
        assert_eq!(payload["worker_fp"], MACHINE);
        assert_eq!(payload["priority"], 3);
    }

    #[test]
    fn the_machine_list_leads_with_the_option_that_pins_none() {
        assert_eq!(super::worker_options(&Default::default()).len(), 1);
        assert_eq!(
            super::worker_options(&Default::default())[0].label,
            ANY_WORKER_LABEL,
            "with no machines known the only answer is \"any machine\", and it must not be a blank \
             control the reader cannot read"
        );
    }
}

//! Queue-task request construction and its input policy.
//! `task_editor` renders these values and submits the resulting RPC request.
//! This module stays independent of component state and layout.

use std::collections::BTreeMap;

use roost_client_core::client::rpc::calls::tasks::EnqueueTask;
use roost_protocol::wire::Worker;
use serde_json::{Value, json};

/// The option that pins no machine: the coordinator picks one that is reachable.
pub(super) const ANY_WORKER_VALUE: &str = "";
/// What that option says.
pub(super) const ANY_WORKER_LABEL: &str = "— any worker —";

/// What the editor says when the prompt is empty.
pub(super) const BODY_REQUIRED_MESSAGE: &str = "Describe what the agent should do first.";

/// The priority the reader has not touched.
pub(super) const DEFAULT_PRIORITY: i64 = 0;

/// The task document the editor enqueues.
///
/// `cwd` and `worker_fp` are OMITTED rather than sent empty: an empty folder is
/// a folder the coordinator would try to resolve, and "any machine" is the
/// absence of a choice, not a choice of the empty string.
pub(super) fn task_payload(body: &str, cwd: &str, worker_fp: &str, priority: i64) -> Value {
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
pub(super) fn completion_check(typed: &str) -> Option<String> {
    let trimmed = typed.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The priority the reader typed, or the default when the field cannot be read
/// as one — an edit in progress such as `-` is not a priority of -1.
pub(super) fn typed_priority(typed: &str) -> i64 {
    typed.trim().parse::<i64>().unwrap_or(DEFAULT_PRIORITY)
}

/// The machine options: no pin, then every machine the registry knows.
pub(super) fn worker_options(workers: &BTreeMap<String, Worker>) -> Vec<(String, String)> {
    let mut options = vec![(ANY_WORKER_VALUE.to_owned(), ANY_WORKER_LABEL.to_owned())];
    options.extend(
        workers
            .values()
            .map(|worker| (worker.fp.to_string(), worker.label.clone())),
    );
    options
}

/// The request a press sends, or the line the form shows instead.
///
/// A blank prompt is refused HERE rather than at the coordinator so the reader
/// sees it in the form they are already looking at; everything else travels.
pub(super) fn enqueue_request(
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

#[cfg(test)]
mod tests {
    use super::{
        ANY_WORKER_LABEL, BODY_REQUIRED_MESSAGE, completion_check, enqueue_request, task_payload,
        typed_priority, worker_options,
    };
    use serde_json::Value;

    const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

    #[test]
    fn an_untouched_folder_and_machine_are_left_out_of_the_payload() {
        let payload = task_payload("ship it", "   ", "", 0);

        assert_eq!(payload["body"], "ship it");
        assert_eq!(payload["priority"], 0);
        assert!(payload.get("cwd").is_none());
        assert!(payload.get("worker_fp").is_none());
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

        assert_eq!(refused.err(), Some(BODY_REQUIRED_MESSAGE.to_owned()));
    }

    #[test]
    fn an_editor_with_a_prompt_and_the_palette_prefill_sends_exactly_that() {
        let request = enqueue_request(" ship it ", " /tmp ", MACHINE, " 3 ", " make test ")
            .expect("a prompt is enough to queue");

        assert_eq!(request.completion_check, Some("make test".to_owned()));
        let payload: Value =
            serde_json::from_str(&request.payload_json).expect("valid request JSON");
        assert_eq!(payload["body"], "ship it");
        assert_eq!(payload["cwd"], "/tmp");
        assert_eq!(payload["worker_fp"], MACHINE);
        assert_eq!(payload["priority"], 3);
    }

    #[test]
    fn the_machine_list_leads_with_the_option_that_pins_none() {
        assert_eq!(worker_options(&Default::default()).len(), 1);
        assert_eq!(worker_options(&Default::default())[0].1, ANY_WORKER_LABEL);
    }
}

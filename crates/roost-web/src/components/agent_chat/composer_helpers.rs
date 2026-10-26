//! Shared focus, draft and stop mechanics for the composer.
//! These helpers keep the mounted input and the harness abort path consistent.
//! Called by the composer and workspace starter actions.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentRunState, GoalState, Transcript, TranscriptItem, parse_slash_command,
};

use crate::pump::Pump;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::AbortAgentChat;

use super::message_actions::EditTarget;

pub(super) fn composer_input_id(conversation_id: &str) -> String {
    format!("agent-chat-input-{conversation_id}")
}

/// The input placeholder for the current composer state, in the wording the composer
/// has always used; `busy_mode` picks the copy while a turn is running.
pub(super) fn composer_placeholder(
    disabled: bool,
    send_available: bool,
    running: bool,
    busy_mode: &str,
) -> &'static str {
    if disabled {
        "The agent host is offline"
    } else if !send_available {
        "Listening…"
    } else if running {
        match busy_mode {
            "follow_up" => "Add a follow-up…",
            "reject" => "This message will be rejected while busy",
            _ => "Steer the agent…",
        }
    } else {
        "Ask the agent, or type / for commands…"
    }
}

/// The goal, whether the run failed, and the session cost, read before the
/// submit closure takes the transcript.
pub(super) fn composer_run_facts(transcript: &Transcript) -> (Option<GoalState>, bool, f64) {
    (
        latest_goal(transcript),
        transcript.run_state == AgentRunState::Failed,
        transcript.usage.cost_usd,
    )
}

/// The transcript's most recent `/goal` state, if one was ever set.
pub(super) fn latest_goal(transcript: &Transcript) -> Option<GoalState> {
    transcript.items.iter().rev().find_map(|item| match item {
        TranscriptItem::Goal { state, .. } => Some(state.clone()),
        _ => None,
    })
}

pub(super) fn fill_draft(
    mut draft: Signal<String>,
    mut selected_option: Signal<usize>,
    text: &str,
) {
    draft.set(text.to_string());
    selected_option.set(0);
}

pub(super) fn focus_field(field: Signal<Option<Rc<MountedData>>>) {
    let Some(handle) = field.peek().clone() else {
        return;
    };
    spawn(async move {
        let _ = handle.set_focus(true).await;
    });
}

#[cfg(target_arch = "wasm32")]
pub(super) fn abort_run(pump: Pump, conversation_id: String) {
    wasm_bindgen_futures::spawn_local(async move {
        let request_id = super::request_ids::new_request_id();
        let abort = AbortAgentChat {
            conversation_id,
            request_id,
            background: Some(false),
        };
        if let Err(error) = pump.rpc().call(&abort).await {
            tracing::warn!(target: "agent_chat", %error, "agent abort failed");
        }
    });
}

/// Takes the edit-and-resend path when an edit is pending: refuses a slash
/// command (ambiguous rewrite-or-run), else rewinds to the edited message and
/// resubmits the edited text. Returns whether the caller's normal submit path
/// was consumed, so `false` means fall through to the ordinary composer send.
pub(super) fn try_submit_edit(
    mut edit_target: Signal<Option<EditTarget>>,
    mut draft: Signal<String>,
    pump: &Pump,
    conversation_id: &str,
    text: &str,
    report: EventHandler<Result<(), String>>,
) -> bool {
    let Some(target) = edit_target.peek().clone() else {
        return false;
    };
    if parse_slash_command(text).is_some() {
        report.call(Err("Finish or cancel the edit first.".to_owned()));
        return true;
    }
    edit_target.set(None);
    draft.set(String::new());
    super::message_actions::rewind_and_submit(
        pump.clone(),
        conversation_id.to_owned(),
        target.item_id,
        text.to_owned(),
    );
    true
}

/// The dismissible strip above the field while editing a past message.
#[component]
pub(super) fn EditBanner(
    mut edit_target: Signal<Option<EditTarget>>,
    mut draft: Signal<String>,
) -> Element {
    if edit_target.read().is_none() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "flex items-center gap-2 rounded-md border border-border bg-muted px-3 py-1.5 text-xs text-muted-foreground",
            "data-testid": "agent-chat-edit-banner",
            crate::components::ui::Badge { variant: crate::components::ui::BadgeVariant::Secondary, "Editing" }
            span { class: "flex-1", "Sending rewinds the conversation to this message." }
            crate::components::ui::IconButton {
                icon: crate::components::ui::IconType::X,
                label: "Cancel edit",
                title: "Cancel edit",
                size: crate::components::ui::ButtonSize::IconXs,
                variant: crate::components::ui::ButtonVariant::Ghost,
                onclick: move |_| {
                    edit_target.set(None);
                    draft.set(String::new());
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use dioxus::core::{AttributeValue, Mutation};
    use dioxus::prelude::*;
    use roost_client_core::ClientCore;

    use super::{EditBanner, EditTarget, try_submit_edit};
    use crate::platform::connect::CoordRpc;
    use crate::pump::Pump;

    const BANNER_TEST_ID: &str = "agent-chat-edit-banner";
    const SETTLE_ROUNDS: usize = 4;

    thread_local! {
        static EDIT_TARGET: RefCell<Option<Signal<Option<EditTarget>>>> = const { RefCell::new(None) };
        static DRAFT: RefCell<Option<Signal<String>>> = const { RefCell::new(None) };
        static PUMP: RefCell<Option<Pump>> = const { RefCell::new(None) };
        static REPORT: RefCell<Option<EventHandler<Result<(), String>>>> = const { RefCell::new(None) };
        static REPORTED: RefCell<Vec<Result<(), String>>> = const { RefCell::new(Vec::new()) };
    }

    fn root() -> Element {
        let edit_target = use_hook(|| Signal::new(None::<EditTarget>));
        let draft = use_hook(|| Signal::new(String::new()));
        let pump = use_hook(|| {
            Pump::new(
                Rc::new(RefCell::new(ClientCore::in_memory("tab-edit-resend"))),
                Signal::new(0_u64),
                Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
            )
        });
        let report =
            use_hook(|| EventHandler::new(|result| REPORTED.with(|r| r.borrow_mut().push(result))));
        EDIT_TARGET.with(|slot| *slot.borrow_mut() = Some(edit_target));
        DRAFT.with(|slot| *slot.borrow_mut() = Some(draft));
        PUMP.with(|slot| *slot.borrow_mut() = Some(pump.clone()));
        REPORT.with(|slot| *slot.borrow_mut() = Some(report));
        rsx! {
            EditBanner { edit_target, draft }
        }
    }

    /// Drains the mutation stream for a few settle rounds, accumulating every
    /// edit so a caller can look for the one it cares about.
    fn settle_capturing(dom: &mut VirtualDom) -> Vec<Mutation> {
        let mut edits = Vec::new();
        for _ in 0..SETTLE_ROUNDS {
            edits.extend(dom.render_immediate_to_vec().edits);
            dom.process_events();
        }
        edits
    }

    fn banner_mounted(edits: &[Mutation]) -> bool {
        edits.iter().any(|edit| {
            matches!(
                edit,
                Mutation::SetAttribute { name, value: AttributeValue::Text(text), .. }
                    if name == "data-testid" && text == BANNER_TEST_ID
            )
        })
    }

    struct Fixture {
        dom: VirtualDom,
        edit_target: Signal<Option<EditTarget>>,
        draft: Signal<String>,
        pump: Pump,
        report: EventHandler<Result<(), String>>,
    }

    fn mounted() -> Fixture {
        let mut dom = VirtualDom::new(root);
        dom.rebuild_in_place();
        settle_capturing(&mut dom);
        let edit_target = EDIT_TARGET
            .with(|slot| *slot.borrow())
            .expect("the root publishes its edit_target signal on first render");
        let draft = DRAFT
            .with(|slot| *slot.borrow())
            .expect("the root publishes its draft signal on first render");
        let pump = PUMP
            .with(|slot| slot.borrow().clone())
            .expect("the root publishes its pump on first render");
        let report = REPORT
            .with(|slot| *slot.borrow())
            .expect("the root publishes its report handler on first render");
        Fixture {
            dom,
            edit_target,
            draft,
            pump,
            report,
        }
    }

    #[test]
    fn the_banner_mounts_once_an_edit_is_pending_and_not_before() {
        let mut fixture = mounted();
        let initial = settle_capturing(&mut fixture.dom);
        assert!(
            !banner_mounted(&initial),
            "the banner is on the page before any edit was requested"
        );
        assert!(fixture.draft.peek().is_empty());

        let mut edit_target = fixture.edit_target;
        fixture.dom.in_runtime(|| {
            edit_target.set(Some(EditTarget {
                item_id: "u1".to_owned(),
            }));
        });
        let after_edit = settle_capturing(&mut fixture.dom);
        assert!(
            banner_mounted(&after_edit),
            "the banner did not mount once an edit was pending"
        );
    }

    #[test]
    fn try_submit_edit_falls_through_when_nothing_is_pending() {
        let fixture = mounted();
        let (edit_target, mut draft, pump, report) = (
            fixture.edit_target,
            fixture.draft,
            fixture.pump.clone(),
            fixture.report,
        );
        fixture.dom.in_runtime(|| {
            draft.set("unrelated draft".to_owned());
            let consumed = try_submit_edit(edit_target, draft, &pump, "c1", "hello", report);
            assert!(
                !consumed,
                "no edit was pending; the normal send path owns this submit"
            );
        });
        assert_eq!(draft.peek().as_str(), "unrelated draft");
    }

    #[test]
    fn try_submit_edit_refuses_a_slash_command_while_editing() {
        REPORTED.with(|r| r.borrow_mut().clear());
        let fixture = mounted();
        let (mut edit_target, mut draft, pump, report) = (
            fixture.edit_target,
            fixture.draft,
            fixture.pump.clone(),
            fixture.report,
        );
        fixture.dom.in_runtime(|| {
            edit_target.set(Some(EditTarget {
                item_id: "u1".to_owned(),
            }));
            draft.set("/retry".to_owned());
            let consumed = try_submit_edit(edit_target, draft, &pump, "c1", "/retry", report);
            assert!(
                consumed,
                "a slash command while editing must not fall through to a run"
            );
        });
        assert!(
            edit_target.peek().is_some(),
            "the refused slash command leaves the edit pending rather than silently cancelling it"
        );
        assert_eq!(draft.peek().as_str(), "/retry");
        assert_eq!(
            REPORTED.with(|r| r.borrow().len()),
            1,
            "the refusal is reported to the composer's error banner"
        );
    }

    #[test]
    fn try_submit_edit_clears_the_edit_for_a_plain_message() {
        let fixture = mounted();
        let (mut edit_target, mut draft, pump, report) = (
            fixture.edit_target,
            fixture.draft,
            fixture.pump.clone(),
            fixture.report,
        );
        fixture.dom.in_runtime(|| {
            edit_target.set(Some(EditTarget {
                item_id: "u1".to_owned(),
            }));
            draft.set("edited text".to_owned());
            let consumed = try_submit_edit(edit_target, draft, &pump, "c1", "edited text", report);
            assert!(consumed, "a pending edit always owns the submit");
        });
        assert!(
            edit_target.peek().is_none(),
            "sending the edit clears edit_target so the banner leaves the page"
        );
        assert!(
            draft.peek().is_empty(),
            "sending the edit clears the draft so it is not resubmitted"
        );
    }
}

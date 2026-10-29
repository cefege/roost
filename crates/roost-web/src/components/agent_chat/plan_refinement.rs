//! Plan-refinement state belongs to the normal agent composer.
//! `AgentChatSurface` provides this context to the transcript card and composer.
//! It validates a selected proposal against the canonical transcript before submission.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{Transcript, TranscriptItem};

use super::message_actions::EditTarget;
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::components::ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, IconButton, IconType,
};

#[derive(Clone, PartialEq)]
pub(super) struct PlanRefinementTarget {
    pub(super) item_id: String,
    pub(super) generation: u64,
    title: String,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct PlanRefinement {
    target: Signal<Option<PlanRefinementTarget>>,
    focus_request: Signal<Option<u64>>,
    edit_target: Signal<Option<EditTarget>>,
}

impl PlanRefinement {
    pub(super) fn provide(edit_target: Signal<Option<EditTarget>>) -> Self {
        use_context_provider(move || Self {
            target: Signal::new(None),
            focus_request: Signal::new(None),
            edit_target,
        })
    }

    pub(super) fn begin(&self, item_id: String, title: String) -> Result<(), &'static str> {
        if self
            .edit_target
            .peek()
            .as_ref()
            .is_some_and(EditTarget::has_attachments)
        {
            return Err(
                "Finish or cancel message editing before refining this plan; its attachments are still retained.",
            );
        }
        let mut edit_target = self.edit_target;
        let mut target = self.target;
        let mut focus_request = self.focus_request;
        let generation = focus_request.peek().unwrap_or_default().wrapping_add(1);
        edit_target.set(None);
        focus_request.set(Some(generation));
        target.set(Some(PlanRefinementTarget {
            item_id,
            generation,
            title,
        }));
        Ok(())
    }

    pub(super) fn cancel(&self) {
        let mut target = self.target;
        target.set(None);
    }

    pub(super) fn is_active(&self) -> bool {
        (self.target)().is_some()
    }

    pub(super) fn selected_target(&self) -> Option<PlanRefinementTarget> {
        (self.target)()
    }

    pub(super) fn focus_request(&self) -> Option<u64> {
        self.is_active().then(|| (self.focus_request)()).flatten()
    }

    pub(super) fn attachments_block_send(
        self,
        staged_files: Signal<Vec<ChosenFile>>,
        edit_target: Signal<Option<EditTarget>>,
    ) -> bool {
        self.is_active()
            && (!staged_files().is_empty()
                || edit_target
                    .peek()
                    .as_ref()
                    .is_some_and(EditTarget::has_attachments))
    }

    pub(super) fn is_current(&self, transcript: &Transcript) -> bool {
        self.target
            .read()
            .as_ref()
            .is_some_and(|target| proposed(transcript, &target.item_id))
    }

    pub(super) fn current_target(&self, transcript: &Transcript) -> Option<PlanRefinementTarget> {
        self.target
            .read()
            .as_ref()
            .filter(|target| proposed(transcript, &target.item_id))
            .cloned()
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(super) fn clear_if_matches(&self, item_id: &str, generation: u64) -> bool {
        let matches = self
            .target
            .peek()
            .as_ref()
            .is_some_and(|target| target.item_id == item_id && target.generation == generation);
        if matches {
            self.cancel();
        }
        matches
    }
}

pub(super) fn has_unresolved_proposal(transcript: &Transcript) -> bool {
    pending_proposal_id(transcript).is_some()
}

pub(super) fn pending_proposal_id(transcript: &Transcript) -> Option<&str> {
    transcript.items.iter().rev().find_map(|item| match item {
        TranscriptItem::Plan { id, state, .. } if state == "proposed" => Some(id.as_str()),
        _ => None,
    })
}

fn proposed(transcript: &Transcript, item_id: &str) -> bool {
    transcript
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            TranscriptItem::Plan { id, state, .. } if id == item_id => Some(state),
            _ => None,
        })
        .is_some_and(|state| state == "proposed")
}

#[component]
pub(super) fn PlanDecisionBanner(item_id: String) -> Element {
    let target_id = format!("plan-actions-{item_id}");
    rsx! {
        div {
            class: "flex items-center gap-2 text-xs text-muted-foreground",
            "data-testid": "agent-plan-decision-banner",
            role: "status",
            Badge { variant: BadgeVariant::Secondary, "Plan awaiting decision" }
            Button {
                variant: ButtonVariant::Link,
                size: ButtonSize::Sm,
                "aria-controls": target_id.clone(),
                onclick: move |_| {
                    #[cfg(target_arch = "wasm32")]
                    show_plan_actions(&target_id);
                },
                "Show plan"
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn show_plan_actions(target_id: &str) {
    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(target_id))
    {
        element.scroll_into_view_with_bool(false);
    }
}

#[component]
pub(super) fn PlanRefinementBanner(
    refinement: PlanRefinement,
    target: PlanRefinementTarget,
    attachments_block_send: bool,
    valid: bool,
) -> Element {
    let title = target.title;
    rsx! {
        div {
            class: "flex items-center gap-2 text-xs text-muted-foreground",
            "data-testid": "agent-plan-refinement-banner",
            Badge { variant: BadgeVariant::Secondary, "Refining plan" }
            span { class: "flex-1", "{title}" }
            if !valid {
                span { role: "status", class: "text-destructive",
                    "This plan is no longer awaiting approval. Cancel refinement to return to chat."
                }
            } else if attachments_block_send {
                span { class: "text-destructive", "Remove attachments before sending feedback." }
            }
            IconButton {
                icon: IconType::X,
                label: "Cancel plan refinement",
                title: "Cancel plan refinement",
                size: ButtonSize::IconXs,
                variant: ButtonVariant::Ghost,
                "data-testid": "agent-plan-refinement-cancel",
                onclick: move |_| refinement.cancel(),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use roost_protocol::wire::agent_chat::AgentRunState;

    #[derive(Clone, Copy)]
    struct Probe {
        refinement: PlanRefinement,
        edit_target: Signal<Option<EditTarget>>,
    }

    fn probe(slot: Rc<RefCell<Option<Probe>>>) -> Element {
        let edit_target = use_signal(|| None::<EditTarget>);
        let refinement = PlanRefinement::provide(edit_target);
        *slot.borrow_mut() = Some(Probe {
            refinement,
            edit_target,
        });
        rsx! {}
    }

    fn mounted() -> (VirtualDom, Probe) {
        let slot = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(probe, Rc::clone(&slot));
        dom.rebuild_in_place();
        let handles = (*slot.borrow()).expect("mounted probe publishes composer handles");
        (dom, handles)
    }

    fn transcript(state: &str) -> Transcript {
        Transcript {
            items: vec![TranscriptItem::Plan {
                id: "plan".to_owned(),
                title: "Plan".to_owned(),
                content: "Implement the change".to_owned(),
                state: state.to_owned(),
            }],
            run_state: AgentRunState::Idle,
            error: None,
            model: None,
            thinking_level: None,
            mode: None,
            usage: Default::default(),
            queued: Vec::new(),
            schedules: Vec::new(),
        }
    }

    #[test]
    fn a_decided_plan_cannot_turn_feedback_into_an_ordinary_message() {
        let (dom, handles) = mounted();
        dom.in_runtime(|| {
            handles
                .refinement
                .begin("plan".to_owned(), "Plan".to_owned())
                .unwrap();
            assert!(
                handles
                    .refinement
                    .current_target(&transcript("proposed"))
                    .is_some()
            );
            assert!(
                handles
                    .refinement
                    .current_target(&transcript("approved"))
                    .is_none()
            );
            assert!(
                handles.refinement.is_active(),
                "stale feedback must still take the refinement rejection path"
            );
            handles.refinement.cancel();
            assert!(!handles.refinement.is_active());
        });
    }

    #[test]
    fn unresolved_proposal_gate_clears_after_a_decision() {
        assert!(has_unresolved_proposal(&transcript("proposed")));
        assert!(!has_unresolved_proposal(&transcript("approved")));
        assert!(!has_unresolved_proposal(&transcript("refined")));
        assert!(!has_unresolved_proposal(&transcript("rejected")));
    }

    #[test]
    fn refinement_exits_plain_editing_but_preserves_retained_attachments() {
        let (dom, mut handles) = mounted();
        dom.in_runtime(|| {
            handles.edit_target.set(Some(EditTarget::PendingRewind {
                item_id: "message".to_owned(),
                attachment_names: Vec::new(),
            }));
            handles
                .refinement
                .begin("plan".to_owned(), "Plan".to_owned())
                .unwrap();
            assert!(
                handles.edit_target.peek().is_none(),
                "refinement must not leave rewind armed"
            );
            handles.refinement.cancel();
            handles.edit_target.set(Some(EditTarget::PendingRewind {
                item_id: "message".to_owned(),
                attachment_names: vec!["diagram.png".to_owned()],
            }));
            assert!(
                handles
                    .refinement
                    .begin("plan".to_owned(), "Plan".to_owned())
                    .is_err()
            );
            assert!(!handles.refinement.is_active());
            assert!(
                handles
                    .edit_target
                    .peek()
                    .as_ref()
                    .is_some_and(EditTarget::has_attachments)
            );
        });
    }

    #[test]
    fn a_late_completion_cannot_clear_a_restarted_refinement() {
        let (dom, handles) = mounted();
        dom.in_runtime(|| {
            handles
                .refinement
                .begin("plan".to_owned(), "Plan".to_owned())
                .unwrap();
            let older = handles.refinement.selected_target().unwrap().generation;
            handles.refinement.cancel();
            handles
                .refinement
                .begin("plan".to_owned(), "Plan".to_owned())
                .unwrap();
            let current = handles.refinement.selected_target().unwrap().generation;
            assert!(!handles.refinement.clear_if_matches("plan", older));
            assert!(handles.refinement.is_active());
            assert!(handles.refinement.clear_if_matches("plan", current));
            assert!(!handles.refinement.is_active());
        });
    }
}

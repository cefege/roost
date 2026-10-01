//! The approver's list: every pending request, and the settled outcome.
//!
//! Ported from `apps/web/src/components/pairing/Onboarding.tsx:130-153`. The
//! rows come from the store's `pair_requests`, which the Sync `Pair` domain
//! keeps current, so the list is a projection of what the coordinator still
//! considers answerable rather than a second copy of it.
//!
//! A ROW LEAVES BY DISPATCH, NEVER BY FILTERING. `ApproverRig` sends
//! `ShellIntent::DismissPairRequest` the moment the coordinator binds a code or
//! agrees to a denial, which is the one place a row is retired; filtering the
//! held request out here as well would hide a row for a second, differently
//! worded reason.
//!
//! THE CODE DIALOG IS NOT HERE. `PairSurface` mounts it beside this list, so an
//! approver who walks to another route does not lose the code they are reading
//! (`PairApprovalProvider.tsx:348-367` renders it from the provider for exactly
//! that reason).

use dioxus::prelude::*;
use roost_client_core::store::PairRequest;

use crate::components::md::stylesheet::use_md_stylesheet;
use crate::components::md::{EmptyState, SectionTitle};
use crate::components::terminal::dom::now_ms;
use crate::pump::use_store;

use super::approval_card::PairRequestCard;
use super::approver::{ApprovalPhase, PairApprover};
use super::notices::{ONBOARDING_STYLESHEET_HREF, PairingStatusNotice};

/// The approver's list content: the settled outcome, and the pending requests.
#[component]
pub fn ApprovalList(approver: PairApprover) -> Element {
    use_md_stylesheet(ONBOARDING_STYLESHEET_HREF);
    let pump = use_store();
    let state = approver.state().read().clone();
    let now = now_ms() as i64;
    let pending = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .pair_requests
            .values()
            .filter(|request| request.is_live_at(now))
            .cloned()
            .collect::<Vec<_>>()
    };
    // The row id is read beside the card that consumes the request: an `rsx!`
    // row's attributes are built before its child, so a card that took the
    // request would leave nothing to name the row with.
    let rows: Vec<(String, PairRequest)> = pending
        .into_iter()
        .map(|request| (request.ephemeral_id.clone(), request))
        .collect();
    let busy = state.busy_request_id.is_some() || state.phase == ApprovalPhase::Cancelling;
    let notice = state.notice.clone();
    rsx! {
        if let Some((tone, message)) = notice {
            PairingStatusNotice {
                tone,
                message,
                test_id: Some("pair-approval-outcome".to_string()),
            }
        }
        if rows.is_empty() {
            div { "data-testid": "onboarding-no-pending",
                EmptyState {
                    icon: "devices".to_string(),
                    title: "No browsers are waiting for approval".to_string(),
                    supporting: Some(
                        "When you open Roost in a new browser and request access, it'll show up here to approve."
                            .to_string(),
                    ),
                    action: None,
                }
            }
        } else {
            div { class: "pairing-approval-list", "data-testid": "pair-approval-list",
                SectionTitle { "Pending pair requests" }
                for (row_id, request) in rows {
                    div {
                        "data-testid": "pair-approval-row",
                        "data-ephemeral-id": row_id,
                        PairRequestCard { request, busy, approver: approver.clone() },
                    }
                }
            }
        }
    }
}

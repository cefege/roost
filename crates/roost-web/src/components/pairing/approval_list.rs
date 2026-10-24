//! The approver's list: every pending request, and the settled outcome. The rows
//! are a projection of the store's `pair_requests`, which the Sync `Pair` domain
//! keeps current, so the list never keeps a second copy of what the coordinator
//! still considers answerable.
//!
//! The code dialog is not here: `PairSurface` mounts it beside this list, so an
//! approver who walks to another route does not lose the code they are reading.

use dioxus::prelude::*;
use roost_client_core::store::PairRequest;

use crate::components::terminal::dom::now_ms;
use crate::pump::use_store;
use libero::components::{Flex, Pictogram, Text, Title};

use super::approval_card::PairRequestCard;
use super::approver::{ApprovalPhase, PairApprover};
use super::notices::PairingStatusNotice;

/// The approver's list content: the settled outcome, and the pending requests.
///
/// A row leaves by dispatch, never by filtering here. `ApproverRig` sends
/// `ShellIntent::DismissPairRequest` the moment the coordinator binds a code or
/// agrees to a denial, which is the one place a row is retired; filtering the
/// held request out here as well would hide a row for a second, differently
/// worded reason.
#[component]
pub fn ApprovalList(approver: PairApprover) -> Element {
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
    let rows: Vec<(String, PairRequest)> = pending
        .into_iter()
        .map(|request| (request.ephemeral_id.clone(), request))
        .collect();
    let busy = state.busy_request_id.is_some() || state.phase == ApprovalPhase::Cancelling;
    let notice = state.notice.clone();
    rsx! {
        if let Some((tone, message)) = notice {
            PairingStatusNotice { tone, message, test_id: Some("pair-approval-outcome".to_owned()) }
        }
        if rows.is_empty() {
            Flex { direction: "column", gap: "sm", "data-testid": "onboarding-no-pending",
                Pictogram { icon: pictogram_icons_lucide::monitor_smartphone::outlined, width: "32", height: "32" }
                Title { size: "md", "No browsers are waiting for approval" }
                Text { size: "sm", color: "muted",
                    "When you open Roost in a new browser and request access, it'll show up here to approve."
                }
            }
        } else {
            Flex { direction: "column", gap: "md", "data-testid": "pair-approval-list",
                Title { size: "md", "Pending pair requests" }
                for (row_id, request) in rows {
                    div { "data-testid": "pair-approval-row", "data-ephemeral-id": row_id,
                        PairRequestCard { request, busy, approver: approver.clone() }
                    }
                }
            }
        }
    }
}

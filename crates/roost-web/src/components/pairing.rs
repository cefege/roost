//! The browser pairing surface: the page an untrusted browser sees, and the
//! approver's page once it is trusted. One component draws both, because
//! `browser_access_state` decides which half a browser gets. The requester and
//! approver ceremonies are owned by `PairSurface` so they survive access changes.

mod approval_card;
mod approval_dialog;
mod approval_list;
mod approver;
mod failure;
mod notices;
mod other_options;
mod phone_pairing;
pub mod phone_qr;
mod request_card;
mod requester;
mod status;

pub use notices::{NoticeTone, PairingPageHeader, PairingStatusNotice};
pub use phone_pairing::PhonePairingCard;
pub use requester::{PairingRequester, use_pairing_requester};

use dioxus::prelude::*;
use roost_client_core::store::BrowserAccessState;

use crate::pump::use_store;

/// The pairing surface, mounted ONCE for the life of the document.
///
/// BOTH CEREMONIES ARE OWNED HERE, not by the panels below, and that is the
/// whole reason this component is route-aware. v2 puts its two providers above
/// the access gate (`App.tsx:138-141`) so an access transition never disposes
/// them; a surface that unmounted with its page would take the requester's
/// token and the approver's six-digit code down with it, so an approver who
/// opened Settings mid-approval would lose the code they were reading to a
/// stranger. Owning both here means this component has to decide for itself
/// whether it draws anything:
///
/// - `Checking` → no page: the checking screen owns the document until the
///   coordinator answers;
/// - `Unauthorized` → the gate panel, at whatever path the reader is on,
///   because an unpaired reader has no other surface to reach;
/// - `Authorized` and on `/pair` → the approver list;
/// - `Authorized` anywhere else → no page, but the code dialog still stands.
///
/// The authorized branch mounts a notification dock, and the "New browser
/// paired" announcement comes from the Sync fold (`handle_sync/fold_controls.rs`);
/// raising it here as well would say it twice.
#[component]
pub fn PairSurface() -> Element {
    let pump = use_store();
    let path = crate::router_state::use_location();
    let requester = use_pairing_requester();
    let approver = approver::use_pair_approver();
    let (access, has_workers) = {
        let core = pump.core();
        let core = core.borrow();
        (
            core.store().browser_access_state,
            !core.store().workers.is_empty(),
        )
    };
    let authorized = access == BrowserAccessState::Authorized;
    let draws_page = draws_pairing_page(access, &path());
    // The dialog is a sibling of the panel, and both draw from one approval:
    // the panel gets a clone so the dialog still owns the handle v2's provider
    // held above the access gate.
    let dialog_approver = approver.clone();
    rsx! {
        if draws_page {
            div {
                class: "onboarding-root box-border flex h-dvh w-full min-w-0 flex-col overflow-x-hidden overflow-y-auto px-4 py-5 min-[600px]:px-6 min-[600px]:py-7",
                "data-testid": "onboarding",
                "data-embedded": "false",
                div {
                    class: "onboarding-panel mx-auto flex w-full min-w-0 max-w-[var(--roost-dialog-max-inline-size)] flex-col gap-5",
                    if authorized {
                        ApproverPanel { approver: approver.clone(), has_workers }
                    } else {
                        RequesterPanel { requester }
                    }
                }
            }
        }
        approval_dialog::ApprovalCodeDialog { approver: dialog_approver }
    }
}

/// Whether [`PairSurface`] draws a page for this access state at this path.
///
/// The path is read through [`crate::routes::Route`], the one URL grammar,
/// because the location the router renders is `pathname` PLUS the query. A raw
/// comparison against `Route::Pair.to_path()` reads `/pair?tv=1` as somewhere
/// else entirely, and an authorized reader who opened the ceremony with any
/// query on the URL then got the code dialog and no ceremony at all.
pub fn draws_pairing_page(access: BrowserAccessState, path: &str) -> bool {
    match access {
        BrowserAccessState::Checking => false,
        BrowserAccessState::Unauthorized => true,
        BrowserAccessState::Authorized => {
            crate::routes::Route::parse(path) == crate::routes::Route::Pair
        }
    }
}

/// The untrusted half: the one primary request action, the poll status, the
/// verification entry, and the collapsed other options.
///
/// Shown at EVERY protected route while the access state is `unauthorized`, not
/// only at `/pair`. A gate that sent an unpaired reader to a bare "not
/// authorized" card first would make the one action that fixes it something they
/// had to go looking for.
#[component]
fn RequesterPanel(requester: PairingRequester) -> Element {
    let state = requester.state();
    let state = state.read();
    let request_error = state.request_failure.clone();
    rsx! {
        PairingPageHeader {
            title: "Pair this browser".to_owned(),
            body: Some(
                "This browser needs approval before it can access your Roost workspace."
                    .to_owned(),
            ),
        }
        request_card::RequestAccessCard { requester: requester.clone() }
        if let Some(message) = request_error {
            PairingStatusNotice {
                tone: NoticeTone::Error,
                message,
                test_id: Some("onboarding-request-error".to_owned()),
            }
        }
        other_options::PairingOtherOptions { requester }
    }
}

/// The trusted half: the pending requests, and the code a phone scans to pair
/// without a request at all. The code dialog is NOT here — it is mounted by
/// [`PairSurface`] so it outlives the page the approver walked away from.
#[component]
fn ApproverPanel(approver: approver::PairApprover, has_workers: bool) -> Element {
    rsx! {
        PairingPageHeader { title: "Browser pairing".to_owned(), body: None }
        if !has_workers {
            p { class: "m-0 text-sm text-muted-foreground",
                "This browser is authorized, but no machines have registered as workers yet."
            }
        }
        approval_list::ApprovalList { approver }
        PhonePairingCard {}
    }
}

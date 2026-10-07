//! The browser pairing surface: the whole page, for a browser that is not
//! trusted and for one that is.
//!
//! ONE COMPONENT, TWO HALVES. v2 renders the same onboarding component in the
//! unauthorized branch of the access gate and at `/pair`
//! (`Onboarding.tsx:109-154`), and which half draws is decided by
//! `browser_access_state`: an unauthorized browser gets the requester panel, an
//! authorized one gets the pending requests to approve. Splitting it into two
//! entry points would mean two places that decide the same question, and they
//! would disagree the first time a browser moved between them.
//!
//! The two halves are not two features. A requester asks and polls; an approver
//! generates a code and watches the requester prove it. They share the
//! coordinator's statuses, so they share the wording (`pairing::status`) and the
//! failure classification (`pairing::failure`): a `verification_failed` the
//! requester reads as "too many incorrect codes" and the approver reads as "this
//! browser is done" is one server fact and must not become two sentences.
//!
//! The ceremony's stages and its two tab-scoped records are NOT here: they are
//! `roost_client_core::client::auth`'s, and this module drives them rather than
//! restating them. The pending rows are the store's, filled by the Sync `Pair`
//! domain. Ported from `apps/web/src/components/pairing/*` and the two
//! providers beside it.
//!
//! WHAT IS NOT HERE. The unauthorized branch mounts no notification dock, so
//! its outcomes render as inline notices rather than as toasts. The authorized
//! branch does have one, and the "New browser paired" announcement comes from
//! the Sync fold (`handle_sync/fold_controls.rs:85-111`) — raising it here as
//! well would say it twice.

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

use crate::components::md::stylesheet::use_md_stylesheet;
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
#[component]
pub fn PairSurface() -> Element {
    use_md_stylesheet(notices::ONBOARDING_STYLESHEET_HREF);
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
                class: "onboarding-root",
                "data-testid": "onboarding",
                "data-embedded": "false",
                div { class: "onboarding-panel",
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
            title: "Pair this browser".to_string(),
            body: Some(
                "This browser needs approval before it can access your Roost workspace."
                    .to_string(),
            ),
        }
        request_card::RequestAccessCard { requester: requester.clone() }
        if let Some(message) = request_error {
            PairingStatusNotice {
                tone: NoticeTone::Error,
                message,
                test_id: Some("onboarding-request-error".to_string()),
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
        PairingPageHeader { title: "Browser pairing".to_string(), body: None }
        if !has_workers {
            p { class: "md-body-m pairing-header__body",
                "This browser is authorized, but no machines have registered as workers yet."
            }
        }
        approval_list::ApprovalList { approver }
        PhonePairingCard {}
    }
}

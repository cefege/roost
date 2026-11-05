//! Transport and browser metrics for one leased OMP semantic UI view.
//! Every request preserves its per-mount view ID; viewport data uses terminal
//! cell geometry and appearance changes use the document's active theme.
use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpActionIdentity;
use serde_json::{Value, json};

use crate::pump::Pump;

pub(super) const VIEW_LEASE_MS: u32 = 30_000;

pub(super) fn open_omp_ui_action(
    cols: u32,
    rows: u32,
    dark: bool,
    reduce_motion: bool,
    reset: bool,
) -> Value {
    let mut action = json!({"method":"open","params":{"cols":cols,"rows":rows,"dark":dark,"reduceMotion":reduce_motion,"features":["send","undo","scroll"]}});
    if reset && let Some(params) = action.get_mut("params").and_then(Value::as_object_mut) {
        params.insert("reset".to_owned(), json!(true));
    }
    action
}

#[cfg(target_arch = "wasm32")]
pub(super) fn measured_viewport(mounted: &MountedData) -> Option<(u32, u32)> {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast;
    let grid: web_sys::Element = mounted.try_as_web_event()?;
    let cell =
        roost_web_terminal::cell_geometry::measure_terminal_cell_box(&grid, "font-mono text-sm")?;
    let element = grid.dyn_into::<web_sys::HtmlElement>().ok()?;
    let geometry =
        roost_web_terminal::cell_geometry::terminal_geometry_for_element(&element, cell)?;
    Some((geometry.cols, geometry.rows))
}

pub(super) fn observe_omp_viewport(
    mounted: std::rc::Rc<MountedData>,
    resized: EventHandler<(u32, u32)>,
) -> Option<crate::components::deck::deck_dom::SizeWatch> {
    #[cfg(target_arch = "wasm32")]
    {
        let measured_node = std::rc::Rc::clone(&mounted);
        let measure = move || {
            if let Some(size) = measured_viewport(&measured_node) {
                resized.call(size);
            }
        };
        measure();
        let observer = crate::components::deck::deck_dom::SizeWatch::new(measure)?;
        observer.watch(&mounted);
        Some(observer)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (mounted, resized);
        None
    }
}

pub(super) fn current_dark_theme() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        return web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .is_some_and(|element| element.class_list().contains("dark"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    false
}

#[cfg(target_arch = "wasm32")]
pub(super) struct ThemeObserver {
    observer: web_sys::MutationObserver,
    _callback: wasm_bindgen::closure::Closure<dyn FnMut()>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for ThemeObserver {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn observe_dark_theme(mut dark: Signal<bool>) -> Option<ThemeObserver> {
    use wasm_bindgen::JsCast;
    let root = web_sys::window()?.document()?.document_element()?;
    let callback = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
        let next = current_dark_theme();
        if *dark.peek() != next {
            dark.set(next);
        }
    }) as Box<dyn FnMut()>);
    let observer = web_sys::MutationObserver::new(callback.as_ref().unchecked_ref()).ok()?;
    let options = web_sys::MutationObserverInit::new();
    options.set_attributes(true);
    observer.observe_with_options(&root, &options).ok()?;
    Some(ThemeObserver {
        observer,
        _callback: callback,
    })
}

pub(crate) fn send_omp_ui_request(
    pump: Pump,
    request_id: String,
    identity: roost_client_core::client::agent_chat::OmpActionIdentity,
    action: Value,
    receipt: bool,
    outcome: EventHandler<Result<(), String>>,
) {
    let tab_id = pump.core().borrow().store().tab_id.clone();
    #[cfg(target_arch = "wasm32")]
    {
        use roost_client_core::client::rpc::calls::agent_chat::SendOmpUi;
        spawn(async move {
            let result = pump
                .rpc()
                .call(&SendOmpUi {
                    conversation_id: identity.conversation_id,
                    request_id,
                    action,
                    view_id: identity.view_id,
                    tab_id,
                    transport_id: identity.transport_id,
                    receipt,
                })
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
            outcome.call(result);
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (pump, request_id, identity, action, receipt, tab_id);
        outcome.call(Err(
            "OMP command UI is available only in the browser.".to_owned()
        ));
    }
}

/// Submit one action through the receipt-tracked queue.
///
/// Open, close, lease renewals, host replies and typed edit/focus bypass the
/// queue; every other input or event is serialised one at a time.
pub(crate) fn dispatch_omp_ui_action(
    pump: Pump,
    queue: std::rc::Rc<std::cell::RefCell<roost_client_core::client::agent_chat::OmpActionQueue>>,
    identity: roost_client_core::client::agent_chat::OmpActionIdentity,
    action: Value,
    outcome: EventHandler<Result<(), String>>,
) -> usize {
    use roost_client_core::client::agent_chat::OmpSubmit;
    let request_id = super::request_ids::new_request_id();
    let submit = queue.borrow_mut().submit(request_id, identity, action);
    match submit {
        OmpSubmit::Bypass(action) => send_omp_ui_request(
            pump,
            action.request_id,
            action.identity,
            action.action,
            false,
            outcome,
        ),
        OmpSubmit::Send(action) => {
            send_queued_action(pump, std::rc::Rc::clone(&queue), action, outcome)
        }
        OmpSubmit::Queued => {}
        OmpSubmit::Full => outcome.call(Err(
            "OMP is still applying input. Wait before retrying that action.".to_owned(),
        )),
    }
    queue.borrow().pending_count()
}

/// Apply every receipt whose transcript fence is satisfied, and release the
/// next queued action after a completion.
pub(crate) fn apply_omp_receipts(
    pump: Pump,
    queue: std::rc::Rc<std::cell::RefCell<roost_client_core::client::agent_chat::OmpActionQueue>>,
    identity: &roost_client_core::client::agent_chat::OmpActionIdentity,
    loaded_seq: u64,
    outcome: EventHandler<Result<(), String>>,
    receipt_error: EventHandler<String>,
) -> usize {
    use roost_client_core::client::agent_chat::OmpReceiptOutcome;
    loop {
        let receipt = pump.write_store(|store| {
            roost_client_core::store::take_ready_omp_receipt(
                store,
                &identity.conversation_id,
                &identity.view_id,
                &identity.transport_id,
                loaded_seq,
            )
        });
        let Some(receipt) = receipt else { break };
        let next = {
            let mut queue = queue.borrow_mut();
            match queue.receive(&receipt, loaded_seq) {
                OmpReceiptOutcome::Completed { next, error, .. } => {
                    if !error.is_empty() {
                        receipt_error.call(error);
                    }
                    *next
                }
                OmpReceiptOutcome::Ignored if !receipt.error.is_empty() => {
                    receipt_error.call(receipt.error.clone());
                    None
                }
                _ => None,
            }
        };
        if let Some(next) = next {
            send_queued_action(pump.clone(), std::rc::Rc::clone(&queue), next, outcome);
        }
    }
    loop {
        let next = {
            let mut queue = queue.borrow_mut();
            match queue.settle(loaded_seq) {
                Some(OmpReceiptOutcome::Completed { next, error, .. }) => {
                    if !error.is_empty() {
                        receipt_error.call(error);
                    }
                    *next
                }
                _ => None,
            }
        };
        let Some(next) = next else { break };
        send_queued_action(pump.clone(), std::rc::Rc::clone(&queue), next, outcome);
    }
    queue.borrow().pending_count()
}

fn send_queued_action(
    pump: Pump,
    queue: std::rc::Rc<std::cell::RefCell<roost_client_core::client::agent_chat::OmpActionQueue>>,
    action: roost_client_core::client::agent_chat::OmpQueuedAction,
    outcome: EventHandler<Result<(), String>>,
) {
    let request_id = action.request_id.clone();
    let identity = action.identity.clone();
    let admitted = EventHandler::new(move |result: Result<(), String>| {
        if result.is_err() && !queue.borrow_mut().reject_admission(&request_id, &identity) {
            return;
        }
        outcome.call(result);
    });
    send_omp_ui_request(
        pump,
        action.request_id,
        action.identity,
        action.action,
        true,
        admitted,
    );
}
pub(super) fn send_direct(
    pump: &Pump,
    identity: &OmpActionIdentity,
    action: Value,
    outcome: EventHandler<Result<(), String>>,
) {
    let request_id = super::request_ids::new_request_id();
    send_omp_ui_request(
        pump.clone(),
        request_id,
        identity.clone(),
        action,
        false,
        outcome,
    );
}

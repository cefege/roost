//! Image reads on the elected direct carrier, matched by request id and settled
//! when the worker answers or the carrier route is lost.
//!
//! Owned by `pump`; called by pane image loading and answered from peer and
//! loopback drains.

use std::collections::BTreeMap;

use roost_client_core::TerminalToken;
use roost_client_core::client::carriers::DirectTerminalImage;
use roost_client_core::effect::DirectCommand;

use super::Pump;

/// How long a direct image read may go unanswered.
pub const DIRECT_IMAGE_TIMEOUT_MS: u64 = 15_000;

type Reply = Box<dyn FnOnce(Result<Vec<u8>, String>)>;

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
struct PendingRead {
    session_id: String,
    sent_on: TerminalToken,
    reply: Reply,
}

/// Direct image reads not yet answered, by request id.
#[derive(Default)]
pub(super) struct DirectImageReads {
    next_request: u64,
    pending: BTreeMap<String, PendingRead>,
}

impl Pump {
    /// Ask `token` for `image_key`; the callback receives the PNG or refusal.
    pub fn read_direct_image(
        &self,
        token: &TerminalToken,
        session_id: &str,
        image_key: u64,
        reply: impl FnOnce(Result<Vec<u8>, String>) + 'static,
    ) {
        let request_id = {
            let mut reads = self.inner.direct_images.borrow_mut();
            reads.next_request += 1;
            let request_id = format!("image-{}", reads.next_request);
            reads.pending.insert(
                request_id.clone(),
                PendingRead {
                    session_id: session_id.to_owned(),
                    sent_on: token.clone(),
                    reply: Box::new(reply),
                },
            );
            request_id
        };
        let command = DirectCommand::TerminalImage {
            session_id: session_id.to_owned(),
            request_id: request_id.clone(),
            image_key,
        };
        let (delay_ms, reason) = match super::carriers::try_send(self, token, &command) {
            Ok(()) => (
                DIRECT_IMAGE_TIMEOUT_MS,
                "terminal peer image read timed out".to_owned(),
            ),
            Err(fault) => (0, fault.to_string()),
        };
        lose_after(self, request_id, delay_ms, reason);
    }
}

/// Settle reads whose route is no longer elected after a carrier closes.
#[cfg(target_arch = "wasm32")]
pub(super) fn lose_reads_off_route(pump: &Pump, reason: &str) {
    let pump = pump.clone();
    let reason = reason.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        let stranded: Vec<String> = pump
            .inner
            .direct_images
            .borrow()
            .pending
            .iter()
            .filter(|(_, read)| {
                pump.elected_direct_history_route(&read.session_id).as_ref() != Some(&read.sent_on)
            })
            .map(|(request_id, _)| request_id.clone())
            .collect();
        for request_id in stranded {
            settle(&pump, &request_id, Err(reason.clone()));
        }
    });
}

/// A carrier's answer to a read this document sent.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(super) fn answered(pump: &Pump, answer: DirectTerminalImage) {
    let result = if answer.error.is_empty() && !answer.png.is_empty() {
        Ok(answer.png)
    } else {
        Err(answer.error)
    };
    settle(pump, &answer.request_id, result);
}

fn settle(pump: &Pump, request_id: &str, outcome: Result<Vec<u8>, String>) {
    let read = pump
        .inner
        .direct_images
        .borrow_mut()
        .pending
        .remove(request_id);
    match read {
        Some(read) => (read.reply)(outcome),
        None => tracing::debug!(target: "terminal_image", request_id,
            "direct image answer arrived for a read no longer pending"),
    }
}

#[cfg(target_arch = "wasm32")]
fn lose_after(pump: &Pump, request_id: String, delay_ms: u64, reason: String) {
    use wasm_bindgen::JsCast as _;
    let Some(window) = web_sys::window() else {
        settle(pump, &request_id, Err(reason));
        return;
    };
    let pump = pump.clone();
    let expire = wasm_bindgen::closure::Closure::once_into_js(move || {
        settle(&pump, &request_id, Err(reason));
    });
    let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(expire.unchecked_ref(), delay);
}

#[cfg(not(target_arch = "wasm32"))]
fn lose_after(pump: &Pump, request_id: String, _delay_ms: u64, reason: String) {
    settle(pump, &request_id, Err(reason));
}

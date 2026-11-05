//! Completion metadata for every typed native input and event action.
//! A receipt leaves the worker only after the native handler finished and every
//! durable frame the worker had produced by then was acknowledged by the browser.
//! A browser that did not ask for a receipt still must not be left with a
//! silent failure: its optimistic edit is released by a private failure receipt.

use super::ViewOrigin;
use crate::omp::emitter::Emitter;
use roost_proto::WOmpUiActionReceipt;
use std::collections::HashMap;
use tokio::time::{Duration, Instant};

const RECEIPT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PENDING_RECEIPTS: usize = 64;

struct PendingReceipt {
    origin: ViewOrigin,
    /// Whether a successful native completion owes the browser a receipt.
    success_receipt: bool,
    target: Option<u64>,
    error: String,
    deadline: Instant,
}

#[derive(Default)]
pub(super) struct Receipts {
    pending: HashMap<String, PendingReceipt>,
    acknowledged: u64,
}

impl Receipts {
    /// Returns false when too many completions are already in flight.
    pub fn track(&mut self, request_id: &str, origin: ViewOrigin, success_receipt: bool) -> bool {
        if self.pending.len() >= MAX_PENDING_RECEIPTS {
            return false;
        }
        self.pending.insert(
            request_id.to_owned(),
            PendingReceipt {
                origin,
                success_receipt,
                target: None,
                error: String::new(),
                deadline: Instant::now() + RECEIPT_TIMEOUT,
            },
        );
        true
    }

    /// Record the native result and fence it behind the frames already emitted.
    /// `failure` is `None` for a successful native completion.
    pub fn complete(&mut self, request_id: &str, failure: Option<String>, emitter: &Emitter) {
        let Some(receipt) = self.pending.get(request_id) else {
            return;
        };
        if receipt.target.is_some() {
            return;
        }
        if failure.is_none() && !receipt.success_receipt {
            // A successful action the browser did not receipt stays silent:
            // the document frames it already receives are the whole answer.
            self.pending.remove(request_id);
            return;
        }
        let sequence = emitter.last_sequence();
        let Some(receipt) = self.pending.get_mut(request_id) else {
            return;
        };
        receipt.error = failure.unwrap_or_default();
        receipt.target = Some(sequence);
        self.release(request_id, emitter);
    }

    pub fn acknowledge(&mut self, sequence: u64, emitter: &Emitter) {
        if sequence > self.acknowledged {
            self.acknowledged = sequence;
        }
        let ready = self
            .pending
            .iter()
            .filter(|(_, receipt)| {
                receipt
                    .target
                    .is_some_and(|target| target <= self.acknowledged)
            })
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        for request_id in ready {
            self.release(&request_id, emitter);
        }
    }

    /// A view, transport or child generation ended: its receipts must not apply later.
    pub fn cancel_transport(&mut self, transport_id: &str) {
        self.pending
            .retain(|_, receipt| receipt.origin.transport_id != transport_id);
    }

    pub fn cancel_view(&mut self, view_id: &str, transport_id: &str) {
        self.pending.retain(|_, receipt| {
            receipt.origin.view_id != view_id || receipt.origin.transport_id != transport_id
        });
    }

    pub fn cancel_all(&mut self) {
        self.pending.clear();
    }

    /// The action never reached the child; there is nothing to complete.
    pub fn forget(&mut self, request_id: &str) {
        self.pending.remove(request_id);
    }

    /// Release a receipt whose frames never became durable, with an error the
    /// browser can show instead of leaving its input pending forever. A silent
    /// action still inside the native handler is not a receipt to release: it
    /// completes through `complete`.
    pub fn expire(&mut self, emitter: &Emitter) {
        let now = Instant::now();
        let expired = self
            .pending
            .iter()
            .filter(|(_, receipt)| receipt.success_receipt || receipt.target.is_some())
            .filter(|(_, receipt)| receipt.deadline <= now)
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        for request_id in expired {
            if let Some(receipt) = self.pending.get_mut(&request_id) {
                receipt.error = "The browser did not confirm this view before it went idle.".into();
                receipt.target = Some(0);
            }
            self.release(&request_id, emitter);
        }
    }

    fn release(&mut self, request_id: &str, emitter: &Emitter) {
        let Some(receipt) = self.pending.get(request_id) else {
            return;
        };
        let Some(target) = receipt.target else {
            return;
        };
        if target > self.acknowledged {
            return;
        }
        let Some(receipt) = self.pending.remove(request_id) else {
            return;
        };
        let sent = emitter.receipt(WOmpUiActionReceipt {
            conversation_id: emitter.conversation_id.clone(),
            origin_request_id: request_id.to_owned(),
            client_id: receipt.origin.client_id,
            view_id: receipt.origin.view_id,
            transport_id: receipt.origin.transport_id,
            error: receipt.error,
            ..Default::default()
        });
        if !sent {
            tracing::debug!(
                conversation_id = %emitter.conversation_id,
                request_id,
                "OMP action receipt could not reach the coordinator"
            );
        }
    }
}

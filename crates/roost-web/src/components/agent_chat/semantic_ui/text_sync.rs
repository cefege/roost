//! Per-view causal barrier between optimistic text and native UI actions.
//! A click or key cannot overtake text still awaiting the controller's frame.
//! Closing a dirty native control cancels its obsolete queued actions.
use dioxus::prelude::*;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;

const MAX_QUEUED_ACTIONS: usize = 128;

#[derive(Default)]
struct TextBarrier {
    pending: BTreeSet<String>,
    actions: VecDeque<Value>,
    prerequisites: VecDeque<Value>,
}
impl TextBarrier {
    fn enqueue(&mut self, action: Value) -> Result<(), ()> {
        if self.actions.len() + self.prerequisites.len() >= MAX_QUEUED_ACTIONS {
            return Err(());
        }
        self.actions.push_back(action);
        Ok(())
    }
    fn prerequisite_pair(&mut self, first: Value, second: Value) -> Result<(), ()> {
        if self.actions.len() + self.prerequisites.len() > MAX_QUEUED_ACTIONS - 2 {
            return Err(());
        }
        self.prerequisites.extend([first, second]);
        Ok(())
    }
    fn next(&mut self) -> Option<Value> {
        if self.pending.is_empty() {
            self.prerequisites
                .pop_front()
                .or_else(|| self.actions.pop_front())
        } else {
            None
        }
    }
    fn pending(&mut self, key: &str, pending: bool) {
        if pending {
            if !self.pending.contains(key) {
                self.pending.insert(key.to_owned());
            }
        } else {
            self.pending.remove(key);
        }
    }
    fn abandon_actions(&mut self) {
        self.actions.clear();
    }
    fn cancel(&mut self, key: &str) {
        if self.pending.remove(key) {
            self.actions.clear();
            self.prerequisites.clear();
        }
    }
}

#[derive(Clone)]
pub(super) struct TextSync(Rc<RefCell<TextSyncOwner>>);
struct TextSyncOwner {
    barrier: TextBarrier,
    send: EventHandler<Value>,
    error: Signal<Option<String>>,
    failure: Signal<u64>,
    discarded_actions: Signal<u64>,
}
impl TextSync {
    pub(super) fn new(send: EventHandler<Value>, error: Signal<Option<String>>) -> Self {
        Self(Rc::new(RefCell::new(TextSyncOwner {
            barrier: TextBarrier::default(),
            send,
            error,
            failure: Signal::new(0),
            discarded_actions: Signal::new(0),
        })))
    }
    pub(super) fn sender(&self) -> EventHandler<Value> {
        self.0.borrow().send
    }
    pub(super) fn update_sender(&self, send: EventHandler<Value>) {
        self.0.borrow_mut().send = send;
    }
    pub(super) fn discard_generation(&self) -> u64 {
        (self.0.borrow().discarded_actions)()
    }
    /// Bumped only when an RPC the text path emitted failed; controls drop the
    /// in-flight edit and show the error instead of waiting forever.
    pub(super) fn input_failure(&self) -> u64 {
        (self.0.borrow().failure)()
    }
    pub(super) fn reject_input(&self, message: &str) {
        if let Ok(mut error) = self.0.borrow_mut().error.try_write() {
            *error = Some(message.into());
        }
    }
    /// Report a typed edit or send the transport refused. No automatic retry.
    pub(super) fn input_failed(&self, message: &str) {
        let mut owner = self.0.borrow_mut();
        if let Ok(mut error) = owner.error.try_write() {
            *error = Some(message.into());
        }
        if let Ok(mut failure) = owner.failure.try_write() {
            *failure = failure.wrapping_add(1);
        }
    }
    pub(super) fn enqueue(&self, action: Value) {
        let mut owner = self.0.borrow_mut();
        if owner.barrier.enqueue(action).is_err() {
            if let Ok(mut error) = owner.error.try_write() {
                *error =
                    Some("OMP is still applying input. Wait before retrying that action.".into());
            }
            return;
        }
        drop(owner);
        self.drain();
    }
    /// Finish asynchronous input before actions queued while it was pending.
    pub(super) fn prerequisite_pair(&self, first: Value, second: Value) -> bool {
        let mut owner = self.0.borrow_mut();
        if owner.barrier.prerequisite_pair(first, second).is_err() {
            if let Ok(mut error) = owner.error.try_write() {
                *error = Some("OMP has too many queued actions. Retry the attachment.".into());
            }
            return false;
        }
        drop(owner);
        self.drain();
        true
    }
    pub(super) fn pending(&self, key: &str, pending: bool) {
        self.0.borrow_mut().barrier.pending(key, pending);
        self.drain();
    }
    pub(super) fn abandon_queued_actions(&self, message: &str) {
        {
            let mut owner = self.0.borrow_mut();
            owner.barrier.abandon_actions();
            owner
                .discarded_actions
                .with_mut(|generation| *generation = generation.wrapping_add(1));
        }
        self.reject_input(message);
    }
    pub(super) fn cancel(&self, key: &str) {
        self.0.borrow_mut().barrier.cancel(key);
    }
    fn drain(&self) {
        loop {
            let next = {
                let mut owner = self.0.borrow_mut();
                owner.barrier.next().map(|action| (owner.send, action))
            };
            let Some((send, action)) = next else {
                return;
            };
            {
                let mut owner = self.0.borrow_mut();
                if let Ok(mut error) = owner.error.try_write() {
                    *error = None;
                }
            }
            send.call(action);
        }
    }
}

pub(super) fn use_text_registration(
    sync: TextSync,
    surface: &str,
    id: &str,
    scope: &str,
) -> String {
    let key = use_hook(|| {
        if scope.is_empty() {
            format!("{surface}\0{id}")
        } else {
            format!("{surface}\0{id}\0{scope}")
        }
    });
    use_drop({
        let key = key.clone();
        move || sync.cancel(&key)
    });
    key
}

#[cfg(test)]
mod tests {
    use super::TextBarrier;
    use serde_json::json;
    #[test]
    fn native_submit_waits_for_every_dirty_field_and_keeps_action_order() {
        let mut barrier = TextBarrier::default();
        barrier.pending("composer", true);
        barrier.pending("query", true);
        assert!(barrier.enqueue(json!({"act":"submit"})).is_ok());
        assert!(barrier.enqueue(json!({"key":"F2"})).is_ok());
        assert!(barrier.next().is_none());
        barrier.pending("query", false);
        assert!(barrier.next().is_none());
        barrier.pending("composer", false);
        assert_eq!(barrier.next(), Some(json!({"act":"submit"})));
        assert_eq!(barrier.next(), Some(json!({"key":"F2"})));
        assert!(barrier.next().is_none());
    }
    #[test]
    fn closing_a_dirty_control_cannot_replay_its_action_into_a_new_control() {
        let mut barrier = TextBarrier::default();
        barrier.pending("editor", true);
        assert!(barrier.enqueue(json!({"act":"save"})).is_ok());
        barrier.cancel("editor");
        barrier.pending("editor", true);
        barrier.pending("editor", false);
        assert!(barrier.next().is_none());
    }
    #[test]
    fn uploaded_paths_keep_pick_order_before_a_send_and_wait_for_text_ack() {
        let mut barrier = TextBarrier::default();
        barrier.pending("text", true);
        barrier.pending("upload-a", true);
        barrier.pending("upload-b", true);
        assert!(barrier.enqueue(json!("send")).is_ok());
        assert!(
            barrier
                .prerequisite_pair(json!("focus-a"), json!("path-a"))
                .is_ok()
        );
        barrier.pending("upload-a", false);
        assert!(barrier.next().is_none());
        assert!(
            barrier
                .prerequisite_pair(json!("focus-b"), json!("path-b"))
                .is_ok()
        );
        barrier.pending("upload-b", false);
        assert!(barrier.next().is_none());
        barrier.pending("text", false);
        for action in ["focus-a", "path-a", "focus-b", "path-b", "send"] {
            assert_eq!(barrier.next(), Some(json!(action)));
        }
        assert!(barrier.next().is_none());
    }
    #[test]
    fn closing_an_upload_owner_discards_its_paths_and_blocked_send() {
        let mut barrier = TextBarrier::default();
        barrier.pending("upload", true);
        assert!(
            barrier
                .prerequisite_pair(json!("focus"), json!("path"))
                .is_ok()
        );
        assert!(barrier.enqueue(json!("send")).is_ok());
        barrier.cancel("upload");
        assert!(barrier.next().is_none());
    }
    #[test]
    fn failed_upload_keeps_send_fenced_and_discard_does_not_send_without_the_image() {
        let mut barrier = TextBarrier::default();
        barrier.pending("failed-upload", true);
        assert!(barrier.enqueue(json!("send")).is_ok());
        assert!(
            barrier
                .prerequisite_pair(json!("focus"), json!("completed-path"))
                .is_ok()
        );
        assert!(barrier.next().is_none());
        barrier.abandon_actions();
        barrier.pending("failed-upload", false);
        assert_eq!(barrier.next(), Some(json!("focus")));
        assert_eq!(barrier.next(), Some(json!("completed-path")));
        assert!(barrier.next().is_none());
        assert!(barrier.enqueue(json!("deliberate-send")).is_ok());
        assert_eq!(barrier.next(), Some(json!("deliberate-send")));
    }
}

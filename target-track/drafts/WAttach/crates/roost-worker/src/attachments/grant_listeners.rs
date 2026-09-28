//! Who hears a grant change. Listeners are called in subscription order after
//! the store has applied the change and released its lock, so a listener may
//! read the store again. Ports the `subscribe`/`notify` half of v2
//! `apps/worker/src/attachments/attachment-grants.ts`. Owned by `grants`; the
//! direct attachment sockets subscribe.

use std::sync::{Arc, Mutex, Weak};

use super::grants::{GrantChange, lock};

pub type GrantListener = Box<dyn Fn(&GrantChange) + Send + Sync>;

/// A registered listener. `cancel` is v2's unsubscribe; dropping it does not
/// unsubscribe.
#[derive(Debug)]
pub struct GrantSubscription {
    id: u64,
    registry: Weak<Mutex<Registry>>,
}

impl GrantSubscription {
    pub fn cancel(self) {
        if let Some(registry) = self.registry.upgrade() {
            lock(&registry).entries.retain(|(id, _)| *id != self.id);
        }
    }
}

#[derive(Default)]
struct Registry {
    next_id: u64,
    entries: Vec<(u64, Arc<GrantListener>)>,
}

#[derive(Default)]
pub(super) struct GrantListeners {
    registry: Arc<Mutex<Registry>>,
}

impl GrantListeners {
    pub(super) fn subscribe(&self, listener: GrantListener) -> GrantSubscription {
        let mut registry = lock(&self.registry);
        registry.next_id += 1;
        let id = registry.next_id;
        registry.entries.push((id, Arc::new(listener)));
        GrantSubscription {
            id,
            registry: Arc::downgrade(&self.registry),
        }
    }

    /// The listener list is copied first, so a listener that subscribes or
    /// cancels while being called changes the next announcement, not this one.
    pub(super) fn notify(&self, changes: &[GrantChange]) {
        if changes.is_empty() {
            return;
        }
        let listeners: Vec<Arc<GrantListener>> = lock(&self.registry)
            .entries
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for change in changes {
            for listener in &listeners {
                listener(change);
            }
        }
    }

    pub(super) fn clear(&self) {
        lock(&self.registry).entries.clear();
    }
}

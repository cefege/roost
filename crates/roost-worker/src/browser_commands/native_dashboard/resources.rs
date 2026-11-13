//! Live ACP child registrations shared by its UI owner and HTTP capability.
//!
//! Removing an owner closes its real upstream streams immediately. A stale
//! capability cannot authorize a reused port, transport or replacement child.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use roost_protocol::native_dashboard::{DashboardTarget, ResourceKey, parse_dashboard_origin};
use tokio::sync::watch;

const MAX_RESOURCES: usize = 32;

#[derive(Debug, Default)]
pub struct DashboardResources {
    entries: Mutex<HashMap<ResourceKey, Arc<OwnedResource>>>,
}

#[derive(Debug)]
pub(super) struct StreamSlot {
    pub response: Option<reqwest::Response>,
    pub touched: Instant,
    pub cancelled: watch::Sender<bool>,
    pub expiry: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
pub(super) struct OwnedResource {
    pub key: ResourceKey,
    pub owner_id: Arc<str>,
    pub target: DashboardTarget,
    pub closed: watch::Sender<bool>,
    pub streams: Mutex<HashMap<String, StreamSlot>>,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.cancelled.send_replace(true);
        self.expiry.abort();
    }
}

impl DashboardResources {
    pub fn register(&self, owner_id: &Arc<str>, key: ResourceKey, origin: &str) -> bool {
        if !key.is_valid() {
            return false;
        }
        let Some(target) = parse_dashboard_origin(origin) else {
            return false;
        };
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        if let Some(existing) = entries.get(&key) {
            return existing.owner_id == *owner_id
                && existing.target == target
                && !*existing.closed.borrow();
        }
        if entries.len() >= MAX_RESOURCES {
            return false;
        }
        let (closed, _) = watch::channel(false);
        tracing::info!(conversation_id = %key.conversation_id, transport_id = %key.transport_id, "OMP stats resource registered");
        entries.insert(
            key.clone(),
            Arc::new(OwnedResource {
                key,
                owner_id: Arc::clone(owner_id),
                target,
                closed,
                streams: Mutex::new(HashMap::new()),
            }),
        );
        true
    }

    pub fn contains(&self, key: &ResourceKey, host: &str, port: u16) -> bool {
        self.get(key, host, port).is_some()
    }

    pub(super) fn get(
        &self,
        key: &ResourceKey,
        host: &str,
        port: u16,
    ) -> Option<Arc<OwnedResource>> {
        let entries = self.entries.lock().ok()?;
        let resource = entries.get(key)?;
        (resource.target.host == host && resource.target.port == port && !*resource.closed.borrow())
            .then(|| Arc::clone(resource))
    }

    pub fn revoke(&self, owner_id: &str, key: &ResourceKey) {
        self.remove_where(|candidate, resource| {
            candidate == key && resource.owner_id.as_ref() == owner_id
        });
    }

    pub fn revoke_transport(&self, owner_id: &str, conversation_id: &str, transport_id: &str) {
        self.remove_where(|key, resource| {
            resource.owner_id.as_ref() == owner_id
                && key.conversation_id == conversation_id
                && key.transport_id == transport_id
        });
    }

    pub fn revoke_owner(&self, owner_id: &str) {
        self.remove_where(|_, resource| resource.owner_id.as_ref() == owner_id);
    }

    fn remove_where(&self, predicate: impl Fn(&ResourceKey, &OwnedResource) -> bool) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|key, resource| {
            if !predicate(key, resource) { return true; }
            resource.closed.send_replace(true);
            if let Ok(mut streams) = resource.streams.lock() { streams.clear(); }
            tracing::info!(conversation_id = %key.conversation_id, transport_id = %key.transport_id, "OMP stats resource revoked");
            false
        });
    }
}

pub(crate) struct ResourceOwner {
    pub registry: Arc<DashboardResources>,
    pub owner_id: Arc<str>,
}

impl Drop for ResourceOwner {
    fn drop(&mut self) {
        self.registry.revoke_owner(&self.owner_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(transport: &str) -> ResourceKey {
        ResourceKey {
            conversation_id: "c1".into(),
            transport_id: transport.into(),
            resource_id: "a".repeat(64),
        }
    }

    #[test]
    fn ownership_cannot_cross_a_transport_or_outlive_its_child() {
        let registry = Arc::new(DashboardResources::default());
        let owner_id: Arc<str> = "child:1".into();
        assert!(registry.register(&owner_id, key("t1"), "http://127.0.0.1:3847"));
        assert!(registry.contains(&key("t1"), "127.0.0.1", 3847));
        assert!(!registry.contains(&key("t2"), "127.0.0.1", 3847));
        assert!(!registry.contains(&key("t1"), "127.0.0.1", 3848));
        let resource = registry.get(&key("t1"), "127.0.0.1", 3847);
        assert!(resource.is_some());
        let replacement: Arc<str> = "child:2".into();
        assert!(registry.register(&replacement, key("t2"), "http://127.0.0.1:3847"));
        let owner = ResourceOwner {
            registry: Arc::clone(&registry),
            owner_id,
        };
        drop(owner);
        assert!(resource.is_some_and(|resource| *resource.closed.borrow()));
        assert!(!registry.contains(&key("t1"), "127.0.0.1", 3847));
        assert!(registry.contains(&key("t2"), "127.0.0.1", 3847));
    }
}

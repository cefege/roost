//! Web Push: the three subscription RPCs, the VAPID identity they share with
//! delivery, and the bounded dispatch a delayed agent transition fans out
//! through. Owned by the coordinator; `rpc` mounts the handlers and the agents
//! domain calls [`dispatch::fire_push_for_transition`].
//!
//! VAPID IS A COORDINATOR-GLOBAL IDENTITY, not a tenant setting. It lives in
//! the `dashboard_id IS NULL` scope of `app_settings` and is read and written
//! nowhere else, so a per-dashboard row can never shadow it.
//!
//! PROVIDER ORIGINS ARE OPERATOR CONFIG, and an empty allowlist disables the
//! whole surface rather than admitting anything: the endpoints are
//! attacker-supplied and only an exact HTTPS origin the operator declared may
//! receive a payload.

pub mod dispatch;
pub mod endpoint_policy;
pub mod rpc;
pub mod sender;
pub mod subscription_store;
pub mod transport;
pub mod vapid;

use std::fmt;
use std::sync::Arc;

use crate::push::vapid::{P256KeypairGenerator, VapidKeyStore, VapidKeys};

/// Everything a push call needs that is not the request: the resolved
/// operator config, the tenancy scope rows are written under, and the VAPID
/// identity shared by the RPC and delivery.
///
/// This is v2's `ConnectDeps` narrowed to the three values the push surface
/// actually reads (`cfg`, `db`, `selfHostedTenant`), carried on `CoordCore` so
/// a handler reaches them without the service impl.
#[derive(Clone)]
pub struct PushRuntime {
    dashboard_id: String,
    allowed_origins: Vec<String>,
    vapid_keys: VapidKeyStore,
}

impl PushRuntime {
    /// A runtime over an already-resolved configuration.
    #[must_use]
    pub fn new(dashboard_id: String, allowed_origins: Vec<String>) -> Self {
        Self {
            dashboard_id,
            allowed_origins,
            vapid_keys: VapidKeyStore::new(Arc::new(P256KeypairGenerator)),
        }
    }

    /// A runtime whose VAPID identity comes from a caller-supplied generator.
    ///
    /// The seam exists so a test can COUNT generations; production always uses
    /// [`P256KeypairGenerator`].
    #[must_use]
    pub fn with_keypair_generator(
        dashboard_id: String,
        allowed_origins: Vec<String>,
        generator: Arc<dyn vapid::VapidKeyGenerator>,
    ) -> Self {
        Self {
            dashboard_id,
            allowed_origins,
            vapid_keys: VapidKeyStore::new(generator),
        }
    }

    /// The dashboard every `push_subscriptions` row is scoped to.
    ///
    /// The table refuses a NULL scope in a trigger, so this is never optional.
    #[must_use]
    pub fn dashboard_id(&self) -> &str {
        &self.dashboard_id
    }

    /// The exact HTTPS origins a push service may be reached on.
    #[must_use]
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// The shared VAPID identity, cached after first use.
    #[must_use]
    pub fn vapid_keys(&self) -> &VapidKeyStore {
        &self.vapid_keys
    }

    /// The stored VAPID keypair, loading or generating it on first use.
    pub async fn keys(
        &self,
        database: &crate::db::CoordDb,
    ) -> Result<VapidKeys, vapid::VapidError> {
        self.vapid_keys.keys(database).await
    }
}

/// Written by hand because the store holds a trait object: a derived rendering
/// of one would be noise, and this is the shape `CoordCore` is asked for.
impl fmt::Debug for PushRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PushRuntime")
            .field("dashboard_id", &self.dashboard_id)
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

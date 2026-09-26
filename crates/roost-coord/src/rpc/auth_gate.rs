// The one place a Connect request is authenticated, authorized and stamped.
//
// Before this existed, `write_gate::method_holds_lease`,
// `method_audit_skips_success` and `should_persist_method_audit` had no caller
// at all: the policy was ported, the gate was not, so every method was
// reachable without a credential and the `AuthRequirement` column of the route
// table was documentation rather than enforcement.
//
// `connectrpc::Router` has no interceptor hook -- interceptors exist only on
// `Service<D>` -- so the listener mounts the generated server directly and
// attaches this to it. That is why this file is separate from the listener: the
// mount is one line and the policy is the whole gate.
//
// Owned by the coordinator's RPC layer. Depends on `auth::` for verification
// and `rpc::method_route` for the per-method requirement; it decides nothing
// that those two do not already decide.

use std::sync::Arc;

use connectrpc::interceptor::{Interceptor, Next, UnaryRequest, UnaryResponse};
use connectrpc::{ConnectError, ErrorCode, RequestContext};

use crate::coord_core::{Caller, CoordCore, ListenerTrust};
use crate::rpc::method_route::{AuthRequirement, auth_requirement};
use crate::rpc::service::{permission_denied_for, principal_satisfies};

/// The header a bearer credential arrives in.
const AUTHORIZATION: &str = "authorization";

/// Authenticates every unary and streaming RPC before a handler sees it.
#[derive(Debug, Clone)]
pub struct AuthGate {
    /// The shared process state: database, key cache, write gate.
    core: CoordCore,
    /// The server-side token ceiling in seconds.
    jwt_max_age_secs: u64,
    /// Whether the listener behind this gate saw peers directly. A forwarded
    /// connection cannot assert `on_host` from an address it did not observe.
    listener_trust: ListenerTrust,
}

impl AuthGate {
    /// A gate over the process's own state.
    #[must_use]
    pub fn new(core: CoordCore, jwt_max_age_secs: u64, listener_trust: ListenerTrust) -> Self {
        Self {
            core,
            jwt_max_age_secs,
            listener_trust,
        }
    }

    /// The bearer token offered on this request, if any.
    fn offered_token(context: &RequestContext) -> Option<String> {
        let raw = context.headers().get(AUTHORIZATION)?.to_str().ok()?;
        let token = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer "))?;
        let token = token.trim();
        (!token.is_empty()).then(|| token.to_owned())
    }

    /// The tab id this request claims, when it claims one.
    fn claimed_tab_id(context: &RequestContext) -> Option<String> {
        let raw = context.headers().get("x-roost-tab-id")?.to_str().ok()?;
        let tab = raw.trim();
        (!tab.is_empty()).then(|| tab.to_owned())
    }

    /// Resolve the credential, or explain the refusal.
    async fn resolve(
        &self,
        context: &RequestContext,
    ) -> Result<crate::auth::authenticate::AuthenticatedCaller, ConnectError> {
        let Some(token) = Self::offered_token(context) else {
            return Err(ConnectError::new(
                ErrorCode::Unauthenticated,
                "no bearer credential",
            ));
        };
        let authenticator = crate::auth::authenticate::Authenticator {
            database: &self.core.services.db,
            keys: &self.core.services.jwt_keys,
            clock: crate::auth::jwt_verify::VerifyClock::at(crate::rpc::service::now_ms()),
            jwt_max_age_secs: self.jwt_max_age_secs,
        };
        authenticator
            .authenticate(&token)
            .await
            .map_err(|failure| ConnectError::new(ErrorCode::Unauthenticated, describe(&failure)))
    }

    /// Authorize and stamp one request, or refuse before the handler runs.
    async fn admit(&self, request: &mut UnaryRequest) -> Result<(), ConnectError> {
        let Some(spec) = request.ctx.spec() else {
            return Err(ConnectError::new(
                ErrorCode::Internal,
                "request carried no procedure spec",
            ));
        };
        let method = spec.method();
        let requirement = auth_requirement(method).unwrap_or(AuthRequirement::Public);

        // A public method still gets a caller when a valid credential rides
        // along, so a handler can personalise its answer; it is not required to
        // present one.
        let authenticated = self.resolve(&request.ctx).await.ok();
        let Some(authenticated) = authenticated else {
            if matches!(requirement, AuthRequirement::Public) {
                return Ok(());
            }
            return Err(ConnectError::new(
                ErrorCode::Unauthenticated,
                format!("{method} requires a credential"),
            ));
        };

        if !principal_satisfies(Some(&authenticated.principal), requirement) {
            return Err(permission_denied_for(requirement));
        }

        let remote_address = request.ctx.peer_addr().map(|addr| addr.to_string());
        let on_host = self
            .listener_trust
            .asserts_locality()
            .then(|| remote_address.as_deref().is_some_and(is_local_address))
            .unwrap_or(false);

        let caller = Caller {
            principal: authenticated.principal,
            tab_id: Self::claimed_tab_id(&request.ctx),
            remote_address,
            on_host,
            listener_trust: self.listener_trust,
        };
        request.ctx.extensions_mut().insert(caller);
        Ok(())
    }
}

/// The operator-facing reason for a refusal.
///
/// The variant name is the reason: it is what the log line and the peer both
/// need, and a second description of the same enum would be a second thing to
/// keep in step with it.
fn describe(failure: &crate::auth::authenticate::AuthFailure) -> String {
    format!("{failure:?}")
}

/// Whether an address the listener observed directly is on this host.
fn is_local_address(address: &str) -> bool {
    address.starts_with("127.") || address == "::1" || address == "[::1]"
}

#[async_trait::async_trait]
impl Interceptor for AuthGate {
    async fn intercept_unary(
        &self,
        mut request: UnaryRequest,
        next: Next<'_>,
    ) -> Result<UnaryResponse, ConnectError> {
        self.admit(&mut request).await?;
        next.run(request).await
    }
}

/// Build the gate the listener mounts, over the shared process state.
#[must_use]
pub fn auth_gate(core: CoordCore, jwt_max_age_secs: u64) -> Arc<AuthGate> {
    Arc::new(AuthGate::new(
        core,
        jwt_max_age_secs,
        ListenerTrust::DirectLoopback,
    ))
}

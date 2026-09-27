// The shared half of the wiring tests: a coordinator with a migrated database
// and the one service implementation, the caller an auth interceptor would have
// stored, and the two helpers every arm assertion needs.
//
// Split from the test files for the 400-line cap, and because the seven arms
// added with the UI and push domains are asserted in a file of their own. This
// module is the single owner of the fixture; two copies of it would be two
// databases and two ways to build a caller.
//
// `unwrap`/`expect` are denied outside `#[cfg(test)]`, and an integration test
// is its own crate rather than a module of one, so the exemption is stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Each of the two wiring binaries uses part of this fixture and neither uses
// all of it, so "never used" here means "not used by the binary that happened
// to compile this module", which is not a defect in the fixture.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, RequestContext};
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::rpc::service::CoordinatorServiceImpl;
use roost_coord::services::CoordServices;

/// The account device an operator acts as, and the machine a worker speaks as.
pub const DEVICE_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const WORKER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A coordinator with a migrated database and the one service implementation.
pub struct ServiceFixture {
    pub service: CoordinatorServiceImpl,
    pub root: PathBuf,
}

impl ServiceFixture {
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-service-wiring-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let config = roost_host::CoordConfig::parse(roost_host::CoordConfigInput {
            db_path: Some(root.join("coord.db")),
            authorized_keys_path: Some(root.join("authorized_keys")),
            log_dir: Some(root.join("logs")),
            ..Default::default()
        })
        .expect("a coordinator config");
        let core = CoordCore::new(Arc::new(CoordServices::new(database)));
        Self {
            service: CoordinatorServiceImpl::new(
                core,
                config,
                "epoch-1".to_owned(),
                0,
                "sha".to_owned(),
            ),
            root,
        }
    }

    /// The account device an operator acts as.
    pub fn device_caller(&self) -> Caller {
        caller_of(Principal::AccountDevice {
            fingerprint: DEVICE_FP.to_owned(),
            label: "test device".to_owned(),
            account_id: "acct_self_hosted".to_owned(),
        })
    }

    /// A worker, who may not read the fleet view.
    pub fn worker_caller(&self) -> Caller {
        caller_of(Principal::Worker {
            fingerprint: WORKER_FP.to_owned(),
            label: "test worker".to_owned(),
        })
    }

    /// A device that also carries the tab fence every UI method requires.
    pub fn tab_device_caller(&self) -> Caller {
        Caller {
            tab_id: Some("tab-1".to_owned()),
            ..self.device_caller()
        }
    }
}

impl Drop for ServiceFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn caller_of(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A context carrying the caller the auth interceptor would have stored.
pub fn context_with(caller: Caller) -> RequestContext {
    let mut context = RequestContext::default();
    context.extensions_mut().insert(caller);
    context
}

/// Bind `$name` to the `ServiceRequest` a generated dispatcher hands an arm,
/// decoded from a body the caller names.
///
/// The request borrows the view and the view borrows the body, so all three are
/// declared in one scope and dropped in the order that keeps both borrows
/// alive; a generated dispatcher owns the body for the length of the call, and
/// so does this.
macro_rules! service_request {
    ($name:ident, $request:ty) => {
        service_request!($name, $request, <$request>::default())
    };
    ($name:ident, $request:ty, $message:expr) => {
        let body = ::roost_proto::buffa::bytes::Bytes::from(
            ::roost_proto::buffa::Message::encode_to_vec(&$message),
        );
        let view = <$request as ::roost_proto::buffa::HasMessageView>::decode_view(&body)
            .expect("the request decodes");
        let $name = ::connectrpc::ServiceRequest::<$request>::from_parts(&view, &body);
    };
}
pub(crate) use service_request;

/// The refusal a wired method gives for a request with no caller on it: the
/// named `Unimplemented` it would give for an absent domain.
///
/// Naming the method is the point -- a refusal that does not say which method it
/// is cannot be told apart from the sixteen methods v2 never answered -- and
/// `Unimplemented` is what proves no handler ran: every handler here answers a
/// missing caller with `PermissionDenied` or a domain error, and none of them
/// answers with a refusal that names the method.
pub fn assert_named_refusal(error: ConnectError, method: &str) {
    assert_eq!(error.code, ErrorCode::Unimplemented, "{method}");
    let message = error.message.clone().unwrap_or_default();
    assert!(
        message.starts_with(&format!("{method}:")),
        "{method} refused without naming itself: {message}"
    );
}

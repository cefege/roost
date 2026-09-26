// The process-wide handle every coordinator domain handler is given.
//
// `CoordinatorServiceImpl` deliberately owns no domain state, so a handler that
// needs the database, the write gate or the JWT cache would otherwise have to
// reach through the service impl -- which is exactly the single file every
// domain slice also has to edit, and a second place state lives. `CoordCore` is
// the one place a handler can be handed everything, and adding a new
// per-process singleton means adding a field here rather than widening the
// service impl.
//
// Owned by the coordinator. Constructed once in `serve` and shared as an
// `Arc`; the caller is NOT in here, because it is per request and travels in
// the request's extensions instead.

use std::sync::Arc;

use crate::coord_core::seams::CoordTerminal;
use crate::push::PushRuntime;
use crate::services::CoordServices;

/// The shared coordinator state, handed to every domain handler.
#[derive(Debug, Clone)]
pub struct CoordCore {
    /// The per-process singletons: database, write gate, key cache, pending
    /// publications.
    pub services: Arc<CoordServices>,
    /// The terminal seams the workers domain consumes, behind traits so this
    /// module never names a terminal type.
    pub terminal: CoordTerminal,
    /// The push runtime, when the coordinator was built with a tenancy scope
    /// and an operator push allowlist.
    ///
    /// `Option` because the two inputs are boot-order facts the coordinator may
    /// not have: `serve` learns the dashboard id from the self-hosted tenant
    /// invariant and the origins from the resolved config. A `None` is a
    /// wiring fault and the push handlers say so as `Internal`, rather than
    /// reading as "Push is switched off" -- a deployment that meant to enable
    /// Push and did not must not look like one that chose not to.
    pub push: Option<PushRuntime>,
}

impl CoordCore {
    /// A core over already-constructed services.
    #[must_use]
    pub fn new(services: Arc<CoordServices>) -> Self {
        Self {
            services,
            terminal: CoordTerminal::none(),
            push: None,
        }
    }

    /// A core with real terminal collaborators rather than the no-op seams.
    #[must_use]
    pub fn with_terminal(services: Arc<CoordServices>, terminal: CoordTerminal) -> Self {
        Self {
            services,
            terminal,
            push: None,
        }
    }

    /// A core with the push runtime installed, for the push handlers.
    #[must_use]
    pub fn with_push(services: Arc<CoordServices>, push: PushRuntime) -> Self {
        Self {
            services,
            terminal: CoordTerminal::none(),
            push: Some(push),
        }
    }

    /// The same core with both collaborators installed.
    #[must_use]
    pub fn with_terminal_and_push(
        services: Arc<CoordServices>,
        terminal: CoordTerminal,
        push: PushRuntime,
    ) -> Self {
        Self {
            services,
            terminal,
            push: Some(push),
        }
    }
}

//! The one worker-owned budget for browser-triggered terminal work: input
//! batches from the coordinator link ("sync") and from direct ports, and route
//! claims. Callers reserve before copying bytes or entering keeper admission,
//! and a reservation is released exactly once — by being dropped — so a slow
//! peer cannot build an unbounded chain of waiting work. Called by
//! `terminal_input::port` and `terminal_input::route_owner`, and the local door.
//! Ports `apps/worker/src/terminal/terminal-input-work-budget.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

pub const TERMINAL_INPUT_WORK_MAX_REQUESTS: usize = 256;
pub const TERMINAL_INPUT_WORK_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS: usize = 32;
pub const TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS: usize = 32;
pub const TERMINAL_DIRECT_ROUTE_CLAIM_WORK_MAX_REQUESTS: usize = 4;
/// The refusal an input reservation carries; the result is pre-write.
pub const INPUT_ADMISSION_FULL: &str = "worker input admission is full";
/// The refusal a route-claim reservation carries.
pub const ROUTE_CLAIM_BUSY: &str = "route_claim_busy";

/// Where a batch came from. A direct port has a per-port ceiling of its own;
/// the coordinator link spends only the worker-wide one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputWorkOrigin {
    Sync,
    Direct { port_id: String },
}

#[derive(Debug, Default, Clone, Copy)]
struct PortUsage {
    input_count: usize,
    input_bytes: usize,
    claim_count: usize,
}

impl PortUsage {
    fn is_unused(self) -> bool {
        self.input_count == 0 && self.input_bytes == 0 && self.claim_count == 0
    }
}

#[derive(Debug, Default)]
struct BudgetState {
    ports: HashMap<String, PortUsage>,
    input_count: usize,
    input_bytes: usize,
    claim_count: usize,
    /// actor/session key → the claim reservation holding it.
    claims: HashMap<String, u64>,
    next_claim: u64,
    /// Bumped by `dispose`: a reservation from an earlier epoch releases nothing.
    epoch: u64,
    disposed: bool,
}

/// v2 `TerminalInputWorkBudget`. Cheap to clone; every clone is one budget.
#[derive(Debug, Clone, Default)]
pub struct TerminalInputWorkBudget {
    state: Arc<Mutex<BudgetState>>,
}

/// One admitted input batch's share of the budget, returned on drop.
#[derive(Debug)]
pub struct InputWorkReservation {
    state: Arc<Mutex<BudgetState>>,
    epoch: u64,
    byte_length: usize,
    port_id: Option<String>,
}

/// One admitted route claim's share of the budget, returned on drop.
#[derive(Debug)]
pub struct RouteClaimReservation {
    state: Arc<Mutex<BudgetState>>,
    epoch: u64,
    id: u64,
    port_id: String,
    actor_session_key: String,
}

impl TerminalInputWorkBudget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve one input batch of `byte_length` bytes, or refuse it with
    /// [`INPUT_ADMISSION_FULL`].
    pub fn reserve_input(
        &self,
        origin: &InputWorkOrigin,
        byte_length: usize,
    ) -> Result<InputWorkReservation, &'static str> {
        let mut state = lock(&self.state);
        if !can_reserve_input(&state, origin, byte_length) {
            tracing::warn!(
                ?origin,
                byte_length,
                input_count = state.input_count,
                "terminal input work refused: the budget is full"
            );
            return Err(INPUT_ADMISSION_FULL);
        }
        state.input_count += 1;
        state.input_bytes += byte_length;
        let port_id = match origin {
            InputWorkOrigin::Sync => None,
            InputWorkOrigin::Direct { port_id } => {
                let usage = state.ports.entry(port_id.clone()).or_default();
                usage.input_count += 1;
                usage.input_bytes += byte_length;
                Some(port_id.clone())
            }
        };
        Ok(InputWorkReservation {
            state: Arc::clone(&self.state),
            epoch: state.epoch,
            byte_length,
            port_id,
        })
    }

    /// Reserve one route claim for a port (a browser connection or a direct
    /// socket) and an actor/session, or refuse it with [`ROUTE_CLAIM_BUSY`]. One
    /// actor/session holds at most one claim at a time.
    pub fn reserve_route_claim(
        &self,
        port_id: &str,
        actor_session_key: &str,
    ) -> Result<RouteClaimReservation, &'static str> {
        let mut state = lock(&self.state);
        let port_claims = state
            .ports
            .get(port_id)
            .map_or(0, |usage| usage.claim_count);
        if state.disposed
            || port_id.is_empty()
            || actor_session_key.is_empty()
            || state.claims.contains_key(actor_session_key)
            || state.claim_count >= TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS
            || port_claims >= TERMINAL_DIRECT_ROUTE_CLAIM_WORK_MAX_REQUESTS
        {
            tracing::info!(
                port_id,
                claim_count = state.claim_count,
                "terminal route claim refused: the budget is busy"
            );
            return Err(ROUTE_CLAIM_BUSY);
        }
        state.next_claim += 1;
        let id = state.next_claim;
        state.claims.insert(actor_session_key.to_owned(), id);
        state.claim_count += 1;
        state
            .ports
            .entry(port_id.to_owned())
            .or_default()
            .claim_count += 1;
        Ok(RouteClaimReservation {
            state: Arc::clone(&self.state),
            epoch: state.epoch,
            id,
            port_id: port_id.to_owned(),
            actor_session_key: actor_session_key.to_owned(),
        })
    }

    /// Refuse everything from now on and forget every outstanding charge; a
    /// reservation dropped afterwards returns nothing.
    pub fn dispose(&self) {
        let mut state = lock(&self.state);
        if state.disposed {
            return;
        }
        let epoch = state.epoch + 1;
        *state = BudgetState {
            epoch,
            disposed: true,
            ..BudgetState::default()
        };
        tracing::info!("terminal input work budget disposed");
    }
}

impl Drop for InputWorkReservation {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        if state.epoch != self.epoch {
            return;
        }
        state.input_count -= 1;
        state.input_bytes -= self.byte_length;
        if let Some(port_id) = &self.port_id {
            release_port(&mut state, port_id, |usage| {
                usage.input_count -= 1;
                usage.input_bytes -= self.byte_length;
            });
        }
    }
}

impl Drop for RouteClaimReservation {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        if state.epoch != self.epoch {
            return;
        }
        if state.claims.get(&self.actor_session_key) == Some(&self.id) {
            state.claims.remove(&self.actor_session_key);
        }
        state.claim_count -= 1;
        release_port(&mut state, &self.port_id, |usage| usage.claim_count -= 1);
    }
}

fn can_reserve_input(state: &BudgetState, origin: &InputWorkOrigin, byte_length: usize) -> bool {
    if state.disposed
        || state.input_count >= TERMINAL_INPUT_WORK_MAX_REQUESTS
        || byte_length > TERMINAL_INPUT_WORK_MAX_BYTES - state.input_bytes
    {
        return false;
    }
    let InputWorkOrigin::Direct { port_id } = origin else {
        return true;
    };
    if port_id.is_empty() {
        return false;
    }
    let usage = state.ports.get(port_id).copied().unwrap_or_default();
    usage.input_count < TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS
        && byte_length <= TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES - usage.input_bytes
}

fn release_port(state: &mut BudgetState, port_id: &str, release: impl FnOnce(&mut PortUsage)) {
    let Some(usage) = state.ports.get_mut(port_id) else {
        return;
    };
    release(usage);
    if usage.is_unused() {
        state.ports.remove(port_id);
    }
}

fn lock(state: &Mutex<BudgetState>) -> MutexGuard<'_, BudgetState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

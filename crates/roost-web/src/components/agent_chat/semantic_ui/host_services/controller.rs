//! The root-scoped controller behind OMP host-service prompts.
//!
//! One controller lives in the command surface for the lifetime of its native
//! view and transport. It owns the pending snapshot, the per-service attempt
//! state, and dispatches every browser action, so a dialog remount changes
//! only the DOM: no effect is started twice and no reply task is lost.

use std::collections::{BTreeMap, BTreeSet};

use dioxus::prelude::*;
use roost_client_core::client::agent_chat::{OmpHostService, OmpHostServiceKind};

use crate::pump::Pump;
use crate::router_state::use_navigate;

use super::actions::{attempt_copy, attempt_open, attempt_stats, dismiss_service};
use super::browser::user_activation_active;
use super::live_audio::LiveAudioOwner;

/// Everything that scopes one controller to the mounted native view.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct OmpHostServiceConfig {
    pub(super) conversation_id: String,
    pub(super) view_id: String,
    pub(super) transport_id: String,
    pub(super) worker_fp: String,
    pub(super) cwd: String,
}

/// The root-owned state and actions the presentational prompts consume.
#[derive(Clone, Copy)]
pub(crate) struct OmpHostServices {
    requests: Signal<Vec<OmpHostService>>,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
    actions: OmpHostServiceActions,
}

impl OmpHostServices {
    /// The services this exact view and transport still hold.
    pub(crate) fn requests(&self) -> Vec<OmpHostService> {
        self.requests.read().clone()
    }

    /// Whether an effect for this service is awaiting the browser or worker.
    pub(crate) fn is_sending(&self, service_id: &str) -> bool {
        self.sending.read().contains(service_id)
    }

    /// The last concrete refusal for this service, if any.
    pub(crate) fn failure(&self, service_id: &str) -> Option<String> {
        self.failures.read().get(service_id).cloned()
    }

    pub(crate) fn copy(&self, service: OmpHostService) {
        self.actions.copy.call(service);
    }

    pub(crate) fn open(&self, service: OmpHostService) {
        self.actions.open.call(service);
    }

    pub(crate) fn open_stats(&self, service: OmpHostService) {
        self.actions.stats.call(service);
    }

    pub(crate) fn open_fork(&self, service: OmpHostService) {
        self.actions.fork.call(service);
    }

    pub(crate) fn dismiss(&self, service: OmpHostService) {
        self.actions.dismiss.call(service);
    }
}

#[derive(Clone, Copy)]
struct OmpHostServiceActions {
    copy: EventHandler<OmpHostService>,
    open: EventHandler<OmpHostService>,
    stats: EventHandler<OmpHostService>,
    fork: EventHandler<OmpHostService>,
    dismiss: EventHandler<OmpHostService>,
}

/// Provide the one controller for this command surface.
///
/// Call once, before any early return, in the component that owns the native
/// view. The controller follows the view's transport identity; the prompt
/// cards mount later and read it from context.
pub(crate) fn use_omp_host_services(
    pump: Pump,
    conversation_id: String,
    view_id: String,
    transport_id: String,
    worker_fp: String,
    cwd: String,
    on_action: EventHandler<serde_json::Value>,
) -> OmpHostServices {
    let config = OmpHostServiceConfig {
        conversation_id,
        view_id,
        transport_id,
        worker_fp,
        cwd,
    };
    let initial_config = config.clone();
    let mut config_signal = use_signal(move || initial_config);
    if *config_signal.peek() != config {
        config_signal.set(config.clone());
    }

    let mut requests = use_signal({
        let initial_pump = pump.clone();
        let initial_config = config.clone();
        move || pending_services(&initial_pump, &initial_config)
    });
    let mut attempted = use_signal(BTreeSet::<String>::new);
    let mut sending = use_signal(BTreeSet::<String>::new);
    let mut failures = use_signal(BTreeMap::<String, String>::new);
    let navigate = use_navigate();
    let live_audio = use_hook({
        let pump = pump.clone();
        let config = config.clone();
        move || LiveAudioOwner::new(pump, config, on_action)
    });
    live_audio.update(&config, on_action);
    use_drop({
        let live_audio = live_audio.clone();
        move || live_audio.dispose()
    });

    let copy_pump = pump.clone();
    let open_pump = pump.clone();
    let stats_pump = pump.clone();
    let fork_pump = pump.clone();
    let dismiss_pump = pump.clone();
    let open_config = config.clone();
    let actions = use_hook(move || OmpHostServiceActions {
        copy: EventHandler::new(move |service: OmpHostService| {
            attempt_copy(copy_pump.clone(), service, sending, failures);
        }),
        open: EventHandler::new(move |service: OmpHostService| {
            let config = match config_signal.try_read() {
                Ok(config) => config.clone(),
                Err(_) => open_config.clone(),
            };
            attempt_open(
                open_pump.clone(),
                service,
                config,
                navigate,
                sending,
                failures,
            );
        }),
        stats: EventHandler::new(move |service: OmpHostService| {
            attempt_stats(stats_pump.clone(), service, sending, failures);
        }),
        fork: EventHandler::new(move |service: OmpHostService| {
            super::fork::attempt(fork_pump.clone(), service, sending, failures);
        }),
        dismiss: EventHandler::new(move |service: OmpHostService| {
            dismiss_service(dismiss_pump.clone(), service, failures);
        }),
    });

    let services = use_context_provider(|| OmpHostServices {
        requests,
        sending,
        failures,
        actions,
    });

    let revision = pump.revision()();
    let sync_pump = pump.clone();
    let sync_audio = live_audio.clone();
    use_effect(use_reactive((&revision, &config), move |(_, config)| {
        let next = pending_services(&sync_pump, &config);
        sync_audio.reconcile(&next);
        let active: BTreeSet<String> = next
            .iter()
            .map(|service| service.service_id.clone())
            .collect();
        if *requests.peek() != next {
            requests.set(next);
        }
        if let (Ok(mut attempted), Ok(mut sending), Ok(mut failures)) = (
            attempted.try_write(),
            sending.try_write(),
            failures.try_write(),
        ) {
            prune_attempt_state(&active, &mut attempted, &mut sending, &mut failures);
        }
    }));

    let auto_pump = pump.clone();
    let auto_audio = live_audio.clone();
    let auto_ids: Vec<String> = requests()
        .iter()
        .map(|service| service.service_id.clone())
        .collect();
    use_effect(use_reactive((&auto_ids, &config), move |(ids, config)| {
        for service_id in ids {
            if attempted.peek().contains(&service_id) {
                continue;
            }
            let Some(service) = pending_services(&auto_pump, &config)
                .into_iter()
                .find(|service| service.service_id == service_id)
            else {
                continue;
            };
            attempted.write().insert(service_id.clone());
            match service.kind {
                OmpHostServiceKind::CopyText => {
                    attempt_copy(auto_pump.clone(), service, sending, failures);
                }
                OmpHostServiceKind::OpenUrl if user_activation_active() => {
                    attempt_open(
                        auto_pump.clone(),
                        service,
                        config.clone(),
                        navigate,
                        sending,
                        failures,
                    );
                }
                OmpHostServiceKind::OpenStatsDashboard if user_activation_active() => {
                    attempt_stats(auto_pump.clone(), service, sending, failures);
                }
                OmpHostServiceKind::OpenFork => {
                    super::fork::attempt(auto_pump.clone(), service, sending, failures);
                }
                OmpHostServiceKind::LiveAudio => {
                    auto_audio.attempt(auto_pump.clone(), service);
                }
                _ => {}
            }
        }
    }));

    services
}

fn pending_services(pump: &Pump, config: &OmpHostServiceConfig) -> Vec<OmpHostService> {
    if config.transport_id.is_empty() {
        return Vec::new();
    }
    let core = pump.core();
    let core = core.borrow();
    core.store().omp_host.pending_services(
        &config.conversation_id,
        &config.view_id,
        &config.transport_id,
    )
}

/// Drop attempt state for services the store no longer holds.
pub(super) fn prune_attempt_state(
    active: &BTreeSet<String>,
    attempted: &mut BTreeSet<String>,
    sending: &mut BTreeSet<String>,
    failures: &mut BTreeMap<String, String>,
) {
    attempted.retain(|service_id| active.contains(service_id));
    sending.retain(|service_id| active.contains(service_id));
    failures.retain(|service_id, _| active.contains(service_id));
}

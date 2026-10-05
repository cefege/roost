//! The OS hand-offs a sidebar row menu lists for its machine (Screen sharing,
//! Open in Finder, Remote Desktop, Copy network share path), disabled with a
//! tooltip until the worker has reported a reachable address. Rendered inside
//! `FolderRowContextMenu` and `SessionRowContextMenu`; the list and URLs are
//! `machine_actions`', the browser steps `platform::{location, file_save}`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::context_menu::CtxMenuItem;
use crate::components::notifications::clipboard::copy_text;
use crate::machine_actions::{
    MachineAction, MachineLaunch, MachineMenuKind, NO_REACHABLE_ADDRESS_TOOLTIP, machine_actions,
    machine_launch, reachable_host,
};
use crate::platform::file_save::save_text_file;
use crate::platform::location::hand_off_to_os;
use crate::pump::{Pump, use_store};

/// One item per hand-off `worker_fp`'s platform offers in `menu`; nothing for
/// a machine that offers none or is not in the roster. Each item's test id is
/// `{test_id_prefix}-{action}{test_id_suffix}`.
#[component]
pub fn MachineActionItems(
    worker_fp: String,
    menu: MachineMenuKind,
    test_id_prefix: String,
    #[props(default)] test_id_suffix: String,
    on_close: EventHandler<()>,
) -> Element {
    let pump = use_store();
    let machine = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .workers
            .get(worker_fp.as_str())
            .map(|worker| (worker.os, reachable_host(worker).map(str::to_owned)))
    };
    let Some((os, host)) = machine else {
        return rsx! {};
    };
    let disabled = host.is_none();
    let title = disabled.then(|| NO_REACHABLE_ADDRESS_TOOLTIP.to_owned());
    rsx! {
        for action in machine_actions(os, menu).iter().copied() {
            CtxMenuItem {
                key: "{action.test_id()}",
                testid: format!("{test_id_prefix}-{}{test_id_suffix}", action.test_id()),
                disabled,
                title: title.clone(),
                onclick: {
                    let pump = pump.clone();
                    let host = host.clone();
                    move |_| {
                        on_close.call(());
                        if let Some(host) = &host {
                            perform_machine_action(&pump, action, host);
                        }
                    }
                },
                {action.label()}
            }
        }
    }
}

/// Run `action` against `host`, toasting what the operator cannot otherwise
/// see: a copied path, or a refusal.
fn perform_machine_action(pump: &Pump, action: MachineAction, host: &str) {
    let launch = match machine_launch(action, host) {
        Ok(launch) => launch,
        Err(error) => {
            report_failure(pump, action, &error.to_string());
            return;
        }
    };
    tracing::info!(target: "sidebar", action = action.test_id(), ?launch, "machine action launched");
    match launch {
        MachineLaunch::Navigate { href } => {
            if !hand_off_to_os(&href) {
                report_failure(pump, action, "the browser refused to open it");
            }
        }
        MachineLaunch::CopyText { text } => {
            if copy_text(&text) {
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                    message: format!("Copied {text}"),
                }));
            } else {
                report_failure(pump, action, "the clipboard is unavailable");
            }
        }
        MachineLaunch::Download {
            file_name,
            mime_type,
            contents,
        } => {
            if !save_text_file(&file_name, mime_type, &contents) {
                report_failure(pump, action, "the browser refused the download");
            }
        }
    }
}

fn report_failure(pump: &Pump, action: MachineAction, reason: &str) {
    let message = format!("{} failed: {reason}", action.label());
    tracing::warn!(target: "sidebar", %message, "machine action failed");
    pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
}

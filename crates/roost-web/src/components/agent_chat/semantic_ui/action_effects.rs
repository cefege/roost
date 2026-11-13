//! Browser effects shared by semantic pointer and menu actions.
//! Uses the native link validator, host-service popup primitive and clipboard
//! hook; completion handlers restore the owning view before publishing errors.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;

use super::super::host_services::open_blank_tab;
use super::super::links::{NativeTarget, resolve_native_target};
use super::super::media::MediaContext;
use crate::platform::location::hand_off_to_os;
use crate::pump::Pump;
use crate::router_state::RouterContext;
use crate::terminal_href::worker_os;

pub(in super::super) fn copy_feedback() -> EventHandler<bool> {
    let pump = consume_context::<Pump>();
    EventHandler::new(move |accepted: bool| {
        if !accepted {
            report_failure(
                &pump,
                "Copy failed: the browser refused clipboard access. Grant permission and try again.",
            );
        }
    })
}

pub(in super::super) fn open_native(href: &str) {
    let context = consume_context::<Signal<MediaContext>>().read().clone();
    let pump = consume_context::<Pump>();
    let target = {
        let core = pump.core();
        let core = core.borrow();
        resolve_native_target(
            href,
            &context.worker_fp,
            &context.cwd,
            worker_os(core.store(), &context.worker_fp),
        )
    };
    open_target(target);
}

pub(in super::super) fn open_target(target: NativeTarget) {
    let pump = consume_context::<Pump>();
    let opened = match target {
        NativeTarget::External { href, scheme: "mailto" } => {
            hand_off_to_os(&href).then_some(()).ok_or_else(|| "The browser refused the mail link.".to_owned())
        }
        NativeTarget::External { href, .. } => {
            open_blank_tab().and_then(|tab| tab.navigate(&href))
        }
        NativeTarget::WorkerFile { href } => {
            consume_context::<RouterContext>().navigate.call(href);
            Ok(())
        }
        NativeTarget::Inert => Err("This target cannot be opened safely. Use an http(s) link, a mail address or a worker file.".to_owned()),
    };
    if let Err(error) = opened {
        report_failure(&pump, &format!("Open failed: {error}"));
    }
}

fn report_failure(pump: &Pump, message: &str) {
    tracing::warn!(target: "agent_chat", %message, "OMP pointer action failed");
    pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
        message: message.to_owned(),
    }));
}

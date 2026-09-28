//! One terminal tab in a pane strip: the select button with its title,
//! carrier mark and agent chip, and the close button. `PaneStrip` owns drag,
//! close and hover state; this maps it onto the stable tab DOM. Ports
//! `apps/web/src/components/deck/PaneTab.tsx`.

use dioxus::prelude::*;
use roost_client_core::TerminalTransport;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::components::md::{Button, ButtonVariant, Icon, IconButton, IconButtonSize, IconSize};
use crate::pump::use_store;
use crate::session_naming::session_title;

/// v2 `sessionTerminalTransportLabel`: the tooltip for a DIRECT carrier, and
/// nothing for the coordinator or an unconfirmed one.
pub fn direct_transport_label(kind: Option<TerminalTransport>) -> Option<&'static str> {
    match kind? {
        TerminalTransport::Loopback => Some("Direct on this device"),
        TerminalTransport::Peer => Some("Direct peer connection"),
        TerminalTransport::Sync => None,
    }
}

/// `data-terminal-transport` for a confirmed carrier.
fn transport_attribute(kind: TerminalTransport) -> &'static str {
    roost_client_core::store::terminal_transport::transport_attribute(kind)
}

/// One tab.
#[component]
pub fn PaneTab(
    session_id: String,
    active: bool,
    dragging: bool,
    closing: bool,
    style: String,
    hover_card_available: bool,
    on_pointer_down: EventHandler<PointerEvent>,
    on_select: EventHandler<()>,
    on_hover_start: EventHandler<super::deck_dom::ClientBox>,
    on_hover_end: EventHandler<()>,
    on_close: EventHandler<MouseEvent>,
) -> Element {
    let pump = use_store();
    let (title, kind) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let title = roost_client_core::store::selectors::session_by_id(store, &session_id)
            .map(|session| session_title(store, session))
            .unwrap_or_default();
        (title, session_terminal_transport_kind(store, &session_id))
    };
    let direct = direct_transport_label(kind);
    let mut element = use_signal(|| None::<std::rc::Rc<MountedData>>);
    // The hover card and the OS tooltip would stack on a plain desktop; the
    // tooltip is the fallback where no hover card is shown.
    let native_tooltip = (!hover_card_available).then(|| match direct {
        Some(carrier) => format!("{title} — {carrier}"),
        None => title.clone(),
    });
    rsx! {
        div {
            class: "df-tab workbench-pane-tab",
            "data-testid": "tab-{session_id}",
            "data-active": if active { "true" } else { "false" },
            "data-dragging": if dragging { "true" } else { "false" },
            "data-closing": if closing { "true" } else { "false" },
            "data-terminal-transport": kind.map(transport_attribute),
            style,
            onmounted: move |event: MountedEvent| element.set(Some(event.data())),
            onmouseenter: move |_| {
                if let Some(anchor) = element.peek().as_deref().and_then(super::deck_dom::client_box) {
                    on_hover_start.call(anchor);
                }
            },
            onmouseleave: move |_| on_hover_end.call(()),
            Button {
                variant: ButtonVariant::Ghost,
                class: "workbench-pane-tab__select",
                "aria-label": title.clone(),
                "aria-current": active.then_some("page"),
                title: native_tooltip,
                onpointerdown: move |event| on_pointer_down.call(event),
                onclick: move |_| on_select.call(()),
                Icon { name: "terminal", size: IconSize::Sm, class: "workbench-pane-tab__icon" }
                if direct.is_some() {
                    Icon { name: "bolt", size: IconSize::Sm, class: "workbench-pane-tab__local" }
                }
                span { class: "df-tab-label workbench-pane-tab__label", "{title}" }
                AgentStatusIndicator {
                    session_id: session_id.clone(),
                    compact: true,
                    suppress_tooltip: hover_card_available,
                }
            }
            IconButton {
                icon: "close",
                label: "Close terminal",
                size: IconButtonSize::IconSm,
                class: "df-tab-close workbench-pane-tab__close",
                "data-testid": "tab-close-{session_id}",
                onclick: move |event| on_close.call(event),
            }
        }
    }
}

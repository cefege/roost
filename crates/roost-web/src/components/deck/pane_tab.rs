//! One terminal tab in a pane strip: the select button with its title,
//! carrier mark and agent chip, and the close button. `PaneStrip` owns drag,
//! close and hover state; this maps it onto the stable tab DOM. Ports
//! `apps/web/src/components/deck/PaneTab.tsx`.

use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::pump::use_store;
use crate::session_naming::session_title;
use dioxus::prelude::*;
use roost_client_core::TerminalTransport;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use crate::components::ui::{ButtonSize, Icon, IconButton, IconType, StatusDot, TabsTrigger};

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
    on_hover_start: EventHandler<super::deck_dom::ClientBox>,
    on_hover_end: EventHandler<()>,
    on_close: EventHandler<MouseEvent>,
) -> Element {
    let pump = use_store();
    let (title, kind, agent_status) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        if let Some(conversation_id) = session_id.strip_prefix("agent:") {
            let conversation = store.agent_chat.conversations.get(conversation_id);
            (
                crate::components::agent_chat::display_conversation_title(
                    conversation.map_or("", |conversation| conversation.title.as_str()),
                )
                .to_owned(),
                None,
                conversation.map(|conversation| match conversation.run_state {
                    roost_protocol::wire::agent_chat::AgentRunState::Running => "info",
                    roost_protocol::wire::agent_chat::AgentRunState::Failed => "warn",
                    roost_protocol::wire::agent_chat::AgentRunState::Idle => "idle",
                }),
            )
        } else {
            let title = roost_client_core::store::selectors::session_by_id(store, &session_id)
                .map(|session| session_title(store, session))
                .unwrap_or_default();
            (
                title,
                session_terminal_transport_kind(store, &session_id),
                None,
            )
        }
    };
    let direct = direct_transport_label(kind);
    let mut element = use_signal(|| None::<std::rc::Rc<MountedData>>);
    // The ring a hovered toast names this tab for. Read through the context the
    // root provides, not from the dock: a toast and a pane tab are siblings,
    // and the tab is the surface the operator can actually see the ring on.
    let notify_target =
        use_context::<crate::components::notifications::notify_target::NotifyTarget>();
    let ringing = crate::components::notifications::notify_target::ring_attribute(
        &notify_target,
        &session_id,
    );
    // The hover card and the OS tooltip would stack on a plain desktop; the
    // tooltip is the fallback where no hover card is shown.
    let native_tooltip = (!hover_card_available).then(|| match direct {
        Some(carrier) => format!("{title} — {carrier}"),
        None => title.clone(),
    });
    // A resting tab REMOVES its style attribute rather than writing an empty
    // one: dioxus-web's `style` setter re-applies every inline property the new
    // value does not name, so `style=""` after a drag kept the dragged tab's
    // `translateX`, clipping it out of the rail and parking the overflow
    // chevron on a strip whose tabs all fit.
    let style = (!style.is_empty()).then_some(style);
    rsx! {
        div {
            class: "group/tab relative inline-flex items-center gap-1 box-border h-full self-stretch flex-[0_1_auto] w-[var(--workbench-tab-width-max)] min-w-[var(--workbench-tab-width-min)] max-w-[var(--workbench-tab-width-max)] cursor-pointer touch-pan-y text-sm data-[dragging=true]:z-[4] data-[dragging=true]:bg-background data-[dragging=true]:cursor-grabbing data-[closing=true]:!min-w-0 data-[closing=true]:!max-w-0 data-[closing=true]:!px-0 data-[closing=true]:z-0 data-[closing=true]:opacity-0 data-[closing=true]:pointer-events-none data-[closing=true]:overflow-hidden data-[closing=true]:transition-[max-width,min-width,padding,opacity] data-[closing=true]:duration-[220ms] @container/pane-tab",
            "data-testid": "tab-{session_id}",
            "data-deck-tab": "true",
            "data-active": if active { "true" } else { "false" },
            "data-dragging": if dragging { "true" } else { "false" },
            "data-closing": if closing { "true" } else { "false" },
            "data-terminal-transport": kind.map(transport_attribute),
            "data-notify-target": ringing,
            style,
            onmounted: move |event: MountedEvent| element.set(Some(event.data())),
            onmouseenter: move |_| {
                if let Some(anchor) = element.peek().as_deref().and_then(super::deck_dom::client_box) {
                    on_hover_start.call(anchor);
                }
            },
            onmouseleave: move |_| on_hover_end.call(()),
            TabsTrigger {
                value: session_id.clone(),
                class: "h-full min-w-0 flex-1 justify-start text-start after:bottom-0",
                aria_label: title.clone(),
                has_panel: false,
                title: native_tooltip,
                onpointerdown: move |event| on_pointer_down.call(event),
                "data-tab-select": "true",
                Icon {
                    icon: if agent_status.is_some() { IconType::Bot } else { IconType::Terminal },
                    class: "size-4 shrink-0 text-muted-foreground group-data-[packed=true]/strip:group-data-[active=true]/tab:hidden",
                }
                if direct.is_some() {
                    Icon { icon: IconType::Zap, class: "size-4 shrink-0 text-warning @max-[120px]/pane-tab:hidden" }
                }
                span { class: "flex-1 min-w-0 truncate group-data-[packed=true]/strip:hidden", "{title}" }
                if let Some(status) = agent_status {
                    StatusDot { status: status.to_owned() }
                } else {
                    AgentStatusIndicator {
                        session_id: session_id.clone(),
                        compact: true,
                        suppress_tooltip: hover_card_available,
                    }
                    crate::components::terminal_bell_mark::TerminalBellMark { session_id: session_id.clone() }
                    crate::components::terminal_signal_marks::TerminalProgressMark { session_id: session_id.clone() }
                }
            }
            IconButton {
                icon: IconType::X,
                label: if agent_status.is_some() { "Close agent" } else { "Close terminal" },
                size: ButtonSize::IconXs,
                class: "shrink-0 opacity-0 pointer-events-none transition-opacity duration-150 group-hover/tab:opacity-100 group-hover/tab:pointer-events-auto group-focus-within/tab:opacity-100 group-data-[active=true]/tab:opacity-100 group-data-[active=true]/tab:pointer-events-auto focus-visible:opacity-100 focus-visible:pointer-events-auto tv:opacity-100 tv:pointer-events-auto pad:opacity-100 pad:pointer-events-auto group-data-[packed=true]/strip:hidden group-data-[packed=true]/strip:group-data-[active=true]/tab:inline-flex",
                "data-testid": "tab-close-{session_id}",
                onclick: move |event| on_close.call(event),
            }
        }
    }
}

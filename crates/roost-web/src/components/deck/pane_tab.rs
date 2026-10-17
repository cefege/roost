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

use crate::components::ui::{Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, StatusDot};

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
    let (title, kind, agent_status) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        if let Some(conversation_id) = session_id.strip_prefix("agent:") {
            let conversation = store.agent_chat.conversations.get(conversation_id);
            (
                conversation.map_or_else(String::new, |conversation| conversation.title.clone()),
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
            class: "group/tab relative inline-flex items-center gap-1 px-1 box-border h-full self-stretch flex-[0_1_auto] w-[var(--workbench-tab-width-max)] min-w-[var(--workbench-tab-width-min)] max-w-[var(--workbench-tab-width-max)] overflow-hidden cursor-pointer touch-pan-y border-0 border-t border-r border-solid border-t-transparent border-r-[var(--workbench-tab-border)] bg-[var(--workbench-tab-strip)] text-[var(--workbench-tab-inactive-foreground)] text-sm transition-colors duration-150 not-data-[active=true]:hover:bg-[var(--workbench-tab-hover)] not-data-[active=true]:hover:text-[var(--workbench-tab-hover-foreground)] data-[active=true]:bg-[var(--workbench-editor)] data-[active=true]:text-[var(--workbench-tab-active-foreground)] data-[active=true]:border-t-[var(--workbench-focus-ring)] group-data-[focused=false]/strip:data-[active=true]:text-[var(--workbench-tab-unfocused-active-foreground)] group-data-[focused=false]/strip:data-[active=true]:border-t-transparent data-[dragging=true]:z-[4] data-[dragging=true]:bg-card data-[dragging=true]:shadow-md data-[dragging=true]:cursor-grabbing data-[closing=true]:!min-w-0 data-[closing=true]:!max-w-0 data-[closing=true]:!px-0 data-[closing=true]:z-0 data-[closing=true]:opacity-0 data-[closing=true]:pointer-events-none data-[closing=true]:overflow-hidden data-[closing=true]:transition-[max-width,min-width,padding,opacity] data-[closing=true]:duration-[220ms] @container/pane-tab",
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
            Button {
                variant: ButtonVariant::Ghost,
                class: "flex flex-1 min-w-0 h-full items-center gap-1 p-0 rounded-none border-0 bg-transparent text-inherit font-normal text-start cursor-pointer hover:bg-transparent hover:text-inherit dark:hover:bg-transparent group-data-[packed=true]/strip:justify-center",
                "aria-label": title.clone(),
                "data-tab-select": "true",
                "aria-current": active.then_some("page"),
                title: native_tooltip,
                onpointerdown: move |event| on_pointer_down.call(event),
                onclick: move |_| on_select.call(()),
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
                size: ButtonSize::IconSm,
                class: "shrink-0 size-6 text-muted-foreground opacity-0 pointer-events-none transition-opacity duration-150 hover:text-foreground group-hover/tab:opacity-100 group-hover/tab:pointer-events-auto group-focus-within/tab:opacity-100 group-data-[active=true]/tab:opacity-100 group-data-[active=true]/tab:pointer-events-auto focus-visible:opacity-100 focus-visible:pointer-events-auto group-data-[packed=true]/strip:hidden group-data-[packed=true]/strip:group-data-[active=true]/tab:inline-flex",
                "data-testid": "tab-close-{session_id}",
                onclick: move |event| on_close.call(event),
            }
        }
    }
}

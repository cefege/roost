//! One terminal tab in a pane strip: the select button with its title,
//! carrier mark and agent chip, and the close button. `PaneStrip` owns drag,
//! close and hover state; this maps it onto the stable tab DOM. Ports
//! `apps/web/src/components/deck/PaneTab.tsx`.

use crate::components::agents::agent_status_indicator::AgentStatusIndicator;
use crate::pump::use_store;
use crate::session_naming::session_title;
use dioxus::prelude::*;
use libero::components::{Box, Pictogram, Text};
use roost_client_core::TerminalTransport;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

use crate::components::agents::agent_status_indicator::agent_dot_color;
use libero::components::{ActionIcon, Button, Indicator};
use libero::sx::sx;
use roost_client_core::client::agents::status_policy::AgentDotStatus;

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
                crate::components::agent_chat::display_conversation_title(
                    conversation.map_or("", |conversation| conversation.title.as_str()),
                )
                .to_owned(),
                None,
                conversation.map(|conversation| match conversation.run_state {
                    roost_protocol::wire::agent_chat::AgentRunState::Running => {
                        AgentDotStatus::Info
                    }
                    roost_protocol::wire::agent_chat::AgentRunState::Failed => AgentDotStatus::Warn,
                    roost_protocol::wire::agent_chat::AgentRunState::Idle => AgentDotStatus::Idle,
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
    let tab_sx = use_hook(|| {
        sx().position("relative")
            .display("inline-flex")
            .flex_direction("row")
            .align_items("center")
            .gap("xs")
            .padding_inline("xs")
            .box_sizing("border-box")
            .height("100%")
            .flex("0 1 auto")
            .width("var(--workbench-tab-width-max)")
            .min_width("var(--workbench-tab-width-min)")
            .max_width("var(--workbench-tab-width-max)")
            .overflow("hidden")
            .cursor("pointer")
            .touch_action("pan-y")
            .border("0")
            .with("border-top", "1px solid transparent")
            .with("border-right", "1px solid var(--workbench-tab-border)")
            .background("var(--workbench-tab-strip)")
            .color("var(--workbench-tab-inactive-foreground)")
            .font_size("sm")
            .transition("background-color 150ms, color 150ms")
            .selector(
                "&:not([data-active=true]):hover",
                sx().background("var(--workbench-tab-hover)")
                    .color("var(--workbench-tab-hover-foreground)"),
            )
            .selector(
                "&[data-active=true]",
                sx().background("var(--workbench-editor)")
                    .color("var(--workbench-tab-active-foreground)")
                    .with("border-top-color", "var(--workbench-focus-ring)"),
            )
            .selector(
                ":where([data-focused=false]) &[data-active=true]",
                sx().color("var(--workbench-tab-unfocused-active-foreground)")
                    .with("border-top-color", "transparent"),
            )
            .selector(
                "&[data-dragging=true]",
                sx().z_index("4")
                    .background("surface")
                    .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Md))
                    .cursor("grabbing"),
            )
            .selector(
                "&[data-closing=true]",
                sx().min_width("0")
                    .max_width("0")
                    .padding_inline("0")
                    .z_index("0")
                    .opacity("0")
                    .pointer_events("none")
                    .overflow("hidden")
                    .transition("max-width 220ms, min-width 220ms, padding 220ms, opacity 220ms"),
            )
    });
    let select_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .flex("1")
            .min_width("0")
            .height("100%")
            .align_items("center")
            .gap("xs")
            .padding("0")
            .border_radius("0")
            .font_weight("normal")
            .color("inherit")
            .text_align("left")
            .cursor("pointer")
            .hover(sx().background("transparent").color("inherit"))
    });
    let title_sx = use_hook(|| {
        sx().flex("1")
            .min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .selector(":where([data-packed=true]) &", sx().display("none"))
    });
    let close_sx = use_hook(|| {
        sx().flex_shrink("0")
            .width("1.5rem")
            .height("1.5rem")
            .color("muted")
            .opacity("0")
            .pointer_events("none")
            .transition("opacity 150ms")
            .hover(sx().color("ink"))
            .selector("&:focus-visible", sx().opacity("1").pointer_events("auto"))
            .selector(
                ":where([data-deck-tab]:hover) &",
                sx().opacity("1").pointer_events("auto"),
            )
            .selector(
                ":where([data-deck-tab]:focus-within) &",
                sx().opacity("1").pointer_events("auto"),
            )
            .selector(
                ":where([data-deck-tab][data-active=true]) &",
                sx().opacity("1").pointer_events("auto"),
            )
            .selector(
                ":where([data-packed=true]) [data-active=false] &",
                sx().display("none"),
            )
            .selector(
                ":where([data-packed=true]) [data-active=true] &",
                sx().display("inline-flex"),
            )
            .selector(
                ":where([data-tv=true]) &",
                sx().opacity("1").pointer_events("auto"),
            )
            .selector(
                ":where([data-pad=true]) &",
                sx().opacity("1").pointer_events("auto"),
            )
    });
    rsx! {
        Box {
            component: "div",
            sx: tab_sx,
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
            // The row paints its own hover and selection fill, so the label must not
            // add the Ghost accent band over it.
            Button {
                variant: "standard",
                sx: select_sx,
                "aria-label": title.clone(),
                "data-tab-select": "true",
                "aria-current": active.then_some("page"),
                title: native_tooltip,
                onpointerdown: move |event| on_pointer_down.call(event),
                onclick: move |_| on_select.call(()),
                Pictogram {
                    icon: if agent_status.is_some() { pictogram_icons_lucide::bot::outlined } else { pictogram_icons_lucide::terminal::outlined },
                    width: "1rem",
                    height: "1rem",
                }
                if direct.is_some() {
                    Pictogram { icon: pictogram_icons_lucide::zap::outlined, width: "1rem", height: "1rem" }
                }
                Text { component: "span", sx: title_sx, "{title}" }
                if let Some(status) = agent_status {
                    Indicator { color: agent_dot_color(status) }
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
            ActionIcon {
                sx: close_sx,
                icon: pictogram_icons_lucide::x::outlined,
                aria_label: if agent_status.is_some() { "Close agent" } else { "Close terminal" },
                size: "sm",
                "data-testid": "tab-close-{session_id}",
                onclick: move |event| on_close.call(event),
            }
        }
    }
}

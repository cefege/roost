//! One workspace tab: identity icon, truncated title, aggregated status and close.
//! `PaneStrip` owns drag, close and hover state; this maps them onto stable DOM.
//! Terminal progress and bells remain accessible through the cue.

use crate::components::terminal_signal_marks::progress_presentation;
use crate::components::ui::placement::ClientRect;
use crate::components::ui::{
    ButtonSize, Icon, IconButton, IconType, Separator, SeparatorOrientation, StatusDot, TabsTrigger,
};
use crate::pump::use_store;
use crate::session_naming::session_title;
use dioxus::prelude::*;
use roost_client_core::TerminalTransport;
use roost_client_core::store::terminal_transport::session_terminal_transport_kind;

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

/// A compact, accessible roll-up of terminal activity.
#[component]
fn TabStatus(session_id: String, suppress_tooltip: bool) -> Element {
    let pump = use_store();
    let (bell_rang, progress) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        (
            store.terminal_bells.is_unseen(&session_id),
            store
                .terminal_signals
                .progress(&session_id)
                .and_then(progress_presentation)
                .map(|(_, state, label)| (state, label)),
        )
    };
    let status = match progress.as_ref().map(|(state, _)| *state) {
        Some("error") => Some("error"),
        Some("paused") => Some("warn"),
        Some(_) => Some("running"),
        None if bell_rang => Some("running"),
        None => None,
    };
    let Some(status) = status else {
        return rsx! {};
    };
    let mut labels = Vec::with_capacity(3);
    if bell_rang {
        labels.push("Bell rang".to_owned());
    }
    if let Some((_, label)) = progress {
        labels.push(label);
    }
    let label = labels.join("; ");
    let title = (!suppress_tooltip).then(|| label.clone());
    rsx! {
        span {
            class: "inline-flex shrink-0",
            role: "img",
            "aria-label": label,
            title,
            StatusDot { status: status.to_owned() }
        }
    }
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
    on_hover_start: EventHandler<ClientRect>,
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
            class: "group/tab relative inline-flex items-center gap-0 box-border h-[var(--workbench-tab-action-size)] self-center flex-[0_1_auto] w-[var(--workbench-tab-width-max)] min-w-[var(--workbench-tab-width-min)] max-w-[var(--workbench-tab-width-max)] data-[active=true]:min-w-[var(--workbench-tab-width-active-min)] cursor-pointer touch-pan-y text-xs data-[dragging=true]:z-[4] data-[dragging=true]:bg-background data-[dragging=true]:cursor-grabbing data-[closing=true]:!min-w-0 data-[closing=true]:!max-w-0 data-[closing=true]:!px-0 data-[closing=true]:z-0 data-[closing=true]:opacity-0 data-[closing=true]:pointer-events-none data-[closing=true]:overflow-hidden data-[closing=true]:transition-[max-width,min-width,padding,opacity] data-[closing=true]:duration-[220ms]",
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
                class: "h-full min-w-0 flex-1 justify-start px-2 text-xs text-start",
                aria_label: title.clone(),
                title: native_tooltip,
                "data-tab-select": session_id.clone(),
                onpointerdown: move |event| on_pointer_down.call(event),
                trailing_action: rsx! {
                    IconButton {
                        icon: IconType::X,
                        label: "Close terminal",
                        size: ButtonSize::IconXs,
                        class: "shrink-0 group-data-[packed=true]/strip:hidden group-data-[packed=true]/strip:group-data-[active=true]/tab:inline-flex",
                        "data-testid": "tab-close-{session_id}",
                        onclick: move |event| on_close.call(event),
                    }
                },
                Icon {
                    icon: IconType::Terminal,
                    class: "size-4 shrink-0",
                }
                span { class: "flex-1 min-w-0 truncate", "{title}" }
                TabStatus {
                    session_id: session_id.clone(),
                    suppress_tooltip: hover_card_available,
                }
            }
            if !active {
                Separator {
                    orientation: SeparatorOrientation::Vertical,
                    decorative: true,
                    class: "absolute end-0 h-4",
                }
            }
        }
    }
}

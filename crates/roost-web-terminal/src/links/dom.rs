//! The browser half of terminal links: `TerminalLinkAttachment` owns the
//! `web_sys` callbacks (listeners, frames, idle scans, the mutation observer,
//! `visibilitychange`) and routes each into `TerminalLinks`; `dom/host.rs` is
//! the live DOM behind the link seams. The pane attaches one per terminal.
//! Ports the browser wiring of `apps/web/src/renderer/terminal-links.ts`.

mod host;

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use js_sys::{Array, Function};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    Document, Element, Event, EventTarget, KeyboardEvent, MouseEvent, MutationObserver, Window,
};

use super::TerminalLinkOptions;
use super::activation::LinkActivationGesture;
use super::attachment::{LinkEvent, LinkListener, TerminalLinks};
use super::scan::FrameCallback;
use crate::cell_row::TERMINAL_LINK_CLASS;
use host::{WebLinkHost, touched_rows};

type SharedLinks = RefCell<TerminalLinks<WebLinkHost>>;

/// One link listener's browser closure, paired with the listener it feeds.
type ListenerClosure = Closure<dyn FnMut(Event)>;

/// The browser callbacks one attachment owns. Each holds only a weak handle,
/// so a callback the browser still queues after disposal does nothing.
struct LinkCallbacks {
    listeners: Vec<(LinkListener, ListenerClosure)>,
    scan_frame: Closure<dyn FnMut(f64)>,
    activation_frame: Closure<dyn FnMut(f64)>,
    idle_scan: Closure<dyn FnMut()>,
    mutations: Closure<dyn FnMut(Array, MutationObserver)>,
    visibility: Closure<dyn FnMut(Event)>,
}

impl LinkCallbacks {
    fn new(links: &Weak<SharedLinks>) -> Self {
        let anchor_selector = format!("a.{TERMINAL_LINK_CLASS}");
        let listeners = LinkListener::ALL
            .into_iter()
            .map(|listener| {
                let (links, selector) = (links.clone(), anchor_selector.clone());
                let react: Closure<dyn FnMut(Event)> = Closure::new(move |event: Event| {
                    let read = link_event(&event, &selector);
                    with_links(&links, |links| {
                        if links.dispatch(listener, read) {
                            event.prevent_default();
                        }
                    });
                });
                (listener, react)
            })
            .collect();
        let frame = |callback: FrameCallback| -> Closure<dyn FnMut(f64)> {
            let links = links.clone();
            Closure::new(move |_: f64| {
                with_links(&links, |links| links.frame_fired(callback));
            })
        };
        let idle_links = links.clone();
        let mutation_links = links.clone();
        let visibility_links = links.clone();
        Self {
            listeners,
            scan_frame: frame(FrameCallback::Scan),
            activation_frame: frame(FrameCallback::ActivationScan),
            idle_scan: Closure::new(move || {
                with_links(&idle_links, TerminalLinks::idle_scan_fired);
            }),
            mutations: Closure::new(move |records: Array, _: MutationObserver| {
                with_links(&mutation_links, |links| {
                    links.mutations_observed(|| touched_rows(&records))
                });
            }),
            visibility: Closure::new(move |_: Event| {
                with_links(&visibility_links, TerminalLinks::visibility_changed);
            }),
        }
    }

    fn listener(&self, listener: LinkListener) -> Option<&Function> {
        self.listeners
            .iter()
            .find(|(held, _)| *held == listener)
            .map(|(_, react)| react.as_ref().unchecked_ref())
    }
}

/// What one attachment installed on the browser, and the only way back out.
///
/// It is held OUTSIDE `SharedLinks` on purpose. A re-entrant link callback holds
/// that borrow, and a teardown that lost it would leave registrations pointing
/// at closures `SharedLinks` is about to free.
struct BrowserRegistrations {
    callbacks: Rc<LinkCallbacks>,
    observer: Option<MutationObserver>,
    window: Window,
    container: Element,
    document: Document,
}

impl BrowserRegistrations {
    /// Remove every registration this attachment made, and need no state
    /// borrow to do it: the targets come from the listener itself, and
    /// removing a callback that was never added is a no-op the browser
    /// already defines. Unconditional by construction, not by state.
    fn unregister(&self) {
        for listener in LinkListener::ALL {
            let Some(callback) = self.callbacks.listener(listener) else {
                continue;
            };
            let target: &EventTarget = if listener.on_window() {
                self.window.as_ref()
            } else {
                self.container.as_ref()
            };
            let _ = target.remove_event_listener_with_callback(listener.event_type(), callback);
        }
        if let Some(observer) = &self.observer {
            observer.disconnect();
        }
        let _ = self.document.remove_event_listener_with_callback(
            "visibilitychange",
            self.callbacks.visibility.as_ref().unchecked_ref(),
        );
    }
}

/// One pane's terminal-link attachment in the browser: v2 `attachTerminalLinks`.
/// Dropping it disposes it, which releases every listener it installed.
pub struct TerminalLinkAttachment {
    links: Rc<SharedLinks>,
    /// Taken by the first teardown, so a second one is a no-op by
    /// construction rather than by a borrow it might lose.
    live: Option<BrowserRegistrations>,
}

impl std::fmt::Debug for TerminalLinkAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalLinkAttachment")
            .finish_non_exhaustive()
    }
}

impl TerminalLinkAttachment {
    /// Attach to `container`, the pane's `.wterm` display. `None` when the
    /// container has no document or window to listen on.
    pub fn attach(container: &Element, options: TerminalLinkOptions) -> Option<Self> {
        let (Some(window), Some(document)) = (web_sys::window(), container.owner_document()) else {
            tracing::warn!(target: "terminal_links", "no document to attach terminal links to");
            return None;
        };
        inject_link_stylesheet_once(&document);
        let mut live = None;
        let links = Rc::new_cyclic(|weak: &Weak<SharedLinks>| {
            let callbacks = Rc::new(LinkCallbacks::new(weak));
            let observer = MutationObserver::new(callbacks.mutations.as_ref().unchecked_ref()).ok();
            let host = WebLinkHost {
                container: container.clone(),
                window: window.clone(),
                document: document.clone(),
                callbacks: Rc::clone(&callbacks),
                observer: observer.clone(),
                hint: RefCell::new(None),
            };
            live = Some(BrowserRegistrations {
                callbacks,
                observer,
                window,
                container: container.clone(),
                document,
            });
            RefCell::new(TerminalLinks::attach(host, options))
        });
        tracing::info!(target: "terminal_links", "terminal links attached");
        Some(Self { links, live })
    }

    /// Foreground (`true`) or withdraw (`false`) the pane's link work.
    pub fn set_active(&self, active: bool) {
        self.with_links(|links| links.set_active(active));
    }

    /// Clear the modifier and pointer levels, releasing any armed hold.
    pub fn release_interaction(&self) {
        self.with_links(TerminalLinks::release_interaction);
    }

    /// Open `anchor` as the context menu does. Returns whether it opened.
    pub fn open_link(&self, anchor: &Element) -> bool {
        let mut opened = false;
        self.with_links(|links| opened = links.open_link(anchor));
        opened
    }

    /// The display text of the target `anchor` opens, or `None`.
    pub fn describe_link(&self, anchor: &Element) -> Option<String> {
        self.links
            .try_borrow()
            .ok()
            .and_then(|links| links.describe_link(anchor))
    }

    /// Tear down for good: listeners, observer, queued scans and the hint.
    ///
    /// Taking `live` is what makes a second call a no-op BY CONSTRUCTION
    /// rather than by a borrow it might lose. The listener, observer and
    /// visibility removals run first because none of them needs the state
    /// borrow: a teardown that lost that borrow to a re-entrant callback must
    /// still not leave registrations pointing at closures about to be freed.
    /// The state machine runs last because it is what cancels the scanner's
    /// queued frames, and a frame the browser still holds cannot be cancelled
    /// from out here — so when that borrow is unavailable the callbacks are
    /// leaked instead. Every one of them upgrades a `Weak<SharedLinks>` that
    /// is by then gone, so a leaked closure does nothing where a freed one is
    /// a trap.
    pub fn dispose(&mut self) {
        let Some(live) = self.live.take() else {
            return;
        };
        live.unregister();
        if self.with_links(TerminalLinks::dispose) {
            return;
        }
        tracing::warn!(target: "terminal_links",
            "a re-entrant teardown could not cancel the link scanner's queued frames; \
             its callbacks are leaked rather than freed under them");
        std::mem::forget(live);
    }

    fn with_links(&self, act: impl FnOnce(&mut TerminalLinks<WebLinkHost>)) -> bool {
        with_links(&Rc::downgrade(&self.links), act)
    }
}

impl Drop for TerminalLinkAttachment {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// Run `act` on the attachment unless it is gone or already running: a
/// callback that fires while another holds it (an `on_open_file` that
/// synchronously re-enters) is dropped with a warning rather than aliased.
/// False when it could not run, which is the one thing `dispose` acts on.
fn with_links(
    links: &Weak<SharedLinks>,
    act: impl FnOnce(&mut TerminalLinks<WebLinkHost>),
) -> bool {
    let Some(links) = links.upgrade() else {
        return false;
    };
    let Ok(mut links) = links.try_borrow_mut() else {
        tracing::warn!(target: "terminal_links", "a re-entrant terminal link callback was dropped");
        return false;
    };
    act(&mut links);
    true
}

/// Read one DOM event: its key, its button and modifier LEVELS (all false for
/// an event that carries none), and the terminal link it landed on.
fn link_event(event: &Event, anchor_selector: &str) -> LinkEvent<Element> {
    let gesture =
        event
            .dyn_ref::<MouseEvent>()
            .map_or_else(LinkActivationGesture::default, |mouse| {
                LinkActivationGesture {
                    button: mouse.button(),
                    ctrl: mouse.ctrl_key(),
                    meta: mouse.meta_key(),
                    shift: mouse.shift_key(),
                    alt: mouse.alt_key(),
                }
            });
    let anchor = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|element| element.closest(anchor_selector).ok().flatten());
    LinkEvent {
        key: event.dyn_ref::<KeyboardEvent>().map(KeyboardEvent::key),
        gesture,
        anchor,
    }
}

/// Install the link stylesheet once per document, byte-for-byte v2's.
fn inject_link_stylesheet_once(document: &Document) {
    let installed = document
        .query_selector("style[data-roost=\"wterm-link\"]")
        .ok()
        .flatten();
    let head = document.query_selector("head").ok().flatten();
    let (None, Some(head)) = (installed, head) else {
        return;
    };
    let Ok(style) = document.create_element("style") else {
        return;
    };
    let _ = style.set_attribute("data-roost", "wterm-link");
    style.set_text_content(Some(&link_stylesheet()));
    let _ = head.append_child(&style);
}

fn link_stylesheet() -> String {
    let link = TERMINAL_LINK_CLASS;
    format!(
        "
.{link} {{
  color: inherit;
  text-decoration: none;
  pointer-events: auto;
  cursor: text;
}}
.wterm[data-link-armed=\"1\"] .{link} {{
  text-decoration: underline;
  text-underline-offset: 2px;
  cursor: pointer;
}}
/* File links pick up the accent so they read as \"opens in Roost\", not the web. */
.wterm[data-link-armed=\"1\"] .{link}[data-kind=\"file\"] {{
  text-decoration-color: var(--md-primary, currentColor);
}}
.wterm-link-hint {{
  position: fixed;
  z-index: 2147483000;
  display: none;
  max-width: 60vw;
  padding: 3px 8px;
  border-radius: var(--md-shape-sm, 6px);
  background: var(--surface-2);
  color: var(--text-hi);
  border: 1px solid var(--border-subtle);
  box-shadow: var(--md-elev-3);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  line-height: 1.4;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  pointer-events: none;
}}
"
    )
}

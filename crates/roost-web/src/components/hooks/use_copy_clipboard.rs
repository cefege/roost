//! Roost's clipboard acceptance and stale-completion policy.
//! Copy controls use the hook's result flag; event owners without a component
//! call copy_text_then. Only the latest accepted write raises confirmation.
//! Expired timers and completions never write a dropped or newer signal.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

const DEFAULT_TIMEOUT_MS: i32 = 2000;

/// A copy function and whether its last copy is still being shown, for
/// `timeout_ms` (two seconds when `None`). Each attempt clears the previous
/// confirmation; only the latest attempt's accepted verdict raises it.
pub fn use_copy_clipboard(timeout_ms: Option<i32>) -> (impl Fn(&str) + Clone, ReadSignal<bool>) {
    let copied = use_signal(|| false);
    let latest = use_hook(|| CopyValue::new(0_u64));
    // Converted once: every `From<Signal>` allocates a box the scope keeps.
    let copied_read = use_hook(move || ReadSignal::from(copied));
    let timeout = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);

    let copy_fn = move |text: &str| {
        let Some(ticket) = begin_copy(latest, copied) else {
            return;
        };
        copy_text_then(text, move |accepted| {
            finish_copy(timeout, ticket, latest, copied, accepted);
        });
    };

    (copy_fn, copied_read)
}

/// Invalidate both the old confirmation and any write still awaiting a verdict.
fn begin_copy(mut latest: CopyValue<u64>, mut copied: Signal<bool>) -> Option<u64> {
    let ticket = {
        let mut latest = latest.try_write().ok()?;
        *latest = latest.wrapping_add(1);
        *latest
    };
    *copied.try_write().ok()? = false;
    Some(ticket)
}

/// A verdict may land after another attempt or after unmount. Neither may
/// publish feedback for the text now on screen.
fn finish_copy(
    timeout_ms: i32,
    ticket: u64,
    latest: CopyValue<u64>,
    mut copied: Signal<bool>,
    accepted: bool,
) {
    if !latest.try_read().is_ok_and(|latest| *latest == ticket) {
        return;
    }
    let Ok(mut flag) = copied.try_write() else {
        return;
    };
    *flag = accepted;
    drop(flag);
    if accepted {
        clear_after(timeout_ms, ticket, latest, copied);
    }
}

/// Lower `copied` once `timeout_ms` has passed, unless a newer copy owns it
/// by then or its component is gone.
#[cfg(target_arch = "wasm32")]
fn clear_after(timeout_ms: i32, ticket: u64, latest: CopyValue<u64>, copied: Signal<bool>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let clear = wasm_bindgen::closure::Closure::once_into_js(move || {
        let mut copied = copied;
        if latest.try_read().is_ok_and(|latest| *latest == ticket)
            && let Ok(mut flag) = copied.try_write()
        {
            *flag = false;
        }
    });
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(clear.unchecked_ref(), timeout_ms);
}

#[cfg(not(target_arch = "wasm32"))]
fn clear_after(timeout_ms: i32, ticket: u64, latest: CopyValue<u64>, copied: Signal<bool>) {
    let _ = (timeout_ms, ticket, latest, copied);
}

/// Copy `text` and report the browser's verdict once the write settles:
/// `true` only for a write the browser accepted. A refusal — no clipboard in
/// an insecure context, no permission, no user activation (Safari and Firefox
/// refuse a write outside a gesture asynchronously) — reports `false`, and the
/// caller then offers a Copy button rather than claim the text was copied.
pub fn copy_text_then(text: &str, on_settled: impl FnOnce(bool) + 'static) {
    #[cfg(target_arch = "wasm32")]
    {
        let written = clipboard_writer().and_then(|(clipboard, write_text)| {
            write_text
                .call1(&clipboard, &wasm_bindgen::JsValue::from_str(text))
                .ok()?
                .dyn_into::<js_sys::Promise>()
                .ok()
        });
        let Some(written) = written else {
            on_settled(false);
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let accepted = wasm_bindgen_futures::JsFuture::from(written).await.is_ok();
            on_settled(accepted);
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
        on_settled(false);
    }
}

/// `navigator.clipboard` and its `writeText`, read reflectively: the clipboard
/// is absent in an insecure context, and the generated getter traps there
/// rather than answering.
#[cfg(target_arch = "wasm32")]
fn clipboard_writer() -> Option<(wasm_bindgen::JsValue, js_sys::Function)> {
    let window = web_sys::window()?;
    let clipboard = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()).ok()?;
    if clipboard.is_undefined() || clipboard.is_null() {
        return None;
    }
    let write_text = js_sys::Reflect::get(&clipboard, &"writeText".into())
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    Some((clipboard, write_text))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Copy)]
    struct CopySignals {
        latest: CopyValue<u64>,
        copied: Signal<bool>,
    }

    fn probe(slot: Rc<RefCell<Option<CopySignals>>>) -> Element {
        let signals = use_hook(|| CopySignals {
            latest: CopyValue::new(0),
            copied: Signal::new(false),
        });
        *slot.borrow_mut() = Some(signals);
        rsx! {}
    }

    fn mounted() -> (VirtualDom, CopySignals) {
        let slot = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(probe, Rc::clone(&slot));
        dom.rebuild_in_place();
        let Some(signals) = *slot.borrow() else {
            panic!("the mounted probe publishes its signals");
        };
        (dom, signals)
    }

    #[test]
    fn another_attempt_clears_success_even_when_its_write_is_refused() {
        let (dom, signals) = mounted();
        dom.in_runtime(|| {
            let first = begin_copy(signals.latest, signals.copied).unwrap();
            finish_copy(2000, first, signals.latest, signals.copied, true);
            assert!(*signals.copied.peek());

            let second = begin_copy(signals.latest, signals.copied).unwrap();
            assert!(!*signals.copied.peek(), "pending text is not confirmed");
            finish_copy(2000, second, signals.latest, signals.copied, false);
            assert!(!*signals.copied.peek(), "refused text is not confirmed");
        });
    }

    #[test]
    fn an_older_success_cannot_confirm_a_newer_refusal() {
        let (dom, signals) = mounted();
        dom.in_runtime(|| {
            let older = begin_copy(signals.latest, signals.copied).unwrap();
            let newer = begin_copy(signals.latest, signals.copied).unwrap();
            finish_copy(2000, newer, signals.latest, signals.copied, false);
            finish_copy(2000, older, signals.latest, signals.copied, true);
            assert!(!*signals.copied.peek());
        });
    }

    #[test]
    fn an_older_refusal_cannot_clear_a_newer_success() {
        let (dom, signals) = mounted();
        dom.in_runtime(|| {
            let older = begin_copy(signals.latest, signals.copied).unwrap();
            let newer = begin_copy(signals.latest, signals.copied).unwrap();
            finish_copy(2000, newer, signals.latest, signals.copied, true);
            finish_copy(2000, older, signals.latest, signals.copied, false);
            assert!(*signals.copied.peek());
        });
    }
}

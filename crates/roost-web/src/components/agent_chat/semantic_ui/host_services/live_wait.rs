//! Bounded browser promises for one live-audio operation.
//! Live peers own cancellation; each wait owns and clears its deadline callback.
//! Device promises are drained separately so late permission never leaks tracks.

use futures::channel::oneshot;
use futures::future::{Either, select};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

pub(super) const MEDIA_TIMEOUT_MS: i32 = 30_000;

struct Deadline {
    window: web_sys::Window,
    timer: i32,
    _callback: Closure<dyn FnMut()>,
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.timer);
    }
}

pub(super) async fn bounded<T>(
    future: impl Future<Output = Result<T, String>>,
    step: &'static str,
) -> Result<T, String> {
    let window = web_sys::window().ok_or("Live audio requires a browser window.")?;
    let (sender, receiver) = oneshot::channel();
    let mut sender = Some(sender);
    let callback = Closure::wrap(Box::new(move || {
        if let Some(sender) = sender.take() {
            let _ = sender.send(());
        }
    }) as Box<dyn FnMut()>);
    let timer = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            MEDIA_TIMEOUT_MS,
        )
        .map_err(|_| "The browser could not start the live-audio deadline.")?;
    let _deadline = Deadline {
        window,
        timer,
        _callback: callback,
    };
    futures::pin_mut!(future);
    match select(future, receiver).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(format!("{step} did not respond in time.")),
    }
}

pub(super) async fn promise(
    promise: js_sys::Promise,
    step: &'static str,
) -> Result<JsValue, String> {
    bounded(
        async move {
            JsFuture::from(promise)
                .await
                .map_err(|error| refusal(step, error))
        },
        step,
    )
    .await
}

pub(super) fn refusal(step: &str, error: JsValue) -> String {
    let field = |key: &str| {
        js_sys::Reflect::get(&error, &JsValue::from_str(key))
            .ok()
            .and_then(|value| value.as_string())
            .filter(|value| !value.is_empty())
    };
    let detail = field("message")
        .or_else(|| field("name"))
        .or_else(|| error.as_string());
    match detail {
        Some(detail) => format!("{step}: {detail}"),
        None => format!("{step} was refused by the browser."),
    }
}

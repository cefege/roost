//! One driven tab: navigation, `window.__bench` queries, and trusted keyboard
//! input through `Input.dispatchKeyEvent` / `Input.insertText`. Called by
//! `browser::pair_browser` and every scenario.

use std::time::{Duration, Instant};

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::network::ClearBrowserCacheParams;
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::BenchError;

const CTRL_MODIFIER: i64 = 2;
const POLL_PERIOD: Duration = Duration::from_millis(10);

/// A tab with the probe installed.
#[derive(Debug, Clone)]
pub struct BenchPage {
    page: Page,
}

/// The key a single character produces, as CDP describes it.
struct KeyStroke<'text> {
    key: &'text str,
    code: &'text str,
    text: Option<&'text str>,
    virtual_key: i64,
    modifiers: i64,
}

impl BenchPage {
    pub(super) fn new(page: Page) -> Self {
        Self { page }
    }

    pub async fn goto(&self, url: &str) -> Result<(), BenchError> {
        self.page.goto(url).await.map_err(BenchError::browser)?;
        Ok(())
    }

    pub async fn close(self) {
        if let Err(error) = self.page.close().await {
            tracing::warn!(%error, "closing a tab failed");
        }
    }

    /// Empty Chromium's HTTP cache so the next navigation refetches the bundle.
    pub async fn clear_http_cache(&self) -> Result<(), BenchError> {
        self.page
            .execute(ClearBrowserCacheParams::default())
            .await
            .map_err(BenchError::browser)?;
        Ok(())
    }
    /// Evaluate `expression` (awaiting a returned promise) and decode its value.
    /// `undefined` and `null` decode from JSON `null`.
    pub async fn eval<T: DeserializeOwned>(&self, expression: &str) -> Result<T, BenchError> {
        let params = EvaluateParams::builder()
            .expression(expression)
            .await_promise(true)
            .return_by_value(true)
            .build()
            .map_err(BenchError::Browser)?;
        let result = self
            .page
            .evaluate_expression(params)
            .await
            .map_err(|error| BenchError::Browser(format!("evaluating `{expression}`: {error}")))?;
        let value = result.value().cloned().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|error| BenchError::Decode {
            context: format!("result of `{expression}`"),
            detail: error.to_string(),
        })
    }

    /// Poll a boolean expression every 10 ms until it holds. A page between
    /// documents throws; that is a "not yet", not a failure.
    pub async fn poll_until(
        &self,
        what: &str,
        deadline: Duration,
        condition: &str,
    ) -> Result<(), BenchError> {
        let started = Instant::now();
        let mut last_error = String::new();
        while started.elapsed() < deadline {
            match self.eval::<bool>(&format!("Boolean({condition})")).await {
                Ok(true) => return Ok(()),
                Ok(false) => last_error.clear(),
                Err(error) => last_error = error.to_string(),
            }
            tokio::time::sleep(POLL_PERIOD).await;
        }
        Err(BenchError::Timeout {
            what: what.to_string(),
            waited_ms: deadline.as_millis(),
            detail: if last_error.is_empty() {
                String::new()
            } else {
                format!(" (last error: {last_error})")
            },
        })
    }

    /// Arm the probe for `needle` and return the page time it was armed at.
    pub async fn arm_text(&self, needle: &str) -> Result<f64, BenchError> {
        self.eval(&format!(
            "window.__bench.arm('text', {}, 0)",
            js_string(needle)
        ))
        .await
    }

    /// Arm the probe for `ch` occurring exactly `target` times in the grid.
    pub async fn arm_count(&self, ch: char, target: usize) -> Result<f64, BenchError> {
        self.eval(&format!(
            "window.__bench.arm('count', {}, {target})",
            js_string(&ch.to_string())
        ))
        .await
    }

    /// The page time the armed condition first held.
    pub async fn await_armed(&self, timeout: Duration) -> Result<f64, BenchError> {
        self.eval(&format!(
            "window.__bench.awaitArmed({})",
            timeout.as_millis()
        ))
        .await
    }

    /// Arm for `needle` and wait for it, for a step whose timing is not measured.
    pub async fn wait_for_text(&self, needle: &str, timeout: Duration) -> Result<f64, BenchError> {
        self.arm_text(needle).await?;
        self.await_armed(timeout).await
    }

    pub async fn count_char(&self, ch: char) -> Result<usize, BenchError> {
        self.eval(&format!(
            "window.__bench.countChar({})",
            js_string(&ch.to_string())
        ))
        .await
    }

    /// Give the terminal keyboard focus with a trusted click in its centre.
    pub async fn focus_terminal(&self) -> Result<(), BenchError> {
        self.page
            .bring_to_front()
            .await
            .map_err(BenchError::browser)?;
        let centre: Option<(f64, f64)> = self
            .eval(
                "(() => { const root = document.querySelector('.wterm'); if (!root) return null; \
                 const box = root.getBoundingClientRect(); \
                 return [box.left + box.width / 2, box.top + box.height / 2]; })()",
            )
            .await?;
        let (x, y) =
            centre.ok_or_else(|| BenchError::Browser("no `.wterm` to focus".to_string()))?;
        for kind in [
            DispatchMouseEventType::MousePressed,
            DispatchMouseEventType::MouseReleased,
        ] {
            let event = DispatchMouseEventParams::builder()
                .r#type(kind)
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .click_count(1)
                .build()
                .map_err(BenchError::Browser)?;
            self.page
                .execute(event)
                .await
                .map_err(BenchError::browser)?;
        }
        // Both clients route keys through a hidden textarea inside the pane. A
        // click that left focus anywhere else (an open dialog keeps it) would
        // send every keystroke into the void, so it is checked, not assumed.
        let focused: String = self
            .eval(
                "(() => { const active = document.activeElement; \
                 return active ? active.tagName + '.' + active.className : ''; })()",
            )
            .await?;
        if !focused.starts_with("TEXTAREA") {
            return Err(BenchError::Browser(format!(
                "clicking the terminal focused `{focused}`, not its input textarea"
            )));
        }
        tracing::info!(focused = %focused, "terminal focused");
        Ok(())
    }

    /// Close any first-visit dialog (v2 opens "What's new" on a fresh
    /// profile) with Escape, as the user would; the client records it as seen,
    /// so later navigations in this profile skip it. Returns how many closed.
    pub async fn dismiss_open_dialogs(&self, settle: Duration) -> Result<usize, BenchError> {
        const VISIBLE_DIALOG: &str = "Array.from(document.querySelectorAll('.roost-dialog, dialog[open], [role=dialog]')).some((node) => node.getClientRects().length > 0)";
        if self
            .poll_until("a first-visit dialog", settle, VISIBLE_DIALOG)
            .await
            .is_err()
        {
            return Ok(0);
        }
        for attempt in 1..=3 {
            self.press(KeyStroke {
                key: "Escape",
                code: "Escape",
                text: None,
                virtual_key: 27,
                modifiers: 0,
            })
            .await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            if !self.eval::<bool>(VISIBLE_DIALOG).await? {
                tracing::info!(attempt, "first-visit dialog dismissed");
                return Ok(attempt);
            }
        }
        Err(BenchError::Browser(
            "a dialog stayed open after three Escapes".to_string(),
        ))
    }

    /// Insert `text` as one IME commit: the fastest way to type a command line.
    pub async fn type_text(&self, text: &str) -> Result<(), BenchError> {
        self.page
            .execute(InsertTextParams::new(text))
            .await
            .map_err(BenchError::browser)?;
        Ok(())
    }

    pub async fn press_enter(&self) -> Result<(), BenchError> {
        self.press(KeyStroke {
            key: "Enter",
            code: "Enter",
            text: Some("\r"),
            virtual_key: 13,
            modifiers: 0,
        })
        .await
    }

    /// One printable ASCII letter, lowercase.
    pub async fn press_letter(&self, letter: char) -> Result<(), BenchError> {
        let key = letter.to_string();
        let code = format!("Key{}", letter.to_ascii_uppercase());
        self.press(KeyStroke {
            key: &key,
            code: &code,
            text: Some(&key),
            virtual_key: i64::from(u32::from(letter.to_ascii_uppercase())),
            modifiers: 0,
        })
        .await
    }

    /// Ctrl-U: bash's kill-line, used to clear the typed echo run.
    pub async fn press_ctrl_u(&self) -> Result<(), BenchError> {
        self.press(KeyStroke {
            key: "u",
            code: "KeyU",
            text: None,
            virtual_key: 85,
            modifiers: CTRL_MODIFIER,
        })
        .await
    }

    /// Run one shell command line: type it and press Enter.
    pub async fn submit_line(&self, line: &str) -> Result<(), BenchError> {
        self.type_text(line).await?;
        self.press_enter().await
    }

    async fn press(&self, stroke: KeyStroke<'_>) -> Result<(), BenchError> {
        for kind in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
            let is_down = kind == DispatchKeyEventType::KeyDown;
            let mut event = DispatchKeyEventParams::builder()
                .r#type(kind)
                .key(stroke.key)
                .code(stroke.code)
                .windows_virtual_key_code(stroke.virtual_key)
                .modifiers(stroke.modifiers);
            if let (true, Some(text)) = (is_down, stroke.text) {
                event = event.text(text).unmodified_text(text);
            }
            let event = event.build().map_err(BenchError::Browser)?;
            self.page
                .execute(event)
                .await
                .map_err(BenchError::browser)?;
        }
        Ok(())
    }
}

/// `value` as a JavaScript string literal.
fn js_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

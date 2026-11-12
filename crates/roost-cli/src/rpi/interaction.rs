//! Terminal implementation of rpi-ai authentication prompts and notifications.
//! Rpi login owns the flow token; each prompt owns its EventStream and raw-mode
//! guard so callback cancellation cannot leave an input task reading the terminal.

use std::io::{IsTerminal, Write};

use crossterm::cursor::MoveToColumn;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};
use futures_util::StreamExt;
use rpi_ai::auth::{AuthEvent, AuthInteraction, AuthPrompt, BoxFutureSend, ModelsError, ModelsErrorCode, SelectOption};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct TerminalInteraction {
    cancellation: CancellationToken,
    prompt_lock: Mutex<()>,
}

struct RawPrompt {
    restore_cooked: bool,
}

struct PromptInput {
    message: String,
    hidden: bool,
    options: Vec<SelectOption>,
    selected: usize,
    text: String,
}

impl TerminalInteraction {
    pub fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation, prompt_lock: Mutex::new(()) }
    }

    async fn read_prompt(&self, prompt: AuthPrompt) -> Result<String, ModelsError> {
        let prompt_cancel = prompt.signal().unwrap_or_default();
        let _prompt_lock = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err(ModelsError::aborted()),
            _ = prompt_cancel.cancelled() => return Err(ModelsError::aborted()),
            guard = self.prompt_lock.lock() => guard,
        };
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            return Err(terminal_error());
        }
        let mut input = PromptInput::new(prompt)?;
        let _raw_prompt = RawPrompt::begin().map_err(|_| terminal_error())?;
        let mut events = EventStream::new();
        input.redraw().map_err(|_| terminal_error())?;
        loop {
            let event = tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => return Err(ModelsError::aborted()),
                _ = prompt_cancel.cancelled() => return Err(ModelsError::aborted()),
                event = events.next() => event,
            };
            match event {
                None => {
                    self.cancellation.cancel();
                    return Err(ModelsError::aborted());
                }
                Some(Err(_)) => return Err(terminal_error()),
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    if key.code == KeyCode::Esc || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c' | 'd' | 'z')))
                    {
                        self.cancellation.cancel();
                        return Err(ModelsError::aborted());
                    }
                    if key.code == KeyCode::Enter {
                        return Ok(input.finish());
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        if key.code == KeyCode::Char('u') { input.text.clear(); }
                    } else {
                        input.key(key.code);
                    }
                    input.redraw().map_err(|_| terminal_error())?;
                }
                Some(Ok(Event::Paste(text))) if input.options.is_empty() => {
                    input.text.extend(text.chars().filter(|character| !character.is_control()));
                    input.redraw().map_err(|_| terminal_error())?;
                }
                _ => {}
            }
        }
    }
}

impl AuthInteraction for TerminalInteraction {
    fn signal(&self) -> Option<CancellationToken> {
        Some(self.cancellation.clone())
    }

    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFutureSend<'a, Result<String, ModelsError>> {
        Box::pin(self.read_prompt(prompt))
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                if let Some(instructions) = instructions { notify_line(&instructions); }
                notify_line(&url);
            }
            AuthEvent::DeviceCode { user_code, verification_uri, .. } => {
                notify_line(&verification_uri);
                notify_line(&format!("Device code: {}", terminal_text(&user_code)));
            }
            AuthEvent::Info { message, links } => {
                notify_line(&message);
                for link in links.unwrap_or_default() { notify_line(&link.url); }
            }
            AuthEvent::Progress { message } => notify_line(&message),
        }
    }
}

impl RawPrompt {
    fn begin() -> std::io::Result<Self> {
        let restore_cooked = !is_raw_mode_enabled()?;
        if restore_cooked { enable_raw_mode()?; }
        Ok(Self { restore_cooked })
    }
}

impl Drop for RawPrompt {
    fn drop(&mut self) {
        let _ = write!(std::io::stderr(), "\r\n");
        if self.restore_cooked { let _ = disable_raw_mode(); }
    }
}

impl PromptInput {
    fn new(prompt: AuthPrompt) -> Result<Self, ModelsError> {
        let (message, hidden, options) = match prompt {
            AuthPrompt::Text { message, .. } => (message, false, Vec::new()),
            AuthPrompt::Secret { message, .. } | AuthPrompt::ManualCode { message, .. } => {
                (message, true, Vec::new())
            }
            AuthPrompt::Select { message, options, .. } => {
                if options.is_empty() { return Err(terminal_error()); }
                (message, false, options)
            }
        };
        Ok(Self { message: terminal_text(&message), hidden, options, selected: 0, text: String::new() })
    }

    fn key(&mut self, code: KeyCode) {
        if self.options.is_empty() {
            match code {
                KeyCode::Backspace => { self.text.pop(); }
                KeyCode::Char(character) if !character.is_control() => self.text.push(character),
                _ => {}
            }
        } else {
            match code {
                KeyCode::Up | KeyCode::Left => {
                    self.selected = if self.selected == 0 { self.options.len() - 1 } else { self.selected - 1 };
                }
                KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                    self.selected = (self.selected + 1) % self.options.len();
                }
                _ => {}
            }
        }
    }

    fn finish(mut self) -> String {
        if self.options.is_empty() { self.text } else { self.options.swap_remove(self.selected).id }
    }

    fn redraw(&self) -> std::io::Result<()> {
        let mut stderr = std::io::stderr();
        crossterm::queue!(stderr, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        write!(stderr, "{}: ", self.message)?;
        if let Some(option) = self.options.get(self.selected) {
            write!(stderr, "{} [arrows, Enter]", terminal_text(&option.label))?;
        } else if !self.hidden {
            write!(stderr, "{}", self.text)?;
        }
        stderr.flush()
    }
}

fn terminal_error() -> ModelsError {
    ModelsError::new(ModelsErrorCode::Auth, "Rpi authentication terminal is unavailable")
}

fn terminal_text(text: &str) -> String {
    text.chars().filter(|character| !character.is_control()).collect()
}

fn notify_line(text: &str) {
    let _ = write!(std::io::stderr(), "{}\r\n", terminal_text(text));
}

//! Read retained PNGs from the terminal core owned by each live session.
//! This is the production implementation of the browser image capability and
//! keeps image bytes in the core rather than copying them into worker state.

use std::sync::Arc;

use roost_protocol::wire::brand::SessionId;

use crate::browser_commands::Boxed;
use crate::browser_commands::terminal_image::TerminalImages;
use crate::session::lifecycle::SessionTable;

/// The session table as an image source.
#[derive(Debug)]
pub struct SessionTerminalImages {
    sessions: Arc<SessionTable>,
}

impl SessionTerminalImages {
    /// Read image content from the worker's live sessions.
    pub fn new(sessions: Arc<SessionTable>) -> Self {
        Self { sessions }
    }
}

impl TerminalImages for SessionTerminalImages {
    fn png(
        &self,
        session_id: SessionId,
        image_key: u64,
    ) -> Boxed<Result<Option<Arc<[u8]>>, crate::browser_commands::Refusal>> {
        let result = self
            .sessions
            .with_record_mut(&session_id, |record| {
                record.terminal_core.image_png(image_key)
            })
            .ok_or_else(|| {
                crate::browser_commands::Refusal::failed("get-terminal-image", "unknown session")
            });
        Box::pin(async move { result })
    }
}

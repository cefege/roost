//! Demand-driven PNG access for images retained by a live terminal session.
//! The terminal core owns image retention; this capability only projects its
//! stable content key into the correlated browser-command reply.

use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};

/// Encoded pixels retained by one live session.
pub trait TerminalImages: Send + Sync {
    /// PNG bytes for a content key, or `None` when the session/key is absent.
    fn png(
        &self,
        session_id: SessionId,
        image_key: u64,
    ) -> Boxed<Result<Option<std::sync::Arc<[u8]>>, Refusal>>;
}

/// Answer one terminal image request.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let ClientControlFrame::GetTerminalImage {
        session_id,
        image_key,
        ..
    } = &command.frame
    else {
        return Err(Refusal::failed(
            "get-terminal-image",
            format!("{} is not a terminal image request", command.frame.kind()),
        ));
    };
    let png = deps.images.png(session_id.clone(), *image_key).await?;
    let png = png.map(|bytes| {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes.as_ref())
    });
    Ok(Answered::Reply(Reply::ok(
        &command.request_id,
        serde_json::json!({ "png": png }),
    )))
}

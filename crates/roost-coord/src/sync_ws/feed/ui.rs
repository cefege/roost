//! The browser-only UI stream: a tab's reported state, a legacy command, and
//! the acknowledged apply that is addressed to exactly one socket.
//!
//! Ported from `apps/coord/src/sync/sync-feed-ui.ts`. This is the adapter whose
//! fan-out rule is NOT "everyone watching", and the difference is the whole
//! file: a worker socket and a read-only feed stay subscribed for delivery
//! counts while dropping every live UI frame, and an `Apply` goes to the one
//! socket that acknowledged it rather than to every browser. Flattening this
//! into the general bus-to-frame path is how a cursor report from one tab ends
//! up executing in another.
//!
//! The frames are CONTROLS. `frame_meta_for` reads a `uiState` or `uiCommand`
//! frame as unsequenced and outside every queue, which is what makes them
//! answer the socket that asked without consuming the application ACK window.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::buffa::MessageField;
use roost_proto::{FirehoseFrame, UiCommandFrame, UiStateFrame};

use crate::events::bus_messages::UiBusMsg;
use crate::sync_ws::feed::FeedFrame;
use crate::ui_state::state_owner::UiStateOwner;

/// What one Sync socket may receive of the UI stream.
///
/// `browser_ui` is the gate v2 passes as `browserUi`; a worker socket and a
/// read-only feed set it false and receive nothing here while remaining
/// subscribed. `socket_id` is the Sync-v2 generation identity an `Apply` is
/// addressed to, and is `None` on a feed with no v2 socket at all -- which is
/// why an `Apply` is refused there rather than broadcast.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiViewer {
    /// Whether this socket is a browser with the UI capability.
    pub browser_ui: bool,
    /// The socket an acknowledged apply is addressed to.
    pub socket_id: Option<String>,
}

impl UiViewer {
    /// A browser on the socket with this identity.
    #[must_use]
    pub fn browser(socket_id: impl Into<String>) -> Self {
        Self {
            browser_ui: true,
            socket_id: Some(socket_id.into()),
        }
    }

    /// A feed with no UI capability: a worker socket, or a read-only browser
    /// that asked for nothing.
    #[must_use]
    pub fn suppressed() -> Self {
        Self::default()
    }
}

/// One UI bus message as the frame this viewer may receive, or `None` when it
/// may not.
///
/// The two refusals are different and both load-bearing. A non-browser drops
/// everything. A browser drops an `Apply` addressed to a different socket but
/// still receives a legacy `Command`, which by construction has no addressee
/// and is a broadcast (`sync-feed-ui.ts:36-39`).
pub fn ui_bus_frame(message: &UiBusMsg, viewer: &UiViewer) -> Option<FeedFrame> {
    if !viewer.browser_ui {
        return None;
    }
    let frame = match message {
        UiBusMsg::State { fp, tab_id, state } => Frame::UiState(Box::new(UiStateFrame {
            fp: fp.clone(),
            tab_id: tab_id.clone(),
            state: MessageField::some(state.clone()),
            ..UiStateFrame::default()
        })),
        UiBusMsg::Command {
            target_tab_id,
            command,
        } => Frame::UiCommand(Box::new(ui_command_frame(target_tab_id, command, "", ""))),
        UiBusMsg::Apply {
            target_tab_id,
            target_socket_id,
            correlation_id,
            command,
        } => {
            if viewer.socket_id.as_deref() != Some(target_socket_id.as_str()) {
                return None;
            }
            Frame::UiCommand(Box::new(ui_command_frame(
                target_tab_id,
                command,
                correlation_id,
                target_socket_id,
            )))
        }
    };
    Some(FeedFrame::of(FirehoseFrame {
        frame: Some(frame),
        ..FirehoseFrame::default()
    }))
}

/// The retained UI reports, as the seed frames a fresh subscriber starts from.
///
/// The bus retains nothing of any UI message (`ui_bus` is built with a bound of
/// zero), so this snapshot is the only way a browser that has been away learns
/// which tabs exist. It is also the reason the seed is a control: it precedes
/// any command, and a client that applied it to a socket would be applying a
/// state nobody asked for (`sync-feed-ui.ts:54-64`).
#[must_use]
pub fn ui_state_seed_frames(states: &UiStateOwner) -> Vec<FeedFrame> {
    states
        .list()
        .into_iter()
        .map(|entry| {
            FeedFrame::of(FirehoseFrame {
                frame: Some(Frame::UiState(Box::new(UiStateFrame {
                    fp: entry.fingerprint,
                    tab_id: entry.tab_id,
                    state: MessageField::some(entry.state),
                    ..UiStateFrame::default()
                }))),
                ..FirehoseFrame::default()
            })
        })
        .collect()
}

fn ui_command_frame(
    target_tab_id: &str,
    command: &roost_proto::UiCommand,
    correlation_id: &str,
    target_socket_id: &str,
) -> UiCommandFrame {
    UiCommandFrame {
        target_tab_id: target_tab_id.to_owned(),
        command: MessageField::some(command.clone()),
        correlation_id: correlation_id.to_owned(),
        target_socket_id: target_socket_id.to_owned(),
        ..UiCommandFrame::default()
    }
}

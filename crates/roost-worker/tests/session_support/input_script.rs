//! The keeper input half `ScriptedKeeper` answers with: what reached the "PTY",
//! and how the keeper answers each acknowledged batch. By default every batch
//! is written and acknowledged in full; a test scripts the next answer to
//! exercise a refusal, a lost result, or a result it releases itself.

use std::collections::VecDeque;
use std::sync::Mutex;

use roost_worker::session::keeper_channels::{
    InputNotWritten, KeeperFault, KeeperInputCommand, KeeperInputResult,
};
use tokio::sync::oneshot;

/// How the keeper answers the next acknowledged batch.
pub enum ScriptedAnswer {
    /// The request never reached the socket; nothing is recorded as written.
    NotWritten(InputNotWritten),
    /// Written, and the keeper answers with this.
    Answered(KeeperInputResult),
    /// Written, and the answer is whatever the test sends later.
    Held(oneshot::Receiver<KeeperInputResult>),
}

#[derive(Default)]
pub struct InputScript {
    answers: Mutex<VecDeque<ScriptedAnswer>>,
    written: Mutex<Vec<(u16, Vec<u8>)>>,
    legacy: Mutex<Vec<(u16, Vec<u8>)>>,
}

impl InputScript {
    /// Answer the next acknowledged batch this way.
    pub fn answer_next(&self, answer: ScriptedAnswer) {
        self.answers.lock().expect("held").push_back(answer);
    }

    /// Write the next batch and hold its answer until the test sends one.
    pub fn hold_next(&self) -> oneshot::Sender<KeeperInputResult> {
        let (release, held) = oneshot::channel();
        self.answer_next(ScriptedAnswer::Held(held));
        release
    }

    /// Every acknowledged batch that reached the socket, in order.
    pub fn written(&self) -> Vec<(u16, Vec<u8>)> {
        self.written.lock().expect("held").clone()
    }

    /// Every legacy chunk, in order.
    pub fn legacy(&self) -> Vec<(u16, Vec<u8>)> {
        self.legacy.lock().expect("held").clone()
    }

    pub fn begin(&self, channel_id: u16, bytes: Vec<u8>) -> KeeperInputCommand {
        let answer = self.answers.lock().expect("held").pop_front();
        if let Some(ScriptedAnswer::NotWritten(reason)) = answer {
            return KeeperInputCommand::not_written(reason);
        }
        let full = KeeperInputResult::Ack {
            written: bytes.len() as u32,
        };
        self.written.lock().expect("held").push((channel_id, bytes));
        match answer {
            Some(ScriptedAnswer::Held(held)) => KeeperInputCommand {
                admission: Ok(()),
                result: Box::pin(async move {
                    held.await.unwrap_or(KeeperInputResult::Ambiguous {
                        written: None,
                        reason: "disconnected".to_string(),
                    })
                }),
            },
            Some(ScriptedAnswer::Answered(result)) => written_with(result),
            Some(ScriptedAnswer::NotWritten(_)) | None => written_with(full),
        }
    }

    pub fn write_legacy(&self, channel_id: u16, bytes: &[u8]) -> Result<(), KeeperFault> {
        self.legacy
            .lock()
            .expect("held")
            .push((channel_id, bytes.to_vec()));
        Ok(())
    }
}

fn written_with(result: KeeperInputResult) -> KeeperInputCommand {
    KeeperInputCommand {
        admission: Ok(()),
        result: Box::pin(std::future::ready(result)),
    }
}

//! The second connection the ordering assertions depend on.
//!
//! See the parent module's header: `LiveEffects` is synchronous, as it is in v2,
//! so the visibility probe cannot await and the reader owns its connection on a
//! dedicated thread instead.

#![allow(dead_code)]

use std::path::Path;
use std::sync::mpsc;

use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

/// A second connection to the same file, reachable from a synchronous callback.
pub struct SyncReader {
    questions: mpsc::Sender<Question>,
}

enum Question {
    /// Is a row with this worker and this exact payload visible yet?
    Committed {
        /// The worker whose row to look for.
        worker_fp: String,
        /// The payload the append persisted.
        payload_json: String,
        /// Where the answer goes.
        answer: mpsc::Sender<bool>,
    },
    /// Stop the thread.
    Stop,
}

impl SyncReader {
    /// Open the second connection on its own thread and runtime.
    pub fn open(path: &Path) -> Self {
        let path = path.to_path_buf();
        let (questions, inbox) = mpsc::channel::<Question>();
        let thread = std::thread::Builder::new()
            .name("roost-event-reader".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the reader runtime builds");
                runtime.block_on(async {
                    let options = SqliteConnectOptions::new().filename(&path);
                    let Ok(mut connection) = SqliteConnection::connect_with(&options).await else {
                        // A reader that cannot open sees nothing, which every
                        // assertion that depends on it will fail loudly.
                        return;
                    };
                    while let Ok(question) = inbox.recv() {
                        match question {
                            Question::Committed {
                                worker_fp,
                                payload_json,
                                answer,
                            } => {
                                let visible = sqlx::query(
                                    "SELECT 1 FROM events WHERE worker_fp = ? AND payload_json = ?",
                                )
                                .bind(&worker_fp)
                                .bind(&payload_json)
                                .fetch_optional(&mut connection)
                                .await
                                .map(|row| row.is_some())
                                .unwrap_or(false);
                                let _ = answer.send(visible);
                            }
                            Question::Stop => return,
                        }
                    }
                });
            })
            .expect("the reader thread starts");
        // The join handle is deliberately dropped: the thread ends when the
        // `Stop` question is answered, and a detached reader cannot wedge a test
        // binary on a join at process exit.
        drop(thread);
        Self { questions }
    }

    /// Whether a second connection can see a committed row for this payload.
    ///
    /// A `false` here is the whole point of the module: it means the publication
    /// that asked ran before the transaction committed, because a WAL reader never
    /// sees uncommitted work.
    pub fn row_is_committed(&self, worker_fp: &str, payload_json: &str) -> bool {
        let (answer, replies) = mpsc::channel();
        if self
            .questions
            .send(Question::Committed {
                worker_fp: worker_fp.to_owned(),
                payload_json: payload_json.to_owned(),
                answer,
            })
            .is_err()
        {
            return false;
        }
        replies.recv().unwrap_or(false)
    }

    /// Stop the reader thread. The question is what ends it: the thread is
    /// blocked on the channel, and answering `Stop` returns it.
    pub fn stop(&self) {
        let _ = self.questions.send(Question::Stop);
    }
}

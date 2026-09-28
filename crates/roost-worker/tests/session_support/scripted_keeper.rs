//! The keeper a session test drives: channel list, history, geometry,
//! reattach, kill, resize and input, each answered from a script and recorded.
//! Split from `fakes.rs` so each file stays under the size cap.

use std::sync::{Arc, Mutex};

use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::payloads::TerminalState;
use roost_worker::session::keeper_channels::{
    KeeperChannels, KeeperFault, KeeperInputCommand, SurvivorHistory,
};
use roost_worker::session::sinks::ChannelBinding;

use super::input_script::InputScript;

/// A keeper that answers from a script and remembers what it was told.
///
/// `Default` is written out rather than derived, and the reason is the one
/// field that cannot have a derived one: a derived `Default` would hand
/// `applied` a `TerminalState` of `0x0`, and `terminal_state()` would then
/// report that the keeper applied a zero-sized terminal. No PTY can be that,
/// and a test that asserted on it would be asserting on a value the protocol
/// cannot carry. `80x24` is the same geometry `with_survivor` starts from, so
/// a defaulted keeper and a survivor keeper agree about what a terminal is.
pub struct ScriptedKeeper {
    pub channels: Mutex<Vec<KeeperChannel>>,
    pub history: Mutex<SurvivorHistory>,
    pub applied: Mutex<TerminalState>,
    pub delivered: Mutex<Option<Arc<dyn ChannelBinding>>>,
    /// Bytes this keeper emits the instant the channel is rebound, i.e. inside
    /// the window the adoption stages.
    ///
    /// SCRIPTED AT REATTACH RATHER THAN BY THE TEST, because the window being
    /// tested is "while the core is being rebuilt" and the test used to open
    /// it by calling `delivered().on_output(..)` BEFORE `adopt_survivor` ran —
    /// which only worked while the rebind was the first thing the function
    /// did. The rebind now follows both reads, so the byte has to arrive with
    /// it, and that is the same window the real keeper's stream occupies.
    pub on_rebind: Mutex<Vec<Vec<u8>>>,
    pub killed: Mutex<Vec<u16>>,
    pub resized: Mutex<Vec<(u16, u64, u16, u16)>>,
    pub list_fails: Mutex<bool>,
    /// What the "PTY" was sent, and how each acknowledged batch is answered.
    pub input: InputScript,
}

impl Default for ScriptedKeeper {
    fn default() -> Self {
        Self {
            channels: Mutex::new(Vec::new()),
            history: Mutex::new(SurvivorHistory::default()),
            applied: Mutex::new(TerminalState {
                applied_seq: 0,
                cols: 80,
                rows: 24,
            }),
            delivered: Mutex::new(None),
            on_rebind: Mutex::new(Vec::new()),
            killed: Mutex::new(Vec::new()),
            resized: Mutex::new(Vec::new()),
            list_fails: Mutex::new(false),
            input: InputScript::default(),
        }
    }
}

impl KeeperChannels for ScriptedKeeper {
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault> {
        if *self.list_fails.lock().expect("held") {
            return Err(KeeperFault {
                operation: "live_channels",
                reason: "the socket went away".to_string(),
            });
        }
        Ok(self.channels.lock().expect("held").clone())
    }
    fn channel_history(&self, _channel_id: u16) -> Result<SurvivorHistory, KeeperFault> {
        Ok(self.history.lock().expect("held").clone())
    }
    fn terminal_state(&self, _channel_id: u16) -> Result<TerminalState, KeeperFault> {
        Ok(*self.applied.lock().expect("held"))
    }
    fn reattach_with_history(
        &self,
        _channel_id: u16,
        _pid: u32,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<SurvivorHistory, KeeperFault> {
        // The staged bytes go in AFTER the binding is stored and BEFORE this
        // call returns, so they land in a `RecordBinding` still in `Staged`
        // mode — which is the window the adoption is supposed to bridge.
        for chunk in self.on_rebind.lock().expect("held").drain(..) {
            binding.on_output(&chunk);
        }
        *self.delivered.lock().expect("held") = Some(binding);
        Ok(self.history.lock().expect("held").clone())
    }
    fn kill_channel(&self, channel_id: u16) -> Result<(), KeeperFault> {
        self.killed.lock().expect("held").push(channel_id);
        Ok(())
    }
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<roost_keeper::client_resize::ResizeOutcome, KeeperFault> {
        self.resized
            .lock()
            .expect("held")
            .push((channel_id, seq, cols, rows));
        Ok(roost_keeper::client_resize::ResizeOutcome::Applied { seq, cols, rows })
    }
    fn begin_input(&self, channel_id: u16, bytes: Vec<u8>) -> KeeperInputCommand {
        self.input.begin(channel_id, bytes)
    }
    fn write_legacy_input(&self, channel_id: u16, bytes: &[u8]) -> Result<(), KeeperFault> {
        self.input.write_legacy(channel_id, bytes)
    }
}

impl ScriptedKeeper {
    pub fn with_survivor(channel_id: u16, pid: u32) -> Self {
        Self {
            channels: Mutex::new(vec![KeeperChannel { channel_id, pid }]),
            history: Mutex::new(SurvivorHistory {
                base_cols: 80,
                base_rows: 24,
                ..SurvivorHistory::default()
            }),
            applied: Mutex::new(TerminalState {
                applied_seq: 7,
                cols: 80,
                rows: 24,
            }),
            ..ScriptedKeeper::default()
        }
    }
    pub fn delivered(&self) -> Arc<dyn ChannelBinding> {
        self.delivered
            .lock()
            .expect("held")
            .clone()
            .expect("adoption delivers before anything else")
    }
    pub fn killed(&self) -> Vec<u16> {
        self.killed.lock().expect("held").clone()
    }
}

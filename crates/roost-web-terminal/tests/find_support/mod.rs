//! Shared fixture for the terminal-find controller suites: a fake renderer host
//! (anchor, highlight log, jumps, find-read ends), a scripted coordinator search
//! RPC with request/cancellation spies, a captured debounce clock, and a
//! backfill that pulls every row. Ports `apps/web/tests/terminalFindController-test-harness.ts`
//! and `apps/web/tests/terminalFindController-search-spy.ts`.
#![allow(dead_code)]

use std::collections::VecDeque;

use roost_client_core::search::{RawMatch, SearchPage};
use roost_web_terminal::BackfillAnchor;
use roost_web_terminal::find::{
    ActiveHit, FindCommand, FindHost, FindQueryOptions, FindRequest, HitRows, SearchReply,
    SearchStop, TerminalFind,
};

pub const EPOCH_A: &str = "grid-a:0";
pub const EPOCH_B: &str = "grid-b:0";
pub const SESSION: &str = "session-1";
pub const SEED: &str = "5b0e8c3e-6f7c-4d62-9a55-3c1f2b8d9e10";

/// One highlight publication, as the painter received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub rows: Vec<u32>,
    pub active: Option<ActiveHit>,
}

/// One call the controller made on its host, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCall {
    Publish(Published),
    Reveal(u32),
    EndFindRead,
}

/// The fake renderer: v2's `{ sbBase: 500, cols: 80, total: 2000 }` anchor.
#[derive(Debug, Clone)]
pub struct FakeHost {
    pub anchor: BackfillAnchor,
    pub calls: Vec<HostCall>,
}

impl FakeHost {
    pub fn jumps(&self) -> Vec<u32> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                HostCall::Reveal(row) => Some(*row),
                _ => None,
            })
            .collect()
    }
    pub fn published(&self) -> Vec<Published> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                HostCall::Publish(published) => Some(published.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn last(&self) -> Published {
        self.published().pop().unwrap_or(Published {
            rows: Vec::new(),
            active: None,
        })
    }
    pub fn clear_jumps(&mut self) {
        self.calls.retain(|call| !matches!(call, HostCall::Reveal(_)));
    }
}

impl FindHost for FakeHost {
    fn anchor(&self) -> Option<BackfillAnchor> {
        Some(self.anchor.clone())
    }
    fn publish_hits(&mut self, hits: HitRows, active: Option<ActiveHit>) {
        self.calls.push(HostCall::Publish(Published {
            rows: hits.rows().collect(),
            active,
        }));
    }
    fn reveal_row(&mut self, row: u32) {
        self.calls.push(HostCall::Reveal(row));
    }
    fn end_find_read(&mut self) {
        self.calls.push(HostCall::EndFindRead);
    }
}

/// What the scripted search RPC does with one request.
pub enum RpcAnswer {
    Reply(SearchReply),
    Error,
    /// Leave the request unanswered until the test resolves it.
    Hold,
}

type SearchRpc = Box<dyn FnMut(&FindRequest, &mut FakeHost) -> RpcAnswer>;

/// The window and stop reason of one reply; v2's `ReplyOptions`.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub stop: SearchStop,
    pub start: u32,
    pub end: u32,
    pub next: Option<u32>,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            stop: SearchStop::Complete,
            start: 0,
            end: 2000,
            next: None,
        }
    }
}

/// v2's `reply(rows, gridEpoch, options)`: every match at col 3, len 4.
pub fn reply(rows: &[u32], epoch: &str, shape: Shape) -> SearchReply {
    SearchReply {
        matches: rows
            .iter()
            .map(|row| RawMatch {
                row: *row,
                col: 3,
                len: 4,
                preview: format!("line {row}"),
            })
            .collect(),
        page: SearchPage {
            scanned_start_row: shape.start,
            scanned_end_row: shape.end,
            next_before_row: shape.next,
        },
        grid_epoch: epoch.to_string(),
        stop: shape.stop,
    }
}

/// `createFindHarness`: the controller, its host, and every spy the suites read.
pub struct FindHarness {
    pub find: TerminalFind,
    pub host: FakeHost,
    pub requests: Vec<FindRequest>,
    pub cancellations: Vec<String>,
    pub pulled: Vec<u32>,
    pub held: Vec<FindRequest>,
    pub max_in_flight: usize,
    armed_debounce: Option<u64>,
    now_ms: u64,
    rpc: SearchRpc,
}

impl FindHarness {
    pub fn new() -> Self {
        Self {
            find: TerminalFind::new(SESSION, SEED),
            host: FakeHost {
                anchor: BackfillAnchor {
                    sb_base: 500,
                    cols: 80,
                    total: 2000,
                    grid_epoch: EPOCH_A.to_string(),
                },
                calls: Vec::new(),
            },
            requests: Vec::new(),
            cancellations: Vec::new(),
            pulled: Vec::new(),
            held: Vec::new(),
            max_in_flight: 0,
            armed_debounce: None,
            now_ms: 0,
            rpc: Box::new(|_, _| RpcAnswer::Reply(reply(&[], EPOCH_A, Shape::default()))),
        }
    }

    pub fn set_rpc(&mut self, rpc: impl FnMut(&FindRequest, &mut FakeHost) -> RpcAnswer + 'static) {
        self.rpc = Box::new(rpc);
    }

    pub fn set_query(&mut self, query: &str) {
        let commands = self
            .find
            .set_query(query, FindQueryOptions::default(), self.now_ms, &mut self.host);
        self.perform(commands);
    }
    pub fn toggle_regex(&mut self) {
        let commands = self.find.toggle_regex(self.now_ms);
        self.perform(commands);
    }
    pub fn toggle_case_sensitive(&mut self) {
        let commands = self.find.toggle_case_sensitive(self.now_ms);
        self.perform(commands);
    }
    pub fn step(&mut self, delta: i64) {
        let commands = self.find.step(delta, &mut self.host);
        self.perform(commands);
    }
    pub fn close_find(&mut self) {
        let commands = self.find.close_find(&mut self.host);
        self.perform(commands);
    }
    pub fn dispose(&mut self) {
        let commands = self.find.dispose();
        self.perform(commands);
    }

    /// Fire the captured debounce timer, if one is armed.
    pub fn fire_debounce(&mut self) {
        let Some(at_ms) = self.armed_debounce.take() else {
            return;
        };
        self.now_ms = at_ms;
        let commands = self.find.on_debounce(at_ms, &mut self.host);
        self.perform(commands);
    }

    /// Answer a held request, as its late RPC settling would.
    pub fn resolve_held(&mut self, search_id: &str, answer: SearchReply) {
        self.held.retain(|request| request.search_id != search_id);
        let commands = self.find.on_page(search_id, &answer, &mut self.host);
        self.perform(commands);
    }

    pub fn rows(&self) -> Vec<u32> {
        self.find.publication().matches().iter().map(|m| m.row).collect()
    }
    pub fn rows_with_epoch(&self) -> Vec<(u32, String)> {
        let matches = self.find.publication().matches();
        matches.iter().map(|m| (m.row, m.epoch.clone())).collect()
    }
    pub fn index(&self) -> u32 {
        self.find.publication().index()
    }

    /// Perform commands the way the pane host does, until nothing is owed.
    pub fn perform(&mut self, commands: Vec<FindCommand>) {
        let mut queue: VecDeque<FindCommand> = commands.into();
        while let Some(command) = queue.pop_front() {
            let in_flight = self.held.len()
                + 1
                + queue.iter().filter(|c| matches!(c, FindCommand::Search(_))).count();
            let follow_up = match command {
                FindCommand::Search(request) => {
                    self.max_in_flight = self.max_in_flight.max(in_flight);
                    self.requests.push(request.clone());
                    match (self.rpc)(&request, &mut self.host) {
                        RpcAnswer::Reply(answer) => {
                            self.find.on_page(&request.search_id, &answer, &mut self.host)
                        }
                        RpcAnswer::Error => {
                            self.find.on_search_error(&request.search_id, &mut self.host)
                        }
                        RpcAnswer::Hold => {
                            self.held.push(request);
                            Vec::new()
                        }
                    }
                }
                FindCommand::CancelSearch { search_id } => {
                    self.cancellations.push(search_id);
                    Vec::new()
                }
                FindCommand::EnsureRowPainted { row, reveal } => {
                    self.pulled.push(row);
                    self.find.on_row_painted(reveal, true, &mut self.host)
                }
                FindCommand::ArmDebounce { at_ms } => {
                    self.armed_debounce = Some(at_ms);
                    Vec::new()
                }
                FindCommand::CancelDebounce => {
                    self.armed_debounce = None;
                    Vec::new()
                }
            };
            queue.extend(follow_up);
        }
    }
}

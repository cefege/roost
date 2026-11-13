//! Presentation-only clocks retained with the accepted transcript replica.
//! Reconciliation reads the canonical folded document, never TSP operations.
//! Origins survive remounts and unrelated frames, but not removal or transport reset.
use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::Transcript;

#[path = "presentation_timer_samples.rs"]
mod samples;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OmpTimerKind {
    Elapsed,
    ToolAge,
    AgentTool,
    AgentStats,
    AgentRetry,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OmpTimerReading {
    pub milliseconds: f64,
    pub countdown: bool,
    pub running: bool,
}

impl OmpTimerReading {
    /// Reads an `elapsed` sample that the replica does not retain (an id-less prop tree),
    /// `since_observed_ms` after the browser first saw it.
    pub fn from_elapsed_props(age: f64, stopped: Option<f64>, since_observed_ms: u64) -> Self {
        let sample = TimerSample {
            age,
            stopped,
            until: None,
            attempt: None,
        };
        TimerOrigin {
            sample,
            observed_ms: 0,
            seen: 0,
        }
        .read(since_observed_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct TimerSample {
    age: f64,
    stopped: Option<f64>,
    until: Option<f64>,
    attempt: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
struct TimerOrigin {
    sample: TimerSample,
    observed_ms: u64,
    seen: u64,
}

impl TimerOrigin {
    fn observe(&mut self, sample: TimerSample, now_ms: u64, pass: u64) {
        if self.sample != sample {
            self.sample = sample;
            self.observed_ms = now_ms;
        }
        self.seen = pass;
    }

    fn read(&self, now_ms: u64) -> OmpTimerReading {
        let sample = self.sample;
        let elapsed = sample
            .stopped
            .unwrap_or_else(|| sample.age + now_ms.saturating_sub(self.observed_ms) as f64);
        let countdown = sample.until.is_some() || sample.age < 0.0;
        let milliseconds = match sample.until {
            Some(deadline) => (elapsed - deadline).min(0.0),
            None if countdown => elapsed.min(0.0),
            None => elapsed,
        };
        OmpTimerReading {
            milliseconds,
            countdown,
            running: sample.stopped.is_none(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct NodeClocks {
    fields: BTreeMap<OmpTimerKind, TimerOrigin>,
    facts: BTreeMap<String, BTreeMap<String, TimerOrigin>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct SurfaceClocks {
    pass: u64,
    nodes: BTreeMap<String, NodeClocks>,
}

/// Local monotonic origins, not wire or persisted conversation data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OmpPresentationClocks {
    transport_id: Option<String>,
    surfaces: BTreeMap<String, SurfaceClocks>,
}

impl OmpPresentationClocks {
    pub fn timer(
        &self,
        surface: &str,
        node: &str,
        kind: OmpTimerKind,
        now_ms: u64,
    ) -> Option<OmpTimerReading> {
        Some(
            self.surfaces
                .get(surface)?
                .nodes
                .get(node)?
                .fields
                .get(&kind)?
                .read(now_ms),
        )
    }

    pub fn fact(
        &self,
        surface: &str,
        node: &str,
        item: &str,
        column: &str,
        now_ms: u64,
    ) -> Option<OmpTimerReading> {
        Some(
            self.surfaces
                .get(surface)?
                .nodes
                .get(node)?
                .facts
                .get(item)?
                .get(column)?
                .read(now_ms),
        )
    }

    pub(super) fn reconcile(&mut self, transcript: &Transcript, now_ms: u64) {
        if self.transport_id != transcript.omp_ui_transport_id {
            self.surfaces.clear();
            self.transport_id
                .clone_from(&transcript.omp_ui_transport_id);
        }
        let Some(document) = &transcript.omp_ui else {
            self.surfaces.clear();
            return;
        };
        self.surfaces
            .retain(|id, _| document.surfaces.contains_key(id));
        for (id, surface) in &document.surfaces {
            if !self.surfaces.contains_key(id) {
                self.surfaces.insert(id.clone(), SurfaceClocks::default());
            }
            let Some(clocks) = self.surfaces.get_mut(id) else {
                continue;
            };
            if let Some(next) = clocks.pass.checked_add(1) {
                clocks.pass = next;
            } else {
                clocks.nodes.clear();
                clocks.pass = 1;
            }
            clocks.scan_node(&surface.root, now_ms);
            let pass = clocks.pass;
            clocks.nodes.retain(|_, node| {
                node.fields.retain(|_, timer| timer.seen == pass);
                node.facts.retain(|_, facts| {
                    facts.retain(|_, timer| timer.seen == pass);
                    !facts.is_empty()
                });
                !node.fields.is_empty() || !node.facts.is_empty()
            });
        }
    }
}

impl SurfaceClocks {
    fn node(&mut self, id: &str) -> Option<&mut NodeClocks> {
        if !self.nodes.contains_key(id) {
            self.nodes.insert(id.to_owned(), NodeClocks::default());
        }
        self.nodes.get_mut(id)
    }

    fn record(&mut self, node: &str, kind: OmpTimerKind, sample: TimerSample, now_ms: u64) {
        let pass = self.pass;
        let Some(node) = self.node(node) else {
            return;
        };
        let timers = &mut node.fields;
        if let Some(timer) = timers.get_mut(&kind) {
            timer.observe(sample, now_ms, pass);
        } else {
            timers.insert(
                kind,
                TimerOrigin {
                    sample,
                    observed_ms: now_ms,
                    seen: pass,
                },
            );
        }
    }

    fn record_fact(&mut self, node: &str, item: &str, column: &str, age: f64, now_ms: u64) {
        let pass = self.pass;
        let Some(node) = self.node(node) else {
            return;
        };
        let rows = &mut node.facts;
        if !rows.contains_key(item) {
            rows.insert(item.to_owned(), BTreeMap::new());
        }
        let Some(facts) = rows.get_mut(item) else {
            return;
        };
        let sample = TimerSample {
            age,
            stopped: None,
            until: None,
            attempt: None,
        };
        if let Some(timer) = facts.get_mut(column) {
            timer.observe(sample, now_ms, pass);
        } else {
            facts.insert(
                column.to_owned(),
                TimerOrigin {
                    sample,
                    observed_ms: now_ms,
                    seen: pass,
                },
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "presentation_clocks_tests.rs"]
mod tests;

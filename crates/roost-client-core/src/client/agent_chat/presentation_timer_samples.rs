//! Timer-valued properties in the accepted semantic document.
//! Called only by retained presentation-clock reconciliation after canonical folding.
//! Numeric picker facts run; source-formatted strings are already stopped.
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

use super::{OmpTimerKind, SurfaceClocks, TimerSample};

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn sample(age: f64, stopped: Option<f64>) -> TimerSample {
    TimerSample {
        age,
        stopped,
        until: None,
        attempt: None,
    }
}

impl SurfaceClocks {
    pub(super) fn scan_node(&mut self, node: &OmpUiNode, now_ms: u64) {
        let active = matches!(
            node.p.get("status").and_then(Value::as_str),
            Some("running" | "pending")
        );
        match node.k.as_str() {
            "elapsed" => {
                if let Some(age) = number(node.p.get("age")) {
                    self.record(
                        &node.id,
                        OmpTimerKind::Elapsed,
                        sample(age, number(node.p.get("stopped"))),
                        now_ms,
                    );
                }
            }
            "tool" => {
                if let Some(age) = number(node.p.get("age")) {
                    self.record(
                        &node.id,
                        OmpTimerKind::ToolAge,
                        sample(age, (!active).then_some(age)),
                        now_ms,
                    );
                }
            }
            "agent" => {
                if let Some(age) = number(node.p.get("tool").and_then(|tool| tool.get("age"))) {
                    self.record(
                        &node.id,
                        OmpTimerKind::AgentTool,
                        sample(age, (!active).then_some(age)),
                        now_ms,
                    );
                }
                if let Some(stats) = node.p.get("stats") {
                    let took = number(stats.get("took"));
                    if let Some(age) = number(stats.get("age")).or(took) {
                        self.record(
                            &node.id,
                            OmpTimerKind::AgentStats,
                            sample(age, took.or_else(|| (!active).then_some(age))),
                            now_ms,
                        );
                    }
                }
                if let Some(retry) = node.p.get("retry")
                    && let Some(age) = number(retry.get("age"))
                    && let Some(delay) = number(retry.get("delay"))
                {
                    let timer = TimerSample {
                        age,
                        stopped: (!active).then_some(age),
                        until: Some(delay),
                        attempt: retry.get("attempt").and_then(Value::as_u64),
                    };
                    self.record(&node.id, OmpTimerKind::AgentRetry, timer, now_ms);
                }
            }
            "picker" => self.scan_picker(node, now_ms),
            _ => {}
        }
        for child in &node.c {
            self.scan_node(child, now_ms);
        }
    }

    fn scan_picker(&mut self, node: &OmpUiNode, now_ms: u64) {
        let Some(columns) = node.p.get("columns").and_then(Value::as_array) else {
            return;
        };
        for column in columns {
            if column.get("format").and_then(Value::as_str) != Some("elapsed") {
                continue;
            }
            let Some(column_id) = column.get("id").and_then(Value::as_str) else {
                continue;
            };
            for item in node
                .p
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(item_id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(age) = number(item.get("facts").and_then(|facts| facts.get(column_id)))
                {
                    self.record_fact(&node.id, item_id, column_id, age, now_ms);
                }
            }
        }
    }
}

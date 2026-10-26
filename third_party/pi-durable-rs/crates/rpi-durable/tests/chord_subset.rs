//! Consumed chord subset tests (FR-B): context cancellation, JSON helpers,
//! delta application and the replicated state service.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rpi_durable::chord::context::{
    BACKGROUND_CONTEXT, await_with_context, create_context_key, with_abort_signal, with_cancel,
    with_context_value, without_abort_signal,
};
use rpi_durable::chord::delta::{
    DeltaError, Op, PathSegment, apply, apply_immutable, apply_immutable_batches, diff_json,
    is_base, op_from_json,
};
use rpi_durable::chord::json::{copy_json, is_json_value};
use rpi_durable::chord::state::{
    DeliveryKind, MutableReplicatedState, ReplicatedStateSource, ReplicatedStateSourceAttachment,
    ReplicatedStateSourceFrame, ReplicatedStateSourceOptions, ReplicatedStateSourceSnapshot,
    StateListener, attach_replicated_state_source,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn context_values_are_typed_and_scoped() {
    let key = create_context_key::<u64>("probe");
    let other = create_context_key::<u64>("other");
    let parent = BACKGROUND_CONTEXT.clone();
    assert_eq!(parent.value(&key), None);
    let child = with_context_value(&key, 7u64, &parent);
    assert_eq!(child.value(&key), Some(7));
    assert_eq!(child.value(&other), None);
    assert_eq!(parent.value(&key), None);
}

#[tokio::test]
async fn with_cancel_derives_an_independent_child() {
    let (child, cancel) = with_cancel(&BACKGROUND_CONTEXT);
    assert!(!child.abort_signal().expect("signal").is_cancelled());
    let pending = await_with_context(std::future::pending::<()>(), &child);
    cancel.cancel();
    assert!(pending.await.is_err(), "cancelled waiter must reject");
    // The static background context stays uncancelled.
    assert!(BACKGROUND_CONTEXT.abort_signal().is_none());
}

#[tokio::test]
async fn without_abort_signal_isolates_parent_cancellation() {
    let (child, cancel) = with_cancel(&BACKGROUND_CONTEXT);
    let isolated = without_abort_signal(&child);
    cancel.cancel();
    assert!(child.abort_signal().expect("signal").is_cancelled());
    assert!(isolated.abort_signal().expect("signal").is_empty());
}

#[tokio::test]
async fn with_abort_signal_combines_parent_and_supplied_cancellation() {
    let (parent, parent_cancel) = with_cancel(&BACKGROUND_CONTEXT);
    let supplied = CancellationToken::new();
    let combined = with_abort_signal(supplied.clone(), &parent);
    supplied.cancel();
    assert!(combined.abort_signal().expect("signal").is_cancelled());
    let another = with_abort_signal(CancellationToken::new(), &parent);
    parent_cancel.cancel();
    assert!(another.abort_signal().expect("signal").is_cancelled());
}

#[test]
fn json_helpers_map_to_serde_json() {
    let value = serde_json::json!({ "a": [1, "two", null], "__proto__": { "polluted": false } });
    assert!(is_json_value(&value));
    let copied = copy_json(&value);
    assert_eq!(copied, value);
}

#[test]
fn delta_applies_sets_patches_moves_and_replacements() {
    let mut root = serde_json::json!({ "nested": { "value": 1 }, "rows": [] });
    let ops = vec![Op::Replace(
        serde_json::json!({ "nested": { "value": 2 }, "rows": [{ "id": 1 }] }),
    )];
    apply(&mut root, &ops).expect("replace");
    let ops = [
        Op::Set {
            path: vec![
                PathSegment::Key("nested".to_string()),
                PathSegment::Key("value".to_string()),
            ],
            value: serde_json::json!(3),
        },
        Op::Patch {
            path: vec![PathSegment::Key("rows".to_string())],
            index: 1,
            remove: 0,
            items: vec![serde_json::json!({ "id": 2 })],
        },
        Op::Move {
            path: vec![PathSegment::Key("rows".to_string())],
            permutation: vec![1, 0],
        },
    ];
    apply(&mut root, &ops).expect("apply");
    assert_eq!(
        root,
        serde_json::json!({ "nested": { "value": 3 }, "rows": [{ "id": 2 }, { "id": 1 }] })
    );
}

#[test]
fn delta_rejects_unsafe_paths_and_unknown_verbs() {
    let mut root = serde_json::json!({});
    let error = apply(
        &mut root,
        &[Op::Set {
            path: vec![PathSegment::Key("__proto__".to_string())],
            value: serde_json::json!(1),
        }],
    )
    .expect_err("reserved segment");
    assert!(matches!(error, DeltaError::UnsafePath(_)));
    let parsed = op_from_json(&serde_json::json!(["unknown"]));
    assert!(matches!(parsed, Err(DeltaError::UnknownVerb(_))));
}

#[test]
fn delta_immutable_batches_round_trip_and_diff_replays() {
    let base = serde_json::json!({ "rows": [1, 2, 3], "nested": { "count": 1 } });
    let ops = vec![Op::Set {
        path: vec![
            PathSegment::Key("nested".to_string()),
            PathSegment::Key("count".to_string()),
        ],
        value: serde_json::json!(2),
    }];
    let applied = apply_immutable(Some(&base), &ops).expect("apply");
    assert_eq!(
        applied,
        serde_json::json!({ "rows": [1, 2, 3], "nested": { "count": 2 } })
    );
    let base_ops = vec![Op::Replace(base.clone())];
    assert!(is_base(&base_ops));
    let from_none = apply_immutable_batches(None, [base_ops.as_slice()]).expect("base replay");
    assert_eq!(from_none, base);

    let next = serde_json::json!({ "rows": [9, 2], "nested": { "count": 5 }, "added": true });
    let diff = diff_json(&base, &next);
    let replayed = apply_immutable(Some(&base), &diff).expect("diff replay");
    assert_eq!(replayed, next);
}

#[test]
fn ops_round_trip_through_the_tuple_form() {
    let ops = vec![
        Op::Replace(serde_json::json!({ "a": 1 })),
        Op::Set {
            path: vec![PathSegment::Key("a".to_string())],
            value: serde_json::json!(2),
        },
        Op::Delete {
            path: vec![PathSegment::Key("a".to_string())],
        },
        Op::Append {
            path: vec![PathSegment::Key("text".to_string())],
            value: "x".to_string(),
        },
        Op::Truncate {
            path: vec![PathSegment::Key("text".to_string())],
            count: 1,
        },
        Op::Patch {
            path: vec![PathSegment::Key("rows".to_string())],
            index: 0,
            remove: 1,
            items: vec![serde_json::json!(1)],
        },
        Op::Move {
            path: vec![PathSegment::Key("rows".to_string())],
            permutation: vec![1, 0],
        },
    ];
    let encoded = serde_json::to_value(&ops).expect("encode");
    assert_eq!(encoded[0], serde_json::json!(["r", { "a": 1 }]));
    assert_eq!(encoded[1], serde_json::json!(["s", ["a"], 2]));
    let decoded: Vec<Op> = serde_json::from_value(encoded).expect("decode");
    assert_eq!(decoded, ops);
}

fn listener(values: Arc<Mutex<Vec<(DeliveryKind, serde_json::Value)>>>) -> StateListener {
    Arc::new(move |value, _context, delivery| {
        values
            .lock()
            .expect("values")
            .push((delivery.kind, value.clone()));
    })
}

#[tokio::test]
async fn mutable_state_publishes_hydration_then_updates() {
    let state = MutableReplicatedState::new(serde_json::json!({ "count": 0 }));
    let values: Arc<Mutex<Vec<(DeliveryKind, serde_json::Value)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let subscription = state.subscribe(listener(Arc::clone(&values)));
    state
        .change(&BACKGROUND_CONTEXT, |draft| {
            if let Some(object) = draft.as_object_mut() {
                object.insert("count".to_string(), serde_json::json!(1));
            }
        })
        .expect("change");
    let delivered = values.lock().expect("values").clone();
    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].0, DeliveryKind::Hydrate);
    assert_eq!(delivered[0].1, serde_json::json!({ "count": 0 }));
    assert_eq!(delivered[1].0, DeliveryKind::Update);
    assert_eq!(delivered[1].1, serde_json::json!({ "count": 1 }));
    subscription.unsubscribe();
    state
        .change(&BACKGROUND_CONTEXT, |draft| {
            if let Some(object) = draft.as_object_mut() {
                object.insert("count".to_string(), serde_json::json!(2));
            }
        })
        .expect("change after unsubscribe");
    assert_eq!(values.lock().expect("values").len(), 2);
}

struct ManualSource {
    snapshot: ReplicatedStateSourceSnapshot,
    frames: Mutex<Vec<ReplicatedStateSourceFrame>>,
}

impl ReplicatedStateSource for ManualSource {
    fn attach(&self) -> ReplicatedStateSourceAttachment {
        let frames = std::mem::take(&mut *self.frames.lock().expect("frames"));
        ReplicatedStateSourceAttachment {
            snapshot: self.snapshot.clone(),
            activate: Box::new(move |mut listener| {
                for frame in frames {
                    listener(frame);
                }
            }),
            dispose: Box::new(|| {}),
        }
    }
}

#[tokio::test]
async fn attached_state_hydrates_snapshot_and_applies_frames() {
    let source = ManualSource {
        snapshot: ReplicatedStateSourceSnapshot {
            value: serde_json::json!({ "count": 0 }),
            cursor: 0,
        },
        frames: Mutex::new(vec![ReplicatedStateSourceFrame {
            cursor: 1,
            value: serde_json::json!({ "count": 1 }),
            ops: vec![Op::Set {
                path: vec![PathSegment::Key("count".to_string())],
                value: serde_json::json!(1),
            }],
            context: BACKGROUND_CONTEXT.clone(),
        }]),
    };
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions::default());
    assert_eq!(state.value(), serde_json::json!({ "count": 1 }));
    state.dispose();
    state.dispose();
}

#[tokio::test]
async fn attached_state_reports_cursor_gaps() {
    let reported = Arc::new(Mutex::new(0usize));
    let reported_for_error = Arc::clone(&reported);
    let source = ManualSource {
        snapshot: ReplicatedStateSourceSnapshot {
            value: serde_json::json!({ "count": 0 }),
            cursor: 5,
        },
        frames: Mutex::new(vec![ReplicatedStateSourceFrame {
            cursor: 7,
            value: serde_json::json!({ "count": 1 }),
            ops: Vec::new(),
            context: BACKGROUND_CONTEXT.clone(),
        }]),
    };
    let state = attach_replicated_state_source(
        &source,
        ReplicatedStateSourceOptions {
            on_error: Some(Arc::new(move |_error| {
                *reported_for_error.lock().expect("reported") += 1;
            })),
        },
    );
    assert_eq!(*reported.lock().expect("reported"), 1);
    assert_eq!(state.value(), serde_json::json!({ "count": 0 }));
    state.dispose();
}

#[tokio::test]
async fn mutable_state_rejects_reentrant_change() {
    let state = MutableReplicatedState::new(serde_json::json!({ "count": 0 }));
    let result = state.change(&BACKGROUND_CONTEXT, |_draft| {
        let nested = state.change(&BACKGROUND_CONTEXT, |_| {});
        assert!(nested.is_err());
    });
    assert!(result.is_ok());
}

#[test]
fn subscription_counting_is_isolated() {
    let state = MutableReplicatedState::new(serde_json::json!(0));
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_for_listener = Arc::clone(&counter);
    let _subscription = state.subscribe(Arc::new(move |_value, _context, _delivery| {
        counter_for_listener.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(counter.load(Ordering::SeqCst), 1, "hydration delivery");
    state
        .replace(&BACKGROUND_CONTEXT, serde_json::json!(1))
        .expect("replace");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

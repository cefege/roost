use std::time::Duration;

use rpi_codemode::{
    CodemodeResult,
    portable::{CodemodeRun, DeclaredTool, Event, Outcome, StartOptions},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn options() -> StartOptions {
    StartOptions {
        declared_tools: vec![
            DeclaredTool {
                name: "slow".into(),
                js_name: "slow".into(),
                description: String::new(),
            },
            DeclaredTool {
                name: "fast".into(),
                js_name: "fast".into(),
                description: String::new(),
            },
        ],
        ..StartOptions::default()
    }
}

async fn start(code: &str, options: StartOptions) -> CodemodeRun {
    CodemodeRun::start(code, options)
        .await
        .expect("worker starts")
}

async fn next_call(run: &mut CodemodeRun) -> (u32, String) {
    loop {
        if let Event::Call { id, name, .. } = run.next_event().await.expect("event") {
            return (id, name);
        }
    }
}

#[tokio::test]
async fn later_issued_fast_call_wins_promise_race() {
    let mut run = start(
        "return await Promise.race([tools.slow(), tools.fast()]);",
        options(),
    )
    .await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    let (slow_id, slow_name) = next_call(&mut run).await;
    let (fast_id, fast_name) = next_call(&mut run).await;
    assert_ne!(slow_name, fast_name);
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Idle { seq: 0, issued: 2 }
    ));
    run.deliver(1, 2_000, fast_id, Outcome::Json(json!("fast")))
        .unwrap();
    if let Event::Finished {
        result: CodemodeResult::Ok {
            value: Some(value), ..
        },
        during,
    } = run.next_event().await.unwrap()
    {
        assert_eq!(value, json!("fast"));
        assert_eq!(during.unwrap().seq, 1);
    } else {
        panic!("script did not finish with the fast call");
    }
    assert_ne!(slow_id, fast_id);
}

#[tokio::test]
async fn out_of_order_delivery_waits_for_the_sequence_gap() {
    let mut run = start(
        "const values = await Promise.all([tools.slow(), tools.fast()]); return values;",
        options(),
    )
    .await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    let (slow_id, _) = next_call(&mut run).await;
    let (fast_id, _) = next_call(&mut run).await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Idle { seq: 0, issued: 2 }
    ));
    run.deliver(2, 2_000, slow_id, Outcome::Json(json!("slow")))
        .unwrap();
    run.deliver(1, 1_000, fast_id, Outcome::Json(json!("fast")))
        .unwrap();
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Idle { seq: 1, issued: 2 }
    ));
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Idle { seq: 2, issued: 2 }
    ));
    if let Event::Finished {
        result: CodemodeResult::Ok {
            value: Some(value), ..
        },
        during,
    } = run.next_event().await.unwrap()
    {
        assert_eq!(value, json!(["slow", "fast"]));
        assert_eq!(during.unwrap().seq, 2);
    } else {
        panic!("script did not finish after the gap was filled");
    }
}

async fn deterministic_sample() -> serde_json::Value {
    let mut options = StartOptions::default();
    options.started_at_ms = 1_700_000_000_000;
    options.seed = 17;
    let mut run = start("return { now: Date.now(), iso: new Date().toISOString(), offset: new Date().getTimezoneOffset(), random: Math.random() };", options).await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    loop {
        if let Event::Finished {
            result: CodemodeResult::Ok {
                value: Some(value), ..
            },
            ..
        } = run.next_event().await.unwrap()
        {
            return value;
        }
    }
}

#[tokio::test]
async fn deterministic_clock_random_and_utc_replay() {
    let first = deterministic_sample().await;
    assert_eq!(first, deterministic_sample().await);
    assert_eq!(first["now"], json!(1_700_000_000_000_u64));
    assert_eq!(first["iso"], json!("2023-11-14T22:13:20.000Z"));
    assert_eq!(first["offset"], json!(0));
}

#[tokio::test]
async fn rejects_unknown_call_and_reused_or_zero_sequence() {
    let mut run = start("return await tools.fast();", options()).await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    let (id, _) = next_call(&mut run).await;
    assert!(run.deliver(0, 10, id, Outcome::Undefined).is_err());
    assert!(run.deliver(1, 10, id + 100, Outcome::Undefined).is_err());
    run.deliver(1, 10, id, Outcome::Undefined).unwrap();
    assert!(run.deliver(1, 10, id, Outcome::Undefined).is_err());
}

#[tokio::test]
async fn timeout_and_cancellation_finish_with_terminal_errors() {
    let mut timeout = options();
    timeout.remaining_timeout = Some(Duration::from_millis(100));
    let mut run = start("console.log('before timeout'); while (true) {}", timeout).await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    assert!(matches!(run.next_event().await.unwrap(), Event::Output(_)));
    let Event::Finished {
        result:
            CodemodeResult::Err {
                error,
                output,
                calls,
            },
        during,
    } = tokio::time::timeout(Duration::from_secs(2), run.next_event())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("timed out run did not finish with an error result");
    };
    assert_eq!(error.kind, rpi_codemode::CodemodeErrorKind::Timeout);
    assert_eq!(error.message, "codemode run timed out");
    assert_eq!(output.len(), 1);
    assert!(calls.is_empty());
    assert!(during.is_none());

    let token = CancellationToken::new();
    let mut cancel_options = options();
    cancel_options.cancellation_token = token.clone();
    let mut run = start("while (true) {}", cancel_options).await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    token.cancel();
    let Event::Finished {
        result: CodemodeResult::Err { error, .. },
        during,
    } = tokio::time::timeout(Duration::from_secs(2), run.next_event())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("cancelled run did not finish with an error result");
    };
    assert_eq!(error.kind, rpi_codemode::CodemodeErrorKind::Aborted);
    assert_eq!(error.message, "codemode run aborted");
    assert!(during.is_none());
}

#[tokio::test]
async fn rejected_tool_preserves_arbitrary_error_properties() {
    let mut run = start(
        "try { await tools.slow(); } catch (error) { return { message: error.message, custom: error.custom }; }",
        options(),
    )
    .await;
    assert!(matches!(
        run.next_event().await.unwrap(),
        Event::Started { .. }
    ));
    let (id, _) = next_call(&mut run).await;
    let properties = serde_json::from_value(json!({
        "custom": { "code": "E_CUSTOM", "values": [1, 2, 3] },
        "message": "must not replace payload message"
    }))
    .unwrap();
    run.deliver(
        1,
        10,
        id,
        Outcome::Error {
            message: "original rejection".into(),
            properties: Some(properties),
        },
    )
    .unwrap();
    let Event::Finished {
        result: CodemodeResult::Ok {
            value: Some(value), ..
        },
        ..
    } = run.next_event().await.unwrap()
    else {
        panic!("script did not catch the tool rejection");
    };
    assert_eq!(
        value,
        json!({
            "message": "original rejection",
            "custom": { "code": "E_CUSTOM", "values": [1, 2, 3] }
        })
    );
}
#[tokio::test]
async fn initial_store_loads_json_values_and_finishes_with_typed_writes() {
    let mut options = StartOptions::default();
    options.initial_store = json!({
        "text": "stored string",
        "object": { "count": 2 },
        "removed": true
    });
    let mut run = start(
        "const text = load('text'); const object = load('object'); store('text', 'updated'); store('object', { count: 3 }); store('removed', undefined); store('added', 'new'); return { text, object };",
        options,
    )
    .await;
    let Event::Finished {
        result:
            CodemodeResult::Ok {
                value: Some(value),
                store_writes,
                ..
            },
        ..
    } = run.next_event().await.unwrap()
    else {
        panic!("store script did not finish successfully");
    };
    assert_eq!(
        value,
        json!({
            "text": "stored string",
            "object": { "count": 2 }
        })
    );
    assert_eq!(
        store_writes.set,
        serde_json::from_value(json!({
            "text": "updated",
            "object": { "count": 3 },
            "added": "new"
        }))
        .unwrap()
    );
    assert_eq!(store_writes.delete, ["removed"]);
}

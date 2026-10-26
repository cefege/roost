//! Smoke tests for the wasmtime/quickjs ABI (port of the upstream
//! `sandbox.test.ts` "script execution" cases @ abe508e1b).

use rpi_codemode::{CodemodeExecuteOptions, CodemodeSandbox, CodemodeSandboxOptions};

fn timeout(ms: u64) -> CodemodeExecuteOptions {
    CodemodeExecuteOptions {
        timeout_ms: Some(rpi_codemode::CodemodeTimeout::Milliseconds(ms)),
        ..Default::default()
    }
}

fn sandbox() -> CodemodeSandbox {
    CodemodeSandbox::new(CodemodeSandboxOptions {
        timeout_ms: rpi_codemode::CodemodeTimeout::Milliseconds(10_000),
        ..Default::default()
    })
    .expect("sandbox")
}

#[tokio::test]
async fn returns_the_scripts_return_value_after_a_json_round_trip() {
    let sandbox = sandbox();
    let result = sandbox
        .execute("return { a: 1, b: [true, 'x'] }", timeout(10_000))
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Ok {
            value,
            output,
            calls,
            ..
        } => {
            assert_eq!(value, Some(serde_json::json!({ "a": 1, "b": [true, "x"] })));
            assert!(output.is_empty());
            assert!(calls.is_empty());
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn supports_top_level_await() {
    let sandbox = sandbox();
    let result = sandbox
        .execute(
            "const x = await Promise.resolve(41); return x + 1",
            timeout(10_000),
        )
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Ok { value, .. } => {
            assert_eq!(value, Some(serde_json::json!(42)))
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_script_returns_null() {
    let sandbox = sandbox();
    let result = sandbox
        .execute("", timeout(10_000))
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Ok { value, .. } => assert_eq!(value, None),
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn tools_are_callable_and_recorded() {
    use std::sync::Arc;

    let sandbox = sandbox();
    sandbox
        .register_tool(rpi_codemode::CodemodeTool {
            name: "add".to_owned(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: false,
            signature: None,
            execute: Arc::new(|args, _ctx| {
                Box::pin(async move {
                    let a = args
                        .get("a")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    let b = args
                        .get("b")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    Ok(serde_json::json!({ "sum": a + b }))
                })
            }),
        })
        .expect("register");
    let result = sandbox
        .execute(
            "const first = await tools.add({ a: 1, b: 2 }); const second = await tools.add({ a: first.sum, b: 10 }); return second.sum;",
            timeout(10_000),
        )
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Ok { value, calls, .. } => {
            assert_eq!(value, Some(serde_json::json!(13)));
            let names: Vec<_> = calls
                .iter()
                .map(|call| (call.name.as_str(), call.status))
                .collect();
            assert_eq!(
                names,
                vec![
                    ("add", rpi_codemode::CodemodeCallStatus::Ok),
                    ("add", rpi_codemode::CodemodeCallStatus::Ok)
                ]
            );
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn timeout_interrupts_a_synchronous_loop() {
    let sandbox = sandbox();
    let result = sandbox
        .execute("while (true) {}", timeout(200))
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Err { error, .. } => {
            assert_eq!(
                error.kind,
                rpi_codemode::CodemodeErrorKind::Timeout,
                "{error:?}"
            );
        }
        other => panic!("expected timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn collects_text_output_and_console() {
    let sandbox = sandbox();
    let result = sandbox
        .execute(
            "console.log(\"hello\", 1, { a: 1 }); text({ json: true }); text(undefined); return null;",
            timeout(10_000),
        )
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Ok { output, .. } => {
            let texts: Vec<String> = output
                .into_iter()
                .map(|item| match item {
                    rpi_codemode::CodemodeOutputItem::Text { text, .. } => text,
                    rpi_codemode::CodemodeOutputItem::Image { .. } => panic!("unexpected image"),
                })
                .collect();
            assert_eq!(
                texts,
                vec!["hello 1 {\"a\":1}", "{\"json\":true}", "undefined"]
            );
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn store_writes_and_snapshot() {
    let sandbox = sandbox();
    let result = sandbox
        .execute(
            "const seen = load(\"counter\"); store(\"counter\", seen + 1); store(\"old\", undefined); return [seen, load(\"counter\")];",
            CodemodeExecuteOptions {
                timeout_ms: Some(rpi_codemode::CodemodeTimeout::Milliseconds(10_000)),
                store: Some(serde_json::json!({ "counter": 41, "old": "x" })),
                ..Default::default()
            },
        )
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Ok {
            value,
            store_writes,
            ..
        } => {
            assert_eq!(value, Some(serde_json::json!([41, 42])));
            assert_eq!(
                store_writes.set.get("counter"),
                Some(&serde_json::json!(42))
            );
            assert_eq!(store_writes.delete, vec!["old".to_owned()]);
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

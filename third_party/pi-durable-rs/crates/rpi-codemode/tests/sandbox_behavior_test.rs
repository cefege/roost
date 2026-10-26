//! Behavior tests ported from `packages/codemode/test/sandbox.test.ts`
//! @ abe508e1b (FR-A/FR-B surfaces not covered by `sandbox_test.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rpi_codemode::{
    CodemodeCallStatus, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult, CodemodeSandbox,
    CodemodeSandboxOptions, CodemodeTimeout, CodemodeTool, CodemodeToolContext,
};

fn sandbox_with(
    tools: Vec<CodemodeTool>,
    globals: Vec<CodemodeTool>,
    timeout_ms: u64,
) -> CodemodeSandbox {
    CodemodeSandbox::new(CodemodeSandboxOptions {
        tools,
        globals,
        timeout_ms: CodemodeTimeout::Milliseconds(timeout_ms),
        ..Default::default()
    })
    .expect("sandbox")
}

fn echo() -> CodemodeTool {
    CodemodeTool {
        name: "echo".to_owned(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: false,
        signature: None,
        execute: Arc::new(|args, _ctx| Box::pin(async move { Ok(args) })),
    }
}

fn timeout(ms: u64) -> CodemodeExecuteOptions {
    CodemodeExecuteOptions {
        timeout_ms: Some(CodemodeTimeout::Milliseconds(ms)),
        ..Default::default()
    }
}

fn ok_value(result: CodemodeResult) -> serde_json::Value {
    match result {
        CodemodeResult::Ok {
            value: Some(value), ..
        } => value,
        other => panic!("expected ok with a value, got {other:?}"),
    }
}

fn err_kind(result: CodemodeResult) -> CodemodeErrorKind {
    match result {
        CodemodeResult::Err { error, .. } => error.kind,
        other => panic!("expected error, got {other:?}"),
    }
}

#[tokio::test]
async fn image_helper_rejects_invalid_inputs_and_derives_mime_from_signature() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute(
            r#"
			const errors = [];
			for (const run of [
				() => image(""),
				() => image("https://example.com/a.png"),
				() => image("data:image/png,raw"),
				() => image({ type: "text", text: "x" }),
				() => image({ type: "image", data: "" }),
				() => image(42),
				() => image("data:image/png;base64,AAAA!"),
				() => image("data:image/png;base64,AAAAA"),
				() => image("data:image/png;base64,AA=A"),
				() => image("data:image/png;base64,"),
				() => image("data:image/png;base64,AAAA\n[Output truncated]"),
				() => image({ type: "image", data: "AAAA!", mimeType: "image/png" }),
				() => image("data:image/png;base64,AAAA"),
				() => image("data:image/png;base64,QUJD"),
				() => image("data:image/jpeg;base64,/9j/9w=="),
			]) {
				try { run(); errors.push("no error"); } catch (error) { errors.push(error.name + ": " + error.message); }
			}
			image("data:image/webp;base64,UklGRgAAAABXRUJQ");
			image("data:image/png;base64,iVBORw0K\r\nGgo=\n");
			return errors;
		"#,
            timeout(10_000),
        )
        .await
        .expect("execution");
    let (value, output) = match result {
        CodemodeResult::Ok {
            value: Some(value),
            output,
            ..
        } => (value, output),
        other => panic!("expected ok, got {other:?}"),
    };
    let errors: Vec<String> = serde_json::from_value(value).expect("errors");
    assert_eq!(
        errors[0],
        "TypeError: image expects a non-empty image URL string, an object with image_url, or a raw MCP image block"
    );
    assert_eq!(
        errors[1],
        "TypeError: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead"
    );
    assert_eq!(
        errors[2],
        "TypeError: invalid image output. Pass a base64 data URI instead"
    );
    assert_eq!(
        errors[3],
        "TypeError: image only accepts MCP image blocks, got \"text\""
    );
    assert_eq!(errors[4], "TypeError: image expected MCP image data");
    assert_eq!(
        errors[5],
        "TypeError: image expects a non-empty image URL string, an object with image_url, or a raw MCP image block"
    );
    for error in &errors[6..12] {
        assert_eq!(
            error,
            "TypeError: invalid image output. The image data is not valid base64 (truncated or corrupted?)"
        );
    }
    for error in &errors[12..] {
        assert_eq!(
            error,
            "TypeError: invalid image output. The image data is not a PNG, JPEG, GIF, or WebP image"
        );
    }
    // MIME comes from the data, not the declared type; wrapped base64 survives.
    assert_eq!(output.len(), 2);
    match (&output[0], &output[1]) {
        (
            rpi_codemode::CodemodeOutputItem::Image { mime_type, .. },
            rpi_codemode::CodemodeOutputItem::Image { data, .. },
        ) => {
            assert_eq!(mime_type, "image/webp");
            assert_eq!(data, "iVBORw0KGgo=");
        }
        other => panic!("expected images, got {other:?}"),
    }
}

#[tokio::test]
async fn guard_names_close_matches_and_in_checks_still_work() {
    let sandbox = sandbox_with(
        vec![
            echo(),
            CodemodeTool {
                name: "web-search".to_owned(),
                ..echo()
            },
        ],
        vec![],
        10_000,
    );
    async fn attempt(sandbox: &CodemodeSandbox, expression: &str) -> serde_json::Value {
        let code = format!("return {expression};");
        match sandbox
            .execute(&code, timeout(10_000))
            .await
            .expect("execution")
        {
            CodemodeResult::Ok {
                value: Some(value), ..
            } => value,
            CodemodeResult::Ok { value: None, .. } => serde_json::Value::Null,
            CodemodeResult::Err { error, .. } => serde_json::Value::String(error.message),
        }
    }
    assert_eq!(
        attempt(&sandbox, "tools.Echo").await,
        serde_json::json!(
            "tools.Echo does not exist. Did you mean tools.echo? ALL_TOOLS lists every tool; searchTools(query) finds tools by topic. Check for a member with \"Echo\" in tools."
        )
    );
    assert!(
        attempt(&sandbox, "tools.websearch")
            .await
            .as_str()
            .unwrap_or_default()
            .contains("Did you mean tools.web_search?")
    );
    assert!(
        attempt(&sandbox, "tools.nothing")
            .await
            .as_str()
            .unwrap_or_default()
            .contains("Available: echo, web_search.")
    );
    assert_eq!(
        attempt(
            &sandbox,
            "['echo' in tools, 'nothing' in tools, String(tools.toString), JSON.stringify(tools)]"
        )
        .await,
        serde_json::json!([true, false, "undefined", "{}"])
    );
}

#[tokio::test]
async fn exit_keeps_output_and_store_writes() {
    let sandbox = sandbox_with(vec![echo()], vec![], 10_000);
    let result = sandbox
        .execute(
            "text(\"before\"); store(\"k\", 1); try { exit(); } catch {} text(\"after\"); return \"unreachable\";",
            timeout(10_000),
        )
        .await
        .expect("execution");
    match result {
        CodemodeResult::Ok {
            value,
            output,
            store_writes,
            ..
        } => {
            assert_eq!(value, None);
            assert_eq!(output.len(), 1);
            assert_eq!(store_writes.set.get("k"), Some(&serde_json::json!(1)));
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn unknown_tools_are_type_errors() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute("return await tools.missing()", timeout(10_000))
        .await
        .expect("execution");
    let CodemodeResult::Err { error, .. } = result else {
        panic!("expected error");
    };
    assert_eq!(error.kind, CodemodeErrorKind::Script);
    assert_eq!(error.name.as_deref(), Some("TypeError"));
}

#[tokio::test]
async fn unawaited_calls_are_cancelled_when_the_script_returns() {
    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_for_tool = aborted.clone();
    let sandbox = sandbox_with(
        vec![CodemodeTool {
            name: "slow".to_owned(),
            execute: Arc::new(move |_args, ctx: CodemodeToolContext| {
                let aborted = aborted_for_tool.clone();
                Box::pin(async move {
                    ctx.signal.cancelled().await;
                    aborted.store(true, Ordering::SeqCst);
                    Err("aborted".to_owned())
                })
            }),
            ..echo()
        }],
        vec![],
        10_000,
    );
    let result = sandbox
        .execute("tools.slow(); return 'early'", timeout(10_000))
        .await
        .expect("execution");
    match result {
        CodemodeResult::Ok { value, calls, .. } => {
            assert_eq!(value, Some(serde_json::json!("early")));
            assert_eq!(calls[0].status, CodemodeCallStatus::Cancelled);
        }
        other => panic!("expected ok, got {other:?}"),
    }
    for _ in 0..100 {
        if aborted.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("tool did not observe cancellation");
}

#[tokio::test]
async fn parallel_executions_do_not_share_state() {
    let sandbox = Arc::new(sandbox_with(vec![], vec![], 10_000));
    let first = sandbox.clone();
    let second = sandbox.clone();
    let third = sandbox.clone();
    let (a, b, c) = tokio::join!(
        async move {
            first
                .execute(
                    "globalThis.shared = 'a'; await null; return globalThis.shared",
                    timeout(10_000),
                )
                .await
                .expect("a")
        },
        async move {
            second
                .execute(
                    "globalThis.shared = 'b'; await null; return globalThis.shared",
                    timeout(10_000),
                )
                .await
                .expect("b")
        },
        async move {
            third
                .execute("return typeof globalThis.shared", timeout(10_000))
                .await
                .expect("c")
        }
    );
    assert_eq!(ok_value(a), serde_json::json!("a"));
    assert_eq!(ok_value(b), serde_json::json!("b"));
    assert_eq!(ok_value(c), serde_json::json!("undefined"));
}

#[tokio::test]
async fn deep_recursion_is_a_catchable_range_error() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute(
            "let depth = 0; function dive() { depth++; dive(); } try { dive(); } catch (error) { return [error.name, depth > 1000]; }",
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(ok_value(result), serde_json::json!(["RangeError", true]));
}

#[tokio::test]
async fn microtask_spinning_loop_times_out() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute("while (true) await null", timeout(200))
        .await
        .expect("execution");
    assert_eq!(err_kind(result), CodemodeErrorKind::Timeout);
}

#[tokio::test]
async fn scripts_waiting_on_an_unsettleable_promise_fail() {
    let sandbox = sandbox_with(vec![echo()], vec![], 10_000);
    let result = sandbox
        .execute(
            "await tools.echo(1); await new Promise(() => {}); return 'never'",
            CodemodeExecuteOptions {
                timeout_ms: Some(CodemodeTimeout::Infinite),
                ..Default::default()
            },
        )
        .await
        .expect("execution");
    let CodemodeResult::Err { error, .. } = result else {
        panic!("expected error");
    };
    assert_eq!(error.kind, CodemodeErrorKind::Script);
    assert!(
        error.message.contains("can never settle"),
        "{}",
        error.message
    );

    // Returning while a call is still pending is not a stall.
    let returned = sandbox
        .execute(
            "tools.echo(2); return 'early'",
            CodemodeExecuteOptions {
                timeout_ms: Some(CodemodeTimeout::Infinite),
                ..Default::default()
            },
        )
        .await
        .expect("execution");
    assert_eq!(ok_value(returned), serde_json::json!("early"));
}

#[tokio::test]
async fn close_aborts_in_flight_executions_and_rejects_new_ones() {
    let (called_tx, mut called_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let sandbox = Arc::new(sandbox_with(
        vec![CodemodeTool {
            name: "hang".to_owned(),
            execute: Arc::new(move |_args, ctx: CodemodeToolContext| {
                let called_tx = called_tx.clone();
                Box::pin(async move {
                    let _ = called_tx.send(());
                    ctx.signal.cancelled().await;
                    Err("aborted".to_owned())
                })
            }),
            ..echo()
        }],
        vec![],
        10_000,
    ));
    let running = {
        let sandbox = sandbox.clone();
        tokio::spawn(async move {
            sandbox
                .execute("await tools.hang(); return 'never'", Default::default())
                .await
                .expect("execution")
        })
    };
    // Wait until the script actually issued the call, then close the sandbox.
    tokio::time::timeout(std::time::Duration::from_secs(30), called_rx.recv())
        .await
        .expect("tool invoked")
        .expect("call signal");
    sandbox.close().await;
    let result = running.await.expect("join");
    let CodemodeResult::Err { error, calls, .. } = result else {
        panic!("expected aborted");
    };
    assert_eq!(error.kind, CodemodeErrorKind::Aborted);
    assert_eq!(error.message, "Sandbox closed");
    assert_eq!(calls[0].status, CodemodeCallStatus::Cancelled);
    assert!(
        sandbox
            .execute("return 1", Default::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn globals_spread_and_namespaces() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_for_global = seen.clone();
    let sandbox = sandbox_with(
        vec![echo()],
        vec![
            CodemodeTool {
                name: "models.list".to_owned(),
                spread: true,
                execute: Arc::new(move |args, _ctx| {
                    seen_for_global.lock().unwrap().push(args);
                    Box::pin(async move { Ok(serde_json::json!("listed")) })
                }),
                ..echo()
            },
            CodemodeTool {
                name: "models.first".to_owned(),
                ..echo()
            },
        ],
        10_000,
    );
    let result = sandbox
        .execute(
            "await models.list('classifier', undefined, 3); await models.list(); try { models.extra = 1; } catch {} return [Object.keys(models), await models.first('a', 'ignored'), 'extra' in models];",
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(
        ok_value(result),
        serde_json::json!([["list", "first"], "a", false])
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![
            serde_json::json!(["classifier", null, 3]),
            serde_json::json!([])
        ]
    );
}

#[test]
fn rejects_invalid_and_reserved_global_names() {
    let global = |name: &str| CodemodeTool {
        name: name.to_owned(),
        ..echo()
    };
    for name in [
        "a.b.c",
        "a.",
        ".a",
        "tools.x",
        "store.x",
        "a.not-valid",
        "not-valid",
    ] {
        let error = CodemodeSandbox::new(CodemodeSandboxOptions {
            globals: vec![global(name)],
            ..Default::default()
        })
        .err()
        .unwrap_or_else(|| panic!("{name} must be rejected"));
        assert!(error.contains("Invalid global"), "{name}: {error}");
    }
    let error = CodemodeSandbox::new(CodemodeSandboxOptions {
        globals: vec![global("models"), global("models.list")],
        ..Default::default()
    })
    .err()
    .expect("namespace conflict");
    assert!(error.contains("conflicts with the namespace"));
}

#[tokio::test]
async fn store_rejects_invalid_keys_values_and_oversized_writes() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute(
            r#"
			const attempt = (fn) => { try { fn(); return "ok"; } catch (error) { return error.name; } };
			return [
				attempt(() => store(1, "x")),
				attempt(() => load({})),
				attempt(() => store("fn", () => 1)),
				attempt(() => store("big", "x".repeat(300 * 1024))),
				attempt(() => { for (let i = 0; i < 8; i++) store("k" + i, "x".repeat(200 * 1024)); }),
			];
		"#,
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(
        ok_value(result),
        serde_json::json!([
            "TypeError",
            "TypeError",
            "TypeError",
            "RangeError",
            "RangeError"
        ])
    );

    let result = sandbox
        .execute("store(\"img\", \"x\".repeat(300 * 1024));", timeout(10_000))
        .await
        .expect("execution");
    let CodemodeResult::Err { error, .. } = result else {
        panic!("expected error");
    };
    assert!(
        error
            .message
            .contains("store(\"img\") value has 307202 characters of JSON"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("Show images with image()"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn register_and_unregister_between_executions() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    sandbox.register_tool(echo()).expect("register");
    assert!(sandbox.register_tool(echo()).is_err());
    assert_eq!(sandbox.tools().len(), 1);
    let result = sandbox
        .execute("return await tools.echo('a')", timeout(10_000))
        .await
        .expect("execution");
    assert_eq!(ok_value(result), serde_json::json!("a"));
    assert!(sandbox.unregister_tool("echo"));
    let result = sandbox
        .execute("return 'echo' in tools", timeout(10_000))
        .await
        .expect("execution");
    assert_eq!(ok_value(result), serde_json::json!(false));
}

#[tokio::test]
async fn escape_hatches_are_absent_and_eval_stays_inside_the_vm() {
    let sandbox = sandbox_with(vec![echo()], vec![], 10_000);
    let result = sandbox
        .execute(
            r#"
			return [
				typeof process, typeof require, typeof module, typeof setTimeout, typeof fetch,
				typeof WebAssembly, typeof std, typeof os, typeof globalThis.constructor,
				eval("typeof process"),
				new Function("return typeof process")(),
				(async () => {}).constructor("return typeof setTimeout")() instanceof Promise,
			];
		"#,
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(
        ok_value(result),
        serde_json::json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "function",
            "undefined",
            "undefined",
            true
        ])
    );

    let result = sandbox
        .execute(
            "try { await import('node:fs'); return 'imported'; } catch (error) { return error.constructor.name; }",
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_ne!(ok_value(result), serde_json::json!("imported"));
}

#[tokio::test]
async fn tools_and_console_are_frozen() {
    let sandbox = sandbox_with(vec![echo()], vec![], 10_000);
    let result = sandbox
        .execute(
            "try { tools.echo = () => 'nope'; } catch {} try { tools.extra = () => 'nope'; } catch {} try { globalThis.tools = null; } catch {} return ['extra' in tools, await tools.echo('still')];",
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(ok_value(result), serde_json::json!([false, "still"]));
}

#[tokio::test]
async fn memory_limit_fails_runaway_allocations_inside_the_script() {
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
        timeout_ms: CodemodeTimeout::Milliseconds(30_000),
        memory_limit_bytes: Some(64 * 1024 * 1024),
        ..Default::default()
    })
    .expect("sandbox");
    let result = sandbox
        .execute(
            "let a = []; try { while (true) a.push('x'.repeat(1 << 20) + a.length); } catch (error) { const n = a.length; a = null; return { n, error: String(error) }; }",
            timeout(30_000),
        )
        .await
        .expect("execution");
    let value = ok_value(result);
    let error = value["error"].as_str().unwrap_or_default();
    assert!(error.contains("out of memory"), "{value}");
    assert!(value["n"].as_u64().unwrap_or(999) < 512, "{value}");
}

#[tokio::test]
async fn date_uses_the_host_timezone_like_upstream() {
    use chrono::{Offset as _, TimeZone as _};

    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute(
            "return new Date(0).getTimezoneOffset()",
            CodemodeExecuteOptions {
                timeout_ms: Some(CodemodeTimeout::Infinite),
                ..Default::default()
            },
        )
        .await
        .expect("execution");
    let observed = ok_value(result).as_i64().expect("number");
    // `getTimezoneOffset()` is minutes west of UTC; the host hook returns
    // seconds east (upstream's `timezoneOffset: "host"` default).
    let offset_seconds = chrono::Local
        .timestamp_opt(0, 0)
        .single()
        .map(|time| time.offset().fix().local_minus_utc())
        .unwrap_or(0);
    assert_eq!(observed, i64::from(-offset_seconds / 60));
}

// ---------------------------------------------------------------------------
// #10444 built-in freeze (b223082bb)
// ---------------------------------------------------------------------------

/// The prelude shares the built-ins with the script, so patching them could
/// corrupt what it sends to the host (sandbox.test.ts "ignores patches to
/// built-ins and built-in globals").
#[tokio::test]
async fn ignores_patches_to_builtins_and_builtin_globals() {
    let sandbox = sandbox_with(vec![echo()], vec![], 10_000);
    let result = sandbox
        .execute(
            r#"
			Array.prototype.toJSON = () => null;
			Object.prototype.toJSON = () => 5;
			Promise.prototype.then = () => {};
			Map.prototype.get = () => undefined;
			globalThis.JSON = { stringify: () => "x", parse: () => "x" };
			store("k", [1]);
			return [await tools.echo([2]), JSON.stringify({ a: 1 })];
		"#,
            timeout(10_000),
        )
        .await
        .expect("execution");
    match result {
        CodemodeResult::Ok {
            value,
            store_writes,
            ..
        } => {
            assert_eq!(value, Some(serde_json::json!([[2], "{\"a\":1}"])));
            assert_eq!(
                store_writes.set.get("k"),
                Some(&serde_json::json!([1])),
                "{store_writes:?}"
            );
        }
        other => panic!("expected ok, got {other:?}"),
    }
}

/// Intrinsics that are only reachable from instances are frozen too, and
/// instance overrides still work through the accessors
/// (sandbox.test.ts "freezes intrinsics that are only reachable from
/// instances" / "still lets instances override properties of frozen
/// prototypes").
#[tokio::test]
async fn freezes_intrinsics_and_still_allows_instance_overrides() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let frozen = sandbox
        .execute(
            r#"
			return [
				Object.getPrototypeOf(function* () {}).prototype,
				Object.getPrototypeOf(async function () {}),
				Object.getPrototypeOf(Int8Array).prototype,
				Object.getPrototypeOf([][Symbol.iterator]()),
				Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())),
				Object.getPrototypeOf(new Map()[Symbol.iterator]()),
				Object.getPrototypeOf(/a/[Symbol.matchAll]("")),
			].every((object) => Object.isFrozen(object));
		"#,
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(ok_value(frozen), serde_json::json!(true));

    let overrides = sandbox
        .execute(
            r#"
			const object = {};
			object.toString = () => "custom";
			function Legacy() {}
			Legacy.prototype = Object.create(Error.prototype);
			Legacy.prototype.constructor = Legacy;
			const bare = new Error();
			bare.message = "set later";
			class MyError extends Error {
				constructor(message) {
					super(message);
					this.name = "MyError";
				}
			}
			let patched = "silent";
			try { Error.prototype.name = "Patched"; } catch (error) { patched = error.constructor.name; }
			return [String(object), new Legacy().constructor === Legacy, bare.message, new MyError("x").name, Error.prototype.name, patched];
		"#,
            timeout(10_000),
        )
        .await
        .expect("execution");
    assert_eq!(
        ok_value(overrides),
        serde_json::json!(["custom", true, "set later", "MyError", "Error", "TypeError"])
    );
}

/// A script can set an error's `name` or `message` to anything; the host
/// requires string fields, so they are coerced (b223082bb).
#[tokio::test]
async fn coerces_non_string_error_fields() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute(
            "const error = new Error('x'); error.message = 42; throw error;",
            timeout(10_000),
        )
        .await
        .expect("execution");
    match result {
        CodemodeResult::Err { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Script);
            assert_eq!(error.name.as_deref(), Some("Error"));
            assert_eq!(error.message, "42");
        }
        other => panic!("expected error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// #10283 output limits (319fecb89)
// ---------------------------------------------------------------------------

/// A script whose output passes the limits fails, even if it catches the
/// error (sandbox.test.ts "fails a script whose output passes the limits,
/// even if it catches the error").
#[tokio::test]
async fn output_past_the_char_limit_fails_the_script() {
    let sandbox = sandbox_with(vec![], vec![], 60_000);
    for print in [
        "text(s)",
        "console.log(s)",
        "image(\"data:image/png;base64,\" + p)",
    ] {
        let result = sandbox
            .execute(
                &format!(
                    r#"
					const s = "x".repeat(1 << 20);
					const p = "iVBORw0KGgoA" + "A".repeat(1 << 20);
					try {{ {print}; }} catch {{}}
					for (;;) {{ try {{ {print}; }} catch {{}} }}
				"#
                ),
                timeout(60_000),
            )
            .await
            .expect("execution");
        match result {
            CodemodeResult::Err { error, output, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Script, "{error:?}");
                assert_eq!(error.name.as_deref(), Some("RangeError"), "{error:?}");
                assert!(
                    error.message.contains("script output exceeded"),
                    "{}",
                    error.message
                );
                let chars: usize = output
                    .iter()
                    .map(|item| match item {
                        rpi_codemode::CodemodeOutputItem::Text { text, .. } => text.chars().count(),
                        rpi_codemode::CodemodeOutputItem::Image { data, .. } => {
                            data.chars().count()
                        }
                    })
                    .sum();
                assert!(
                    chars <= rpi_codemode::MAX_OUTPUT_CHARS,
                    "output limit not enforced: {chars} chars"
                );
                assert!(
                    chars > rpi_codemode::MAX_OUTPUT_CHARS - (2 << 20),
                    "output stopped early: {chars} chars"
                );
            }
            other => panic!("expected the output limit to fail the script, got {other:?}"),
        }
    }
}

/// The item limit covers loops that print empty strings
/// (sandbox.test.ts same case).
#[tokio::test]
async fn output_past_the_item_limit_fails_the_script() {
    let sandbox = sandbox_with(vec![], vec![], 60_000);
    let result = sandbox
        .execute("for (;;) text(\"\");", timeout(60_000))
        .await
        .expect("execution");
    match result {
        CodemodeResult::Err { error, output, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Script);
            assert_eq!(error.name.as_deref(), Some("RangeError"));
            assert_eq!(output.len(), rpi_codemode::MAX_OUTPUT_ITEMS);
        }
        other => panic!("expected the item limit to fail the script, got {other:?}"),
    }
}

/// `console.*` items are marked so the coding agent can group them instead
/// of mixing them into `text()` output (eb326d265).
#[tokio::test]
async fn console_output_is_marked() {
    let sandbox = sandbox_with(vec![], vec![], 10_000);
    let result = sandbox
        .execute(
            "text(\"plain\");\nconsole.log(\"logged\");\nconsole.error(\"bad\");",
            timeout(10_000),
        )
        .await
        .expect("execution");
    let CodemodeResult::Ok { output, .. } = result else {
        panic!("expected ok");
    };
    let items: Vec<(String, bool)> = output
        .iter()
        .map(|item| match item {
            rpi_codemode::CodemodeOutputItem::Text { text, console } => (text.clone(), *console),
            other => panic!("expected text items, got {other:?}"),
        })
        .collect();
    assert_eq!(
        items,
        vec![
            ("plain".to_owned(), false),
            ("logged".to_owned(), true),
            ("bad".to_owned(), true),
        ]
    );
}

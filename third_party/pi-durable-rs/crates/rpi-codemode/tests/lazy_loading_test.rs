//! FR-D R1 lazy-loading assertion: the sandbox engine compiles only on the
//! first script execution (single test per binary so the process-global
//! `OnceLock` is observable).

use rpi_codemode::{
    CodemodeExecuteOptions, CodemodeSandbox, CodemodeSandboxOptions, CodemodeTimeout,
};

#[tokio::test]
async fn engine_is_not_loaded_before_the_first_script() {
    assert!(
        !rpi_codemode::module_is_compiled(),
        "module must not be compiled before the first execution"
    );
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
        timeout_ms: CodemodeTimeout::Milliseconds(10_000),
        ..Default::default()
    })
    .expect("sandbox");
    let result = sandbox
        .execute(
            "return 1",
            CodemodeExecuteOptions {
                timeout_ms: Some(CodemodeTimeout::Milliseconds(10_000)),
                ..Default::default()
            },
        )
        .await
        .expect("execution");
    assert!(matches!(result, rpi_codemode::CodemodeResult::Ok { .. }));
    assert!(
        rpi_codemode::module_is_compiled(),
        "module must be compiled after the first execution"
    );

    // Engine failures degrade to a sandbox error result, never a crash.
    let failing = CodemodeSandbox::new(CodemodeSandboxOptions {
        timeout_ms: CodemodeTimeout::Milliseconds(10_000),
        ..Default::default()
    })
    .expect("sandbox");
    let failing = {
        let mut failing = failing;
        failing.fail_engine = Some("Failed to load QuickJS: injected".to_owned());
        failing
    };
    let result = failing
        .execute("return 1", Default::default())
        .await
        .expect("execution");
    match result {
        rpi_codemode::CodemodeResult::Err { error, .. } => {
            assert_eq!(error.kind, rpi_codemode::CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "Failed to load QuickJS: injected");
        }
        other => panic!("expected sandbox error, got {other:?}"),
    }
}

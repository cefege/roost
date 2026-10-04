//! The activation, run in a child process, and the boot events it reported.
//!
//! `runtime::serve_until` spends the authorisation through `ProcessEnv`, and
//! the process environment is the only channel to it. Mutating this process's
//! environment is `unsafe` in edition 2024 and the workspace forbids `unsafe`
//! outright, so the activation runs in a child: the parent hands the child its
//! paths through `Command::env`, which is the safe way to give a process an
//! environment. The child is this same test binary running one named test, so
//! the boot, the keeper handshake and the spend are the real ones.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use roost_host::HostPlatform;
use roost_worker::runtime::door_serve::ENV_DOOR_BIND;

use super::{Definition, boot, platform, serve_once};

/// Set only on a child this module spawned; a harness run of this test leaves
/// it unset and the test does nothing.
const CHILD_ENV: &str = "ROOST_V3_TEST_RETIRE_CHILD";

/// The scratch root the child resolves its own boot configuration from.
const ROOT_ENV: &str = "ROOST_V3_TEST_RETIRE_ROOT";

/// The libtest name of [`runs_one_activation`] in whichever module path this
/// copy was compiled under.
fn child_test_name() -> String {
    let module = module_path!();
    let within_crate = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{within_crate}::runs_one_activation")
}

/// The two line kinds the child prints, so nothing else on its stdout is read.
const REPORT_TAG: &str = "roost-retire-child";
const EVENT_TAG: &str = "roost-retire-event";

/// One activation, run in a child, and what it reported.
///
/// The events are the ones `tracing` emitted during that activation. They
/// arrive over the child's stdout rather than a shared subscriber because the
/// subscriber that records them can only be installed on the thread the events
/// are emitted from, and in a child that thread is not the test's.
pub struct Activation {
    events: Vec<BootEvent>,
    failure: Option<String>,
}

impl Activation {
    /// Whether the activation ended the way a healthy boot ends, carrying the
    /// reason it did not when it did not.
    pub fn outcome(&self) -> anyhow::Result<()> {
        match &self.failure {
            Some(reason) => Err(anyhow::anyhow!("the worker refused to boot: {reason}")),
            None => Ok(()),
        }
    }

    /// Every event of the activation, in the order it emitted them.
    pub fn events(&self) -> &[BootEvent] {
        &self.events
    }
}

/// Run one activation in a child process and bring back what it reported.
///
/// The parent keeps the keeper socket open across this call: the child admits
/// the keeper this process is listening on, which is the admission the spend's
/// position is measured against.
pub fn serve_in_child(root: &Path, host: HostPlatform, definition: &Definition) -> Activation {
    let mut command = Command::new(std::env::current_exe().expect("a running test has a binary"));
    command
        .arg("--exact")
        .arg(child_test_name())
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(ROOT_ENV, root)
        // `env_key` takes no `self`: the variable NAME does not depend on which
        // definition was written, only on the platform. Called as an associated
        // function for that reason, not as a method on the definition.
        .env(Definition::env_key(host), definition.path())
        // An ephemeral loopback port for the child's door. The default is one
        // fixed port, and the tests in this binary run their children in
        // parallel: every child after the first would refuse to boot on
        // `Address already in use`, which reads as a spend defect and is not.
        .env(ENV_DOOR_BIND, "127.0.0.1:0");
    let output = command.output().expect("the activation child started");
    parse(&output)
}

/// Read back what the child printed, and what it did not manage to say.
fn parse(output: &std::process::Output) -> Activation {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut events = Vec::new();
    let mut report = None;
    for line in stdout.lines() {
        let Some((tag, rest)) = line.split_once('\t') else {
            continue;
        };
        if tag == EVENT_TAG {
            let (removed, name) = rest
                .split_once('\t')
                .expect("an event line carries the spend and the message");
            events.push(BootEvent {
                name: name.to_owned(),
                removed: match removed {
                    "true" => Some(true),
                    "false" => Some(false),
                    absent => {
                        if absent != "-" {
                            panic!(
                                "an event reported a spend that is neither present nor absent: \
                                 {absent}"
                            );
                        }
                        None
                    }
                },
            });
        } else if tag == REPORT_TAG {
            report = Some(rest.to_owned());
        }
    }
    let failure = match &report {
        Some(reason) if reason == "ok" => None,
        Some(reason) => Some(reason.to_string()),
        // The report is printed last, so a child that never reached it did not
        // finish an activation, whatever its exit status says.
        None => Some(format!(
            "the activation child reported nothing, exited with {}, and said: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    };
    Activation { events, failure }
}

/// THE CHILD. One activation, and the events it emitted, on stdout.
///
/// It is a harness test like any other, and returns there: the marker is set
/// only for a child spawned by [`serve_in_child`].
#[tokio::test]
async fn runs_one_activation() {
    if std::env::var(CHILD_ENV).is_err() {
        return;
    }
    let root = std::env::var(ROOT_ENV).expect("a child is told its scratch root");
    let (captured, _capture) = spend_events();
    let boot = boot(Path::new(&root), platform());
    let outcome = serve_once(boot).await;
    for event in captured() {
        let removed = match event.removed {
            Some(value) => value.to_string(),
            None => "-".to_string(),
        };
        println!("{EVENT_TAG}\t{removed}\t{}", event.name);
    }
    match outcome {
        Ok(()) => println!("{REPORT_TAG}\tok"),
        Err(error) => println!("{REPORT_TAG}\terr {error}"),
    }
}

/// One boot event, in the order the activation emitted it.
///
/// Ordered rather than counted, because the property this file is about is
/// WHERE the spend lands and not only that it happened: a spend above keeper
/// admission destroys an authorisation the admission may not consume, and a
/// spend that is simply absent is loud while a spend in the wrong place is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootEvent {
    /// The event's message, which is the human-readable name of the transition.
    pub name: String,
    /// `removed_from_service_definition`, on the spend event and nowhere else.
    pub removed: Option<bool>,
}

/// Just the spends, in order, out of a whole activation's events.
pub fn spends(events: &[BootEvent]) -> Vec<bool> {
    events.iter().filter_map(|event| event.removed).collect()
}

/// Where in the activation an event with this message was emitted.
pub fn position_of(events: &[BootEvent], name: &str) -> Option<usize> {
    events.iter().position(|event| event.name == name)
}

/// Record every event on the current thread, and hand back the reader AND the
/// guard that keeps the subscriber installed. Dropping the guard uninstalls it
/// mid-test.
///
/// Thread-local on purpose: it takes precedence over whatever global
/// subscriber `install_observability` installs inside `serve_until`, and a
/// global capture would also swallow the events of whatever else this test
/// binary is doing in parallel.
pub fn spend_events() -> (
    impl Fn() -> Vec<BootEvent>,
    tracing::subscriber::DefaultGuard,
) {
    let recorded: Arc<Mutex<Vec<BootEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    let guard = tracing::subscriber::set_default(SpendCapture(sink));
    let read = move || recorded.lock().expect("the capture lock").clone();
    (read, guard)
}

/// A subscriber that keeps every event this activation emits, in order.
struct SpendCapture(Arc<Mutex<Vec<BootEvent>>>);

impl tracing::Subscriber for SpendCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = BootEventFields {
            message: String::new(),
            removed: None,
        };
        event.record(&mut visitor);
        self.0.lock().expect("the capture lock").push(BootEvent {
            name: visitor.message,
            removed: visitor.removed,
        });
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Reads the two fields this test is about, and ignores every other one.
///
/// `message` rather than `metadata().name()`: the name a `tracing` macro
/// generates is the file and line it was written at, which moves the moment an
/// unrelated line is added above it, and a test keyed on that fails for a
/// reason that has nothing to do with the property.
struct BootEventFields {
    message: String,
    removed: Option<bool>,
}

impl tracing::field::Visit for BootEventFields {
    /// The message arrives HERE and not through `record_str`: the `tracing`
    /// macros record `message` as `format_args!`, whose `Value` implementation
    /// hands a visitor the arguments themselves, and whose `Debug` is its
    /// `Display` — so `{:?}` of it is the bare message, not a quoted one. A
    /// `&str` field like `key` takes `record_str` instead, and is not the
    /// message, so the two never race for the same value.
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "removed_from_service_definition" {
            self.removed = Some(value);
        }
    }
}

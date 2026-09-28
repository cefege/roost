//! One-shot terminal-find handoff tests for warm and cold pane mounts: latest
//! pending semantics, identity-safe unregistration, preferred current-epoch
//! metadata forwarding, and credential-bound registry reset. Test names are
//! v2's, from `apps/web/tests/renderer/terminalFindIntent.test.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use roost_web_terminal::find::intent::{
    FindIntentRegistry, FindIntentSink, TerminalFindIntentOptions,
};
use roost_web_terminal::find::{FindQueryOptions, PreferredMatch};

#[derive(Default)]
struct Record {
    opened: usize,
    queries: Vec<(String, FindQueryOptions)>,
}

/// v2's `fakeFind()`: a mounted pane that records what it was asked.
#[derive(Clone, Default)]
struct FakeFind(Rc<RefCell<Record>>);

impl FakeFind {
    fn sink(&self) -> Box<dyn FindIntentSink> {
        Box::new(self.clone())
    }
    fn opened(&self) -> usize {
        self.0.borrow().opened
    }
    fn queries(&self) -> Vec<(String, FindQueryOptions)> {
        self.0.borrow().queries.clone()
    }
}

impl FindIntentSink for FakeFind {
    fn open_find(&mut self) {
        self.0.borrow_mut().opened += 1;
    }
    fn set_query(&mut self, query: &str, options: FindQueryOptions) {
        self.0
            .borrow_mut()
            .queries
            .push((query.to_string(), options));
    }
}

fn case_sensitive() -> TerminalFindIntentOptions {
    TerminalFindIntentOptions {
        case_sensitive: Some(true),
        preferred_global_match: None,
    }
}

#[test]
fn a_warm_pane_consumes_the_literal_intent_immediately() {
    let mut registry = FindIntentRegistry::new();
    let mounted = FakeFind::default();
    let registration = registry.register("session-a", mounted.sink());
    let preferred = PreferredMatch {
        grid_epoch: "grid-a:0".to_string(),
        row: 41,
        col: 7,
    };
    registry.request(
        "session-a",
        "Needle.*",
        TerminalFindIntentOptions {
            case_sensitive: Some(true),
            preferred_global_match: Some(preferred.clone()),
        },
    );
    assert_eq!(mounted.opened(), 1);
    let options = FindQueryOptions {
        literal: true,
        case_sensitive: Some(true),
        preferred_match: Some(preferred),
    };
    assert_eq!(mounted.queries(), vec![("Needle.*".to_string(), options)]);
    registry.unregister("session-a", registration);
}

#[test]
fn a_cold_pane_consumes_only_the_latest_pending_intent_on_mount() {
    let mut registry = FindIntentRegistry::new();
    registry.request(
        "session-cold",
        "older",
        TerminalFindIntentOptions::default(),
    );
    registry.request("session-cold", "newer", case_sensitive());
    let mounted = FakeFind::default();
    registry.register("session-cold", mounted.sink());
    assert_eq!(mounted.opened(), 1);
    let options = FindQueryOptions {
        literal: true,
        case_sensitive: Some(true),
        preferred_match: None,
    };
    assert_eq!(mounted.queries(), vec![("newer".to_string(), options)]);
}

#[test]
fn an_older_disposer_cannot_unregister_its_replacement() {
    let mut registry = FindIntentRegistry::new();
    let first = FakeFind::default();
    let second = FakeFind::default();
    let unregister_first = registry.register("session-a", first.sink());
    let unregister_second = registry.register("session-a", second.sink());
    registry.unregister("session-a", unregister_first);
    registry.request(
        "session-a",
        "replacement",
        TerminalFindIntentOptions::default(),
    );
    assert_eq!(first.opened(), 0);
    assert_eq!(second.opened(), 1);
    assert_eq!(second.queries()[0].0, "replacement");
    registry.unregister("session-a", unregister_second);
}

#[test]
fn an_auth_boundary_reset_clears_mounted_callbacks_and_cold_intents() {
    let mut registry = FindIntentRegistry::new();
    let retired = FakeFind::default();
    registry.register("mounted-old", retired.sink());
    registry.request(
        "cold-old",
        "retired needle",
        TerminalFindIntentOptions::default(),
    );

    registry.reset();
    registry.request(
        "mounted-old",
        "next needle",
        TerminalFindIntentOptions::default(),
    );
    assert_eq!(retired.opened(), 0);

    let cold_replacement = FakeFind::default();
    registry.register("cold-old", cold_replacement.sink());
    assert_eq!(cold_replacement.opened(), 0);

    let mounted_replacement = FakeFind::default();
    registry.register("mounted-old", mounted_replacement.sink());
    assert_eq!(mounted_replacement.queries()[0].0, "next needle");
}

//! Records every `tracing` event the pager emits on this test's thread, so a
//! suite can assert v2's `diag()` lines (event name plus flat fields) exactly.
//! Thread-local on purpose: each test owns its pager and its capture.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::subscriber::DefaultGuard;

/// One captured event: the message is the diag event name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEvent {
    pub message: String,
    pub fields: BTreeMap<String, String>,
}

/// The expected field map of one diag line.
pub fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// The installed capture; dropping it uninstalls the subscriber.
pub struct Capture {
    recorded: Arc<Mutex<Vec<CapturedEvent>>>,
    _guard: DefaultGuard,
}

impl Capture {
    pub fn install() -> Self {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let guard = tracing::subscriber::set_default(Recorder(Arc::clone(&recorded)));
        // A callsite another test already registered keeps its cached interest
        // until it is re-decided against the subscriber now in scope.
        tracing::callsite::rebuild_interest_cache();
        Self {
            recorded,
            _guard: guard,
        }
    }

    pub fn events(&self) -> Vec<CapturedEvent> {
        self.recorded.lock().expect("the capture lock").clone()
    }
}

struct Recorder(Arc<Mutex<Vec<CapturedEvent>>>);

impl tracing::Subscriber for Recorder {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let mut fields = visitor.0;
        let message = fields.remove("message").unwrap_or_default();
        self.0
            .lock()
            .expect("the capture lock")
            .push(CapturedEvent { message, fields });
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

#[derive(Default)]
struct FieldVisitor(BTreeMap<String, String>);

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

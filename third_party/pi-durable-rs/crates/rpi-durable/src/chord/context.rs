//! Immutable invocation-scoped context values and cancellation
//! (port of `chord/src/context/index.ts` @ `abe508e1b`).
//!
//! Upstream carries an `AbortSignal` behind a `ContextKey`; rpi maps that to
//! [`tokio_util::sync::CancellationToken`], the workspace-wide cancellation
//! primitive (V17-13 §1 material, `rpi-agent/src/agent_loop.rs` precedent).

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

static NEXT_CONTEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// Typed identity for one value carried by a [`Context`].
pub struct ContextKey<T> {
    id: u64,
    description: &'static str,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> ContextKey<T> {
    /// Create a unique key; two keys are never interchangeable even for the
    /// same value type (upstream creates a fresh symbol per call).
    pub fn new(description: &'static str) -> Self {
        Self {
            id: NEXT_CONTEXT_KEY.fetch_add(1, Ordering::Relaxed),
            description,
            _marker: std::marker::PhantomData,
        }
    }

    /// Stable key identity (used by [`Context::value`]).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Description shown in `Context` debug output.
    pub fn description(&self) -> &'static str {
        self.description
    }
}

impl<T> Clone for ContextKey<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            description: self.description,
            _marker: std::marker::PhantomData,
        }
    }
}

enum ContextInner {
    Empty {
        name: &'static str,
    },
    Value {
        parent: Context,
        key: u64,
        value: Arc<dyn Any + Send + Sync>,
    },
}

/// Immutable tree of typed key/value pairs plus an optional cancellation signal.
#[derive(Clone)]
pub struct Context {
    inner: Arc<ContextInner>,
}

impl Context {
    fn empty(name: &'static str) -> Self {
        Self {
            inner: Arc::new(ContextInner::Empty { name }),
        }
    }

    fn with_value_boxed(parent: Context, key: u64, value: Arc<dyn Any + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(ContextInner::Value { parent, key, value }),
        }
    }

    /// Read a value recorded by [`with_context_value`].
    pub fn value<T: Clone + Send + Sync + 'static>(&self, key: &ContextKey<T>) -> Option<T> {
        let mut current = self.clone();
        loop {
            match current.inner.as_ref() {
                ContextInner::Empty { .. } => return None,
                ContextInner::Value {
                    parent,
                    key: stored,
                    value,
                } => {
                    if *stored == key.id {
                        return value.downcast_ref::<T>().cloned();
                    }
                    current = parent.clone();
                }
            }
        }
    }

    /// The cancellation signal of this context, when one is installed.
    ///
    /// A `without_abort_signal` node stores an explicit empty signal and stops
    /// the walk, so parent cancellation does not leak through.
    pub fn abort_signal(&self) -> Option<AbortSignal> {
        let mut current = self.clone();
        loop {
            match current.inner.as_ref() {
                ContextInner::Empty { .. } => return None,
                ContextInner::Value { parent, key, value } => {
                    if *key == ABORT_SIGNAL_CONTEXT_KEY.id {
                        return value.downcast_ref::<AbortSignal>().cloned();
                    }
                    current = parent.clone();
                }
            }
        }
    }

    /// Human-readable chain description (upstream `toString`).
    pub fn name(&self) -> String {
        match self.inner.as_ref() {
            ContextInner::Empty { name } => (*name).to_string(),
            ContextInner::Value {
                parent,
                key: _,
                value: _,
            } => format!("{}.WithValue", parent.name()),
        }
    }
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Static background context: no values, no cancellation.
pub static BACKGROUND_CONTEXT: LazyLock<Context> =
    LazyLock::new(|| Context::empty("[Context BACKGROUND_CONTEXT]"));
/// Static TODO context: no values, no cancellation.
pub static TODO_CONTEXT: LazyLock<Context> =
    LazyLock::new(|| Context::empty("[Context TODO_CONTEXT]"));

static ABORT_SIGNAL_CONTEXT_KEY: LazyLock<ContextKey<AbortSignal>> =
    LazyLock::new(|| ContextKey::new("chord.abortSignal"));

/// Combined cancellation state of a context: every token that can cancel it.
///
/// Upstream stores one `AbortSignal` (with `AbortSignal.any` composition); rpi
/// keeps the token list so composition stays synchronous (V17-13 §7.3).
#[derive(Clone, Default)]
pub struct AbortSignal {
    tokens: Vec<CancellationToken>,
}

impl AbortSignal {
    /// An empty signal that never cancels.
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether any token is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.tokens.iter().any(CancellationToken::is_cancelled)
    }

    /// Whether the signal carries no cancellation source.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Wait until any token cancels.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        if self.tokens.is_empty() {
            std::future::pending::<()>().await;
            return;
        }
        let futures: Vec<_> = self
            .tokens
            .iter()
            .map(|token| Box::pin(token.cancelled()))
            .collect();
        futures::future::select_all(futures).await;
    }

    /// A single token cancelled when any source cancels; used by callers that
    /// need a `CancellationToken`.
    pub fn token(&self) -> CancellationToken {
        if self.tokens.len() == 1 {
            return self.tokens[0].clone();
        }
        let combined = CancellationToken::new();
        for token in &self.tokens {
            let combined = combined.clone();
            let token = token.clone();
            if token.is_cancelled() {
                combined.cancel();
                return combined;
            }
            tokio::spawn(async move {
                token.cancelled().await;
                combined.cancel();
            });
        }
        combined
    }

    pub(crate) fn from_tokens(tokens: Vec<CancellationToken>) -> Self {
        Self { tokens }
    }

    pub(crate) fn tokens(&self) -> &[CancellationToken] {
        &self.tokens
    }
}

/// Create a typed context key. Equivalent to `createContextKey`.
pub fn create_context_key<T>(description: &'static str) -> ContextKey<T> {
    ContextKey::new(description)
}

/// Derive a context containing one additional or replaced value.
pub fn with_context_value<T: Clone + Send + Sync + 'static>(
    key: &ContextKey<T>,
    value: T,
    parent: &Context,
) -> Context {
    Context::with_value_boxed(parent.clone(), key.id, Arc::new(value))
}

/// Derive a context cancelled by either the parent signal or `signal`.
///
/// The parent context remains unchanged.
pub fn with_abort_signal(signal: CancellationToken, context: &Context) -> Context {
    let mut tokens = context
        .abort_signal()
        .map(|signal| signal.tokens().to_vec())
        .unwrap_or_default();
    tokens.push(signal);
    with_context_value(
        &ABORT_SIGNAL_CONTEXT_KEY,
        AbortSignal::from_tokens(tokens),
        context,
    )
}

/// Derive a context retaining all values except caller cancellation (mandatory
/// cleanup only).
pub fn without_abort_signal(context: &Context) -> Context {
    with_context_value(&ABORT_SIGNAL_CONTEXT_KEY, AbortSignal::none(), context)
}

/// Handle returned by [`with_cancel`]; cancels only the derived context.
#[derive(Clone)]
pub struct CancelHandle {
    token: CancellationToken,
}

impl CancelHandle {
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether this child context has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// Derive an independently cancellable child context.
pub fn with_cancel(context: &Context) -> (Context, CancelHandle) {
    let token = CancellationToken::new();
    let mut tokens = context
        .abort_signal()
        .map(|signal| signal.tokens().to_vec())
        .unwrap_or_default();
    tokens.push(token.clone());
    let child = with_context_value(
        &ABORT_SIGNAL_CONTEXT_KEY,
        AbortSignal::from_tokens(tokens),
        context,
    );
    (child, CancelHandle { token })
}

/// Cancellation error from [`await_with_context`].
#[derive(Debug, Clone, Error)]
#[error("The operation was aborted")]
pub struct Aborted;

impl Aborted {
    /// The cancellation signal, for callers that need to distinguish sources.
    pub fn is_aborted() -> Self {
        Self
    }
}

/// Observe a future until it settles or the context is cancelled.
///
/// Cancellation rejects only this waiter; it does not cancel the underlying
/// future (upstream `awaitWithContext`).
pub async fn await_with_context<F: Future>(
    future: F,
    context: &Context,
) -> Result<F::Output, Aborted> {
    let Some(signal) = context.abort_signal() else {
        return Ok(future.await);
    };
    if signal.is_cancelled() {
        return Err(Aborted);
    }
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(Aborted),
        output = future => Ok(output),
    }
}

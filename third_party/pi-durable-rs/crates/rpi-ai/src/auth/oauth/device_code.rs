//! Port of `packages/ai/src/auth/oauth/device-code.ts` @ pi 0.82.1 (2efa728).
//!
//! RFC 8628 device-code polling framework: default 5s interval, `slow_down`
//! prefers the server-provided interval (otherwise +5s), 1s floor, cancel via
//! `CancellationToken` with an interruptible sleep. User-facing messages are
//! verbatim below.
//!
//! Intentional differences: `Date.now()` becomes a monotonic millisecond
//! clock behind [`DeviceFlowClock`] (injectable so tests can drive time
//! deterministically, mirroring the upstream fake-timer tests); thrown
//! `Error`s become [`ModelsError`] with the same message text.

use std::future::Future;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::super::resolve::{ModelsError, ModelsErrorCode};
use crate::auth::types::BoxFutureSend;

/// `CANCEL_MESSAGE`.
pub const CANCEL_MESSAGE: &str = "Login cancelled";
/// `TIMEOUT_MESSAGE`.
pub const TIMEOUT_MESSAGE: &str = "Device flow timed out";
/// `SLOW_DOWN_TIMEOUT_MESSAGE` (verbatim; WSL/VM clock drift hint).
pub const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
/// `MINIMUM_INTERVAL_MS`.
const MINIMUM_INTERVAL_MS: u64 = 1000;
/// `DEFAULT_POLL_INTERVAL_SECONDS` — RFC 8628 section 3.2: if the
/// authorization server omits `interval`, the client must use 5 seconds.
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
/// `SLOW_DOWN_INTERVAL_INCREMENT_MS` — RFC 8628 section 3.5: `slow_down`
/// means the polling interval must increase by 5 seconds.
const SLOW_DOWN_INTERVAL_INCREMENT_MS: u64 = 5000;

/// `OAuthDeviceCodePollResult<T>`.
pub enum DeviceCodePollResult<T> {
    Pending,
    SlowDown { interval_seconds: Option<f64> },
    Failed { message: String },
    Complete { value: T },
}

/// `OAuthDeviceCodePollOptions<T>`.
pub struct DeviceCodePollOptions<F> {
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: Option<f64>,
    pub wait_before_first_poll: bool,
    pub poll: F,
    pub signal: Option<CancellationToken>,
}

/// Clock/sleep seam — production uses [`TokioClock`]; tests inject a manual
/// clock (upstream tests reach the same coverage via vitest fake timers).
pub trait DeviceFlowClock: Send + Sync {
    fn now_ms(&self) -> u64;
    /// `abortableSleep`: resolves after `ms`, or errs with [`CANCEL_MESSAGE`]
    /// when `signal` is cancelled first.
    fn sleep(
        &self,
        ms: u64,
        signal: Option<CancellationToken>,
    ) -> BoxFutureSend<'_, Result<(), ModelsError>>;
}

/// Production clock: monotonic `Instant` + `tokio::time::sleep`.
pub struct TokioClock;

impl DeviceFlowClock for TokioClock {
    fn now_ms(&self) -> u64 {
        static EPOCH: std::sync::OnceLock<tokio::time::Instant> = std::sync::OnceLock::new();
        EPOCH
            .get_or_init(tokio::time::Instant::now)
            .elapsed()
            .as_millis() as u64
    }

    fn sleep(
        &self,
        ms: u64,
        signal: Option<CancellationToken>,
    ) -> BoxFutureSend<'_, Result<(), ModelsError>> {
        Box::pin(async move {
            let sleep = tokio::time::sleep(std::time::Duration::from_millis(ms));
            match signal {
                Some(token) => {
                    if token.is_cancelled() {
                        return Err(cancelled());
                    }
                    tokio::select! {
                        _ = sleep => Ok(()),
                        _ = token.cancelled() => Err(cancelled()),
                    }
                }
                None => {
                    sleep.await;
                    Ok(())
                }
            }
        })
    }
}

fn cancelled() -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, CANCEL_MESSAGE)
}

fn error(message: &str) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message)
}

/// `pollOAuthDeviceCodeFlow` — see module docs.
pub async fn poll_oauth_device_code_flow<T, F, Fut>(
    options: DeviceCodePollOptions<F>,
) -> Result<T, ModelsError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = DeviceCodePollResult<T>>,
{
    poll_oauth_device_code_flow_with_clock(options, Arc::new(TokioClock)).await
}

/// Clock-injectable variant (test seam; upstream uses fake timers instead).
pub async fn poll_oauth_device_code_flow_with_clock<T, F, Fut>(
    options: DeviceCodePollOptions<F>,
    clock: Arc<dyn DeviceFlowClock>,
) -> Result<T, ModelsError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = DeviceCodePollResult<T>>,
{
    let DeviceCodePollOptions {
        interval_seconds,
        expires_in_seconds,
        wait_before_first_poll,
        mut poll,
        signal,
    } = options;

    let deadline = match expires_in_seconds {
        Some(expires) => clock.now_ms() as f64 + expires * 1000.0,
        None => f64::INFINITY,
    };
    let mut interval_ms = MINIMUM_INTERVAL_MS
        .max((interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0).floor() as u64);

    let mut slow_down_responses = 0u32;
    if wait_before_first_poll {
        let remaining_ms = deadline - clock.now_ms() as f64;
        if remaining_ms > 0.0 {
            clock
                .sleep(
                    (interval_ms as f64).min(remaining_ms) as u64,
                    signal.clone(),
                )
                .await?;
        }
    }

    while (clock.now_ms() as f64) < deadline {
        if signal.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(cancelled());
        }

        match poll().await {
            DeviceCodePollResult::Complete { value } => return Ok(value),
            DeviceCodePollResult::Failed { message } => return Err(error(&message)),
            DeviceCodePollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                // Use the server-provided interval when given (GitHub reports
                // the new required minimum in `interval`); trusting only a
                // client-tracked value risks polling early forever under
                // WSL/VM clock drift. Otherwise apply RFC 8628 section 3.5:
                // increase by 5 seconds.
                interval_ms = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        MINIMUM_INTERVAL_MS.max((seconds * 1000.0).floor() as u64)
                    }
                    _ => MINIMUM_INTERVAL_MS.max(interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS),
                };
            }
            DeviceCodePollResult::Pending => {}
        }

        let remaining_ms = deadline - clock.now_ms() as f64;
        if remaining_ms <= 0.0 {
            break;
        }

        clock
            .sleep(
                (interval_ms as f64).min(remaining_ms) as u64,
                signal.clone(),
            )
            .await?;
    }

    Err(error(if slow_down_responses > 0 {
        SLOW_DOWN_TIMEOUT_MESSAGE
    } else {
        TIMEOUT_MESSAGE
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::oneshot;

    use super::*;

    /// Manual clock: `advance` fires sleepers whose deadline has passed.
    /// Mirrors the vitest fake-timer driving in `oauth-device-code.test.ts`.
    struct FakeClock {
        inner: Mutex<FakeClockInner>,
    }

    struct FakeClockInner {
        now_ms: u64,
        sleepers: Vec<(u64, oneshot::Sender<()>)>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                inner: Mutex::new(FakeClockInner {
                    now_ms: 0,
                    sleepers: Vec::new(),
                }),
            }
        }

        fn advance(&self, ms: u64) {
            let senders = {
                let mut inner = self.inner.lock().expect("lock");
                inner.now_ms += ms;
                let now = inner.now_ms;
                let (fire, keep) = inner
                    .sleepers
                    .drain(..)
                    .partition(|(wake_at, _)| *wake_at <= now);
                inner.sleepers = keep;
                fire
            };
            for (_, sender) in senders {
                let _ = sender.send(());
            }
        }

        fn sleeper_count(&self) -> usize {
            self.inner.lock().expect("lock").sleepers.len()
        }
    }

    impl DeviceFlowClock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.inner.lock().expect("lock").now_ms
        }

        fn sleep(
            &self,
            ms: u64,
            signal: Option<CancellationToken>,
        ) -> BoxFutureSend<'_, Result<(), ModelsError>> {
            if let Some(token) = &signal
                && token.is_cancelled()
            {
                return Box::pin(async { Err(cancelled()) });
            }
            if ms == 0 {
                return Box::pin(async { Ok(()) });
            }
            let wake_at = self.now_ms() + ms;
            let (tx, rx) = oneshot::channel();
            self.inner
                .lock()
                .expect("lock")
                .sleepers
                .push((wake_at, tx));
            Box::pin(async move {
                match signal {
                    Some(token) => tokio::select! {
                        _ = rx => Ok(()),
                        _ = token.cancelled() => Err(cancelled()),
                    },
                    None => rx.await.map(|_| ()).map_err(|_| cancelled()),
                }
            })
        }
    }

    /// Yield until `condition` holds (lets the spawned flow make progress).
    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..1000 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition not met");
    }

    /// Give a (possibly misbehaving) flow the chance to run — used for the
    /// negative "did NOT poll yet" assertions.
    async fn yield_a_few_times() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    type PollTimes = Arc<Mutex<Vec<u64>>>;

    fn times(poll_times: &PollTimes) -> Vec<u64> {
        poll_times.lock().expect("lock").clone()
    }

    type PollFn = Box<
        dyn FnMut() -> BoxFutureSend<'static, DeviceCodePollResult<&'static str>> + Send + Sync,
    >;

    struct Harness {
        clock: Arc<FakeClock>,
        poll_times: PollTimes,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                clock: Arc::new(FakeClock::new()),
                poll_times: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn spawn(
            &self,
            interval_seconds: Option<f64>,
            expires_in_seconds: Option<f64>,
            wait_before_first_poll: bool,
            signal: Option<CancellationToken>,
            poll: PollFn,
        ) -> tokio::task::JoinHandle<Result<&'static str, ModelsError>> {
            let clock: Arc<dyn DeviceFlowClock> = self.clock.clone();
            tokio::spawn(poll_oauth_device_code_flow_with_clock(
                DeviceCodePollOptions {
                    interval_seconds,
                    expires_in_seconds,
                    wait_before_first_poll,
                    poll,
                    signal,
                },
                clock,
            ))
        }

        fn clock_as(&self) -> Arc<dyn DeviceFlowClock> {
            self.clock.clone()
        }
    }

    /// `polls immediately and returns the completed value`.
    #[tokio::test]
    async fn polls_immediately_and_returns_the_completed_value() {
        let harness = Harness::new();
        let poll_times = harness.poll_times.clone();
        let clock = harness.clock_as();
        let poll_count = Arc::new(AtomicUsize::new(0));
        let handle = harness.spawn(
            Some(2.0),
            Some(30.0),
            false,
            None,
            Box::new(move || {
                let poll_times = poll_times.clone();
                let clock = clock.clone();
                let poll_count = poll_count.clone();
                Box::pin(async move {
                    poll_times.lock().expect("lock").push(clock.now_ms());
                    if poll_count.fetch_add(1, Ordering::SeqCst) == 0 {
                        DeviceCodePollResult::Pending
                    } else {
                        DeviceCodePollResult::Complete { value: "token" }
                    }
                })
            }),
        );

        wait_until(|| times(&harness.poll_times).len() == 1).await;
        assert_eq!(times(&harness.poll_times), vec![0]);
        wait_until(|| harness.clock.sleeper_count() == 1).await;

        harness.clock.advance(1999);
        yield_a_few_times().await;
        assert_eq!(times(&harness.poll_times), vec![0]);

        harness.clock.advance(1);
        let result = handle.await.expect("join");
        assert_eq!(result.expect("token"), "token");
        assert_eq!(times(&harness.poll_times), vec![0, 2000]);
    }

    /// `can wait before the first poll`.
    #[tokio::test]
    async fn can_wait_before_the_first_poll() {
        let harness = Harness::new();
        let poll_times = harness.poll_times.clone();
        let clock = harness.clock_as();
        let handle = harness.spawn(
            Some(2.0),
            Some(30.0),
            true,
            None,
            Box::new(move || {
                let poll_times = poll_times.clone();
                let clock = clock.clone();
                Box::pin(async move {
                    poll_times.lock().expect("lock").push(clock.now_ms());
                    DeviceCodePollResult::Complete { value: "token" }
                })
            }),
        );

        wait_until(|| harness.clock.sleeper_count() == 1).await;
        harness.clock.advance(1999);
        yield_a_few_times().await;
        assert_eq!(times(&harness.poll_times), Vec::<u64>::new());

        harness.clock.advance(1);
        let result = handle.await.expect("join");
        assert_eq!(result.expect("token"), "token");
        assert_eq!(times(&harness.poll_times), vec![2000]);
    }

    /// `increases the interval by 5 seconds after slow_down without a server interval`.
    #[tokio::test]
    async fn increases_the_interval_by_5_seconds_after_slow_down_without_a_server_interval() {
        let harness = Harness::new();
        let poll_times = harness.poll_times.clone();
        let clock = harness.clock_as();
        let poll_count = Arc::new(AtomicUsize::new(0));
        let handle = harness.spawn(
            Some(2.0),
            Some(900.0),
            false,
            None,
            Box::new(move || {
                let poll_times = poll_times.clone();
                let clock = clock.clone();
                let poll_count = poll_count.clone();
                Box::pin(async move {
                    poll_times.lock().expect("lock").push(clock.now_ms());
                    if poll_count.fetch_add(1, Ordering::SeqCst) == 0 {
                        DeviceCodePollResult::SlowDown {
                            interval_seconds: None,
                        }
                    } else {
                        DeviceCodePollResult::Complete { value: "token" }
                    }
                })
            }),
        );

        wait_until(|| harness.clock.sleeper_count() == 1).await;
        assert_eq!(times(&harness.poll_times), vec![0]);

        harness.clock.advance(6999);
        yield_a_few_times().await;
        assert_eq!(times(&harness.poll_times), vec![0]);

        harness.clock.advance(1);
        let result = handle.await.expect("join");
        assert_eq!(result.expect("token"), "token");
        assert_eq!(times(&harness.poll_times), vec![0, 7000]);
    }

    /// `honors a server-provided slow_down interval`.
    #[tokio::test]
    async fn honors_a_server_provided_slow_down_interval() {
        let harness = Harness::new();
        let poll_times = harness.poll_times.clone();
        let clock = harness.clock_as();
        let poll_count = Arc::new(AtomicUsize::new(0));
        let handle = harness.spawn(
            Some(2.0),
            Some(900.0),
            false,
            None,
            Box::new(move || {
                let poll_times = poll_times.clone();
                let clock = clock.clone();
                let poll_count = poll_count.clone();
                Box::pin(async move {
                    poll_times.lock().expect("lock").push(clock.now_ms());
                    if poll_count.fetch_add(1, Ordering::SeqCst) == 0 {
                        DeviceCodePollResult::SlowDown {
                            interval_seconds: Some(30.0),
                        }
                    } else {
                        DeviceCodePollResult::Complete { value: "token" }
                    }
                })
            }),
        );

        wait_until(|| harness.clock.sleeper_count() == 1).await;
        assert_eq!(times(&harness.poll_times), vec![0]);

        harness.clock.advance(29999);
        yield_a_few_times().await;
        assert_eq!(times(&harness.poll_times), vec![0]);

        harness.clock.advance(1);
        let result = handle.await.expect("join");
        assert_eq!(result.expect("token"), "token");
        assert_eq!(times(&harness.poll_times), vec![0, 30000]);
    }

    /// `cancels an in-flight wait`.
    #[tokio::test]
    async fn cancels_an_in_flight_wait() {
        let harness = Harness::new();
        let token = CancellationToken::new();
        let handle = harness.spawn(
            Some(5.0),
            Some(30.0),
            false,
            Some(token.clone()),
            Box::new(move || Box::pin(async move { DeviceCodePollResult::Pending })),
        );

        wait_until(|| harness.clock.sleeper_count() == 1).await;
        token.cancel();
        let error = handle.await.expect("join").expect_err("cancelled");
        assert_eq!(error.message, "Login cancelled");
    }

    /// Extra (no upstream counterpart): pins the verbatim timeout messages
    /// with and without a preceding `slow_down`.
    #[tokio::test]
    async fn times_out_with_verbatim_messages() {
        for (slow_down, expected) in [(false, TIMEOUT_MESSAGE), (true, SLOW_DOWN_TIMEOUT_MESSAGE)] {
            let harness = Harness::new();
            let handle = harness.spawn(
                Some(2.0),
                Some(10.0),
                false,
                None,
                Box::new(move || {
                    Box::pin(async move {
                        if slow_down {
                            DeviceCodePollResult::SlowDown {
                                interval_seconds: None,
                            }
                        } else {
                            DeviceCodePollResult::Pending
                        }
                    })
                }),
            );
            wait_until(|| harness.clock.sleeper_count() == 1).await;
            harness.clock.advance(10_000);
            let error = handle.await.expect("join").expect_err("timed out");
            assert_eq!(error.message, expected);
        }
    }
}

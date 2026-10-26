//! Cancel-aware SSE body reads (P1-5).
//!
//! The upstream SDK hands the abort signal to `fetch`, so cancellation
//! interrupts an in-flight body read immediately. The port's byte streams are
//! decoupled from the request future (`custom_fetch` only races the signal
//! during send), so an adapter that checks `is_cancelled()` only *after*
//! `next().await` returns stays blocked until the next chunk arrives or
//! the idle timeout fires (unbounded by default since rpi#54 / D-103;
//! finite only when the user configures `httpIdleTimeoutMs`).
//!
//! [`next_chunk_or_cancelled`] closes that gap for every streaming adapter:
//! the body read is raced against `CancellationToken::cancelled`, matching the
//! `mistral_conversations` implementation that previously was the only
//! adapter with the correct semantics.

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

/// Outcome of [`next_chunk_or_cancelled`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamNext<T> {
    /// The stream produced an item (`None` = stream ended).
    Item(T),
    /// The abort token fired before the next chunk arrived.
    Cancelled,
}

/// Await the next body chunk while staying responsive to `signal`.
///
/// `None` for `signal` behaves exactly like `stream.next().await` (no
/// cancellation support — same as an unset upstream signal).
pub(crate) async fn next_chunk_or_cancelled<S>(
    stream: &mut S,
    signal: Option<&CancellationToken>,
) -> StreamNext<Option<S::Item>>
where
    S: futures::Stream + Unpin,
{
    match signal {
        Some(token) => {
            tokio::select! {
                chunk = stream.next() => StreamNext::Item(chunk),
                () = token.cancelled() => StreamNext::Cancelled,
            }
        }
        None => StreamNext::Item(stream.next().await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[tokio::test]
    async fn returns_items_and_stream_end() {
        let mut stream = stream::iter(vec![1u8, 2, 3]);
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, None).await,
            StreamNext::Item(Some(1))
        );
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, None).await,
            StreamNext::Item(Some(2))
        );
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, None).await,
            StreamNext::Item(Some(3))
        );
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, None).await,
            StreamNext::Item(None)
        );
    }

    /// P1-5 core regression: cancellation must win even while the stream is
    /// stalled (no chunk will ever arrive) — the pre-fix shape awaited
    /// `next()` first and never observed the token on an idle body.
    #[tokio::test]
    async fn cancellation_interrupts_a_stalled_stream() {
        let mut stalled = stream::pending::<u8>();
        let token = CancellationToken::new();
        tokio::spawn({
            let token = token.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                token.cancel();
            }
        });
        let started = std::time::Instant::now();
        assert_eq!(
            next_chunk_or_cancelled(&mut stalled, Some(&token)).await,
            StreamNext::Cancelled
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "cancellation must not wait for the idle timeout"
        );
    }

    /// A stream that keeps producing chunks still delivers them while the
    /// token stays live.
    #[tokio::test]
    async fn items_flow_while_token_is_live() {
        let mut stream = stream::iter(vec![7u8, 8]);
        let token = CancellationToken::new();
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, Some(&token)).await,
            StreamNext::Item(Some(7))
        );
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, Some(&token)).await,
            StreamNext::Item(Some(8))
        );
        assert_eq!(
            next_chunk_or_cancelled(&mut stream, Some(&token)).await,
            StreamNext::Item(None)
        );
    }
}

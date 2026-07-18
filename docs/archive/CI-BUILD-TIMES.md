# CI build-time baselines

Tracked so future-Claude can see whether sccache + nextest are paying
off vs. the pre-N4 baseline.

## Pre-N4 baseline (branch `next`, pre-N4)

Local M1 Pro, cold target/:

| command | wall-time |
|---|---|
| `cargo check -p roost-coordinator` | ~65s |
| `cargo build --locked` (coord) | ~95s |
| `cargo test --locked` (coord) | ~70s |
| `cargo build --locked -p idea-worker` | ~55s |
| `cargo test --locked` (worker) | ~50s |

Warm target/, no source change:

| command | wall-time |
|---|---|
| `cargo check -p roost-coordinator` | <1s |
| `cargo test --locked` (coord) | ~10s |
| `cargo test --locked` (worker) | ~5s |

CI cold (GitHub Actions, no cache hit): ~6-7 min total wall-time.

## N4: sccache + nextest

CI:

| variant | wall-time |
|---|---|
| cold (no cache) | TODO |
| warm (full cache) | TODO |

(Numbers populated on first green CI run after this commit.)

## Notes

- sccache uses the `mozilla-actions/sccache-action` cache backed by
  the GHA cache (`SCCACHE_GHA_ENABLED=true`); local dev uses the
  default disk cache.
- nextest replaces `cargo test` in CI; parallel test runner, no
  unsoundness vs. the stock harness for our suite.
- sccache stats print at end of the CI job; check there for
  cache-hit rate.

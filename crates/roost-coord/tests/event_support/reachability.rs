//! Whether a capability is REACHABLE, as a test rather than as a review note.
//!
//! Read by `event_reachability.rs`. Split out so these and the coordinator
//! fixture they read do not share a file: these answer "does anything call
//! this", the fixture answers "is this coordinator booted", and mixing them made
//! the file the largest in the test tree for no reader's benefit.
//!
//! **A GREP CANNOT READ THE DIRECTION OF DATA.** The first version of the reader
//! asked for any file that mentioned `snapshot_reap_ids` without declaring it,
//! and `append_transaction.rs:78` BUILDS the field without declaring it — so a
//! producer satisfied a test written for a consumer, and the guard passed six of
//! six green on a capability with no consumer. Two conjuncts exist because of
//! that: outside the named producers, AND in read position. The dot is the
//! direction, syntactically, and it cannot go stale the way a list can.

/// Whether anything READS the ids a deferred append returns.
///
/// **TWO CONJUNCTS, and the second one is what stops this rotting.** A first
/// version grepped for any mention of `snapshot_reap_ids` that did not declare
/// it, and `append_transaction.rs:78` BUILDS the field without declaring it — so
/// a PRODUCER satisfied a test written for a CONSUMER, six of six green on a
/// capability with no consumer. Naming the four producers fixed the current
/// ambiguity and left the class open: a FIFTH producer would be counted as a
/// consumer and turn this green with nothing draining, which is the original
/// failure arriving by a different route.
///
/// So both, and neither replaces the other:
///
/// - **outside the four named producer files**, and
/// - **the field in READ position** — a dot. A read is `result.snapshot_reap_ids`;
///   a construction is `snapshot_reap_ids:`.
///
/// THE DOT IS NECESSARY AND NOT SUFFICIENT, which is why the file list stays.
/// A producer that does `out.snapshot_reap_ids = v` has a dot and is a write.
/// And the two failure directions are opposite, so the guard is deliberately
/// biased toward refusing: a real reader the guard misses turns the test RED
/// when it should be green (a bug report), while a producer the guard counts
/// turns it GREEN when it should be red (silence).
pub fn the_deferred_reap_ids_have_a_production_reader() -> bool {
    const PRODUCERS: [&str; 4] = [
        "events/append.rs",
        "events/append_publication.rs",
        "events/append_transaction.rs",
        "events/pending_publications.rs",
    ];
    roost_src_files()
        .filter(|(path, _)| !PRODUCERS.iter().any(|p| path.ends_with(p)))
        .any(|(_, source)| source.contains(".snapshot_reap_ids"))
}

/// Whether the DEFERRED-APPEND PATH has an execution path at all.
///
/// THE CAMOUFLAGE THIS EXISTS TO DEFEAT. The guard in `event_reachability.rs`
/// asserts that the ids come back correctly, and that is exactly what makes
/// the capability look covered while nothing consumes it. **A passing assertion
/// on unreachable code reads as coverage and is more dangerous than no test at
/// all** — a missing assertion looks like a gap that invites a question, and a
/// passing one closes it.
///
/// WHY THIS ASKS ABOUT THE FLAG AND NOT ABOUT THE IDS. The first version of this
/// guard grepped `src/` for a file that mentions `snapshot_reap_ids` without
/// declaring it, and **it passed — on a capability with no consumer**, because
/// `append_transaction.rs` builds the field and does not declare it, so a
/// PRODUCER satisfied a test written for a CONSUMER. That is this session's own
/// class of defect, committed by the guard meant to catch it, and the fix is to
/// ask a question grep can answer without ambiguity.
///
/// `defer_snapshot_reap: true` can only be written by a caller constructing
/// `AppendOptions` to defer. The declaration, the `Debug` field and the read
/// inside `build_result` are the only other mentions of the name and none of
/// them can produce a `true` — so this returns true if and only if some
/// production caller defers, and false while the only caller that would is the
/// unwritten worker link.
pub fn the_deferred_append_path_has_an_execution_path() -> bool {
    roost_src_files().any(|(_, source)| source.contains("defer_snapshot_reap: true"))
}

/// Every `.rs` file under the crate's `src/`, as text.
///
/// `src/` only, and not `tests/`: counting assertions in the test tree would
/// make this guard its own camouflage.
fn roost_src_files() -> impl Iterator<Item = (String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .flat_map(|domain| {
            std::fs::read_dir(domain.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .filter(|file| file.path().extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|file| {
            let path = file.path().to_string_lossy().into_owned();
            std::fs::read_to_string(file.path())
                .ok()
                .map(|source| (path, source))
        })
}

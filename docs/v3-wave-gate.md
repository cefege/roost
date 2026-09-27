## A short-circuiting operator whose operands MUTATE

The most valuable defect class this wave produced, and it is not a lint, not a
missing test, and not a measurement error. It is invisible to every gate.

```rust
let had_scoped = !store.mcp_relays.is_empty()
    || !store.pair_requests.is_empty()
    || toasts::clear_all(&mut store.toasts)          // mutates
    || transfers::clear_all(&mut store.transfers)    // mutates
    || !store.spawns.is_empty()
    || !store.pending_closes.is_empty();
```

**`||` stops at the first truthy term, so a mutating operand is CONDITIONALLY
EXECUTED.** The test populated a `pair_request`, that term answered `true`, and
**both `clear_all` calls were never made.** One assertion caught the toast half;
the very next assertion at `:371` — `assert!(store.transfers.is_empty(), "a card
names a file and a path from the old account")` — was **the same line failing a
second time.** The leak is a toast quoting a machine the new account cannot see.

**The rules, and they generalise past the crate they were found in:**

1. **Never put a mutating call in a `||`/`&&` chain.** An operand with a side
   effect is not a value, and `||` is not a place to put one.
2. **When a clear, flush or sweep is an operand, read its result from a call
   already made** — run the clears first, then build the predicate from their
   return values. The disjunction is then logically identical in every case, so
   behaviour a reviewer cares about (here, `revision`) **provably cannot change**
   and only the slices that get emptied differ. That is what makes such a fix
   safe to land beside 211 passing tests: the argument is structural, not
   empirical.
3. **A dormant copy of the same chain is a defect with a fuse.** The identical
   expression existed one function away with **no in-tree caller**, and was fixed
   in the same commit.
4. **Check the siblings for the class, then stop.** The sibling credential
   boundary here was straight-line with no predicate, so scope stayed at the two
   chains. A class-wide sweep on a hunch is how a fix becomes a refactor.

**The audit it suggests, and it is cheap:** grep for `||` and `&&` chains whose
operands are not field reads. Most are; the ones that **call** anything are a
short list, and each is either a bug or a comment saying why it is safe. That is
the `file:line` discipline the mutation rows already use, applied to a shape
rather than to a status.

**Why nothing caught it.** It passed `cargo check`. It passed clippy. It passed
**every test except the two that name it** — and those two were already written,
already in the suite, waiting. A defect that only one assertion names is a defect
that ships, so the question after any caught leak is *what else does this line
reach*, and here the answer was one assertion away. The sibling defect found an
earlier commit was the same shape: `mark_session_open` documented starting a
browser lifecycle fence and did not, because the fence is installed in **two**
places — the closed set and `AgentStatusOrder::record_close`, which retires the
occupant in a different map — so the guard passed, `or_insert_with` did not fire,
`accepts` hit `is_retired`, and the fence never started.

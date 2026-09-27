//! The predictive echo: the characters a fast typist sees before the PTY has
//! echoed them back, and the one rule that stops a prediction from outliving the
//! answer it was guessing at.
//!
//! Ported from v2's `apps/web/src/client/input/predictiveEcho*.ts` and
//! `apps/web/src/renderer/predictiveEcho.ts`. The GRID arithmetic — which cell a
//! prediction occupies, and where the cursor lands after it — is here; painting
//! the prediction and taking it back off is `roost-web-terminal`'s, because that
//! is DOM.
//!
//! Depends on `roost_protocol`'s cell model. `docs/FAILURE-INDEX.md:2354` and
//! `:2386` are the two defects this exists to not have: a predicted character
//! flashing the wrong glyph, and sustained fast typing wiping its own
//! predictions once a second.

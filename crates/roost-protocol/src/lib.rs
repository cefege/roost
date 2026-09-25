//! Pure I/O-free wire and terminal logic: event variants and the canonical fold, cell and grid models, viewport geometry, peer packet framing, the keeper-update contract, and layout documents. Builds for wasm32 so browser, native, coordinator and worker share one implementation.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]

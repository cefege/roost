//! The imperative web-sys terminal renderer plus its input, IME, mouse, selection and link controllers. Owns the DOM contract and holds no framework code, so the state machine is testable outside a browser.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]

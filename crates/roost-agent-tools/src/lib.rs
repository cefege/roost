//! The worker-side agent tools: read, write, the hashline edit, bash, grep, glob,
//! LSP and the on-demand language-server installer. The worker runtime calls
//! `ToolHost`; argument shapes come from roost-protocol.

#![forbid(unsafe_code)]

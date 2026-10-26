//! Bedrock transport helpers: hand-written SigV4 signing and AWS event-stream
//! decoding (design §14; split out of `bedrock_converse_stream.rs`).

pub mod event_stream;
pub mod sigv4;

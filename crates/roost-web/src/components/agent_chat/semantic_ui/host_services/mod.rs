//! Browser-side prompts for OMP host-service requests.
//!
//! One controller lives in the command surface for the lifetime of its native
//! view and transport; the prompt cards are presentational children of that
//! scope. A dialog can therefore mount and unmount its DOM without repeating a
//! clipboard write or losing an admitted reply.

mod actions;
mod browser;
mod controller;
mod fork;
mod live_audio;
#[cfg(target_arch = "wasm32")]
mod live_media;
#[cfg(target_arch = "wasm32")]
mod live_peer;
#[cfg(target_arch = "wasm32")]
mod live_requests;
#[cfg(target_arch = "wasm32")]
mod live_wait;
mod prompts;
mod reply;
#[cfg(test)]
mod tests;

pub(super) use browser::open_blank_tab;

pub(crate) use controller::use_omp_host_services;
pub(crate) use prompts::OmpHostServicePrompts;

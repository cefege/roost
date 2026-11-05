//! Browser-side prompts for OMP host-service requests.
//!
//! One controller lives in the command surface for the lifetime of its native
//! view and transport; the prompt cards are presentational children of that
//! scope. A dialog can therefore mount and unmount its DOM without repeating a
//! clipboard write or losing an admitted reply.

mod actions;
mod browser;
mod controller;
mod prompts;
mod reply;
#[cfg(test)]
mod tests;

pub(crate) use controller::use_omp_host_services;
pub(crate) use prompts::OmpHostServicePrompts;

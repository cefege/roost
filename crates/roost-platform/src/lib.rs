//! Path and shell conventions, with no I/O, no clock, no environment and no
//! async: the host platform vocabulary, a lexical path algebra that
//! implements POSIX and Windows rules explicitly, the one POSIX shell quoter,
//! and the two environment entry names an installed service definition
//! carries. Every crate that touches the filesystem depends on this instead of
//! asking the OS directly.

#![forbid(unsafe_code)]

pub mod host_platform;
mod native_path;
pub mod shell_quote;
pub mod worker_service_env;

pub use host_platform::{HostPlatform, PlatformError};
pub use native_path::{
    DARWIN_PRIVATE_ROOTS, NativePathCrumb, NativePathError, decode_native_path_route,
    encode_native_path_route, native_path_basename, native_path_crumbs, native_path_dirname,
    native_path_identity_key, native_path_join, native_path_to_fs_path, normalize_native_path,
    same_worker_folder,
};
pub use shell_quote::posix_shell_quote;
pub use worker_service_env::{AGENT_CONVERSATION_RESTORE_ENV, KEEPER_FORCE_LIVE_RETIRE_ENV};

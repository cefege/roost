//! Pure platform path and shell conventions, without I/O or environment reads.
//! Owns POSIX/Windows path algebra and shell quoting.
//! Enrollment, pairing links and installed-service environment names use these rules.
//! Native hosts and browser clients share the same explicit platform vocabulary.

#![forbid(unsafe_code)]

pub mod browser_pairing_link;
pub mod host_platform;
pub mod machine_join_command;
mod native_path;
pub mod shell_quote;
pub mod worker_service_env;

pub use browser_pairing_link::{PAIR_FRAGMENT_KEY, browser_pairing_link};
pub use host_platform::{HostPlatform, PlatformError};
pub use machine_join_command::{
    BOOTSTRAP_TOKEN_ENV, COORDINATOR_URL_ENV, INSTALL_POWERSHELL_URL, INSTALL_SCRIPT_URL,
    WORKER_LABEL_ENV, machine_join_command, machine_join_command_powershell,
};
pub use native_path::{
    DARWIN_PRIVATE_ROOTS, NativePathCrumb, NativePathError, decode_native_path_route,
    encode_native_path_route, native_path_basename, native_path_crumbs, native_path_dirname,
    native_path_identity_key, native_path_join, native_path_to_fs_path, normalize_native_path,
    same_worker_folder,
};
pub use shell_quote::{posix_shell_quote, powershell_single_quote};
pub use worker_service_env::KEEPER_FORCE_LIVE_RETIRE_ENV;

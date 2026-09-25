//! A full lexical path algebra for POSIX and Windows, with no filesystem
//! access at all. `std::path` is deliberately not used anywhere under here: it
//! follows the rules of the host this process runs on, and the whole point is
//! that a coordinator on macOS can reason about a path on a worker that runs
//! Windows. Existing-path realpath resolution happens at the worker boundary,
//! where the machine that owns the path can answer.

mod fs_path;
mod identity;
mod normalize;
mod parts;
mod route;

pub use fs_path::native_path_to_fs_path;
pub use identity::{DARWIN_PRIVATE_ROOTS, native_path_identity_key, same_worker_folder};
pub use normalize::{NativePathError, normalize_native_path};
pub use parts::{
    NativePathCrumb, native_path_basename, native_path_crumbs, native_path_dirname,
    native_path_join,
};
pub use route::{decode_native_path_route, encode_native_path_route};

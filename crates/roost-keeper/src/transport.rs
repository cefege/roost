//! The keeper socket's stream types and the one "is this a socket file" test.
//! Unix uses std's AF_UNIX types; Windows uses `uds_windows`, the same
//! AF_UNIX family over Winsock with std's API shape. The server, client and
//! input queue import from here, and the worker's keeper probes call
//! [`is_socket_file`].

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

/// Whether `path` is a bound AF_UNIX socket file (not followed through links).
#[cfg(unix)]
pub fn is_socket_file(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        use std::os::unix::fs::FileTypeExt;
        metadata.file_type().is_socket()
    })
}

/// Whether `path` is a bound AF_UNIX socket file (not followed through links).
///
/// A Windows AF_UNIX socket file is a reparse point
/// (`FILE_ATTRIBUTE_REPARSE_POINT`, 0x400).
#[cfg(windows)]
pub fn is_socket_file(path: &std::path::Path) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    })
}

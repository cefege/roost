//! The coordinator's own `main.err.log`, for a host with no service manager to
//! redirect its stderr (a container). `main.rs` opens it when
//! `ROOST_LOG_FILE_DIR` is set and hands it to the logging facade as the
//! warn/error copy; `roost doctor` reads it back by the same name. One
//! previous file is kept, as `main.err.log.1`, which doctor's rotation match
//! already covers.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The directory the coordinator writes `main.err.log` into, when set.
pub const LOG_FILE_DIR_ENV: &str = "ROOST_LOG_FILE_DIR";

/// The size past which `main.err.log` is rotated to `main.err.log.1`.
pub const ERROR_LOG_ROTATE_BYTES: u64 = 32 * 1024 * 1024;

/// An append-only log file that rotates once it would pass `rotate_at` bytes.
#[derive(Debug)]
pub struct RotatingLogFile {
    path: PathBuf,
    file: File,
    written: u64,
    rotate_at: u64,
}

impl RotatingLogFile {
    /// Open (or create) `dir/main.err.log` for appending, counting what an
    /// earlier process already wrote toward the rotation size.
    pub fn open(dir: &Path, rotate_at: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(crate::doctor::log_sources::ERR_LOG_BASE);
        let file = OpenOptions::new().append(true).create(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            written,
            rotate_at,
        })
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        let mut previous = self.path.clone().into_os_string();
        previous.push(".1");
        std::fs::rename(&self.path, &previous)?;
        self.file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingLogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.rotate_at {
            self.rotate()?;
        }
        self.file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::RotatingLogFile;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("roost-log-file-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_write_past_the_size_moves_the_old_lines_to_the_previous_file() {
        let dir = scratch_dir("rotate");
        let mut file = RotatingLogFile::open(&dir, 64).unwrap();
        file.write_all(&[b'a'; 50]).unwrap();
        file.write_all(&[b'b'; 50]).unwrap();
        assert_eq!(
            std::fs::read(dir.join("main.err.log")).unwrap(),
            vec![b'b'; 50]
        );
        assert_eq!(
            std::fs::read(dir.join("main.err.log.1")).unwrap(),
            vec![b'a'; 50]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reopening_counts_what_an_earlier_process_wrote() {
        let dir = scratch_dir("reopen");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.err.log"), [b'x'; 10]).unwrap();
        let mut file = RotatingLogFile::open(&dir, 64).unwrap();
        assert_eq!(file.written, 10);
        file.write_all(&[b'y'; 60]).unwrap();
        assert_eq!(
            std::fs::read(dir.join("main.err.log.1")).unwrap(),
            vec![b'x'; 10],
            "the earlier bytes count toward the rotation size"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

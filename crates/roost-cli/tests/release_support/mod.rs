//! A release origin served over real HTTP, because the product fetches over
//! HTTP and a test that used `file://` would be testing a transport nothing
//! ships.
//!
//! `ROOST_RELEASE_BASE_URL` is the one seam `update::release` reads, so pointing
//! it here exercises the production path end to end: the same URL builder, the
//! same sidecar-first verification, the same streaming hash. What differs is
//! only the origin.
//!
//! The property the caller has to be able to assert is that the INSTALLED bytes
//! are the ones this origin served, which is why every asset here is a marker
//! the test can search for rather than a plausible-looking binary.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One served file: the body, and the digest a `sha256sum` sidecar publishes.
#[derive(Debug, Clone)]
pub struct FakeAsset {
    /// The name the release publishes it under.
    pub name: String,
    /// The bytes served for that name.
    pub body: Vec<u8>,
    /// The digest of `body`, lower-case hex.
    pub sha256: String,
}

impl FakeAsset {
    /// A plain marker body, for an asset nothing executes.
    pub fn named(name: &str, marker: &str) -> Self {
        Self::with_body(name, format!("# roost asset\n# marker: {marker}\n").into_bytes())
    }

    /// A `roost` stand-in that is a REAL program.
    ///
    /// A deploy reads the keeper contract by running the staged `roost`, so a
    /// body that is only bytes would make the fixture test a failure instead
    /// of a fetch. The script answers `__keeper-contract` with a shape the
    /// shared contract accepts, and refuses everything else the way the real
    /// binary does — so a test that installs these bytes exercises the same
    /// path a real target takes.
    pub fn roost_program(name: &str) -> Self {
        let body = concat!(
            "#!/bin/sh\n",
            "# roost asset\n",
            "# marker: roost\n",
            "if [ \"$1\" = \"__keeper-contract\" ]; then\n",
            "  printf '%s\\n' '{\"protocol_version\":3,\"supported_features\":[],\
             \"required_features\":[],\"implementation_digest\":null,\"platform\":\"linux\",\
             \"arch\":\"x86_64\",\"build_sha\":\"0.0.0\"}'\n",
            "  exit 0\n",
            "fi\n",
            "echo \"this is not the coordinator you are looking for\" >&2\nexit 2\n"
        )
        .as_bytes()
        .to_vec();
        Self::with_body(name, body)
    }

    fn with_body(name: &str, body: Vec<u8>) -> Self {
        Self {
            name: name.to_string(),
            sha256: sha256_hex(&body),
            body,
        }
    }
}

/// A running origin. Dropping it stops the listener and removes the directory.
pub struct FakeRelease {
    /// The tag this origin publishes under.
    pub tag: String,
    /// The base URL to point `ROOST_RELEASE_BASE_URL` at.
    pub base_url: String,
    /// The assets it serves, keyed by name.
    pub assets: BTreeMap<String, FakeAsset>,
    root: PathBuf,
}

impl FakeRelease {
    /// Start an origin publishing `assets` under `tag`, in a fresh directory.
    ///
    /// The listener runs on its own thread and answers each request from the
    /// map, so a test never has to sequence a server it also drives.
    pub fn start(tag: &str, assets: Vec<FakeAsset>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-fake-release-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the fake release root is created");

        let mut served = BTreeMap::new();
        for asset in assets {
            for file_name in [asset.name.clone(), format!("{}.sha256", asset.name)] {
                std::fs::write(root.join(&file_name), &asset.body)
                    .or_else(|_| std::fs::write(root.join(&file_name), format!("{}  {}\n", asset.sha256, asset.name)))
                    .expect("the asset is written to the origin's directory");
            }
            served.insert(asset.name.clone(), asset);
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("the origin binds a loopback port");
        let port = listener.local_addr().expect("the bound address is readable").port();
        let served = Arc::new(served);
        let answering = Arc::clone(&served);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let served = Arc::clone(&answering);
                std::thread::spawn(move || {
                    let _ = answer(stream, &served);
                });
            }
        });

        Self {
            tag: tag.to_string(),
            base_url: format!("http://127.0.0.1:{port}"),
            assets: (*served).clone(),
            root,
        }
    }

    /// The body this origin serves for `name`, for a test to assert against.
    pub fn body(&self, name: &str) -> Vec<u8> {
        self.assets
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not one of the assets this release publishes"))
            .body
            .clone()
    }

    /// The file this origin left the assets in, for a test that wants to read
    /// the digest sidecar directly.
    pub fn directory(&self) -> &Path {
        &self.root
    }
}

impl Drop for FakeRelease {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Answer one request: the first path segment names the asset, `.sha256` is
/// the sidecar, and anything else is a 404.
fn answer(mut stream: TcpStream, served: &BTreeMap<String, FakeAsset>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // Drain the headers, so the client sees a complete response rather than a
    // reset when it is done writing.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        if line == "\r\n" || line == "\n" {
            break;
        }
        line.clear();
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
    let name = path.trim_start_matches('/');
    let (status, reason, body) = match name.strip_suffix(".sha256") {
        Some(asset) => match served.get(asset) {
            Some(found) => (
                "200 OK",
                "text/plain",
                format!("{}  {}\n", found.sha256, found.name).into_bytes(),
            ),
            None => ("404 Not Found", "text/plain", b"no such asset\n".to_vec()),
        },
        None => match served.get(name) {
            Some(found) => ("200 OK", "application/octet-stream", found.body.clone()),
            None => ("404 Not Found", "text/plain", b"no such asset\n".to_vec()),
        },
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {reason}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

/// SHA-256 as lower-case hex, the spelling both `sha256sum` and
/// `shasum -a 256` publish.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Read a whole file, for a test asserting on what an install wrote.
pub fn read(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("{} cannot be read: {error}", path.display()))
        .read_to_end(&mut bytes)
        .expect("the file is readable");
    bytes
}

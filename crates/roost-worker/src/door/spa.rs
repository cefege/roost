//! The door's page responder: one complete disk build, chosen once (v2
//! `packages/host/src/spa.ts` `createSpaResponder`, which the worker's
//! `boot/boot-local-terminal.ts` handed to its door). Every path decision is
//! `roost_host::spa_path`'s, shared with the coordinator's front door
//! (`roost-coord` `http::spa`); this adapter reads the bytes, compresses and
//! memoizes, and sets the headers. Called by `runtime::door_routes`.

use std::path::{Path, PathBuf};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use roost_host::spa_path::{self, ContentEncoding, SpaTarget};
use tokio::io::AsyncWriteExt as _;

use super::spa_cache::{FileState, SpaCache};

/// The shell's name inside a build.
const INDEX_NAME: &str = "index.html";

/// The one build a door serves. `None` answers 404 to every page, which
/// `runtime::door_serve` reports when the door starts serving.
#[derive(Debug, Default)]
pub struct SpaMount {
    root: Option<PathBuf>,
    cache: SpaCache,
}

impl SpaMount {
    /// The build under `web_dist_path`, or none when it holds no `index.html`.
    pub fn from_dist_path(web_dist_path: Option<&Path>) -> Self {
        Self {
            root: spa_path::resolve_disk_spa_root(web_dist_path),
            cache: SpaCache::default(),
        }
    }

    /// The build's root, when there is one.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Answer one page request (v2 `spaResponse(url, method, acceptEncoding)`).
    pub async fn respond(&self, path: &str, method: &Method, accept_encoding: &str) -> Response {
        if !matches!(method, &Method::GET | &Method::HEAD) {
            return text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        }
        let Some(root) = self.root() else {
            return text(StatusCode::NOT_FOUND, "not found");
        };
        let head_only = method == Method::HEAD;
        let (file, is_index, precompressed) = match spa_path::resolve(root, path, accept_encoding) {
            SpaTarget::Asset { file, encoding } => (file, false, encoding == ContentEncoding::Gzip),
            SpaTarget::IndexFallback { file } => (file, true, false),
            SpaTarget::NotFound => return text(StatusCode::NOT_FOUND, "not found"),
        };
        // The headers describe the asset the URL NAMES: the cache rule keys on
        // its `assets/` and `fonts/` prefixes, and a `.gz` sibling is transport.
        let mut named = file
            .strip_prefix(root)
            .unwrap_or(file.as_path())
            .to_string_lossy()
            .into_owned();
        if precompressed && let Some(raw_length) = named.strip_suffix(".gz").map(str::len) {
            named.truncate(raw_length);
        }
        let compressible = spa_path::is_compressible(Path::new(&named));
        let compress = precompressed || (compressible && spa_path::accepts_gzip(accept_encoding));
        let body = match self
            .load(&file, compress && !precompressed, head_only)
            .await
        {
            Ok(body) => body,
            Err(reason) => {
                tracing::warn!(path = %file.display(), %reason, "the local door could not read a resolved page file");
                return text(StatusCode::NOT_FOUND, "not found");
            }
        };
        let cache = cache_control(&named, is_index);
        page(&named, cache, compressible, compress, body)
    }

    /// The bytes to send: `None` for a HEAD, the memoized gzip body when this
    /// side compresses, the file as it is otherwise.
    async fn load(
        &self,
        file: &Path,
        compress: bool,
        head_only: bool,
    ) -> std::io::Result<Option<Bytes>> {
        if head_only {
            return Ok(None);
        }
        if !compress {
            return tokio::fs::read(file)
                .await
                .map(|bytes| Some(Bytes::from(bytes)));
        }
        let state = FileState::of(file).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the file went away after it was resolved",
            )
        })?;
        if let Some(cached) = self.cache.get(file, state) {
            return Ok(Some(cached));
        }
        let mut source = tokio::io::BufReader::new(tokio::fs::File::open(file).await?);
        let mut encoder = async_compression::tokio::write::GzipEncoder::new(Vec::new());
        tokio::io::copy(&mut source, &mut encoder).await?;
        // Writes the gzip trailer; without it the member decodes truncated.
        encoder.shutdown().await?;
        let body = Bytes::from(encoder.into_inner());
        self.cache.put(file, state, body.clone());
        Ok(Some(body))
    }
}

/// v2's cache rule. The shell reached as a deep link is never cached; the
/// shell requested by its own name is an ordinary stable-named file there, and
/// revalidates like one.
fn cache_control(named: &str, is_index: bool) -> &'static str {
    if is_index {
        spa_path::cache_control_for(INDEX_NAME)
    } else if named == INDEX_NAME {
        "no-cache"
    } else {
        spa_path::cache_control_for(named)
    }
}

fn page(
    named: &str,
    cache: &'static str,
    compressible: bool,
    compress: bool,
    body: Option<Bytes>,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(spa_path::content_type_for(Path::new(named))),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    // Every compressible file varies, gzipped or not: a shared cache that kept
    // the identity body would hand it to a client that only accepts gzip.
    if compressible {
        headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if compress {
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    let mut response = (StatusCode::OK, headers).into_response();
    *response.body_mut() = Body::from(body.unwrap_or_default());
    response
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

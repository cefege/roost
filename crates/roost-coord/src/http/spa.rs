//! The browser's front door: the shell, the bundles, and the deep links between
//! them. Mounted as a middleware in front of the Connect service, so it is the
//! first thing a request meets and the last thing that can claim a page.
//!
//! Every path decision belongs to [`roost_host::spa_path`], which is pure and
//! which the worker's local door calls with the same three arguments. This
//! module owns only what that resolver cannot: reading the bytes, negotiating
//! and memoizing the gzip body, and setting the response headers.
//!
//! WHAT IT REFUSES, AND WHY IT IS A LAYER RATHER THAN A ROUTE. v2 branches on
//! the path inside one fetch handler (`coord-factory.ts:167-196`): Connect
//! first, then the export, then `/api/*`, then the SPA. A middleware outside
//! the router is that same order, and it has one property a `Router::fallback`
//! does not: the mounted routes — the two WebSocket upgrades and the export —
//! are never inspected as static paths, because the request still reaches them
//! through the router untouched.
//!
//! The method rule is v2's (`spa.ts:161-163`): a page is GET or HEAD, and
//! anything else is 405 rather than a page. `/roost.`, `/ws/` and `/api/` are
//! excluded FIRST, so a Connect POST, a socket upgrade and the export are
//! exactly as they were.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use roost_host::spa_path::{self, ContentEncoding, SpaTarget};
use tokio::io::AsyncWriteExt as _;
use tracing::warn;

use crate::http::listener::CONNECT_PATH_PREFIX;
use crate::http::spa_cache::{FileState, SpaCache};

/// The API namespace, which no static path may claim: `/api/db-export` is a
/// route and every other `/api/` path is a 404, never the shell.
const API_PREFIX: &str = "/api/";

/// The socket namespace, both upgrades. A handshake is not a page request and
/// must never be resolved against a build.
const WS_PREFIX: &str = "/ws/";

/// What one process serves, chosen once at boot.
///
/// `root` is resolved through [`spa_path::resolve_disk_spa_root`], so "there is
/// a build" is a fact with a file behind it rather than a setting that was
/// spelled. A `None` here answers 404 to every page, which is what `serve.rs`
/// reports at boot so the two cannot disagree.
#[derive(Debug, Default)]
pub struct SpaMount {
    root: Option<PathBuf>,
    cache: SpaCache,
}

impl SpaMount {
    /// The one build this process serves, or `None` when the configured path
    /// holds no `index.html`.
    #[must_use]
    pub fn from_dist_path(web_dist_path: Option<&Path>) -> Self {
        Self {
            root: spa_path::resolve_disk_spa_root(web_dist_path),
            cache: SpaCache::default(),
        }
    }

    /// The build's root, when there is one.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

/// The middleware `http::listener` mounts. See the module header for the order
/// this preserves and the paths it declines to claim.
pub async fn spa_layer(
    State(mount): State<Arc<SpaMount>>,
    request: Request,
    next: Next,
) -> Response {
    // Read, never destructured: a request this layer passes through has to
    // reach the router whole, body and all.
    let path = request.uri().path().to_owned();
    if owns_another_surface(&path) {
        return next.run(request).await;
    }
    if !matches!(request.method(), &Method::GET | &Method::HEAD) {
        return method_not_allowed();
    }
    let Some(root) = mount.root() else {
        return not_found();
    };
    let accept_encoding = accept_encoding_of(request.headers());
    let head_only = request.method() == Method::HEAD;
    let target = spa_path::resolve(root, &path, &accept_encoding);
    serve(&mount, root, target, &accept_encoding, head_only).await
}

/// Whether a path belongs to a surface that is not the SPA: Connect, the API
/// namespace, or one of the two socket upgrades.
fn owns_another_surface(path: &str) -> bool {
    path.starts_with(CONNECT_PATH_PREFIX)
        || path.starts_with(API_PREFIX)
        || path.starts_with(WS_PREFIX)
}

/// The `Accept-Encoding` header, or the empty string. Absent is not `*`: a
/// client that sent nothing has said nothing about gzip.
fn accept_encoding_of(headers: &HeaderMap) -> String {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Serve one resolved target.
async fn serve(
    mount: &SpaMount,
    root: &Path,
    target: SpaTarget,
    accept_encoding: &str,
    head_only: bool,
) -> Response {
    let (file, sibling) = match target {
        SpaTarget::Asset { file, encoding } => (file, encoding == ContentEncoding::Gzip),
        SpaTarget::IndexFallback { file } => (file, false),
        SpaTarget::NotFound => return not_found(),
    };
    // Every header below describes the asset the URL NAMES, relative to the
    // build. Two things depend on that and neither survives a bare file name:
    // the cache rule keys on the `assets/` and `fonts/` PREFIXES, and a
    // precompressed sibling is a transport detail whose `.gz` suffix would
    // otherwise turn `app.js` into an octet-stream nobody can execute.
    let relative = file.strip_prefix(root).unwrap_or(file.as_path());
    let named = without_gzip_suffix(&relative.to_string_lossy(), sibling);
    let cache = spa_path::cache_control_for(&named);
    let compressible = spa_path::is_compressible(Path::new(&named));
    let compress = sibling || (compressible && spa_path::accepts_gzip(accept_encoding));
    let body = match load(&file, compress, mount, head_only).await {
        Ok(body) => body,
        Err(reason) => {
            warn!(path = %file.display(), %reason, "spa: the resolved file could not be read");
            return not_found();
        }
    };
    response(&named, cache, compressible, compress, body, head_only)
}

/// The name the URL named, with the transport's `.gz` suffix taken off.
fn without_gzip_suffix(relative: &str, sibling: bool) -> String {
    match relative.strip_suffix(".gz") {
        Some(raw) if sibling => raw.to_owned(),
        _ => relative.to_owned(),
    }
}

/// The bytes to send, compressed when the client admits it.
///
/// The compression decision is composed here rather than left to the resolver
/// because only this side can act on it: the resolver reports a precompressed
/// sibling when one exists, and a caller holding a compressor makes one
/// otherwise. A build that ships no `.gz` files still gets gzip on the wire,
/// which is what the fixed extension set in `spa_path` exists for.
async fn load(
    file: &Path,
    compress: bool,
    mount: &SpaMount,
    head_only: bool,
) -> std::io::Result<Option<Bytes>> {
    if head_only {
        return Ok(None);
    }
    if compress {
        return compressed(file, mount).await.map(Some);
    }
    tokio::fs::read(file)
        .await
        .map(|bytes| Some(Bytes::from(bytes)))
}

/// The memoized gzip body for `file`, compressed on a miss.
async fn compressed(file: &Path, mount: &SpaMount) -> std::io::Result<Bytes> {
    let Some(state) = FileState::of(file) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the file went away between the resolve and the read",
        ));
    };
    if let Some(cached) = mount.cache.get(file, &state) {
        return Ok(cached);
    }
    let mut source = tokio::io::BufReader::new(tokio::fs::File::open(file).await?);
    let mut encoder = async_compression::tokio::write::GzipEncoder::new(Vec::new());
    tokio::io::copy(&mut source, &mut encoder).await?;
    // Writes the gzip trailer; a dropped encoder leaves a member that
    // decompresses to a truncated file.
    encoder.shutdown().await?;
    let body = Bytes::from(encoder.into_inner());
    mount.cache.put(file, state, body.clone());
    Ok(body)
}

/// The response for a file already read, with v2's four header rules.
fn response(
    named: &str,
    cache: &'static str,
    compressible: bool,
    compress: bool,
    body: Option<Bytes>,
    head_only: bool,
) -> Response {
    let mut headers = HeaderMap::new();
    set(
        &mut headers,
        header::CONTENT_TYPE,
        spa_path::content_type_for(Path::new(named)),
    );
    set(&mut headers, header::CACHE_CONTROL, cache);
    // `vary` rides with every compressible file, gzipped or not: the two
    // answers for one URL differ, and a shared cache that kept the identity
    // body would hand it to a client that only accepts gzip.
    if compressible {
        headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if compress {
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    if let Some(bytes) = body.as_ref() {
        set(
            &mut headers,
            header::CONTENT_LENGTH,
            &bytes.len().to_string(),
        );
    }
    let sent = body.filter(|_| !head_only).unwrap_or_default();
    let mut response = (StatusCode::OK, headers).into_response();
    *response.body_mut() = Body::from(sent);
    response
}

fn set(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

/// A page path that is not GET or HEAD. v2's 405, for the same reason: a POST
/// to `/s/:id` is a broken client, and answering it with the shell turns that
/// into a page that silently does nothing.
fn method_not_allowed() -> Response {
    text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

fn not_found() -> Response {
    text(StatusCode::NOT_FOUND, "not found")
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

//! The one static-file decision a complete SPA build needs, decided without a
//! web framework so the coordinator's front door and the worker's local door
//! cannot disagree about what a path is.
//!
//! The single entry point is [`resolve`]:
//! `(dist_root, request_path, accept_encoding) -> SpaTarget`, where
//! `SpaTarget` is `Asset { file, encoding } | IndexFallback { file } |
//! NotFound`. `file` is the path to READ. `encoding` is `Brotli` or `Gzip`
//! only when a real precompressed sibling was found, brotli first; a caller
//! that can compress itself composes gzip from [`is_compressible`] and
//! [`accepts_gzip`].
//!
//! Ported from `packages/host/src/spa.ts` over contract §6.2. The rules it
//! keeps, and why each exists:
//!
//! - A real file is served. A path under `assets/` that is not one 404s,
//!   because a content-hashed bundle must never fall back to HTML: a stale
//!   bundle reference would download a page and report it as a bundle.
//! - Anything else gets `index.html`, which is what makes `/s/:id` deep links
//!   work. They are not files and not under `assets/`, so they are pages.
//! - `..` and absolute paths are refused before any filesystem read, and the
//!   request path is used VERBATIM: it is never percent-decoded, so `%2e%2e`
//!   looks for a file literally named `%2e%2e` and finds nothing.
//! - A root with no `index.html` answers `NotFound` for everything, which is
//!   the state a boot-time log line must report rather than a 404 per page.
//!
//! Depends on `std` alone. Callers: `roost-coord::http::spa`, and the worker's
//! `door`.

use std::path::{Path, PathBuf};

/// Whether the bytes the caller is about to send are already compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentEncoding {
    /// The file on disk, sent as it is.
    Identity,
    /// A gzip body. The `file` path names the `.gz` sibling, never the raw one.
    Gzip,
    /// A brotli body. The `file` path names the `.br` sibling, never the raw
    /// one. Only a build writes these; no server compresses brotli itself.
    Brotli,
}

impl ContentEncoding {
    /// The `Content-Encoding` header value for bytes in this coding, `None`
    /// for identity.
    #[must_use]
    pub fn header_value(self) -> Option<&'static str> {
        match self {
            Self::Identity => None,
            Self::Gzip => Some("gzip"),
            Self::Brotli => Some("br"),
        }
    }

    /// The suffix a precompressed sibling in this coding carries, `.gz` or
    /// `.br`; `None` for identity. Callers strip it to recover the name the
    /// URL named.
    #[must_use]
    pub fn sibling_suffix(self) -> Option<&'static str> {
        match self {
            Self::Identity => None,
            Self::Gzip => Some(".gz"),
            Self::Brotli => Some(".br"),
        }
    }
}

/// What a request path resolves to inside one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaTarget {
    /// A real file. `file` is what to read; `encoding` says whether those bytes
    /// are already compressed.
    Asset {
        /// The file to read, already proven to be a file inside `dist_root`.
        file: PathBuf,
        /// What the bytes in `file` are.
        encoding: ContentEncoding,
    },
    /// Not a file and not under `assets/`: serve the shell, let the router in
    /// the bundle resolve the path. `file` is the root's `index.html`.
    IndexFallback { file: PathBuf },
    /// A 404: a path that resolves to nothing, a traversal, an `assets/` miss,
    /// or any request at all when the root holds no build.
    NotFound,
}

/// The `Cache-Control` value for a served path, relative to the build root.
///
/// The four cases from `spa.ts:117-148` and the reason each exists: the shell
/// is revalidated on every load (a deploy that changes it would otherwise be
/// invisible to a returning browser; the pairing token rides in the URL
/// fragment, which no cache stores), a
/// content-hashed `assets/` name is immutable by construction, the four woff2
/// faces are stable-named and large enough that `no-cache` revalidates all of
/// them on every cold load, and every other stable name — icons, the
/// manifest, wasm — must revalidate so swapping it lands. The directory is read
/// as a path component, so a Windows `assets\app.js` is the same name.
#[must_use]
pub fn cache_control_for(rel: &str) -> &'static str {
    let mut parts = Path::new(rel).components();
    let top = match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(top)), Some(_)) => top.to_str(),
        _ => None,
    };
    if rel == INDEX_NAME {
        "no-cache"
    } else if top == Some("assets") {
        "public, max-age=31536000, immutable"
    } else if top == Some("fonts") {
        "public, max-age=604800"
    } else {
        "no-cache"
    }
}

/// The `Content-Type` for a path, or `application/octet-stream`.
///
/// `spa.ts:17-30` verbatim. An unknown extension is deliberately the generic
/// type rather than a guess: a wrong `text/*` on an image is a
/// content-confusion bug, and a missing one is only a download.
#[must_use]
pub fn content_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Whether a build may compress this path, the fixed extension set. `wasm`
/// is in it because the bundle's wasm is the largest file a cold load fetches
/// and compresses to under a third of its size.
#[must_use]
pub fn is_compressible(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some("js")
            | Some("mjs")
            | Some("css")
            | Some("html")
            | Some("json")
            | Some("svg")
            | Some("map")
            | Some("txt")
            | Some("webmanifest")
            | Some("wasm")
    )
}

/// Whether an `Accept-Encoding` value admits gzip.
///
/// Ported from `spa.ts:68-82` with its shape intact, because two of its cases
/// are load-bearing and easy to lose to a tidier rewrite: the FIRST `gzip`
/// item decides (a later one cannot re-enable it), and `gzip;q=0` disables it,
/// which is what a client sending a *list* means by refusing.
#[must_use]
pub fn accepts_gzip(accept_encoding: &str) -> bool {
    accepts(accept_encoding, "gzip")
}

/// Whether an `Accept-Encoding` value admits brotli, by the same rules as
/// [`accepts_gzip`]: the first `br` item decides and `br;q=0` refuses.
#[must_use]
pub fn accepts_brotli(accept_encoding: &str) -> bool {
    accepts(accept_encoding, "br")
}

/// The one `Accept-Encoding` reading both codings share.
fn accepts(accept_encoding: &str, wanted: &str) -> bool {
    let mut wildcard = false;
    for item in accept_encoding.split(',') {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let mut accepted = true;
        for parameter in parts {
            if let Some(weight) = parameter
                .trim()
                .strip_prefix("q")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
            {
                accepted = weight.trim().parse::<f64>().is_ok_and(|q| q > 0.0);
            }
        }
        if coding == wanted {
            return accepted;
        }
        if coding == "*" {
            wildcard = accepted;
        }
    }
    wildcard
}

/// The directory whose `index.html` is servable, or `None` when this path holds
/// no usable build.
///
/// The single existence check every caller shares, and the answer a boot-time
/// log line reports: a missing build otherwise presents only as a 404 on every
/// page, which reads like an edge or DNS fault.
#[must_use]
pub fn resolve_disk_spa_root(web_dist_path: Option<&Path>) -> Option<PathBuf> {
    let root = web_dist_path?;
    let index = root.join(INDEX_NAME);
    index.is_file().then(|| root.to_path_buf())
}

/// Resolve one request path against one build. See the module header for the
/// rules and [`resolve_disk_spa_root`] for the "is there a build at all" gate
/// this repeats per request.
#[must_use]
pub fn resolve(dist_root: &Path, request_path: &str, accept_encoding: &str) -> SpaTarget {
    let index = dist_root.join(INDEX_NAME);
    if !index.is_file() {
        return SpaTarget::NotFound;
    }
    let relative = request_path.trim_start_matches('/');
    if reaches_outside(relative) {
        return SpaTarget::NotFound;
    }
    if !relative.is_empty()
        && let Some(file) = contained_file(dist_root, relative)
    {
        return asset(dist_root, file, accept_encoding);
    }
    if relative.starts_with(ASSETS_PREFIX) {
        return SpaTarget::NotFound;
    }
    SpaTarget::IndexFallback { file: index }
}

/// Whether a request path was reaching for something outside the build.
///
/// Refused OUTRIGHT rather than left to the deep-link rule, and that is the one
/// place this resolver is stricter than the source it is ported from: `..` is
/// never a client route, so
/// answering the shell for it would turn a probe that was reaching outward into
/// a 200. The request path is never percent-decoded, so `%2e%2e` is a filename
/// rather than a traversal and finds nothing.
fn reaches_outside(relative: &str) -> bool {
    relative
        .split('/')
        .any(|segment| segment == ".." || segment.contains('\\') || segment.contains('\0'))
}

/// The build's shell.
const INDEX_NAME: &str = "index.html";

/// The one directory whose misses are 404s rather than the shell.
const ASSETS_PREFIX: &str = "assets/";

/// The file `relative` names inside `root`, or `None` for a segment that names
/// no directory, a directory, and anything that is not a file.
fn contained_file(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut walked = root.to_path_buf();
    for segment in relative.split('/') {
        // A traversal is already refused by `reaches_outside`, before the
        // filesystem ever saw the path.
        if segment.is_empty() || segment == "." {
            return None;
        }
        walked.push(segment);
    }
    walked.is_file().then_some(walked)
}

/// The asset answer for a file already proven to be inside the root, preferring
/// a precompressed sibling when the client admits one: brotli first, because
/// it is the smaller body, then gzip.
fn asset(root: &Path, file: PathBuf, accept_encoding: &str) -> SpaTarget {
    if is_compressible(&file) {
        let admitted = [
            (ContentEncoding::Brotli, accepts_brotli(accept_encoding)),
            (ContentEncoding::Gzip, accepts_gzip(accept_encoding)),
        ];
        for (encoding, accepted) in admitted {
            if accepted && let Some(sibling) = sibling(root, &file, encoding) {
                return SpaTarget::Asset {
                    file: sibling,
                    encoding,
                };
            }
        }
    }
    SpaTarget::Asset {
        file,
        encoding: ContentEncoding::Identity,
    }
}

/// The precompressed sibling of `file` in `encoding`, when it is a file inside
/// `root`.
fn sibling(root: &Path, file: &Path, encoding: ContentEncoding) -> Option<PathBuf> {
    let mut name = file.as_os_str().to_owned();
    name.push(encoding.sibling_suffix()?);
    let sibling = PathBuf::from(name);
    (sibling.starts_with(root) && sibling.is_file()).then_some(sibling)
}

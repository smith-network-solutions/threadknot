//! Project file browsing + viewing (fs.tree / fs.read over WS, raw bytes via
//! GET /file). Scoped to a project root with symlink-safe path confinement.

use crate::server::ServerState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};

/// Excluded directory *names* (matched anywhere in the tree). Dotfiles and
/// gitignored files are deliberately included — users want to see generated
/// artifacts, lockfiles, `.env.example`, etc.
const EXCLUDED_DIRS: [&str; 3] = [".git", "node_modules", "target"];

/// Hard cap on tree entries. BFS ordering means the deepest/last entries get
/// trimmed rather than whole top-level branches.
const MAX_ENTRIES: usize = 25_000;

/// Read window for `fs.read` — 1 MiB. Larger files come back `truncated`.
const READ_CAP: u64 = 1024 * 1024;

/// `fs.tree` — full recursive listing of the project (capped, BFS).
/// Payload: `{ projectId }` →
/// `{ root, entries: [{ path, kind: "file"|"dir", sizeBytes? }], truncated }`
pub fn tree(state: &ServerState, payload: &Value) -> anyhow::Result<Value> {
    let project_id = payload
        .get("projectId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing field: projectId"))?;
    let project = state
        .hub
        .store
        .project(project_id)
        .ok_or_else(|| anyhow::anyhow!("unknown project"))?;
    let root = PathBuf::from(&project.path);

    let mut entries: Vec<Value> = Vec::new();
    let mut truncated = false;
    // BFS over directories. `queue` holds (absolute_dir, relative_prefix).
    let mut queue: VecDeque<(PathBuf, String)> = VecDeque::new();
    queue.push_back((root.clone(), String::new()));

    'bfs: while let Some((dir, prefix)) = queue.pop_front() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Deterministic order (case-insensitive by name).
        let mut children: Vec<std::fs::DirEntry> = read_dir.flatten().collect();
        children.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());

        for entry in children {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `file_type()` reads the dir entry directly and does NOT follow
            // symlinks, so a symlinked directory reports `is_symlink()`.
            let Ok(ft) = entry.file_type() else { continue };
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };

            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break 'bfs;
            }

            if ft.is_symlink() {
                // Never follow symlinks. Surface them as files (no size); the
                // read path confines them again so an escaping link can't leak.
                entries.push(json!({ "path": rel, "kind": "file" }));
            } else if ft.is_dir() {
                if EXCLUDED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                entries.push(json!({ "path": rel, "kind": "dir" }));
                queue.push_back((entry.path(), rel));
            } else {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                entries.push(json!({ "path": rel, "kind": "file", "sizeBytes": size }));
            }
        }
    }

    Ok(json!({
        "root": project.path,
        "entries": entries,
        "truncated": truncated,
    }))
}

/// `fs.read` — UTF-8 text contents, 1 MiB cap.
/// Payload: `{ projectId, path }` →
/// `{ path, contents, byteLength, truncated, binary }`
pub fn read(state: &ServerState, payload: &Value) -> anyhow::Result<Value> {
    let project_id = payload
        .get("projectId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing field: projectId"))?;
    let rel = payload
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing field: path"))?;
    let project = state
        .hub
        .store
        .project(project_id)
        .ok_or_else(|| anyhow::anyhow!("unknown project"))?;

    let target = confine(Path::new(&project.path), rel)?;
    let meta = std::fs::metadata(&target)?;
    anyhow::ensure!(meta.is_file(), "not a file");
    let byte_length = meta.len();

    // Read at most READ_CAP bytes for the text window.
    use std::io::Read;
    let mut file = std::fs::File::open(&target)?;
    let mut window: Vec<u8> = Vec::new();
    file.by_ref().take(READ_CAP).read_to_end(&mut window)?;

    let binary = window.contains(&0);
    let truncated = byte_length > window.len() as u64;
    let contents = if binary {
        String::new()
    } else {
        String::from_utf8_lossy(&window).into_owned()
    };

    Ok(json!({
        "path": rel,
        "contents": contents,
        "byteLength": byte_length,
        "truncated": truncated,
        "binary": binary,
    }))
}

/// `GET /file?project=…&path=…&token=…[&download=1]` — raw bytes for
/// images/PDF/binary viewing and downloads.
pub async fn file_handler(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let principal = match crate::server::authorize_bytes(
        &state,
        &headers,
        &params,
        crate::mobile::Capability::Files,
    ) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    // Remote project: stream the bytes from the owning machine (mesh-gated).
    if let Some(resp) = crate::server::maybe_proxy_bytes(&state, &principal, "/file", &params).await
    {
        return resp;
    }
    let (Some(project_id), Some(rel)) = (params.get("project"), params.get("path")) else {
        return (StatusCode::BAD_REQUEST, "missing params").into_response();
    };
    let Some(project) = state.hub.store.project(project_id) else {
        return (StatusCode::NOT_FOUND, "unknown project").into_response();
    };
    let target = match confine(Path::new(&project.path), rel) {
        Ok(p) => p,
        Err(_) => return (StatusCode::FORBIDDEN, "forbidden path").into_response(),
    };
    let Ok(file) = std::fs::File::open(&target) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // A directory opens successfully on Unix and only fails on the first read, so
    // the kind is checked here rather than surfacing as a truncated body.
    let Ok(meta) = file.metadata().map(|m| m.is_file().then_some(m.len())) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Some(total) = meta else {
        return (StatusCode::NOT_FOUND, "not a file").into_response();
    };

    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = mime_for_ext(&ext);
    let filename = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Set explicitly because a streamed body has no length of its own, and
        // without it a download shows no progress and no total size.
        .header(header::CONTENT_LENGTH, total);

    if params.get("download").map(String::as_str) == Some("1") {
        resp = resp.header(header::CONTENT_DISPOSITION, content_disposition(&filename));
    }

    match resp.body(stream_range(file, 0, total)) {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "response error").into_response(),
    }
}

/// A file (or a byte range of one) as a response body that never holds more than
/// one chunk in memory.
///
/// `/file`, `/attachment` and `/artifact-file` used to `std::fs::read` the whole
/// thing, so serving a 4 GB recording allocated 4 GB on the machine the owner is
/// sitting at, and N concurrent requests allocated N times that (SEC-014).
/// Streaming is the fix rather than a size cap: `Body::from_stream` is polled by
/// hyper as it writes, so a slow client stalls this stream instead of
/// accumulating anything, and no legitimate download has to be refused for being
/// large. That backpressure is also what makes the streaming path safe on the
/// peer byte proxy, which already streamed.
///
/// `len` is a byte count from `start`; the stream ends early if the file is
/// shorter (it can be replaced mid-read, and truncating the body is better than
/// hanging on a length that no longer exists).
pub fn stream_range(file: std::fs::File, start: u64, len: u64) -> axum::body::Body {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    struct Cursor {
        file: tokio::fs::File,
        seek_to: Option<u64>,
        remaining: u64,
    }

    let cursor = Cursor {
        file: tokio::fs::File::from_std(file),
        seek_to: (start > 0).then_some(start),
        remaining: len,
    };
    axum::body::Body::from_stream(futures::stream::unfold(
        Some(cursor),
        |state| async move {
            let mut cursor = state?;
            if cursor.remaining == 0 {
                return None;
            }
            if let Some(offset) = cursor.seek_to.take() {
                if let Err(e) = cursor.file.seek(std::io::SeekFrom::Start(offset)).await {
                    return Some((Err(e), None));
                }
            }
            let want = cursor
                .remaining
                .min(crate::limits::FILE_STREAM_CHUNK as u64) as usize;
            let mut buf = vec![0u8; want];
            match cursor.file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    cursor.remaining -= n as u64;
                    Some((Ok(buf), Some(cursor)))
                }
                Err(e) => Some((Err(e), None)),
            }
        },
    ))
}

/// Resolve `rel` against `root` with defense-in-depth path confinement:
///  1. reject empty / absolute paths and any `.` prefix / `..` traversal,
///  2. canonicalize both root and target and re-verify containment (catches
///     symlink escapes that survive the lexical check).
///
/// Returns the canonicalized, confirmed-inside-root target path.
pub(crate) fn confine(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!rel.is_empty(), "empty path");
    let rel_path = Path::new(rel);
    anyhow::ensure!(!rel_path.is_absolute(), "absolute path rejected");
    for comp in rel_path.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            // ParentDir / RootDir / Prefix are all traversal vectors.
            _ => anyhow::bail!("path traversal rejected"),
        }
    }

    let joined = root.join(rel_path);
    let canon_root = root
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("bad project root: {e}"))?;
    let canon_target = joined
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("no such path"))?;
    anyhow::ensure!(
        canon_target.starts_with(&canon_root),
        "path escapes project root"
    );
    Ok(canon_target)
}

/// Extension → MIME type for `GET /file`. Covers the common web/image/pdf/text
/// types the viewer renders inline; everything else is served as an opaque
/// download. Kept local to this module (do not extend store.rs).
fn mime_for_ext(ext: &str) -> HeaderValue {
    let mime = match ext {
        // text / code
        "txt" | "text" | "log" => "text/plain; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "yaml" | "yml" => "text/plain; charset=utf-8",
        "toml" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        // images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        // video — the Files tab plays these inline, so the type has to be right
        // or the <video> element refuses the source.
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        // documents
        "pdf" => "application/pdf",
        // fonts
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(mime)
}

/// Build a `Content-Disposition: attachment` value with an RFC 5987 encoded
/// filename plus a sanitized ASCII fallback for legacy clients.
pub fn content_disposition(filename: &str) -> String {
    // ASCII fallback: drop quotes/backslashes/control chars, replace non-ASCII.
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_control() || c == '"' || c == '\\' {
                '_'
            } else if c.is_ascii() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii = if ascii.trim().is_empty() {
        "download".to_string()
    } else {
        ascii
    };
    let encoded = rfc5987_encode(filename);
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Percent-encode `s` per RFC 5987 (`attr-char` stays literal; everything else,
/// including all non-ASCII bytes, is `%HH` encoded).
fn rfc5987_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            );
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

//! Local dev-server discovery for the in-app browser's empty state
//! (t3code PortScanner pattern): probe common dev ports on localhost, and for
//! the ones that accept a connection, do a best-effort `GET /` to pull the
//! page `<title>`.
//!
//! Phones can't reach `localhost` (that's the phone itself), so the returned
//! `url` is built from this machine's LAN IP — same source as `server::lan_url`.

use crate::server::ServerState;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// TCP connect timeout per candidate port.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
/// Deadline for the whole probe-and-read of one open port.
const READ_TIMEOUT: Duration = Duration::from_millis(600);
/// Cap the amount of the HTTP response we buffer while hunting for `<title>`.
const MAX_BODY: usize = 64 * 1024;

/// Common dev-server ports, roughly the t3code set plus threadknot-relevant ones.
fn candidate_ports() -> Vec<u16> {
    let mut ports: Vec<u16> = Vec::new();
    ports.extend(3000..=3010); // Next.js / Node / CRA range
    ports.extend([4200, 4321, 5000]); // Angular, Astro, Flask/misc
    ports.extend(5173..=5180); // Vite range
    ports.extend([5733]); // t3code web
    ports.extend([6006]); // Storybook
    ports.extend([8000]); // Django / http.server
    ports.extend(8080..=8090); // generic http / webpack
    ports.extend([8788]); // wrangler / cloudflare
    ports.extend([9000]); // php / generic
    ports.extend([1420]); // Tauri (devdock) vite
    ports.extend([13773]); // t3code desktop server
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Resolve this machine's LAN IP so the emitted URLs work from a phone.
/// Falls back to `localhost` (fine for the desktop webview).
fn host_for_urls() -> String {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "localhost".into())
}

/// `ports.scan` — `{ ports: [{ port, url, title? }] }`, sorted by port.
pub async fn scan(state: &ServerState) -> Value {
    let own_port = state.config.port;
    let host = host_for_urls();

    let probes = candidate_ports()
        .into_iter()
        .filter(|p| *p != own_port)
        .map(|port| async move { probe_port(port).await.map(|title| (port, title)) });

    let results = futures_util::future::join_all(probes).await;

    let mut ports: Vec<Value> = results
        .into_iter()
        .flatten()
        .map(|(port, title)| {
            let mut obj = json!({
                "port": port,
                "url": format!("http://{host}:{port}"),
            });
            if let Some(title) = title {
                obj["title"] = Value::String(title);
            }
            obj
        })
        .collect();

    ports.sort_by_key(|v| v["port"].as_u64().unwrap_or(0));
    json!({ "ports": ports })
}

/// Try to connect to `127.0.0.1:port`. Returns `Some(title_opt)` when the port
/// accepts a connection (a running server); `None` when nothing is listening.
async fn probe_port(port: u16) -> Option<Option<String>> {
    let addr = ("127.0.0.1", port);
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        _ => return None,
    };

    // It's listening — best-effort GET for the <title>, but never let a slow or
    // non-HTTP server hide a live port: on any read error we still report it.
    let title = tokio::time::timeout(READ_TIMEOUT, fetch_title(stream, port))
        .await
        .ok()
        .flatten();
    Some(title)
}

/// Hand-rolled `GET /` over the socket; extract `<title>…</title>` if the
/// response looks like HTML. No HTTP crate needed.
async fn fetch_title(mut stream: TcpStream, port: u16) -> Option<String> {
    let req = format!(
        "GET / HTTP/1.1\r\nHost: localhost:{port}\r\nUser-Agent: threadknot\r\n\
         Accept: text/html\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.ok()?;

    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() >= MAX_BODY {
            break;
        }
        // Stop early once we've seen the closing </title> (or head).
        if find_ci(&buf, b"</title>").is_some() || find_ci(&buf, b"</head>").is_some() {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    extract_title(&text)
}

/// Case-insensitive substring search over bytes.
fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w.iter().zip(needle).all(|(a, b)| a.eq_ignore_ascii_case(b)))
}

/// Pull the trimmed text of the first `<title>…</title>`, if present.
fn extract_title(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    let open = find_ci(bytes, b"<title")?;
    // Skip past the '>' that closes the opening tag (handles attributes).
    let after_open = open + html[open..].find('>')? + 1;
    let close_rel = find_ci(&bytes[after_open..], b"</title>")?;
    let raw = &html[after_open..after_open + close_rel];
    let title = decode_entities(raw.trim());
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(120).collect())
    }
}

/// Decode the handful of HTML entities that show up in titles.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

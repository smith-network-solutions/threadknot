//! Axum server: serves the web UI, exposes /ws (token-gated) on the LAN so a phone
//! browser can drive agent sessions, and fans out hub events to every client.

use crate::agents::{claude, codex, kimi, Hub};
use crate::mobile::{MobileStore, Principal};
use crate::protocol::*;
use crate::store::ServerConfig;
use anyhow::Context as _;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use axum::routing::{any, get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ServerState {
    pub hub: Arc<Hub>,
    pub config: ServerConfig,
    pub device: Arc<crate::device::Device>,
    pub peernet: Arc<crate::peernet::PeerNet>,
    pub lan_url: String,
    pub agents_cache: Arc<RwLock<Option<Vec<AgentInfo>>>>,
    pub terms: Arc<crate::term::TermRegistry>,
    pub browsers: Arc<crate::browser::BrowserRegistry>,
    pub browser_profiles: Arc<crate::browser_profiles::BrowserProfileStore>,
    pub mobile: Arc<MobileStore>,
    pub dictation: Arc<crate::dictation::Dictation>,
}

impl ServerState {
    /// Resolve a presented token to a principal: the master token (server.json)
    /// or a paired mobile device's revocable credential.
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        if token == self.config.token {
            return Some(Principal::Master);
        }
        self.mobile.authenticate(token).map(Principal::Device)
    }

    /// Human-readable server name shown in the mobile app's server list.
    pub fn server_name(&self) -> String {
        gethostname::gethostname().to_string_lossy().into_owned()
    }
}

pub fn lan_url(port: u16, token: &str) -> String {
    let ip = local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".into());
    format!("http://{ip}:{port}/?token={token}")
}

fn resolve_dist() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("THREADKNOT_DIST") {
        let p = PathBuf::from(p);
        if p.join("index.html").exists() {
            return Some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("dist"));
        candidates.push(cwd.join("../dist"));
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..5 {
            if let Some(d) = dir {
                candidates.push(d.join("dist"));
                dir = d.parent().map(|p| p.to_path_buf());
            } else {
                break;
            }
        }
    }
    candidates.into_iter().find(|c| c.join("index.html").exists())
}

pub async fn run(state: ServerState) {
    // Expo push dispatcher for paired phones (spawned here — needs the runtime).
    state.hub.attach_push(crate::push::PushService::spawn(
        Arc::clone(&state.mobile),
        state.config.server_id.clone(),
    ));

    // Probe agent availability/models in the background so `hello` is fast.
    {
        let cache = Arc::clone(&state.agents_cache);
        let hermes = Arc::clone(&state.hub.hermes);
        let claudex = Arc::clone(&state.hub.claudex);
        tokio::spawn(async move {
            let info = build_agents_info(&hermes, &claudex).await;
            *cache.write().await = Some(info);
        });
    }

    // Background subscription-usage poller (sidebar meter).
    crate::usage::spawn_poller(Arc::clone(&state.hub));

    // The live-presence poller for registered Hermes gateways is spawned only
    // after the port bind succeeds (below), so a second app instance that
    // fails to bind never leaves a detached task probing gateways forever.

    // Background scheduled-runs loop (fires recurring agent turns).
    crate::schedules::spawn_scheduler(Arc::clone(&state.hub));

    // Background "is a newer master out?" poller (pulses the settings gear).
    crate::update::spawn_poller(Arc::clone(&state.hub));

    // Mesh runtime: peer sockets + presence + announce + mDNS discovery.
    state.peernet.start();

    let mut app = Router::new()
        .route("/ws", any(ws_handler))
        .route("/attachment", get(attachment_handler))
        .route("/artifact-file", get(artifact_file_handler))
        .route("/file", get(crate::files::file_handler))
        .route("/term", any(crate::term::ws_handler))
        .route("/browser", any(crate::browser::ws_handler))
        .route("/mcp", any(crate::mcp::mcp_handler))
        .route("/api/server-info", get(server_info_handler))
        .route("/api/peer/pair", post(peer_pair_handler))
        .route("/api/mobile/pair", post(mobile_pair_handler))
        .route("/api/mobile/push", post(mobile_push_handler))
        .route("/api/mobile/push/test", post(mobile_push_test_handler))
        .route("/api/mobile/unpair", post(mobile_unpair_handler));

    if let Some(dist) = resolve_dist() {
        let index = dist.join("index.html");
        let serve = tower_http::services::ServeDir::new(&dist)
            .fallback(tower_http::services::ServeFile::new(index));
        // The bundle is ~1.3 MB of JS/CSS and compresses ~5x. Uncompressed it
        // dominates first paint on a phone or over a tunnel, so gzip/br it —
        // scoped to the static service, well clear of the WebSocket upgrade.
        let serve = tower::ServiceBuilder::new()
            .layer(tower_http::compression::CompressionLayer::new())
            .service(serve);
        app = app.fallback_service(serve);
    } else {
        app = app.fallback(get(|| async {
            Html(
                "<h1>Threadknot</h1><p>Web UI not built yet — run <code>npm run build</code> in the \
                 threadknot directory, then reload.</p>",
            )
        }));
    }

    let app = app
        .layer(axum::middleware::from_fn(cache_policy))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state.clone());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], state.config.port));
    tracing::info!("threadknot server on {addr} — LAN: {}", state.lan_url);
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            // Only the process that actually owns Threadknot's port may recover
            // persisted in-flight turns. Doing this before bind would let an
            // accidental second launch interrupt or duplicate live work.
            state.hub.recover_orphaned_threads();
            // Tie the presence poller to this server's lifetime: spawned now
            // that the bind owns the port, and aborted when `serve` returns so
            // a dead process never keeps probing gateways.
            let poller = crate::hermes::spawn_status_poller(Arc::clone(&state.hub));
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("server error: {e}");
            }
            poller.abort();
        }
        Err(e) => tracing::error!("cannot bind {addr}: {e}"),
    }
}

/// Cache policy for the bundled web UI.
///
/// Vite fingerprints everything under `/assets/`, so those files are safe to
/// cache forever. `index.html` is the opposite: it names the current bundle, and
/// `ServeDir` sends it with only `Last-Modified` and no `Cache-Control` at all.
/// A browser left to its own heuristics then treats the shell as fresh — iOS
/// Safari especially — so a phone on the LAN keeps booting a stale bundle and
/// never picks up a rebuilt frontend, even across an app restart. `no-cache`
/// means "revalidate", not "don't store", so the common case is still a cheap
/// 304 against the validator `ServeDir` already sends.
///
/// Only fills the header in when a handler has not set its own.
async fn cache_policy(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{header, HeaderValue};
    let immutable = req.uri().path().starts_with("/assets/");
    let mut resp = next.run(req).await;
    if !resp.headers().contains_key(header::CACHE_CONTROL) {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if immutable {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            }),
        );
    }
    resp
}

/// Stream a token-gated byte endpoint (`/file`, `/attachment`,
/// `/artifact-file`) from a PEER machine through this server. Clients only
/// ever hold THIS server's token; the peer's master token stays server-side
/// and is attached here. The caller must have authenticated the client
/// already.
pub(crate) async fn proxy_peer_bytes(
    state: &ServerState,
    machine_id: &str,
    endpoint: &str,
    params: &HashMap<String, String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let Some(peer) = state.peernet.registry.peer(machine_id) else {
        return (StatusCode::NOT_FOUND, "unknown machine").into_response();
    };
    let Some(addr) = peer
        .last_good_address
        .clone()
        .or_else(|| peer.addresses.first().cloned())
    else {
        return (StatusCode::BAD_GATEWAY, "no known address for that machine").into_response();
    };
    let query: Vec<(String, String)> = params
        .iter()
        .filter(|(k, _)| k.as_str() != "token" && k.as_str() != "machineId")
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain(std::iter::once(("token".to_string(), peer.token.clone())))
        .collect();
    let url = format!(
        "http://{}{endpoint}",
        crate::peernet::host_port(&addr, peer.port)
    );
    let resp = match crate::hermes::http_client()
        .get(&url)
        .query(&query)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("peer fetch failed: {e}")).into_response()
        }
    };
    let status =
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = axum::response::Response::builder().status(status);
    for name in ["content-type", "content-disposition", "content-length", "x-content-type-options"] {
        if let Some(v) = resp.headers().get(name) {
            if let Ok(v) = axum::http::HeaderValue::from_bytes(v.as_bytes()) {
                builder = builder.header(name, v);
            }
        }
    }
    match builder.body(axum::body::Body::from_stream(resp.bytes_stream())) {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "proxy response error").into_response(),
    }
}

/// If the request names a peer machine, stream it from there instead of
/// serving locally. Returns None when the request is local.
async fn maybe_proxy_bytes(
    state: &ServerState,
    endpoint: &str,
    params: &HashMap<String, String>,
) -> Option<axum::response::Response> {
    let mid = params.get("machineId")?;
    if mid == &state.device.machine_id {
        return None;
    }
    Some(proxy_peer_bytes(state, mid, endpoint, params).await)
}

/// Serve a stored attachment (token-gated) so the transcript can render
/// thumbnails on reload. `GET /attachment?thread=…&id=…&token=…`.
async fn attachment_handler(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    if state
        .authenticate(params.get("token").map(String::as_str).unwrap_or(""))
        .is_none()
    {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    if let Some(resp) = maybe_proxy_bytes(&state, "/attachment", &params).await {
        return resp;
    }
    let (Some(thread), Some(id)) = (params.get("thread"), params.get("id")) else {
        return (StatusCode::BAD_REQUEST, "missing params").into_response();
    };
    let Some(path) = state.hub.store.attachment_path(thread, id) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    (
        [(axum::http::header::CONTENT_TYPE, crate::store::mime_for_ext(ext))],
        bytes,
    )
        .into_response()
}

/// Serve a produced artifact's durable snapshot (token-gated). Unlike `/file`,
/// this reads the copy under `~/.threadknot/artifacts/` so it survives the
/// working-tree file being moved or deleted. `GET /artifact-file?id=…&token=…[&download=1]`.
async fn artifact_file_handler(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::http::{header, HeaderValue, StatusCode};
    if state
        .authenticate(params.get("token").map(String::as_str).unwrap_or(""))
        .is_none()
    {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    if let Some(resp) = maybe_proxy_bytes(&state, "/artifact-file", &params).await {
        return resp;
    }
    let Some(id) = params.get("id") else {
        return (StatusCode::BAD_REQUEST, "missing params").into_response();
    };
    let Some(rec) = state.hub.store.artifact_by_id(id) else {
        return (StatusCode::NOT_FOUND, "unknown artifact").into_response();
    };
    let Some(path) = state.hub.store.artifact_snapshot_path(&rec.thread_id, id) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // Records written before a type was recognized carry the octet-stream
    // fallback, and a <video> element refuses to play that. Re-derive from the
    // stored extension so existing artifacts start working without a migration.
    let resolved = if rec.mime_type == "application/octet-stream" {
        crate::artifacts::deliverable_mime(&crate::artifacts::ext_of(std::path::Path::new(
            &rec.rel_path,
        )))
        .unwrap_or("application/octet-stream")
        .to_string()
    } else {
        rec.mime_type.clone()
    };
    let mime = HeaderValue::from_str(&resolved)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));

    let download = params.get("download").map(String::as_str) == Some("1");
    let disposition = crate::files::content_disposition(&crate::protocol::artifact_file_name(&rec));

    // Range support exists for video: without it a <video> element cannot seek,
    // so scrubbing a recorded walkthrough silently does nothing.
    let total = bytes.len() as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        // Query form so the same URL still seeks through the peer byte proxy,
        // which forwards the query string but not request headers.
        .or_else(|| params.get("range").cloned())
        .and_then(|raw| parse_byte_range(&raw, total));

    let mut resp = axum::response::Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::ACCEPT_RANGES, "bytes");
    if download {
        resp = resp.header(header::CONTENT_DISPOSITION, disposition);
    }

    let (status, body) = match range {
        Some((start, end)) => {
            resp = resp.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            );
            (
                StatusCode::PARTIAL_CONTENT,
                bytes[start as usize..=end as usize].to_vec(),
            )
        }
        None => (StatusCode::OK, bytes),
    };
    match resp.status(status).body(axum::body::Body::from(body)) {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "response error").into_response(),
    }
}

/// Parse a single-range `bytes=start-end` value against a known total length.
/// Multi-range requests are not supported; returning `None` makes the caller
/// serve the whole body, which is a legal response to any range request.
fn parse_byte_range(raw: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = raw.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let (start, end) = match (start_s.trim(), end_s.trim()) {
        // "bytes=-N" — the trailing N bytes.
        ("", last) => {
            let n: u64 = last.parse().ok()?;
            (total.saturating_sub(n.min(total)), total - 1)
        }
        // "bytes=N-" — from N to the end.
        (first, "") => (first.parse().ok()?, total - 1),
        (first, last) => (first.parse().ok()?, last.parse::<u64>().ok()?.min(total - 1)),
    };
    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

/// The Hermes picker entry is built from the registry, not a network probe:
/// each registered gateway appears as a "model" (the composer's second
/// dropdown), so choosing an agent kind + specific Hermes agent reuses the
/// exact picker flow claude/codex already have.
fn hermes_agents_info(registry: &crate::hermes::HermesRegistry) -> AgentInfo {
    let agents = registry.list();
    let models: Vec<ModelInfo> = agents
        .iter()
        .map(|a| ModelInfo {
            id: a.id.clone(),
            name: a.name.clone(),
            image: a.image.clone(),
            supports_wide_context: None,
            fixed_context_window: None,
            efforts: None,
            default_effort: None,
        })
        .collect();
    AgentInfo {
        id: Agent::Hermes,
        name: "Hermes".into(),
        available: !agents.is_empty(),
        auth_hint: agents
            .is_empty()
            .then(|| "Add a Hermes agent in Settings (gateway URL + API key)".into()),
        default_model: models.first().map(|m| m.id.clone()).unwrap_or_default(),
        models,
    }
}

/// Claudex is built from its registry the same way Hermes is: each profile is
/// a "model" in the composer's second dropdown, so picking the agent kind plus
/// a profile reuses the picker flow every other agent already has.
fn claudex_agents_info(registry: &crate::claudex::ClaudexRegistry) -> AgentInfo {
    let profiles = registry.list();
    let models: Vec<ModelInfo> = profiles
        .iter()
        .map(|p| ModelInfo {
            id: p.id.clone(),
            name: p.name.clone(),
            image: p.avatar.clone(),
            // The `[1m]` context toggle is Anthropic-only; a bridged profile
            // states its real upstream window instead.
            supports_wide_context: None,
            fixed_context_window: p.context_window,
            efforts: (!p.efforts.is_empty()).then(|| p.efforts.clone()),
            default_effort: p.default_effort.clone(),
        })
        .collect();
    AgentInfo {
        id: Agent::Claudex,
        name: "Claudex".into(),
        // The `claude` CLI is the harness either way, so a missing CLI makes
        // this unusable no matter how many profiles exist.
        available: !profiles.is_empty() && claude::probe().0,
        auth_hint: profiles
            .is_empty()
            .then(|| "Add a Claudex profile in Settings (bridge URL + model)".into()),
        default_model: models.first().map(|m| m.id.clone()).unwrap_or_default(),
        models,
    }
}

/// Swap a cached registry-backed entry after a change and tell clients to
/// re-fetch `hello` — the claude/codex probes are untouched (they're slow).
async fn refresh_agent_entry(state: &ServerState, entry: AgentInfo) {
    let mut cache = state.agents_cache.write().await;
    if let Some(list) = cache.as_mut() {
        match list.iter_mut().find(|a| a.id == entry.id) {
            Some(slot) => *slot = entry,
            None => list.push(entry),
        }
    }
    drop(cache);
    state.hub.broadcast_state("agents", None);
}

async fn refresh_hermes_agents(state: &ServerState) {
    let entry = hermes_agents_info(&state.hub.hermes);
    refresh_agent_entry(state, entry).await;
}

async fn refresh_claudex_agents(state: &ServerState) {
    let entry = claudex_agents_info(&state.hub.claudex);
    refresh_agent_entry(state, entry).await;
}

async fn build_agents_info(
    hermes: &crate::hermes::HermesRegistry,
    claudex: &crate::claudex::ClaudexRegistry,
) -> Vec<AgentInfo> {
    let (claude_ok, claude_hint) = claude::probe();
    let claude_info = AgentInfo {
        id: Agent::Claude,
        name: "Claude Code".into(),
        available: claude_ok,
        auth_hint: claude_hint,
        models: claude::builtin_models(),
        default_model: claude::DEFAULT_MODEL.into(),
    };

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    // Bound the Codex probe: spawning `codex app-server` (a node process) can be
    // slow to start, and a stalled handshake would otherwise block for the RPC's
    // full 120s timeout, keeping `hello` empty that whole time. On timeout we fall
    // back to a placeholder model so Codex still appears and is selectable.
    let codex_fallback = |hint: String| AgentInfo {
        id: Agent::Codex,
        name: "Codex".into(),
        available: false,
        auth_hint: Some(hint),
        models: vec![ModelInfo {
            id: "gpt-5.5".into(),
            name: "GPT-5.5".into(),
            image: None,
            supports_wide_context: None,
            fixed_context_window: None,
            efforts: None,
            default_effort: None,
        }],
        default_model: "gpt-5.5".into(),
    };
    let codex_probe = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        codex::probe(&home.to_string_lossy()),
    )
    .await;
    let codex_info = match codex_probe {
        Ok(Ok((authed, models, default_model))) => AgentInfo {
            id: Agent::Codex,
            name: "Codex".into(),
            available: authed,
            auth_hint: (!authed).then(|| "Codex is not authenticated — run `codex login`".into()),
            models,
            default_model,
        },
        Ok(Err(e)) => codex_fallback(format!("Codex unavailable: {e:#}")),
        Err(_) => codex_fallback("Codex probe timed out — is `codex` installed?".into()),
    };
    let kimi_fallback = |hint: String| AgentInfo {
        id: Agent::Kimi,
        name: "Kimi Code".into(),
        available: false,
        auth_hint: Some(hint),
        models: kimi::builtin_models(),
        default_model: kimi::DEFAULT_MODEL.into(),
    };
    let kimi_probe = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        kimi::probe(&home.to_string_lossy()),
    )
    .await;
    let kimi_info = match kimi_probe {
        Ok(Ok(true)) => AgentInfo {
            id: Agent::Kimi,
            name: "Kimi Code".into(),
            available: true,
            auth_hint: None,
            models: kimi::builtin_models(),
            default_model: kimi::DEFAULT_MODEL.into(),
        },
        Ok(Ok(false)) => kimi_fallback("Kimi is not authenticated — run `kimi login`".into()),
        Ok(Err(error)) => {
            let detail = format!("{error:#}");
            if detail.contains("not authenticated") || detail.contains("Authentication required") {
                kimi_fallback("Kimi is not authenticated — run `kimi login`".into())
            } else {
                kimi_fallback(format!("Kimi unavailable: {detail}"))
            }
        }
        Err(_) => kimi_fallback("Kimi probe timed out — is `kimi` installed?".into()),
    };
    vec![
        claude_info,
        codex_info,
        kimi_info,
        hermes_agents_info(hermes),
        claudex_agents_info(claudex),
    ]
}

/// Bearer credential from the Authorization header, falling back to a JSON
/// body field — RN fetch on the phone sends the header; curl smoke tests
/// often find the body easier.
fn bearer_or_field(headers: &axum::http::HeaderMap, body: &Value, key: &str) -> Option<String> {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = v.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                return Some(token.trim().to_string());
            }
        }
    }
    body.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// `GET /api/server-info?token=…` — identity probe used by the mobile app to
/// validate a pasted URL and detect URL changes for an already-paired server.
async fn server_info_handler(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let token = params.get("token").map(String::as_str).unwrap_or("");
    if state.authenticate(token).is_none() {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    axum::Json(json!({
        "app": "threadknot",
        "version": env!("THREADKNOT_VERSION"),
        "serverId": state.config.server_id,
        "name": state.server_name(),
    }))
    .into_response()
}

/// `POST /api/peer/pair` — the receiving half of mutual one-paste pairing.
/// The initiating Threadknot authenticates with OUR master token (the paste) and
/// sends its own identity INCLUDING its master token; we store it as a peer
/// and answer with our identity. After one call, both sides hold each
/// other's tokens: full mutual trust.
async fn peer_pair_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let Some(token) = bearer_or_field(&headers, &body, "token") else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };
    if state.authenticate(&token) != Some(Principal::Master) {
        return (StatusCode::UNAUTHORIZED, "pairing requires this server's master token")
            .into_response();
    }
    let mesh_version = body.get("meshVersion").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if mesh_version != crate::device::MESH_VERSION {
        return (
            StatusCode::CONFLICT,
            format!(
                "mesh version mismatch (theirs {mesh_version}, ours {}) — update Threadknot on the older machine",
                crate::device::MESH_VERSION
            ),
        )
            .into_response();
    }
    let get = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (machine_id, peer_token) = (get("machineId"), get("peerToken"));
    if machine_id.is_empty() || peer_token.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing machineId/peerToken").into_response();
    }
    if machine_id == state.device.machine_id {
        return (StatusCode::BAD_REQUEST, "a machine cannot pair with itself").into_response();
    }
    let addresses: Vec<String> = body
        .get("addresses")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let peer = crate::peers::Peer {
        machine_id,
        name: {
            let n = get("name");
            if n.is_empty() { "Threadknot".into() } else { n.chars().take(64).collect() }
        },
        // Appearance travels with the name; older Threadknots simply omit it.
        avatar: body.get("avatar").and_then(|v| v.as_str()).map(String::from),
        color: body.get("color").and_then(|v| v.as_str()).map(String::from),
        avatar_override: None,
        color_override: None,
        // Seed the LWW clock from the pairing frame if the peer sent one.
        profile_updated_at: body
            .get("profileUpdatedAt")
            .and_then(|v| v.as_str())
            .map(String::from),
        token: peer_token,
        port: body.get("port").and_then(|v| v.as_u64()).unwrap_or(42800) as u16,
        addresses,
        last_good_address: None,
        last_seen_at: None,
        added_at: now_iso(),
        mesh_version,
    };
    match state.peernet.registry.upsert(peer) {
        Ok(_) => {
            state.peernet.kick.notify_waiters();
            state.hub.broadcast_state("peers", None);
            axum::Json(json!({
                "machineId": state.device.machine_id,
                "name": state.device.friendly_name(),
                "avatar": state.device.avatar(),
                "color": state.device.color(),
                "profileUpdatedAt": state.device.profile_updated_at(),
                "port": state.config.port,
                "meshVersion": crate::device::MESH_VERSION,
                "addresses": crate::peernet::local_addresses(),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/mobile/pair` — master token in, revocable device credential out.
/// The phone stores only the device credential and discards the master token.
async fn mobile_pair_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let Some(token) = bearer_or_field(&headers, &body, "token") else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };
    if state.authenticate(&token) != Some(Principal::Master) {
        return (StatusCode::UNAUTHORIZED, "pairing requires the server's master token")
            .into_response();
    }
    let name = body
        .get("deviceName")
        .and_then(|v| v.as_str())
        .unwrap_or("Mobile device")
        .chars()
        .take(64)
        .collect::<String>();
    let platform = body
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .chars()
        .take(16)
        .collect::<String>();
    match state.mobile.pair(name, platform) {
        Ok((device, credential)) => {
            state.hub.broadcast_state("mobileDevices", None);
            axum::Json(json!({
                "serverId": state.config.server_id,
                "serverName": state.server_name(),
                "version": env!("THREADKNOT_VERSION"),
                "deviceId": device.id,
                "credential": credential,
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// Resolve the calling device from its credential (mobile-only endpoints).
fn device_from_request(
    state: &ServerState,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> Result<String, Box<axum::response::Response>> {
    use axum::http::StatusCode;
    let token = bearer_or_field(headers, body, "credential").ok_or_else(|| {
        Box::new((StatusCode::UNAUTHORIZED, "missing credential").into_response())
    })?;
    match state.authenticate(&token) {
        Some(Principal::Device(id)) => Ok(id),
        _ => Err(Box::new(
            (StatusCode::UNAUTHORIZED, "unknown or revoked device").into_response(),
        )),
    }
}

/// `POST /api/mobile/push` — register/refresh this device's Expo push token
/// and notification preferences.
async fn mobile_push_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let device_id = match device_from_request(&state, &headers, &body) {
        Ok(id) => id,
        Err(resp) => return *resp,
    };
    let expo = body
        .get("expoPushToken")
        .and_then(|v| v.as_str())
        .map(String::from);
    let enabled = body.get("notificationsEnabled").and_then(|v| v.as_bool());
    let errors = body.get("notifyErrors").and_then(|v| v.as_bool());
    let name = body.get("deviceName").and_then(|v| v.as_str()).map(String::from);
    let scope = body
        .get("notifyScope")
        .and_then(|v| v.as_str())
        .and_then(crate::mobile::NotifyScope::parse);
    // Absent means "leave it alone"; an empty array is a real instruction to
    // clear the list, so the two cases must stay distinguishable.
    let workspaces = body.get("notifyWorkspaces").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str())
            .map(String::from)
            .collect::<Vec<_>>()
    });
    match state.mobile.update(&device_id, |d| {
        if let Some(t) = expo {
            d.expo_push_token = if t.is_empty() { None } else { Some(t) };
        }
        if let Some(v) = enabled {
            d.notifications_enabled = v;
        }
        if let Some(v) = errors {
            d.notify_errors = v;
        }
        if let Some(v) = scope {
            d.notify_scope = v;
        }
        if let Some(v) = workspaces {
            d.notify_workspaces = v;
        }
        if let Some(n) = name {
            if !n.trim().is_empty() {
                d.name = n.chars().take(64).collect();
            }
        }
    }) {
        Ok(device) => {
            state.hub.broadcast_state("mobileDevices", None);
            axum::Json(json!({ "device": device })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/mobile/push/test` — round-trip proof: server → Expo → this phone.
async fn mobile_push_test_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let device_id = match device_from_request(&state, &headers, &body) {
        Ok(id) => id,
        Err(resp) => return *resp,
    };
    let Some(push) = state.hub.push() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "push service not running").into_response();
    };
    push.enqueue(crate::push::PushJob {
        kind: crate::push::PushKind::Test,
        project_id: String::new(),
        // `only_device` bypasses subscription filtering: a test must prove the
        // transport even when the device subscribes to nothing.
        workspace_id: String::new(),
        project_name: state.server_name(),
        thread_id: String::new(),
        thread_title: format!("Threadknot {}", env!("CARGO_PKG_VERSION")),
        only_device: Some(device_id),
    });
    axum::Json(json!({})).into_response()
}

/// `POST /api/mobile/unpair` — a device removes itself (Settings → remove server).
async fn mobile_unpair_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let device_id = match device_from_request(&state, &headers, &body) {
        Ok(id) => id,
        Err(resp) => return *resp,
    };
    match state.mobile.revoke(&device_id) {
        Ok(()) => {
            state.hub.broadcast_state("mobileDevices", None);
            axum::Json(json!({})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    let token = params.get("token").map(String::as_str).unwrap_or("");
    let Some(principal) = state.authenticate(token) else {
        return (axum::http::StatusCode::UNAUTHORIZED, "bad token").into_response();
    };
    ws.on_upgrade(move |socket| handle_socket(socket, state, principal))
}

/// How many requests one socket may have in flight at once. High enough that
/// the client's boot fan-out never queues, low enough that a wedged client
/// can't spawn unbounded work.
const MAX_INFLIGHT_REQUESTS: usize = 32;

async fn handle_socket(socket: WebSocket, state: ServerState, principal: Principal) {
    let (mut sink, mut stream) = socket.split();
    let mut events = state.hub.broadcast.subscribe();
    let mut relayed = state.hub.relay.subscribe();

    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // The thread this socket is currently displaying, learned from its
    // `thread.get` calls. Streaming deltas (seq < 0) for any OTHER thread are
    // discarded by the client anyway, so sending them is pure wasted bandwidth
    // — on a phone or a tunnel that firehose is what starves the response the
    // user is actually waiting on.
    let viewing: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let writer_viewing = viewing.clone();

    // Writer: multiplex responses, local broadcasts, and frames relayed from
    // peer machines (pre-serialized, origin-tagged) onto the socket.
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = out_rx.recv() => {
                    let Some(msg) = msg else { break };
                    if sink.send(Message::Text(msg.into())).await.is_err() { break; }
                }
                ev = events.recv() => {
                    match ev {
                        Ok(ev) => {
                            if let ServerMessage::Event { thread_id, seq, .. } = &ev {
                                // Persisted events (seq >= 0) always go out: they drive
                                // status, attention badges and notifications for threads
                                // the client isn't looking at. Deltas do not.
                                if *seq < 0
                                    && writer_viewing.lock().unwrap().as_deref()
                                        != Some(thread_id.as_str())
                                {
                                    continue;
                                }
                            }
                            if let Ok(text) = serde_json::to_string(&ev) {
                                if sink.send(Message::Text(text.into())).await.is_err() { break; }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                ev = relayed.recv() => {
                    match ev {
                        Ok(text) => {
                            if sink.send(Message::Text(text.into())).await.is_err() { break; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
    });

    // Requests run concurrently. Handling them inline used to serialize the
    // socket: one slow call (a git shell-out, a request routed to a sluggish
    // peer) stalled every request queued behind it, which is how opening a
    // thread could sit on "loading log…" for tens of seconds. The client
    // correlates responses by id and orders anything order-sensitive with its
    // own awaits, so out-of-order completion is safe.
    let inflight = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT_REQUESTS));
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(req) = serde_json::from_str::<ClientRequest>(&text) else {
            continue;
        };
        let id = req.id;
        // Note what this socket is watching before the fetch runs, so deltas
        // arriving during the replay aren't dropped.
        if req.kind == "thread.get" {
            if let Some(tid) = req.payload.get("threadId").and_then(|v| v.as_str()) {
                *viewing.lock().unwrap() = Some(tid.to_string());
            }
        }
        let Ok(permit) = inflight.clone().acquire_owned().await else {
            break;
        };
        let state = state.clone();
        let principal = principal.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let frame = match handle_request(&state, &principal, req).await {
                Ok(data) => ServerMessage::Response {
                    id,
                    ok: true,
                    data: Some(data),
                    error: None,
                },
                Err(e) => ServerMessage::Response {
                    id,
                    ok: false,
                    data: None,
                    error: Some(format!("{e:#}")),
                },
            };
            if let Ok(text) = serde_json::to_string(&frame) {
                let _ = out_tx.send(text);
            }
        });
    }
    writer.abort();
}

/// Head and tail kept from an oversized tool output on replay.
const REPLAY_OUTPUT_HEAD: usize = 1_500;
const REPLAY_OUTPUT_TAIL: usize = 500;

/// Shrink a thread replay by eliding the middle of long historical tool
/// outputs.
///
/// Tool output dominates a long thread's log — on a real 2.6 MB thread it was
/// 79% of the payload. Every one of those cards renders collapsed, so the
/// bytes buy nothing until someone expands that specific old call, yet the
/// whole log has to arrive before the feed paints. Nothing is lost: the card
/// pulls its full text back with `thread.toolOutput` when opened. Live events
/// are untouched — only the replay is trimmed, and only past the cap.
fn trim_replay_output(events: &mut [PersistedEvent]) {
    const CAP: usize = REPLAY_OUTPUT_HEAD + REPLAY_OUTPUT_TAIL;
    for pe in events.iter_mut() {
        let AgentEvent::ToolEnd {
            output, truncated, ..
        } = &mut pe.event
        else {
            continue;
        };
        let Some(text) = output else { continue };
        if text.len() <= CAP {
            continue;
        }
        let head = floor_char_boundary(text, REPLAY_OUTPUT_HEAD);
        let tail = ceil_char_boundary(text, text.len() - REPLAY_OUTPUT_TAIL);
        let elided = tail - head;
        *text = format!(
            "{}\n\n… {elided} characters elided — expand to load the full output …\n\n{}",
            &text[..head],
            &text[tail..]
        );
        *truncated = true;
    }
}

/// The full, untrimmed output of one historical tool call. Scans the log
/// backwards so the last result for a call id wins.
fn full_tool_output(
    store: &crate::store::Store,
    thread_id: &str,
    call_id: &str,
) -> Option<String> {
    store.read_events(thread_id).into_iter().rev().find_map(|pe| {
        match pe.event {
            AgentEvent::ToolEnd {
                call_id: id,
                output,
                ..
            } if id == call_id => output,
            _ => None,
        }
    })
}

/// Largest char boundary at or below `idx` (`str::floor_char_boundary` is
/// still unstable).
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Smallest char boundary at or above `idx`.
fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// A list-of-strings payload field, tolerating a single string or absence.
fn string_list(payload: &Value, key: &str) -> Vec<String> {
    match payload.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(single)) => vec![single.clone()],
        _ => Vec::new(),
    }
}

/// `agents: ["claude", "codex", "kimi"]` for the skill half of the Library.
/// Only these three have a skills directory Threadknot manages, so an unknown name
/// is an error rather than a silent no-op that looks like a failed install.
fn skill_targets(payload: &Value) -> anyhow::Result<Vec<crate::library::SkillTarget>> {
    let targets: Vec<crate::library::SkillTarget> = string_list(payload, "agents")
        .iter()
        .map(|name| match name.as_str() {
            "claude" => Ok(crate::library::SkillTarget::Claude),
            "codex" => Ok(crate::library::SkillTarget::Codex),
            "kimi" => Ok(crate::library::SkillTarget::Kimi),
            other => Err(anyhow::anyhow!("{other} has no skills directory")),
        })
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(!targets.is_empty(), "pick at least one agent");
    Ok(targets)
}

fn field<'a>(payload: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing field: {key}"))
}

fn bool_field(payload: &Value, key: &str) -> anyhow::Result<bool> {
    payload
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow::anyhow!("missing field: {key}"))
}

/// Tri-state appearance patch from a client payload: an absent key leaves
/// the field untouched, `null` clears it, a string sets it (validation
/// happens where the value lands).
fn appearance_patch(payload: &Value, key: &str) -> anyhow::Result<Option<Option<String>>> {
    Ok(match payload.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => anyhow::bail!("{key} must be a string or null"),
    })
}

/// Sidebar art is deliberately stored as a compact data URL: it survives
/// desktop/LAN clients and workspace mesh replication without another asset
/// transport. The UI resizes to 256px; this bound protects hand-written RPCs.
fn optional_sidebar_image(payload: &Value, key: &str) -> anyhow::Result<Option<String>> {
    let Some(value) = payload.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let image = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{key} must be an image data URL or null"))?;
    anyhow::ensure!(image.len() <= 700_000, "image is too large");
    let (_, encoded) = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/webp;base64,",
        "data:image/gif;base64,",
    ]
    .into_iter()
    .find_map(|prefix| image.strip_prefix(prefix).map(|data| (prefix, data)))
    .ok_or_else(|| anyhow::anyhow!("image must be PNG, JPEG, WebP, or GIF"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("image contains invalid base64"))?;
    anyhow::ensure!(!bytes.is_empty(), "image is empty");
    anyhow::ensure!(bytes.len() <= 512 * 1024, "image is too large");
    Ok(Some(image.to_string()))
}

/// Requests that route by an explicit `machineId` in the payload: when it
/// names another machine, the request is forwarded verbatim (sans machineId)
/// over that peer's socket and its response returned. This is the entire
/// remote-thread mechanism — the owner executes; we relay. Requests whose
/// machineId IS a real local parameter (peer.*, workspace.attachRoot) are
/// deliberately absent.
const ROUTABLE: &[&str] = &[
    // Editing another machine's REAL profile: the request is forwarded to the
    // owning machine (which strips machineId, runs as master, updates its own
    // device.json, and gossips the change out).
    "device.rename",
    "device.setAppearance",
    "thread.list",
    "thread.get",
    "thread.search",
    "thread.toolOutput",
    "thread.preview",
    "thread.create",
    "thread.rename",
    "thread.setSettled",
    "thread.setFavorite",
    "thread.delete",
    "thread.setAgent",
    "thread.setSettings",
    // A review runs a turn, so like turn.start it must execute on the machine
    // that owns the thread and its working tree.
    "thread.review",
    "thread.parley.start",
    "thread.archive",
    // Archives live on the machine whose archiveDir holds them, so listing,
    // restoring, and deleting one all route to the owner. (settings.get/set
    // deliberately stay local: each machine's archiveDir is its own setting.)
    "archive.list",
    "archive.restore",
    "archive.delete",
    "turn.start",
    "turn.steer",
    "turn.interrupt",
    "approval.respond",
    "question.respond",
    "fs.listDir",
    "fs.tree",
    "fs.read",
    "term.list",
    "term.create",
    "term.rename",
    "term.delete",
    "artifacts.list",
    "artifacts.delete",
    // Designing a palette from a wallpaper shells out to the local Claude CLI,
    // so a phone routes it to a machine that actually has that binary.
    "theme.aiPalette",
    // Browser logins belong to the machine whose Chrome holds the session, so
    // managing them routes to that machine. Without this the owner would have
    // to physically sit at a machine to give it a login — the exact trip
    // Threadknot exists to avoid. The master-only guard runs BEFORE routing, so a
    // phone is still refused here rather than laundered through a peer.
    "browser.profile.list",
    "browser.profile.create",
    "browser.profile.update",
    "browser.profile.delete",
    // The Library is per-machine: skills live in that machine's CLI folders and
    // an MCP server is launched by that machine's Threadknot. Routing lets the
    // fleet view show and manage another machine's shelf; the master-only guard
    // still runs first, so a phone cannot launder an install through a peer.
    "library.list",
    "library.skill.install",
    "library.skill.copy",
    "library.skill.remove",
    "library.mcp.save",
    "library.mcp.install",
    "library.mcp.delete",
];

/// Whether a request kind participates in machineId routing. The whole git
/// family routes (repos/status/diff/stage/commit/… all act on the owner's
/// working tree); ports.scan stays machine-local.
fn is_routable(kind: &str) -> bool {
    ROUTABLE.contains(&kind) || kind.starts_with("git.")
}

/// Push a workspace record to every paired machine (best-effort; an offline
/// peer reconciles at its next connect via the resync push). The catalog is
/// mesh-wide — every workspace is visible on every machine — so targets are
/// the whole peer registry, not the member list.
async fn replicate_workspace(state: &ServerState, ws: &Workspace) {
    for peer in state.peernet.registry.list() {
        if peer.machine_id == state.device.machine_id {
            continue;
        }
        if let Err(e) = state
            .peernet
            .request(&peer.machine_id, "mesh.workspaceUpsert", json!({ "workspace": ws }))
            .await
        {
            tracing::debug!("workspace replicate to {} deferred: {e:#}", peer.machine_id);
        }
    }
}

/// Tell every paired machine a workspace is gone (best-effort; offline peers
/// reconcile via the tombstones pushed at resync).
async fn replicate_workspace_delete(state: &ServerState, id: &str, deleted_at: &str) {
    for peer in state.peernet.registry.list() {
        if peer.machine_id == state.device.machine_id {
            continue;
        }
        if let Err(e) = state
            .peernet
            .request(
                &peer.machine_id,
                "mesh.workspaceDelete",
                json!({ "id": id, "deletedAt": deleted_at }),
            )
            .await
        {
            tracing::debug!("workspace delete to {} deferred: {e:#}", peer.machine_id);
        }
    }
}

async fn handle_request(
    state: &ServerState,
    principal: &Principal,
    req: ClientRequest,
) -> anyhow::Result<Value> {
    let hub = &state.hub;
    let store = &hub.store;
    let p = req.payload;

    // Gate BEFORE routing: these mutate a source checkout and can stop and
    // relaunch the desktop app, so a revocable phone credential must not reach
    // them — not locally, and not by asking a peer machine to do it instead.
    // Reading update status stays open so the fleet view works on mobile.
    if crate::update::is_privileged(&req.kind) {
        anyhow::ensure!(
            *principal == Principal::Master,
            "updating Threadknot requires this machine's master token, not a device credential"
        );
    }

    // Creating a signed-in browser profile, widening the sites it may visit, or
    // erasing one is an authority change over stored logins. A revocable phone
    // credential can drive a session the owner already set up; it cannot decide
    // what that session is allowed to reach.
    if req.kind.starts_with("browser.profile.") && req.kind != "browser.profile.list" {
        anyhow::ensure!(
            *principal == Principal::Master,
            "managing signed-in browser profiles requires this machine's master token"
        );
    }

    // Installing an MCP server hands every agent on this machine a new tool —
    // often a local process launched with Threadknot's own privileges, often
    // carrying a credential. Installing a skill writes instructions the CLIs
    // obey. Both are authority changes, so they need the machine's master
    // token; reading the shelf does not.
    if req.kind.starts_with("library.") && req.kind != "library.list" {
        anyhow::ensure!(
            *principal == Principal::Master,
            "installing skills and MCP servers requires this machine's master token"
        );
    }

    if is_routable(&req.kind) {
        if let Some(mid) = p.get("machineId").and_then(|v| v.as_str()) {
            if mid != state.device.machine_id {
                let mid = mid.to_string();
                let mut payload = p.clone();
                if let Some(obj) = payload.as_object_mut() {
                    obj.remove("machineId");
                }
                return state.peernet.request(&mid, &req.kind, payload).await;
            }
        }
    }

    match req.kind.as_str() {
        "hello" => {
            // If the background probe hasn't populated the cache yet, build the
            // agent list on demand so `hello` never returns an empty list — an
            // empty list leaves the UI with no selectable agents/models until the
            // socket reconnects (the frontend only requests agents on connect).
            let agents = {
                let cached = state.agents_cache.read().await.clone();
                match cached {
                    Some(a) => a,
                    None => {
                        let info = build_agents_info(&state.hub.hermes, &state.hub.claudex).await;
                        *state.agents_cache.write().await = Some(info.clone());
                        info
                    }
                }
            };
            Ok(json!({
                // Git-derived at compile time (build.rs): "0.1.<commit count>",
                // so every commit to master bumps what the UI shows.
                "version": env!("THREADKNOT_VERSION"),
                "gitHash": env!("THREADKNOT_GIT_HASH"),
                "buildDate": env!("THREADKNOT_BUILD_DATE"),
                "lanUrl": state.lan_url,
                "agents": agents,
                // Named reviewer presets for Parley debates (personas.json);
                // edited via persona.save/delete, which pulse "identity".
                "personas": hub.personas.list(),
                "serverId": state.config.server_id,
                "serverName": state.server_name(),
                // Mesh identity (machineId == serverId today; kept separate on
                // the wire so clients never have to know that).
                "machineId": state.device.machine_id,
                "friendlyName": state.device.friendly_name(),
                // Peers read these off the hello like the friendly name; a
                // key present as null is an explicit "cleared" (absent keys
                // mean an older Threadknot and leave stored values alone).
                "avatar": state.device.avatar(),
                "color": state.device.color(),
                "profileUpdatedAt": state.device.profile_updated_at(),
                "meshVersion": crate::device::MESH_VERSION,
                // Whether the composer's mic button can do anything here, and
                // why not when it can't.
                "dictation": crate::dictation::capability(*principal == Principal::Master),
            }))
        }
        "app.changelog" => {
            // Both embedded at compile time by build.rs, so the app always
            // carries the history of the exact build it was compiled from.
            // `notes` (CHANGELOG.md releases, {version, date, notes[]}) are the
            // client-facing update notes the UI shows; `entries` (git log,
            // {version, hash, date, subject, body}) stay internal.
            let entries: Value =
                serde_json::from_str(env!("THREADKNOT_CHANGELOG_JSON")).unwrap_or_else(|_| json!([]));
            let notes: Value = serde_json::from_str(env!("THREADKNOT_RELEASE_NOTES_JSON"))
                .unwrap_or_else(|_| json!([]));
            Ok(json!({ "entries": entries, "notes": notes }))
        }
        "device.info" => Ok(state.device.info()),
        "device.rename" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "renaming this machine requires the desktop app"
            );
            let name = state.device.set_friendly_name(field(&p, "name")?)?;
            // Gossip the new name to connected peers (they merge it LWW).
            state.peernet.announce_now();
            // If a routed edit changed THIS machine's own profile, the local
            // UI must refetch hello to see it.
            hub.broadcast_state("identity", None);
            Ok(json!({ "friendlyName": name }))
        }
        "device.setAppearance" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "changing this machine's appearance requires the desktop app"
            );
            let (avatar, color) = state.device.set_appearance(
                appearance_patch(&p, "image")?,
                appearance_patch(&p, "color")?,
            )?;
            // Push the change to connected peers now instead of waiting for
            // their next reconnect hello.
            state.peernet.announce_now();
            // If a routed edit changed THIS machine's own profile, the local
            // UI must refetch hello to see it.
            hub.broadcast_state("identity", None);
            Ok(json!({ "avatar": avatar, "color": color }))
        }
        "peer.list" => {
            let peers: Vec<Value> = state
                .peernet
                .registry
                .list()
                .into_iter()
                .map(|p| {
                    let online = state.peernet.is_online(&p.machine_id);
                    let mut v = serde_json::to_value(&p).unwrap_or(json!({}));
                    v["online"] = json!(online);
                    v
                })
                .collect();
            Ok(json!({ "peers": peers, "discovered": state.peernet.discovered() }))
        }
        "peer.add" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "peer management requires the desktop app"
            );
            // Accept a bare host:port, an origin, or the full LAN URL with
            // ?token=… (the form Settings displays for copy-paste).
            let raw = field(&p, "url")?.trim();
            let parsed = url::Url::parse(raw)
                .or_else(|_| url::Url::parse(&format!("http://{raw}")))
                .map_err(|_| anyhow::anyhow!("invalid peer URL"))?;
            let host = parsed.host_str().ok_or_else(|| anyhow::anyhow!("missing host"))?.to_string();
            let port = parsed.port().unwrap_or(42800);
            let token = p
                .get("token")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| {
                    parsed
                        .query_pairs()
                        .find(|(k, _)| k == "token")
                        .map(|(_, v)| v.into_owned())
                })
                .ok_or_else(|| anyhow::anyhow!("missing token (paste the peer's URL with ?token=… or fill the token field)"))?;

            // Call the peer's pairing endpoint with OUR identity + master
            // token — after this exchange trust is mutual.
            let resp = crate::hermes::http_client()
                .post(format!("http://{}/api/peer/pair", crate::peernet::host_port(&host, port)))
                .bearer_auth(&token)
                .json(&json!({
                    "machineId": state.device.machine_id,
                    "name": state.device.friendly_name(),
                    "avatar": state.device.avatar(),
                    "color": state.device.color(),
                    "profileUpdatedAt": state.device.profile_updated_at(),
                    "port": state.config.port,
                    "peerToken": state.config.token,
                    "addresses": crate::peernet::local_addresses(),
                    "meshVersion": crate::device::MESH_VERSION,
                }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .context("could not reach the peer — check the URL and that Threadknot is running there")?;
            anyhow::ensure!(
                resp.status().is_success(),
                "peer refused pairing: {}",
                resp.text().await.unwrap_or_default()
            );
            let them: Value = resp.json().await.context("bad pairing response")?;
            let their_id = them.get("machineId").and_then(|v| v.as_str()).unwrap_or("");
            anyhow::ensure!(!their_id.is_empty(), "peer sent no machine id");
            anyhow::ensure!(
                their_id != state.device.machine_id,
                "that URL points at this machine"
            );
            let mut addresses = vec![host.clone()];
            if let Some(list) = them.get("addresses").and_then(|v| v.as_array()) {
                for a in list.iter().filter_map(|x| x.as_str()) {
                    if !addresses.iter().any(|x| x == a) {
                        addresses.push(a.to_string());
                    }
                }
            }
            let peer = state.peernet.registry.upsert(crate::peers::Peer {
                machine_id: their_id.to_string(),
                name: them
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Threadknot")
                    .chars()
                    .take(64)
                    .collect(),
                avatar: them.get("avatar").and_then(|v| v.as_str()).map(String::from),
                color: them.get("color").and_then(|v| v.as_str()).map(String::from),
                avatar_override: None,
                color_override: None,
                profile_updated_at: them
                    .get("profileUpdatedAt")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                token,
                port: them.get("port").and_then(|v| v.as_u64()).unwrap_or(port as u64) as u16,
                addresses,
                last_good_address: Some(host),
                last_seen_at: Some(now_iso()),
                added_at: now_iso(),
                mesh_version: them.get("meshVersion").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            })?;
            state.peernet.kick.notify_waiters();
            hub.broadcast_state("peers", None);
            Ok(serde_json::to_value(peer)?)
        }
        "peer.remove" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "peer management requires the desktop app"
            );
            let machine_id = field(&p, "machineId")?;
            state.peernet.registry.remove(machine_id)?;
            state.peernet.kick.notify_waiters();
            hub.broadcast_state("peers", None);
            // Its remote-only workspaces go too — they were only reachable
            // through the peering we just dropped.
            if store.purge_peer_workspaces(machine_id)? {
                hub.broadcast_state("workspaces", None);
            }
            Ok(json!({}))
        }
        "peer.setAppearance" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "peer management requires the desktop app"
            );
            let avatar = appearance_patch(&p, "image")?;
            if let Some(Some(image)) = &avatar {
                crate::device::validate_avatar(image)?;
            }
            let color = match appearance_patch(&p, "color")? {
                Some(Some(c)) => Some(Some(crate::device::validate_accent_color(&c)?)),
                other => other,
            };
            // A LOCAL override for how this peer is drawn here: it never
            // rides a peer frame and wins over the advertised appearance.
            let peer = state
                .peernet
                .registry
                .set_overrides(field(&p, "machineId")?, avatar, color)?;
            hub.broadcast_state("peers", None);
            let mut v = serde_json::to_value(&peer)?;
            v["online"] = json!(state.peernet.is_online(&peer.machine_id));
            Ok(v)
        }
        "peer.announce" => {
            // A connected peer telling us its fresh address set (DHCP moved
            // it, or it just came online). Only refresh hints for machines
            // we've actually paired with; identity is the machine id.
            let machine_id = field(&p, "machineId")?;
            let addresses: Vec<String> = p
                .get("addresses")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let port = p.get("port").and_then(|v| v.as_u64()).map(|v| v as u16);
            let moved = state
                .peernet
                .registry
                .note_addresses(machine_id, &addresses, port, None)?;
            // Announces also carry the peer's current REAL profile (name +
            // appearance, absent on older peers). Merge it last-write-wins by
            // the profileUpdatedAt clock the peer stamped.
            let styled = state.peernet.registry.note_profile(
                machine_id,
                p.get("name").and_then(|v| v.as_str()).map(String::from),
                crate::peernet::patch_str(&p, "avatar"),
                crate::peernet::patch_str(&p, "color"),
                p.get("profileUpdatedAt").and_then(|v| v.as_str()).map(String::from),
            )?;
            if moved {
                state.peernet.kick.notify_waiters();
            }
            if moved || styled {
                hub.broadcast_state("peers", None);
            }
            Ok(json!({}))
        }
        "workspace.list" => Ok(json!({ "workspaces": store.list_workspaces() })),
        "workspace.rename" => {
            let ws = store.rename_workspace(
                field(&p, "workspaceId")?,
                field(&p, "name")?.to_string(),
            )?;
            hub.broadcast_state("workspaces", None);
            replicate_workspace(state, &ws).await;
            Ok(serde_json::to_value(ws)?)
        }
        "workspace.setFavorite" => {
            let ws = store.set_workspace_favorite(
                field(&p, "workspaceId")?,
                bool_field(&p, "favorite")?,
            )?;
            hub.broadcast_state("workspaces", None);
            replicate_workspace(state, &ws).await;
            Ok(serde_json::to_value(ws)?)
        }
        "workspace.setImage" => {
            let image = optional_sidebar_image(&p, "image")?;
            let ws = store.set_workspace_image(field(&p, "workspaceId")?, image)?;
            hub.broadcast_state("workspaces", None);
            replicate_workspace(state, &ws).await;
            Ok(serde_json::to_value(ws)?)
        }
        "workspace.attachRoot" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "workspace roots are managed from the desktop app"
            );
            let workspace_id = field(&p, "workspaceId")?.to_string();
            let machine_id = field(&p, "machineId")?.to_string();
            let path = field(&p, "path")?.to_string();
            anyhow::ensure!(store.workspace(&workspace_id).is_some(), "unknown workspace");
            // The root's project lives on (and is created by) its owner.
            let project: Project = if machine_id == state.device.machine_id {
                store.create_project_raw(path, None)?
            } else {
                let v = state
                    .peernet
                    .request(&machine_id, "mesh.createProject", json!({ "path": path }))
                    .await?;
                serde_json::from_value(v)?
            };
            let ws = store.add_workspace_member(
                &workspace_id,
                WorkspaceMember {
                    machine_id: machine_id.clone(),
                    project_id: project.id.clone(),
                    name: Some(project.name.clone()),
                    path: Some(project.path.clone()),
                },
            )?;
            hub.broadcast_state("workspaces", None);
            hub.broadcast_state("projects", None);
            replicate_workspace(state, &ws).await;
            Ok(json!({ "workspace": ws, "project": project }))
        }
        "workspace.detachRoot" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "workspace roots are managed from the desktop app"
            );
            let workspace_id = field(&p, "workspaceId")?.to_string();
            let machine_id = field(&p, "machineId")?.to_string();
            let project_id = field(&p, "projectId")?.to_string();
            // The founding root shares the workspace's id; detaching it would
            // collide with the re-wrap workspace of the same id.
            anyhow::ensure!(
                project_id != workspace_id,
                "this is the workspace's original root — rename or delete the workspace instead"
            );
            let ws = store.remove_workspace_member(&workspace_id, &project_id)?;
            // The detached root must stay visible on its owner: re-wrap it
            // into its own workspace there.
            if machine_id == state.device.machine_id {
                store.wrap_project_in_workspace(&project_id)?;
            } else if let Err(e) = state
                .peernet
                .request(&machine_id, "mesh.rewrapProject", json!({ "projectId": project_id }))
                .await
            {
                tracing::warn!("rewrap on {machine_id} deferred: {e:#}");
            }
            hub.broadcast_state("workspaces", None);
            replicate_workspace(state, &ws).await;
            Ok(serde_json::to_value(ws)?)
        }
        // ---- mesh.* : peer-to-peer replication plumbing (peers authenticate
        // with the master token, so Principal::Master is implied) ----
        "mesh.createProject" => {
            anyhow::ensure!(*principal == Principal::Master, "mesh calls are peer-only");
            let project = store.create_project_raw(field(&p, "path")?.to_string(), None)?;
            hub.broadcast_state("projects", None);
            Ok(serde_json::to_value(project)?)
        }
        "mesh.workspaceUpsert" => {
            anyhow::ensure!(*principal == Principal::Master, "mesh calls are peer-only");
            let ws: Workspace = serde_json::from_value(
                p.get("workspace")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing workspace"))?,
            )?;
            if store.upsert_workspace_replica(ws)? {
                // A replica change may have detached one of our projects —
                // re-wrap anything left uncovered so it stays visible.
                for project in store.list_projects() {
                    store.wrap_project_in_workspace(&project.id)?;
                }
                hub.broadcast_state("workspaces", None);
            }
            Ok(json!({}))
        }
        "mesh.workspaceDelete" => {
            anyhow::ensure!(*principal == Principal::Master, "mesh calls are peer-only");
            let id = field(&p, "id")?;
            let deleted_at = field(&p, "deletedAt")?;
            if store.apply_workspace_tombstone(id, deleted_at)? {
                // The dead workspace may have wrapped one of our projects —
                // re-wrap anything left uncovered so it stays visible.
                for project in store.list_projects() {
                    store.wrap_project_in_workspace(&project.id)?;
                }
                hub.broadcast_state("workspaces", None);
            }
            Ok(json!({}))
        }
        "mesh.rewrapProject" => {
            anyhow::ensure!(*principal == Principal::Master, "mesh calls are peer-only");
            store.wrap_project_in_workspace(field(&p, "projectId")?)?;
            hub.broadcast_state("workspaces", None);
            Ok(json!({}))
        }
        "hermes.agent.list" => Ok(json!({ "agents": hub.hermes.list() })),
        "hermes.agent.statuses" => {
            // Initial snapshot on connect; live changes arrive via the
            // `hermes.statuses` broadcast the presence poller sends. Carries
            // `{ revision, statuses }` so the client can discard whichever of
            // the two loses their delivery race (see App.tsx).
            Ok(serde_json::to_value(hub.hermes_status.snapshot())?)
        }
        "hermes.agent.add" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Hermes agent management requires the desktop app"
            );
            let base_url = crate::hermes::normalize_base_url(field(&p, "baseUrl")?)?;
            let api_key = field(&p, "apiKey")?.trim().to_string();
            anyhow::ensure!(!api_key.is_empty(), "missing API key");
            // Probe before storing: a typo'd URL/key should fail here, not on
            // the first turn.
            let (model, _version) = crate::hermes::probe(&base_url, &api_key).await?;
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&model)
                .chars()
                .take(48)
                .collect::<String>();
            let agent = hub.hermes.add(name, base_url, api_key, model)?;
            refresh_hermes_agents(state).await;
            // Probe the new gateway now so it does not sit unknown for 20s.
            hub.hermes_status.kick();
            Ok(serde_json::to_value(agent)?)
        }
        "hermes.agent.remove" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Hermes agent management requires the desktop app"
            );
            hub.hermes.remove(field(&p, "agentId")?)?;
            refresh_hermes_agents(state).await;
            // Re-poll so the removed gateway's status is dropped and a fresh
            // snapshot (without it) is broadcast.
            hub.hermes_status.kick();
            Ok(json!({}))
        }
        "hermes.agent.setImage" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Hermes agent management requires the desktop app"
            );
            let image = optional_sidebar_image(&p, "image")?;
            let agent = hub.hermes.set_image(field(&p, "agentId")?, image)?;
            refresh_hermes_agents(state).await;
            Ok(serde_json::to_value(agent)?)
        }
        "hermes.agent.setAvatar" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Hermes agent management requires the desktop app"
            );
            // Same picture as setImage under its current wire name, with the
            // tighter avatar bound (these ride sidebar lists constantly).
            let image = appearance_patch(&p, "image")?.flatten();
            if let Some(image) = &image {
                crate::device::validate_avatar(image)?;
            }
            let agent = hub.hermes.set_image(field(&p, "agentId")?, image)?;
            refresh_hermes_agents(state).await;
            Ok(serde_json::to_value(agent)?)
        }
        "hermes.agent.details" => {
            let agent = hub
                .hermes
                .agent(field(&p, "agentId")?)
                .ok_or_else(|| anyhow::anyhow!("unknown Hermes agent"))?;
            crate::hermes::details(&agent.base_url, &agent.api_key).await
        }
        // User-crafted appearance themes. Machine-local, so like the workspace
        // image (also a data URL) they are NOT gated on the master principal —
        // a paired phone can craft one too. Changes broadcast a `themes`
        // state-changed frame; every client answers with a fresh `theme.list`.
        "theme.list" => Ok(json!({ "themes": hub.themes.list() })),
        "theme.save" => {
            let theme: CustomTheme = serde_json::from_value(
                p.get("theme")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing field: theme"))?,
            )?;
            let saved = hub.themes.save(theme)?;
            hub.broadcast_state("themes", None);
            Ok(serde_json::to_value(saved)?)
        }
        "theme.remove" => {
            hub.themes.remove(field(&p, "themeId")?)?;
            hub.broadcast_state("themes", None);
            Ok(json!({}))
        }
        // Ask the local Claude CLI to design a palette from a wallpaper. Cheap
        // and stateless, so it is in ROUTABLE: a phone whose own machine has no
        // CLI can name one that does via `machineId` and the owner runs it.
        "theme.aiPalette" => {
            let palette =
                crate::ai_palette::generate(
                    field(&p, "imageDataUrl")?,
                    p.get("hint").and_then(|v| v.as_str()),
                )
                .await?;
            Ok(serde_json::to_value(palette)?)
        }
        "claudex.profile.list" => Ok(json!({
            "profiles": hub.claudex.list().iter().map(|p| p.public()).collect::<Vec<_>>(),
        })),
        "claudex.profile.add" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Claudex profile management requires the desktop app"
            );
            let input: crate::claudex::ProfileInput = serde_json::from_value(p.clone())?;
            let profile = hub.claudex.add(input)?;
            refresh_claudex_agents(state).await;
            Ok(profile.public())
        }
        "claudex.profile.update" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Claudex profile management requires the desktop app"
            );
            let id = field(&p, "profileId")?.to_string();
            let input: crate::claudex::ProfileInput = serde_json::from_value(p.clone())?;
            let profile = hub.claudex.update(&id, input)?;
            // The bridge this profile used may no longer be the right one.
            hub.claudex_sidecars.stop(&id).await;
            refresh_claudex_agents(state).await;
            Ok(profile.public())
        }
        "claudex.profile.remove" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Claudex profile management requires the desktop app"
            );
            let id = field(&p, "profileId")?.to_string();
            hub.claudex.remove(&id)?;
            hub.claudex_sidecars.stop(&id).await;
            refresh_claudex_agents(state).await;
            Ok(json!({}))
        }
        "claudex.profile.setAvatar" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Claudex profile management requires the desktop app"
            );
            let image = appearance_patch(&p, "image")?.flatten();
            if let Some(image) = &image {
                crate::device::validate_avatar(image)?;
            }
            let profile = hub.claudex.set_avatar(field(&p, "profileId")?, image)?;
            refresh_claudex_agents(state).await;
            Ok(profile.public())
        }
        // Reachability check for the Settings UI. This STARTS the sidecar if
        // one is configured, so "test" answers the question the user is
        // actually asking: will a turn work right now?
        "claudex.profile.test" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "Claudex profile management requires the desktop app"
            );
            let profile = hub
                .claudex
                .profile(field(&p, "profileId")?)
                .ok_or_else(|| anyhow::anyhow!("unknown Claudex profile"))?;
            let state_ = hub.claudex_sidecars.ensure(&profile).await?;
            Ok(json!({ "sidecar": state_, "baseUrl": profile.base_url }))
        }
        "claudex.profile.status" => {
            let profile = hub
                .claudex
                .profile(field(&p, "profileId")?)
                .ok_or_else(|| anyhow::anyhow!("unknown Claudex profile"))?;
            Ok(json!({ "sidecar": hub.claudex_sidecars.status(&profile).await }))
        }
        "mobile.device.list" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "device management requires the desktop app"
            );
            Ok(json!({ "devices": state.mobile.list() }))
        }
        "mobile.device.revoke" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "device management requires the desktop app"
            );
            state.mobile.revoke(field(&p, "deviceId")?)?;
            hub.broadcast_state("mobileDevices", None);
            Ok(json!({}))
        }
        "project.create" => {
            let path = field(&p, "path")?.to_string();
            let name = p.get("name").and_then(|v| v.as_str()).map(String::from);
            let project = store.create_project(path, name)?;
            hub.broadcast_state("projects", None);
            Ok(serde_json::to_value(project)?)
        }
        "project.list" => Ok(json!({ "projects": store.list_projects() })),
        "project.delete" => {
            let project_id = field(&p, "projectId")?.to_string();
            // Kill live shells + delete scrollback for this project's terminals
            // before the records are cascaded away by `delete_project`.
            for t in store.list_terminals(&project_id) {
                state.terms.delete(&project_id, &t.id);
            }
            let cascade = store.delete_project(&project_id)?;
            hub.broadcast_state("projects", None);
            hub.broadcast_state("workspaces", None);
            for ws in &cascade.updated {
                replicate_workspace(state, ws).await;
            }
            for id in &cascade.deleted {
                replicate_workspace_delete(state, id, &cascade.deleted_at).await;
            }
            Ok(json!({}))
        }
        "thread.create" => {
            let project_id = field(&p, "projectId")?.to_string();
            // Hermes threads have no folder — they land in the hidden home
            // project, materialized on first use.
            if project_id == crate::store::HERMES_HOME_PROJECT_ID
                && store.ensure_hermes_home()?
            {
                hub.broadcast_state("projects", None);
            }
            let agent: Agent = serde_json::from_value(
                p.get("agent").cloned().unwrap_or(json!("claude")),
            )?;
            let settings: ThreadSettings = serde_json::from_value(
                p.get("settings")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing settings"))?,
            )?;
            let thread = store.create_thread(project_id.clone(), agent, settings)?;
            hub.broadcast_state("threads", Some(project_id));
            Ok(serde_json::to_value(thread)?)
        }
        "thread.list" => Ok(json!({ "threads": store.list_threads(field(&p, "projectId")?) })),
        "thread.get" => {
            let thread_id = field(&p, "threadId")?;
            let thread = store
                .thread(thread_id)
                .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
            let mut events = store.read_events(thread_id);
            trim_replay_output(&mut events);
            Ok(json!({ "thread": thread, "events": events }))
        }
        "thread.search" => {
            let query = field(&p, "query")?.trim().to_string();
            anyhow::ensure!(query.chars().count() <= 200, "search query is too long");
            let thread_ids = string_list(&p, "threadIds");
            anyhow::ensure!(thread_ids.len() <= 10_000, "too many threads to search");
            let store = Arc::clone(&hub.store);
            let thread_ids = tokio::task::spawn_blocking(move || {
                store.search_thread_content(&thread_ids, &query)
            })
            .await
            .context("thread search task failed")?;
            Ok(json!({ "threadIds": thread_ids }))
        }
        "thread.toolOutput" => {
            let thread_id = field(&p, "threadId")?;
            let call_id = field(&p, "callId")?;
            Ok(json!({ "output": full_tool_output(store, thread_id, call_id) }))
        }
        "thread.preview" => {
            // Unknown/empty threads answer with an empty preview rather than an
            // error; remote threads route to their owner via the machineId above.
            Ok(serde_json::to_value(store.thread_preview(field(&p, "threadId")?))?)
        }
        "thread.rename" => {
            let title = field(&p, "title")?.to_string();
            let thread = store.update_thread(field(&p, "threadId")?, |t| t.title = title)?;
            hub.broadcast_state("threads", Some(thread.project_id.clone()));
            Ok(serde_json::to_value(thread)?)
        }
        "thread.setSettled" => {
            let settled = p
                .get("settled")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| anyhow::anyhow!("missing settled"))?;
            // Parking stamps the time (the shelf sorts by it); un-parking
            // clears the stamp and pins the thread active, so idle
            // auto-settle can't immediately swallow it again.
            // Only an idle thread can be parked. Settling a running or
            // blocked thread would hide it, and the event that later says
            // "it finished" reaches only clients connected at that moment —
            // so a client that was asleep would find a finished (or crashed)
            // run buried in a collapsed shelf with nothing to flag it.
            //
            // Checked INSIDE the write closure, not before it: a read-then-write
            // pair leaves a window where an agent event flips the thread to
            // running and clears its settled fields, and the write would then
            // re-park a thread that had just started work.
            //
            // Quiet write: filing a thread is not activity in it. Bumping
            // updated_at here would relabel a stale thread as fresh and
            // restart the idle window auto-settle measures.
            let thread = store.try_update_thread_quiet(field(&p, "threadId")?, |t| {
                anyhow::ensure!(
                    !settled || t.status == ThreadStatus::Idle,
                    "thread is still working — interrupt or wait before settling it"
                );
                t.settled_at = if settled { Some(now_iso()) } else { None };
                t.kept_active_at = if settled { None } else { Some(now_iso()) };
                Ok(())
            })?;
            hub.broadcast_state("threads", Some(thread.project_id.clone()));
            Ok(serde_json::to_value(thread)?)
        }
        "thread.setFavorite" => {
            let favorite = bool_field(&p, "favorite")?;
            // Starring must NOT bump updatedAt: it is not chat activity, and a
            // recency bump would jump the thread in the sidebar sort and make
            // "Xm ago" lie. set_thread_favorite mutates + flushes only.
            let thread = store.set_thread_favorite(field(&p, "threadId")?, favorite)?;
            hub.broadcast_state("threads", Some(thread.project_id.clone()));
            Ok(serde_json::to_value(thread)?)
        }
        "thread.setAgent" => {
            let agent: Agent = serde_json::from_value(
                p.get("agent")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing agent"))?,
            )?;
            let settings: ThreadSettings = serde_json::from_value(
                p.get("settings")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing settings"))?,
            )?;
            let thread = hub.set_agent(field(&p, "threadId")?, agent, settings)?;
            Ok(serde_json::to_value(thread)?)
        }
        "thread.setSettings" => {
            let settings: ThreadSettings = serde_json::from_value(
                p.get("settings")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing settings"))?,
            )?;
            let thread = hub.set_settings(field(&p, "threadId")?, settings)?;
            Ok(serde_json::to_value(thread)?)
        }
        // Throw a second agent at this thread as a read-only adversarial
        // reviewer, for exactly one turn (Parley phase 1; see docs/PARLEY.md).
        "thread.review" => {
            let agent: Agent = serde_json::from_value(
                p.get("agent")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing agent"))?,
            )?;
            let model = p
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let effort = p
                .get("effort")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let access: Option<Access> = p
                .get("access")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?;
            let instructions = p
                .get("instructions")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let participant = hub.review(
                field(&p, "threadId")?,
                crate::agents::ReviewerSpec {
                    agent,
                    model,
                    effort,
                    access,
                    name: None,
                    persona: None,
                    personality: None,
                },
                instructions,
            )?;
            Ok(serde_json::to_value(participant)?)
        }
        "thread.parley.start" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ReviewerSpecPayload {
                agent: Agent,
                #[serde(default)]
                model: Option<String>,
                #[serde(default)]
                effort: Option<String>,
                #[serde(default)]
                access: Option<Access>,
                #[serde(default)]
                name: Option<String>,
                /// Stable persona identity (personas.json id): two personas on
                /// the same setup seat two lanes.
                #[serde(default)]
                persona_id: Option<String>,
                #[serde(default)]
                personality: Option<String>,
            }
            let reviewers: Vec<ReviewerSpecPayload> = serde_json::from_value(
                p.get("reviewers")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing reviewers"))?,
            )?;
            let rounds = p.get("rounds").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
            let execute = p
                .get("execute")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let instructions = p
                .get("instructions")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let thread = hub.start_parley(
                field(&p, "threadId")?,
                reviewers
                    .into_iter()
                    .map(|r| crate::agents::ReviewerSpec {
                        agent: r.agent,
                        model: r.model,
                        effort: r.effort,
                        access: r.access,
                        name: r.name,
                        persona: r.persona_id,
                        personality: r.personality,
                    })
                    .collect(),
                rounds,
                execute,
                instructions,
            )?;
            Ok(serde_json::to_value(thread)?)
        }
        "persona.save" => {
            // Any authenticated client may edit personas: they are shared
            // review config, not secrets (unlike Hermes API keys), and a
            // paired phone starting debates needs to shape its reviewers.
            let persona: crate::personas::ReviewerPersona = serde_json::from_value(
                p.get("persona")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing persona"))?,
            )?;
            let saved = hub.personas.save(persona)?;
            // Personas ride hello; identity is the scope that refetches it.
            hub.broadcast_state("identity", None);
            Ok(serde_json::to_value(saved)?)
        }
        "persona.delete" => {
            hub.personas.delete(field(&p, "personaId")?)?;
            hub.broadcast_state("identity", None);
            Ok(json!({}))
        }
        "thread.delete" => {
            let thread_id = field(&p, "threadId")?;
            hub.stop_session(thread_id);
            state.browsers.remove(thread_id);
            store.delete_thread(thread_id)?;
            hub.broadcast_state("threads", None);
            Ok(json!({}))
        }
        "turn.start" => {
            let attachments: Vec<IncomingAttachment> = p
                .get("attachments")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| anyhow::anyhow!("bad attachments"))?
                .unwrap_or_default();
            hub.start_turn(
                field(&p, "threadId")?,
                field(&p, "text")?.to_string(),
                attachments,
            )?;
            Ok(json!({}))
        }
        "turn.steer" => {
            hub.steer_turn(field(&p, "threadId")?, field(&p, "text")?.to_string())?;
            Ok(json!({}))
        }
        "turn.interrupt" => {
            hub.interrupt(field(&p, "threadId")?)?;
            Ok(json!({}))
        }
        "approval.respond" => {
            hub.respond_approval(
                field(&p, "threadId")?,
                field(&p, "approvalId")?.to_string(),
                field(&p, "optionId")?.to_string(),
            )?;
            Ok(json!({}))
        }
        "question.respond" => {
            let answers: HashMap<String, Vec<String>> = p
                .get("answers")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?
                .unwrap_or_default();
            hub.respond_question(
                field(&p, "threadId")?,
                field(&p, "requestId")?.to_string(),
                answers,
            )?;
            Ok(json!({}))
        }
        "schedule.create" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Create {
                project_id: String,
                agent: Agent,
                settings: ThreadSettings,
                #[serde(default)]
                name: Option<String>,
                prompt: String,
                cadence: Cadence,
            }
            let c: Create = serde_json::from_value(p)?;
            anyhow::ensure!(!c.prompt.trim().is_empty(), "prompt is empty");
            let name = c
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| c.prompt.trim().chars().take(40).collect());
            let schedule = store.create_schedule(Schedule {
                id: new_id(),
                project_id: c.project_id,
                agent: c.agent,
                settings: c.settings,
                name,
                prompt: c.prompt,
                next_run_at: crate::schedules::next_run_iso(&c.cadence),
                cadence: c.cadence,
                enabled: true,
                created_at: now_iso(),
                last_run_at: None,
                last_thread_id: None,
                last_error: None,
            })?;
            hub.broadcast_state("schedules", None);
            hub.sched.kick.notify_one();
            Ok(serde_json::to_value(schedule)?)
        }
        "schedule.list" => Ok(json!({ "schedules": store.list_schedules() })),
        "schedule.update" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Update {
                schedule_id: String,
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                prompt: Option<String>,
                #[serde(default)]
                cadence: Option<Cadence>,
                #[serde(default)]
                enabled: Option<bool>,
                #[serde(default)]
                agent: Option<Agent>,
                #[serde(default)]
                settings: Option<ThreadSettings>,
                #[serde(default)]
                project_id: Option<String>,
            }
            let u: Update = serde_json::from_value(p)?;
            if let Some(pid) = &u.project_id {
                anyhow::ensure!(store.project(pid).is_some(), "unknown project");
            }
            let schedule = store.update_schedule(&u.schedule_id, |s| {
                if let Some(v) = u.name {
                    if !v.trim().is_empty() {
                        s.name = v;
                    }
                }
                if let Some(v) = u.prompt {
                    if !v.trim().is_empty() {
                        s.prompt = v;
                    }
                }
                if let Some(v) = u.cadence {
                    s.cadence = v;
                }
                if let Some(v) = u.enabled {
                    s.enabled = v;
                }
                if let Some(v) = u.agent {
                    s.agent = v;
                }
                if let Some(v) = u.settings {
                    s.settings = v;
                }
                if let Some(v) = u.project_id {
                    s.project_id = v;
                }
                // Re-plan so a stale next-run can't fire (or read) wrong.
                s.next_run_at = crate::schedules::next_run_iso(&s.cadence);
            })?;
            hub.broadcast_state("schedules", None);
            hub.sched.kick.notify_one();
            Ok(serde_json::to_value(schedule)?)
        }
        "schedule.delete" => {
            store.delete_schedule(field(&p, "scheduleId")?)?;
            hub.broadcast_state("schedules", None);
            Ok(json!({}))
        }
        "schedule.run" => {
            let thread_id = crate::schedules::run_now(hub, field(&p, "scheduleId")?)?;
            Ok(json!({ "threadId": thread_id }))
        }
        "usage.get" => {
            if hub.usage.is_empty() {
                hub.usage.kick(false);
            }
            Ok(json!({ "usage": hub.usage.snapshot() }))
        }
        "usage.refresh" => {
            hub.usage.kick(true);
            Ok(json!({}))
        }
        // Dictation drives this machine's own microphone, so it is never peer
        // routed and never available to a paired device's credential.
        "dictation.start" => {
            anyhow::ensure!(
                *principal == Principal::Master,
                "dictation records this machine's mic, so it only runs from the app on that machine"
            );
            Ok(json!({ "recordingId": state.dictation.start().await? }))
        }
        "dictation.stop" => {
            let text = state.dictation.stop(field(&p, "recordingId")?).await?;
            Ok(json!({ "text": text }))
        }
        "dictation.cancel" => {
            state.dictation.cancel(field(&p, "recordingId")?).await;
            Ok(json!({}))
        }
        // Must precede the generic git arm: these act on Threadknot's own checkout,
        // not on a project repo, so they take no repoId.
        k if k.starts_with("git.selfUpdate") => crate::update::handle(hub, k, &p).await,
        k if k.starts_with("git.") => crate::git::handle(state, k, &p).await,
        "fs.tree" => crate::files::tree(state, &p),
        "fs.read" => crate::files::read(state, &p),
        "artifacts.list" => {
            let project_id = field(&p, "projectId")?;
            let artifacts = match p.get("threadId").and_then(|v| v.as_str()) {
                Some(thread_id) => store.list_artifacts_for_thread(thread_id),
                None => store.list_artifacts_for_project(project_id),
            };
            Ok(json!({ "artifacts": artifacts }))
        }
        "artifacts.delete" => {
            let artifact = store.delete_artifact(field(&p, "artifactId")?)?;
            hub.broadcast_state("artifacts", Some(artifact.project_id));
            Ok(json!({}))
        }
        // Signed-in browser profiles. Managing them is a master-only action:
        // a paired phone can *drive* a session the owner set up, but creating
        // one, widening its scope, or erasing it is not a remote capability.
        "browser.profile.list" => Ok(json!({ "profiles": state.browser_profiles.list() })),
        "browser.profile.create" => {
            let profile = state
                .browser_profiles
                .create(field(&p, "name")?, &string_list(&p, "origins"))?;
            hub.broadcast_state("browserProfiles", None);
            Ok(serde_json::to_value(profile)?)
        }
        "browser.profile.update" => {
            let origins = p
                .get("origins")
                .is_some()
                .then(|| string_list(&p, "origins"));
            let profile = state.browser_profiles.update(
                field(&p, "profileId")?,
                p.get("name").and_then(Value::as_str),
                origins.as_deref(),
            )?;
            // Scope changes must reach the live browser, not just the record.
            state.browsers.close_sessions_using(&profile.id);
            hub.broadcast_state("browserProfiles", None);
            Ok(serde_json::to_value(profile)?)
        }
        "browser.profile.delete" => {
            let profile_id = field(&p, "profileId")?.to_string();
            // Close first: an open Chrome would rewrite the cookie jar we are
            // about to erase, leaving the user still signed in.
            state.browsers.close_sessions_using(&profile_id);
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            state.browser_profiles.delete(&profile_id)?;
            hub.broadcast_state("browserProfiles", None);
            Ok(json!({}))
        }
        // The Library: skills (folders each CLI discovers itself) and MCP
        // servers (which Threadknot injects at spawn). Listing is open so a phone
        // can see what a machine has; everything that changes what the agents
        // can run is master-only, gated above.
        "library.list" => Ok(json!({
            "skills": crate::library::list_skills(),
            "mcpServers": hub.library.list(),
            "catalog": crate::catalog::catalog(),
        })),
        "library.skill.install" => {
            let targets = skill_targets(&p)?;
            // Either a catalog id or a pasted repo URL — never both silently.
            let (source, catalog_id) = match p.get("catalogId").and_then(Value::as_str) {
                Some(id) => {
                    let entry = crate::catalog::find_skill(id)
                        .ok_or_else(|| anyhow::anyhow!("unknown catalog skill: {id}"))?;
                    match entry.origin {
                        // Threadknot's own document skills live in the binary: no
                        // network, no GitHub, nothing to fail partway.
                        crate::catalog::SkillSourceRef::Bundled => {
                            let bundled = crate::bundled::find(&entry.id).ok_or_else(|| {
                                anyhow::anyhow!("{} is not bundled in this build", entry.id)
                            })?;
                            let skill =
                                crate::library::install_bundled_skill(bundled, &targets)?;
                            hub.broadcast_state("library", None);
                            return Ok(json!({ "skill": skill }));
                        }
                        crate::catalog::SkillSourceRef::Github { repo, git_ref, path } => (
                            crate::library::SkillSource { repo, git_ref, path },
                            Some(entry.id),
                        ),
                    }
                }
                None => (
                    crate::library::SkillSource::parse(field(&p, "source")?)?,
                    None,
                ),
            };
            let skill = crate::library::install_skill(
                &source,
                &targets,
                catalog_id,
                p.get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|n| !n.trim().is_empty()),
            )
            .await?;
            hub.broadcast_state("library", None);
            Ok(json!({ "skill": skill }))
        }
        "library.skill.copy" => {
            let targets = skill_targets(&p)?;
            let copied = crate::library::copy_skill(field(&p, "skillId")?, &targets)?;
            hub.broadcast_state("library", None);
            Ok(json!({ "copied": copied }))
        }
        "library.skill.remove" => {
            let targets = skill_targets(&p)?;
            let removed = crate::library::remove_skill(field(&p, "skillId")?, &targets)?;
            hub.broadcast_state("library", None);
            Ok(json!({ "removed": removed }))
        }
        "library.mcp.save" => {
            let server: crate::library::McpServer = serde_json::from_value(
                p.get("server")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing server"))?,
            )?;
            let saved = hub.library.save(server)?;
            hub.broadcast_state("library", None);
            Ok(json!({ "server": saved }))
        }
        "library.mcp.install" => {
            let id = field(&p, "catalogId")?;
            let entry = crate::catalog::find_mcp(id)
                .ok_or_else(|| anyhow::anyhow!("unknown catalog server: {id}"))?;
            let inputs: std::collections::BTreeMap<String, String> = p
                .get("inputs")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let mut server = crate::catalog::render(&entry, &inputs)?;
            if let Some(name) = p.get("name").and_then(Value::as_str) {
                if !name.trim().is_empty() {
                    server.name = name.trim().to_string();
                }
            }
            if let Some(agents) = p.get("agents") {
                server.agents = serde_json::from_value(agents.clone())?;
            }
            let saved = hub.library.save(server)?;
            hub.broadcast_state("library", None);
            Ok(json!({ "server": saved }))
        }
        "library.mcp.delete" => {
            hub.library.delete(field(&p, "serverId")?)?;
            hub.broadcast_state("library", None);
            Ok(json!({}))
        }
        "term.list" => {
            let project_id = field(&p, "projectId")?;
            let terms: Vec<Value> = store
                .list_terminals(project_id)
                .into_iter()
                .map(|t| {
                    let alive = state.terms.alive(project_id, &t.id);
                    json!({ "id": t.id, "name": t.name, "createdAt": t.created_at, "alive": alive })
                })
                .collect();
            Ok(json!({ "terms": terms }))
        }
        "term.create" => {
            let project_id = field(&p, "projectId")?.to_string();
            let name = p.get("name").and_then(|v| v.as_str()).map(String::from);
            let terminal = store.create_terminal(project_id.clone(), name)?;
            hub.broadcast_state("terminals", Some(project_id));
            Ok(json!({
                "id": terminal.id,
                "name": terminal.name,
                "createdAt": terminal.created_at,
                "alive": false,
            }))
        }
        "term.rename" => {
            let name = field(&p, "name")?.to_string();
            let terminal = store.rename_terminal(field(&p, "termId")?, name)?;
            hub.broadcast_state("terminals", Some(terminal.project_id.clone()));
            Ok(json!({
                "id": terminal.id,
                "name": terminal.name,
                "createdAt": terminal.created_at,
                "alive": state.terms.alive(&terminal.project_id, &terminal.id),
            }))
        }
        "term.delete" => {
            let term_id = field(&p, "termId")?.to_string();
            let terminal = store
                .terminal(&term_id)
                .ok_or_else(|| anyhow::anyhow!("unknown terminal"))?;
            state.terms.delete(&terminal.project_id, &term_id);
            store.delete_terminal(&term_id)?;
            hub.broadcast_state("terminals", Some(terminal.project_id));
            Ok(json!({}))
        }
        "ports.scan" => Ok(crate::ports::scan(state).await),
        "fs.listDir" => {
            let path = p
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));
            let mut entries: Vec<Value> = Vec::new();
            for entry in std::fs::read_dir(&path)?.flatten() {
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let name = entry.file_name().to_string_lossy().into_owned();
                if !is_dir || name.starts_with('.') {
                    continue;
                }
                entries.push(json!({
                    "name": name,
                    "path": entry.path().to_string_lossy(),
                    "isDir": true,
                }));
            }
            entries.sort_by(|a, b| {
                a["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
            });
            Ok(json!({
                "path": path.to_string_lossy(),
                "parent": path.parent().map(|p| p.to_string_lossy()),
                "entries": entries,
            }))
        }
        "thread.archive" => {
            let thread_id = field(&p, "threadId")?.to_string();
            let thread = store
                .thread(&thread_id)
                .ok_or_else(|| anyhow::anyhow!("unknown thread"))?;
            let events = store.read_events(&thread_id);
            let terminals: Vec<ArchivedTerminal> = store
                .list_terminals(&thread.project_id)
                .into_iter()
                .map(|t| {
                    let path = crate::store::data_dir()
                        .join("terminals")
                        .join(format!("{}.scrollback", t.id));
                    let scrollback = std::fs::read(&path)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default();
                    ArchivedTerminal {
                        id: t.id,
                        name: t.name,
                        created_at: t.created_at,
                        scrollback,
                    }
                })
                .collect();
            let project = store.project(&thread.project_id);
            let project_name = project
                .as_ref()
                .map(|pr| pr.name.clone())
                .unwrap_or_default();
            let project_path = project
                .as_ref()
                .map(|pr| pr.path.clone())
                .unwrap_or_default();
            let header = ArchiveHeader {
                id: new_id(),
                thread_id: thread.id.clone(),
                title: thread.title.clone(),
                agent: thread.agent,
                project_id: thread.project_id.clone(),
                project_name,
                archived_at: now_iso(),
                updated_at: thread.updated_at.clone(),
                event_count: events.len() as u64,
                terminal_count: terminals.len() as u64,
            };
            let file = ArchiveFile {
                header,
                project_path,
                thread,
                events,
                terminals,
            };
            store.write_archive(&file)?;
            // Tear down the live thread, mirroring thread.delete.
            hub.stop_session(&thread_id);
            state.browsers.remove(&thread_id);
            store.delete_thread(&thread_id)?;
            hub.broadcast_state("threads", None);
            hub.broadcast_state("archives", None);
            Ok(json!({ "archive": file.header }))
        }
        "archive.list" => Ok(json!({ "archives": store.list_archives() })),
        "archive.restore" => {
            let archive_id = field(&p, "archiveId")?.to_string();
            let file = store.read_archive(&archive_id)?;
            let thread = store.restore_thread(file.thread, file.events)?;
            store.delete_archive(&archive_id)?;
            hub.broadcast_state("threads", None);
            hub.broadcast_state("archives", None);
            Ok(json!({ "thread": thread }))
        }
        "archive.delete" => {
            let archive_id = field(&p, "archiveId")?.to_string();
            store.delete_archive(&archive_id)?;
            hub.broadcast_state("archives", None);
            Ok(json!({}))
        }
        "settings.get" => Ok(json!({ "archiveDir": store.archive_dir().to_string_lossy() })),
        "settings.set" => {
            let dir = store.set_archive_dir(field(&p, "archiveDir")?)?;
            Ok(json!({ "archiveDir": dir.to_string_lossy() }))
        }
        other => anyhow::bail!("unknown request type: {other}"),
    }
}

#[cfg(test)]
mod replay_trim_tests {
    use super::*;

    fn tool_end(output: &str) -> PersistedEvent {
        PersistedEvent {
            seq: 1,
            ts: "2026-07-27T00:00:00Z".into(),
            speaker: None,
            event: AgentEvent::ToolEnd {
                call_id: "c1".into(),
                name: "Bash".into(),
                output: Some(output.into()),
                is_error: false,
                truncated: false,
            },
        }
    }

    fn parts(pe: &PersistedEvent) -> (String, bool) {
        match &pe.event {
            AgentEvent::ToolEnd {
                output, truncated, ..
            } => (output.clone().unwrap_or_default(), *truncated),
            _ => panic!("not a tool_end"),
        }
    }

    #[test]
    fn short_output_is_untouched() {
        let mut events = vec![tool_end("all good")];
        trim_replay_output(&mut events);
        assert_eq!(parts(&events[0]), ("all good".to_string(), false));
    }

    #[test]
    fn long_output_keeps_head_and_tail_and_flags_itself() {
        let original = format!("{}{}{}", "H".repeat(1_500), "M".repeat(50_000), "T".repeat(500));
        let mut events = vec![tool_end(&original)];
        trim_replay_output(&mut events);
        let (trimmed, truncated) = parts(&events[0]);
        assert!(truncated, "client needs to know it can fetch the rest");
        assert!(trimmed.len() < original.len() / 10);
        assert!(trimmed.starts_with(&"H".repeat(1_500)));
        assert!(trimmed.ends_with(&"T".repeat(500)));
        assert!(trimmed.contains("elided"));
    }

    /// The cut points are byte offsets into a UTF-8 string — landing mid-character
    /// would panic on slicing, so both ends round to a boundary.
    #[test]
    fn multibyte_output_does_not_split_a_character() {
        let original = "→".repeat(20_000); // 3 bytes each, so every cap lands mid-char
        let mut events = vec![tool_end(&original)];
        trim_replay_output(&mut events);
        let (trimmed, truncated) = parts(&events[0]);
        assert!(truncated);
        assert!(trimmed.starts_with('→') && trimmed.ends_with('→'));
    }
}

#[cfg(test)]
mod sidebar_image_tests {
    use super::optional_sidebar_image;
    use serde_json::json;

    #[test]
    fn accepts_supported_data_urls_and_null() {
        let image = "data:image/png;base64,iVBORw0KGgo=";
        assert_eq!(
            optional_sidebar_image(&json!({ "image": image }), "image").unwrap(),
            Some(image.into())
        );
        assert_eq!(
            optional_sidebar_image(&json!({ "image": null }), "image").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_untrusted_or_broken_image_values() {
        assert!(optional_sidebar_image(
            &json!({ "image": "data:image/svg+xml;base64,PHN2Zz4=" }),
            "image"
        )
        .is_err());
        assert!(optional_sidebar_image(
            &json!({ "image": "data:image/png;base64,not base64" }),
            "image"
        )
        .is_err());
    }
}

#[cfg(test)]
mod state_bounds {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    #[test]
    fn server_state_is_shareable() {
        assert_send_sync::<super::ServerState>();
    }
}

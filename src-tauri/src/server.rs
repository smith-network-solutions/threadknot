//! Axum server: serves the web UI, exposes /ws (token-gated) on the LAN so a phone
//! browser can drive agent sessions, and fans out hub events to every client.

use crate::agents::{claude, codex, kimi, Hub};
use crate::mobile::{Capability, MobileStore, Principal};
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
    /// Non-interactive command jobs (`exec.*`). Machine-local and in-memory: a
    /// job's process dies with this one, so a handle that outlived a restart
    /// could only ever resolve to a lie.
    pub exec: Arc<crate::exec::ExecRegistry>,
    /// Every live authenticated socket, so revoking a device (or narrowing its
    /// grants) closes the connections it already holds.
    pub sessions: Arc<crate::sessions::SessionRegistry>,
    /// Opaque cookie sessions for remote browsers (SEC-006).
    pub browser_sessions: Arc<crate::ingress::BrowserSessions>,
    /// Whether remote access is on, and the public origin it was given.
    pub remote: Arc<crate::remote::RemoteStore>,
    /// This machine's mesh certificate authority and leaf, for the TLS listener
    /// paired machines dial (SEC-012).
    pub mesh: Arc<crate::mesh::MeshIdentity>,
    /// Single-use challenges issued by `/api/peer/identity` and consumed by
    /// `/api/peer/pair`, so pairing proves the master token instead of sending it.
    pub pairing_challenges: Arc<crate::mesh::PairingChallenges>,
    /// The outbound relay connector (Stage 2): dials out, holds a multiplexed
    /// session, and forwards each inbound stream to the strict loopback ingress.
    pub connector: Arc<crate::connector::Connector>,
    /// Which listener this clone of the state serves. The strict ingress gets
    /// its own clone with `IngressPolicy::Remote`; nothing else differs.
    pub policy: crate::ingress::IngressPolicy,
}

impl ServerState {
    /// Resolve a presented token to a principal: the master token
    /// (`server.json`), a paired mobile device's revocable credential carrying
    /// whatever grants that device currently holds, or a paired machine's peer
    /// credential.
    ///
    /// A peer resolves to `Principal::Peer` with **no** asserted authority. The
    /// grants it acts with come from the request frame, not from the credential
    /// — see `mesh_principal`. Defaulting to "the peer's owner" here instead
    /// would mean any handler that forgot to consult the frame silently ran as
    /// Master again, which is the bug being fixed.
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        // Constant-time compare: the master token is a fixed secret, and a
        // byte-at-a-time `==` on it is exactly the kind of oracle a patient
        // attacker on the LAN can grind against.
        if !self.config.token.is_empty() && constant_time_eq(token, &self.config.token) {
            return Some(Principal::Master);
        }
        if let Some(grant) = self.mobile.authenticate(token) {
            return Some(Principal::Device(grant));
        }
        self.peernet
            .registry
            .authenticate(token)
            .map(|machine_id| {
                Principal::Peer(crate::mobile::PeerPrincipal {
                    machine_id,
                    // Narrowest possible starting point: no grants at all until
                    // a frame says otherwise.
                    on_behalf_of: Some(Vec::new()),
                })
            })
    }

    /// The TLS port paired machines dial. LAN port + 2 — the strict remote
    /// ingress already took +1.
    pub fn mesh_port(&self) -> u16 {
        self.config.port.saturating_add(2)
    }

    /// The principal for a device id, read fresh so a cookie session never
    /// carries authority the owner has since taken away.
    fn principal_for_device(&self, device_id: &str) -> Option<Principal> {
        self.mobile.grant_for(device_id).map(Principal::Device)
    }

    /// Resolve an incoming HTTP/WebSocket request under this listener's policy.
    ///
    /// Everything that authenticates goes through here, so "what does the
    /// strict ingress accept" is answered in exactly one place rather than at
    /// each of the seven routes that used to read `?token=` for themselves.
    pub fn authenticate_ingress(
        &self,
        headers: &axum::http::HeaderMap,
        params: &HashMap<String, String>,
    ) -> Result<crate::ingress::Authenticated, crate::ingress::Rejection> {
        // The mesh listener is not gated on the remote switch: the mesh is the
        // LAN product and works whether or not this machine is published.
        if self.policy.is_remote() && !self.remote.enabled() {
            return Err(crate::ingress::Rejection {
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
                message: "remote access is turned off for this machine",
            });
        }
        crate::ingress::authenticate(
            self.policy,
            headers,
            params,
            |token| self.authenticate(token),
            |cookie| {
                self.browser_sessions
                    .resolve(cookie)
                    .and_then(|device_id| self.principal_for_device(&device_id))
            },
        )
    }

    /// Close every live socket held by `device_id`, and drop its browser
    /// sessions — the enforcement half of a revoke or a capability reduction.
    pub fn close_device_sessions(&self, device_id: &str) {
        let closed = self.sessions.close_device(device_id);
        let cookies = self.browser_sessions.revoke_device(device_id);
        if closed > 0 || cookies > 0 {
            tracing::info!(
                "device {device_id}: closed {closed} live socket(s), dropped {cookies} browser session(s)"
            );
        }
    }

    /// Human-readable server name shown in the mobile app's server list.
    pub fn server_name(&self) -> String {
        gethostname::gethostname().to_string_lossy().into_owned()
    }

    /// Origin only — the LAN URL with its `?token=…` stripped. This is what a
    /// QR pairing payload carries, so scanning a screen never leaks the master
    /// token the way photographing the printed LAN URL would.
    pub fn lan_origin(&self) -> String {
        self.lan_url
            .split('?')
            .next()
            .unwrap_or(&self.lan_url)
            .trim_end_matches('/')
            .to_string()
    }
}

/// Compare two secrets without leaking where they first differ.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Hosts whose pages are always allowed to open a Threadknot WebSocket: the
/// Tauri webview's own origin (which is never the server's host) and loopback,
/// which covers `npm run dev` driving the real backend.
const TRUSTED_WS_HOSTS: &[&str] = &["tauri.localhost", "localhost", "127.0.0.1", "::1"];

/// Whether a browser is allowed to open a WebSocket to us from this page.
///
/// Native clients (the Expo shell's fetch, curl, the peer mesh) send no
/// `Origin` at all and are unaffected — the token is the credential there. What
/// this stops is a *page on another site*, opened in a browser that can reach
/// the LAN, silently attaching to `/ws`, `/term` or `/browser`. Same-origin
/// pages (the bundled UI, a phone browser on the LAN URL, a tunnelled host) all
/// present an `Origin` equal to the `Host` they connected to.
pub(crate) fn ws_origin_allowed(headers: &axum::http::HeaderMap) -> bool {
    let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        // No Origin: not a browser navigation-initiated request.
        return true;
    };
    let Ok(origin) = url::Url::parse(origin) else {
        // Includes the literal `null` origin a sandboxed frame sends.
        return false;
    };
    // Tauri serves the desktop UI from its own scheme on macOS/Linux.
    if origin.scheme() == "tauri" {
        return true;
    }
    let Some(host) = origin.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if TRUSTED_WS_HOSTS.contains(&host) {
        return true;
    }
    // Otherwise it must be the very host this request was addressed to.
    headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.rsplit_once(':').map(|(name, _)| name).unwrap_or(h))
        .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
        .is_some_and(|h| h.eq_ignore_ascii_case(host))
}

/// The deep link a pairing QR encodes. `threadknot://` (the Expo app's scheme)
/// rather than an http URL: an http QR would be opened by any camera app as a
/// web page, and this payload is only meaningful to the mobile app.
pub fn pairing_payload(origin: &str, code: &str) -> String {
    format!(
        "threadknot://pair?u={}&c={}",
        urlencoding::encode(origin),
        urlencoding::encode(code)
    )
}

/// Render `payload` as a standalone SVG QR. Returned as markup rather than a
/// raster so it stays crisp at whatever size the settings dialog gives it.
fn pairing_qr_svg(payload: &str) -> anyhow::Result<String> {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};
    // Medium EC: a screen is a clean scanning surface, and the extra data
    // capacity keeps the module count (and so the density) low.
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::M)
        .context("encode pairing QR")?;
    Ok(code
        .render()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        // Light-on-dark would invert the code; scanners want dark modules on a
        // light field, so the QR keeps its own colors regardless of the theme.
        .dark_color(svg::Color("#0b0d12"))
        .light_color(svg::Color("#ffffff"))
        .build())
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
    // A dispatched worker reports home from a turn boundary deep inside a
    // driver, which has no ServerState — so the hub needs its own handle to the
    // mesh. Attached here for the same reason push is: `Hub::new` runs before
    // the peer runtime exists.
    state.hub.attach_peernet(Arc::clone(&state.peernet));
    // Either direction of the link is enough to finish a dispatch: the worker
    // pushes its report, and the parent also asks. See `spawn_reconciler`.
    crate::dispatch::spawn_reconciler(state.clone());

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
    crate::schedules::spawn_scheduler(state.clone());

    // Background "is a newer master out?" poller (pulses the settings gear).
    crate::update::spawn_poller(Arc::clone(&state.hub));

    // Mesh runtime: peer sockets + presence + announce + mDNS discovery.
    state.peernet.start();

    let app = build_router(state.clone());

    // The strict ingress. Bound unconditionally because it is loopback-only —
    // binding it exposes nothing, and having the socket always present is what
    // makes turning remote access on and off instant rather than a restart.
    // Everything it serves is refused while remote access is off.
    spawn_remote_listener(&state);

    // The mesh listener. TLS, and the only door a paired machine may use.
    spawn_mesh_listener(&state);

    // The relay connector. Dials out; nothing here listens. It is started
    // unconditionally and returns to waiting when disabled, so flipping the
    // switch takes effect without a restart.
    state.connector.attach_hub(Arc::clone(&state.hub));
    state.connector.attach_remote(Arc::clone(&state.remote));
    state.connector.start();

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

/// Bind the loopback-only listener the connector dials.
///
/// A failure here is logged and dropped rather than fatal: the LAN product must
/// keep working on a machine where something else already holds that port.
fn spawn_remote_listener(state: &ServerState) {
    let mut remote_state = state.clone();
    remote_state.policy = crate::ingress::IngressPolicy::Remote;
    let port = state.remote.port();
    tokio::spawn(async move {
        // 127.0.0.1 explicitly, never 0.0.0.0: the strict policy must not be
        // something a caller on the LAN can choose to be judged by.
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!("threadknot remote ingress on {addr} (loopback only)");
                let router = build_router(remote_state);
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!("remote ingress error: {e}");
                }
            }
            Err(e) => tracing::warn!("remote ingress not available on {addr}: {e}"),
        }
    });
}

/// Bind the TLS listener paired machines dial (SEC-012).
///
/// Failure is logged, not fatal, for the same reason the strict listener's is:
/// a machine where something else holds that port must still work as a solo
/// desktop. What it loses is the mesh, and the peer list says so.
fn spawn_mesh_listener(state: &ServerState) {
    let mut mesh_state = state.clone();
    mesh_state.policy = crate::ingress::IngressPolicy::Mesh;
    let port = state.mesh_port();
    let acceptor = match state.mesh.server_config() {
        Ok(config) => tokio_rustls::TlsAcceptor::from(config),
        Err(e) => {
            tracing::error!("mesh listener disabled — cannot build TLS config: {e:#}");
            return;
        }
    };
    tokio::spawn(async move {
        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("mesh listener not available on {addr}: {e}");
                return;
            }
        };
        tracing::info!("threadknot mesh listener on {addr} (TLS, peer credentials only)");
        let router = build_router(mesh_state);
        let service = hyper_util::service::TowerToHyperService::new(router);
        loop {
            let Ok((tcp, _remote)) = listener.accept().await else {
                continue;
            };
            let acceptor = acceptor.clone();
            let service = service.clone();
            // One task per connection, and a handshake failure is a debug line
            // and nothing else: anything can connect to a TLS port and send
            // garbage, and logging that at warn level hands a passer-by the
            // ability to fill this machine's disk.
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let io = hyper_util::rt::TokioIo::new(tls);
                // Both HTTP/1.1 and h2: the byte proxy is plain HTTP requests
                // while `/ws`, `/term` and `/browser` need HTTP/1.1's upgrade
                // mechanism, and `auto` picks per connection via ALPN.
                let builder =
                    hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
                if let Err(e) = builder.serve_connection_with_upgrades(io, service).await {
                    tracing::debug!("mesh connection ended: {e}");
                }
            });
        }
    });
}

/// The whole HTTP/WebSocket surface, assembled without binding a port — so the
/// authorization matrix can drive the real handlers rather than a stand-in.
pub fn build_router(state: ServerState) -> Router {
    let mut app = Router::new()
        .route("/ws", any(ws_handler))
        .route("/attachment", get(attachment_handler))
        .route("/artifact-file", get(artifact_file_handler))
        .route("/file", get(crate::files::file_handler))
        .route("/term", any(crate::term::ws_handler))
        .route("/browser", any(crate::browser::ws_handler))
        .route("/api/server-info", get(server_info_handler))
        .route("/api/mobile/pair", post(mobile_pair_handler))
        .route("/api/mobile/push", post(mobile_push_handler))
        .route("/api/mobile/push/test", post(mobile_push_test_handler))
        .route("/api/mobile/unpair", post(mobile_unpair_handler))
        // Exchange a pairing proof or a native device bearer for an opaque
        // cookie, so a remote browser never has to hold a credential it can
        // leak through a URL, a screenshot or an extension.
        .route(
            "/api/session",
            post(session_bootstrap_handler).delete(session_signout_handler),
        );

    // `/mcp` hands a local agent this machine's tool surface and
    // `/api/peer/identity` is the mesh pairing bootstrap. Neither is something a
    // relay has any business carrying, so they are absent from the strict router
    // rather than merely guarded on it.
    if !state.policy.is_remote() {
        app = app
            .route("/mcp", any(crate::mcp::mcp_handler))
            // Public identity: a certificate and a machine id, nothing secret.
            // It lives on the plain-HTTP listener because it is the bootstrap —
            // the caller has no certificate to verify against yet, which is
            // precisely what this hands them.
            .route("/api/peer/identity", get(peer_identity_handler));
    }

    // Pairing completes on the mesh listener, so the exchange is inside TLS.
    // It authenticates by proof rather than by peer credential — a machine being
    // paired for the first time does not have one yet — which is why it is a
    // route rather than something `authenticate_ingress` handles.
    if state.policy.is_mesh() {
        app = app.route("/api/peer/pair", post(peer_pair_handler));
    }

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

    let app = app.layer(axum::middleware::from_fn(cache_policy));
    // Permissive CORS is a LAN convenience (curl, scripts, the dev server). The
    // strict ingress ships no CORS headers at all, so a browser refuses to hand
    // another origin's script the response — which is the only defence that
    // still means anything once authentication is a cookie.
    let app = if state.policy.is_remote() {
        // The remote-off gate as a LAYER, not only inside `authenticate_ingress`.
        //
        // Two reasons it has to be here. The static bundle is served by
        // `fallback_service`, which never calls `authenticate_ingress` — it
        // cannot, since the shell must load before anyone can sign in — so with
        // the check only at authentication time, turning remote access off still
        // served 5 MB of JavaScript to anyone who resolved the hostname, and
        // charged it to the owner's fair-use allowance. And a route added later
        // that forgets to authenticate is covered by construction rather than by
        // remembering.
        app.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            remote_enabled_gate,
        ))
        .layer(axum::middleware::from_fn(remote_security_headers))
    } else {
        app.layer(tower_http::cors::CorsLayer::permissive())
    };
    app.with_state(state)
}

/// Refuse everything on the strict ingress while remote access is off.
///
/// "Off" has to mean off for *every* byte, not only for authenticated ones.
async fn remote_enabled_gate(
    State(state): State<ServerState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !state.remote.enabled() {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "remote access is turned off for this machine",
        )
            .into_response();
    }
    next.run(req).await
}

/// Response headers for the public origin.
///
/// Only on the strict ingress: HSTS on a plain-http LAN address would pin a
/// browser to https for an origin that has none, which bricks the LAN UI for
/// six months.
async fn remote_security_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{header, HeaderName, HeaderValue};
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    let mut put = |name: HeaderName, value: &'static str| {
        headers.insert(name, HeaderValue::from_static(value));
    };
    put(
        header::STRICT_TRANSPORT_SECURITY,
        "max-age=31536000; includeSubDomains",
    );
    // No referrer at all: the URL alone tells another site which installation
    // and which thread someone is looking at.
    put(header::REFERRER_POLICY, "no-referrer");
    put(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    // `data:`/`blob:` are load-bearing here — sidebar art is stored as a data
    // URL and recordings play from a blob. `frame-ancestors 'none'` is what
    // stops a hostile page framing the UI and driving it with the user's cookie.
    put(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self';          script-src 'self';          style-src 'self' 'unsafe-inline';          img-src 'self' data: blob:;          media-src 'self' data: blob:;          font-src 'self' data:;          connect-src 'self' ws: wss:;          object-src 'none';          base-uri 'none';          form-action 'none';          frame-ancestors 'none'",
    );
    resp
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
    on_behalf_of: Option<Vec<Capability>>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let Some(peer) = state.peernet.registry.peer(machine_id) else {
        return (StatusCode::NOT_FOUND, "unknown machine").into_response();
    };
    if peer.needs_upgrade() {
        return (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} was paired before Threadknot encrypted mesh connections — update Threadknot \
                 on that machine, then pair the two again",
                peer.name
            ),
        )
            .into_response();
    }
    let Some(addr) = peer
        .last_good_address
        .clone()
        .or_else(|| peer.addresses.first().cloned())
    else {
        return (StatusCode::BAD_GATEWAY, "no known address for that machine").into_response();
    };
    // Every credential-bearing key is stripped and none is added: the peer's
    // credential rides an `Authorization` header, so nothing authenticating
    // appears in this URL at all.
    let query: Vec<(String, String)> = params
        .iter()
        .filter(|(k, _)| !crate::ingress::is_credential_query_key(k) && k.as_str() != "machineId")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let client = match state.peernet.byte_client(&peer, &addr) {
        Ok(client) => client,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("peer TLS setup failed: {e:#}"))
                .into_response()
        }
    };
    // The synthetic mesh name, resolved to `addr` by the client built above, so
    // the certificate check proves identity while the address stays disposable.
    let url = format!(
        "https://{}:{}{endpoint}",
        crate::mesh::mesh_dns_name(&peer.machine_id),
        peer.mesh_port
    );
    let mut request = client
        .get(&url)
        .query(&query)
        .timeout(std::time::Duration::from_secs(60))
        .header("authorization", format!("Bearer {}", peer.outbound_credential));
    if let Some(value) = crate::ingress::mesh_grants_header_value(on_behalf_of.as_deref()) {
        request = request.header(crate::ingress::MESH_GRANTS_HEADER, value);
    }
    let resp = match request.send().await {
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
///
/// The `mesh` check runs HERE, on this side, so a device that may not reach
/// other machines is refused where its identity is still known. It is no longer
/// the *only* check — the caller's grants now travel with the request and the far
/// side enforces them too — but keeping it means a denial is reported by the
/// machine the person is actually talking to.
pub(crate) async fn maybe_proxy_bytes(
    state: &ServerState,
    principal: &Principal,
    endpoint: &str,
    params: &HashMap<String, String>,
) -> Option<axum::response::Response> {
    let mid = params.get("machineId")?;
    if mid == &state.device.machine_id {
        return None;
    }
    if let Err(e) = principal.require(Capability::Mesh) {
        return Some(forbidden(e));
    }
    Some(proxy_peer_bytes(state, mid, endpoint, params, principal.mesh_assertion()).await)
}

/// A capability refusal as an HTTP response. Deliberately says *what* was
/// missing: the person holding the phone can only fix this on the desktop, and
/// a silent 404 would read as a bug.
pub(crate) fn forbidden(error: anyhow::Error) -> axum::response::Response {
    (axum::http::StatusCode::FORBIDDEN, format!("{error:#}")).into_response()
}

/// Resolve the principal for a byte endpoint and check the grant it needs.
/// `Err` is the response to return.
pub(crate) fn authorize_bytes(
    state: &ServerState,
    headers: &axum::http::HeaderMap,
    params: &HashMap<String, String>,
    capability: Capability,
) -> Result<Principal, Box<axum::response::Response>> {
    let authenticated = state
        .authenticate_ingress(headers, params)
        .map_err(|r| Box::new(r.into_response()))?;
    authenticated
        .principal
        .require(capability)
        .map_err(|e| Box::new(forbidden(e)))?;
    Ok(authenticated.principal)
}

/// Serve a stored attachment (token-gated) so the transcript can render
/// thumbnails on reload. `GET /attachment?thread=…&id=…&token=…`.
async fn attachment_handler(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::http::StatusCode;
    // An attachment is chat content, so it rides with the `threads` grant
    // rather than with project file access.
    let principal = match authorize_bytes(&state, &headers, &params, Capability::Threads) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    if let Some(resp) = maybe_proxy_bytes(&state, &principal, "/attachment", &params).await {
        return resp;
    }
    let (Some(thread), Some(id)) = (params.get("thread"), params.get("id")) else {
        return (StatusCode::BAD_REQUEST, "missing params").into_response();
    };
    let Some(path) = state.hub.store.attachment_path(thread, id) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // Streamed, not read into memory: see `files::stream_range`.
    let Ok((file, total)) = open_with_len(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, crate::store::mime_for_ext(ext))
        .header(axum::http::header::CONTENT_LENGTH, total)
        .body(crate::files::stream_range(file, 0, total))
    {
        Ok(r) => r,
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "response error").into_response(),
    }
}

/// Open a file and read its length in one step, so a streamed response can still
/// declare `Content-Length`.
fn open_with_len(path: &std::path::Path) -> std::io::Result<(std::fs::File, u64)> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    // A directory opens successfully on Unix and only fails on the first read.
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a file",
        ));
    }
    Ok((file, meta.len()))
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
    let principal = match authorize_bytes(&state, &headers, &params, Capability::Files) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    if let Some(resp) = maybe_proxy_bytes(&state, &principal, "/artifact-file", &params).await {
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
    // Streamed, not read into memory: see `files::stream_range`. This one matters
    // most — an artifact is often a screen recording, and a range request from a
    // <video> element used to read the entire file to serve a few hundred KB of it.
    let Ok((file, total)) = open_with_len(&path) else {
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

    let (status, start, len) = match range {
        Some((start, end)) => {
            resp = resp.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            );
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
        None => (StatusCode::OK, 0, total),
    };
    resp = resp.header(header::CONTENT_LENGTH, len);
    match resp.status(status).body(crate::files::stream_range(file, start, len)) {
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

/// The grant set a Master-authenticated payload asked for, defaulting to
/// [`crate::mobile::default_capabilities`] when the key is absent. Unknown names
/// are dropped, so a client can never widen the set by inventing one.
fn requested_capabilities(payload: &Value) -> Vec<Capability> {
    match payload.get("capabilities").and_then(|v| v.as_array()) {
        Some(list) => crate::mobile::normalize_capabilities(
            list.iter()
                .filter_map(|v| v.as_str())
                .filter_map(Capability::parse),
        ),
        None => crate::mobile::default_capabilities(),
    }
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
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(rejection) = state.authenticate_ingress(&headers, &params) {
        return rejection.into_response();
    }
    axum::Json(json!({
        "app": "threadknot",
        "version": env!("THREADKNOT_VERSION"),
        "serverId": state.config.server_id,
        "name": state.server_name(),
    }))
    .into_response()
}

/// `GET /api/peer/identity` — the public half of pairing.
///
/// Unauthenticated and deliberately so: everything it returns is public
/// information — a machine id, a display name, a certificate authority, a port.
/// A certificate is not a secret; its whole job is to be handed out.
///
/// It also mints a single-use challenge. That is what turns the *next* request
/// into a proof of the master token rather than a transmission of it, and what
/// binds the proof to the certificate the caller actually saw. See
/// `mesh::pairing_proof`.
async fn peer_identity_handler(State(state): State<ServerState>) -> axum::response::Response {
    axum::Json(json!({
        "machineId": state.device.machine_id,
        "name": state.device.friendly_name(),
        "avatar": state.device.avatar(),
        "color": state.device.color(),
        "profileUpdatedAt": state.device.profile_updated_at(),
        "port": state.config.port,
        "meshPort": state.mesh_port(),
        "meshCa": state.mesh.ca_pem,
        "meshVersion": crate::device::MESH_VERSION,
        "addresses": crate::peernet::local_addresses(),
        "challenge": state.pairing_challenges.issue(),
    }))
    .into_response()
}

/// `POST /api/peer/pair` — the receiving half of mutual pairing, over TLS.
///
/// The exchange this replaced sent each machine's **master token** to the other
/// and stored it as the peer credential forever. Now:
///
/// * the initiator proves it knows our master token without sending it, and the
///   proof is bound to the certificate it saw, so intercepting the identity
///   fetch and substituting a certificate does not yield a usable proof;
/// * what is actually exchanged is a pair of freshly minted, independently
///   rotatable per-link credentials;
/// * it runs on the mesh listener, so the whole exchange is inside TLS.
///
/// Mounted on the mesh router but authenticating by proof rather than by peer
/// credential — a machine being paired for the first time does not have one yet.
async fn peer_pair_handler(
    State(state): State<ServerState>,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let get = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (machine_id, challenge, proof) = (get("machineId"), get("challenge"), get("proof"));

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
    if machine_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing machineId").into_response();
    }
    if machine_id == state.device.machine_id {
        return (StatusCode::BAD_REQUEST, "a machine cannot pair with itself").into_response();
    }
    // Challenge first: single-use and short-lived, so a captured proof is dead
    // before it can be replayed and there is nothing to grind against.
    if !state.pairing_challenges.redeem(&challenge) {
        return (
            StatusCode::UNAUTHORIZED,
            "pairing challenge expired or already used — start pairing again",
        )
            .into_response();
    }
    let expected = crate::mesh::pairing_proof(
        &state.config.token,
        &challenge,
        &machine_id,
        &state.mesh.ca_pem,
    );
    if !crate::mesh::constant_time_eq(&expected, &proof) {
        return (
            StatusCode::UNAUTHORIZED,
            "pairing proof did not verify — check the token, and that nothing is intercepting the \
             connection",
        )
            .into_response();
    }

    let peer_ca = get("meshCa");
    let peer_mesh_port = body.get("meshPort").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let their_credential_for_us = get("credentialForYou");
    if peer_ca.is_empty() || peer_mesh_port == 0 || their_credential_for_us.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "missing meshCa/meshPort/credentialForYou",
        )
            .into_response();
    }
    // Refuse a CA we cannot build a trust anchor from, here rather than at the
    // first connection attempt: a pair that looks successful and then never
    // connects is a much worse failure than a pairing that says no.
    if let Err(e) = crate::mesh::client_config_for_ca(&peer_ca) {
        return (
            StatusCode::BAD_REQUEST,
            format!("that machine sent an unusable certificate: {e:#}"),
        )
            .into_response();
    }

    let addresses: Vec<String> = body
        .get("addresses")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    // The credential THEY will present to US. Minted here, hashed here, and the
    // plaintext leaves exactly once — in the response below.
    let our_credential_for_them = crate::mesh::mint_credential();
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
        outbound_credential: their_credential_for_us,
        inbound_credential_hash: crate::mesh::hash_credential(&our_credential_for_them),
        mesh_ca: peer_ca,
        mesh_port: peer_mesh_port,
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
                "meshPort": state.mesh_port(),
                "meshCa": state.mesh.ca_pem,
                "meshVersion": crate::device::MESH_VERSION,
                "addresses": crate::peernet::local_addresses(),
                // Their half of the exchange. Sent once, over TLS, and stored
                // here only as a hash.
                "credentialForYou": our_credential_for_them,
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/mobile/pair` — proof of physical access in, revocable device
/// credential out. That proof is either the master token (pasted LAN URL) or a
/// one-time `pairingCode` scanned off the desktop's QR. The phone stores only
/// the device credential; with the QR path it never sees the master token at all.
async fn mobile_pair_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let scanned = body.get("pairingCode").and_then(|v| v.as_str());
    // Authority comes from the pairing proof, never from the joining client's
    // request body: a scanned code carries exactly the grants the owner bound
    // to it, and only a caller already holding the master token may name a set.
    let capabilities = match scanned {
        Some(code) => match state.mobile.redeem_pairing(code) {
            Some(granted) => granted,
            None => {
                // One message for wrong/expired/already-used: which one it was
                // is not something an unauthenticated caller should learn.
                return (
                    StatusCode::UNAUTHORIZED,
                    "that pairing code is expired or already used — show a fresh QR",
                )
                    .into_response();
            }
        },
        None => {
            // The master-token path is a LAN convenience (paste the printed
            // URL). Remotely there is no legitimate way to be holding that
            // token, so a scanned code is the only accepted proof.
            if state.policy.is_remote() {
                return (
                    StatusCode::FORBIDDEN,
                    "pair with a one-time code — the master token is not accepted remotely",
                )
                    .into_response();
            }
            let Some(token) = bearer_or_field(&headers, &body, "token") else {
                return (StatusCode::UNAUTHORIZED, "missing token").into_response();
            };
            if state.authenticate(&token) != Some(Principal::Master) {
                return (StatusCode::UNAUTHORIZED, "pairing requires the server's master token")
                    .into_response();
            }
            requested_capabilities(&body)
        }
    };
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
    match state.mobile.pair(name, platform, capabilities) {
        Ok((device, credential)) => {
            state.hub.broadcast_state("mobileDevices", None);
            axum::Json(json!({
                "serverId": state.config.server_id,
                "serverName": state.server_name(),
                "version": env!("THREADKNOT_VERSION"),
                "deviceId": device.id,
                "credential": credential,
                "capabilities": device
                    .capabilities
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// `POST /api/session` — pairing proof or native bearer in, opaque cookie out.
///
/// This is the endpoint that lets credentials leave URLs (SEC-006). A remote
/// browser presents a one-time pairing code exactly once; from then on it holds
/// an `HttpOnly` cookie it cannot read, cannot copy out of the address bar, and
/// cannot leak through a `Referer`. The CSRF token in the response body is the
/// half the page *is* meant to hold, and is derived from the cookie, so a page
/// on another origin cannot compute it.
///
/// Strict ingress only. The cookie is `Secure`, so a browser on the plain-http
/// LAN address would silently drop it; the LAN keeps its query token during the
/// migration window instead of getting a session that half works.
async fn session_bootstrap_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    if !state.policy.is_remote() {
        return (
            StatusCode::NOT_FOUND,
            "browser sessions are for remote connections; this one authenticates by token",
        )
            .into_response();
    }
    if !state.remote.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "remote access is turned off for this machine",
        )
            .into_response();
    }
    if !ws_origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }

    // Either a scanned pairing code (first visit) or the credential a native
    // shell already holds (re-establishing a session for its WebView).
    let device = match body.get("pairingCode").and_then(|v| v.as_str()) {
        Some(code) => {
            let Some(capabilities) = state.mobile.redeem_pairing(code) else {
                return (
                    StatusCode::UNAUTHORIZED,
                    "that pairing code is expired or already used — show a fresh QR",
                )
                    .into_response();
            };
            let name = body
                .get("deviceName")
                .and_then(|v| v.as_str())
                .unwrap_or("Remote browser")
                .chars()
                .take(64)
                .collect::<String>();
            let platform = body
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("browser")
                .chars()
                .take(16)
                .collect::<String>();
            match state.mobile.pair(name, platform, capabilities) {
                Ok((device, _credential)) => {
                    // The plaintext credential is deliberately dropped here. A
                    // browser gets a cookie; only the native shell, which has a
                    // keychain to put it in, gets a bearer token.
                    state.hub.broadcast_state("mobileDevices", None);
                    device
                }
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
                }
            }
        }
        None => {
            let Some(token) = crate::ingress::bearer(&headers) else {
                return (StatusCode::UNAUTHORIZED, "missing pairing code or credential")
                    .into_response();
            };
            match state.authenticate(&token) {
                Some(Principal::Device(grant)) => match state.mobile.device(&grant.id) {
                    Some(device) => device,
                    None => {
                        return (StatusCode::UNAUTHORIZED, "unknown or revoked device")
                            .into_response()
                    }
                },
                // Including a valid master token: it never becomes a remote session.
                _ => {
                    return (
                        StatusCode::FORBIDDEN,
                        "only a paired device credential can open a remote session",
                    )
                        .into_response()
                }
            }
        }
    };

    let cookie = state.browser_sessions.issue(&device.id);
    let csrf = crate::ingress::csrf_token(&cookie);
    let body = json!({
        "deviceId": device.id,
        "deviceName": device.name,
        "capabilities": device.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "csrf": csrf,
        "serverId": state.config.server_id,
        "serverName": state.server_name(),
        "version": env!("THREADKNOT_VERSION"),
    });
    (
        [(
            axum::http::header::SET_COOKIE,
            crate::ingress::session_cookie_header(&cookie),
        )],
        axum::Json(body),
    )
        .into_response()
}

/// `DELETE /api/session` — sign this browser out and drop the stored session.
async fn session_signout_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Some(cookie) = crate::ingress::cookie_value(&headers, crate::ingress::SESSION_COOKIE) {
        if let Some(device_id) = state.browser_sessions.resolve(&cookie) {
            state.browser_sessions.revoke_device(&device_id);
            state.sessions.close_device(&device_id);
        }
    }
    (
        [(
            axum::http::header::SET_COOKIE,
            crate::ingress::clear_cookie_header(),
        )],
        axum::Json(json!({})),
    )
        .into_response()
}

/// Resolve the calling device from its credential (mobile-only endpoints).
fn device_from_request(
    state: &ServerState,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> Result<String, Box<axum::response::Response>> {
    use axum::http::StatusCode;
    // A remote browser authenticates by cookie, which the browser attaches on
    // the strength of the URL alone — so it, and only it, needs the
    // double-submit proof that the caller is a page we served.
    if let Some(cookie) = crate::ingress::cookie_value(headers, crate::ingress::SESSION_COOKIE) {
        if let Some(device_id) = state.browser_sessions.resolve(&cookie) {
            if !crate::ingress::csrf_ok(crate::ingress::AuthKind::Cookie, headers) {
                return Err(Box::new(
                    (StatusCode::FORBIDDEN, "missing or stale CSRF token").into_response(),
                ));
            }
            return Ok(device_id);
        }
    }
    let token = if state.policy.is_remote() {
        // No body-field credentials remotely: the header is the one place a
        // credential can travel without ending up somewhere it gets copied.
        crate::ingress::bearer(headers)
    } else {
        bearer_or_field(headers, body, "credential")
    }
    .ok_or_else(|| {
        Box::new((StatusCode::UNAUTHORIZED, "missing credential").into_response())
    })?;
    match state.authenticate(&token) {
        Some(Principal::Device(grant)) => Ok(grant.id),
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
    let previews = body.get("notificationPreviews").and_then(|v| v.as_bool());
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
        if let Some(v) = previews {
            d.notification_previews = v;
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
        notice: None,
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
            state.close_device_sessions(&device_id);
            state.hub.broadcast_state("mobileDevices", None);
            axum::Json(json!({})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    if !ws_origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let principal = match state.authenticate_ingress(&headers, &params) {
        Ok(authenticated) => authenticated.principal,
        Err(rejection) => return rejection.into_response(),
    };
    // Admitted before the upgrade, because a refusal after it can only be a
    // close frame with no explanation in it (SEC-014).
    let guard = match state.sessions.try_register(
        &principal,
        state.policy,
        crate::sessions::SessionKind::App,
    ) {
        Ok(guard) => guard,
        Err(e) => return (StatusCode::TOO_MANY_REQUESTS, format!("{e:#}")).into_response(),
    };
    // A frame larger than this is refused from its header, before its payload is
    // read, so an oversized frame costs a disconnect rather than an allocation.
    ws.max_message_size(crate::limits::MAX_WS_MESSAGE_BYTES)
        .max_frame_size(crate::limits::MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            // Revoking the device (or narrowing its grants) drops this future,
            // which drops the socket — the connection closes without waiting for
            // the client to notice.
            tokio::select! {
                _ = handle_socket(socket, state.clone(), principal.clone()) => {}
                _ = guard.closed() => {
                    tracing::info!("closing app socket: session revoked");
                }
            }
        })
        .into_response()
}

/// How many requests one socket may have in flight at once. High enough that
/// the client's boot fan-out never queues, low enough that a wedged client
/// can't spawn unbounded work.
const MAX_INFLIGHT_REQUESTS: usize = 32;

/// A spawned task that must not outlive the socket it serves.
///
/// Dropping a `JoinHandle` only detaches the task, and both of this socket's
/// helpers park on channels that a revoked connection never closes — so without
/// this, "revoke closes the socket" would leave the writer holding the sink open
/// and the fan-out subscribed to the hub forever.
struct TaskGuard(tokio::task::JoinHandle<()>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn handle_socket(socket: WebSocket, state: ServerState, principal: Principal) {
    use crate::limits::{self, Enqueued, FrameClass};

    let (mut sink, mut stream) = socket.split();
    let mut events = state.hub.broadcast.subscribe();
    let mut relayed = state.hub.relay.subscribe();

    // Bounded, and the bound is the point (SEC-014). This channel used to be
    // unbounded, so a client that stopped reading its socket — a phone whose
    // screen locked mid-turn, a tab the OS froze, a relay hop that stalled —
    // grew this process's heap for as long as the agent kept talking. That heap
    // is on the workstation its owner is sitting at.
    let (out_tx, mut out_rx) =
        tokio::sync::mpsc::channel::<String>(limits::OUTBOUND_QUEUE_FRAMES);

    // Set when a frame that MUST be delivered could not be queued. See the
    // policy note on `crate::limits::enqueue`: a response cannot be dropped,
    // because the client is blocked on its id and would wait forever, and a
    // persisted event cannot be dropped, because it carries status the UI has no
    // other way to learn. Both therefore end the connection instead, which is
    // recoverable — the client reconnects, replays and reissues.
    let hangup = Arc::new(tokio::sync::Notify::new());

    // The thread this socket is currently displaying, learned from its
    // `thread.get` calls. Streaming deltas (seq < 0) for any OTHER thread are
    // discarded by the client anyway, so sending them is pure wasted bandwidth
    // — on a phone or a tunnel that firehose is what starves the response the
    // user is actually waiting on.
    let viewing: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Writer: nothing but "take the next queued frame and write it". It is
    // deliberately the only task that touches the sink and the only one that
    // blocks on a slow client, so the fan-out below stays free to keep draining
    // the hub's broadcast — which is what stops a stalled sink turning into
    // indiscriminate broadcast lag that loses persisted events along with deltas.
    let _writer = TaskGuard(tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    }));

    // Fan-out: local broadcasts and frames relayed from peer machines
    // (pre-serialized, origin-tagged), classified and queued under the policy
    // for their class.
    let _fanout = TaskGuard(tokio::spawn({
        let out_tx = out_tx.clone();
        let viewing = Arc::clone(&viewing);
        let hangup = Arc::clone(&hangup);
        async move {
            loop {
                let (text, class) = tokio::select! {
                    ev = events.recv() => match ev {
                        Ok(ev) => {
                            let class = match &ev {
                                // Persisted events (seq >= 0) always go out: they drive
                                // status, attention badges and notifications for threads
                                // the client isn't looking at. Deltas do not.
                                ServerMessage::Event { thread_id, seq, .. } if *seq < 0 => {
                                    if viewing.lock().unwrap().as_deref()
                                        != Some(thread_id.as_str())
                                    {
                                        continue;
                                    }
                                    FrameClass::Delta
                                }
                                _ => FrameClass::Persisted,
                            };
                            match serde_json::to_string(&ev) {
                                Ok(text) => (text, class),
                                Err(_) => continue,
                            }
                        }
                        // Lagged means the hub outran this socket by more than the
                        // shared ring, and there is no way to tell whether what it
                        // skipped was deltas or persisted state. Disconnecting on it
                        // would put any client slower than a streaming turn into a
                        // reconnect loop, so this stays a gap the next `thread.get`
                        // repairs — the queue below is what the bound is for.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::debug!("app socket lagged the hub by {n} event(s)");
                            continue;
                        }
                        Err(_) => break,
                    },
                    ev = relayed.recv() => match ev {
                        Ok(text) => {
                            let class = relayed_frame_class(&text);
                            (text, class)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    },
                };
                if limits::enqueue(&out_tx, text, class).await == Enqueued::Hangup {
                    hangup.notify_one();
                    break;
                }
            }
        }
    }));

    // Requests run concurrently. Handling them inline used to serialize the
    // socket: one slow call (a git shell-out, a request routed to a sluggish
    // peer) stalled every request queued behind it, which is how opening a
    // thread could sit on "loading log…" for tens of seconds. The client
    // correlates responses by id and orders anything order-sensitive with its
    // own awaits, so out-of-order completion is safe.
    let inflight = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT_REQUESTS));
    // Per connection rather than per principal, so the check is a float compare
    // against a local and never a lock on the hottest path in the process. What
    // bounds a principal is this rate multiplied by its connection cap.
    let mut rate = limits::TokenBucket::new(limits::WS_REQUESTS_PER_SECOND, limits::WS_REQUEST_BURST);
    loop {
        let msg = tokio::select! {
            biased;
            _ = hangup.notified() => {
                tracing::info!("closing app socket: client is not draining its own frames");
                break;
            }
            msg = stream.next() => msg,
        };
        // A frame over `MAX_WS_MESSAGE_BYTES` arrives here as an error, having
        // been refused from its length header rather than buffered.
        let Some(Ok(msg)) = msg else { break };
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
        if !rate.take() {
            // Answered rather than dropped: the client is waiting on this id, and
            // a refusal it can read and back off from beats a socket that goes
            // quiet. Named limits, because the only person who can act on this is
            // whoever is looking at the screen.
            let frame = ServerMessage::Response {
                id,
                ok: false,
                data: None,
                error: Some(format!(
                    "rate limit: at most {} requests per second on one connection (burst {})",
                    limits::WS_REQUESTS_PER_SECOND as u64, limits::WS_REQUEST_BURST as u64
                )),
            };
            if let Ok(text) = serde_json::to_string(&frame) {
                if limits::enqueue(&out_tx, text, FrameClass::Response).await == Enqueued::Hangup {
                    break;
                }
            }
            continue;
        }
        let Ok(permit) = inflight.clone().acquire_owned().await else {
            break;
        };
        let state = state.clone();
        // A peer socket carries requests from every client on that machine, so
        // the authority is resolved per frame rather than once at upgrade.
        let principal = effective_principal(&principal, &req);
        let out_tx = out_tx.clone();
        let hangup = Arc::clone(&hangup);
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
                if limits::enqueue(&out_tx, text, FrameClass::Response).await == Enqueued::Hangup {
                    hangup.notify_one();
                }
            }
        });
    }
    // The two `TaskGuard`s abort here, which drops the sink and closes the
    // socket; the session guard in `ws_handler` deregisters on the way out.
}

/// The class of a frame relayed from a peer machine.
///
/// These arrive already serialized, so the alternative to a targeted parse is to
/// treat every one of them as undroppable — which would let a peer's streaming
/// firehose disconnect a slow client that was only ever behind on deltas. The
/// shape read here is a subset of `ServerMessage::Event`; anything else, including
/// anything unparseable, is treated as state and therefore never dropped.
fn relayed_frame_class(text: &str) -> crate::limits::FrameClass {
    #[derive(serde::Deserialize)]
    struct Shape {
        #[serde(rename = "type")]
        kind: Option<String>,
        seq: Option<i64>,
    }
    match serde_json::from_str::<Shape>(text) {
        Ok(shape) if shape.kind.as_deref() == Some("event") && shape.seq.unwrap_or(0) < 0 => {
            crate::limits::FrameClass::Delta
        }
        _ => crate::limits::FrameClass::Persisted,
    }
}

/// Resolve the authority one frame carries.
///
/// For everything except a peer this is the connection's own principal, and the
/// frame's `mesh` field is **discarded** — that discard is the security property.
/// A phone can put any assertion it likes in a frame; only a socket that
/// authenticated with a peer credential has its assertion read.
///
/// For a peer, an absent assertion means "that machine's owner". That is a
/// widening relative to the connection default (`authenticate` starts a peer with
/// no grants at all), and deliberately so: the connection default exists to make
/// a handler that forgets to call this function powerless rather than omnipotent,
/// while the frame is the actual source of truth. Machine-to-machine plumbing
/// (`mesh.workspaceUpsert` and friends) sends no assertion because there is no
/// human behind it, and only an authenticated paired machine can produce a frame
/// here at all.
fn effective_principal(connection: &Principal, req: &ClientRequest) -> Principal {
    let Principal::Peer(peer) = connection else {
        return connection.clone();
    };
    let on_behalf_of = req.mesh.as_ref().and_then(|assertion| {
        assertion.on_behalf_of.as_ref().map(|names| {
            names
                .iter()
                .filter_map(|name| Capability::parse(name))
                .collect::<Vec<_>>()
        })
    });
    Principal::Peer(crate::mobile::PeerPrincipal {
        machine_id: peer.machine_id.clone(),
        on_behalf_of,
    })
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

/// A dispatching schedule is a standing instruction to run code — here and,
/// worse, on other machines — on a timer that nobody is watching. The
/// capability table maps every `schedule.*` kind to `Threads`, which is right
/// for a schedule that starts a turn and far too weak for one that dispatches;
/// this is the second grant, named at the only place that can see the payload.
/// Exactly the argument `dispatch.create` makes for requiring both.
fn require_dispatch_authority(
    principal: &Principal,
    dispatch: Option<&crate::protocol::ScheduleDispatch>,
) -> anyhow::Result<()> {
    if dispatch.is_some() {
        principal.require(crate::mobile::Capability::Terminal)?;
    }
    Ok(())
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
    // Asking a machine what it is: os, arch, and which agent CLIs it has. The
    // fleet roster an orchestrating agent reads before choosing where to send
    // work, and nothing in it is secret (pairing hands out more).
    "device.info",
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
    "fs.mkdir",
    "fs.tree",
    "fs.read",
    // Running a command on a named machine is the point of the family: a thread
    // pinned to one box still has to build on another. Every arm answers
    // immediately (the job is the handle), so none of these can hold a peer
    // request open past its timeout.
    "exec.start",
    "exec.status",
    "exec.cancel",
    "exec.list",
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

/// The grant a request kind needs, by consequence rather than by module.
///
/// One table, consulted centrally before anything else runs, is the point:
/// per-arm checks are what let `browser.profile.list` and the peer terminal
/// splice drift out of the policy. Kinds with no entry are unprivileged
/// (`hello`, `project.list`, presence, themes…) and any kind that needs the
/// master credential outright is refused separately, above this.
fn required_capability(kind: &str) -> Option<Capability> {
    if kind.starts_with("git.") {
        return Some(Capability::Git);
    }
    if kind.starts_with("term.") {
        return Some(Capability::Terminal);
    }
    // Same authority as a pty, because it is the same authority: arbitrary code
    // in a project directory. That it captures output instead of echoing it
    // changes the ergonomics, not the blast radius.
    if kind.starts_with("exec.") {
        return Some(Capability::Terminal);
    }
    // Sending a dispatch starts an agent that will run commands — on this
    // machine or on another one — so it needs the grant that running commands
    // needs. Deliberately not a new capability: an existing phone's stored
    // grants would have to be re-issued to mean anything, and "may open a
    // terminal" is exactly the authority being delegated. Reading the ledger is
    // ordinary thread information.
    if kind.starts_with("dispatch.") {
        return Some(match kind {
            "dispatch.list" | "dispatch.get" => Capability::Threads,
            _ => Capability::Terminal,
        });
    }
    if kind.starts_with("fs.") || kind.starts_with("artifacts.") {
        return Some(Capability::Files);
    }
    if kind.starts_with("thread.")
        || kind.starts_with("turn.")
        || kind.starts_with("schedule.")
        || kind.starts_with("archive.")
        || kind == "approval.respond"
        || kind == "question.respond"
    {
        return Some(Capability::Threads);
    }
    None
}

/// Request kinds that carry a full `ThreadSettings` object from the client.
const SETTINGS_INGESTION_KINDS: &[&str] = &[
    "thread.create",
    "thread.setAgent",
    "thread.setSettings",
    "schedule.create",
    "schedule.update",
];

/// Kinds that *run* a thread (or schedule) whose stored settings may already
/// carry signed-browser authority the caller was never granted.
const SETTINGS_EXERCISING_KINDS: &[&str] = &[
    "turn.start",
    "turn.steer",
    "thread.review",
    "thread.parley.start",
];

/// Whether these settings hand an agent the owner's logged-in browser identity.
fn settings_claim_signed_browser(settings: &ThreadSettings) -> bool {
    settings.claude_chrome
        || settings
            .browser_profile_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
}

/// The same test against a raw client payload, before it is deserialized —
/// unknown/extra keys and a missing `settings` object are simply "no claim".
fn payload_claims_signed_browser(payload: &Value) -> bool {
    let Some(settings) = payload.get("settings") else {
        return false;
    };
    let profile = settings
        .get("browserProfileId")
        .and_then(|v| v.as_str())
        .is_some_and(|id| !id.trim().is_empty());
    let chrome = settings
        .get("claudeChrome")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    profile || chrome
}

/// SEC-003: authority carried by a *payload field* gets the same check as a
/// privileged request kind.
///
/// `browser.profile.*` is master-only, and `/browser` refuses a device a
/// signed-in session — but none of that mattered while a device could simply
/// post `browserProfileId` inside a `ThreadSettings` blob and have an agent
/// drive the owner's logged-in Chrome for it. This runs on every path that
/// ingests settings, and it runs BEFORE mesh routing, so asking a peer to do it
/// instead is refused here rather than arriving there as that machine's owner.
fn authorize_settings(state: &ServerState, principal: &Principal, kind: &str, payload: &Value)
    -> anyhow::Result<()>
{
    if principal.can(Capability::SignedBrowser) {
        return Ok(());
    }
    if SETTINGS_INGESTION_KINDS.contains(&kind) && payload_claims_signed_browser(payload) {
        anyhow::bail!(
            "{} — signed-in browser profiles cannot be selected from this device",
            Capability::SignedBrowser.denial()
        );
    }
    // Running an existing thread is the same authority by another route: the
    // profile resolver attaches whatever profile the thread already names.
    // Only checkable for threads this machine owns; a request routed to a peer
    // is refused or allowed by the `mesh` grant alone.
    if SETTINGS_EXERCISING_KINDS.contains(&kind) && !names_remote_machine(state, payload) {
        if let Some(thread_id) = payload.get("threadId").and_then(|v| v.as_str()) {
            if let Some(thread) = state.hub.store.thread(thread_id) {
                if settings_claim_signed_browser(&thread.settings) {
                    anyhow::bail!(
                        "{} — this chat is bound to a signed-in browser profile",
                        Capability::SignedBrowser.denial()
                    );
                }
            }
        }
    }
    if kind == "schedule.run" {
        if let Some(id) = payload.get("scheduleId").and_then(|v| v.as_str()) {
            if let Some(schedule) = state.hub.store.list_schedules().into_iter().find(|s| s.id == id)
            {
                anyhow::ensure!(
                    !settings_claim_signed_browser(&schedule.settings),
                    "{} — this schedule is bound to a signed-in browser profile",
                    Capability::SignedBrowser.denial()
                );
            }
        }
    }
    Ok(())
}

/// Whether this payload targets another machine.
fn names_remote_machine(state: &ServerState, payload: &Value) -> bool {
    payload
        .get("machineId")
        .and_then(|v| v.as_str())
        .is_some_and(|mid| mid != state.device.machine_id)
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
            .request(&peer.machine_id, "mesh.workspaceUpsert", json!({ "workspace": ws }), None)
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
                // Replication is machine-to-machine with no caller behind it, so
                // it asserts nothing and arrives as that machine's owner.
                None,
            )
            .await
        {
            tracing::debug!("workspace delete to {} deferred: {e:#}", peer.machine_id);
        }
    }
}

/// Dispatch one client RPC. This is where request-kind, capability and
/// payload-field authorization all live, so the authorization matrix in
/// `tests/authorization_matrix.rs` drives it directly rather than a stand-in.
pub async fn handle_request(
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
            principal.is_owner(),
            "updating Threadknot requires this machine's master token, not a device credential"
        );
    }

    // Creating a signed-in browser profile, widening the sites it may visit, or
    // erasing one is an authority change over stored logins. A revocable phone
    // credential can drive a session the owner already set up; it cannot decide
    // what that session is allowed to reach.
    if req.kind.starts_with("browser.profile.") && req.kind != "browser.profile.list" {
        anyhow::ensure!(
            principal.is_owner(),
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
            principal.is_owner(),
            "installing skills and MCP servers requires this machine's master token"
        );
    }

    // Central capability gate. Also BEFORE routing: a device that may not open
    // a terminal here must not be able to open one on a paired machine either,
    // and peer requests carry that machine's master token, so this side is the
    // only side that still knows who is really asking.
    if let Some(capability) = required_capability(&req.kind) {
        principal.require(capability)?;
    }
    authorize_settings(state, principal, &req.kind, &p)?;

    if is_routable(&req.kind) {
        if let Some(mid) = p.get("machineId").and_then(|v| v.as_str()) {
            if mid != state.device.machine_id {
                principal.require(Capability::Mesh)?;
                let mid = mid.to_string();
                let mut payload = p.clone();
                if let Some(obj) = payload.as_object_mut() {
                    obj.remove("machineId");
                }
                // The caller's own grants travel with the request. This is
                // the structural half of the fix: the far side no longer has to
                // trust that this side checked, because it can check itself.
                return state
                    .peernet
                    .request(&mid, &req.kind, payload, principal.mesh_assertion())
                    .await;
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
                // SEC-001: `lan_url` embeds `config.token`, which authenticates
                // as Master. Handing it to every authenticated principal meant a
                // paired phone could read its own `hello`, lift the master token
                // out of it, and reconnect with full administrative authority.
                // A device gets the address only; the credential stays here.
                // `is_local_master`, deliberately not `is_owner`: a peer acting
                // for its own owner administers ITS machine, and handing it our
                // token-bearing URL would rebuild SEC-001 across the mesh.
                "lanUrl": if principal.is_local_master() { state.lan_url.clone() } else { state.lan_origin() },
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
                "dictation": state.dictation.capability(principal.is_owner()),
                // What this connection may actually do, so the UI can hide what
                // the owner did not grant instead of offering it and failing.
                // Advisory only — every one of these is enforced server-side.
                "principal": principal.label(),
                "capabilities": principal
                    .capabilities()
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>(),
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
                principal.is_owner(),
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
                principal.is_owner(),
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
                    // A pair from before the encrypted mesh is not "offline" —
                    // it is refused, and only re-pairing fixes it. Reporting it
                    // as offline would send someone hunting a network fault that
                    // does not exist.
                    v["needsUpgrade"] = json!(p.needs_upgrade());
                    v
                })
                .collect();
            Ok(json!({ "peers": peers, "discovered": state.peernet.discovered() }))
        }
        "peer.add" => {
            anyhow::ensure!(
                principal.is_owner(),
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

            // Two phases, and the split is the security property.
            //
            // Phase 1 fetches public identity over plain HTTP: machine id, name,
            // certificate authority, mesh port, and a single-use challenge.
            // Nothing secret crosses, so there is nothing here to steal.
            let identity: Value = crate::hermes::http_client()
                .get(format!(
                    "http://{}/api/peer/identity",
                    crate::peernet::host_port(&host, port)
                ))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .context("could not reach the peer — check the URL and that Threadknot is running there")?
                .error_for_status()
                .context("that machine did not answer a Threadknot identity probe")?
                .json()
                .await
                .context("bad identity response — is that really a Threadknot?")?;

            let their_id = identity
                .get("machineId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            anyhow::ensure!(!their_id.is_empty(), "peer sent no machine id");
            anyhow::ensure!(
                their_id != state.device.machine_id,
                "that URL points at this machine"
            );
            let their_mesh_version =
                identity.get("meshVersion").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            anyhow::ensure!(
                their_mesh_version == crate::device::MESH_VERSION,
                "that machine speaks mesh version {their_mesh_version} and this one speaks {} — \
                 update Threadknot on the older machine, then pair them again",
                crate::device::MESH_VERSION
            );
            let their_ca = identity
                .get("meshCa")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let their_mesh_port =
                identity.get("meshPort").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let challenge = identity
                .get("challenge")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            anyhow::ensure!(
                !their_ca.is_empty() && their_mesh_port != 0 && !challenge.is_empty(),
                "that machine did not offer an encrypted mesh — update Threadknot there"
            );
            // Refuse an unusable certificate now rather than after storing a
            // pair that could never connect.
            crate::mesh::client_config_for_ca(&their_ca)
                .context("that machine sent an unusable certificate")?;

            // Phase 2 runs over TLS pinned to the certificate authority phase 1
            // returned, and proves knowledge of their master token *without
            // sending it*. The proof covers the fingerprint of the CA we just
            // received, so if phase 1 was intercepted and a substitute
            // certificate injected, the proof is computed over the attacker's
            // fingerprint and the real machine refuses it. That is what closes
            // the trust-on-first-use hole rather than accepting it.
            let proof = crate::mesh::pairing_proof(
                &token,
                &challenge,
                &state.device.machine_id,
                &their_ca,
            );
            // The credential THEY will present to US: minted here, and only its
            // hash is kept.
            let our_credential_for_them = crate::mesh::mint_credential();
            let probe = crate::peers::Peer {
                machine_id: their_id.clone(),
                mesh_ca: their_ca.clone(),
                mesh_port: their_mesh_port,
                ..crate::peers::Peer::blank()
            };
            let client = state.peernet.byte_client(&probe, &host)?;
            let resp = client
                .post(format!(
                    "https://{}:{their_mesh_port}/api/peer/pair",
                    crate::mesh::mesh_dns_name(&their_id)
                ))
                .json(&json!({
                    "machineId": state.device.machine_id,
                    "name": state.device.friendly_name(),
                    "avatar": state.device.avatar(),
                    "color": state.device.color(),
                    "profileUpdatedAt": state.device.profile_updated_at(),
                    "port": state.config.port,
                    "meshPort": state.mesh_port(),
                    "meshCa": state.mesh.ca_pem,
                    "meshVersion": crate::device::MESH_VERSION,
                    "addresses": crate::peernet::local_addresses(),
                    "challenge": challenge,
                    "proof": proof,
                    "credentialForYou": our_credential_for_them,
                }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .context("could not complete the encrypted pairing handshake")?;
            anyhow::ensure!(
                resp.status().is_success(),
                "peer refused pairing: {}",
                resp.text().await.unwrap_or_default()
            );
            let them: Value = resp.json().await.context("bad pairing response")?;
            let their_credential_for_us = them
                .get("credentialForYou")
                .and_then(|v| v.as_str())
                .filter(|c| !c.is_empty())
                .context("peer sent no credential")?
                .to_string();

            let mut addresses = vec![host.clone()];
            if let Some(list) = identity.get("addresses").and_then(|v| v.as_array()) {
                for a in list.iter().filter_map(|x| x.as_str()) {
                    if !addresses.iter().any(|x| x == a) {
                        addresses.push(a.to_string());
                    }
                }
            }
            let peer = state.peernet.registry.upsert(crate::peers::Peer {
                machine_id: their_id.clone(),
                name: them
                    .get("name")
                    .or_else(|| identity.get("name"))
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
                outbound_credential: their_credential_for_us,
                inbound_credential_hash: crate::mesh::hash_credential(&our_credential_for_them),
                mesh_ca: their_ca,
                mesh_port: their_mesh_port,
                port: them.get("port").and_then(|v| v.as_u64()).unwrap_or(port as u64) as u16,
                addresses,
                last_good_address: Some(host),
                last_seen_at: Some(now_iso()),
                added_at: now_iso(),
                mesh_version: their_mesh_version,
            })?;
            state.peernet.kick.notify_waiters();
            hub.broadcast_state("peers", None);
            Ok(serde_json::to_value(peer)?)
        }
        "peer.remove" => {
            anyhow::ensure!(
                principal.is_owner(),
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
                principal.is_owner(),
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
        "workspace.setHidden" => {
            let ws = store.set_workspace_hidden(
                field(&p, "workspaceId")?,
                bool_field(&p, "hidden")?,
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
                principal.is_owner(),
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
                    .request(&machine_id, "mesh.createProject", json!({ "path": path }), None)
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
                principal.is_owner(),
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
                .request(&machine_id, "mesh.rewrapProject", json!({ "projectId": project_id }), None)
                .await
            {
                tracing::warn!("rewrap on {machine_id} deferred: {e:#}");
            }
            hub.broadcast_state("workspaces", None);
            replicate_workspace(state, &ws).await;
            Ok(serde_json::to_value(ws)?)
        }
        // ---- mesh.* : peer-to-peer replication plumbing ----
        //
        // `is_peer`, not `is_owner`: these are machine-to-machine calls with no
        // human on the other end, so the credential that carries them is a peer
        // credential and nothing else. Checking the transport rather than the
        // authority level is what finally makes the "peer-only" in the messages
        // below true — under the old model a local master token satisfied it.
        "mesh.createProject" => {
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
            let project = store.create_project_raw(field(&p, "path")?.to_string(), None)?;
            hub.broadcast_state("projects", None);
            Ok(serde_json::to_value(project)?)
        }
        "mesh.workspaceUpsert" => {
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
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
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
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
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
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
                principal.is_owner(),
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
                principal.is_owner(),
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
                principal.is_owner(),
                "Hermes agent management requires the desktop app"
            );
            let image = optional_sidebar_image(&p, "image")?;
            let agent = hub.hermes.set_image(field(&p, "agentId")?, image)?;
            refresh_hermes_agents(state).await;
            Ok(serde_json::to_value(agent)?)
        }
        "hermes.agent.setAvatar" => {
            anyhow::ensure!(
                principal.is_owner(),
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
                principal.is_owner(),
                "Claudex profile management requires the desktop app"
            );
            let input: crate::claudex::ProfileInput = serde_json::from_value(p.clone())?;
            let profile = hub.claudex.add(input)?;
            refresh_claudex_agents(state).await;
            Ok(profile.public())
        }
        "claudex.profile.update" => {
            anyhow::ensure!(
                principal.is_owner(),
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
                principal.is_owner(),
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
                principal.is_owner(),
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
                principal.is_owner(),
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
        // Minting a pairing code creates a fresh path to a device credential,
        // so it is an authority change: master-only, exactly like revoking one.
        // A paired phone must not be able to bring in more phones.
        "mobile.pair.begin" => {
            anyhow::ensure!(
                principal.is_owner(),
                "showing a pairing QR requires this machine's master token"
            );
            // Grants are bound to the code here, on the desktop, by the owner.
            // The joining client redeems the code and takes what it carries; it
            // has no way to ask for more.
            let capabilities = requested_capabilities(&p);
            // Which address the QR should send the phone to. A phone on
            // cellular cannot reach `192.168.x.x`, and a phone on the sofa
            // should not be routed through a relay — so the owner picks, and
            // the remote answer comes from stored configuration. Deriving it
            // from the request's `Host` would let anyone who can reach this
            // server have a QR minted that points a phone at a host they chose.
            let target = p.get("target").and_then(|v| v.as_str()).unwrap_or("lan");
            let origin = match target {
                "remote" => state.remote.origin().ok_or_else(|| {
                    anyhow::anyhow!(
                        "set this machine's public address and turn remote access on first"
                    )
                })?,
                _ => state.lan_origin(),
            };
            // A long-lived code exists for one situation: handing an external
            // tester (App Store review) a link that still works whenever they
            // get to it, days later. It stays one-time and capability-scoped —
            // only the window widens — and it is refused on `lan`, where the
            // owner is standing at the screen and a 3-minute code costs nothing.
            // Capped, so a typo cannot mint something effectively permanent.
            let ttl = match p.get("ttlSeconds").and_then(|v| v.as_u64()) {
                Some(requested) if target == "remote" => {
                    anyhow::ensure!(
                        requested > 0,
                        "ttlSeconds must be positive"
                    );
                    std::time::Duration::from_secs(
                        requested.min(crate::mobile::PAIRING_TTL_MAX.as_secs()),
                    )
                }
                Some(_) => anyhow::bail!(
                    "a long-lived pairing code is only allowed for the remote origin"
                ),
                None => crate::mobile::PAIRING_TTL,
            };
            let code = state
                .mobile
                .begin_pairing_with_ttl(capabilities.clone(), ttl);
            let payload = pairing_payload(&origin, &code);
            Ok(json!({
                "payload": payload,
                "qrSvg": pairing_qr_svg(&payload)?,
                "code": crate::mobile::format_pairing_code(&code),
                "url": origin,
                "target": target,
                "ttlSeconds": ttl.as_secs(),
                "capabilities": capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            }))
        }
        // The dialog closed (or the countdown ran out client-side): stop
        // honoring what was on screen now rather than at the TTL.
        "mobile.pair.cancel" => {
            anyhow::ensure!(
                principal.is_owner(),
                "pairing management requires this machine's master token"
            );
            state.mobile.cancel_pairings();
            Ok(json!({ "ok": true }))
        }
        // ---- connector.* : the hosted relay (Stage 2) ----
        //
        // Machine administration throughout, so owner-only and deliberately
        // absent from ROUTABLE: each machine enrolls itself with its own key.
        // Enrolling a peer from here would mean holding a key on behalf of a
        // machine that should be the only thing that ever holds it.
        "connector.status" => {
            anyhow::ensure!(
                principal.is_owner(),
                "remote access settings require the desktop app"
            );
            Ok(serde_json::to_value(state.connector.status())?)
        }
        "connector.enroll" => {
            anyhow::ensure!(
                principal.is_owner(),
                "enrolling this machine requires the desktop app"
            );
            let token = field(&p, "enrollmentToken")?;
            let name = p
                .get("machineName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from)
                .unwrap_or_else(|| state.device.friendly_name());
            let status = state.connector.enroll(token, &name).await?;
            // The relay assigns the hostname, so enrollment is also what
            // provisions the pairing origin. Deriving it anywhere else would mean
            // guessing, and a guessed origin in a QR sends a phone nowhere.
            if !status.public_origin.is_empty() {
                state
                    .remote
                    .set(Some(true), Some(Some(status.public_origin.clone())))?;
            }
            hub.broadcast_state("identity", None);
            Ok(serde_json::to_value(status)?)
        }
        // The interactive way to connect a machine: no token, nothing to copy.
        // The desktop asks for a request, the owner opens the URL and presses
        // Approve, and the connector notices on its own — see `begin_approval`.
        "connector.beginApproval" => {
            anyhow::ensure!(
                principal.is_owner(),
                "connecting this machine requires the desktop app"
            );
            let name = p
                .get("machineName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from)
                .unwrap_or_else(|| state.device.friendly_name());
            let approval = state.connector.begin_approval(&name).await?;
            hub.broadcast_state("connector", None);
            Ok(serde_json::to_value(approval)?)
        }
        "connector.cancelApproval" => {
            anyhow::ensure!(
                principal.is_owner(),
                "remote access settings require the desktop app"
            );
            state.connector.cancel_approval();
            Ok(serde_json::to_value(state.connector.status())?)
        }
        "connector.setEnabled" => {
            anyhow::ensure!(
                principal.is_owner(),
                "remote access settings require the desktop app"
            );
            let enabled = p
                .get("enabled")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| anyhow::anyhow!("missing enabled"))?;
            let status = state.connector.set_enabled(enabled)?;
            // Turning the connector off also closes the strict ingress, so a
            // browser holding a cookie for the public origin loses it now rather
            // than at its next request.
            if !enabled {
                state.remote.set(Some(false), None)?;
                let dropped = state.browser_sessions.revoke_all();
                let closed = state.sessions.close_remote();
                tracing::info!(
                    "connector disabled: {dropped} session(s), {closed} socket(s) dropped"
                );
            }
            hub.broadcast_state("identity", None);
            Ok(serde_json::to_value(status)?)
        }
        // Remote access: machine administration, so master-only and never
        // routed to a peer — each machine is provisioned for itself.
        "remote.get" => {
            anyhow::ensure!(
                principal.is_owner(),
                "remote access settings require the desktop app"
            );
            let config = state.remote.get();
            Ok(json!({
                "enabled": config.enabled,
                "origin": config.origin,
                "loopbackPort": config.port,
                "browserSessions": state.browser_sessions.len(),
            }))
        }
        "remote.set" => {
            anyhow::ensure!(
                principal.is_owner(),
                "remote access settings require the desktop app"
            );
            let enabled = p.get("enabled").and_then(|v| v.as_bool());
            let origin = appearance_patch(&p, "origin")?;
            let was_enabled = state.remote.enabled();
            let config = state.remote.set(enabled, origin)?;
            // Turning it off has to mean off *now*: outstanding browser
            // sessions die and anything already connected through the strict
            // ingress is dropped. Changing the origin invalidates them too —
            // the cookie was issued for a hostname that no longer applies.
            if was_enabled && (!config.enabled || enabled.is_none()) {
                let dropped = state.browser_sessions.revoke_all();
                let closed = state.sessions.close_remote();
                tracing::info!("remote access changed: {dropped} session(s), {closed} socket(s) dropped");
            }
            hub.broadcast_state("identity", None);
            Ok(json!({
                "enabled": config.enabled,
                "origin": config.origin,
                "loopbackPort": config.port,
            }))
        }
        "mobile.device.list" => {
            anyhow::ensure!(
                principal.is_owner(),
                "device management requires the desktop app"
            );
            Ok(json!({ "devices": state.mobile.list() }))
        }
        "mobile.device.revoke" => {
            anyhow::ensure!(
                principal.is_owner(),
                "device management requires the desktop app"
            );
            let device_id = field(&p, "deviceId")?.to_string();
            state.mobile.revoke(&device_id)?;
            // SEC-008: the record is gone, but the sockets it already opened
            // would otherwise stay live and fully usable. Revoke means now.
            state.close_device_sessions(&device_id);
            hub.broadcast_state("mobileDevices", None);
            Ok(json!({}))
        }
        // Editing what a paired device may do. Master-only, like every other
        // authority change, and never routed to a peer: each machine grants for
        // itself.
        "mobile.device.setCapabilities" => {
            anyhow::ensure!(
                principal.is_owner(),
                "changing a device's permissions requires the desktop app"
            );
            let device_id = field(&p, "deviceId")?.to_string();
            anyhow::ensure!(
                p.get("capabilities").is_some_and(|v| v.is_array()),
                "capabilities must be an array"
            );
            let device = state
                .mobile
                .set_capabilities(&device_id, requested_capabilities(&p))?;
            // A reduction is a partial revocation: close what the device holds
            // so it has to come back through the gate with its new grants.
            state.close_device_sessions(&device_id);
            hub.broadcast_state("mobileDevices", None);
            Ok(json!({ "device": device }))
        }
        "project.create" => {
            let path = field(&p, "path")?.to_string();
            let name = p.get("name").and_then(|v| v.as_str()).map(String::from);
            let machine_id = p
                .get("machineId")
                .and_then(|v| v.as_str())
                .map(String::from);
            // A machineId naming a PEER makes this remote-first creation: the
            // peer creates the project (mesh.createProject), we wrap it in its
            // 1:1 workspace here and replicate the record out — the same
            // bookkeeping workspace.attachRoot does, minus the attach. The
            // machineId is a real local parameter, so this kind is NOT in
            // ROUTABLE.
            match machine_id {
                Some(m) if m != state.device.machine_id => {
                    anyhow::ensure!(
                        principal.is_owner(),
                        "workspaces on another machine are created from the desktop app"
                    );
                    let v = state
                        .peernet
                        .request(&m, "mesh.createProject", json!({ "path": path }), None)
                        .await?;
                    let project: Project = serde_json::from_value(v)?;
                    let ws = store.create_workspace_for_remote_root(&project, &m)?;
                    hub.broadcast_state("workspaces", None);
                    hub.broadcast_state("projects", None);
                    replicate_workspace(state, &ws).await;
                    Ok(serde_json::to_value(project)?)
                }
                _ => {
                    let project = store.create_project(path, name)?;
                    hub.broadcast_state("projects", None);
                    Ok(serde_json::to_value(project)?)
                }
            }
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
            if crate::store::is_quick_home_project_id(&project_id) {
                anyhow::ensure!(
                    project_id == crate::store::quick_home_project_id(&store.local_machine_id()),
                    "quick-chat home belongs to another machine"
                );
                if store.ensure_quick_home()? {
                    hub.broadcast_state("projects", None);
                }
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
                #[serde(default)]
                dispatch: Option<crate::protocol::ScheduleDispatch>,
            }
            let c: Create = serde_json::from_value(p)?;
            anyhow::ensure!(!c.prompt.trim().is_empty(), "prompt is empty");
            require_dispatch_authority(principal, c.dispatch.as_ref())?;
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
                dispatch: c.dispatch,
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
            // Tri-state, like `appearance_patch`: absent leaves the schedule's
            // mode alone (an older client cannot silently un-dispatch one),
            // `null` switches it back to running here, an object sets it.
            let dispatch: Option<Option<crate::protocol::ScheduleDispatch>> = match p.get("dispatch")
            {
                None => None,
                Some(Value::Null) => Some(None),
                Some(v) => Some(Some(serde_json::from_value(v.clone())?)),
            };
            let u: Update = serde_json::from_value(p)?;
            if let Some(pid) = &u.project_id {
                anyhow::ensure!(store.project(pid).is_some(), "unknown project");
            }
            require_dispatch_authority(principal, dispatch.as_ref().and_then(|d| d.as_ref()))?;
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
                if let Some(v) = dispatch {
                    s.dispatch = v;
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
            let schedule_id = field(&p, "scheduleId")?;
            // Running a dispatching schedule runs code on other machines, so
            // the same grant that creating one needs is needed to fire one.
            let saved = store.schedule(schedule_id);
            require_dispatch_authority(principal, saved.as_ref().and_then(|s| s.dispatch.as_ref()))?;
            let thread_id = crate::schedules::run_now(state, schedule_id).await?;
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
        "dictation.settings.get" => {
            anyhow::ensure!(
                principal.is_owner(),
                "dictation settings are only available in the app on that machine"
            );
            Ok(state.dictation.settings())
        }
        "dictation.settings.save" => {
            anyhow::ensure!(
                principal.is_owner(),
                "dictation settings are only available in the app on that machine"
            );
            let settings = state.dictation.configure(
                field(&p, "provider")?,
                field(&p, "baseUrl")?,
                field(&p, "model")?,
                p.get("apiKey").and_then(Value::as_str),
            )?;
            hub.broadcast_state("identity", None);
            Ok(settings)
        }
        "dictation.start" => {
            anyhow::ensure!(
                principal.is_owner(),
                "dictation records this machine's mic, so it only runs from the app on that machine"
            );
            Ok(json!({ "recordingId": state.dictation.start().await? }))
        }
        "dictation.stop" => {
            anyhow::ensure!(
                principal.is_owner(),
                "dictation records this machine's mic, so it only runs from the app on that machine"
            );
            let text = state.dictation.stop(field(&p, "recordingId")?).await?;
            Ok(json!({ "text": text }))
        }
        "dictation.cancel" => {
            anyhow::ensure!(
                principal.is_owner(),
                "dictation records this machine's mic, so it only runs from the app on that machine"
            );
            state.dictation.cancel(field(&p, "recordingId")?).await;
            Ok(json!({}))
        }
        // Must precede the generic git arm: these act on Threadknot's own checkout,
        // not on a project repo, so they take no repoId.
        k if k.starts_with("git.selfUpdate") => crate::update::handle(hub, k, &p).await,
        k if k.starts_with("git.") => crate::git::handle(state, k, &p).await,
        "fs.tree" => crate::files::tree(state, &p),
        "fs.read" => crate::files::read(state, &p),
        k if k.starts_with("exec.") => crate::exec::handle(state, k, &p).await,
        k if k.starts_with("dispatch.") || k.starts_with("mesh.dispatch") => {
            crate::dispatch::handle(state, principal, k, &p).await
        }
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
        // Enumerating the owner's signed-in logins is part of that authority —
        // the id list is exactly what a caller needs to smuggle a profile into
        // ThreadSettings. Answered as "you have none" rather than as a refusal:
        // a device without the grant cannot select one anyway, and the picker
        // that asks for this list on open should render empty, not error.
        "browser.profile.list" => Ok(json!({
            "profiles": if principal.can(Capability::SignedBrowser) {
                state.browser_profiles.list()
            } else {
                Vec::new()
            }
        })),
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
        "fs.mkdir" => {
            let path = PathBuf::from(field(&p, "path")?);
            std::fs::create_dir_all(&path)
                .with_context(|| format!("cannot create folder: {}", path.display()))?;
            let canonical = std::fs::canonicalize(&path)?;
            anyhow::ensure!(canonical.is_dir(), "not a directory: {}", path.display());
            Ok(json!({ "path": canonical.to_string_lossy() }))
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

//! Which door a request came in by, and what that door allows.
//!
//! Threadknot binds three listeners. The compatibility listener on `0.0.0.0` is
//! the LAN/Tauri product as it has always worked: a token in the URL, the master
//! credential accepted, everything reachable. The strict listener is bound to
//! loopback and is dialled only by the connector process on this same machine,
//! which is what a relay's traffic arrives through. The mesh listener is TLS on
//! `0.0.0.0:<port+2>` and accepts exactly one thing: a paired machine's per-link
//! credential, in a header.
//!
//! The policy is a property of the socket, deliberately. It cannot be a header
//! ("I came from the relay" is spoofable from the LAN) and it cannot be the
//! source address (the desktop's own webview is also `127.0.0.1`). Binding a
//! second socket is the only version of this that a caller cannot choose for
//! itself.
//!
//! On the strict listener:
//!
//! * credentials in the query string are refused outright — even valid ones,
//!   because a URL is copied, logged, put in a `Referer` and handed to browser
//!   extensions;
//! * the master credential is refused, however it is presented;
//! * what remains is a native device bearer token, or an opaque `HttpOnly`
//!   session cookie minted by `POST /api/session`.

use crate::mobile::Principal;
use axum::http::{header, HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Which listener a request arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressPolicy {
    /// `0.0.0.0:<port>` — LAN browsers, the Tauri webview, paired phones on the
    /// same network, and peer machines. Legacy query-token authentication lives
    /// here and only here.
    Compat,
    /// `127.0.0.1:<port+1>` — the connector, and therefore the public relay.
    Remote,
    /// `0.0.0.0:<port+2>`, **TLS** — paired Threadknot machines, and nothing
    /// else. Accepts exactly one thing: a per-peer credential in an
    /// `Authorization` header. No query credential, no master credential, no
    /// cookie, no anonymous request. It is a separate socket for the same reason
    /// the strict ingress is: a peer credential must be judged by the door it
    /// arrived at, and a browser on the LAN must not be able to pick that door.
    Mesh,
}

impl IngressPolicy {
    pub fn is_remote(self) -> bool {
        self == IngressPolicy::Remote
    }

    pub fn is_mesh(self) -> bool {
        self == IngressPolicy::Mesh
    }

    /// Whether this door forbids credentials in the URL. Both hardened
    /// listeners do; only the LAN compatibility listener still allows them, and
    /// only during the migration window.
    pub fn forbids_url_credentials(self) -> bool {
        self != IngressPolicy::Compat
    }
}

/// Why a request was turned away at the door, before any capability check.
#[derive(Debug)]
pub struct Rejection {
    pub status: StatusCode,
    pub message: &'static str,
}

impl Rejection {
    fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }

    pub fn into_response(self) -> axum::response::Response {
        use axum::response::IntoResponse as _;
        (self.status, self.message).into_response()
    }
}

/// Query keys that carry a credential. Present on the strict ingress, they are
/// an error even when the value is valid — the point is to stop credentials
/// existing in URLs at all, not to check them more carefully.
const CREDENTIAL_QUERY_KEYS: [&str; 3] = ["token", "credential", "access_token"];

/// The cookie holding an opaque browser session.
pub const SESSION_COOKIE: &str = "tk_session";

/// Double-submit header paired with [`SESSION_COOKIE`].
pub const CSRF_HEADER: &str = "x-threadknot-csrf";

/// Header a peer uses to say whose authority a request carries.
///
/// A comma-separated list of capability names narrows the request to exactly
/// those grants. **Absent** means the request came from that machine's own
/// owner, which is why the header is only ever read on the mesh listener: on any
/// other door "absent" describes every request ever made, and reading it there
/// would turn an ordinary LAN request into an owner-authority one.
pub const MESH_GRANTS_HEADER: &str = "x-threadknot-mesh-grants";

/// Parse [`MESH_GRANTS_HEADER`] into the `on_behalf_of` form.
///
/// `None` (header absent) is the peer's own owner. `Some(list)` is a narrowing,
/// and an unparseable capability name is **dropped** rather than erroring — a
/// newer peer must never be able to widen an older machine by naming a
/// capability it does not understand, and erroring instead would let a newer
/// peer break an older one by mentioning one.
pub fn mesh_grants(headers: &HeaderMap) -> Option<Vec<crate::mobile::Capability>> {
    let raw = headers.get(MESH_GRANTS_HEADER)?.to_str().ok()?;
    Some(
        raw.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter_map(crate::mobile::Capability::parse)
            .collect(),
    )
}

/// Render an `on_behalf_of` for the header. `None` sends no header at all.
pub fn mesh_grants_header_value(
    on_behalf_of: Option<&[crate::mobile::Capability]>,
) -> Option<String> {
    on_behalf_of.map(|capabilities| {
        capabilities
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// How long a browser session survives without being used.
const SESSION_IDLE_TTL_DAYS: i64 = 30;

/// One issued browser session. The plaintext token lives only in the client's
/// cookie jar; we keep its hash, exactly like a device credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSession {
    id: String,
    /// The paired device this session speaks for. Authority is always resolved
    /// through this id at request time, so narrowing a device's grants applies
    /// to its browser sessions with no extra bookkeeping.
    device_id: String,
    token_hash: String,
    created_at: String,
    last_used_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionFile {
    #[serde(default)]
    sessions: Vec<StoredSession>,
}

/// Opaque browser sessions, persisted so a desktop restart does not sign every
/// remote browser out.
pub struct BrowserSessions {
    path: PathBuf,
    sessions: Mutex<Vec<StoredSession>>,
}

impl BrowserSessions {
    pub fn open(dir: &std::path::Path) -> anyhow::Result<Self> {
        let path = dir.join("browser-sessions.json");
        let sessions = if path.exists() {
            serde_json::from_str::<SessionFile>(&std::fs::read_to_string(&path)?)
                .map(|f| f.sessions)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let store = Self {
            path,
            sessions: Mutex::new(sessions),
        };
        store.expire_idle();
        Ok(store)
    }

    fn flush(&self, sessions: &[StoredSession]) {
        let file = SessionFile {
            sessions: sessions.to_vec(),
        };
        let Ok(json) = serde_json::to_string_pretty(&file) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if crate::store::write_private(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn expire_idle(&self) {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(SESSION_IDLE_TTL_DAYS);
        let mut sessions = self.sessions.lock().unwrap();
        let before = sessions.len();
        sessions.retain(|s| {
            chrono::DateTime::parse_from_rfc3339(&s.last_used_at)
                .map(|t| t.with_timezone(&chrono::Utc) > cutoff)
                .unwrap_or(false)
        });
        if sessions.len() != before {
            self.flush(&sessions);
        }
    }

    /// Mint a session for `device_id`. Returns the plaintext cookie value,
    /// which is never stored and never shown again.
    pub fn issue(&self, device_id: &str) -> String {
        let token = format!("tks_{}{}", crate::store::generate_token(), crate::store::generate_token());
        let now = crate::protocol::now_iso();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.push(StoredSession {
            id: crate::protocol::new_id(),
            device_id: device_id.to_string(),
            token_hash: crate::mobile::hash_credential(&token),
            created_at: now.clone(),
            last_used_at: now,
        });
        self.flush(&sessions);
        token
    }

    /// The device a presented cookie speaks for, refreshing its idle clock.
    pub fn resolve(&self, token: &str) -> Option<String> {
        if !token.starts_with("tks_") {
            return None;
        }
        let hash = crate::mobile::hash_credential(token);
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.iter_mut().find(|s| s.token_hash == hash)?;
        session.last_used_at = crate::protocol::now_iso();
        let device_id = session.device_id.clone();
        let snapshot = sessions.clone();
        drop(sessions);
        self.flush(&snapshot);
        Some(device_id)
    }

    /// Drop every session belonging to `device_id` — the cookie half of a
    /// revoke. Returns how many were dropped.
    pub fn revoke_device(&self, device_id: &str) -> usize {
        if device_id.is_empty() {
            return 0;
        }
        let mut sessions = self.sessions.lock().unwrap();
        let before = sessions.len();
        sessions.retain(|s| s.device_id != device_id);
        let dropped = before - sessions.len();
        if dropped > 0 {
            self.flush(&sessions);
        }
        dropped
    }

    /// Invalidate every browser session — what "turn remote access off" means
    /// for anyone currently holding one.
    pub fn revoke_all(&self) -> usize {
        let mut sessions = self.sessions.lock().unwrap();
        let dropped = sessions.len();
        sessions.clear();
        self.flush(&sessions);
        dropped
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The CSRF token paired with a session cookie.
///
/// Derived from the cookie rather than stored beside it: the cookie is
/// `HttpOnly`, so a page on another origin cannot read it and therefore cannot
/// compute this, which is the whole requirement. Nothing extra to persist, and
/// no way for the two to drift apart.
pub fn csrf_token(session_cookie: &str) -> String {
    crate::mobile::hash_credential(&format!("threadknot-csrf:{session_cookie}"))
}

/// `Set-Cookie` for a freshly minted session.
///
/// Host-scoped (no `Domain`), so it can never be sent to a sibling
/// installation's hostname on the same relay domain. `SameSite=Strict` because
/// nothing here is ever a legitimate cross-site navigation target.
pub fn session_cookie_header(token: &str) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={}",
        SESSION_IDLE_TTL_DAYS * 24 * 60 * 60
    )
}

/// `Set-Cookie` that clears the session.
pub fn clear_cookie_header() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0")
}

/// One cookie's value out of a `Cookie` header.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
}

/// The `Authorization: Bearer …` value, if any.
pub fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())?
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
}

/// Whether the query string carries anything that looks like a credential.
pub fn query_carries_credential(params: &HashMap<String, String>) -> bool {
    CREDENTIAL_QUERY_KEYS
        .iter()
        .any(|key| params.contains_key(*key))
}

/// Whether one query key is credential-bearing.
///
/// Exposed so that code forwarding a request onward strips the same set this
/// module refuses. Two lists that were supposed to match is how a credential
/// ends up forwarded to a door that rejects it.
pub fn is_credential_query_key(key: &str) -> bool {
    CREDENTIAL_QUERY_KEYS.contains(&key)
}

/// How a request authenticated, so callers can apply CSRF only where it means
/// something (a cookie is attached by the browser; a bearer token is not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    QueryToken,
    Bearer,
    Cookie,
}

pub struct Authenticated {
    pub principal: Principal,
    pub kind: AuthKind,
}

/// Resolve a request to a principal under `policy`.
///
/// `lookup` maps a presented credential to a principal, and `session` maps a
/// cookie to the device it speaks for — both supplied by the caller so this
/// stays free of `ServerState`.
pub fn authenticate(
    policy: IngressPolicy,
    headers: &HeaderMap,
    params: &HashMap<String, String>,
    lookup: impl Fn(&str) -> Option<Principal>,
    session: impl Fn(&str) -> Option<Principal>,
) -> Result<Authenticated, Rejection> {
    let cookie = cookie_value(headers, SESSION_COOKIE);
    let bearer = bearer(headers);

    if policy.is_mesh() {
        // Refused before anything is even looked up: a credential in a URL is
        // the bug SEC-012 exists to remove, so the mesh door must not accept one
        // even transitionally.
        if query_carries_credential(params) {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "credentials must not be sent in the URL on this connection",
            ));
        }
        let token = bearer.ok_or_else(|| {
            Rejection::new(StatusCode::UNAUTHORIZED, "this connection requires a peer credential")
        })?;
        let principal = lookup(&token)
            .ok_or_else(|| Rejection::new(StatusCode::UNAUTHORIZED, "unknown credential"))?;
        // Only a peer credential opens this door. A master token or a phone's
        // device credential presented here is refused even though it is valid,
        // for the same reason the strict ingress refuses the master token: this
        // socket's whole meaning is "the caller is a paired machine", and one
        // door that accepts two kinds of principal is a door whose policy has to
        // be re-derived at every handler.
        let Principal::Peer(mut peer) = principal else {
            return Err(Rejection::new(
                StatusCode::FORBIDDEN,
                "only a peer credential is accepted on the mesh listener",
            ));
        };
        // The credential says *which machine*; the header says *whose authority*.
        // Keeping those separate is the whole of mesh principal propagation: the
        // credential cannot be forged, and the header can only narrow.
        peer.on_behalf_of = mesh_grants(headers);
        return Ok(Authenticated {
            principal: Principal::Peer(peer),
            kind: AuthKind::Bearer,
        });
    }

    if policy.is_remote() {
        if query_carries_credential(params) {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "credentials must not be sent in the URL on this connection",
            ));
        }
        if let Some(cookie) = cookie {
            let principal = session(&cookie).ok_or_else(|| {
                Rejection::new(StatusCode::UNAUTHORIZED, "session expired — sign in again")
            })?;
            return Ok(Authenticated {
                principal,
                kind: AuthKind::Cookie,
            });
        }
        if let Some(token) = bearer {
            let principal = lookup(&token)
                .ok_or_else(|| Rejection::new(StatusCode::UNAUTHORIZED, "unknown credential"))?;
            // Invariant 2: valid is not sufficient. The master credential is
            // this machine's own administrative key and never crosses a relay,
            // so presenting it here is a configuration error, not a login.
            if principal.is_local_master() {
                return Err(Rejection::new(
                    StatusCode::FORBIDDEN,
                    "the master credential is not accepted on remote connections",
                ));
            }
            // A peer credential is scoped to one machine-to-machine link and is
            // meaningless from a browser. Refusing it here keeps the relay out
            // of the mesh's trust path entirely: a compromised relay must not be
            // able to replay a peer credential it somehow observed and arrive as
            // a paired machine.
            if principal.is_peer() {
                return Err(Rejection::new(
                    StatusCode::FORBIDDEN,
                    "a peer credential is not accepted on remote connections",
                ));
            }
            return Ok(Authenticated {
                principal,
                kind: AuthKind::Bearer,
            });
        }
        return Err(Rejection::new(
            StatusCode::UNAUTHORIZED,
            "this connection requires a paired device session",
        ));
    }

    // Compatibility ingress: unchanged. A cookie still works (the same browser
    // may have bootstrapped one), then a bearer, then the legacy query token.
    if let Some(cookie) = cookie {
        if let Some(principal) = session(&cookie) {
            return Ok(Authenticated {
                principal,
                kind: AuthKind::Cookie,
            });
        }
    }
    if let Some(token) = bearer.or_else(|| params.get("token").cloned()) {
        if let Some(principal) = lookup(&token) {
            return Ok(Authenticated {
                principal,
                kind: if headers.contains_key(header::AUTHORIZATION) {
                    AuthKind::Bearer
                } else {
                    AuthKind::QueryToken
                },
            });
        }
    }
    Err(Rejection::new(StatusCode::UNAUTHORIZED, "bad token"))
}

/// Whether a cookie-authenticated state change carries its double-submit proof.
///
/// Only cookie sessions need this: a bearer token is attached by client code
/// that had to know the credential, whereas a cookie is attached by the browser
/// on the strength of the URL alone.
pub fn csrf_ok(kind: AuthKind, headers: &HeaderMap) -> bool {
    if kind != AuthKind::Cookie {
        return true;
    }
    let Some(cookie) = cookie_value(headers, SESSION_COOKIE) else {
        return false;
    };
    headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|presented| presented == csrf_token(&cookie))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobile::{Capability, DeviceGrant};

    fn device() -> Principal {
        Principal::Device(DeviceGrant {
            id: "dev-1".into(),
            capabilities: vec![Capability::Threads],
        })
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The lookup used by most tests: "amd_ok" is a device, "master" is Master.
    fn lookup(token: &str) -> Option<Principal> {
        match token {
            "amd_ok" => Some(device()),
            "master" => Some(Principal::Master),
            _ => None,
        }
    }

    fn sessions(token: &str) -> Option<Principal> {
        (token == "tks_live").then(device)
    }

    #[test]
    fn remote_refuses_every_credential_in_the_url() {
        for key in CREDENTIAL_QUERY_KEYS {
            let err = authenticate(
                IngressPolicy::Remote,
                &HeaderMap::new(),
                &query(&[(key, "amd_ok")]),
                lookup,
                sessions,
            )
            .err()
            .unwrap_or_else(|| panic!("{key} in the URL must be refused"));
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
        }
        // Even alongside a credential that would otherwise work.
        assert!(authenticate(
            IngressPolicy::Remote,
            &headers(&[("authorization", "Bearer amd_ok")]),
            &query(&[("token", "amd_ok")]),
            lookup,
            sessions,
        )
        .is_err());
    }

    #[test]
    fn remote_refuses_the_master_credential_even_when_valid() {
        let err = authenticate(
            IngressPolicy::Remote,
            &headers(&[("authorization", "Bearer master")]),
            &HashMap::new(),
            lookup,
            sessions,
        )
        .err()
        .expect("master must not authenticate remotely");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn remote_accepts_a_device_bearer_and_a_session_cookie() {
        let native = authenticate(
            IngressPolicy::Remote,
            &headers(&[("authorization", "Bearer amd_ok")]),
            &HashMap::new(),
            lookup,
            sessions,
        )
        .expect("native device bearer");
        assert_eq!(native.kind, AuthKind::Bearer);
        assert_eq!(native.principal, device());

        let browser = authenticate(
            IngressPolicy::Remote,
            &headers(&[("cookie", "tk_session=tks_live")]),
            &HashMap::new(),
            lookup,
            sessions,
        )
        .expect("cookie session");
        assert_eq!(browser.kind, AuthKind::Cookie);

        // A dead cookie is a sign-in prompt, not a fallthrough to anything else.
        let stale = authenticate(
            IngressPolicy::Remote,
            &headers(&[("cookie", "tk_session=tks_dead")]),
            &HashMap::new(),
            lookup,
            sessions,
        )
        .err()
        .expect("expired session");
        assert_eq!(stale.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn compat_keeps_the_lan_product_working() {
        let query_token = authenticate(
            IngressPolicy::Compat,
            &HeaderMap::new(),
            &query(&[("token", "master")]),
            lookup,
            sessions,
        )
        .expect("the LAN URL still authenticates");
        assert_eq!(query_token.kind, AuthKind::QueryToken);
        assert!(query_token.principal.is_local_master());

        assert!(authenticate(
            IngressPolicy::Compat,
            &HeaderMap::new(),
            &query(&[("token", "nonsense")]),
            lookup,
            sessions,
        )
        .is_err());
    }

    #[test]
    fn csrf_is_required_only_for_cookies_and_cannot_be_guessed() {
        let cookie = "tks_live";
        let good = headers(&[
            ("cookie", &format!("tk_session={cookie}") as &str),
            ("x-threadknot-csrf", &csrf_token(cookie)),
        ]);
        assert!(csrf_ok(AuthKind::Cookie, &good));

        let missing = headers(&[("cookie", &format!("tk_session={cookie}") as &str)]);
        assert!(!csrf_ok(AuthKind::Cookie, &missing));

        let wrong = headers(&[
            ("cookie", &format!("tk_session={cookie}") as &str),
            ("x-threadknot-csrf", "not-it"),
        ]);
        assert!(!csrf_ok(AuthKind::Cookie, &wrong));

        // A bearer caller had to know the credential, so it needs no second proof.
        assert!(csrf_ok(AuthKind::Bearer, &HeaderMap::new()));
        assert!(csrf_ok(AuthKind::QueryToken, &HeaderMap::new()));

        // The token is derived from the cookie, so it is unguessable without it.
        assert_ne!(csrf_token("tks_live"), csrf_token("tks_other"));
    }

    #[test]
    fn cookies_parse_out_of_a_crowded_header() {
        let map = headers(&[("cookie", "other=1; tk_session=tks_live; theme=dark")]);
        assert_eq!(cookie_value(&map, SESSION_COOKIE).as_deref(), Some("tks_live"));
        assert_eq!(cookie_value(&map, "missing"), None);
        assert_eq!(cookie_value(&HeaderMap::new(), SESSION_COOKIE), None);
    }

    #[test]
    fn session_store_issues_resolves_and_revokes() {
        let dir = std::env::temp_dir().join(format!("tk-sess-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = BrowserSessions::open(&dir).unwrap();

        let token = store.issue("dev-1");
        assert!(token.starts_with("tks_"));
        assert_eq!(store.resolve(&token).as_deref(), Some("dev-1"));
        assert_eq!(store.resolve("tks_nope"), None);
        assert_eq!(store.resolve("not-a-session"), None);

        // The plaintext is never written down.
        let raw = std::fs::read_to_string(dir.join("browser-sessions.json")).unwrap();
        assert!(!raw.contains(&token), "session token must not persist");

        // Survives a restart, dies on revoke.
        let reloaded = BrowserSessions::open(&dir).unwrap();
        assert_eq!(reloaded.resolve(&token).as_deref(), Some("dev-1"));
        assert_eq!(reloaded.revoke_device("dev-2"), 0, "another device is untouched");
        assert_eq!(reloaded.revoke_device("dev-1"), 1);
        assert_eq!(reloaded.resolve(&token), None);

        let a = reloaded.issue("dev-1");
        let b = reloaded.issue("dev-2");
        assert_eq!(reloaded.revoke_all(), 2, "disabling remote access signs everyone out");
        assert!(reloaded.resolve(&a).is_none() && reloaded.resolve(&b).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_session_cookie_is_locked_down() {
        let header = session_cookie_header("tks_x");
        for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(header.contains(attribute), "cookie is missing {attribute}");
        }
        assert!(
            !header.to_lowercase().contains("domain="),
            "host-scoped: a sibling installation on the relay domain must never receive it"
        );
    }
}

//! Live authenticated connections, tracked by the device that opened them.
//!
//! Authentication is resolved once, at upgrade. Without this registry, removing
//! a phone from `mobile.json` only stopped its *next* connection: the app,
//! terminal and browser sockets it already held stayed open and fully usable,
//! so "revoke" did not actually revoke anything until the user closed the app.
//!
//! Every authenticated socket registers here and holds a [`SessionGuard`].
//! Revoking a device — or narrowing its grants, which is a revocation of part of
//! its authority — closes those sockets immediately.

use crate::ingress::IngressPolicy;
use crate::limits;
use crate::mobile::Principal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Which socket this is, because the three cost very different things and a
/// generous number for one is an absurd number for another (SEC-014).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// `/ws` — the app protocol.
    App,
    /// `/term` — an interactive shell.
    Terminal,
    /// `/browser` — a screencast plus a Chrome behind it.
    Browser,
}

impl SessionKind {
    fn limit(self) -> usize {
        match self {
            SessionKind::App => limits::MAX_APP_SOCKETS_PER_PRINCIPAL,
            SessionKind::Terminal => limits::MAX_TERMINAL_SOCKETS_PER_PRINCIPAL,
            SessionKind::Browser => limits::MAX_BROWSER_SOCKETS_PER_PRINCIPAL,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SessionKind::App => "app",
            SessionKind::Terminal => "terminal",
            SessionKind::Browser => "browser",
        }
    }
}

/// The identity a quota is counted against.
///
/// A device is its pairing, so revoking and re-pairing starts a fresh count. A
/// peer is the machine, not whoever it is currently acting for: one paired
/// machine's fan-out is one machine's worth of load however many of its own
/// clients are behind it. Master is the desktop itself, and is counted too —
/// the desktop's own webview can wedge and reconnect in a loop just as a phone
/// can, and a limit that exempts the common case is a limit that is never tested.
fn quota_owner(principal: &Principal) -> String {
    match principal {
        Principal::Master => "master".to_string(),
        Principal::Device(grant) => format!("device:{}", grant.id),
        Principal::Peer(peer) => format!("peer:{}", peer.machine_id),
    }
}

struct Entry {
    device_id: String,
    /// Who this counts against for the per-principal socket caps.
    owner: String,
    kind: SessionKind,
    /// Which listener opened it, so "turn remote access off" can close the
    /// remote sockets without dropping the desktop's own connection.
    policy: IngressPolicy,
    /// Flipped to `true` exactly once, to close this socket. A watch channel
    /// rather than a `Notify` so a guard that starts awaiting *after* the close
    /// still sees it — a missed wake-up would leave a revoked socket alive.
    close: tokio::sync::watch::Sender<bool>,
}

#[derive(Default)]
pub struct SessionRegistry {
    live: Mutex<HashMap<u64, Entry>>,
    next: AtomicU64,
}

impl SessionRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a socket opened by `principal`, or refuse it because that
    /// principal already holds this kind's share of the machine.
    ///
    /// Master sessions are tracked too (so the count is honest) but are never
    /// closed by `close_device` — the master credential is the desktop's own,
    /// not a revocable pairing.
    ///
    /// Called *before* the WebSocket upgrade, which is the only place a refusal
    /// can still be an HTTP status with an explanation in it; after the upgrade
    /// the only vocabulary left is a close frame.
    pub fn try_register(
        self: &Arc<Self>,
        principal: &Principal,
        policy: IngressPolicy,
        kind: SessionKind,
    ) -> anyhow::Result<SessionGuard> {
        let owner = quota_owner(principal);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (close, closed) = tokio::sync::watch::channel(false);
        {
            let mut live = self.live.lock().unwrap();
            let held = live
                .values()
                .filter(|e| e.owner == owner && e.kind == kind)
                .count();
            anyhow::ensure!(
                held < kind.limit(),
                "too many open {} connections: this device already holds {held} and the limit is {} \
                 per device — close one and try again",
                kind.label(),
                kind.limit()
            );
            live.insert(
                id,
                Entry {
                    device_id: principal.device_id().unwrap_or_default().to_string(),
                    owner,
                    kind,
                    policy,
                    close,
                },
            );
        }
        Ok(SessionGuard {
            registry: Arc::clone(self),
            id,
            closed,
        })
    }

    /// Close every live socket belonging to `device_id`. Returns how many were
    /// signalled. An empty id matches nothing, so a master session can never be
    /// caught by a device revocation.
    pub fn close_device(&self, device_id: &str) -> usize {
        if device_id.is_empty() {
            return 0;
        }
        let live = self.live.lock().unwrap();
        live.values()
            .filter(|e| e.device_id == device_id)
            .map(|e| {
                let _ = e.close.send(true);
            })
            .count()
    }

    /// Close every socket that arrived on the strict ingress — what turning
    /// remote access off has to mean for anyone already connected through it.
    pub fn close_remote(&self) -> usize {
        let live = self.live.lock().unwrap();
        live.values()
            .filter(|e| e.policy.is_remote())
            .map(|e| {
                let _ = e.close.send(true);
            })
            .count()
    }

    pub fn live_count(&self) -> usize {
        self.live.lock().unwrap().len()
    }

    /// Live sockets currently held by `device_id`.
    pub fn device_count(&self, device_id: &str) -> usize {
        self.live
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.device_id == device_id)
            .count()
    }

    fn deregister(&self, id: u64) {
        self.live.lock().unwrap().remove(&id);
    }
}

/// Held for the lifetime of one authenticated socket. Dropping it deregisters
/// the session, so a normal disconnect needs no bookkeeping at the call site.
pub struct SessionGuard {
    registry: Arc<SessionRegistry>,
    id: u64,
    closed: tokio::sync::watch::Receiver<bool>,
}

impl SessionGuard {
    /// Resolves when this session has been revoked. Callers `select!` it against
    /// their socket loop; dropping the loop future drops the socket, which is
    /// what actually closes the connection.
    pub async fn closed(&self) {
        let mut rx = self.closed.clone();
        // Already closed before anyone awaited (revoke raced the upgrade).
        if *rx.borrow() {
            return;
        }
        let _ = rx.wait_for(|closed| *closed).await;
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.registry.deregister(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobile::{Capability, DeviceGrant};

    const LAN: IngressPolicy = IngressPolicy::Compat;

    fn device(id: &str) -> Principal {
        Principal::Device(DeviceGrant {
            id: id.into(),
            capabilities: vec![Capability::Threads],
        })
    }

    /// An app socket that is expected to be admitted.
    fn app(
        registry: &Arc<SessionRegistry>,
        principal: &Principal,
        policy: IngressPolicy,
    ) -> SessionGuard {
        registry
            .try_register(principal, policy, SessionKind::App)
            .expect("within the connection limit")
    }

    #[test]
    fn the_n_plus_first_socket_of_one_kind_is_refused_by_name() {
        let registry = SessionRegistry::new();
        let principal = device("dev-1");
        let held: Vec<SessionGuard> = (0..limits::MAX_TERMINAL_SOCKETS_PER_PRINCIPAL)
            .map(|_| {
                registry
                    .try_register(&principal, LAN, SessionKind::Terminal)
                    .expect("under the limit")
            })
            .collect();

        let refused = match registry.try_register(&principal, LAN, SessionKind::Terminal) {
            Ok(_) => panic!("the N+1th terminal must be refused"),
            Err(e) => e,
        };
        let message = format!("{refused:#}");
        assert!(message.contains("terminal"), "{message}");
        assert!(
            message.contains(&limits::MAX_TERMINAL_SOCKETS_PER_PRINCIPAL.to_string()),
            "the refusal must name the limit: {message}"
        );

        // The cap is per kind and per principal, not a global connection budget.
        registry
            .try_register(&principal, LAN, SessionKind::Browser)
            .expect("a browser socket is a different budget");
        registry
            .try_register(&device("dev-2"), LAN, SessionKind::Terminal)
            .expect("another device has its own budget");

        // And it is a *live* count: closing one makes room again.
        drop(held);
        registry
            .try_register(&principal, LAN, SessionKind::Terminal)
            .expect("a closed socket frees its slot");
    }

    #[tokio::test]
    async fn revoking_a_device_closes_only_its_sockets() {
        let registry = SessionRegistry::new();
        let mine = app(&registry, &device("dev-1"), LAN);
        let also_mine = app(&registry, &device("dev-1"), LAN);
        let theirs = app(&registry, &device("dev-2"), LAN);
        let master = app(&registry, &Principal::Master, LAN);
        assert_eq!(registry.live_count(), 4);

        assert_eq!(registry.close_device("dev-1"), 2);
        // Both of this device's sockets resolve; the others are still waiting.
        tokio::time::timeout(std::time::Duration::from_secs(1), mine.closed())
            .await
            .expect("revoked socket must close");
        tokio::time::timeout(std::time::Duration::from_secs(1), also_mine.closed())
            .await
            .expect("every socket of a revoked device closes");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), theirs.closed())
                .await
                .is_err(),
            "another device's socket must be untouched"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), master.closed())
                .await
                .is_err(),
            "the desktop's own session is not a revocable pairing"
        );
    }

    /// A revoke that lands between the upgrade and the first await must still be
    /// seen — otherwise the socket sleeps forever holding live authority.
    #[tokio::test]
    async fn a_close_before_the_first_await_is_not_missed() {
        let registry = SessionRegistry::new();
        let guard = app(&registry, &device("dev-1"), LAN);
        registry.close_device("dev-1");
        tokio::time::timeout(std::time::Duration::from_secs(1), guard.closed())
            .await
            .expect("close must not be lost");
    }

    /// Turning remote access off must not drop the desktop's own socket.
    #[tokio::test]
    async fn closing_remote_sessions_leaves_the_lan_alone() {
        let registry = SessionRegistry::new();
        let lan = app(&registry, &device("dev-1"), IngressPolicy::Compat);
        let remote = app(&registry, &device("dev-1"), IngressPolicy::Remote);

        assert_eq!(registry.close_remote(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(1), remote.closed())
            .await
            .expect("the remote socket closes");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), lan.closed())
                .await
                .is_err(),
            "the same device's LAN socket keeps working"
        );
    }

    #[tokio::test]
    async fn dropping_a_guard_deregisters_it() {
        let registry = SessionRegistry::new();
        {
            let _guard = app(&registry, &device("dev-1"), LAN);
            assert_eq!(registry.device_count("dev-1"), 1);
        }
        assert_eq!(registry.live_count(), 0);
        assert_eq!(registry.close_device("dev-1"), 0);
        assert_eq!(
            registry.close_device(""),
            0,
            "an empty device id must never match a master session"
        );
    }
}

//! Live mesh runtime: one persistent outbound WebSocket per paired peer,
//! presence tracking, `peer.announce` fan-out, and mDNS discovery.
//!
//! DHCP resilience is three layers, and the machine id is the ONLY durable
//! key in all of them:
//! 1. mDNS (`_threadknot._tcp.local.`) — a known machine id seen at a new
//!    address updates the registry and triggers a reconnect.
//! 2. Active announce — on startup and whenever the local interface set
//!    changes, we push `peer.announce {machineId, addresses, port}` down
//!    every connected peer socket; the receiving side refreshes its hints.
//! 3. Verified reconnects — every connection replays `hello` and checks the
//!    reported machine id against the registry BEFORE trusting the address
//!    (a reused DHCP lease answering on a stale IP is detected, not obeyed).

use crate::agents::Hub;
use crate::device::Device;
use crate::mobile::Capability;
use crate::peers::{Peer, PeerRegistry};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, Notify};
use tokio_tungstenite::tungstenite::Message;

/// How long one routed request waits for its peer's answer. Independent of the
/// queue bound: the bound decides whether a request is accepted at all, this
/// decides how long an accepted one may sit unanswered.
const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// An RPC to forward down a peer's live socket; `reply` resolves with the
/// peer's response frame data.
struct OutRequest {
    kind: String,
    payload: Value,
    /// Whose authority the far side should run this as. `None` is this machine's
    /// owner; `Some(list)` narrows it to a device's grants. Carried per request
    /// because one socket multiplexes every client on this machine.
    on_behalf_of: Option<Vec<Capability>>,
    reply: oneshot::Sender<Result<Value>>,
}

/// Build the TLS-and-credential half of a peer request.
///
/// Both the credential and the grant assertion are **headers**. That is the
/// point of SEC-012: a URL is copied into logs, `Referer` headers, shell
/// history and crash reports, so a credential in one is a credential published.
fn peer_headers(
    peer: &Peer,
    on_behalf_of: Option<&[Capability]>,
) -> Vec<(&'static str, String)> {
    let mut headers = vec![(
        "authorization",
        format!("Bearer {}", peer.outbound_credential),
    )];
    if let Some(value) = crate::ingress::mesh_grants_header_value(on_behalf_of) {
        headers.push((crate::ingress::MESH_GRANTS_HEADER, value));
    }
    headers
}

/// Dial a peer's TLS mesh listener at `addr` and complete the handshake against
/// its **pinned** certificate authority.
///
/// The synthetic `<machine-id>.threadknot.mesh` name is what makes identity
/// independent of address: the TCP connection goes to whatever hint we are
/// currently trying, while the certificate check asks "is this really machine
/// X". An attacker who takes the address — a reused DHCP lease, an mDNS spoof,
/// an ARP game — completes the TCP connection and then fails the handshake.
async fn connect_tls(
    peer: &Peer,
    addr: &str,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    anyhow::ensure!(!peer.mesh_ca.is_empty(), "no pinned certificate for this peer");
    anyhow::ensure!(peer.mesh_port != 0, "no mesh port for this peer");
    let config = crate::mesh::client_config_for_ca(&peer.mesh_ca)?;
    let target = host_port(addr, peer.mesh_port);
    let tcp = tokio::time::timeout(Duration::from_secs(8), tokio::net::TcpStream::connect(&target))
        .await
        .context("connect timeout")?
        .with_context(|| format!("connect {target}"))?;
    // Nagle off: this carries interactive keystrokes and small RPC frames, where
    // coalescing shows up directly as input latency.
    let _ = tcp.set_nodelay(true);
    let name = crate::mesh::mesh_dns_name(&peer.machine_id);
    let server_name = rustls_pki_types::ServerName::try_from(name.clone())
        .with_context(|| format!("invalid mesh name {name}"))?;
    let connector = tokio_rustls::TlsConnector::from(config);
    tokio::time::timeout(Duration::from_secs(8), connector.connect(server_name, tcp))
        .await
        .context("TLS handshake timeout")?
        .context("peer certificate did not match the one pinned at pairing")
}

/// An unpaired Threadknot seen via mDNS (Settings shows these with a pre-filled
/// address so pairing is one token-paste away).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPeer {
    pub machine_id: String,
    pub name: String,
    pub addresses: Vec<String>,
    pub port: u16,
    pub mesh_version: u32,
}

pub struct PeerNet {
    pub registry: Arc<PeerRegistry>,
    hub: Arc<Hub>,
    device: Arc<Device>,
    own_port: u16,
    online: Mutex<HashMap<String, bool>>,
    discovered: Mutex<HashMap<String, DiscoveredPeer>>,
    tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Live outbound request channels, one per CONNECTED peer.
    ///
    /// Bounded (SEC-014): a peer that completes the TLS handshake and then stops
    /// reading used to grow this queue for as long as callers kept asking, on
    /// *this* machine. See `request` for what a full queue means.
    links: Mutex<HashMap<String, mpsc::Sender<OutRequest>>>,
    /// Wake the supervisor to reconcile tasks with the registry NOW
    /// (peer added/removed, fresh address hints).
    pub kick: Notify,
    /// Tells every connected peer task to re-send `peer.announce`.
    announce: broadcast::Sender<()>,
    /// One `reqwest` client per peer *and address*, for the byte proxy.
    ///
    /// Cached because each one carries a rustls config built from that peer's
    /// pinned CA plus a name-resolution override, and rebuilding it for every
    /// thumbnail in a remote file tree is real work. Keyed by address too, so a
    /// DHCP move invalidates the entry that pointed at the old one instead of
    /// silently failing every request.
    byte_clients: Mutex<HashMap<(String, String), reqwest::Client>>,
}

impl PeerNet {
    pub fn new(
        registry: Arc<PeerRegistry>,
        hub: Arc<Hub>,
        device: Arc<Device>,
        own_port: u16,
    ) -> Arc<Self> {
        let (announce, _) = broadcast::channel(4);
        Arc::new(Self {
            registry,
            hub,
            device,
            own_port,
            online: Mutex::new(HashMap::new()),
            discovered: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            kick: Notify::new(),
            announce,
            byte_clients: Mutex::new(HashMap::new()),
        })
    }

    /// An HTTPS client that trusts exactly this peer's pinned certificate
    /// authority and resolves its synthetic mesh name to `addr`.
    ///
    /// `resolve` rather than connecting to the IP directly, because the
    /// certificate names the machine, not the address. Without the override the
    /// synthetic name would go to real DNS and fail; with it, TLS verifies
    /// identity while the address remains a disposable hint.
    pub fn byte_client(&self, peer: &Peer, addr: &str) -> Result<reqwest::Client> {
        let key = (peer.machine_id.clone(), addr.to_string());
        if let Some(client) = self.byte_clients.lock().unwrap().get(&key) {
            return Ok(client.clone());
        }
        anyhow::ensure!(!peer.mesh_ca.is_empty(), "no pinned certificate for this peer");
        anyhow::ensure!(peer.mesh_port != 0, "no mesh port for this peer");
        let socket: std::net::SocketAddr = host_port(addr, peer.mesh_port)
            .parse()
            .with_context(|| format!("peer address {addr} is not an IP"))?;
        let client = reqwest::Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_pem(peer.mesh_ca.as_bytes())
                    .context("parse peer mesh CA")?,
            )
            // The public roots are off deliberately: a peer is authenticated by
            // its own CA and nothing else, so trusting a public authority as
            // well would mean any certificate for `*.threadknot.mesh` from any
            // CA on earth impersonates a paired machine.
            .tls_built_in_root_certs(false)
            .resolve(&crate::mesh::mesh_dns_name(&peer.machine_id), socket)
            .build()
            .context("build peer byte client")?;
        self.byte_clients
            .lock()
            .unwrap()
            .insert(key, client.clone());
        Ok(client)
    }

    /// Spawn the supervisor + interface watcher + mDNS. Needs a runtime.
    pub fn start(self: &Arc<Self>) {
        let net = Arc::clone(self);
        tokio::spawn(async move { net.supervisor().await });
        let net = Arc::clone(self);
        tokio::spawn(async move { net.interface_watcher().await });
        let net = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = net.mdns().await {
                tracing::warn!("mdns disabled: {e:#}");
            }
        });
    }

    pub fn is_online(&self, machine_id: &str) -> bool {
        *self.online.lock().unwrap().get(machine_id).unwrap_or(&false)
    }

    /// Forward an RPC to a connected peer and await its response. This is the
    /// whole remote-thread proxy: the request rides the persistent peer
    /// socket exactly as a browser client would send it.
    ///
    /// `on_behalf_of` carries the *caller's* authority across the link. `None`
    /// is this machine's owner. Passing a device's grants is what stops the far
    /// side treating a phone's request as its own owner's — the laundering that
    /// SEC-002 patched case by case and this closes structurally.
    pub async fn request(
        &self,
        machine_id: &str,
        kind: &str,
        payload: Value,
        on_behalf_of: Option<Vec<Capability>>,
    ) -> Result<Value> {
        let tx = self
            .links
            .lock()
            .unwrap()
            .get(machine_id)
            .cloned()
            .ok_or_else(|| self.offline_error(machine_id))?;
        let (reply, rx) = oneshot::channel();
        // `try_send`, not `send`: a full queue is not backpressure to wait out, it
        // is evidence that the peer's socket is not draining, and the answer is
        // the same in 30 seconds as it is now. Waiting for the per-request timeout
        // instead would hold the caller — a phone tapping a remote thread — for
        // half a minute before saying so, and would let every subsequent caller
        // queue behind it.
        tx.try_send(OutRequest {
            kind: kind.to_string(),
            payload,
            on_behalf_of,
            reply,
        })
        .map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => self.unresponsive_error(machine_id),
            mpsc::error::TrySendError::Closed(_) => anyhow::anyhow!("peer disconnected"),
        })?;
        tokio::time::timeout(PEER_REQUEST_TIMEOUT, rx)
            .await
            .context("peer request timed out")?
            .map_err(|_| anyhow::anyhow!("peer disconnected mid-request"))?
    }

    /// A connected peer that has stopped draining its request queue, phrased for
    /// the fleet view. Distinct from "offline": the link is up and the
    /// certificate checked out, so telling someone their machine is offline would
    /// send them looking at the network instead of at that machine.
    fn unresponsive_error(&self, machine_id: &str) -> anyhow::Error {
        let name = self
            .registry
            .peer(machine_id)
            .map(|p| p.name)
            .unwrap_or_else(|| machine_id.to_string());
        anyhow::anyhow!(
            "{name} is connected but not responding — {} requests are already queued for it, \
             which is the limit; it may be busy or wedged",
            crate::limits::PEER_REQUEST_QUEUE
        )
    }

    /// Why a peer cannot be reached, phrased for whoever is looking at the
    /// fleet view. A pair that predates the encrypted mesh is a *different*
    /// problem from a machine being asleep, and saying "offline" for both sends
    /// someone hunting a network fault that does not exist.
    fn offline_error(&self, machine_id: &str) -> anyhow::Error {
        let Some(peer) = self.registry.peer(machine_id) else {
            return anyhow::anyhow!("unknown machine");
        };
        if peer.needs_upgrade() {
            return anyhow::anyhow!(
                "{} was paired before Threadknot encrypted mesh connections — update Threadknot on \
                 that machine, then pair the two again",
                peer.name
            );
        }
        anyhow::anyhow!("{} is offline", peer.name)
    }

    pub fn discovered(&self) -> Vec<DiscoveredPeer> {
        let paired: BTreeSet<String> = self
            .registry
            .list()
            .into_iter()
            .map(|p| p.machine_id)
            .collect();
        self.discovered
            .lock()
            .unwrap()
            .values()
            .filter(|d| !paired.contains(&d.machine_id) && d.machine_id != self.device.machine_id)
            .cloned()
            .collect()
    }

    fn set_online(&self, machine_id: &str, online: bool) {
        let changed = {
            let mut map = self.online.lock().unwrap();
            map.insert(machine_id.to_string(), online) != Some(online)
        };
        if changed {
            self.hub.broadcast_state("peers", None);
        }
    }

    /// One task per registered peer; removed peers get their task aborted.
    async fn supervisor(self: Arc<Self>) {
        loop {
            {
                let peers = self.registry.list();
                let ids: BTreeSet<&str> = peers.iter().map(|p| p.machine_id.as_str()).collect();
                let mut tasks = self.tasks.lock().unwrap();
                tasks.retain(|id, handle| {
                    if ids.contains(id.as_str()) && !handle.is_finished() {
                        true
                    } else {
                        handle.abort();
                        false
                    }
                });
                for p in &peers {
                    if !tasks.contains_key(&p.machine_id) {
                        let net = Arc::clone(&self);
                        let id = p.machine_id.clone();
                        tasks.insert(
                            p.machine_id.clone(),
                            tokio::spawn(async move { net.peer_task(id).await }),
                        );
                    }
                }
                // Presence for removed peers must not linger.
                let mut online = self.online.lock().unwrap();
                online.retain(|id, _| ids.contains(id.as_str()));
            }
            tokio::select! {
                _ = self.kick.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(10)) => {}
            }
        }
    }

    /// Connect-verify-hold loop for one peer, cycling address candidates
    /// with capped backoff.
    async fn peer_task(self: Arc<Self>, machine_id: String) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let Some(peer) = self.registry.peer(&machine_id) else {
                return;
            };
            // A pair from before the encrypted mesh is never connected. There is
            // deliberately no fallback to the old plaintext transport: keeping
            // one would mean an attacker on the LAN only has to wait for the
            // machine that has not been updated, which is the same as not having
            // fixed anything. The fleet view shows why, and re-pairing fixes it.
            if peer.needs_upgrade() {
                self.set_online(&machine_id, false);
                tracing::warn!(
                    "peer {} ({machine_id}) predates the encrypted mesh — not connecting; re-pair to upgrade",
                    peer.name
                );
                tokio::select! {
                    _ = self.kick.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
                continue;
            }
            // last-good first, then the hint list in order.
            let mut candidates: Vec<String> = Vec::new();
            if let Some(good) = &peer.last_good_address {
                candidates.push(good.clone());
            }
            for a in &peer.addresses {
                if !candidates.contains(a) {
                    candidates.push(a.clone());
                }
            }
            let mut connected = false;
            for addr in candidates {
                match self.connect_and_hold(&machine_id, &addr).await {
                    Ok(()) => {
                        // Held a verified session that later closed cleanly.
                        connected = true;
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("peer {machine_id} via {addr}: {e:#}");
                    }
                }
            }
            self.set_online(&machine_id, false);
            if connected {
                backoff = Duration::from_secs(1);
            } else {
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
            tokio::select! {
                _ = self.kick.notified() => { backoff = Duration::from_secs(1); }
                _ = tokio::time::sleep(backoff) => {}
            }
        }
    }

    async fn connect_and_hold(self: &Arc<Self>, machine_id: &str, addr: &str) -> Result<()> {
        let peer = self.registry.peer(machine_id).context("peer removed")?;
        let tls = connect_tls(&peer, addr).await?;
        // No `on_behalf_of` on the socket itself: this link carries plumbing
        // frames and requests from many different callers, and each routed frame
        // states its own authority. A connection-level assertion here would be
        // the wrong granularity and would have to be the widest of them.
        let request = mesh_ws_request(&peer, "/ws", "", None)?;
        let (mut ws, _) = tokio::time::timeout(
            Duration::from_secs(8),
            tokio_tungstenite::client_async(request, tls),
        )
        .await
        .context("connect timeout")?
        .context("peer refused the mesh credential")?;

        // Verify identity BEFORE trusting the address: whatever answered
        // must report the machine id we paired with. Belt and braces now that
        // the certificate already proves it — but this check is also what
        // detects a machine that has been *re-installed* and kept its address
        // while its identity changed underneath.
        ws.send(Message::Text(
            json!({"id": 1, "type": "hello", "payload": {}}).to_string().into(),
        ))
        .await?;
        let hello = tokio::time::timeout(Duration::from_secs(10), read_response(&mut ws, 1))
            .await
            .context("hello timeout")??;
        let reported = hello
            .get("machineId")
            .or_else(|| hello.get("serverId"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        anyhow::ensure!(
            reported == machine_id,
            "address {addr} answered as {reported:?}, expected {machine_id}"
        );

        let _ = self
            .registry
            .note_addresses(machine_id, &[addr.to_string()], None, Some(addr));
        // The hello also carries the peer's current REAL profile (name +
        // appearance); merge it into the peer record last-write-wins by the
        // profileUpdatedAt clock. Older peers omit the fields, which leaves
        // the stored values alone.
        if self
            .registry
            .note_profile(
                machine_id,
                hello.get("friendlyName").and_then(|v| v.as_str()).map(String::from),
                patch_str(&hello, "avatar"),
                patch_str(&hello, "color"),
                hello.get("profileUpdatedAt").and_then(|v| v.as_str()).map(String::from),
            )
            .unwrap_or(false)
        {
            self.hub.broadcast_state("peers", None);
        }
        self.set_online(machine_id, true);
        tracing::info!("peer online: {} via {addr}", peer.name);

        // Publish the outbound-request channel so handle_request can proxy
        // RPCs to this peer; torn down (with pending requests failed) on any
        // exit from the hold loop below.
        let (out_tx, mut out_rx) = mpsc::channel::<OutRequest>(crate::limits::PEER_REQUEST_QUEUE);
        self.links
            .lock()
            .unwrap()
            .insert(machine_id.to_string(), out_tx);
        let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
        let mut req_id: u64 = 2;

        let held: Result<()> = async {
            // Tell the peer where WE live (covers the my-IP-changed-but-
            // yours-didn't half of a DHCP move)…
            ws.send(Message::Text(self.announce_frame(&mut req_id).to_string().into()))
                .await?;
            // …and push the whole workspace catalog — every workspace on
            // every machine is mesh-visible — plus tombstones, so edits AND
            // deletes reconcile after either side was offline (receiver
            // merges LWW by updatedAt / deletedAt).
            for w in self.hub.store.list_workspaces() {
                let id = req_id;
                req_id += 1;
                let frame = json!({
                    "id": id,
                    "type": "mesh.workspaceUpsert",
                    "payload": { "workspace": w },
                });
                ws.send(Message::Text(frame.to_string().into())).await?;
            }
            for (wid, deleted_at) in self.hub.store.workspace_tombstones() {
                let id = req_id;
                req_id += 1;
                let frame = json!({
                    "id": id,
                    "type": "mesh.workspaceDelete",
                    "payload": { "id": wid, "deletedAt": deleted_at },
                });
                ws.send(Message::Text(frame.to_string().into())).await?;
            }
            // Any dispatch this machine finished while that one was away still
            // owes it an answer. Completion is pushed rather than awaited — a
            // build outlives every timeout between here and there — so this
            // reconnect is the only thing that closes the loop for a worker
            // that finished into a void.
            crate::dispatch::flush_pending_reports(&self.hub, machine_id);

            let mut announce_rx = self.announce.subscribe();
            let mut ping = tokio::time::interval(Duration::from_secs(20));
            ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    msg = ws.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                self.handle_peer_frame(machine_id, &text, &mut pending);
                            }
                            Some(Ok(Message::Ping(data))) => ws.send(Message::Pong(data)).await?,
                            Some(Ok(Message::Close(_))) | None => return Ok(()),
                            Some(Ok(_)) => {}
                            Some(Err(e)) => return Err(e.into()),
                        }
                    }
                    out = out_rx.recv() => {
                        let Some(out) = out else { return Ok(()) };
                        let id = req_id;
                        req_id += 1;
                        let mut frame = json!({ "id": id, "type": out.kind, "payload": out.payload });
                        // A `mesh` sibling only when the caller was narrower
                        // than this machine's owner. Omitting it for the owner
                        // keeps plumbing frames as they were and makes the
                        // narrowing case the visible one on the wire.
                        if let Some(grants) = out.on_behalf_of {
                            frame["mesh"] = json!({
                                "onBehalfOf": grants.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                            });
                        }
                        pending.insert(id, out.reply);
                        ws.send(Message::Text(frame.to_string().into())).await?;
                    }
                    _ = announce_rx.recv() => {
                        ws.send(Message::Text(self.announce_frame(&mut req_id).to_string().into())).await?;
                    }
                    _ = ping.tick() => {
                        ws.send(Message::Ping(Vec::new().into())).await?;
                    }
                }
            }
        }
        .await;

        self.links.lock().unwrap().remove(machine_id);
        // Dropping the pending senders resolves in-flight request() calls
        // with "peer disconnected mid-request".
        drop(pending);
        held
    }

    /// One inbound frame from a peer socket: route responses to their
    /// waiting request, relay the peer's LOCAL event/state broadcasts to our
    /// own clients (origin-tagged so they are never relayed a second hop).
    fn handle_peer_frame(
        &self,
        machine_id: &str,
        text: &str,
        pending: &mut HashMap<u64, oneshot::Sender<Result<Value>>>,
    ) {
        let Ok(mut v) = serde_json::from_str::<Value>(text) else {
            return;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("response") => {
                let Some(id) = v.get("id").and_then(|i| i.as_u64()) else {
                    return;
                };
                let Some(reply) = pending.remove(&id) else {
                    return; // fire-and-forget frame (announce, resync)
                };
                let result = if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
                    Ok(v.get("data").cloned().unwrap_or(Value::Null))
                } else {
                    Err(anyhow::anyhow!(
                        "{}",
                        v.get("error").and_then(|e| e.as_str()).unwrap_or("peer error")
                    ))
                };
                let _ = reply.send(result);
            }
            Some("event") | Some("state.changed") => {
                // Relay only frames the peer produced itself — a frame that
                // already carries an origin came from a third machine and
                // relaying it again would ping-pong forever.
                if v.get("origin").is_none() {
                    v["origin"] = json!(machine_id);
                    let _ = self.hub.relay.send(v.to_string());
                }
            }
            _ => {}
        }
    }

    fn announce_frame(&self, req_id: &mut u64) -> Value {
        let id = *req_id;
        *req_id += 1;
        json!({
            "id": id,
            "type": "peer.announce",
            "payload": {
                "machineId": self.device.machine_id,
                "addresses": local_addresses(),
                "port": self.own_port,
                // The REAL profile (name + appearance) rides the announce so
                // edits reach connected peers live, exactly like fresh address
                // hints. An avatar data URL can be tens of KB, but the frontend
                // caps it at ~16 KB and announces are rare (connect/interface
                // change/profile edit), so the frame stays cheap. The receiver
                // merges it last-write-wins by profileUpdatedAt.
                "name": self.device.friendly_name(),
                "avatar": self.device.avatar(),
                "color": self.device.color(),
                "profileUpdatedAt": self.device.profile_updated_at(),
            }
        })
    }

    /// Re-announce to every connected peer NOW (used when this machine's
    /// appearance changes; interface changes have their own watcher).
    pub fn announce_now(&self) {
        let _ = self.announce.send(());
    }

    /// Poll the local interface set; on change, re-announce everywhere.
    async fn interface_watcher(self: Arc<Self>) {
        let mut last: BTreeSet<String> = local_addresses().into_iter().collect();
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now: BTreeSet<String> = local_addresses().into_iter().collect();
            if now != last && !now.is_empty() {
                tracing::info!("local addresses changed: {last:?} -> {now:?}; announcing");
                last = now;
                let _ = self.announce.send(());
            }
        }
    }

    /// Advertise ourselves and browse for other Threadknots. `mdns-sd` keeps
    /// registrations fresh across interface changes (addr_auto).
    async fn mdns(self: Arc<Self>) -> Result<()> {
        use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
        const SERVICE: &str = "_threadknot._tcp.local.";

        let daemon = ServiceDaemon::new().context("mdns daemon")?;
        let host = format!(
            "{}.local.",
            gethostname::gethostname().to_string_lossy().replace(' ', "-")
        );
        let props = [
            ("machineId", self.device.machine_id.as_str()),
            ("name", &self.device.friendly_name()),
            ("meshVersion", &crate::device::MESH_VERSION.to_string()),
        ];
        let info = ServiceInfo::new(
            SERVICE,
            &self.device.machine_id,
            &host,
            "",
            self.own_port,
            &props[..],
        )
        .context("mdns service info")?
        .enable_addr_auto();
        daemon.register(info).context("mdns register")?;

        let receiver = daemon.browse(SERVICE).context("mdns browse")?;
        while let Ok(event) = receiver.recv_async().await {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let get = |k: &str| info.get_property_val_str(k).unwrap_or("").to_string();
                    let machine_id = get("machineId");
                    if machine_id.is_empty() || machine_id == self.device.machine_id {
                        continue;
                    }
                    let addresses: Vec<String> = info
                        .get_addresses()
                        .iter()
                        .filter(|a| a.is_ipv4())
                        .map(|a| a.to_string())
                        .collect();
                    if addresses.is_empty() {
                        continue;
                    }
                    if self.registry.peer(&machine_id).is_some() {
                        // Known machine seen (possibly at a new address):
                        // refresh hints and let its task reconnect.
                        if self
                            .registry
                            .note_addresses(&machine_id, &addresses, Some(info.get_port()), None)
                            .unwrap_or(false)
                        {
                            self.kick.notify_waiters();
                            self.hub.broadcast_state("peers", None);
                        }
                    } else {
                        self.discovered.lock().unwrap().insert(
                            machine_id.clone(),
                            DiscoveredPeer {
                                machine_id,
                                name: get("name"),
                                addresses,
                                port: info.get_port(),
                                mesh_version: get("meshVersion").parse().unwrap_or(0),
                            },
                        );
                        self.hub.broadcast_state("peers", None);
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    // fullname = "<machineId>._threadknot._tcp.local."
                    let id = fullname.split('.').next().unwrap_or("").to_string();
                    if self.discovered.lock().unwrap().remove(&id).is_some() {
                        self.hub.broadcast_state("peers", None);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Build the WebSocket upgrade request for a peer endpoint.
///
/// The URI names the synthetic mesh host so it matches the certificate, and the
/// credential and grant assertion ride headers. `query` must already be encoded
/// and must not contain a credential — the mesh listener rejects one outright.
fn mesh_ws_request(
    peer: &Peer,
    path: &str,
    query: &str,
    on_behalf_of: Option<&[Capability]>,
) -> Result<axum::http::Request<()>> {
    let authority = format!(
        "{}:{}",
        crate::mesh::mesh_dns_name(&peer.machine_id),
        peer.mesh_port
    );
    let uri = if query.is_empty() {
        format!("wss://{authority}{path}")
    } else {
        format!("wss://{authority}{path}?{query}")
    };
    let mut builder = axum::http::Request::builder()
        .uri(&uri)
        .method("GET")
        // tungstenite fills the handshake headers, but `Host` has to agree with
        // the synthetic name or the far side's Origin check sees a mismatch.
        .header("host", &authority)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header(
            "sec-websocket-key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        );
    for (name, value) in peer_headers(peer, on_behalf_of) {
        builder = builder.header(name, value);
    }
    builder.body(()).context("build peer websocket request")
}

/// Bidirectionally splice a client's `/term` WebSocket onto a peer machine.
pub async fn splice_term(
    client: axum::extract::ws::WebSocket,
    state: crate::server::ServerState,
    machine_id: String,
    params: HashMap<String, String>,
    on_behalf_of: Option<Vec<Capability>>,
) {
    splice_ws(client, state, machine_id, params, "term", on_behalf_of).await
}

/// Bidirectionally splice a client's `/browser` WebSocket onto a peer
/// machine: Chrome runs there, the screencast frames stream through here.
/// This is what lets the owner sign a remote machine's browser in from the
/// machine they are actually sitting at.
pub async fn splice_browser(
    client: axum::extract::ws::WebSocket,
    state: crate::server::ServerState,
    machine_id: String,
    params: HashMap<String, String>,
    on_behalf_of: Option<Vec<Capability>>,
) {
    splice_ws(client, state, machine_id, params, "browser", on_behalf_of).await
}

/// Bidirectionally splice a client WebSocket onto the SAME endpoint on a peer
/// machine: the work runs there, the bytes stream through here.
///
/// The per-peer credential is attached server-side; the client's own credential
/// never works on the peer directly. `on_behalf_of` is the caller's authority,
/// which the far side enforces — one splice carries exactly one caller, so
/// unlike the multiplexed `/ws` link this can be a connection-level header.
///
/// Callers decide *who* may splice; this function assumes that check passed.
async fn splice_ws(
    client: axum::extract::ws::WebSocket,
    state: crate::server::ServerState,
    machine_id: String,
    params: HashMap<String, String>,
    path: &str,
    on_behalf_of: Option<Vec<Capability>>,
) {
    use axum::extract::ws::Message as AMsg;
    use tokio_tungstenite::tungstenite::Message as TMsg;

    let Some(peer) = state.peernet.registry.peer(&machine_id) else {
        return;
    };
    let Some(addr) = peer
        .last_good_address
        .clone()
        .or_else(|| peer.addresses.first().cloned())
    else {
        return;
    };
    // Every credential-bearing key is stripped rather than rewritten: the mesh
    // listener refuses a URL carrying one, so forwarding the client's own token
    // would fail the request outright instead of merely being redundant.
    let query: String = params
        .iter()
        .filter(|(k, _)| !crate::ingress::is_credential_query_key(k) && k.as_str() != "machineId")
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let tls = match connect_tls(&peer, &addr).await {
        Ok(tls) => tls,
        Err(e) => {
            tracing::warn!("{path} splice: cannot reach {} at {addr}: {e:#}", peer.name);
            return;
        }
    };
    let request = match mesh_ws_request(&peer, &format!("/{path}"), &query, on_behalf_of.as_deref())
    {
        Ok(request) => request,
        Err(e) => {
            tracing::warn!("{path} splice: {e:#}");
            return;
        }
    };
    let peer_ws = match tokio::time::timeout(
        Duration::from_secs(8),
        tokio_tungstenite::client_async(request, tls),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        _ => {
            tracing::warn!("{path} splice: {} refused the mesh credential", peer.name);
            return;
        }
    };

    let (mut c_tx, mut c_rx) = client.split();
    let (mut p_tx, mut p_rx) = peer_ws.split();

    // Client -> peer (keystrokes + resize control frames).
    let mut up = tokio::spawn(async move {
        while let Some(Ok(m)) = c_rx.next().await {
            let out = match m {
                AMsg::Text(t) => TMsg::Text(t.as_str().into()),
                AMsg::Binary(b) => TMsg::Binary(b),
                AMsg::Close(_) => break,
                _ => continue,
            };
            if p_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = p_tx.send(TMsg::Close(None)).await;
    });
    // Peer -> client (pty output + control notices).
    let mut down = tokio::spawn(async move {
        while let Some(Ok(m)) = p_rx.next().await {
            let out = match m {
                TMsg::Text(t) => AMsg::Text(t.as_str().into()),
                TMsg::Binary(b) => AMsg::Binary(b),
                TMsg::Close(_) => break,
                _ => continue,
            };
            if c_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = c_tx.send(AMsg::Close(None)).await;
    });
    tokio::select! {
        _ = &mut up => down.abort(),
        _ = &mut down => up.abort(),
    }
}

/// Tri-state read of an optional string field from a peer frame: an absent
/// key (an older Threadknot) is `None` (leave the stored value alone), an
/// explicit `null` is `Some(None)` (clear), a string is `Some(Some(v))`.
pub fn patch_str(v: &Value, key: &str) -> Option<Option<String>> {
    match v.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(x) => Some(x.as_str().map(String::from)),
    }
}

/// Format host:port, bracketing IPv6 literals.
pub fn host_port(addr: &str, port: u16) -> String {
    if addr.contains(':') && !addr.starts_with('[') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

/// Read frames until the response with `id` arrives (skipping broadcast
/// frames the server pushes to every client).
async fn read_response<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    id: u64,
) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(msg) = ws.next().await {
        let Message::Text(text) = msg? else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("response")
            && v.get("id").and_then(|i| i.as_u64()) == Some(id)
        {
            anyhow::ensure!(
                v.get("ok").and_then(|o| o.as_bool()) == Some(true),
                "request failed: {}",
                v.get("error").and_then(|e| e.as_str()).unwrap_or("?")
            );
            return Ok(v.get("data").cloned().unwrap_or(Value::Null));
        }
    }
    anyhow::bail!("socket closed before response {id}")
}

/// Non-loopback IPv4 addresses of this machine (hints only — identity is
/// always the machine id).
pub fn local_addresses() -> Vec<String> {
    match local_ip_address::list_afinet_netifas() {
        Ok(ifas) => ifas
            .into_iter()
            .filter(|(_, ip)| ip.is_ipv4() && !ip.is_loopback())
            .map(|(_, ip)| ip.to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::Peer;

    /// A real `PeerNet` over a throwaway data dir. Nothing is started, so no
    /// sockets, no mDNS: the queue behaviour under test is entirely local.
    fn peernet(label: &str) -> (std::path::PathBuf, Arc<PeerNet>) {
        let root = std::env::temp_dir().join(format!("threadknot-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(crate::store::Store::open_for_test(root.join("data")).unwrap());
        let hub = Hub::new(Arc::clone(&store), 42800);
        let registry = Arc::new(crate::peers::PeerRegistry::open(store.dir()).unwrap());
        registry
            .upsert(Peer {
                machine_id: "sleepy".into(),
                name: "Sleepy".into(),
                mesh_port: 42802,
                mesh_ca: "not-used-here".into(),
                added_at: crate::protocol::now_iso(),
                ..Peer::blank()
            })
            .unwrap();
        let device = Arc::new(crate::device::Device::load(store.dir(), "local-machine").unwrap());
        (root, PeerNet::new(registry, hub, device, 42800))
    }

    /// A peer that stops draining its request queue must fail the caller *fast*
    /// and legibly. Waiting out the 30-second per-request timeout would hold the
    /// phone that tapped a remote thread for half a minute and let every
    /// subsequent caller pile up behind it.
    #[tokio::test]
    async fn a_full_peer_queue_fails_fast_with_a_legible_error() {
        let (root, net) = peernet("peer-queue");

        // A link whose receiver nobody reads: exactly what a connected peer that
        // has stopped answering looks like from this side.
        let (tx, _rx) = mpsc::channel::<OutRequest>(crate::limits::PEER_REQUEST_QUEUE);
        for _ in 0..crate::limits::PEER_REQUEST_QUEUE {
            let (reply, _) = oneshot::channel();
            tx.try_send(OutRequest {
                kind: "hello".into(),
                payload: json!({}),
                on_behalf_of: None,
                reply,
            })
            .expect("the queue accepts up to its bound");
        }
        assert!(
            tx.try_send(OutRequest {
                kind: "hello".into(),
                payload: json!({}),
                on_behalf_of: None,
                reply: oneshot::channel().0,
            })
            .is_err(),
            "the queue is bounded, so the next one has nowhere to go"
        );
        net.links.lock().unwrap().insert("sleepy".into(), tx);

        let started = std::time::Instant::now();
        let error = net
            .request("sleepy", "hello", json!({}), None)
            .await
            .expect_err("a saturated peer queue must refuse");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "the refusal must not wait out the {PEER_REQUEST_TIMEOUT:?} request timeout (took {elapsed:?})"
        );
        let message = format!("{error:#}");
        assert!(message.contains("Sleepy"), "name the machine: {message}");
        assert!(
            message.contains("not responding"),
            "distinguish this from offline: {message}"
        );
        assert!(
            message.contains(&crate::limits::PEER_REQUEST_QUEUE.to_string()),
            "name the limit: {message}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The same peer with room in its queue is not refused — a bound that
    /// refuses a healthy machine is an outage, not a limit.
    #[tokio::test]
    async fn a_peer_with_queue_room_is_accepted_and_then_times_out_normally() {
        let (root, net) = peernet("peer-queue-room");
        let (tx, mut rx) = mpsc::channel::<OutRequest>(crate::limits::PEER_REQUEST_QUEUE);
        net.links.lock().unwrap().insert("sleepy".into(), tx);

        let call = tokio::spawn({
            let net = Arc::clone(&net);
            async move { net.request("sleepy", "hello", json!({}), None).await }
        });
        let queued = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the request reaches the link")
            .expect("and carries the kind");
        assert_eq!(queued.kind, "hello");
        // Dropping the reply sender is what a peer socket does when it closes.
        drop(queued);
        let error = call.await.unwrap().expect_err("no answer, no result");
        assert!(
            format!("{error:#}").contains("mid-request"),
            "a dropped reply is a disconnect, not a queue refusal"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}

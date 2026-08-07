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
use crate::peers::PeerRegistry;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, Notify};
use tokio_tungstenite::tungstenite::Message;

/// An RPC to forward down a peer's live socket; `reply` resolves with the
/// peer's response frame data.
struct OutRequest {
    kind: String,
    payload: Value,
    reply: oneshot::Sender<Result<Value>>,
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
    links: Mutex<HashMap<String, mpsc::UnboundedSender<OutRequest>>>,
    /// Wake the supervisor to reconcile tasks with the registry NOW
    /// (peer added/removed, fresh address hints).
    pub kick: Notify,
    /// Tells every connected peer task to re-send `peer.announce`.
    announce: broadcast::Sender<()>,
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
        })
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
    pub async fn request(&self, machine_id: &str, kind: &str, payload: Value) -> Result<Value> {
        let tx = self
            .links
            .lock()
            .unwrap()
            .get(machine_id)
            .cloned()
            .ok_or_else(|| {
                let name = self
                    .registry
                    .peer(machine_id)
                    .map(|p| p.name)
                    .unwrap_or_else(|| machine_id.to_string());
                anyhow::anyhow!("{name} is offline")
            })?;
        let (reply, rx) = oneshot::channel();
        tx.send(OutRequest {
            kind: kind.to_string(),
            payload,
            reply,
        })
        .map_err(|_| anyhow::anyhow!("peer disconnected"))?;
        tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .context("peer request timed out")?
            .map_err(|_| anyhow::anyhow!("peer disconnected mid-request"))?
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
        let url = format!(
            "ws://{}/ws?token={}",
            host_port(addr, peer.port),
            peer.token
        );
        let (mut ws, _) = tokio::time::timeout(
            Duration::from_secs(8),
            tokio_tungstenite::connect_async(&url),
        )
        .await
        .context("connect timeout")?
        .context("connect failed")?;

        // Verify identity BEFORE trusting the address: whatever answered
        // must report the machine id we paired with.
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
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutRequest>();
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
                        let frame = json!({ "id": id, "type": out.kind, "payload": out.payload });
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

/// Bidirectionally splice a client's `/term` WebSocket onto a peer machine.
pub async fn splice_term(
    client: axum::extract::ws::WebSocket,
    state: crate::server::ServerState,
    machine_id: String,
    params: HashMap<String, String>,
) {
    splice_ws(client, state, machine_id, params, "term").await
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
) {
    splice_ws(client, state, machine_id, params, "browser").await
}

/// Bidirectionally splice a client WebSocket onto the SAME endpoint on a peer
/// machine: the work runs there, the bytes stream through here. The peer's
/// master token is attached server-side; the client's own token never works on
/// the peer directly. Callers are responsible for deciding *who* may splice —
/// this function assumes that check already passed.
async fn splice_ws(
    client: axum::extract::ws::WebSocket,
    state: crate::server::ServerState,
    machine_id: String,
    params: HashMap<String, String>,
    path: &str,
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
    let query: String = params
        .iter()
        .filter(|(k, _)| k.as_str() != "token" && k.as_str() != "machineId")
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .chain(std::iter::once(format!("token={}", peer.token)))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("ws://{}/{path}?{query}", host_port(&addr, peer.port));
    let peer_ws = match tokio::time::timeout(
        Duration::from_secs(8),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        _ => {
            tracing::warn!("{path} splice: cannot reach {} at {addr}", peer.name);
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

//! Remote servers this machine is a **client of**.
//!
//! ```text
//!   your desktop ──wss──▶ relay ──▶ the team's shared box
//!        (client)                          (server)
//! ```
//!
//! Distinct from `peernet.rs`, and the distinction is the whole point of this
//! file. A **peer** is another machine of yours: the link is symmetric, both
//! sides dial, both push their entire workspace catalog on connect, and each
//! side's catalog already contains replicas it picked up from its own peers —
//! so pairing three people's laptops to one shared box merges all three
//! sidebars, transitively, through the box. That is correct for a fleet you own
//! and wrong for a machine you share with colleagues.
//!
//! A **server** is somebody's shared machine that you are a guest on. The link
//! here is asymmetric by construction:
//!
//! * **Only this side dials.** The server has no record of us, no credential
//!   for us, and no way to originate a request in this direction. It cannot ask
//!   us for our catalog because there is no channel on which to ask.
//! * **We send nothing but requests we originated.** There is no announce, no
//!   catalog push, no tombstone resync — the frames peernet sends on connect
//!   simply do not exist here.
//! * **Nothing we learn is written to `projects.json`.** Remote workspaces are
//!   held in memory, keyed by server, and rendered as their own sidebar
//!   section. Replicating them into the store the way a peer replica is stored
//!   would put them straight into the catalog our own peers receive on their
//!   next connect, which is the exact bleed this file exists to prevent.
//!
//! # Why a device credential, and why that is the good news
//!
//! We authenticate with a **device bearer token** — the same credential a
//! paired browser or phone holds — not a peer credential. That is not a
//! shortcut; the strict ingress refuses a peer credential outright on a remote
//! connection (see `ingress.rs`, "keeps the relay out of the mesh's trust path
//! entirely"), so over a relay this is the only door that opens.
//!
//! Three things fall out of it for free:
//!
//! * **Capability scoping.** We hold exactly the grants the server's owner
//!   picked when they minted the code. A desktop that was never granted
//!   `terminal` there cannot open one, and the check runs on their side.
//! * **Identity.** The credential is assignable to a person over there
//!   (`device.setPerson`), so threads we start on that machine are stamped with
//!   *us* rather than with their owner — which is what makes their sidebar's
//!   people row tell the truth about our work.
//! * **Revocation.** They revoke the device and we are out, immediately, with
//!   no cooperation needed from this end.
//!
//! # What this is not
//!
//! It is not isolation *on the server*. Everything in `people.rs`'s header
//! applies: agents there run as that machine's OS user, and a grant of `files`
//! or `terminal` reaches everything on it. This file controls what crosses the
//! link, not what a guest can see once across.

use crate::limits;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a routed request waits before giving up. Shorter than the peer
/// timeout: a peer is usually on the same LAN and a server is across the
/// internet, but a person staring at a spinner has the same patience either
/// way, and a relay that has gone quiet is not going to answer at second 45.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Reconnect backoff bounds. A laptop that closes its lid should not hammer
/// somebody's relay when it wakes on a network that is not up yet.
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// A remote Threadknot we hold a guest credential on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServer {
    /// Local record id. Not the remote's machine id: we may be handed a fresh
    /// credential for the same box, and the record should survive that.
    pub id: String,
    /// Friendly name, from the remote's `hello`. Refreshed on every connect.
    pub name: String,
    /// Origin we dial, e.g. `https://team.threadknot.ai`. Stored with the
    /// scheme so a plain-HTTP LAN server and a relay origin are both expressible
    /// and neither is inferred.
    pub origin: String,
    /// The remote's machine id, which is what `machineId`-routed requests name.
    /// Empty until the first successful `hello`.
    #[serde(default)]
    pub machine_id: String,
    /// Our device bearer over there. Never serialized to clients — it is a
    /// login, and the sidebar has no use for it.
    #[serde(skip_serializing, default)]
    pub credential: String,
    /// The device id the remote minted for us, so its owner can find the row to
    /// assign or revoke.
    #[serde(default)]
    pub device_id: String,
    /// Which person we act as over there, from `hello`. `None` on a server that
    /// has not assigned our credential to anyone, where we are its owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    /// That person's display name, so the UI can say who we are over there
    /// rather than only that we are somebody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_name: Option<String>,
    /// What we were granted there, from `hello`. Advisory on this side; the
    /// server enforces it regardless. Held so the UI can grey out what was
    /// never granted instead of offering it and failing.
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub added_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

impl RemoteServer {
    /// `wss://host/path` (or `ws://` for a plain-HTTP origin), for the socket.
    fn ws_url(&self) -> String {
        let base = self.origin.trim_end_matches('/');
        if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}/ws")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}/ws")
        } else {
            format!("wss://{base}/ws")
        }
    }

    pub fn public(&self, online: bool) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "origin": self.origin,
            "machineId": self.machine_id,
            "deviceId": self.device_id,
            "personId": self.person_id,
            "personName": self.person_name,
            "capabilities": self.capabilities,
            "addedAt": self.added_at,
            "lastSeenAt": self.last_seen_at,
            "online": online,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ServersFile {
    #[serde(default)]
    servers: Vec<DiskServer>,
}

/// `RemoteServer` skips the credential on serialize so client-facing lists
/// cannot leak it; the disk format carries it in a sibling field. Same shape
/// (and same reason) as `mobile.rs`'s `DiskDevice`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskServer {
    #[serde(flatten)]
    server: RemoteServer,
    credential: String,
}

pub struct ServerRegistry {
    path: PathBuf,
    servers: Mutex<Vec<RemoteServer>>,
}

impl ServerRegistry {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("servers.json");
        let servers = if path.exists() {
            let file: ServersFile =
                serde_json::from_str(&std::fs::read_to_string(&path).context("read servers.json")?)
                    .context("parse servers.json")?;
            file.servers
                .into_iter()
                .map(|d| RemoteServer {
                    credential: d.credential,
                    ..d.server
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            servers: Mutex::new(servers),
        })
    }

    fn flush(&self, servers: &[RemoteServer]) -> Result<()> {
        let file = ServersFile {
            servers: servers
                .iter()
                .map(|s| DiskServer {
                    credential: s.credential.clone(),
                    server: s.clone(),
                })
                .collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        // Bearer credentials for other people's machines: owner-only from the
        // first byte, like `server.json` and `peers.json`.
        crate::store::write_private(&tmp, &serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<RemoteServer> {
        self.servers.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<RemoteServer> {
        self.servers.lock().unwrap().iter().find(|s| s.id == id).cloned()
    }

    /// The record whose remote machine id is `machine_id`. This is the lookup
    /// `machineId` routing uses, which is why an unconnected record (empty
    /// machine id) can never match: `machine_id` is non-empty by the time any
    /// caller has an id to route by.
    pub fn by_machine(&self, machine_id: &str) -> Option<RemoteServer> {
        if machine_id.is_empty() {
            return None;
        }
        self.servers
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.machine_id == machine_id)
            .cloned()
    }

    pub fn add(&self, server: RemoteServer) -> Result<RemoteServer> {
        let mut servers = self.servers.lock().unwrap();
        anyhow::ensure!(
            servers.len() < limits::MAX_REMOTE_SERVERS,
            "at most {} remote servers",
            limits::MAX_REMOTE_SERVERS
        );
        servers.push(server.clone());
        self.flush(&servers)?;
        Ok(server)
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut RemoteServer)) -> Result<RemoteServer> {
        let mut servers = self.servers.lock().unwrap();
        let server = servers
            .iter_mut()
            .find(|s| s.id == id)
            .context("unknown server")?;
        f(server);
        let out = server.clone();
        self.flush(&servers)?;
        Ok(out)
    }

    pub fn remove(&self, id: &str) -> Result<RemoteServer> {
        let mut servers = self.servers.lock().unwrap();
        let i = servers.iter().position(|s| s.id == id).context("unknown server")?;
        let gone = servers.remove(i);
        self.flush(&servers)?;
        Ok(gone)
    }
}

struct OutRequest {
    kind: String,
    payload: Value,
    reply: oneshot::Sender<Result<Value>>,
}

/// Live outbound links, keyed by the REMOTE machine id so `machineId` routing
/// can find them with the same key it uses for peers.
pub struct ServerNet {
    pub registry: Arc<ServerRegistry>,
    hub: Arc<crate::agents::Hub>,
    links: Mutex<HashMap<String, mpsc::Sender<OutRequest>>>,
    online: Mutex<HashMap<String, bool>>,
    /// Wakes the supervisor when a server is added or removed, so a fresh
    /// record connects now rather than at the next poll.
    kick: Arc<tokio::sync::Notify>,
}

impl ServerNet {
    pub fn new(registry: Arc<ServerRegistry>, hub: Arc<crate::agents::Hub>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            hub,
            links: Mutex::new(HashMap::new()),
            online: Mutex::new(HashMap::new()),
            kick: Arc::new(tokio::sync::Notify::new()),
        })
    }

    pub fn is_online(&self, machine_id: &str) -> bool {
        *self.online.lock().unwrap().get(machine_id).unwrap_or(&false)
    }

    /// Whether `machine_id` names a remote server we hold a link record for.
    /// Checked BEFORE the peer table when routing, because a machine cannot be
    /// both and a server record is the more specific claim.
    pub fn knows(&self, machine_id: &str) -> bool {
        self.registry.by_machine(machine_id).is_some()
    }

    pub fn wake(&self) {
        self.kick.notify_waiters();
    }

    /// Send one request to a remote server and await its answer.
    ///
    /// There is no `on_behalf_of` parameter, and that is deliberate rather than
    /// an omission. Over a peer link the far side trusts us to describe our own
    /// caller because it minted us a credential as an equal. Here we are a
    /// guest: the server decides what our credential may do and who it speaks
    /// for, from its own records, on every request. Nothing we could assert
    /// would be believed, so nothing is asserted.
    pub async fn request(&self, machine_id: &str, kind: &str, payload: Value) -> Result<Value> {
        let tx = self
            .links
            .lock()
            .unwrap()
            .get(machine_id)
            .cloned()
            .ok_or_else(|| self.offline_error(machine_id))?;
        let (reply, rx) = oneshot::channel();
        tx.try_send(OutRequest {
            kind: kind.to_string(),
            payload,
            reply,
        })
        .map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => anyhow::anyhow!(
                "{} is not keeping up with requests — try again in a moment",
                self.name_of(machine_id)
            ),
            mpsc::error::TrySendError::Closed(_) => anyhow::anyhow!("disconnected"),
        })?;
        tokio::time::timeout(REQUEST_TIMEOUT, rx)
            .await
            .context("the server did not answer in time")?
            .map_err(|_| anyhow::anyhow!("disconnected mid-request"))?
    }

    fn name_of(&self, machine_id: &str) -> String {
        self.registry
            .by_machine(machine_id)
            .map(|s| s.name)
            .unwrap_or_else(|| "that server".into())
    }

    fn offline_error(&self, machine_id: &str) -> anyhow::Error {
        anyhow::anyhow!(
            "{} is not connected right now",
            self.name_of(machine_id)
        )
    }

    fn set_online(&self, machine_id: &str, online: bool) {
        let changed = {
            let mut map = self.online.lock().unwrap();
            map.insert(machine_id.to_string(), online) != Some(online)
        };
        if changed {
            self.hub.broadcast_state("servers", None);
        }
    }

    /// Fold a `hello` answer into the stored record.
    ///
    /// Everything it carries can change on the far side without our
    /// involvement — they reassign the credential to a different person, or
    /// narrow its grants — so this is the only place these fields are written
    /// and it runs on every hello rather than only the first.
    fn apply_hello(&self, server_id: &str, machine_id: &str, hello: &Value, origin: &str) {
        let name = hello
            .get("friendlyName")
            .or_else(|| hello.get("serverName"))
            .and_then(|v| v.as_str())
            .unwrap_or(origin)
            .to_string();
        let person_id = hello.get("person").and_then(|v| v.as_str()).map(String::from);
        let person_name = hello
            .get("personName")
            .and_then(|v| v.as_str())
            .map(String::from);
        let capabilities: Vec<String> = hello
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let changed = self
            .registry
            .get(server_id)
            .map(|s| {
                s.name != name
                    || s.person_id != person_id
                    || s.person_name != person_name
                    || s.capabilities != capabilities
            })
            .unwrap_or(true);
        let machine_id = machine_id.to_string();
        let _ = self.registry.update(server_id, |s| {
            s.machine_id = machine_id;
            s.name = name;
            s.person_id = person_id;
            s.person_name = person_name;
            s.capabilities = capabilities;
            s.last_seen_at = Some(crate::protocol::now_iso());
        });
        if changed {
            self.hub.broadcast_state("servers", None);
        }
    }

    /// Re-read who we are and what we may do over there, now.
    ///
    /// The connect-time hello goes stale the moment the far side's owner
    /// assigns our credential to somebody or changes its grants, and a link
    /// that stays up for days would go on reporting the old answer. Cheap
    /// enough to run alongside a catalog fetch, which is what the sidebar does
    /// on every refresh.
    pub async fn refresh_identity(&self, machine_id: &str) -> Result<()> {
        let Some(server) = self.registry.by_machine(machine_id) else {
            return Ok(());
        };
        let hello = self.request(machine_id, "hello", json!({})).await?;
        self.apply_hello(&server.id, machine_id, &hello, &server.origin);
        Ok(())
    }

    /// One supervisor task per record, restarted as records come and go.
    pub fn spawn_supervisor(self: &Arc<Self>) {
        let net = Arc::clone(self);
        tokio::spawn(async move {
            let mut running: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
            loop {
                let wanted: Vec<RemoteServer> = net.registry.list();
                // Start a holder for anything new.
                for server in &wanted {
                    if running.get(&server.id).is_some_and(|h| !h.is_finished()) {
                        continue;
                    }
                    let net2 = Arc::clone(&net);
                    let id = server.id.clone();
                    running.insert(
                        server.id.clone(),
                        tokio::spawn(async move { net2.hold_forever(&id).await }),
                    );
                }
                // Stop holders for records that are gone.
                let live: std::collections::HashSet<&str> =
                    wanted.iter().map(|s| s.id.as_str()).collect();
                running.retain(|id, handle| {
                    if live.contains(id.as_str()) {
                        true
                    } else {
                        handle.abort();
                        false
                    }
                });
                tokio::select! {
                    _ = net.kick.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
            }
        });
    }

    /// Reconnect loop for one record, with backoff.
    async fn hold_forever(self: Arc<Self>, server_id: &str) {
        let mut backoff = RECONNECT_MIN;
        loop {
            let Some(server) = self.registry.get(server_id) else {
                return; // removed while we were sleeping
            };
            match self.connect_and_hold(&server).await {
                Ok(()) => backoff = RECONNECT_MIN,
                Err(e) => {
                    tracing::debug!("server {} link ended: {e:#}", server.name);
                    backoff = (backoff * 2).min(RECONNECT_MAX);
                }
            }
            if !server.machine_id.is_empty() {
                self.set_online(&server.machine_id, false);
                self.links.lock().unwrap().remove(&server.machine_id);
            }
            tokio::time::sleep(backoff).await;
        }
    }

    /// Dial, identify, then pump requests until the socket dies.
    async fn connect_and_hold(&self, server: &RemoteServer) -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let request = ws_request(server)?;
        let (mut ws, _) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async(request),
        )
        .await
        .context("connect timed out")?
        .context("the server refused our credential")?;

        // `hello` first: it tells us the machine id to route by, who we are
        // over there, and what we were granted. All three can change without
        // our involvement (they reassign the device, they narrow its grants),
        // so this is re-read on every connect rather than trusted from disk.
        ws.send(Message::Text(
            json!({ "id": 1, "type": "hello", "payload": {} }).to_string().into(),
        ))
        .await?;
        let hello = tokio::time::timeout(CONNECT_TIMEOUT, read_response(&mut ws, 1))
            .await
            .context("hello timed out")??;

        let machine_id = hello
            .get("machineId")
            .or_else(|| hello.get("serverId"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        anyhow::ensure!(!machine_id.is_empty(), "the server did not identify itself");
        anyhow::ensure!(
            hello.get("principal").and_then(|v| v.as_str()) != Some("master"),
            "that credential is the server's master token — pair a device credential instead, \
             so the server's owner can scope and revoke it"
        );

        self.apply_hello(&server.id, &machine_id, &hello, &server.origin);

        let (out_tx, mut out_rx) = mpsc::channel::<OutRequest>(limits::PEER_REQUEST_QUEUE);
        self.links
            .lock()
            .unwrap()
            .insert(machine_id.clone(), out_tx);
        self.set_online(&machine_id, true);
        self.hub.broadcast_state("servers", None);
        tracing::info!("server online: {} ({})", server.origin, machine_id);

        let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
        let mut req_id: u64 = 2;
        let result: Result<()> = async {
            loop {
                tokio::select! {
                    outgoing = out_rx.recv() => {
                        let Some(req) = outgoing else { return Ok(()) };
                        let id = req_id;
                        req_id += 1;
                        let frame = json!({
                            "id": id,
                            "type": req.kind,
                            "payload": req.payload,
                        });
                        pending.insert(id, req.reply);
                        ws.send(Message::Text(frame.to_string().into())).await?;
                    }
                    incoming = ws.next() => {
                        let Some(msg) = incoming else { return Ok(()) };
                        match msg? {
                            Message::Text(text) => {
                                self.handle_frame(&machine_id, &text, &mut pending)
                            }
                            Message::Close(_) => return Ok(()),
                            _ => {}
                        }
                    }
                }
            }
        }
        .await;

        // Fail everything still waiting rather than let it time out one by one.
        for (_, reply) in pending.drain() {
            let _ = reply.send(Err(anyhow::anyhow!("the server disconnected")));
        }
        result
    }

    /// One inbound frame. Responses resolve their waiting request; the
    /// server's own event and state broadcasts are relayed to our clients with
    /// an origin tag, exactly as a peer's are, so an open remote thread streams
    /// live instead of only refreshing on request.
    fn handle_frame(
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
                    return;
                };
                let result = if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
                    Ok(v.get("data").cloned().unwrap_or(Value::Null))
                } else {
                    Err(anyhow::anyhow!(
                        "{}",
                        v.get("error").and_then(|e| e.as_str()).unwrap_or("server error")
                    ))
                };
                let _ = reply.send(result);
            }
            Some("event") | Some("state.changed") => {
                // A frame that already carries an origin reached that server
                // from a third machine. Relaying it here would put a machine we
                // have no relationship with into our clients' event stream, and
                // it is the mirror of the catalog rule: what belongs to this
                // server crosses, what merely passed through it does not.
                if v.get("origin").is_none() {
                    v["origin"] = json!(machine_id);
                    let _ = self.hub.relay.send(v.to_string());
                }
            }
            _ => {}
        }
    }
}

/// The WebSocket handshake, with the device bearer in a header.
///
/// A header rather than `?token=`: the strict ingress refuses a credential in
/// the URL outright, and it is right to — a URL ends up in logs, `Referer`
/// headers and crash reports. The relay sees the request line either way.
fn ws_request(server: &RemoteServer) -> Result<axum::http::Request<()>> {
    let url = server.ws_url();
    let authority = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .context("malformed origin")?
        .to_string();
    let builder = axum::http::Request::builder()
        .uri(&url)
        .method("GET")
        .header("host", &authority)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header(
            "sec-websocket-key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("authorization", format!("Bearer {}", server.credential));
    builder.body(()).context("build handshake")
}

/// Read frames until the response to `want_id` arrives.
async fn read_response<S>(ws: &mut S, want_id: u64) -> Result<Value>
where
    S: futures_util::Stream<
            Item = std::result::Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    while let Some(msg) = ws.next().await {
        let Message::Text(text) = msg? else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("response") {
            continue;
        }
        if v.get("id").and_then(|i| i.as_u64()) != Some(want_id) {
            continue;
        }
        return if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
            Ok(v.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(anyhow::anyhow!(
                "{}",
                v.get("error").and_then(|e| e.as_str()).unwrap_or("request failed")
            ))
        };
    }
    anyhow::bail!("the socket closed before answering")
}

/// Redeem a pairing code (or a master token, on a LAN server) at `origin` and
/// come back with a device credential.
///
/// This is the same `POST /api/mobile/pair` a phone uses. Nothing about the
/// desktop makes it a different kind of client here, which is the point: the
/// server's owner sees one more row under paired devices, assigns it to a
/// person, scopes its grants, and revokes it the same way.
pub async fn redeem(origin: &str, pairing_code: Option<&str>, token: Option<&str>, device_name: &str) -> Result<(String, String)> {
    let base = origin.trim_end_matches('/');
    anyhow::ensure!(
        base.starts_with("http://") || base.starts_with("https://"),
        "the address must start with http:// or https://"
    );
    let mut body = json!({ "deviceName": device_name, "platform": "desktop" });
    match (pairing_code, token) {
        (Some(code), _) => body["pairingCode"] = json!(code),
        (None, Some(t)) => body["token"] = json!(t),
        (None, None) => anyhow::bail!("a pairing code is required"),
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let res = client
        .post(format!("{base}/api/mobile/pair"))
        .json(&body)
        .send()
        .await
        .context("could not reach that address")?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", text.trim());
    let parsed: Value = serde_json::from_str(&text).context("the server sent an unexpected reply")?;
    let credential = parsed
        .get("credential")
        .and_then(|v| v.as_str())
        .context("the server did not return a credential")?
        .to_string();
    let device_id = parsed
        .get("deviceId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok((credential, device_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(origin: &str) -> RemoteServer {
        RemoteServer {
            id: "s1".into(),
            name: "Team box".into(),
            origin: origin.into(),
            machine_id: "m1".into(),
            credential: "amd_secret".into(),
            device_id: "d1".into(),
            person_id: None,
            person_name: None,
            capabilities: vec![],
            added_at: crate::protocol::now_iso(),
            last_seen_at: None,
        }
    }

    #[test]
    fn https_origins_dial_wss_and_http_dials_ws() {
        assert_eq!(server("https://team.example.com").ws_url(), "wss://team.example.com/ws");
        assert_eq!(server("http://192.168.1.5:42800").ws_url(), "ws://192.168.1.5:42800/ws");
        // A trailing slash is what someone pastes out of a browser bar.
        assert_eq!(server("https://team.example.com/").ws_url(), "wss://team.example.com/ws");
    }

    #[test]
    fn the_credential_rides_a_header_and_never_the_url() {
        let req = ws_request(&server("https://team.example.com")).unwrap();
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer amd_secret"
        );
        // The strict ingress refuses a credential in the query outright, so a
        // regression that put it there would fail at the far end — but it would
        // also have been logged by every hop on the way. Catch it here.
        assert!(!req.uri().to_string().contains("amd_secret"));
        assert!(req.uri().to_string().starts_with("wss://"));
    }

    #[test]
    fn the_credential_never_reaches_a_client_facing_list() {
        let json = serde_json::to_string(&server("https://team.example.com")).unwrap();
        assert!(!json.contains("amd_secret"), "credential leaked: {json}");
    }

    #[test]
    fn records_round_trip_through_disk_with_their_credentials() {
        let dir = std::env::temp_dir().join(format!("threadknot-servers-{}", crate::protocol::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = ServerRegistry::open(&dir).unwrap();
        registry.add(server("https://team.example.com")).unwrap();

        let reopened = ServerRegistry::open(&dir).unwrap();
        let back = reopened.get("s1").unwrap();
        assert_eq!(back.credential, "amd_secret");
        assert_eq!(back.origin, "https://team.example.com");
        // Routing finds it by the REMOTE machine id, and never by an empty one.
        assert!(reopened.by_machine("m1").is_some());
        assert!(reopened.by_machine("").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unconnected_record_is_not_routable() {
        let dir = std::env::temp_dir().join(format!("threadknot-servers-{}", crate::protocol::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = ServerRegistry::open(&dir).unwrap();
        let mut fresh = server("https://team.example.com");
        fresh.machine_id = String::new();
        registry.add(fresh).unwrap();
        // Before the first hello there is no id to route by, and an empty
        // lookup must not match it.
        assert!(registry.by_machine("").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}

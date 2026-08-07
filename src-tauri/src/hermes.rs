//! Registered remote Hermes Agent gateways (Nous Research hermes-agent).
//!
//! Each entry is one per-profile API server: a base URL like
//! `http://host:8651/v1` plus its bearer key. Persisted atomically to
//! `~/.threadknot/hermes.json`. The API key is needed in plaintext for outbound
//! calls, so it lives on disk (0600-style trust, same as server.json's token)
//! but is never serialized into client-facing lists.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

use crate::protocol::{now_iso, HermesAgentStatus, HermesStatusSnapshot, ServerMessage};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesAgent {
    pub id: String,
    /// Display name — defaults to the gateway's advertised model (profile) name.
    pub name: String,
    /// Small square data URL used as this agent's sidebar profile picture.
    /// Serialized under BOTH `image` (legacy) and `avatar` (current wire
    /// name); `set_image` keeps the two in lockstep and `open` mirrors
    /// whichever one an older file carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Gateway base URL including the /v1 suffix trimmed off (we store the
    /// origin, e.g. `http://192.168.0.97:8651`).
    pub base_url: String,
    /// Bearer key for this gateway. Never sent to clients.
    #[serde(skip_serializing, default)]
    pub api_key: String,
    /// Model id the gateway advertises (the Hermes profile name).
    pub model: String,
    pub created_at: String,
}

/// Disk format carries the key in a sibling field because `HermesAgent`
/// skips it on serialize (client lists must not leak it) — same pattern as
/// `mobile.rs`'s `DiskDevice`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskAgent {
    #[serde(flatten)]
    agent: HermesAgent,
    api_key: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HermesFile {
    #[serde(default)]
    agents: Vec<DiskAgent>,
}

pub struct HermesRegistry {
    path: PathBuf,
    agents: Mutex<Vec<HermesAgent>>,
}

impl HermesRegistry {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("hermes.json");
        let agents = if path.exists() {
            let file: HermesFile = serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse hermes.json")?;
            file.agents
                .into_iter()
                .map(|d| {
                    let mut agent = HermesAgent {
                        api_key: d.api_key,
                        ..d.agent
                    };
                    // Older files carry only `image`; mirror so the wire
                    // shape always exposes `avatar` too.
                    if agent.avatar.is_none() {
                        agent.avatar = agent.image.clone();
                    } else if agent.image.is_none() {
                        agent.image = agent.avatar.clone();
                    }
                    agent
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            agents: Mutex::new(agents),
        })
    }

    fn flush(&self, agents: &[HermesAgent]) -> Result<()> {
        let file = HermesFile {
            agents: agents
                .iter()
                .map(|a| DiskAgent {
                    api_key: a.api_key.clone(),
                    agent: a.clone(),
                })
                .collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn add(&self, name: String, base_url: String, api_key: String, model: String) -> Result<HermesAgent> {
        let agent = HermesAgent {
            id: crate::protocol::new_id(),
            name,
            image: None,
            avatar: None,
            base_url,
            api_key,
            model,
            created_at: crate::protocol::now_iso(),
        };
        let mut agents = self.agents.lock().unwrap();
        anyhow::ensure!(
            !agents.iter().any(|a| a.base_url == agent.base_url),
            "a Hermes agent with this URL is already registered"
        );
        agents.push(agent.clone());
        self.flush(&agents)?;
        Ok(agent)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut agents = self.agents.lock().unwrap();
        let before = agents.len();
        agents.retain(|a| a.id != id);
        anyhow::ensure!(agents.len() < before, "unknown Hermes agent");
        self.flush(&agents)
    }

    pub fn set_image(&self, id: &str, image: Option<String>) -> Result<HermesAgent> {
        let mut agents = self.agents.lock().unwrap();
        let agent = agents
            .iter_mut()
            .find(|a| a.id == id)
            .context("unknown Hermes agent")?;
        agent.image = image.clone();
        agent.avatar = image;
        let out = agent.clone();
        self.flush(&agents)?;
        Ok(out)
    }

    pub fn list(&self) -> Vec<HermesAgent> {
        self.agents.lock().unwrap().clone()
    }

    pub fn agent(&self, id: &str) -> Option<HermesAgent> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }
}

/// Normalize a pasted gateway URL to an origin: strips a trailing `/v1` (the
/// form Hermes docs hand out for OpenAI-compatible clients) and any trailing
/// slash, so stored URLs join cleanly with endpoint paths.
pub fn normalize_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let url = url::Url::parse(trimmed).context("invalid URL")?;
    anyhow::ensure!(
        url.scheme() == "http" || url.scheme() == "https",
        "URL must be http(s)"
    );
    Ok(trimmed.to_string())
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .build()
        .expect("reqwest client")
}

async fn get_json(base: &str, key: &str, path: &str, timeout: Duration) -> Result<Value> {
    let resp = http_client()
        .get(format!("{base}{path}"))
        .bearer_auth(key)
        .timeout(timeout)
        .send()
        .await
        .with_context(|| format!("GET {path} failed"))?;
    anyhow::ensure!(
        resp.status().is_success(),
        "GET {path} returned {}",
        resp.status()
    );
    resp.json().await.with_context(|| format!("parse {path}"))
}

/// Validate a gateway: liveness + auth + advertised model (profile) name.
pub async fn probe(base: &str, key: &str) -> Result<(String, String)> {
    let health = get_json(base, key, "/health", Duration::from_secs(8)).await?;
    let version = health
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let models = get_json(base, key, "/v1/models", Duration::from_secs(8)).await?;
    let model = models
        .pointer("/data/0/id")
        .and_then(|v| v.as_str())
        .context("gateway advertised no model — is the API server enabled?")?
        .to_string();
    Ok((model, version))
}

/// Live agent detail for the Settings view: health, skills, toolsets (which
/// includes MCP-mounted toolsets — that's how hermes surfaces MCP servers).
pub async fn details(base: &str, key: &str) -> Result<Value> {
    let timeout = Duration::from_secs(12);
    let health = get_json(base, key, "/health", timeout).await?;
    let skills = get_json(base, key, "/v1/skills", timeout)
        .await
        .unwrap_or_else(|_| json!({ "data": [] }));
    let toolsets = get_json(base, key, "/v1/toolsets", timeout)
        .await
        .unwrap_or_else(|_| json!({ "data": [] }));
    Ok(json!({
        "health": {
            "ok": health.get("status").and_then(|v| v.as_str()) == Some("ok"),
            "version": health.get("version").and_then(|v| v.as_str()),
        },
        "skills": skills.get("data").cloned().unwrap_or(json!([])),
        "toolsets": toolsets.get("data").cloned().unwrap_or(json!([])),
    }))
}

// ---- Live presence poller ---------------------------------------------------

/// How often every registered gateway is re-probed for its Online/Offline dot.
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(20);
/// Short per-probe timeout so a hung gateway drops to Offline quickly (and,
/// because probes run concurrently, never delays the others).
const STATUS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The committed presence map plus its revision, guarded together so every
/// read (snapshot) and write (poll commit) sees a consistent (revision, map)
/// pair. `revision` bumps on every commit and only ever increases within a
/// server process, letting a client order two frames that raced in delivery.
#[derive(Default)]
struct StatusInner {
    data: HashMap<String, HermesAgentStatus>,
    revision: u64,
}

impl StatusInner {
    /// Snapshot the current map (ordered by agent id for a stable wire shape)
    /// tagged with the current revision. Caller holds the lock.
    fn snapshot(&self) -> HermesStatusSnapshot {
        let mut statuses: Vec<HermesAgentStatus> = self.data.values().cloned().collect();
        statuses.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        HermesStatusSnapshot {
            revision: self.revision,
            statuses,
        }
    }
}

/// Server-held presence map for the registered Hermes gateways, keyed by agent
/// id. Populated by [`spawn_status_poller`]; the snapshot answers
/// `hermes.agent.statuses` and rides the `hermes.statuses` broadcast.
#[derive(Default)]
pub struct HermesStatusState {
    inner: Mutex<StatusInner>,
    /// Notified when the registry changes (agent added/removed) so the poller
    /// re-probes immediately instead of leaving the change unseen for 20s.
    kick: Notify,
}

impl HermesStatusState {
    /// Current statuses + revision, ordered by agent id for a stable wire shape.
    pub fn snapshot(&self) -> HermesStatusSnapshot {
        self.inner.lock().unwrap().snapshot()
    }

    /// Ask the poller to re-probe now (a gateway was added or removed).
    pub fn kick(&self) {
        self.kick.notify_one();
    }
}

/// A single `/health` probe for the presence poller: returns the advertised
/// version when the gateway answers `status == "ok"`, otherwise an error which
/// the caller treats as Offline.
async fn health_version(base: &str, key: &str, timeout: Duration) -> Result<Option<String>> {
    let health = get_json(base, key, "/health", timeout).await?;
    anyhow::ensure!(
        health.get("status").and_then(|v| v.as_str()) == Some("ok"),
        "gateway health not ok"
    );
    Ok(health
        .get("version")
        .and_then(|v| v.as_str())
        .map(String::from))
}

/// Background task: every [`STATUS_POLL_INTERVAL`] (and on any registry kick),
/// probe every registered gateway concurrently and broadcast when a gateway's
/// online state changes or the set of registered agents changes. All probe
/// errors are Offline; nothing here can crash the server.
pub fn spawn_status_poller(hub: Arc<crate::agents::Hub>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            poll_statuses_once(&hub).await;
            tokio::select! {
                _ = tokio::time::sleep(STATUS_POLL_INTERVAL) => {}
                _ = hub.hermes_status.kick.notified() => {}
            }
        }
    })
}

async fn poll_statuses_once(hub: &crate::agents::Hub) {
    // Probe all agents concurrently so one hung gateway never delays the rest.
    let mut set = tokio::task::JoinSet::new();
    for agent in hub.hermes.list() {
        set.spawn(async move {
            let started = Instant::now();
            // `since_at` is provisional here (this probe has no view of the
            // previous state); the commit below preserves the real value for
            // states that did not flip.
            let checked = now_iso();
            match health_version(&agent.base_url, &agent.api_key, STATUS_PROBE_TIMEOUT).await {
                Ok(version) => HermesAgentStatus {
                    agent_id: agent.id,
                    online: true,
                    last_checked_at: checked.clone(),
                    since_at: checked,
                    latency_ms: Some(started.elapsed().as_millis() as u64),
                    version,
                },
                Err(err) => {
                    tracing::debug!("hermes health probe for {} failed: {err:#}", agent.id);
                    HermesAgentStatus {
                        agent_id: agent.id,
                        online: false,
                        last_checked_at: checked.clone(),
                        since_at: checked,
                        latency_ms: None,
                        version: None,
                    }
                }
            }
        });
    }

    // Collect the finished probes (a JoinSet panic is simply skipped).
    let mut fresh: HashMap<String, HermesAgentStatus> = HashMap::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(status) = joined {
            fresh.insert(status.agent_id.clone(), status);
        }
    }

    // Commit under one consistent view: intersect against the CURRENT registry
    // (so an agent removed mid-probe is dropped, never resurrected), carry each
    // state's `since_at` forward across polls that did not flip it, swap in the
    // fresh map, and bump the revision. Then decide whether anything a client
    // cares about changed — the set of agents, or any one flipping
    // online<->offline. Latency-only jitter does NOT rebroadcast.
    let broadcast = {
        let signature = |m: &HashMap<String, HermesAgentStatus>| {
            let mut v: Vec<(String, bool)> =
                m.iter().map(|(id, s)| (id.clone(), s.online)).collect();
            v.sort();
            v
        };
        let mut inner = hub.hermes_status.inner.lock().unwrap();
        // Re-read the registered ids under the commit lock and drop results for
        // gateways no longer present (Finding 2).
        let registered: std::collections::HashSet<String> =
            hub.hermes.list().into_iter().map(|a| a.id).collect();
        fresh.retain(|id, _| registered.contains(id));
        // Preserve `since_at` for states that did not flip; otherwise it is the
        // moment this new state was entered (Finding 4).
        for status in fresh.values_mut() {
            if let Some(prev) = inner.data.get(&status.agent_id) {
                if prev.online == status.online {
                    status.since_at = prev.since_at.clone();
                }
            }
        }
        let before = signature(&inner.data);
        inner.data = fresh;
        inner.revision += 1;
        let changed = signature(&inner.data) != before;
        changed.then(|| inner.snapshot())
    };
    if let Some(snapshot) = broadcast {
        let _ = hub.broadcast.send(ServerMessage::HermesStatuses {
            revision: snapshot.revision,
            statuses: snapshot.statuses,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_list_remove_roundtrip_and_key_never_leaks() {
        let dir = std::env::temp_dir().join(format!("threadknot-hermes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = HermesRegistry::open(&dir).unwrap();
        let a = reg
            .add(
                "chip".into(),
                "http://192.168.0.97:8651".into(),
                "chip_secret".into(),
                "chip".into(),
            )
            .unwrap();

        // Duplicate URL is rejected.
        assert!(reg
            .add("x".into(), "http://192.168.0.97:8651".into(), "k".into(), "x".into())
            .is_err());

        // Client-facing serialization never carries the key…
        let listed = serde_json::to_string(&reg.list()).unwrap();
        assert!(!listed.contains("chip_secret"));
        // …but the registry (and a reload from disk) still has it.
        assert_eq!(reg.agent(&a.id).unwrap().api_key, "chip_secret");
        let image = "data:image/webp;base64,aW1hZ2U=".to_string();
        let updated = reg.set_image(&a.id, Some(image.clone())).unwrap();
        assert_eq!(updated.image.as_deref(), Some(image.as_str()));
        // `avatar` is the same picture under its current wire name.
        assert_eq!(updated.avatar.as_deref(), Some(image.as_str()));
        let reloaded = HermesRegistry::open(&dir).unwrap();
        assert_eq!(reloaded.agent(&a.id).unwrap().api_key, "chip_secret");
        assert_eq!(reloaded.agent(&a.id).unwrap().image, Some(image.clone()));
        assert_eq!(reloaded.agent(&a.id).unwrap().avatar, Some(image));

        reg.remove(&a.id).unwrap();
        assert!(reg.list().is_empty());
        assert!(reg.remove(&a.id).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn normalizes_pasted_urls() {
        assert_eq!(
            normalize_base_url("http://192.168.0.97:8651/v1").unwrap(),
            "http://192.168.0.97:8651"
        );
        assert_eq!(
            normalize_base_url("http://192.168.0.97:8651/").unwrap(),
            "http://192.168.0.97:8651"
        );
        assert!(normalize_base_url("ftp://x").is_err());
        assert!(normalize_base_url("not a url").is_err());
    }
}

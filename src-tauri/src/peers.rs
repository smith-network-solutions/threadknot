//! Registered peer Threadknot machines (the mesh registry).
//!
//! Same pattern as `hermes.rs`: entries persist atomically to
//! `~/.threadknot/peers.json` with the peer's master token in a disk-only field
//! that is never serialized into client-facing lists. Identity is the
//! machine id; addresses are disposable hints kept fresh by mDNS discovery
//! and `peer.announce` — a reconnect ALWAYS re-verifies the machine id
//! before trusting whatever answered at an address.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    /// The peer's stable identity (its server_id). THE key — addresses and
    /// ports may change under DHCP, this never does.
    pub machine_id: String,
    /// Friendly name as advertised at pairing (peer may rename later).
    pub name: String,
    /// Profile picture (data URL) as advertised by the peer at pairing/
    /// announce/hello. Display-only, refreshed like the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Accent color as advertised by the peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Local override for the peer's avatar. Never sent to the peer; wins
    /// over the advertised value in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_override: Option<String>,
    /// Local override for the peer's accent color. Never sent to the peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_override: Option<String>,
    /// ISO-8601 timestamp the peer stamped on its advertised profile. The
    /// last-write-wins clock: an incoming name/avatar/color is only applied
    /// when its timestamp is newer than (or equal to) this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_updated_at: Option<String>,
    /// The peer's master token. Never sent to clients.
    #[serde(skip_serializing, default)]
    pub token: String,
    pub port: u16,
    /// Candidate addresses, best first. Refreshed by announce/discovery.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// Last address a verified connection succeeded on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_good_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    pub added_at: String,
    pub mesh_version: u32,
}

/// Disk format carries the token in a sibling field because `Peer` skips it
/// on serialize — same pattern as `hermes.rs`'s `DiskAgent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskPeer {
    #[serde(flatten)]
    peer: Peer,
    token: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PeersFile {
    #[serde(default)]
    peers: Vec<DiskPeer>,
}

pub struct PeerRegistry {
    path: PathBuf,
    peers: Mutex<Vec<Peer>>,
}

impl PeerRegistry {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("peers.json");
        let peers = if path.exists() {
            let file: PeersFile = serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse peers.json")?;
            file.peers
                .into_iter()
                .map(|d| Peer {
                    token: d.token,
                    ..d.peer
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            peers: Mutex::new(peers),
        })
    }

    fn flush(&self, peers: &[Peer]) -> Result<()> {
        let file = PeersFile {
            peers: peers
                .iter()
                .map(|p| DiskPeer {
                    token: p.token.clone(),
                    peer: p.clone(),
                })
                .collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        // Master tokens at rest — owner-only, like a key file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Insert or refresh a peer (pairing is idempotent — re-pairing the same
    /// machine updates its token/name/addresses instead of duplicating).
    pub fn upsert(&self, peer: Peer) -> Result<Peer> {
        let mut peers = self.peers.lock().unwrap();
        if let Some(existing) = peers.iter_mut().find(|p| p.machine_id == peer.machine_id) {
            existing.name = peer.name;
            // Advertised appearance refreshes with the pairing; the LOCAL
            // overrides are ours and survive a re-pair untouched.
            existing.avatar = peer.avatar;
            existing.color = peer.color;
            existing.token = peer.token;
            existing.port = peer.port;
            for a in peer.addresses {
                if !existing.addresses.contains(&a) {
                    existing.addresses.push(a);
                }
            }
            existing.mesh_version = peer.mesh_version;
            let out = existing.clone();
            self.flush(&peers)?;
            return Ok(out);
        }
        peers.push(peer.clone());
        self.flush(&peers)?;
        Ok(peer)
    }

    pub fn remove(&self, machine_id: &str) -> Result<()> {
        let mut peers = self.peers.lock().unwrap();
        let before = peers.len();
        peers.retain(|p| p.machine_id != machine_id);
        anyhow::ensure!(peers.len() < before, "unknown peer");
        self.flush(&peers)
    }

    pub fn list(&self) -> Vec<Peer> {
        self.peers.lock().unwrap().clone()
    }

    pub fn peer(&self, machine_id: &str) -> Option<Peer> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.machine_id == machine_id)
            .cloned()
    }

    /// Refresh a peer's address hints (announce/mDNS/successful connect).
    /// New addresses go to the FRONT so the freshest hint is tried first;
    /// `last_good` also stamps `last_seen_at`.
    pub fn note_addresses(
        &self,
        machine_id: &str,
        addresses: &[String],
        port: Option<u16>,
        last_good: Option<&str>,
    ) -> Result<bool> {
        let mut peers = self.peers.lock().unwrap();
        let Some(peer) = peers.iter_mut().find(|p| p.machine_id == machine_id) else {
            return Ok(false);
        };
        let mut changed = false;
        for a in addresses.iter().rev() {
            if let Some(pos) = peer.addresses.iter().position(|x| x == a) {
                if pos != 0 {
                    let a = peer.addresses.remove(pos);
                    peer.addresses.insert(0, a);
                    changed = true;
                }
            } else {
                peer.addresses.insert(0, a.clone());
                changed = true;
            }
        }
        if let Some(p) = port {
            if peer.port != p {
                peer.port = p;
                changed = true;
            }
        }
        if let Some(good) = last_good {
            peer.last_good_address = Some(good.to_string());
            peer.last_seen_at = Some(crate::protocol::now_iso());
            changed = true;
        }
        if changed {
            self.flush(&peers)?;
        }
        Ok(changed)
    }

    /// Store the appearance a peer advertised about itself (pairing, hello,
    /// announce). Outer `None` means the peer's frame omitted the field (an
    /// older Threadknot) and leaves the stored value alone; `Some(None)` is an
    /// explicit clear. Returns whether anything changed.
    pub fn note_appearance(
        &self,
        machine_id: &str,
        avatar: Option<Option<String>>,
        color: Option<Option<String>>,
    ) -> Result<bool> {
        let mut peers = self.peers.lock().unwrap();
        let Some(peer) = peers.iter_mut().find(|p| p.machine_id == machine_id) else {
            return Ok(false);
        };
        let mut changed = false;
        if let Some(a) = avatar {
            if peer.avatar != a {
                peer.avatar = a;
                changed = true;
            }
        }
        if let Some(c) = color {
            if peer.color != c {
                peer.color = c;
                changed = true;
            }
        }
        if changed {
            self.flush(&peers)?;
        }
        Ok(changed)
    }

    /// Merge a peer's advertised REAL profile (name/avatar/color) under
    /// last-write-wins. Each argument uses the same patch semantics as
    /// `note_appearance`: outer `None` means the frame omitted the field, so
    /// leave the stored value alone; `Some(None)` is an explicit clear.
    ///
    /// The LWW-guarded fields (name/avatar/color) are only applied when the
    /// incoming `updated_at` is >= the stored `profile_updated_at` (ISO-8601
    /// strings compare correctly lexicographically). If the stored timestamp
    /// is `None`, the incoming profile always wins. If the incoming
    /// `updated_at` is `None`, treat it as older and skip those fields.
    /// Returns whether anything changed.
    pub fn note_profile(
        &self,
        machine_id: &str,
        name: Option<String>,
        avatar: Option<Option<String>>,
        color: Option<Option<String>>,
        updated_at: Option<String>,
    ) -> Result<bool> {
        let mut peers = self.peers.lock().unwrap();
        let Some(peer) = peers.iter_mut().find(|p| p.machine_id == machine_id) else {
            return Ok(false);
        };
        // Decide whether the incoming profile is fresh enough to win.
        let apply = match (&updated_at, &peer.profile_updated_at) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(incoming), Some(stored)) => incoming.as_str() >= stored.as_str(),
        };
        let mut changed = false;
        if apply {
            if let Some(n) = name {
                if peer.name != n {
                    peer.name = n;
                    changed = true;
                }
            }
            if let Some(a) = avatar {
                if peer.avatar != a {
                    peer.avatar = a;
                    changed = true;
                }
            }
            if let Some(c) = color {
                if peer.color != c {
                    peer.color = c;
                    changed = true;
                }
            }
            // Advance the clock even if the concrete values matched, so a
            // later stale frame can still be rejected.
            if let Some(ts) = updated_at {
                if peer.profile_updated_at.as_deref() != Some(ts.as_str()) {
                    peer.profile_updated_at = Some(ts);
                    changed = true;
                }
            }
        }
        if changed {
            self.flush(&peers)?;
        }
        Ok(changed)
    }

    /// Patch the LOCAL appearance overrides for a peer (display-only, never
    /// sent to the peer). Same patch semantics as `note_appearance`.
    pub fn set_overrides(
        &self,
        machine_id: &str,
        avatar: Option<Option<String>>,
        color: Option<Option<String>>,
    ) -> Result<Peer> {
        let mut peers = self.peers.lock().unwrap();
        let peer = peers
            .iter_mut()
            .find(|p| p.machine_id == machine_id)
            .context("unknown peer")?;
        if let Some(a) = avatar {
            peer.avatar_override = a;
        }
        if let Some(c) = color {
            peer.color_override = c;
        }
        let out = peer.clone();
        self.flush(&peers)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, addr: &str) -> Peer {
        Peer {
            machine_id: id.into(),
            name: format!("peer-{id}"),
            avatar: None,
            color: None,
            avatar_override: None,
            color_override: None,
            profile_updated_at: None,
            token: format!("token-{id}"),
            port: 42800,
            addresses: vec![addr.into()],
            last_good_address: None,
            last_seen_at: None,
            added_at: crate::protocol::now_iso(),
            mesh_version: crate::device::MESH_VERSION,
        }
    }

    #[test]
    fn registry_roundtrip_upsert_and_token_never_leaks() {
        let dir = std::env::temp_dir().join(format!("threadknot-peers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = PeerRegistry::open(&dir).unwrap();

        reg.upsert(peer("m1", "192.168.0.10")).unwrap();
        reg.upsert(peer("m2", "192.168.0.20")).unwrap();
        // Re-pairing updates in place (idempotent), merging addresses.
        let mut m1b = peer("m1", "192.168.0.11");
        m1b.token = "rotated".into();
        reg.upsert(m1b).unwrap();
        assert_eq!(reg.list().len(), 2);
        let m1 = reg.peer("m1").unwrap();
        assert_eq!(m1.token, "rotated");
        assert_eq!(m1.addresses, vec!["192.168.0.10", "192.168.0.11"]);

        // Client-facing serialization never carries tokens…
        let listed = serde_json::to_string(&reg.list()).unwrap();
        assert!(!listed.contains("rotated") && !listed.contains("token-m2"));
        // …but a reload from disk still has them.
        let reloaded = PeerRegistry::open(&dir).unwrap();
        assert_eq!(reloaded.peer("m1").unwrap().token, "rotated");

        // Announce moves fresh addresses to the front + stamps last-good.
        reloaded
            .note_addresses("m1", &["10.0.0.5".into()], Some(43000), Some("10.0.0.5"))
            .unwrap();
        let m1 = reloaded.peer("m1").unwrap();
        assert_eq!(m1.addresses[0], "10.0.0.5");
        assert_eq!(m1.port, 43000);
        assert_eq!(m1.last_good_address.as_deref(), Some("10.0.0.5"));
        assert!(m1.last_seen_at.is_some());

        reloaded.remove("m2").unwrap();
        assert!(reloaded.remove("m2").is_err());
        assert_eq!(reloaded.list().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appearance_and_overrides_roundtrip() {
        let dir = std::env::temp_dir().join(format!("threadknot-peers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = PeerRegistry::open(&dir).unwrap();
        reg.upsert(peer("m1", "192.168.0.10")).unwrap();

        // Advertised appearance: present-key patches apply, absent keys
        // (older peers) leave stored values alone.
        let avatar = "data:image/webp;base64,aW1hZ2U=".to_string();
        assert!(reg
            .note_appearance("m1", Some(Some(avatar.clone())), Some(Some("#e0a34c".into())))
            .unwrap());
        assert!(!reg.note_appearance("m1", None, None).unwrap());
        assert!(reg.note_appearance("m1", None, Some(None)).unwrap());
        assert!(!reg.note_appearance("unknown", Some(None), None).unwrap());

        // Local overrides patch independently and never touch the
        // advertised fields.
        let m1 = reg
            .set_overrides("m1", Some(Some("data:image/png;base64,eA==".into())), None)
            .unwrap();
        assert_eq!(m1.avatar.as_deref(), Some(avatar.as_str()));
        assert_eq!(m1.color, None);
        assert_eq!(m1.avatar_override.as_deref(), Some("data:image/png;base64,eA=="));
        assert!(reg.set_overrides("unknown", None, None).is_err());

        // Re-pairing refreshes the advertised appearance but keeps our
        // local overrides.
        let mut repair = peer("m1", "192.168.0.11");
        repair.color = Some("blue".into());
        let m1 = reg.upsert(repair).unwrap();
        assert_eq!(m1.avatar, None);
        assert_eq!(m1.color.as_deref(), Some("blue"));
        assert_eq!(m1.avatar_override.as_deref(), Some("data:image/png;base64,eA=="));

        // Everything survives a reload.
        let reloaded = PeerRegistry::open(&dir).unwrap();
        let m1 = reloaded.peer("m1").unwrap();
        assert_eq!(m1.color.as_deref(), Some("blue"));
        assert_eq!(m1.avatar_override.as_deref(), Some("data:image/png;base64,eA=="));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn note_profile_last_write_wins() {
        let dir = std::env::temp_dir().join(format!("threadknot-peers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = PeerRegistry::open(&dir).unwrap();
        reg.upsert(peer("m1", "192.168.0.10")).unwrap();

        // First profile (no stored timestamp yet) always applies.
        assert!(reg
            .note_profile(
                "m1",
                Some("Newer".into()),
                Some(Some("data:image/png;base64,eA==".into())),
                None,
                Some("2026-07-23T12:00:00Z".into()),
            )
            .unwrap());
        let m1 = reg.peer("m1").unwrap();
        assert_eq!(m1.name, "Newer");
        assert_eq!(m1.profile_updated_at.as_deref(), Some("2026-07-23T12:00:00Z"));

        // A STALE frame (older timestamp) must NOT overwrite the newer stored
        // profile.
        assert!(!reg
            .note_profile(
                "m1",
                Some("Older".into()),
                None,
                None,
                Some("2026-07-23T11:00:00Z".into()),
            )
            .unwrap());
        assert_eq!(reg.peer("m1").unwrap().name, "Newer");

        // A frame with no timestamp is treated as older and skipped.
        assert!(!reg
            .note_profile("m1", Some("NoClock".into()), None, None, None)
            .unwrap());
        assert_eq!(reg.peer("m1").unwrap().name, "Newer");

        // A strictly newer frame wins.
        assert!(reg
            .note_profile(
                "m1",
                Some("Newest".into()),
                None,
                None,
                Some("2026-07-23T13:00:00Z".into()),
            )
            .unwrap());
        assert_eq!(reg.peer("m1").unwrap().name, "Newest");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Registered peer Threadknot machines (the mesh registry).
//!
//! Entries persist atomically to `~/.threadknot/peers.json`. Identity is the
//! machine id; addresses are disposable hints kept fresh by mDNS discovery and
//! `peer.announce` — a reconnect ALWAYS re-verifies the identity before trusting
//! whatever answered at an address.
//!
//! # What a pair holds (SEC-012)
//!
//! This used to be one field: the peer's **master token**, which every
//! connection then carried in a plaintext `ws://` URL. It is now four, and the
//! split is the fix:
//!
//! - `outbound_credential` — the secret *we* present when we call *them*. They
//!   minted it for us at pairing.
//! - `inbound_credential_hash` — the hash of the secret we accept *from them*.
//!   We minted it, handed over the plaintext once, and kept only the hash.
//! - `mesh_ca` — their certificate authority, pinned at pairing. This is what
//!   authenticates a peer by *key* rather than by address, so a reused DHCP
//!   lease or an mDNS spoof cannot impersonate a machine.
//! - `mesh_port` — where their TLS mesh listener lives.
//!
//! Each direction has its own credential so either side can rotate unilaterally
//! without a negotiation, and neither ever holds a secret that grants authority
//! anywhere but on the one link it was minted for. No master token is stored,
//! and none crosses the wire.

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
    /// The secret WE present when calling this peer. Never sent to clients, and
    /// never placed in a URL — it travels in an `Authorization` header inside
    /// TLS. Blank on a pair carried over from before SEC-012, which is exactly
    /// what `needs_upgrade` detects.
    #[serde(skip_serializing, default)]
    pub outbound_credential: String,
    /// Hash of the secret we accept FROM this peer. Only the hash: the
    /// plaintext was handed over once at pairing and is not recoverable here.
    #[serde(skip_serializing, default)]
    pub inbound_credential_hash: String,
    /// The peer's pinned certificate authority (PEM). Public information, but
    /// there is no reason for a phone to carry it, so it stays disk-only.
    #[serde(skip_serializing, default)]
    pub mesh_ca: String,
    /// Where the peer's TLS mesh listener is. Separate from `port`, which is
    /// still the plain-HTTP LAN port a browser uses.
    #[serde(default)]
    pub mesh_port: u16,
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

impl Peer {
    /// A placeholder carrying only what a TLS dial needs (machine id, pinned
    /// CA, mesh port). Used during pairing, where the peer is not in the
    /// registry yet but its certificate is already known.
    pub fn blank() -> Self {
        Self {
            machine_id: String::new(),
            name: String::new(),
            avatar: None,
            color: None,
            avatar_override: None,
            color_override: None,
            profile_updated_at: None,
            outbound_credential: String::new(),
            inbound_credential_hash: String::new(),
            mesh_ca: String::new(),
            mesh_port: 0,
            port: 0,
            addresses: Vec::new(),
            last_good_address: None,
            last_seen_at: None,
            added_at: String::new(),
            mesh_version: crate::device::MESH_VERSION,
        }
    }

    /// Whether this pair predates the encrypted mesh and cannot be connected.
    ///
    /// Rather than falling back to the old plaintext transport for these, the
    /// mesh refuses them and the UI says "update Threadknot on that machine".
    /// A silent fallback would mean the fix only applies to pairs made after the
    /// upgrade, which is the same as not shipping it: an attacker on the LAN
    /// would just wait for the one legacy pair.
    pub fn needs_upgrade(&self) -> bool {
        self.mesh_version < crate::device::MESH_VERSION
            || self.mesh_ca.is_empty()
            || self.outbound_credential.is_empty()
            || self.mesh_port == 0
    }
}

/// Disk format carries the secrets in sibling fields because `Peer` skips them
/// on serialize — same pattern as `hermes.rs`'s `DiskAgent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskPeer {
    #[serde(flatten)]
    peer: Peer,
    #[serde(default)]
    outbound_credential: String,
    #[serde(default)]
    inbound_credential_hash: String,
    #[serde(default)]
    mesh_ca: String,
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
                    outbound_credential: d.outbound_credential,
                    inbound_credential_hash: d.inbound_credential_hash,
                    mesh_ca: d.mesh_ca,
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
                    outbound_credential: p.outbound_credential.clone(),
                    inbound_credential_hash: p.inbound_credential_hash.clone(),
                    mesh_ca: p.mesh_ca.clone(),
                    peer: p.clone(),
                })
                .collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        // Per-peer credentials at rest — owner-only, like a key file. (No master
        // token has been stored here since SEC-012; the mode is for the
        // credentials and the hashes.)
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
            // Re-pairing mints fresh credentials on both sides and re-pins the
            // CA, which is also the only supported way to *rotate* a pair. A
            // blank incoming value would mean the caller had nothing to offer,
            // so keep what we hold rather than erasing a working link.
            if !peer.outbound_credential.is_empty() {
                existing.outbound_credential = peer.outbound_credential;
            }
            if !peer.inbound_credential_hash.is_empty() {
                existing.inbound_credential_hash = peer.inbound_credential_hash;
            }
            if !peer.mesh_ca.is_empty() {
                existing.mesh_ca = peer.mesh_ca;
            }
            if peer.mesh_port != 0 {
                existing.mesh_port = peer.mesh_port;
            }
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

    /// Resolve a credential a caller presented to the machine id that holds it.
    ///
    /// Compared against the stored **hash** in constant time, and scanned across
    /// every peer rather than looked up by a claimed machine id: a caller that
    /// could name which peer to check would be able to grind one pair's
    /// credential at a time. Here it either matches some pair or it matches
    /// nothing, and the caller learns nothing either way.
    pub fn authenticate(&self, credential: &str) -> Option<String> {
        if credential.is_empty() {
            return None;
        }
        let presented = crate::mesh::hash_credential(credential);
        let peers = self.peers.lock().unwrap();
        // Fold rather than short-circuit, so the work done does not depend on
        // which pair matched or how far down the list it was.
        peers.iter().fold(None, |found, p| {
            if !p.inbound_credential_hash.is_empty()
                && crate::mesh::constant_time_eq(&p.inbound_credential_hash, &presented)
            {
                Some(p.machine_id.clone())
            } else {
                found
            }
        })
    }

    /// Mint a new credential for `machine_id` to present to us, store its hash,
    /// and return the plaintext for the one delivery it gets.
    pub fn rotate_inbound(&self, machine_id: &str) -> Result<String> {
        let plaintext = crate::mesh::mint_credential();
        let mut peers = self.peers.lock().unwrap();
        let peer = peers
            .iter_mut()
            .find(|p| p.machine_id == machine_id)
            .context("unknown peer")?;
        peer.inbound_credential_hash = crate::mesh::hash_credential(&plaintext);
        self.flush(&peers)?;
        Ok(plaintext)
    }

    /// Record the credential a peer minted for us to present to them.
    pub fn set_outbound(&self, machine_id: &str, credential: &str) -> Result<()> {
        let mut peers = self.peers.lock().unwrap();
        let peer = peers
            .iter_mut()
            .find(|p| p.machine_id == machine_id)
            .context("unknown peer")?;
        peer.outbound_credential = credential.to_string();
        self.flush(&peers)
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
            outbound_credential: format!("outbound-{id}"),
            inbound_credential_hash: crate::mesh::hash_credential(&format!("inbound-{id}")),
            mesh_ca: format!("-----BEGIN CERTIFICATE-----\nca-{id}\n-----END CERTIFICATE-----\n"),
            mesh_port: 42802,
            port: 42800,
            addresses: vec![addr.into()],
            last_good_address: None,
            last_seen_at: None,
            added_at: crate::protocol::now_iso(),
            mesh_version: crate::device::MESH_VERSION,
        }
    }

    #[test]
    fn registry_roundtrip_upsert_and_credentials_never_leak() {
        let dir = std::env::temp_dir().join(format!("threadknot-peers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = PeerRegistry::open(&dir).unwrap();

        reg.upsert(peer("m1", "192.168.0.10")).unwrap();
        reg.upsert(peer("m2", "192.168.0.20")).unwrap();
        // Re-pairing updates in place (idempotent), merging addresses.
        let mut m1b = peer("m1", "192.168.0.11");
        m1b.outbound_credential = "rotated".into();
        reg.upsert(m1b).unwrap();
        assert_eq!(reg.list().len(), 2);
        let m1 = reg.peer("m1").unwrap();
        assert_eq!(m1.outbound_credential, "rotated");
        assert_eq!(m1.addresses, vec!["192.168.0.10", "192.168.0.11"]);

        // Client-facing serialization never carries a credential or a CA…
        let listed = serde_json::to_string(&reg.list()).unwrap();
        assert!(!listed.contains("rotated"), "outbound credential leaked to clients");
        assert!(!listed.contains("outbound-m2"), "outbound credential leaked to clients");
        assert!(!listed.contains("inbound"), "inbound credential hash leaked to clients");
        assert!(!listed.contains("BEGIN CERTIFICATE"), "pinned CA leaked to clients");
        // …but a reload from disk still has them, or every pair would break on
        // restart.
        let reloaded = PeerRegistry::open(&dir).unwrap();
        let m1 = reloaded.peer("m1").unwrap();
        assert_eq!(m1.outbound_credential, "rotated");
        assert_eq!(m1.inbound_credential_hash, crate::mesh::hash_credential("inbound-m1"));
        assert!(m1.mesh_ca.contains("ca-m1"));
        assert_eq!(m1.mesh_port, 42802);

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
    fn a_presented_credential_resolves_to_exactly_one_peer() {
        let dir = std::env::temp_dir().join(format!("threadknot-peers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = PeerRegistry::open(&dir).unwrap();
        reg.upsert(peer("m1", "192.168.0.10")).unwrap();
        reg.upsert(peer("m2", "192.168.0.20")).unwrap();

        assert_eq!(reg.authenticate("inbound-m1").as_deref(), Some("m1"));
        assert_eq!(reg.authenticate("inbound-m2").as_deref(), Some("m2"));
        // One pair's credential must not authenticate as another machine.
        assert_ne!(reg.authenticate("inbound-m1").as_deref(), Some("m2"));
        // Nothing else works, and an empty credential is never a match — a
        // blank `Authorization` header must not resolve to the first peer whose
        // hash happens to be blank.
        assert_eq!(reg.authenticate("inbound-m3"), None);
        assert_eq!(reg.authenticate(""), None);
        assert_eq!(reg.authenticate("outbound-m1"), None, "the outbound half is not accepted inbound");

        // A peer with no stored hash (a pair carried over from before SEC-012)
        // authenticates nothing at all, rather than matching the empty hash.
        let mut legacy = peer("m3", "192.168.0.30");
        legacy.inbound_credential_hash = String::new();
        reg.upsert(legacy).unwrap();
        assert_eq!(reg.authenticate(""), None);
        assert_eq!(reg.authenticate(&crate::mesh::hash_credential("")), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_replaces_the_accepted_credential_and_stores_only_a_hash() {
        let dir = std::env::temp_dir().join(format!("threadknot-peers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = PeerRegistry::open(&dir).unwrap();
        reg.upsert(peer("m1", "192.168.0.10")).unwrap();

        let fresh = reg.rotate_inbound("m1").unwrap();
        assert_eq!(reg.authenticate(&fresh).as_deref(), Some("m1"));
        // Rotation must actually invalidate the old secret, or it is not
        // rotation — it is addition.
        assert_eq!(reg.authenticate("inbound-m1"), None);
        // Only the hash is kept: the plaintext is unrecoverable from the store.
        let stored = reg.peer("m1").unwrap().inbound_credential_hash;
        assert_ne!(stored, fresh);
        assert_eq!(stored, crate::mesh::hash_credential(&fresh));

        // Two rotations never produce the same secret.
        let again = reg.rotate_inbound("m1").unwrap();
        assert_ne!(again, fresh);
        assert_eq!(reg.authenticate(&fresh), None);
        assert!(reg.rotate_inbound("nope").is_err());

        reg.set_outbound("m1", "given-by-peer").unwrap();
        assert_eq!(reg.peer("m1").unwrap().outbound_credential, "given-by-peer");
        assert!(reg.set_outbound("nope", "x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pair_from_before_the_encrypted_mesh_is_flagged_rather_than_downgraded() {
        // The whole point of `needs_upgrade`: a legacy pair must be refused and
        // reported, never quietly served over the old plaintext transport. Any
        // one missing piece is enough — a pair with no pinned CA cannot
        // authenticate the peer, and one with no credential has nothing to
        // present but a master token.
        let mut legacy = peer("old", "192.168.0.9");
        legacy.mesh_version = 1;
        assert!(legacy.needs_upgrade());

        let mut no_ca = peer("no-ca", "192.168.0.9");
        no_ca.mesh_ca = String::new();
        assert!(no_ca.needs_upgrade());

        let mut no_cred = peer("no-cred", "192.168.0.9");
        no_cred.outbound_credential = String::new();
        assert!(no_cred.needs_upgrade());

        let mut no_port = peer("no-port", "192.168.0.9");
        no_port.mesh_port = 0;
        assert!(no_port.needs_upgrade());

        assert!(!peer("current", "192.168.0.9").needs_upgrade());
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

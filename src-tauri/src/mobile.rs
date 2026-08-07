//! Mobile companion devices: revocable per-device credentials (paired once via
//! the master token, or by scanning a one-time QR pairing code) and the Expo
//! push-token registry the push service reads.
//! Persisted atomically to `~/.threadknot/mobile.json`; only credential *hashes*
//! are stored — the plaintext credential is returned exactly once at pair time.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Who a request authenticated as. The master token (server.json) can do
/// everything; a paired device credential can drive sessions and manage only
/// its own registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Master,
    Device(String),
}

fn default_true() -> bool {
    true
}

/// How a device's workspace list is read. `All` is the default so devices
/// paired before this existed keep receiving everything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyScope {
    #[default]
    All,
    Selected,
    None,
}

impl NotifyScope {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "all" => Some(Self::All),
            "selected" => Some(Self::Selected),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileDevice {
    pub id: String,
    pub name: String,
    /// "ios" | "android" | free-form.
    pub platform: String,
    /// sha256 hex of the bearer credential. Never serialized (device lists go
    /// to clients); the disk format persists it via `DiskDevice`'s sibling field.
    #[serde(skip_serializing, default)]
    pub credential_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expo_push_token: Option<String>,
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    /// Whether agent `error` events should also push (noisier).
    #[serde(default)]
    pub notify_errors: bool,
    #[serde(default)]
    pub notify_scope: NotifyScope,
    /// Workspace ids this device has opted out of (scope `All`) or opted into
    /// (scope `Selected`). One list, read either way, so the UI only ever has
    /// to express "notify me about this workspace: yes/no".
    #[serde(default)]
    pub notify_workspaces: Vec<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

impl MobileDevice {
    /// Whether this device wants to be woken about `workspace_id`. Callers
    /// resolve the workspace before asking; an empty id means "not attached to
    /// any workspace" and is only ever notified in `All` scope.
    pub fn wants_workspace(&self, workspace_id: &str) -> bool {
        if !self.notifications_enabled {
            return false;
        }
        let listed = self.notify_workspaces.iter().any(|w| w == workspace_id);
        match self.notify_scope {
            NotifyScope::All => !listed,
            NotifyScope::Selected => listed,
            NotifyScope::None => false,
        }
    }
}

/// Serde needs the hash back when *loading* the file, but `MobileDevice`
/// skips it on serialize so device lists sent to clients never leak it —
/// so the disk format carries it in a sibling field.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MobileFile {
    #[serde(default)]
    devices: Vec<DiskDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskDevice {
    #[serde(flatten)]
    device: MobileDevice,
    credential_hash: String,
}

/// How long a QR pairing code stays redeemable. Long enough to unlock a phone
/// and open the scanner, short enough that a photographed screen is worthless
/// by the time anyone acts on it.
pub const PAIRING_TTL: Duration = Duration::from_secs(180);

/// Live codes are capped so a stuck client reopening the dialog can't grow the
/// set of valid secrets without bound. Oldest is evicted first.
const MAX_PENDING_PAIRINGS: usize = 4;

/// Crockford-style base32: no I/L/O/U, so a code is unambiguous when someone
/// gives up on the camera and reads it out loud.
const PAIRING_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 10 chars over a 32-symbol alphabet = 50 bits, single-use, 3-minute TTL.
const PAIRING_CODE_LEN: usize = 10;

/// A one-time pairing code displayed as a QR on the desktop. Deliberately
/// in-memory only: it must die with the process, and nothing about it is worth
/// surviving a restart.
#[derive(Debug, Clone)]
struct PendingPairing {
    code: String,
    expires_at: Instant,
}

/// Normalize a scanned/typed code: case and the display hyphen are noise.
fn normalize_pairing_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Group as `XXXXX-XXXXX` for the human-readable fallback under the QR.
pub fn format_pairing_code(code: &str) -> String {
    if code.len() == PAIRING_CODE_LEN {
        format!("{}-{}", &code[..5], &code[5..])
    } else {
        code.to_string()
    }
}

pub struct MobileStore {
    path: PathBuf,
    devices: Mutex<Vec<MobileDevice>>,
    /// Outstanding QR pairing codes (never persisted — see `PendingPairing`).
    pending: Mutex<Vec<PendingPairing>>,
}

impl MobileStore {
    pub fn open(dir: &std::path::Path) -> Result<Self> {
        let path = dir.join("mobile.json");
        let devices = if path.exists() {
            let file: MobileFile = serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse mobile.json")?;
            file.devices
                .into_iter()
                .map(|d| MobileDevice {
                    credential_hash: d.credential_hash,
                    ..d.device
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            devices: Mutex::new(devices),
            pending: Mutex::new(Vec::new()),
        })
    }

    /// Mint a one-time pairing code for the QR the desktop is about to show.
    pub fn begin_pairing(&self) -> String {
        self.begin_pairing_with_ttl(PAIRING_TTL)
    }

    fn begin_pairing_with_ttl(&self, ttl: Duration) -> String {
        use rand::Rng;
        let mut rng = rand::rng();
        let code: String = (0..PAIRING_CODE_LEN)
            .map(|_| PAIRING_ALPHABET[rng.random_range(0..PAIRING_ALPHABET.len())] as char)
            .collect();
        let mut pending = self.pending.lock().unwrap();
        let now = Instant::now();
        pending.retain(|p| p.expires_at > now);
        while pending.len() >= MAX_PENDING_PAIRINGS {
            pending.remove(0);
        }
        pending.push(PendingPairing {
            code: code.clone(),
            expires_at: now + ttl,
        });
        code
    }

    /// Redeem a scanned code. Single use: a successful redemption removes it,
    /// so the same QR can never mint a second credential.
    pub fn redeem_pairing(&self, presented: &str) -> bool {
        let wanted = normalize_pairing_code(presented);
        if wanted.len() != PAIRING_CODE_LEN {
            return false;
        }
        let mut pending = self.pending.lock().unwrap();
        let now = Instant::now();
        pending.retain(|p| p.expires_at > now);
        match pending.iter().position(|p| p.code == wanted) {
            Some(i) => {
                pending.remove(i);
                true
            }
            None => false,
        }
    }

    /// Invalidate every outstanding code — the desktop closed the QR dialog, so
    /// what was on screen must stop working immediately rather than at the TTL.
    pub fn cancel_pairings(&self) {
        self.pending.lock().unwrap().clear();
    }

    fn flush(&self, devices: &[MobileDevice]) -> Result<()> {
        let file = MobileFile {
            devices: devices
                .iter()
                .map(|d| DiskDevice {
                    credential_hash: d.credential_hash.clone(),
                    device: d.clone(),
                })
                .collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Register a device and mint its credential. Returns (device, plaintext
    /// credential) — the plaintext is never stored or shown again.
    pub fn pair(&self, name: String, platform: String) -> Result<(MobileDevice, String)> {
        let credential = format!(
            "amd_{}{}",
            crate::store::generate_token(),
            crate::store::generate_token()
        );
        let device = MobileDevice {
            id: crate::protocol::new_id(),
            name,
            platform,
            credential_hash: hash_credential(&credential),
            expo_push_token: None,
            notifications_enabled: true,
            notify_errors: false,
            notify_scope: NotifyScope::All,
            notify_workspaces: Vec::new(),
            created_at: crate::protocol::now_iso(),
            last_seen_at: None,
        };
        let mut devices = self.devices.lock().unwrap();
        devices.push(device.clone());
        self.flush(&devices)?;
        Ok((device, credential))
    }

    /// Resolve a bearer credential to its device id (constant lookup by hash).
    pub fn authenticate(&self, credential: &str) -> Option<String> {
        if !credential.starts_with("amd_") {
            return None;
        }
        let hash = hash_credential(credential);
        self.devices
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.credential_hash == hash)
            .map(|d| d.id.clone())
    }

    pub fn list(&self) -> Vec<MobileDevice> {
        self.devices.lock().unwrap().clone()
    }

    pub fn device(&self, id: &str) -> Option<MobileDevice> {
        self.devices
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.id == id)
            .cloned()
    }

    pub fn revoke(&self, id: &str) -> Result<()> {
        let mut devices = self.devices.lock().unwrap();
        let before = devices.len();
        devices.retain(|d| d.id != id);
        anyhow::ensure!(devices.len() < before, "unknown device");
        self.flush(&devices)
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut MobileDevice)) -> Result<MobileDevice> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices
            .iter_mut()
            .find(|d| d.id == id)
            .context("unknown device")?;
        f(device);
        device.last_seen_at = Some(crate::protocol::now_iso());
        let out = device.clone();
        self.flush(&devices)?;
        Ok(out)
    }

    /// Devices that should receive a push right now (registered + subscribed
    /// to this workspace). Filtering here rather than on the phone is what
    /// makes it work while the phone is asleep.
    pub fn push_targets(&self, workspace_id: &str, include_errors_only: bool) -> Vec<MobileDevice> {
        self.devices
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.expo_push_token.is_some() && d.wants_workspace(workspace_id))
            .filter(|d| !include_errors_only || d.notify_errors)
            .cloned()
            .collect()
    }

    /// Expo reported this push token dead — stop sending to it.
    pub fn disable_push_token(&self, expo_token: &str) {
        let mut devices = self.devices.lock().unwrap();
        let mut dirty = false;
        for d in devices.iter_mut() {
            if d.expo_push_token.as_deref() == Some(expo_token) {
                d.expo_push_token = None;
                dirty = true;
            }
        }
        if dirty {
            let _ = self.flush(&devices);
        }
    }
}

pub fn hash_credential(credential: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(credential.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (PathBuf, MobileStore) {
        let dir = std::env::temp_dir().join(format!("threadknot-mobile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = MobileStore::open(&dir).unwrap();
        (dir, store)
    }

    #[test]
    fn pair_authenticate_revoke_roundtrip() {
        let (dir, store) = temp_store();
        let (device, credential) = store.pair("Pixel".into(), "android".into()).unwrap();
        assert!(credential.starts_with("amd_"));
        assert_eq!(store.authenticate(&credential), Some(device.id.clone()));
        assert_eq!(store.authenticate("amd_nope"), None);
        assert_eq!(store.authenticate("not-a-device-token"), None);

        // Credential survives a reload (hash persisted, plaintext not).
        let reloaded = MobileStore::open(&dir).unwrap();
        assert_eq!(reloaded.authenticate(&credential), Some(device.id.clone()));
        let raw = std::fs::read_to_string(dir.join("mobile.json")).unwrap();
        assert!(!raw.contains(&credential), "plaintext credential must not persist");

        store.revoke(&device.id).unwrap();
        assert_eq!(store.authenticate(&credential), None);
        assert!(store.revoke(&device.id).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pairing_code_is_single_use() {
        let (dir, store) = temp_store();
        let code = store.begin_pairing();
        assert_eq!(code.len(), PAIRING_CODE_LEN);
        assert!(
            code.chars().all(|c| PAIRING_ALPHABET.contains(&(c as u8))),
            "code must stay in the unambiguous alphabet: {code}"
        );
        // Hyphen and case are display noise, not part of the secret.
        assert!(store.redeem_pairing(&format_pairing_code(&code).to_lowercase()));
        assert!(
            !store.redeem_pairing(&code),
            "a redeemed code must not mint a second credential"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pairing_codes_expire_and_can_be_cancelled() {
        let (dir, store) = temp_store();
        let stale = store.begin_pairing_with_ttl(Duration::ZERO);
        assert!(!store.redeem_pairing(&stale), "an expired code is dead");

        let live = store.begin_pairing();
        store.cancel_pairings();
        assert!(
            !store.redeem_pairing(&live),
            "closing the QR dialog must invalidate what was on screen"
        );

        assert!(!store.redeem_pairing(""), "empty input is not a code");
        assert!(!store.redeem_pairing("SHORT"), "wrong length is not a code");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_pairings_are_capped() {
        let (dir, store) = temp_store();
        let codes: Vec<String> = (0..MAX_PENDING_PAIRINGS + 2)
            .map(|_| store.begin_pairing())
            .collect();
        assert_eq!(store.pending.lock().unwrap().len(), MAX_PENDING_PAIRINGS);
        // The two oldest were evicted; the newest survive.
        for old in &codes[..2] {
            assert!(!store.redeem_pairing(old), "evicted code must not redeem");
        }
        for fresh in &codes[2..] {
            assert!(store.redeem_pairing(fresh), "recent code must still redeem");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn push_targets_and_dead_token_cleanup() {
        let (dir, store) = temp_store();
        let (a, _) = store.pair("iPhone".into(), "ios".into()).unwrap();
        let (b, _) = store.pair("Pixel".into(), "android".into()).unwrap();
        store
            .update(&a.id, |d| d.expo_push_token = Some("ExponentPushToken[aaa]".into()))
            .unwrap();
        store
            .update(&b.id, |d| {
                d.expo_push_token = Some("ExponentPushToken[bbb]".into());
                d.notifications_enabled = false;
            })
            .unwrap();
        let targets = store.push_targets("ws-1", false);
        assert_eq!(targets.len(), 1, "disabled device excluded");
        assert_eq!(targets[0].id, a.id);
        assert!(store.push_targets("ws-1", true).is_empty(), "errors are opt-in");

        store.disable_push_token("ExponentPushToken[aaa]");
        assert!(store.push_targets("ws-1", false).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_scoping_splits_devices() {
        let (dir, store) = temp_store();
        let (mine, _) = store.pair("Desk".into(), "android".into()).unwrap();
        let (hers, _) = store.pair("Hers".into(), "ios".into()).unwrap();
        for (d, ws) in [(&mine, "ws-mine"), (&hers, "ws-hers")] {
            store
                .update(&d.id, |dev| {
                    dev.expo_push_token = Some(format!("ExponentPushToken[{}]", dev.name));
                    dev.notify_scope = NotifyScope::Selected;
                    dev.notify_workspaces = vec![ws.to_string()];
                })
                .unwrap();
        }

        let mine_targets = store.push_targets("ws-mine", false);
        assert_eq!(mine_targets.len(), 1);
        assert_eq!(mine_targets[0].id, mine.id, "her phone stays quiet");
        let hers_targets = store.push_targets("ws-hers", false);
        assert_eq!(hers_targets.len(), 1);
        assert_eq!(hers_targets[0].id, hers.id);
        assert!(
            store.push_targets("ws-third", false).is_empty(),
            "an allowlist must not leak an unlisted workspace"
        );
        assert!(
            store.push_targets("", false).is_empty(),
            "unresolvable workspace fails closed under an allowlist"
        );

        // Scope survives a reload, and `All` keeps its list as a mute list.
        store
            .update(&mine.id, |dev| {
                dev.notify_scope = NotifyScope::All;
                dev.notify_workspaces = vec!["ws-noisy".into()];
            })
            .unwrap();
        let reloaded = MobileStore::open(&dir).unwrap();
        assert_eq!(reloaded.push_targets("ws-noisy", false).len(), 0);
        assert_eq!(reloaded.push_targets("ws-anything", false)[0].id, mine.id);
        assert_eq!(
            reloaded.push_targets("", false)[0].id,
            mine.id,
            "server-level notices still reach an unfiltered device"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

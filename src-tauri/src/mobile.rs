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

/// One consequence a paired device may be granted. Grants are chosen by the
/// desktop owner, stored server-side against the device, and checked centrally
/// — a client never asserts its own authority.
///
/// The split is by *consequence*, not by endpoint: `files` is read access to
/// project bytes, `terminal` is an interactive shell, `signedBrowser` is the
/// owner's logged-in identity. `mesh` never widens another grant — it only lets
/// the grants a device already holds reach paired machines.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    /// View threads, send/steer/interrupt turns, answer approvals + questions.
    Threads,
    /// Browse, preview and download project/artifact bytes.
    Files,
    /// Read and act on repository state.
    Git,
    /// Create and attach to local PTYs — an interactive shell on this machine.
    Terminal,
    /// View and drive disposable (unsigned) browser sessions.
    Browser,
    /// Select and drive durable signed-in browser profiles. High-risk identity
    /// authority; never granted implicitly.
    SignedBrowser,
    /// Exercise already-granted capabilities on paired Threadknot machines.
    Mesh,
}

impl Capability {
    pub const ALL: [Capability; 7] = [
        Capability::Threads,
        Capability::Files,
        Capability::Git,
        Capability::Terminal,
        Capability::Browser,
        Capability::SignedBrowser,
        Capability::Mesh,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Threads => "threads",
            Capability::Files => "files",
            Capability::Git => "git",
            Capability::Terminal => "terminal",
            Capability::Browser => "browser",
            Capability::SignedBrowser => "signedBrowser",
            Capability::Mesh => "mesh",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Capability::ALL.into_iter().find(|c| c.as_str() == raw)
    }

    /// Why a device was refused — phrased for the person holding the phone,
    /// who can only fix this from the desktop.
    pub fn denial(self) -> &'static str {
        match self {
            Capability::Threads => "this device is not allowed to drive chats",
            Capability::Files => "this device is not allowed to read project files",
            Capability::Git => "this device is not allowed to use git",
            Capability::Terminal => "this device is not allowed to open terminals",
            Capability::Browser => "this device is not allowed to use the browser",
            Capability::SignedBrowser => {
                "this device is not allowed to use signed-in browser profiles"
            }
            Capability::Mesh => "this device is not allowed to reach other machines",
        }
    }
}

/// Schema version of the stored grant set. Bumped when the meaning of a stored
/// capability changes, so an old record can be re-interpreted rather than
/// silently mis-enforced.
pub const CAPABILITIES_VERSION: u32 = 1;

/// What a device gets when the owner does not narrow it — and what a device
/// paired before capabilities existed is read back as, so no phone in the field
/// loses a feature it already had.
///
/// `signedBrowser` is deliberately absent: driving the owner's logged-in browser
/// identity is never implicit, on any device, however it was paired.
pub fn default_capabilities() -> Vec<Capability> {
    vec![
        Capability::Threads,
        Capability::Files,
        Capability::Git,
        Capability::Terminal,
        Capability::Browser,
        Capability::Mesh,
    ]
}

fn default_capabilities_version() -> u32 {
    CAPABILITIES_VERSION
}

/// Normalize a requested grant set: known names only, de-duplicated, in a
/// stable order. Unknown names are dropped rather than erroring — a newer
/// desktop must never be able to widen an older server by naming a capability
/// it does not understand.
pub fn normalize_capabilities(requested: impl IntoIterator<Item = Capability>) -> Vec<Capability> {
    let mut out: Vec<Capability> = requested.into_iter().collect();
    out.sort();
    out.dedup();
    out
}

/// An authenticated device plus the grants it currently holds. Resolved at the
/// boundary from the presented credential, never from the request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGrant {
    pub id: String,
    pub capabilities: Vec<Capability>,
}

impl DeviceGrant {
    pub fn can(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// A paired machine calling in, carrying the authority of whoever asked *it*.
///
/// This is the mesh-principal-propagation half of SEC-012. Before it, a peer
/// authenticated with the other machine's **master token**, so every routed
/// request arrived here as this machine's owner — a phone that had been denied
/// `terminal` locally could ask a peer to open one and it would be honoured as
/// Master. The caller-side check in `handle_request` closed the common cases,
/// but only the *originating* side knew who was really asking, and anything it
/// could not inspect from there (a remote thread's stored settings, SEC-003's
/// residual gap) stayed open.
///
/// Now the routed frame carries the caller's own authority and this side
/// enforces it. The peer is trusted to describe its own caller honestly —
/// it authenticated with a credential we minted for it alone — but it can only
/// ever *narrow*: there is no assertion a peer can make that grants more than
/// the peer's own owner already had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPrincipal {
    /// Which paired machine the request came in over.
    pub machine_id: String,
    /// The grants the originating caller held on that machine. `None` means the
    /// request originated from that machine's own owner, which carries the
    /// authority of a local Master for everything except this machine's own
    /// master credential (see `is_local_master`).
    pub on_behalf_of: Option<Vec<Capability>>,
}

/// Who a request authenticated as. The master token (server.json) can do
/// everything; a paired device credential can do exactly what its stored grants
/// allow and manage only its own registration; a paired machine acts with
/// whatever authority its own caller had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Master,
    Device(DeviceGrant),
    Peer(PeerPrincipal),
}

impl Principal {
    /// Exactly the master credential of **this** machine.
    ///
    /// The narrow test, and the one to reach for whenever the answer would
    /// disclose or accept this machine's own master token — `hello`'s
    /// token-bearing `lanUrl` is the canonical case. A peer must be false here
    /// even when it is acting for its own owner: it is administering its
    /// machine, and handing it our credential would rebuild SEC-001 across the
    /// mesh instead of across a pairing.
    pub fn is_local_master(&self) -> bool {
        matches!(self, Principal::Master)
    }

    /// Whether this principal carries machine-administration authority.
    ///
    /// The broad test, and the right one for the "requires this machine's master
    /// token" guards. A peer acting for its own owner passes: the fleet view
    /// exists so that sitting at one machine can administer another, and that
    /// was already true when peers authenticated as Master. What changed is that
    /// a peer acting for a *device* no longer passes.
    pub fn is_owner(&self) -> bool {
        match self {
            Principal::Master => true,
            Principal::Device(_) => false,
            Principal::Peer(peer) => peer.on_behalf_of.is_none(),
        }
    }

    /// Whether this request arrived over a peer link, and from which machine.
    pub fn peer_machine_id(&self) -> Option<&str> {
        match self {
            Principal::Peer(peer) => Some(&peer.machine_id),
            _ => None,
        }
    }

    pub fn is_peer(&self) -> bool {
        matches!(self, Principal::Peer(_))
    }

    pub fn device_id(&self) -> Option<&str> {
        match self {
            Principal::Device(grant) => Some(&grant.id),
            Principal::Master | Principal::Peer(_) => None,
        }
    }

    /// Master holds every capability implicitly; a device holds only what the
    /// owner stored against it; a peer holds whatever its own caller held.
    pub fn can(&self, capability: Capability) -> bool {
        match self {
            Principal::Master => true,
            Principal::Device(grant) => grant.can(capability),
            Principal::Peer(peer) => match &peer.on_behalf_of {
                None => true,
                Some(capabilities) => capabilities.contains(&capability),
            },
        }
    }

    pub fn require(&self, capability: Capability) -> anyhow::Result<()> {
        anyhow::ensure!(self.can(capability), "{}", capability.denial());
        Ok(())
    }

    /// How this principal should be described to the machine it is about to
    /// route a request to.
    ///
    /// `None` for Master — the far side reads an absent assertion as "the
    /// owner". `Some(caps)` for a device. For a request arriving from a peer and
    /// routed onward, the *original* caller's authority is carried through
    /// unchanged rather than being re-widened at each hop, so a three-machine
    /// path enforces the same grants as a two-machine one.
    pub fn mesh_assertion(&self) -> Option<Vec<Capability>> {
        match self {
            Principal::Master => None,
            Principal::Device(grant) => Some(grant.capabilities.clone()),
            Principal::Peer(peer) => peer.on_behalf_of.clone(),
        }
    }

    /// What `hello` reports, so the UI can hide what was never granted.
    pub fn label(&self) -> &'static str {
        match self {
            Principal::Master => "master",
            Principal::Device(_) => "device",
            Principal::Peer(_) => "peer",
        }
    }

    /// The concrete grant list this principal holds, for `hello`.
    pub fn capabilities(&self) -> Vec<Capability> {
        match self {
            Principal::Master => Capability::ALL.to_vec(),
            Principal::Device(grant) => grant.capabilities.clone(),
            Principal::Peer(peer) => match &peer.on_behalf_of {
                None => Capability::ALL.to_vec(),
                Some(capabilities) => capabilities.clone(),
            },
        }
    }
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
    /// What this device may do. Absent on records written before capabilities
    /// existed, which read back as [`default_capabilities`].
    #[serde(default = "default_capabilities")]
    pub capabilities: Vec<Capability>,
    #[serde(default = "default_capabilities_version")]
    pub capabilities_version: u32,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

impl MobileDevice {
    pub fn can(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

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
    /// Grants the desktop owner selected when this QR was shown. The joining
    /// client redeems the code and takes these — it cannot request, add, or
    /// widen them.
    capabilities: Vec<Capability>,
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

    /// Mint a one-time pairing code for the QR the desktop is about to show,
    /// bound to the grants the owner picked.
    pub fn begin_pairing(&self, capabilities: Vec<Capability>) -> String {
        self.begin_pairing_with_ttl(capabilities, PAIRING_TTL)
    }

    fn begin_pairing_with_ttl(&self, capabilities: Vec<Capability>, ttl: Duration) -> String {
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
            capabilities: normalize_capabilities(capabilities),
        });
        code
    }

    /// Redeem a scanned code, yielding the grants the owner bound to it. Single
    /// use: a successful redemption removes it, so the same QR can never mint a
    /// second credential.
    pub fn redeem_pairing(&self, presented: &str) -> Option<Vec<Capability>> {
        let wanted = normalize_pairing_code(presented);
        if wanted.len() != PAIRING_CODE_LEN {
            return None;
        }
        let mut pending = self.pending.lock().unwrap();
        let now = Instant::now();
        pending.retain(|p| p.expires_at > now);
        let i = pending.iter().position(|p| p.code == wanted)?;
        Some(pending.remove(i).capabilities)
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
        // Credential hashes and the grant set both live here: owner-only, and
        // written that way before any bytes land (rename carries the mode over).
        crate::store::write_private(&tmp, &serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Register a device and mint its credential. Returns (device, plaintext
    /// credential) — the plaintext is never stored or shown again.
    pub fn pair(
        &self,
        name: String,
        platform: String,
        capabilities: Vec<Capability>,
    ) -> Result<(MobileDevice, String)> {
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
            capabilities: normalize_capabilities(capabilities),
            capabilities_version: CAPABILITIES_VERSION,
            created_at: crate::protocol::now_iso(),
            last_seen_at: None,
        };
        let mut devices = self.devices.lock().unwrap();
        devices.push(device.clone());
        self.flush(&devices)?;
        Ok((device, credential))
    }

    /// Resolve a bearer credential to the device and the grants it holds RIGHT
    /// NOW (lookup by hash). Authority is read from storage on every
    /// authentication, so narrowing a device's grants takes effect on its next
    /// request without any client cooperation.
    pub fn authenticate(&self, credential: &str) -> Option<DeviceGrant> {
        if !credential.starts_with("amd_") {
            return None;
        }
        let hash = hash_credential(credential);
        self.devices
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.credential_hash == hash)
            .map(|d| DeviceGrant {
                id: d.id.clone(),
                capabilities: d.capabilities.clone(),
            })
    }

    /// The grants a device holds right now, by id — how a cookie session
    /// resolves its authority without storing any of it on the session.
    pub fn grant_for(&self, device_id: &str) -> Option<DeviceGrant> {
        self.devices
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.id == device_id)
            .map(|d| DeviceGrant {
                id: d.id.clone(),
                capabilities: d.capabilities.clone(),
            })
    }

    /// Replace a device's grants (Master-only, enforced by the caller).
    /// Reductions must be treated like a revocation for live sockets — see
    /// `SessionRegistry::close_device`.
    pub fn set_capabilities(
        &self,
        id: &str,
        capabilities: Vec<Capability>,
    ) -> Result<MobileDevice> {
        let normalized = normalize_capabilities(capabilities);
        self.update(id, |d| {
            d.capabilities = normalized;
            d.capabilities_version = CAPABILITIES_VERSION;
        })
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
        let (device, credential) = store.pair("Pixel".into(), "android".into(), default_capabilities()).unwrap();
        assert!(credential.starts_with("amd_"));
        assert_eq!(store.authenticate(&credential).map(|g| g.id), Some(device.id.clone()));
        assert!(store.authenticate("amd_nope").is_none());
        assert!(store.authenticate("not-a-device-token").is_none());

        // Credential survives a reload (hash persisted, plaintext not).
        let reloaded = MobileStore::open(&dir).unwrap();
        assert_eq!(
            reloaded.authenticate(&credential).map(|g| g.id),
            Some(device.id.clone())
        );
        let raw = std::fs::read_to_string(dir.join("mobile.json")).unwrap();
        assert!(!raw.contains(&credential), "plaintext credential must not persist");

        store.revoke(&device.id).unwrap();
        assert!(store.authenticate(&credential).is_none());
        assert!(store.revoke(&device.id).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pairing_code_is_single_use() {
        let (dir, store) = temp_store();
        let code = store.begin_pairing(default_capabilities());
        assert_eq!(code.len(), PAIRING_CODE_LEN);
        assert!(
            code.chars().all(|c| PAIRING_ALPHABET.contains(&(c as u8))),
            "code must stay in the unambiguous alphabet: {code}"
        );
        // Hyphen and case are display noise, not part of the secret.
        assert!(store
            .redeem_pairing(&format_pairing_code(&code).to_lowercase())
            .is_some());
        assert!(
            store.redeem_pairing(&code).is_none(),
            "a redeemed code must not mint a second credential"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pairing_codes_expire_and_can_be_cancelled() {
        let (dir, store) = temp_store();
        let stale = store.begin_pairing_with_ttl(default_capabilities(), Duration::ZERO);
        assert!(
            store.redeem_pairing(&stale).is_none(),
            "an expired code is dead"
        );

        let live = store.begin_pairing(default_capabilities());
        store.cancel_pairings();
        assert!(
            store.redeem_pairing(&live).is_none(),
            "closing the QR dialog must invalidate what was on screen"
        );

        assert!(store.redeem_pairing("").is_none(), "empty input is not a code");
        assert!(
            store.redeem_pairing("SHORT").is_none(),
            "wrong length is not a code"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_pairings_are_capped() {
        let (dir, store) = temp_store();
        let codes: Vec<String> = (0..MAX_PENDING_PAIRINGS + 2)
            .map(|_| store.begin_pairing(default_capabilities()))
            .collect();
        assert_eq!(store.pending.lock().unwrap().len(), MAX_PENDING_PAIRINGS);
        // The two oldest were evicted; the newest survive.
        for old in &codes[..2] {
            assert!(
                store.redeem_pairing(old).is_none(),
                "evicted code must not redeem"
            );
        }
        for fresh in &codes[2..] {
            assert!(
                store.redeem_pairing(fresh).is_some(),
                "recent code must still redeem"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// SEC-004: the grants live on the pairing code, chosen by the desktop
    /// owner. Whatever the joining client says it wants is irrelevant — it can
    /// only redeem what was bound.
    #[test]
    fn pairing_code_carries_the_owners_grants() {
        let (dir, store) = temp_store();
        let narrow = vec![Capability::Threads];
        let code = store.begin_pairing(narrow.clone());
        assert_eq!(store.redeem_pairing(&code), Some(narrow.clone()));

        // And a device paired from that redemption authenticates with exactly
        // those grants — not the defaults.
        let (device, credential) = store
            .pair("Locked down".into(), "ios".into(), narrow)
            .unwrap();
        let grant = store.authenticate(&credential).unwrap();
        assert_eq!(grant.id, device.id);
        assert!(grant.can(Capability::Threads));
        for denied in [
            Capability::Files,
            Capability::Git,
            Capability::Terminal,
            Capability::Browser,
            Capability::SignedBrowser,
            Capability::Mesh,
        ] {
            assert!(!grant.can(denied), "{denied:?} was never granted");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// signedBrowser is the owner's logged-in identity: it is never part of the
    /// default set, so no pairing path and no legacy record can hand it out
    /// implicitly.
    #[test]
    fn signed_browser_is_never_implicit() {
        assert!(!default_capabilities().contains(&Capability::SignedBrowser));
        let (dir, store) = temp_store();
        let (_, credential) = store
            .pair("Phone".into(), "ios".into(), default_capabilities())
            .unwrap();
        assert!(!store
            .authenticate(&credential)
            .unwrap()
            .can(Capability::SignedBrowser));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A device paired before capabilities existed has no `capabilities` key on
    /// disk. It must read back as the default set — losing its terminal or its
    /// files pane on upgrade would be an outage, not a hardening.
    #[test]
    fn legacy_records_backfill_the_default_grants() {
        let dir = std::env::temp_dir().join(format!("threadknot-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let credential = "amd_legacy_credential";
        std::fs::write(
            dir.join("mobile.json"),
            serde_json::json!({
                "devices": [{
                    "id": "dev-1",
                    "name": "Old Pixel",
                    "platform": "android",
                    "credentialHash": hash_credential(credential),
                    "notificationsEnabled": true,
                    "createdAt": "2026-01-01T00:00:00Z",
                }]
            })
            .to_string(),
        )
        .unwrap();

        let store = MobileStore::open(&dir).unwrap();
        let grant = store.authenticate(credential).unwrap();
        assert_eq!(grant.capabilities, default_capabilities());
        assert_eq!(store.list()[0].capabilities_version, CAPABILITIES_VERSION);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Narrowing grants must be visible to the very next authentication — the
    /// resolver reads storage, it does not trust anything the client cached.
    #[test]
    fn narrowing_grants_takes_effect_immediately_and_persists() {
        let (dir, store) = temp_store();
        let (device, credential) = store
            .pair("Phone".into(), "ios".into(), default_capabilities())
            .unwrap();
        assert!(store.authenticate(&credential).unwrap().can(Capability::Terminal));

        store
            .set_capabilities(&device.id, vec![Capability::Threads, Capability::Threads])
            .unwrap();
        let grant = store.authenticate(&credential).unwrap();
        assert_eq!(grant.capabilities, vec![Capability::Threads], "deduplicated");
        assert!(!grant.can(Capability::Terminal));
        assert!(!grant.can(Capability::Mesh));

        let reloaded = MobileStore::open(&dir).unwrap();
        assert_eq!(
            reloaded.authenticate(&credential).unwrap().capabilities,
            vec![Capability::Threads],
            "a reduction must survive a restart"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Capability names are a wire contract (mobile UI + stored records), so a
    /// rename would silently drop a grant on reload.
    #[test]
    fn capability_names_round_trip() {
        for capability in Capability::ALL {
            assert_eq!(Capability::parse(capability.as_str()), Some(capability));
            let json = serde_json::to_string(&capability).unwrap();
            assert_eq!(json, format!("\"{}\"", capability.as_str()));
        }
        assert_eq!(Capability::parse("master"), None);
        assert_eq!(Capability::parse(""), None);
    }

    #[test]
    fn push_targets_and_dead_token_cleanup() {
        let (dir, store) = temp_store();
        let (a, _) = store.pair("iPhone".into(), "ios".into(), default_capabilities()).unwrap();
        let (b, _) = store.pair("Pixel".into(), "android".into(), default_capabilities()).unwrap();
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
        let (mine, _) = store.pair("Desk".into(), "android".into(), default_capabilities()).unwrap();
        let (hers, _) = store.pair("Hers".into(), "ios".into(), default_capabilities()).unwrap();
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

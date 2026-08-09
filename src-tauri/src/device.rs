//! Machine identity for the mesh: who this Threadknot install is, what it can
//! run, and the friendly name peers show for it.
//!
//! The machine id IS `server.json`'s `server_id` — one identity per install
//! (mobile push already keys on it), never a second UUID. `device.json` holds
//! only what that file doesn't: the user-editable friendly name.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Bumped when the peer wire protocol changes incompatibly. Exchanged in
/// `device.info` and pairing so mismatched Threadknots refuse to pair with a
/// readable "update Threadknot on X" instead of failing mid-turn.
///
/// **2** — SEC-012. Peer links are TLS against a certificate authority pinned at
/// pairing, credentials are per-link and travel in headers, and routed requests
/// carry the originating caller's grants. Version 1 is not merely different, it
/// is the vulnerability: a v1 peer can only be reached by putting a master token
/// in a plaintext URL, so v1 pairs are refused rather than downgraded to.
pub const MESH_VERSION: u32 = 2;

/// `Default` is written out rather than derived because the derive and serde's
/// field defaults are two different things: a *missing file* takes
/// `Default::default()`, a *missing field* takes `#[serde(default = …)]`, and
/// `#[derive(Default)]` would have given a first-run machine
/// `accepts_dispatch: false` while every upgraded machine got `true`. One
/// source of truth for both paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceFile {
    #[serde(default)]
    friendly_name: String,
    /// Small square data URL peers show as this machine's profile picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
    /// CSS accent color peers show for this machine (e.g. "#e0a34c").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    /// ISO-8601 timestamp of the last profile edit (name/avatar/color). Used
    /// as the last-write-wins clock for peer-to-peer profile gossip: a peer
    /// only overwrites its stored copy of this machine's profile when the
    /// incoming timestamp is newer.
    #[serde(default)]
    profile_updated_at: String,
    /// Whether this machine will run work another machine's thread sends it.
    ///
    /// Being in the mesh is not consent to this. Pairing already lets a peer
    /// read files and drive threads that exist here; a dispatch starts a *new*
    /// agent with a brief written by a model on somebody else's box. Default on,
    /// because a single-owner fleet is the case Threadknot is built for — but
    /// the knob has to exist before the first dispatch, not after the first
    /// surprise.
    #[serde(default = "accept_default")]
    accepts_dispatch: bool,
    /// The most access a dispatched worker may run with here, whatever the
    /// sender asked for. `None` = no ceiling beyond the sender's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_dispatch_access: Option<crate::protocol::Access>,
}

fn accept_default() -> bool {
    true
}

impl Default for DeviceFile {
    fn default() -> Self {
        Self {
            friendly_name: String::new(),
            avatar: None,
            color: None,
            profile_updated_at: String::new(),
            accepts_dispatch: accept_default(),
            max_dispatch_access: None,
        }
    }
}

pub struct Device {
    /// == `server.json`'s `server_id` (stable per install).
    pub machine_id: String,
    file: Mutex<DeviceFile>,
    path: PathBuf,
}

impl Device {
    /// Load (or create) `<dir>/device.json`. A missing/blank friendly name
    /// defaults to the hostname and is persisted so it stays stable even if
    /// the hostname later changes.
    pub fn load(dir: &Path, machine_id: &str) -> Result<Self> {
        let path = dir.join("device.json");
        let mut file: DeviceFile = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse device.json")?
        } else {
            DeviceFile::default()
        };
        let mut dirty = false;
        if file.friendly_name.trim().is_empty() {
            file.friendly_name = hostname();
            dirty = true;
        }
        // Seed the LWW clock on first load so older installs (no timestamp)
        // start with a concrete value peers can compare against.
        if file.profile_updated_at.trim().is_empty() {
            file.profile_updated_at = crate::protocol::now_iso();
            dirty = true;
        }
        if dirty {
            std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
        }
        Ok(Self {
            machine_id: machine_id.to_string(),
            file: Mutex::new(file),
            path,
        })
    }

    pub fn friendly_name(&self) -> String {
        self.file.lock().unwrap().friendly_name.clone()
    }

    pub fn avatar(&self) -> Option<String> {
        self.file.lock().unwrap().avatar.clone()
    }

    pub fn color(&self) -> Option<String> {
        self.file.lock().unwrap().color.clone()
    }

    /// ISO-8601 timestamp of the last profile edit (the LWW clock).
    pub fn profile_updated_at(&self) -> String {
        self.file.lock().unwrap().profile_updated_at.clone()
    }

    fn save(&self, file: &DeviceFile) -> Result<()> {
        std::fs::write(&self.path, serde_json::to_string_pretty(file)?)?;
        Ok(())
    }

    pub fn set_friendly_name(&self, name: &str) -> Result<String> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "name cannot be empty");
        let name: String = name.chars().take(64).collect();
        let mut file = self.file.lock().unwrap();
        file.friendly_name = name.clone();
        file.profile_updated_at = crate::protocol::now_iso();
        self.save(&file)?;
        Ok(name)
    }

    /// Patch the machine's appearance. Outer `None` leaves a field untouched,
    /// `Some(None)` clears it, `Some(Some(v))` validates and sets it. Returns
    /// the resulting `(avatar, color)`.
    pub fn set_appearance(
        &self,
        avatar: Option<Option<String>>,
        color: Option<Option<String>>,
    ) -> Result<(Option<String>, Option<String>)> {
        let avatar = match avatar {
            Some(Some(image)) => {
                validate_avatar(&image)?;
                Some(Some(image))
            }
            other => other,
        };
        let color = match color {
            Some(Some(c)) => Some(Some(validate_accent_color(&c)?)),
            other => other,
        };
        let mut file = self.file.lock().unwrap();
        if let Some(a) = avatar {
            file.avatar = a;
        }
        if let Some(c) = color {
            file.color = c;
        }
        file.profile_updated_at = crate::protocol::now_iso();
        self.save(&file)?;
        Ok((file.avatar.clone(), file.color.clone()))
    }

    /// The `device.info` payload: identity + live capability detection (what
    /// the machine picker and pairing show for this machine).
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({
            "machineId": self.machine_id,
            "friendlyName": self.friendly_name(),
            "avatar": self.avatar(),
            "color": self.color(),
            "profileUpdatedAt": self.profile_updated_at(),
            "hostname": hostname(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "version": env!("THREADKNOT_VERSION"),
            "meshVersion": MESH_VERSION,
            "capabilities": capabilities(),
            "acceptsDispatch": self.accepts_dispatch(),
            "maxDispatchAccess": self.max_dispatch_access(),
        })
    }

    pub fn accepts_dispatch(&self) -> bool {
        self.file.lock().unwrap().accepts_dispatch
    }

    pub fn max_dispatch_access(&self) -> Option<crate::protocol::Access> {
        self.file.lock().unwrap().max_dispatch_access
    }

    /// Settings → machines → "accept dispatched work". Machine-local and never
    /// gossiped: it is this machine's own policy about itself, and a peer
    /// learns it by asking (`device.info`), not by being told.
    pub fn set_dispatch_policy(
        &self,
        accepts: Option<bool>,
        ceiling: Option<Option<crate::protocol::Access>>,
    ) -> Result<()> {
        let mut file = self.file.lock().unwrap();
        if let Some(accepts) = accepts {
            file.accepts_dispatch = accepts;
        }
        if let Some(ceiling) = ceiling {
            file.max_dispatch_access = ceiling;
        }
        self.save(&file)
    }
}

/// A profile image must be an inline base64 image data URL and stay small
/// enough to ride pairing/announce/hello frames (the frontend resizes to a
/// few KB before sending; 64 KB is the hard server-side ceiling).
pub fn validate_avatar(image: &str) -> Result<()> {
    anyhow::ensure!(
        image.starts_with("data:image/") && image.contains(";base64,"),
        "avatar must be an image data URL"
    );
    anyhow::ensure!(image.len() <= 64 * 1024, "Image too large, pick a smaller one");
    Ok(())
}

/// An accent color is a short CSS color string ("#e0a34c", "tomato",
/// "rgb(1,2,3)"): bounded and whitespace-free so it can be dropped straight
/// into a style attribute.
pub fn validate_accent_color(color: &str) -> Result<String> {
    let color = color.trim();
    anyhow::ensure!(!color.is_empty(), "color cannot be empty");
    anyhow::ensure!(color.len() <= 32, "color is too long");
    anyhow::ensure!(
        !color.chars().any(char::is_whitespace),
        "color cannot contain whitespace"
    );
    let first = color.chars().next().unwrap();
    anyhow::ensure!(
        first == '#' || first.is_ascii_alphabetic(),
        "color must be a CSS color like #e0a34c"
    );
    Ok(color.to_string())
}

/// Real hostname via gethostname(2) — NOT `$HOSTNAME`, which is a shell
/// variable that .desktop-launched processes usually don't inherit.
fn hostname() -> String {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    if name.is_empty() {
        "unknown".into()
    } else {
        name
    }
}

/// What agent drivers this machine can run, from a PATH scan. Advertised in
/// the machine picker so you know before creating a thread whether the target
/// machine can actually drive the chosen agent.
fn capabilities() -> Vec<String> {
    let mut caps = vec!["filesystem".to_string(), "terminal".to_string()];
    for (cmd, cap) in [
        ("claude", "run-claude"),
        ("codex", "run-codex"),
        ("kimi", "run-kimi"),
    ] {
        if crate::agents::resolve_bin(cmd).is_some() {
            caps.push(cap.to_string());
        }
    }
    if has_cmd("git") {
        caps.push("git".into());
    }
    caps
}

fn has_cmd(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_persists_friendly_name_and_reports_identity() {
        let dir = std::env::temp_dir().join(format!("threadknot-device-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let d = Device::load(&dir, "machine-1").unwrap();
        assert_eq!(d.machine_id, "machine-1");
        let default_name = d.friendly_name();
        assert!(!default_name.is_empty());

        d.set_friendly_name("  Dev Rig  ").unwrap();
        assert_eq!(d.friendly_name(), "Dev Rig");
        assert!(d.set_friendly_name("   ").is_err());

        // Reload keeps the custom name (it must survive hostname changes).
        let reloaded = Device::load(&dir, "machine-1").unwrap();
        assert_eq!(reloaded.friendly_name(), "Dev Rig");

        let info = reloaded.info();
        assert_eq!(info["machineId"], "machine-1");
        assert_eq!(info["meshVersion"], MESH_VERSION);
        assert!(info["capabilities"].as_array().unwrap().len() >= 2);
        // A first-run machine and an upgraded one must agree: the derive and
        // the serde field default are different code paths, and they disagreed.
        assert_eq!(
            info["acceptsDispatch"], true,
            "a machine created from scratch accepts dispatch, like an upgraded one"
        );
        let upgraded: DeviceFile = serde_json::from_str("{}").unwrap();
        assert!(upgraded.accepts_dispatch);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appearance_patches_persist_and_clear() {
        let dir = std::env::temp_dir().join(format!("threadknot-device-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = Device::load(&dir, "machine-1").unwrap();

        let avatar = "data:image/webp;base64,aW1hZ2U=".to_string();
        let (a, c) = d
            .set_appearance(Some(Some(avatar.clone())), Some(Some("#e0a34c".into())))
            .unwrap();
        assert_eq!(a.as_deref(), Some(avatar.as_str()));
        assert_eq!(c.as_deref(), Some("#e0a34c"));

        // Absent leaves a field, null (Some(None)) clears it; both survive a
        // reload and never disturb the friendly name.
        d.set_friendly_name("Dev Rig").unwrap();
        let (a, c) = d.set_appearance(None, Some(None)).unwrap();
        assert_eq!(a.as_deref(), Some(avatar.as_str()));
        assert_eq!(c, None);
        let reloaded = Device::load(&dir, "machine-1").unwrap();
        assert_eq!(reloaded.avatar().as_deref(), Some(avatar.as_str()));
        assert_eq!(reloaded.color(), None);
        assert_eq!(reloaded.friendly_name(), "Dev Rig");

        let info = reloaded.info();
        assert_eq!(info["avatar"], avatar.as_str());
        assert_eq!(info["color"], serde_json::Value::Null);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn avatar_and_color_validation() {
        assert!(validate_avatar("data:image/png;base64,aW1hZ2U=").is_ok());
        assert!(validate_avatar("not-a-data-url").is_err());
        assert!(validate_avatar("data:text/plain;base64,aW1hZ2U=").is_err());
        let huge = format!("data:image/png;base64,{}", "A".repeat(64 * 1024));
        assert_eq!(
            validate_avatar(&huge).unwrap_err().to_string(),
            "Image too large, pick a smaller one"
        );

        assert_eq!(validate_accent_color(" #e0a34c ").unwrap(), "#e0a34c");
        assert_eq!(validate_accent_color("tomato").unwrap(), "tomato");
        assert_eq!(validate_accent_color("rgb(1,2,3)").unwrap(), "rgb(1,2,3)");
        assert!(validate_accent_color("").is_err());
        assert!(validate_accent_color("red green").is_err());
        assert!(validate_accent_color("0x123456").is_err());
        assert!(validate_accent_color(&"c".repeat(33)).is_err());
    }
}

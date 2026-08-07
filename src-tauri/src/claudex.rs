//! Claudex profiles: the Claude Code harness driven by a non-Anthropic model.
//!
//! A profile is everything needed to point one `claude` child process at an
//! Anthropic-compatible endpoint that is NOT Anthropic: the base URL, the
//! upstream model id, an optional bearer token, extra environment, and an
//! isolated `CLAUDE_CONFIG_DIR` so the bridged sessions never touch (or
//! resume from) the real Claude login's home.
//!
//! The intended default is a local Anthropic⇄Codex bridge (`claude-code-proxy`)
//! talking to a ChatGPT subscription, but nothing here is Codex-specific — any
//! gateway that speaks the Anthropic Messages API works, including OpenRouter.
//!
//! Two deliberate constraints:
//!
//! * **Loopback only for supervised sidecars.** A bridge listener is
//!   unauthenticated by design, and Threadknot itself binds the LAN. Letting a
//!   profile spawn one on a routable address would hand the whole subscription
//!   to anyone on the network, so `sidecar` is refused unless the base URL is
//!   loopback.
//! * **Secrets never leave the server.** `auth_token` and env vars marked
//!   sensitive live on disk (same trust level as `server.json`'s token) and are
//!   replaced by presence flags in [`ClaudexProfile::public`], which is the only
//!   shape clients ever see.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Child;

/// How long to wait for a freshly spawned sidecar to start listening.
const SIDECAR_READY_TIMEOUT: Duration = Duration::from_secs(20);
const SIDECAR_POLL_INTERVAL: Duration = Duration::from_millis(250);
const REACH_TIMEOUT: Duration = Duration::from_millis(1_500);

/// One extra environment variable handed to the bridged `claude` process.
/// `sensitive` only controls wire redaction — the value is stored either way.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub sensitive: bool,
}

/// A local process Threadknot starts on demand so the profile's base URL answers.
/// Optional: point a profile at an already-running bridge and leave this unset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sidecar {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudexProfile {
    pub id: String,
    pub name: String,
    /// Small square data URL used as this profile's picker/sidebar image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Anthropic-compatible endpoint origin, e.g. `http://127.0.0.1:18765`.
    pub base_url: String,
    /// Upstream model id as the gateway names it, e.g. `gpt-5.6-sol`.
    pub model: String,
    /// Model for Claude Code's cheap background calls (titles, topic
    /// detection). Falls back to `model` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
    /// Real upstream context window. Claude's own 200K/1M assumptions do not
    /// apply, and a `[1m]` model suffix is only a client hint — it does not
    /// widen anything upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// Bearer presented as `ANTHROPIC_AUTH_TOKEN`. Often a placeholder: a
    /// subscription-backed bridge authenticates upstream on its own.
    #[serde(default)]
    pub auth_token: String,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<Sidecar>,
    pub created_at: String,
}

impl ClaudexProfile {
    /// Client-facing view. Secret values become presence flags; everything a
    /// settings UI needs to render and re-edit the profile survives.
    pub fn public(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "avatar": self.avatar,
            "baseUrl": self.base_url,
            "model": self.model,
            "smallModel": self.small_model,
            "contextWindow": self.context_window,
            "efforts": self.efforts,
            "defaultEffort": self.default_effort,
            "hasAuthToken": !self.auth_token.is_empty(),
            "env": self.env.iter().map(|v| json!({
                "name": v.name,
                "value": if v.sensitive { Value::Null } else { json!(v.value) },
                "sensitive": v.sensitive,
            })).collect::<Vec<_>>(),
            "sidecar": self.sidecar,
            "createdAt": self.created_at,
        })
    }

    /// Isolated `CLAUDE_CONFIG_DIR` for this profile, under the store dir.
    /// Keeping bridged state out of `~/.claude` is what stops a bridged thread
    /// from resuming (or corrupting) a real Anthropic session's transcript.
    pub fn config_dir(&self, store_dir: &Path) -> PathBuf {
        store_dir.join("claudex").join(&self.id)
    }

    /// Environment overlay for the `claude` child. Profile `env` is applied
    /// last so an advanced user can override any default here.
    pub fn env(&self, store_dir: &Path) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = vec![
            (
                "CLAUDE_CONFIG_DIR".into(),
                self.config_dir(store_dir).to_string_lossy().into_owned(),
            ),
            ("ANTHROPIC_BASE_URL".into(), self.base_url.clone()),
            (
                "ANTHROPIC_AUTH_TOKEN".into(),
                if self.auth_token.is_empty() {
                    "unused".into()
                } else {
                    self.auth_token.clone()
                },
            ),
            // An inherited key would otherwise take precedence over the token
            // above and send this traffic to Anthropic on a real API account.
            ("ANTHROPIC_API_KEY".into(), String::new()),
            // Telemetry/ping traffic has nowhere to go through a gateway.
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()),
            // Bridges stream; the non-streaming retry path usually 404s.
            ("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK".into(), "1".into()),
        ];
        if let Some(small) = &self.small_model {
            env.push(("ANTHROPIC_SMALL_FAST_MODEL".into(), small.clone()));
        }
        if let Some(window) = self.context_window {
            // Compact against the REAL upstream window, not Claude's default
            // assumption for an Anthropic model of the same shape.
            env.push(("CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(), window.to_string()));
        }
        for var in &self.env {
            env.retain(|(name, _)| name != &var.name);
            env.push((var.name.clone(), var.value.clone()));
        }
        env
    }
}

/// Fields a client may set when adding or editing a profile. Absent optional
/// fields mean "leave unchanged" on edit; `auth_token: None` keeps the stored
/// token, which is what lets the UI show a write-only field.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInput {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub small_model: Option<String>,
    pub context_window: Option<u64>,
    pub efforts: Option<Vec<String>>,
    pub default_effort: Option<String>,
    pub auth_token: Option<String>,
    pub env: Option<Vec<EnvVar>>,
    pub sidecar: Option<Option<Sidecar>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ClaudexFile {
    #[serde(default)]
    profiles: Vec<ClaudexProfile>,
}

pub struct ClaudexRegistry {
    path: PathBuf,
    dir: PathBuf,
    profiles: Mutex<Vec<ClaudexProfile>>,
}

impl ClaudexRegistry {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("claudex.json");
        let profiles = if path.exists() {
            let file: ClaudexFile = serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse claudex.json")?;
            file.profiles
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            dir: dir.to_path_buf(),
            profiles: Mutex::new(profiles),
        })
    }

    pub fn store_dir(&self) -> &Path {
        &self.dir
    }

    fn flush(&self, profiles: &[ClaudexProfile]) -> Result<()> {
        let file = ClaudexFile {
            profiles: profiles.to_vec(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn add(&self, input: ProfileInput) -> Result<ClaudexProfile> {
        let name = required(input.name, "name")?;
        let base_url = normalize_base_url(&required(input.base_url, "baseUrl")?)?;
        let model = required(input.model, "model")?;
        // Left blank, ask the provider rather than guessing. A wrong window is
        // not cosmetic: it sets CLAUDE_CODE_AUTO_COMPACT_WINDOW.
        let context_window = input
            .context_window
            .filter(|window| *window > 0)
            .or_else(|| catalog_window(&model));
        let sidecar = input.sidecar.flatten();
        if sidecar.is_some() {
            ensure_loopback(&base_url)?;
        }
        let profile = ClaudexProfile {
            id: crate::protocol::new_id(),
            name,
            avatar: None,
            base_url,
            model,
            small_model: blank_to_none(input.small_model),
            context_window,
            efforts: input.efforts.unwrap_or_default(),
            default_effort: blank_to_none(input.default_effort),
            auth_token: input.auth_token.unwrap_or_default(),
            env: input.env.unwrap_or_default(),
            sidecar,
            created_at: crate::protocol::now_iso(),
        };
        let mut profiles = self.profiles.lock().unwrap();
        anyhow::ensure!(
            !profiles.iter().any(|p| p.name == profile.name),
            "a Claudex profile named \"{}\" already exists",
            profile.name
        );
        profiles.push(profile.clone());
        self.flush(&profiles)?;
        Ok(profile)
    }

    pub fn update(&self, id: &str, input: ProfileInput) -> Result<ClaudexProfile> {
        let mut profiles = self.profiles.lock().unwrap();
        let taken = profiles
            .iter()
            .any(|p| p.id != id && Some(&p.name) == input.name.as_ref());
        anyhow::ensure!(!taken, "another Claudex profile already has that name");
        let profile = profiles
            .iter_mut()
            .find(|p| p.id == id)
            .context("unknown Claudex profile")?;
        if let Some(name) = input.name {
            anyhow::ensure!(!name.trim().is_empty(), "name is required");
            profile.name = name;
        }
        if let Some(base_url) = input.base_url {
            profile.base_url = normalize_base_url(&base_url)?;
        }
        if let Some(model) = input.model {
            anyhow::ensure!(!model.trim().is_empty(), "model is required");
            profile.model = model;
        }
        if let Some(small) = input.small_model {
            profile.small_model = blank_to_none(Some(small));
        }
        // 0 means "blank" from the form — re-derive from the catalog for
        // whatever model the profile now points at.
        if let Some(window) = input.context_window {
            profile.context_window = match window {
                0 => catalog_window(&profile.model),
                window => Some(window),
            };
        }
        if let Some(efforts) = input.efforts {
            profile.efforts = efforts;
        }
        if let Some(effort) = input.default_effort {
            profile.default_effort = blank_to_none(Some(effort));
        }
        // Absent = keep the stored secret; empty string = deliberately clear it.
        if let Some(token) = input.auth_token {
            profile.auth_token = token;
        }
        if let Some(env) = input.env {
            profile.env = env;
        }
        if let Some(sidecar) = input.sidecar {
            profile.sidecar = sidecar;
        }
        if profile.sidecar.is_some() {
            ensure_loopback(&profile.base_url)?;
        }
        let out = profile.clone();
        self.flush(&profiles)?;
        Ok(out)
    }

    pub fn set_avatar(&self, id: &str, avatar: Option<String>) -> Result<ClaudexProfile> {
        let mut profiles = self.profiles.lock().unwrap();
        let profile = profiles
            .iter_mut()
            .find(|p| p.id == id)
            .context("unknown Claudex profile")?;
        profile.avatar = avatar;
        let out = profile.clone();
        self.flush(&profiles)?;
        Ok(out)
    }

    /// Removing a profile also erases its config home. That directory holds the
    /// bridged conversations' transcripts and nothing else reaches them once
    /// the profile is gone, so leaving it behind would accumulate unreachable
    /// chat content forever — same reasoning as signing out a browser profile.
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut profiles = self.profiles.lock().unwrap();
        let before = profiles.len();
        let removed = profiles.iter().find(|p| p.id == id).cloned();
        profiles.retain(|p| p.id != id);
        anyhow::ensure!(profiles.len() < before, "unknown Claudex profile");
        self.flush(&profiles)?;
        if let Some(profile) = removed {
            let dir = profile.config_dir(&self.dir);
            // Best effort: a failure here must not strand the registry edit,
            // which is already durable.
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %dir.display(),
                        %error,
                        "could not erase the removed Claudex profile's config home"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<ClaudexProfile> {
        self.profiles.lock().unwrap().clone()
    }

    pub fn profile(&self, id: &str) -> Option<ClaudexProfile> {
        self.profiles
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }
}

/// Usable context window for a model, from the Codex CLI's own per-account
/// model catalog (`~/.codex/models_cache.json`).
///
/// This is the authoritative number for a Codex-backed bridge and it is NOT
/// the model's advertised API window: `gpt-5.6-sol` is documented at 1.05M
/// tokens, but the Codex product serves it at `context_window: 272000` with
/// `max_context_window: 272000` — the ceiling, not a current setting. (Compare
/// `gpt-5.4`, whose `max_context_window` really is 1000000.) Do not "fix" this
/// to 1M: on the subscription path the request is rejected long before that.
///
/// The catalog also carries `effective_context_window_percent` (95), the share
/// Codex will actually accept. We return the EFFECTIVE window, because that is
/// the point Claude Code must compact by — compacting at the raw 272000 is
/// already ~13.6k tokens too late and the turn fails mid-work.
pub fn catalog_window(model: &str) -> Option<u64> {
    let path = dirs::home_dir()?.join(".codex/models_cache.json");
    let cache: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let entry = cache
        .get("models")?
        .as_array()?
        .iter()
        .find(|m| m.get("slug").and_then(Value::as_str) == Some(model))?;
    let window = entry.get("context_window").and_then(Value::as_u64)?;
    let percent = entry
        .get("effective_context_window_percent")
        .and_then(Value::as_u64)
        .filter(|p| (1..=100).contains(p))
        .unwrap_or(100);
    Some(window * percent / 100)
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    let value = value.unwrap_or_default();
    anyhow::ensure!(!value.trim().is_empty(), "{field} is required");
    Ok(value.trim().to_string())
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Normalize a pasted endpoint to an origin (plus any path prefix), dropping a
/// trailing slash and the `/v1` suffix people copy out of gateway docs —
/// Claude Code appends `/v1/messages` itself.
pub fn normalize_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let url = url::Url::parse(trimmed).context("invalid URL")?;
    anyhow::ensure!(
        url.scheme() == "http" || url.scheme() == "https",
        "URL must be http(s)"
    );
    anyhow::ensure!(url.host_str().is_some(), "URL must include a host");
    Ok(trimmed.to_string())
}

fn is_loopback(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn ensure_loopback(base_url: &str) -> Result<()> {
    anyhow::ensure!(
        is_loopback(base_url),
        "a managed bridge must listen on loopback (127.0.0.1 or localhost) — \
         its listener is unauthenticated, and Threadknot itself is reachable on the LAN"
    );
    Ok(())
}

fn socket_addrs(base_url: &str) -> Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    let url = url::Url::parse(base_url).context("invalid URL")?;
    let host = url.host_str().context("URL must include a host")?;
    let port = url
        .port_or_known_default()
        .context("URL must include a port")?;
    Ok((host, port)
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {host}:{port}"))?
        .collect())
}

/// Is something accepting connections at this base URL right now?
pub async fn is_listening(base_url: &str) -> bool {
    let Ok(addrs) = socket_addrs(base_url) else {
        return false;
    };
    for addr in addrs {
        if tokio::time::timeout(REACH_TIMEOUT, tokio::net::TcpStream::connect(addr))
            .await
            .is_ok_and(|r| r.is_ok())
        {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SidecarState {
    /// Reachable, and not started by us — an externally managed bridge.
    External,
    /// Reachable, running under Threadknot's supervision.
    Managed,
    /// Nothing is listening and no sidecar is configured to start.
    Stopped,
}

/// Starts and owns the bridge processes profiles ask for. A sidecar is keyed by
/// profile id; children are killed on drop, so quitting Threadknot takes the
/// bridges with it.
#[derive(Default)]
pub struct SidecarSupervisor {
    children: tokio::sync::Mutex<HashMap<String, Child>>,
}

impl SidecarSupervisor {
    /// Make sure the profile's base URL answers, starting its sidecar if one is
    /// configured and nothing is listening yet. Returns how that came about.
    pub async fn ensure(&self, profile: &ClaudexProfile) -> Result<SidecarState> {
        let mut children = self.children.lock().await;
        // A previously spawned child that has since exited must not mask a
        // restart attempt.
        if let Some(child) = children.get_mut(&profile.id) {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => {
                    children.remove(&profile.id);
                }
                Ok(None) => {
                    if is_listening(&profile.base_url).await {
                        return Ok(SidecarState::Managed);
                    }
                    children.remove(&profile.id);
                }
            }
        }
        if is_listening(&profile.base_url).await {
            return Ok(SidecarState::External);
        }
        let Some(sidecar) = &profile.sidecar else {
            anyhow::bail!(
                "nothing is listening at {} — start the bridge, or give this profile \
                 a sidecar command so Threadknot can start it",
                profile.base_url
            );
        };
        ensure_loopback(&profile.base_url)?;
        let bin = crate::agents::resolve_bin(&sidecar.command).unwrap_or_else(|| {
            PathBuf::from(&sidecar.command)
        });
        let mut cmd = tokio::process::Command::new(bin);
        cmd.env("PATH", crate::agents::agent_path())
            .args(&sidecar.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::agents::no_console(&mut cmd);
        let mut child = cmd.spawn().with_context(|| {
            format!("failed to start the bridge `{}`", sidecar.command)
        })?;
        // Mirror the sidecar's own diagnostics into our log; without this a
        // bridge that starts and immediately rejects its config is invisible.
        if let Some(stderr) = child.stderr.take() {
            let label = profile.name.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "claudex_sidecar", profile = %label, "{line}");
                }
            });
        }

        let deadline = tokio::time::Instant::now() + SIDECAR_READY_TIMEOUT;
        loop {
            if is_listening(&profile.base_url).await {
                children.insert(profile.id.clone(), child);
                return Ok(SidecarState::Managed);
            }
            if let Ok(Some(status)) = child.try_wait() {
                anyhow::bail!("the bridge `{}` exited with {status}", sidecar.command);
            }
            if tokio::time::Instant::now() >= deadline {
                // `child` drops here, and kill_on_drop reaps the stuck process.
                anyhow::bail!(
                    "the bridge `{}` did not start listening at {} within {}s",
                    sidecar.command,
                    profile.base_url,
                    SIDECAR_READY_TIMEOUT.as_secs()
                );
            }
            tokio::time::sleep(SIDECAR_POLL_INTERVAL).await;
        }
    }

    /// Stop the sidecar we started for this profile, if any.
    pub async fn stop(&self, profile_id: &str) {
        if let Some(mut child) = self.children.lock().await.remove(profile_id) {
            let _ = child.kill().await;
        }
    }

    /// Current state without starting anything.
    pub async fn status(&self, profile: &ClaudexProfile) -> SidecarState {
        let managed = self.children.lock().await.contains_key(&profile.id);
        if !is_listening(&profile.base_url).await {
            return SidecarState::Stopped;
        }
        if managed {
            SidecarState::Managed
        } else {
            SidecarState::External
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, url: &str) -> ProfileInput {
        ProfileInput {
            name: Some(name.into()),
            base_url: Some(url.into()),
            model: Some("gpt-5.6-sol".into()),
            ..Default::default()
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("threadknot-claudex-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_update_remove_roundtrip_and_secrets_never_leak() {
        let dir = temp_dir("crud");
        let reg = ClaudexRegistry::open(&dir).unwrap();
        let mut add = input("Sol", "http://127.0.0.1:18765/v1");
        add.auth_token = Some("super-secret".into());
        add.env = Some(vec![EnvVar {
            name: "CCP_CODEX_TRANSPORT".into(),
            value: "http".into(),
            sensitive: false,
        }]);
        let p = reg.add(add).unwrap();
        // The pasted `/v1` suffix is dropped — Claude Code appends it itself.
        assert_eq!(p.base_url, "http://127.0.0.1:18765");

        // Duplicate names are rejected.
        assert!(reg.add(input("Sol", "http://127.0.0.1:19000")).is_err());

        let public = serde_json::to_string(&p.public()).unwrap();
        assert!(!public.contains("super-secret"));
        assert!(public.contains("\"hasAuthToken\":true"));
        // Non-sensitive env stays visible so the UI can render it.
        assert!(public.contains("CCP_CODEX_TRANSPORT"));

        // An omitted token on edit keeps the stored one.
        let edited = reg
            .update(
                &p.id,
                ProfileInput {
                    model: Some("gpt-5.6-terra".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(edited.model, "gpt-5.6-terra");
        assert_eq!(edited.auth_token, "super-secret");

        let reloaded = ClaudexRegistry::open(&dir).unwrap();
        assert_eq!(reloaded.profile(&p.id).unwrap().auth_token, "super-secret");

        // Removal takes the profile's transcripts with it — nothing else can
        // reach them once the profile is gone.
        let home = p.config_dir(&dir);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        assert!(home.exists());
        reloaded.remove(&p.id).unwrap();
        assert!(!home.exists());
        assert!(reloaded.remove(&p.id).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sensitive_env_values_are_redacted_but_kept_on_disk() {
        let dir = temp_dir("redact");
        let reg = ClaudexRegistry::open(&dir).unwrap();
        let mut add = input("Router", "https://openrouter.ai/api");
        add.env = Some(vec![EnvVar {
            name: "OPENROUTER_KEY".into(),
            value: "sk-live".into(),
            sensitive: true,
        }]);
        let p = reg.add(add).unwrap();
        let public = serde_json::to_string(&p.public()).unwrap();
        assert!(!public.contains("sk-live"));
        assert!(public.contains("OPENROUTER_KEY"));
        assert_eq!(
            ClaudexRegistry::open(&dir).unwrap().profile(&p.id).unwrap().env[0].value,
            "sk-live"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_managed_sidecar_must_be_loopback() {
        let dir = temp_dir("loopback");
        let reg = ClaudexRegistry::open(&dir).unwrap();
        let sidecar = Some(Sidecar {
            command: "claude-code-proxy".into(),
            args: vec!["serve".into()],
        });
        let mut lan = input("LAN bridge", "http://192.168.0.54:18765");
        lan.sidecar = Some(sidecar.clone());
        assert!(reg.add(lan).is_err());

        let mut local = input("Local bridge", "http://127.0.0.1:18765");
        local.sidecar = Some(sidecar);
        let p = reg.add(local).unwrap();
        // …and it cannot be moved onto the LAN afterwards either.
        assert!(reg
            .update(
                &p.id,
                ProfileInput {
                    base_url: Some("http://192.168.0.54:18765".into()),
                    ..Default::default()
                },
            )
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn env_overlay_isolates_the_config_home_and_neutralizes_api_keys() {
        let dir = temp_dir("env");
        let reg = ClaudexRegistry::open(&dir).unwrap();
        let mut add = input("Sol", "http://127.0.0.1:18765");
        add.small_model = Some("gpt-5.6-luna".into());
        add.context_window = Some(272_000);
        // A user override wins over our default for the same variable.
        add.env = Some(vec![EnvVar {
            name: "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            value: "0".into(),
            sensitive: false,
        }]);
        let p = reg.add(add).unwrap();
        let env: HashMap<String, String> = p.env(&dir).into_iter().collect();

        assert_eq!(
            env["CLAUDE_CONFIG_DIR"],
            dir.join("claudex").join(&p.id).to_string_lossy()
        );
        assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:18765");
        assert_eq!(env["ANTHROPIC_API_KEY"], "");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "unused");
        assert_eq!(env["ANTHROPIC_SMALL_FAST_MODEL"], "gpt-5.6-luna");
        assert_eq!(env["CLAUDE_CODE_AUTO_COMPACT_WINDOW"], "272000");
        assert_eq!(env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "0");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The window is not cosmetic — it becomes CLAUDE_CODE_AUTO_COMPACT_WINDOW.
    /// A blank field must resolve to something, and an explicit value must
    /// never be second-guessed.
    #[test]
    fn a_blank_window_is_detected_and_an_explicit_one_is_respected() {
        let dir = temp_dir("window");
        let reg = ClaudexRegistry::open(&dir).unwrap();

        // Explicit wins, always.
        let mut explicit = input("Explicit", "http://127.0.0.1:18765");
        explicit.context_window = Some(500_000);
        assert_eq!(
            reg.add(explicit).unwrap().context_window,
            Some(500_000)
        );

        // Blank (absent or 0) defers to the catalog. Whether this machine has
        // one is environmental, so assert the two coherent outcomes rather
        // than a number: either it matched the catalog, or it stayed unset and
        // the driver falls back to Claude's conservative default.
        let detected = reg.add(input("Detected", "http://127.0.0.1:18766")).unwrap();
        assert_eq!(detected.context_window, catalog_window("gpt-5.6-sol"));

        let mut zeroed = input("Zeroed", "http://127.0.0.1:18767");
        zeroed.context_window = Some(0);
        assert_eq!(
            reg.add(zeroed).unwrap().context_window,
            catalog_window("gpt-5.6-sol")
        );

        // Clearing on edit re-detects against the model it now points at.
        let cleared = reg
            .update(
                &detected.id,
                ProfileInput {
                    model: Some("gpt-5.6-luna".into()),
                    context_window: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared.context_window, catalog_window("gpt-5.6-luna"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Guards the number itself. `gpt-5.6-sol` is documented at 1.05M tokens,
    /// but Codex serves it at 272000 with `max_context_window` also 272000 —
    /// so 1M is not a setting away, it is unavailable on this path. The 95%
    /// effective policy is what Codex will actually accept.
    #[test]
    fn catalog_window_reports_the_effective_not_advertised_window() {
        let Some(sol) = catalog_window("gpt-5.6-sol") else {
            return; // no Codex catalog on this machine
        };
        assert_eq!(sol, 258_400, "272000 raw x 95% effective");
        assert!(sol < 1_000_000, "the 1.05M figure is the API window, not Codex's");
        assert_eq!(catalog_window("definitely-not-a-model"), None);
    }

    #[test]
    fn rejects_unusable_urls() {
        assert!(normalize_base_url("ftp://x").is_err());
        assert!(normalize_base_url("not a url").is_err());
        assert_eq!(
            normalize_base_url(" http://127.0.0.1:18765/ ").unwrap(),
            "http://127.0.0.1:18765"
        );
        assert!(is_loopback("http://localhost:18765"));
        assert!(is_loopback("http://[::1]:18765"));
        assert!(!is_loopback("http://192.168.0.54:18765"));
    }
}

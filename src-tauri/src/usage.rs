//! Per-provider subscription usage (the sidebar meter).
//!
//! - **Claude**: GET `https://api.anthropic.com/api/oauth/usage` with the OAuth
//!   token Claude Code itself stores — the login Keychain on macOS,
//!   `~/.claude/.credentials.json` everywhere else (see `claude_oauth`) — which
//!   is the same endpoint the CLI's `/usage` command hits. NOTE (learned from
//!   Traycer): this endpoint can 429 with a multi-minute penalty window, so
//!   refreshes are floored (see `KICK_FLOOR`) and the poll interval is long.
//! - **Codex**: short-lived `codex app-server` probe speaking its JSON-RPC
//!   (`account/rateLimits/read`). Live sessions also push
//!   `account/rateLimits/updated`, which the codex driver feeds back into the
//!   hub cache for free mid-turn freshness.
//! - **Kimi**: GET `https://api.kimi.com/coding/v1/usages` with the managed
//!   Kimi Code OAuth login from `~/.kimi-code/credentials/kimi-code.json`.
//!   Access-token refresh follows the official CLI's device OAuth flow and
//!   shares its cross-process refresh lock.
//!
//! Snapshots are cached on the hub and pushed to every client as a `usage`
//! frame; `usage.get` returns the cache, `usage.refresh` forces a re-fetch.

use crate::agents::Hub;
use crate::protocol::{now_iso, Agent, ProviderUsage, RateWindow, ServerMessage};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

/// Full re-poll interval (Traycer uses 15 min for the subprocess lane).
const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Floor between kick-triggered refreshes (turn end, client ask).
const KICK_FLOOR: Duration = Duration::from_secs(120);
/// Settle time after a kick before fetching, so a burst of turn-ends coalesces.
const KICK_DEBOUNCE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct UsageState {
    data: Mutex<HashMap<Agent, ProviderUsage>>,
    kick: Notify,
    force: AtomicBool,
}

impl UsageState {
    pub fn snapshot(&self) -> Vec<ProviderUsage> {
        let map = self.data.lock().unwrap();
        // Stable order: Claude, Codex, then Kimi.
        let mut out = Vec::new();
        for a in [Agent::Claude, Agent::Codex, Agent::Kimi] {
            if let Some(u) = map.get(&a) {
                out.push(u.clone());
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.data.lock().unwrap().is_empty()
    }

    /// Ask the poller to refresh soon (subject to the floor unless `force`).
    pub fn kick(&self, force: bool) {
        if force {
            self.force.store(true, Ordering::Relaxed);
        }
        self.kick.notify_one();
    }

    fn store(&self, u: ProviderUsage) {
        self.data.lock().unwrap().insert(u.agent, u);
    }
}

/// Store a snapshot (e.g. from a live codex session's rate-limit notification)
/// and push the new state to all clients. Rolling updates are sparse — keep the
/// previously observed plan if the new snapshot lacks one.
pub fn publish(hub: &Hub, mut usage: ProviderUsage) {
    if usage.plan.is_none() {
        usage.plan = hub
            .usage
            .data
            .lock()
            .unwrap()
            .get(&usage.agent)
            .and_then(|prev| prev.plan.clone());
    }
    hub.usage.store(usage);
    let _ = hub.broadcast.send(ServerMessage::Usage {
        usage: hub.usage.snapshot(),
    });
}

pub fn spawn_poller(hub: Arc<Hub>) {
    tokio::spawn(async move {
        let mut last_fetch: HashMap<Agent, Instant> = HashMap::new();
        loop {
            let force = hub.usage.force.swap(false, Ordering::Relaxed);
            let now = Instant::now();
            let due = |a: Agent, last: &HashMap<Agent, Instant>| {
                force
                    || last
                        .get(&a)
                        .map(|t| now.duration_since(*t) >= KICK_FLOOR)
                        .unwrap_or(true)
            };

            let claude_due = due(Agent::Claude, &last_fetch);
            let codex_due = due(Agent::Codex, &last_fetch);
            let kimi_due = due(Agent::Kimi, &last_fetch);
            let (c, x, k) = tokio::join!(
                async {
                    if claude_due {
                        Some(fetch_claude().await)
                    } else {
                        None
                    }
                },
                async {
                    if codex_due {
                        Some(fetch_codex().await)
                    } else {
                        None
                    }
                },
                async {
                    if kimi_due {
                        Some(fetch_kimi().await)
                    } else {
                        None
                    }
                },
            );
            let mut changed = false;
            for u in [c, x, k].into_iter().flatten() {
                // Keep the last good snapshot instead of clobbering it with a
                // transient failure (Traycer's "retain last-good" rule).
                let keep_last_good = !u.available
                    && hub
                        .usage
                        .data
                        .lock()
                        .unwrap()
                        .get(&u.agent)
                        .map(|prev| prev.available)
                        .unwrap_or(false);
                last_fetch.insert(u.agent, Instant::now());
                if !keep_last_good {
                    hub.usage.store(u);
                    changed = true;
                }
            }
            if changed {
                let _ = hub.broadcast.send(ServerMessage::Usage {
                    usage: hub.usage.snapshot(),
                });
            }

            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = hub.usage.kick.notified() => {
                    tokio::time::sleep(KICK_DEBOUNCE).await;
                }
            }
        }
    });
}

fn unavailable(agent: Agent, error: impl Into<String>) -> ProviderUsage {
    ProviderUsage {
        agent,
        available: false,
        plan: None,
        windows: Vec::new(),
        error: Some(error.into()),
        fetched_at: now_iso(),
    }
}

fn window_label(mins: Option<u64>) -> String {
    match mins {
        Some(m) if m <= 360 => "5h".into(),
        Some(10080) => "Week".into(),
        Some(m) if m % 1440 == 0 => format!("{}d", m / 1440),
        Some(m) => format!("{}h", m.div_ceil(60)),
        None => "Window".into(),
    }
}

// ---- Claude -----------------------------------------------------------------

/// Keychain service name Claude Code stores its credentials under on macOS.
#[cfg(target_os = "macos")]
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// Ceiling on the Keychain read. It normally returns in milliseconds; the
/// budget is for the case where macOS puts an unlock or allow-access dialog in
/// front of it, which would otherwise stall this poll (and Codex's and Kimi's,
/// which fetch in the same `join!`) for as long as the user ignores it.
#[cfg(target_os = "macos")]
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Read the `claudeAiOauth` block for the signed-in subscription.
///
/// macOS moved this out of `~/.claude/.credentials.json` and into the login
/// Keychain, leaving a file behind that holds only `mcpOAuth` — so a Mac with a
/// perfectly good Max login reads as logged out if the file is all you look at.
/// Both are checked, Keychain first. Agent turns were never affected: those
/// spawn the `claude` CLI, which finds its own token either way.
async fn claude_oauth() -> Result<Value, String> {
    // Distinguishes "no credentials anywhere" (never logged in) from "found
    // credentials, but no subscription token in them" (logged out, or a
    // file that only carries MCP tokens).
    let mut saw_credentials = false;

    #[cfg(target_os = "macos")]
    {
        if let Some(raw) = keychain_credentials().await {
            saw_credentials = true;
            if let Some(oauth) = oauth_block(&raw) {
                return Ok(oauth);
            }
        }
    }

    let path = match dirs::home_dir() {
        Some(h) => h.join(".claude/.credentials.json"),
        None => return Err("no home directory".into()),
    };
    if let Ok(raw) = std::fs::read_to_string(&path) {
        saw_credentials = true;
        if let Some(oauth) = oauth_block(&raw) {
            return Ok(oauth);
        }
    }

    Err(if saw_credentials {
        "not logged in (no Claude OAuth token)".into()
    } else {
        "not logged in (no Claude credentials)".into()
    })
}

/// The `claudeAiOauth` object out of a credentials blob, only if it actually
/// carries an access token.
fn oauth_block(raw: &str) -> Option<Value> {
    let creds: Value = serde_json::from_str(raw).ok()?;
    let oauth = creds.get("claudeAiOauth")?;
    oauth.get("accessToken")?.as_str()?;
    Some(oauth.clone())
}

/// Claude Code's Keychain item, via `/usr/bin/security`.
///
/// Shelling out rather than calling Security.framework directly is deliberate:
/// the item's ACL trusts the `security` binary, so this reads clean, whereas a
/// call from inside this app is a different executable than the one that stored
/// the item and raises the "wants to use your confidential information" prompt.
#[cfg(target_os = "macos")]
async fn keychain_credentials() -> Option<String> {
    let mut cmd = tokio::process::Command::new("/usr/bin/security");
    cmd.args([
        "find-generic-password",
        "-s",
        CLAUDE_KEYCHAIN_SERVICE,
        "-w",
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    // A timed-out read leaves a dialog on screen; dropping the child dismisses it.
    .kill_on_drop(true);
    crate::agents::no_console(&mut cmd);

    // Timeout: a prompt is waiting on the user. Error: no `security` binary.
    let out = tokio::time::timeout(KEYCHAIN_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None; // no such item — never logged in on this machine
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

async fn fetch_claude() -> ProviderUsage {
    let agent = Agent::Claude;
    let oauth = match claude_oauth().await {
        Ok(v) => v,
        Err(e) => return unavailable(agent, e),
    };
    let Some(token) = oauth["accessToken"].as_str() else {
        return unavailable(agent, "not logged in (no Claude OAuth token)");
    };
    let plan = oauth["subscriptionType"].as_str().map(titlecase);

    // curl with the config on stdin so the token never appears in argv.
    let cfg = format!(
        "url = \"https://api.anthropic.com/api/oauth/usage\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"anthropic-beta: oauth-2025-04-20\"\n\
         silent\nfail\nmax-time = 15\n"
    );
    let mut curl = tokio::process::Command::new("curl");
    curl.args(["-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    crate::agents::no_console(&mut curl);
    let mut child = match curl.spawn() {
        Ok(c) => c,
        Err(e) => return unavailable(agent, format!("curl unavailable: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(cfg.as_bytes()).await;
    }
    let out = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => return unavailable(agent, format!("usage fetch failed: {e}")),
    };
    if !out.status.success() {
        return unavailable(agent, "usage fetch failed (login expired or rate-limited)");
    }
    let v: Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return unavailable(agent, "unexpected usage response"),
    };

    let windows = parse_claude_windows(&v);
    if windows.is_empty() {
        return unavailable(agent, "no usage windows in response");
    }
    ProviderUsage {
        agent,
        available: true,
        plan,
        windows,
        error: None,
        fetched_at: now_iso(),
    }
}

fn parse_claude_windows(v: &Value) -> Vec<RateWindow> {
    let mut windows = Vec::new();

    // Current Claude Code releases receive the normalized list rendered by
    // `/usage`. Model-scoped weekly limits such as Fable live only here.
    if let Some(limits) = v["limits"].as_array() {
        for limit in limits {
            let Some(pct) = limit["percent"].as_f64() else {
                continue;
            };
            let (label, mins) = match limit["kind"].as_str() {
                Some("session") => ("5h".to_string(), 300),
                Some("weekly_all") => ("Week".to_string(), 10080),
                Some("weekly_scoped") => {
                    let Some(name) = limit
                        .pointer("/scope/model/display_name")
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    (name.to_string(), 10080)
                }
                _ => continue,
            };
            windows.push(RateWindow {
                label,
                used_percent: pct,
                resets_at: limit["resets_at"].as_str().map(String::from),
                window_mins: Some(mins),
            });
        }
    }

    if !windows.is_empty() {
        return windows;
    }

    // Backward compatibility with older endpoint responses.
    for (label, key) in [("5h", "five_hour"), ("Week", "seven_day")] {
        if let Some(pct) = v[key]["utilization"].as_f64() {
            windows.push(RateWindow {
                label: label.into(),
                used_percent: pct,
                resets_at: v[key]["resets_at"].as_str().map(String::from),
                window_mins: Some(if label == "5h" { 300 } else { 10080 }),
            });
        }
    }
    windows
}

fn titlecase(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---- Kimi -------------------------------------------------------------------

const KIMI_USAGE_BASE_URL: &str = "https://api.kimi.com/coding/v1";
const KIMI_OAUTH_HOST: &str = "https://auth.kimi.com";
const KIMI_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";

struct KimiRefreshLock {
    path: PathBuf,
}

impl Drop for KimiRefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

fn kimi_home() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("KIMI_CODE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    dirs::home_dir()
        .map(|home| home.join(".kimi-code"))
        .ok_or_else(|| "no home directory".into())
}

fn read_kimi_credentials(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|_| "not logged in (no Kimi credentials)".to_string())?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|_| "unreadable Kimi credentials".to_string())?;
    if !value.is_object() {
        return Err("unreadable Kimi credentials".into());
    }
    Ok(value)
}

fn kimi_token_is_fresh(credentials: &Value) -> bool {
    let Some(token) = credentials["access_token"].as_str() else {
        return false;
    };
    if token.is_empty() {
        return false;
    }
    let Some(expires_at) = credentials["expires_at"].as_i64() else {
        return true;
    };
    if expires_at == 0 {
        return true;
    }
    let expires_in = credentials["expires_in"].as_i64().unwrap_or(0);
    let refresh_threshold = std::cmp::max(300, expires_in / 2);
    expires_at - chrono::Utc::now().timestamp() >= refresh_threshold
}

async fn acquire_kimi_refresh_lock(home: &Path) -> Result<KimiRefreshLock, String> {
    let oauth_dir = home.join("oauth");
    std::fs::create_dir_all(&oauth_dir)
        .map_err(|e| format!("cannot prepare Kimi OAuth lock: {e}"))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(oauth_dir.join("kimi-code"))
        .map_err(|e| format!("cannot prepare Kimi OAuth lock: {e}"))?;

    // Kimi's official CLI uses proper-lockfile on the sentinel above, whose
    // actual lock is this sibling directory. Sharing it avoids rotating the
    // same one-time refresh token from two processes at once.
    let lock_path = oauth_dir.join("kimi-code.lock");
    for _ in 0..60 {
        match std::fs::create_dir(&lock_path) {
            Ok(()) => return Ok(KimiRefreshLock { path: lock_path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock_path)
                    .and_then(|m| m.modified())
                    .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
                    .map(|age| age > Duration::from_secs(10))
                    .unwrap_or(false);
                if stale && std::fs::remove_dir(&lock_path).is_ok() {
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => return Err(format!("cannot acquire Kimi OAuth lock: {error}")),
        }
    }
    Err("timed out waiting for Kimi OAuth refresh lock".into())
}

fn write_kimi_credentials(path: &Path, credentials: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "invalid Kimi credentials path".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot prepare Kimi credentials: {e}"))?;
    let tmp = parent.join(format!(
        ".kimi-code.json.tmp-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|e| format!("cannot update Kimi credentials: {e}"))?;
    let mut bytes = serde_json::to_vec_pretty(credentials)
        .map_err(|e| format!("cannot encode Kimi credentials: {e}"))?;
    bytes.push(b'\n');
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot update Kimi credentials: {error}"));
    }
    drop(file);
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot update Kimi credentials: {error}"));
    }
    Ok(())
}

async fn kimi_access_token(
    client: &reqwest::Client,
    home: &Path,
    rejected_token: Option<&str>,
) -> Result<String, String> {
    let path = home.join("credentials/kimi-code.json");
    let initial = read_kimi_credentials(&path)?;
    let initial_token = initial["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "not logged in (no Kimi OAuth token)".to_string())?;
    if kimi_token_is_fresh(&initial)
        && rejected_token
            .map(|rejected| rejected != initial_token)
            .unwrap_or(true)
    {
        return Ok(initial_token.to_string());
    }

    let _lock = acquire_kimi_refresh_lock(home).await?;
    let mut credentials = read_kimi_credentials(&path)?;
    let current_token = credentials["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "not logged in (no Kimi OAuth token)".to_string())?;
    let peer_refreshed = rejected_token
        .map(|rejected| rejected != current_token)
        .unwrap_or(false);
    if kimi_token_is_fresh(&credentials) && (rejected_token.is_none() || peer_refreshed) {
        return Ok(current_token.to_string());
    }

    let refresh_token = credentials["refresh_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Kimi login has no refresh token; run `kimi login`".to_string())?;
    let oauth_host = std::env::var("KIMI_CODE_OAUTH_HOST")
        .or_else(|_| std::env::var("KIMI_OAUTH_HOST"))
        .unwrap_or_else(|_| KIMI_OAUTH_HOST.into())
        .trim_end_matches('/')
        .to_string();
    let body = format!(
        "client_id={}&grant_type=refresh_token&refresh_token={}",
        urlencoding::encode(KIMI_CLIENT_ID),
        urlencoding::encode(refresh_token)
    );
    let response = client
        .post(format!("{oauth_host}/api/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Kimi OAuth refresh failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Kimi OAuth refresh rejected (HTTP {}); run `kimi login`",
            response.status().as_u16()
        ));
    }
    let refreshed: Value = response
        .json()
        .await
        .map_err(|_| "unexpected Kimi OAuth refresh response".to_string())?;
    let access_token = refreshed["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Kimi OAuth refresh returned no access token".to_string())?;
    let new_refresh_token = refreshed["refresh_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Kimi OAuth refresh returned no refresh token".to_string())?;
    let expires_in = kimi_number(&refreshed["expires_in"])
        .filter(|n| *n > 0.0)
        .ok_or_else(|| "Kimi OAuth refresh returned no expiry".to_string())?
        as i64;

    let object = credentials
        .as_object_mut()
        .ok_or_else(|| "unreadable Kimi credentials".to_string())?;
    object.insert("access_token".into(), Value::String(access_token.into()));
    object.insert(
        "refresh_token".into(),
        Value::String(new_refresh_token.into()),
    );
    object.insert("expires_in".into(), Value::from(expires_in));
    object.insert(
        "expires_at".into(),
        Value::from(chrono::Utc::now().timestamp() + expires_in),
    );
    if let Some(scope) = refreshed["scope"].as_str() {
        object.insert("scope".into(), Value::String(scope.into()));
    }
    object.insert(
        "token_type".into(),
        Value::String(
            refreshed["token_type"]
                .as_str()
                .unwrap_or("Bearer")
                .into(),
        ),
    );
    let access_token = access_token.to_string();
    write_kimi_credentials(&path, &credentials)?;
    Ok(access_token)
}

fn kimi_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .filter(|number| number.is_finite())
}

fn kimi_reset_at(row: &Value) -> Option<String> {
    for key in ["reset_at", "resetAt", "reset_time", "resetTime"] {
        if let Some(value) = row[key].as_str().filter(|value| !value.is_empty()) {
            return Some(value.into());
        }
    }
    for key in ["reset_in", "resetIn", "ttl", "window"] {
        if let Some(seconds) = kimi_number(&row[key]).filter(|seconds| *seconds > 0.0) {
            return Some(
                (chrono::Utc::now() + chrono::Duration::seconds(seconds as i64))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            );
        }
    }
    None
}

fn kimi_window_mins(label: &str) -> Option<u64> {
    let lower = label.to_ascii_lowercase();
    if lower.contains("week") {
        return Some(10_080);
    }
    if lower.contains("5h") {
        return Some(300);
    }
    None
}

fn kimi_usage_row(row: &Value, label: String, window_mins: Option<u64>) -> Option<RateWindow> {
    let limit = kimi_number(&row["limit"]);
    let used = kimi_number(&row["used"]).or_else(|| {
        let remaining = kimi_number(&row["remaining"])?;
        Some(limit? - remaining)
    });
    if used.is_none() && limit.is_none() {
        return None;
    }
    let used_percent = match (used, limit) {
        (Some(used), Some(limit)) if limit > 0.0 => (used / limit * 100.0).clamp(0.0, 100.0),
        _ => 0.0,
    };
    Some(RateWindow {
        window_mins: window_mins.or_else(|| kimi_window_mins(&label)),
        label,
        used_percent,
        resets_at: kimi_reset_at(row),
    })
}

fn parse_kimi_windows(value: &Value) -> Vec<RateWindow> {
    let mut windows = Vec::new();
    let summary = &value["usage"];
    if summary.is_object() {
        let label = summary["name"]
            .as_str()
            .or_else(|| summary["title"].as_str())
            .unwrap_or("Weekly limit")
            .to_string();
        if let Some(window) = kimi_usage_row(summary, label, None) {
            windows.push(window);
        }
    }

    if let Some(limits) = value["limits"].as_array() {
        for (index, item) in limits.iter().enumerate() {
            if !item.is_object() {
                continue;
            }
            let detail = if item["detail"].is_object() {
                &item["detail"]
            } else {
                item
            };
            let duration = kimi_number(
                item["window"]
                    .get("duration")
                    .or_else(|| item.get("duration"))
                    .or_else(|| detail.get("duration"))
                    .unwrap_or(&Value::Null),
            )
            .filter(|duration| *duration >= 0.0)
            .map(|duration| duration as u64);
            let time_unit = item["window"]
                .get("timeUnit")
                .or_else(|| item.get("timeUnit"))
                .or_else(|| detail.get("timeUnit"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let window_mins = duration.map(|duration| {
                if time_unit.contains("MINUTE") {
                    duration
                } else if time_unit.contains("HOUR") {
                    duration * 60
                } else if time_unit.contains("DAY") {
                    duration * 1_440
                } else {
                    duration.div_ceil(60)
                }
            });
            let explicit_label = ["name", "title", "scope"]
                .iter()
                .find_map(|key| {
                    item[*key]
                        .as_str()
                        .or_else(|| detail[*key].as_str())
                        .filter(|value| !value.is_empty())
                })
                .map(String::from);
            let label = explicit_label.unwrap_or_else(|| match (duration, time_unit) {
                (Some(duration), unit) if unit.contains("MINUTE") && duration >= 60 && duration % 60 == 0 => {
                    format!("{}h limit", duration / 60)
                }
                (Some(duration), unit) if unit.contains("MINUTE") => format!("{duration}m limit"),
                (Some(duration), unit) if unit.contains("HOUR") => format!("{duration}h limit"),
                (Some(duration), unit) if unit.contains("DAY") => format!("{duration}d limit"),
                (Some(duration), _) => format!("{duration}s limit"),
                (None, _) => format!("Limit #{}", index + 1),
            });
            if let Some(window) = kimi_usage_row(detail, label, window_mins) {
                windows.push(window);
            }
        }
    }
    windows
}

async fn request_kimi_usage(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<reqwest::Response, String> {
    client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Kimi usage fetch failed: {e}"))
}

async fn fetch_kimi() -> ProviderUsage {
    match tokio::time::timeout(Duration::from_secs(35), fetch_kimi_inner()).await {
        Ok(result) => result,
        Err(_) => unavailable(Agent::Kimi, "Kimi usage probe timed out"),
    }
}

async fn fetch_kimi_inner() -> ProviderUsage {
    let agent = Agent::Kimi;
    let home = match kimi_home() {
        Ok(home) => home,
        Err(error) => return unavailable(agent, error),
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => return unavailable(agent, format!("Kimi HTTP client failed: {error}")),
    };
    let mut token = match kimi_access_token(&client, &home, None).await {
        Ok(token) => token,
        Err(error) => return unavailable(agent, error),
    };
    let base_url = std::env::var("KIMI_CODE_BASE_URL")
        .unwrap_or_else(|_| KIMI_USAGE_BASE_URL.into())
        .trim_end_matches('/')
        .to_string();
    let url = format!("{base_url}/usages");
    let mut response = match request_kimi_usage(&client, &url, &token).await {
        Ok(response) => response,
        Err(error) => return unavailable(agent, error),
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        token = match kimi_access_token(&client, &home, Some(&token)).await {
            Ok(token) => token,
            Err(error) => return unavailable(agent, error),
        };
        response = match request_kimi_usage(&client, &url, &token).await {
            Ok(response) => response,
            Err(error) => return unavailable(agent, error),
        };
    }
    if !response.status().is_success() {
        return unavailable(
            agent,
            format!("Kimi usage fetch failed (HTTP {})", response.status().as_u16()),
        );
    }
    let value: Value = match response.json().await {
        Ok(value) => value,
        Err(_) => return unavailable(agent, "unexpected Kimi usage response"),
    };
    let windows = parse_kimi_windows(&value);
    if windows.is_empty() {
        return unavailable(agent, "no Kimi usage windows in response");
    }
    ProviderUsage {
        agent,
        available: true,
        plan: None,
        windows,
        error: None,
        fetched_at: now_iso(),
    }
}

// ---- Codex ------------------------------------------------------------------

/// Parse the `rateLimits` object shared by `account/rateLimits/read` responses
/// and `account/rateLimits/updated` notifications.
pub fn parse_codex_rate_limits(rate_limits: &Value) -> Option<ProviderUsage> {
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let w = &rate_limits[key];
        if let Some(pct) = w["usedPercent"].as_f64() {
            let mins = w["windowDurationMins"].as_u64();
            windows.push(RateWindow {
                label: window_label(mins),
                used_percent: pct,
                resets_at: w["resetsAt"]
                    .as_i64()
                    .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                window_mins: mins,
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(ProviderUsage {
        agent: Agent::Codex,
        available: true,
        plan: rate_limits["planType"].as_str().map(titlecase),
        windows,
        error: None,
        fetched_at: now_iso(),
    })
}

async fn fetch_codex() -> ProviderUsage {
    match tokio::time::timeout(Duration::from_secs(25), fetch_codex_inner()).await {
        Ok(r) => r,
        Err(_) => unavailable(Agent::Codex, "codex probe timed out"),
    }
}

async fn fetch_codex_inner() -> ProviderUsage {
    let agent = Agent::Codex;
    let Some(bin) = crate::agents::resolve_bin("codex") else {
        return unavailable(agent, "codex CLI not found");
    };
    let mut codex = tokio::process::Command::new(&bin);
    codex
        .arg("app-server")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::agents::no_console(&mut codex);
    let mut child = match codex.spawn() {
        Ok(c) => c,
        Err(e) => return unavailable(agent, format!("codex spawn failed: {e}")),
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();

    let init = json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "threadknot", "title": "Threadknot", "version": env!("CARGO_PKG_VERSION") },
        },
    });
    let read = json!({ "id": 2, "method": "account/rateLimits/read", "params": {} });
    let send = async {
        stdin.write_all(init.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin
            .write_all(json!({ "method": "initialized" }).to_string().as_bytes())
            .await?;
        stdin.write_all(b"\n").await?;
        stdin.write_all(read.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    };
    if let Err(e) = send.await {
        return unavailable(agent, format!("codex probe write failed: {e}"));
    }

    let result = loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v["id"].as_i64() == Some(2) {
                    if let Some(err) = v.get("error") {
                        break Err(format!(
                            "codex: {}",
                            err["message"].as_str().unwrap_or("error")
                        ));
                    }
                    break Ok(v["result"].clone());
                }
            }
            Ok(None) => break Err("codex app-server exited early".into()),
            Err(e) => break Err(format!("codex probe read failed: {e}")),
        }
    };
    let _ = child.kill().await;

    match result {
        Ok(res) => parse_codex_rate_limits(&res["rateLimits"])
            .unwrap_or_else(|| unavailable(agent, "no rate-limit windows in response")),
        Err(e) => unavailable(agent, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_limits_include_model_scoped_windows() {
        let response = json!({
            "limits": [
                { "kind": "session", "percent": 15.0, "resets_at": "2026-07-22T16:00:00Z" },
                { "kind": "weekly_all", "percent": 77.0, "resets_at": "2026-07-22T15:00:00Z" },
                {
                    "kind": "weekly_scoped",
                    "percent": 83.0,
                    "resets_at": "2026-07-22T15:00:00Z",
                    "scope": { "model": { "display_name": "Fable" } }
                }
            ]
        });

        let windows = parse_claude_windows(&response);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "5h");
        assert_eq!(windows[1].label, "Week");
        assert_eq!(windows[2].label, "Fable");
        assert_eq!(windows[2].used_percent, 83.0);
        assert_eq!(windows[2].window_mins, Some(10080));
    }

    /// macOS leaves an MCP-only credentials file next to the Keychain item that
    /// holds the real subscription token. Treating that file as a login is what
    /// made a signed-in Max account report "not logged in".
    #[test]
    fn claude_mcp_only_credentials_are_not_a_login() {
        let mcp_only = json!({
            "mcpOAuth": { "plugin:vercel:vercel|abc": { "serverName": "vercel" } }
        })
        .to_string();
        assert!(oauth_block(&mcp_only).is_none());

        let logged_in = json!({
            "mcpOAuth": {},
            "claudeAiOauth": { "accessToken": "sk-test", "subscriptionType": "max" }
        })
        .to_string();
        let oauth = oauth_block(&logged_in).expect("subscription token is a login");
        assert_eq!(oauth["subscriptionType"], "max");

        // A token-less block is a logged-out account, not a login.
        let logged_out = json!({ "claudeAiOauth": { "subscriptionType": "max" } }).to_string();
        assert!(oauth_block(&logged_out).is_none());
        assert!(oauth_block("not json").is_none());
    }

    #[test]
    fn claude_legacy_windows_remain_supported() {
        let response = json!({
            "five_hour": { "utilization": 15.0, "resets_at": "2026-07-22T16:00:00Z" },
            "seven_day": { "utilization": 77.0, "resets_at": "2026-07-22T15:00:00Z" }
        });

        let windows = parse_claude_windows(&response);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5h");
        assert_eq!(windows[1].label, "Week");
    }

    #[test]
    fn kimi_managed_usage_includes_summary_and_limit_windows() {
        let response = json!({
            "usage": {
                "name": "Weekly limit",
                "used": 40,
                "limit": 1000,
                "resetAt": "2026-07-29T15:00:00Z"
            },
            "limits": [
                {
                    "detail": {
                        "used": "25",
                        "limit": "100",
                        "reset_at": "2026-07-27T20:00:00Z"
                    },
                    "window": { "duration": 300, "timeUnit": "MINUTE" }
                },
                {
                    "name": "Daily cap",
                    "detail": { "remaining": 10, "limit": 50 },
                    "window": { "duration": 24, "timeUnit": "HOUR" }
                }
            ]
        });

        let windows = parse_kimi_windows(&response);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "Weekly limit");
        assert_eq!(windows[0].used_percent, 4.0);
        assert_eq!(windows[0].window_mins, Some(10_080));
        assert_eq!(windows[1].label, "5h limit");
        assert_eq!(windows[1].used_percent, 25.0);
        assert_eq!(windows[1].window_mins, Some(300));
        assert_eq!(windows[2].label, "Daily cap");
        assert_eq!(windows[2].used_percent, 80.0);
        assert_eq!(windows[2].window_mins, Some(1_440));
    }

}

//! Bitwarden vault access for the browser pane's "fill login" menu item.
//!
//! # Why this exists rather than the extension
//!
//! Threadknot's browser is headless Chrome, and its right-click menu is drawn
//! by the viewer — a headless browser has no native menu that could reach the
//! screencast. Bitwarden's own "Bitwarden ▸" submenu comes from the
//! `chrome.contextMenus` API, which only ever renders in Chrome's *native*
//! menu, so Threadknot's menu cannot host it no matter what is installed. And
//! `--load-extension` no longer loads anything on Chrome 151 regardless (see
//! docs/BROWSER.md). Talking to the vault directly is the only route to
//! "right-click, fill this login".
//!
//! # The trade this makes
//!
//! Everywhere else, Threadknot deliberately holds sessions and never
//! credentials — the human signs in by hand and Chrome keeps the cookie. This
//! module is the exception, taken knowingly: it holds an unlocked vault session
//! and types real passwords into pages. So the handling rules are strict, and
//! every one of them is load-bearing:
//!
//! - The master password is never written to disk, never logged, and never
//!   placed in an error message. It reaches `bw` on **stdin**, not as an
//!   argument — process arguments are readable by any other process on the
//!   machine (`ps`, WMI), which would leak it to every agent Threadknot runs.
//! - The session key is held in memory only, and dropped on lock/expiry.
//! - Secrets never enter the activity feed, so an agent watching the browser
//!   sees "filled a login", not the login.
//! - `Debug` is implemented by hand for the types that carry secrets, because
//!   a derived one puts them in any log line that formats the struct.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an unlocked vault stays usable. Chosen by the owner (6h): long
/// enough to cover a working day's sittings, short enough that a machine left
/// alone overnight is locked by morning.
const LOCK_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// What the viewer needs to know to decide which UI to show.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VaultState {
    /// No `bw` on PATH — the feature is unavailable rather than broken.
    NotInstalled,
    /// `bw` is present but nobody has logged in. Login needs email, master
    /// password and usually a 2FA code, so it is left to a terminal rather
    /// than reimplemented here.
    LoggedOut,
    /// Logged in, vault sealed. Needs the master password.
    Locked,
    /// Ready to list and fill.
    Unlocked,
}

/// One vault entry, reduced to what a menu can show. Deliberately carries NO
/// password: the secret is fetched only when a specific entry is chosen, so a
/// listing cannot spill a vault into a log or a WebSocket frame.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub username: String,
}

/// A username/password pair on its way into a form. Never serialized, never
/// logged; `Debug` is redacted by hand.
pub struct Login {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A derived Debug would print the password the first time anything
        // formatted this struct into a log line.
        f.write_str("Login { username: <redacted>, password: <redacted> }")
    }
}

struct Unlocked {
    key: String,
    since: Instant,
}

#[derive(Default)]
pub struct Vault {
    session: Mutex<Option<Unlocked>>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.session.lock().map(|s| s.is_some()).unwrap_or(false);
        write!(f, "Vault {{ unlocked: {held} }}")
    }
}

/// Locate the Bitwarden CLI. `bw` is an npm global, so on Windows the thing on
/// PATH is `bw.cmd` — a shim that `Command` cannot execute directly, hence the
/// explicit extension search rather than a bare `Command::new("bw")`.
pub fn cli_path() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("THREADKNOT_BW") {
        if !explicit.is_empty() {
            let p = std::path::PathBuf::from(explicit);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    for name in ["bw.cmd", "bw.exe", "bw"] {
        if let Ok(p) = which::which(name) {
            return Some(p);
        }
    }
    None
}

/// Run `bw` with the given args, optionally feeding stdin, and return stdout.
///
/// `secret_stdin` exists so the master password never becomes a process
/// argument. Errors carry stderr, which `bw` keeps free of the password — but
/// the caller still must not attach the input to any message it builds.
fn run(args: &[&str], secret_stdin: Option<&str>, session: Option<&str>) -> Result<String> {
    let bw = cli_path().ok_or_else(|| anyhow!("the Bitwarden CLI (bw) is not installed"))?;
    let mut cmd = Command::new(bw);
    cmd.args(args)
        .stdin(if secret_stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Passed as an environment variable rather than `--session <key>` for the
    // same reason as the password: arguments are world-readable.
    if let Some(key) = session {
        cmd.env("BW_SESSION", key);
    }
    // Keep the CLI non-interactive: without this a missing session makes `bw`
    // block on a password prompt forever, and the caller just hangs.
    cmd.env("BW_NOINTERACTION", "true");
    let mut child = cmd.spawn().context("running the Bitwarden CLI")?;
    if let Some(input) = secret_stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("could not write to the Bitwarden CLI"))?;
        // Written and dropped immediately so the secret's lifetime is the call.
        stdin.write_all(input.as_bytes())?;
        stdin.write_all(b"\n")?;
        drop(stdin);
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(if err.is_empty() {
            "the Bitwarden CLI failed".to_string()
        } else {
            err
        }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Vault {
    /// The live session key, if one is held and still inside its window.
    /// Expiry is checked here rather than on a timer so a vault that has sat
    /// untouched past the window is locked by the act of asking for it.
    fn key(&self) -> Option<String> {
        let mut guard = self.session.lock().ok()?;
        match guard.as_ref() {
            Some(s) if s.since.elapsed() < LOCK_AFTER => Some(s.key.clone()),
            Some(_) => {
                *guard = None;
                None
            }
            None => None,
        }
    }

    pub fn state(&self) -> VaultState {
        if cli_path().is_none() {
            return VaultState::NotInstalled;
        }
        if self.key().is_some() {
            return VaultState::Unlocked;
        }
        // `bw status` answers unauthenticated / locked / unlocked for the CLI's
        // own stored state. Ours is the narrower question — do WE hold a usable
        // session — so its "unlocked" still means Locked to us if we hold no key.
        match run(&["status"], None, None) {
            Ok(json) => {
                if json.contains("\"unauthenticated\"") {
                    VaultState::LoggedOut
                } else {
                    VaultState::Locked
                }
            }
            Err(_) => VaultState::LoggedOut,
        }
    }

    /// Exchange the master password for a session key. The password is consumed
    /// here and never stored.
    pub fn unlock(&self, password: &str) -> Result<()> {
        let key = run(&["unlock", "--raw"], Some(password), None).map_err(|e| {
            // `bw`'s own wording for a bad password is clear enough to pass on;
            // what must never happen is echoing the input back.
            anyhow!("{e}")
        })?;
        if key.is_empty() {
            return Err(anyhow!("the Bitwarden CLI returned no session key"));
        }
        *self
            .session
            .lock()
            .map_err(|_| anyhow!("vault lock poisoned"))? = Some(Unlocked {
            key,
            since: Instant::now(),
        });
        Ok(())
    }

    pub fn lock(&self) {
        if let Ok(mut guard) = self.session.lock() {
            *guard = None;
        }
    }

    /// Logins whose stored URI matches `origin`'s host, newest CLI order.
    ///
    /// Matching is on the registrable host rather than the full URL: a vault
    /// entry saved for `example.com` has to match a login page at
    /// `accounts.example.com/signin`, which a string compare of URLs never
    /// would. `bw list --search` is a coarse text search, so its results are
    /// filtered here against the entry's own URIs.
    pub fn logins_for(&self, origin: &str) -> Result<Vec<Entry>> {
        let key = self
            .key()
            .ok_or_else(|| anyhow!("the vault is locked"))?;
        let host = host_of(origin).ok_or_else(|| anyhow!("no host in {origin}"))?;
        let needle = registrable(&host);
        let raw = run(&["list", "items", "--search", &needle], None, Some(&key))?;
        let items: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        let mut out = Vec::new();
        for item in items.as_array().unwrap_or(&Vec::new()) {
            let Some(login) = item.get("login") else {
                continue;
            };
            let uris = login
                .get("uris")
                .and_then(|u| u.as_array())
                .cloned()
                .unwrap_or_default();
            let matches = uris.iter().any(|u| {
                u.get("uri")
                    .and_then(|s| s.as_str())
                    .and_then(host_of)
                    .map(|h| {
                        // Either side may be the broader one: a vault URI of
                        // "example.com" covers "accounts.example.com", and one
                        // saved from the deep link covers the bare host.
                        h == host || h.ends_with(&format!(".{host}")) || host.ends_with(&format!(".{h}"))
                    })
                    .unwrap_or(false)
            });
            if !matches {
                continue;
            }
            out.push(Entry {
                id: item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                name: item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string(),
                username: login
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Ok(out)
    }

    /// The credential for one entry. Fetched per fill so nothing holds a
    /// password between the click and the keystrokes.
    pub fn login(&self, id: &str) -> Result<Login> {
        let key = self
            .key()
            .ok_or_else(|| anyhow!("the vault is locked"))?;
        let raw = run(&["get", "item", id], None, Some(&key))?;
        let item: serde_json::Value =
            serde_json::from_str(&raw).context("reading the vault entry")?;
        let login = item
            .get("login")
            .ok_or_else(|| anyhow!("that entry has no login"))?;
        Ok(Login {
            username: login
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            password: login
                .get("password")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }
}

/// Host of a URL, lowercased. Accepts bare hosts too, since vault URIs are
/// often stored without a scheme.
fn host_of(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // Strip a port. An IPv6 literal is full of colons, so it is cut after its
    // closing bracket instead of at the first one.
    let host = if host.starts_with('[') {
        match host.find(']') {
            Some(end) => &host[..=end],
            None => host,
        }
    } else {
        host.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// A coarse "name" to hand `bw list --search`: the last two labels of the host.
/// Only a search hint — the real decision is the URI comparison above — so the
/// well-known-suffix inaccuracy (co.uk) costs a slightly wider search, not a
/// wrong match.
fn registrable(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() <= 2 {
        return host.to_string();
    }
    parts[parts.len() - 2..].join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hosts_out_of_urls_and_bare_uris() {
        assert_eq!(host_of("https://accounts.example.com/signin").unwrap(), "accounts.example.com");
        // Vault entries are routinely saved without a scheme.
        assert_eq!(host_of("example.com").unwrap(), "example.com");
        assert_eq!(host_of("http://user:pw@Example.COM:8080/x").unwrap(), "example.com");
        assert_eq!(host_of("http://[::1]:42800/").unwrap(), "[::1]");
        assert!(host_of("   ").is_none());
    }

    #[test]
    fn search_needle_narrows_to_the_registrable_name() {
        assert_eq!(registrable("accounts.google.com"), "google.com");
        assert_eq!(registrable("example.com"), "example.com");
        assert_eq!(registrable("localhost"), "localhost");
    }

    /// A password reaching a log through a formatted struct is the failure this
    /// whole module is written to avoid, so the redaction is a test, not a
    /// convention.
    #[test]
    fn secrets_are_redacted_in_debug_output() {
        let login = Login {
            username: "oscar@example.com".into(),
            password: "hunter2-should-never-appear".into(),
        };
        let shown = format!("{login:?}");
        assert!(!shown.contains("hunter2-should-never-appear"), "{shown}");
        assert!(!shown.contains("oscar@example.com"), "{shown}");

        let vault = Vault::default();
        assert_eq!(format!("{vault:?}"), "Vault { unlocked: false }");
    }

    #[test]
    fn a_locked_vault_refuses_to_list_or_fetch() {
        let vault = Vault::default();
        assert!(vault.logins_for("https://example.com").is_err());
        assert!(vault.login("whatever").is_err());
    }
}

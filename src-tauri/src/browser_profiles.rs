//! Signed-in browser profiles: durable Chrome user-data directories a thread's
//! browser can attach to, so a site stays logged in across sessions and app
//! restarts.
//!
//! **Threadknot stores sessions, never credentials.** The human signs in by hand in
//! the shared browser pane — the agent never sees a password, a one-time code,
//! or a passkey prompt. What persists afterwards is whatever Chrome itself kept:
//! cookies, localStorage, IndexedDB. Chrome's own password manager stays off.
//!
//! Because a signed-in browser turns "the agent reads a page" into "the agent
//! can act as you", a profile **may be scoped to a set of origins** and that
//! scope is enforced inside the browser (see `browser.rs`), not merely checked
//! in the tool layer. Scoping is optional: listing no sites (stored as the
//! wildcard `*`) opens the profile to the whole web — http and https only, so
//! `file://` and other non-web schemes stay out either way.
//!
//! At rest the profile directory carries Chrome's own storage, which is only as
//! protected as `~/.threadknot` (0700) — chromiumoxide launches Chrome with
//! `--password-store=basic`, whose cookie key is a fixed constant. This matches
//! the trust model Threadknot already has: `server.json` holds the master token in
//! plaintext, and the `claude`/`codex` CLI logins sit unencrypted in the home
//! directory. Anyone who can read the home directory already holds those.
//!
//! Profiles are **bound to the machine that created them** and are never synced
//! across the mesh; a session belongs to the browser it was created in.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    /// Origins whose documents this profile may load: `https://example.com`, or
    /// `https://*.example.com` for a domain and its subdomains. Everything else
    /// is refused by the browser itself.
    #[serde(default)]
    pub origins: Vec<String>,
    /// The machine that owns this profile. Sessions are not portable.
    pub machine_id: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfileFile {
    #[serde(default)]
    profiles: Vec<BrowserProfile>,
}

/// What the browser layer needs to open a session on a profile.
#[derive(Debug, Clone)]
pub struct ProfileSpec {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub origins: Vec<String>,
}

pub struct BrowserProfileStore {
    path: PathBuf,
    root: PathBuf,
    machine_id: String,
    profiles: Mutex<Vec<BrowserProfile>>,
}

impl BrowserProfileStore {
    pub fn open(dir: &Path, machine_id: &str) -> Result<Self> {
        let path = dir.join("browser-profiles.json");
        let profiles = if path.exists() {
            serde_json::from_str::<ProfileFile>(&std::fs::read_to_string(&path)?)
                .context("parse browser-profiles.json")?
                .profiles
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            root: dir.join("browser").join("profiles"),
            machine_id: machine_id.to_string(),
            profiles: Mutex::new(profiles),
        })
    }

    fn flush(&self, profiles: &[BrowserProfile]) -> Result<()> {
        let file = ProfileFile {
            profiles: profiles.to_vec(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<BrowserProfile> {
        self.profiles.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<BrowserProfile> {
        self.profiles
            .lock()
            .unwrap()
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
    }

    pub fn dir_for(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Resolve a profile into everything a browser session needs, refusing any
    /// profile that is unusable: unknown, owned by another machine, or not yet
    /// scoped to an origin.
    pub fn spec(&self, id: &str) -> Result<ProfileSpec> {
        let profile = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown browser profile {id}"))?;
        if profile.machine_id != self.machine_id {
            bail!(
                "browser profile {:?} belongs to another machine — sign in on that machine, or create a profile here",
                profile.name
            );
        }
        if profile.origins.is_empty() {
            bail!(
                "browser profile {:?} has no allowed sites yet; add at least one before using it",
                profile.name
            );
        }
        let dir = self.dir_for(&profile.id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create profile directory {}", dir.display()))?;
        Ok(ProfileSpec {
            id: profile.id,
            name: profile.name,
            dir,
            origins: profile.origins,
        })
    }

    pub fn create(&self, name: &str, origins: &[String]) -> Result<BrowserProfile> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a browser profile needs a name");
        }
        let origins = normalize_origins(origins)?;
        let profile = BrowserProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            origins,
            machine_id: self.machine_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_used_at: None,
        };
        let mut profiles = self.profiles.lock().unwrap();
        profiles.push(profile.clone());
        self.flush(&profiles)?;
        Ok(profile)
    }

    pub fn update(
        &self,
        id: &str,
        name: Option<&str>,
        origins: Option<&[String]>,
    ) -> Result<BrowserProfile> {
        let origins = origins.map(normalize_origins).transpose()?;
        let mut profiles = self.profiles.lock().unwrap();
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown browser profile {id}"))?;
        if let Some(name) = name {
            let name = name.trim();
            if name.is_empty() {
                bail!("a browser profile needs a name");
            }
            profile.name = name.to_string();
        }
        if let Some(origins) = origins {
            profile.origins = origins;
        }
        let updated = profile.clone();
        self.flush(&profiles)?;
        Ok(updated)
    }

    pub fn touch(&self, id: &str) {
        let mut profiles = self.profiles.lock().unwrap();
        if let Some(profile) = profiles.iter_mut().find(|profile| profile.id == id) {
            profile.last_used_at = Some(chrono::Utc::now().to_rfc3339());
            let _ = self.flush(&profiles);
        }
    }

    /// Forget a profile AND erase its stored session. Deleting the record while
    /// leaving cookies on disk would be the worst of both worlds.
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut profiles = self.profiles.lock().unwrap();
        let Some(index) = profiles.iter().position(|profile| profile.id == id) else {
            return Ok(());
        };
        profiles.remove(index);
        self.flush(&profiles)?;
        drop(profiles);
        let dir = self.dir_for(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("cannot erase profile data at {}", dir.display()))?;
        }
        Ok(())
    }
}

/// Accept what a person would type ("example.com", "https://app.example.com",
/// "*.example.com") and store one canonical form per entry. No sites at all —
/// or an explicit `*` — means unscoped: the whole web, stored as `["*"]`.
fn normalize_origins(origins: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in origins {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        if raw == "*" || raw.eq_ignore_ascii_case("all") || raw.eq_ignore_ascii_case("any") {
            return Ok(vec!["*".to_string()]);
        }
        let (scheme, rest) = match raw.split_once("://") {
            Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
            None => ("https".to_string(), raw),
        };
        if !matches!(scheme.as_str(), "http" | "https") {
            bail!("only http and https sites can be allowed: {raw:?}");
        }
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host.is_empty() || host.starts_with('*') && !host.starts_with("*.") {
            bail!("not a usable site: {raw:?}");
        }
        let canonical = format!("{scheme}://{host}");
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    }
    if out.is_empty() {
        return Ok(vec!["*".to_string()]);
    }
    Ok(out)
}

/// Does `url` fall inside one of the profile's allowed origins?
///
/// Host matching is exact, or a `*.` wildcard covering the domain and its
/// subdomains. Ports must match, because `localhost:3000` and `localhost:8080`
/// are different applications.
pub fn origin_allowed(allowed: &[String], url: &str) -> bool {
    // Chrome's own surfaces are not the web and carry no site credentials.
    if url.is_empty()
        || url == "about:blank"
        || url.starts_with("chrome://")
        || url.starts_with("chrome-error://")
        || url.starts_with("devtools://")
    {
        return true;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    // An unscoped profile may visit the whole web — but only the web. A
    // signed-in browser acting as its owner still has no business on file://
    // or other non-web schemes.
    if allowed.iter().any(|entry| entry == "*") {
        return matches!(scheme.as_str(), "http" | "https");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority)
        .to_ascii_lowercase();
    allowed.iter().any(|entry| {
        let Some((allowed_scheme, allowed_host)) = entry.split_once("://") else {
            return false;
        };
        if allowed_scheme != scheme {
            return false;
        }
        match allowed_host.strip_prefix("*.") {
            Some(domain) => host == domain || host.ends_with(&format!(".{domain}")),
            None => host == allowed_host,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> BrowserProfileStore {
        BrowserProfileStore::open(dir, "machine-a").unwrap()
    }

    #[test]
    fn profiles_persist_and_erase_their_session_data() {
        let dir = std::env::temp_dir().join(format!("threadknot-profiles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let profiles = store(&dir);
        let created = profiles.create("Wave", &["wave.com".into()]).unwrap();
        assert_eq!(created.origins, vec!["https://wave.com"]);

        // Session data lives beside the record and goes with it on delete.
        let data = profiles.dir_for(&created.id);
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("Cookies"), b"session").unwrap();

        let reloaded = store(&dir);
        assert_eq!(reloaded.list().len(), 1);
        reloaded.delete(&created.id).unwrap();
        assert!(!data.exists(), "deleting a profile must erase its cookies");
        assert!(store(&dir).list().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_profile_is_unusable_unless_it_is_owned_here_and_no_sites_means_the_web() {
        let dir = std::env::temp_dir().join(format!("threadknot-profiles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let profiles = store(&dir);
        // Listing no sites is not an error — it is an unscoped profile.
        let unscoped = profiles.create("No sites", &[]).unwrap();
        assert_eq!(unscoped.origins, vec!["*"]);
        assert!(profiles.spec(&unscoped.id).is_ok());

        let created = profiles.create("Wave", &["wave.com".into()]).unwrap();
        assert!(profiles.spec(&created.id).is_ok());

        // A profile carried over from another machine is refused, not silently
        // opened with an empty session.
        let elsewhere = BrowserProfileStore::open(&dir, "machine-b").unwrap();
        let error = elsewhere.spec(&created.id).unwrap_err().to_string();
        assert!(error.contains("another machine"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn origin_matching_is_scheme_host_and_port_exact_with_subdomain_wildcards() {
        let allowed = normalize_origins(&[
            "wave.com".into(),
            "https://*.example.com".into(),
            "http://localhost:3000".into(),
        ])
        .unwrap();
        assert!(origin_allowed(&allowed, "https://wave.com/invoices"));
        assert!(origin_allowed(&allowed, "https://app.example.com/x"));
        assert!(origin_allowed(&allowed, "https://example.com"));
        assert!(origin_allowed(&allowed, "http://localhost:3000/app"));

        // Look-alikes, wrong scheme, and wrong port are all outside.
        assert!(!origin_allowed(&allowed, "https://wave.com.evil.test/"));
        assert!(!origin_allowed(&allowed, "http://wave.com/"));
        assert!(!origin_allowed(&allowed, "http://localhost:8080/"));
        assert!(!origin_allowed(&allowed, "https://evil.test/"));
        // Credentials in the URL must not smuggle a host past the check.
        assert!(!origin_allowed(&allowed, "https://wave.com@evil.test/"));
        // Chrome's own pages are not the web.
        assert!(origin_allowed(&allowed, "about:blank"));
    }

    #[test]
    fn a_wildcard_profile_covers_the_web_and_nothing_else() {
        // "*", "all", "any", and an empty list all mean the same thing.
        for input in [vec!["*".to_string()], vec!["ALL".to_string()], vec![]] {
            assert_eq!(normalize_origins(&input).unwrap(), vec!["*"]);
        }
        // A wildcard alongside real sites still means everything.
        assert_eq!(
            normalize_origins(&["wave.com".into(), "*".into()]).unwrap(),
            vec!["*"]
        );
        let allowed = normalize_origins(&[]).unwrap();
        assert!(origin_allowed(&allowed, "https://anything.example/path"));
        assert!(origin_allowed(&allowed, "http://localhost:3000/"));
        assert!(origin_allowed(&allowed, "about:blank"));
        // The whole web is not the whole machine.
        assert!(!origin_allowed(&allowed, "file:///etc/passwd"));
        assert!(!origin_allowed(&allowed, "chrome-extension://abc/x.html"));
    }
}

//! Remote-access configuration: whether this installation answers on the strict
//! ingress at all, and what public origin it has been given.
//!
//! Two things are deliberately awkward here.
//!
//! **Off by default.** Remote access is opt-in. The strict listener is bound
//! regardless — it is loopback-only, so binding it exposes nothing — but it
//! refuses everything until the owner turns remote access on. That makes the
//! switch instant in both directions instead of "takes effect next launch",
//! which matters most in the direction that counts: off.
//!
//! **The origin is configured, never observed.** A pairing QR has to carry an
//! address the phone can actually reach, and a `192.168.x.x` is useless to a
//! phone on cellular. The tempting fix — echo the request's `Host` header — is
//! a pairing-redirection hole: anyone who can reach the server can then have a
//! QR minted that points a phone at a host they chose. So the remote origin is
//! stored, set by the owner (or, later, by the relay's control plane at
//! registration), and used verbatim.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConfig {
    /// Whether the strict ingress accepts anything at all.
    #[serde(default)]
    pub enabled: bool,
    /// The public origin this installation is reachable at, e.g.
    /// `https://calm-harbor.remote.threadknot.app`. `None` until provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Loopback port the connector dials. Defaults to the LAN port + 1.
    pub port: u16,
}

impl RemoteConfig {
    fn new(port: u16) -> Self {
        Self {
            enabled: false,
            origin: None,
            port,
        }
    }
}

pub struct RemoteStore {
    path: PathBuf,
    config: Mutex<RemoteConfig>,
}

impl RemoteStore {
    pub fn open(dir: &std::path::Path, lan_port: u16) -> Result<Self> {
        let path = dir.join("remote.json");
        let default_port = lan_port.checked_add(1).unwrap_or(lan_port);
        let config = if path.exists() {
            serde_json::from_str::<RemoteConfig>(&std::fs::read_to_string(&path)?)
                .unwrap_or_else(|_| RemoteConfig::new(default_port))
        } else {
            RemoteConfig::new(default_port)
        };
        // An explicit override wins, so a second instance on one machine does
        // not fight the first for the loopback port.
        let config = match std::env::var("THREADKNOT_REMOTE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
        {
            Some(port) => RemoteConfig { port, ..config },
            None => config,
        };
        Ok(Self {
            path,
            config: Mutex::new(config),
        })
    }

    pub fn get(&self) -> RemoteConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn enabled(&self) -> bool {
        self.config.lock().unwrap().enabled
    }

    pub fn port(&self) -> u16 {
        self.config.lock().unwrap().port
    }

    /// The provisioned public origin, only while remote access is on. A stale
    /// origin from a disabled installation must not end up in a QR.
    pub fn origin(&self) -> Option<String> {
        let config = self.config.lock().unwrap();
        config.enabled.then(|| config.origin.clone()).flatten()
    }

    /// Apply an owner (or control-plane) change. Returns the stored result.
    pub fn set(&self, enabled: Option<bool>, origin: Option<Option<String>>) -> Result<RemoteConfig> {
        let mut config = self.config.lock().unwrap();
        if let Some(origin) = origin {
            config.origin = match origin {
                Some(raw) => Some(normalize_origin(&raw)?),
                None => None,
            };
        }
        if let Some(enabled) = enabled {
            anyhow::ensure!(
                !enabled || config.origin.is_some(),
                "set the public address for this machine before turning remote access on"
            );
            config.enabled = enabled;
        }
        let out = config.clone();
        let json = serde_json::to_string_pretty(&out)?;
        drop(config);
        crate::store::write_private(&self.path, &json).context("write remote.json")?;
        Ok(out)
    }
}

/// Validate and canonicalize a public origin.
///
/// HTTPS only, no path, no query, no credentials, no port games — this string
/// ends up in a QR code that points a phone at a machine, so anything
/// surprising in it is a redirection primitive rather than a typo.
pub fn normalize_origin(raw: &str) -> Result<String> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "the address is empty");
    let url = url::Url::parse(raw).context("that is not a valid address")?;
    anyhow::ensure!(
        url.scheme() == "https",
        "a remote address must be https — a phone off your network has no other protection"
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "a remote address must not carry credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "a remote address must not carry a query string"
    );
    anyhow::ensure!(
        matches!(url.path(), "" | "/"),
        "a remote address must not carry a path"
    );
    let host = url.host_str().context("that address has no host")?;
    anyhow::ensure!(
        !host.eq_ignore_ascii_case("localhost") && host.parse::<std::net::IpAddr>().is_err(),
        "a remote address must be a public hostname, not a local address"
    );
    Ok(match url.port() {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (PathBuf, RemoteStore) {
        let dir = std::env::temp_dir().join(format!("tk-remote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = RemoteStore::open(&dir, 42800).unwrap();
        (dir, store)
    }

    #[test]
    fn remote_access_is_off_until_it_is_provisioned_and_turned_on() {
        let (dir, store) = temp_store();
        assert!(!store.enabled(), "opt-in, always");
        assert_eq!(store.port(), 42801);
        assert_eq!(store.origin(), None);

        // Cannot enable without somewhere to be reached.
        assert!(store.set(Some(true), None).is_err());

        store
            .set(None, Some(Some("https://calm-harbor.remote.threadknot.app".into())))
            .unwrap();
        assert_eq!(
            store.origin(),
            None,
            "a configured origin is still not a live one while disabled"
        );

        store.set(Some(true), None).unwrap();
        assert_eq!(
            store.origin().as_deref(),
            Some("https://calm-harbor.remote.threadknot.app")
        );

        // Survives a restart, and turning it back off hides the origin again.
        let reloaded = RemoteStore::open(&dir, 42800).unwrap();
        assert!(reloaded.enabled());
        reloaded.set(Some(false), None).unwrap();
        assert_eq!(reloaded.origin(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn origins_that_could_redirect_a_pairing_are_refused() {
        assert_eq!(
            normalize_origin(" https://a.example.com/ ").unwrap(),
            "https://a.example.com"
        );
        assert_eq!(
            normalize_origin("https://a.example.com:8443").unwrap(),
            "https://a.example.com:8443"
        );
        for bad in [
            "",
            "not a url",
            // Plaintext to a phone off the network.
            "http://a.example.com",
            // Credentials, paths and queries all change where a phone lands.
            "https://user:pw@a.example.com",
            "https://a.example.com/pair",
            "https://a.example.com/?next=evil",
            // A LAN address is exactly the thing a remote origin exists to replace.
            "https://192.168.1.10",
            "https://localhost",
            // Not a URL scheme we would ever dial.
            "javascript:alert(1)",
        ] {
            assert!(normalize_origin(bad).is_err(), "{bad} must be refused");
        }
    }
}

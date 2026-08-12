//! The outbound connector: how this desktop becomes reachable from anywhere
//! without opening a router port (Stage 2).
//!
//! ```text
//!   phone ──TLS 443──▶ relay ──yamux stream──▶ [here] ──▶ 127.0.0.1:<remote port>
//! ```
//!
//! Everything here is one direction: this process dials **out**, holds the
//! connection, and accepts multiplexed streams on it. Nothing listens.
//!
//! # The invariant this file exists to keep
//!
//! Every inbound stream is joined to a fresh TCP connection to Threadknot's own
//! **strict loopback listener** — `127.0.0.1:<remote port>` — and there is no
//! code path, configuration key or protocol field that can change that target.
//! That is security invariant 9, and it is the difference between "the relay was
//! compromised" and "every customer's workstation became an SSRF proxy into
//! their employer's network". The protocol has no field for a destination
//! precisely so that this file cannot be talked into using one.
//!
//! # Why the connector holds no application authority
//!
//! The connector credential authenticates *an installation to the relay*. It is
//! not a login. Traffic arriving through it is still authenticated and
//! authorized by the strict ingress exactly as if it had come from a phone on
//! the LAN — cookie or device bearer, capability-checked, master credential
//! refused. Possession of the connector key gets an attacker a tunnel to a
//! server that will then ask them who they are.

use anyhow::{Context, Result};
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use relay_protocol as proto;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// Where the relay's connector endpoint lives. Overridable for testing against a
/// local relay; the default is the production SNI from the shared protocol crate.
fn relay_endpoint() -> (String, u16, String) {
    let host = std::env::var("THREADKNOT_RELAY_HOST").unwrap_or_else(|_| proto::CONNECTOR_SNI.into());
    let port = std::env::var("THREADKNOT_RELAY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(443);
    // The SNI is what the relay routes on, and it is *not* always the host we
    // dial: pointing `THREADKNOT_RELAY_HOST` at a test box must still present
    // the SNI the relay expects, or it lands on the data plane instead of the
    // connector endpoint.
    let sni = std::env::var("THREADKNOT_RELAY_SNI").unwrap_or_else(|_| proto::CONNECTOR_SNI.into());
    (host, port, sni)
}

fn control_base() -> String {
    std::env::var("THREADKNOT_CONTROL_URL")
        .unwrap_or_else(|_| format!("https://{}", proto::CONTROL_SNI))
}

/// Persisted connector state. The private key is a sibling file, not a field
/// here, so it can be `0600` on its own.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Assigned at enrollment. Empty until then.
    #[serde(default)]
    pub installation_id: String,
    /// Server-assigned. **Never** chosen here — see invariant 10. Stored only so
    /// Settings can display it while offline.
    #[serde(default)]
    pub hostname: String,
}

/// What Settings shows. Deliberately verbose about *why* something is not
/// working: the person reading it cannot see the relay's logs, and "disconnected"
/// with no reason is the single most common way a tunnel product wastes a
/// support hour.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorStatus {
    /// `off` | `unenrolled` | `connecting` | `online` | `error`
    pub state: &'static str,
    pub hostname: String,
    pub public_origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_since: Option<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// False when the subscription has lapsed or a kill switch is on. Existing
    /// traffic still flows; only new sessions are refused.
    pub accepting_new_sessions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
    /// Days left in the trial, so Settings can warn *before* anything stops
    /// working. `hold_reason` cannot serve this purpose — it is only populated
    /// once the hold is already in force, which meant the first signal a customer
    /// ever got was a machine that had stopped accepting sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial_days_left: Option<i64>,
    /// A device-approval request waiting on a human. Present only while one is in
    /// flight, so the UI can show progress without polling a second endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_quota_bytes: Option<u64>,
    /// Streams currently proxied through this connector.
    pub live_streams: u32,
}

/// A device-approval request in flight, as Settings renders it.
///
/// `device_code` is deliberately absent: it is the secret that collects the
/// enrollment, it stays in the connector, and a status object is exactly the kind
/// of thing that gets logged or sent to a browser.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalStatus {
    /// Hyphenated for reading aloud. A fallback for a machine with no browser —
    /// the normal path is the link, which carries the code already.
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_at: String,
    /// `waiting` | `denied` | `expired`. Approval is not a state here: a granted
    /// request becomes an enrolment, and the panel disappears.
    pub state: &'static str,
}

pub struct Connector {
    dir: std::path::PathBuf,
    config: Mutex<ConnectorConfig>,
    status: Mutex<ConnectorStatus>,
    /// Wake the supervisor now — enabled/disabled, or freshly enrolled.
    kick: Notify,
    /// Broadcast a status change to connected clients.
    hub: Mutex<Option<Arc<crate::agents::Hub>>>,
    /// Where the pairing origin is recorded. The relay assigns the hostname at
    /// enrollment, so enrollment is the only moment that origin is known — and a
    /// device approval completes on a background task, with no request handler
    /// around to write it. Without this, approving would leave "pair a phone →
    /// from anywhere" refusing with "set this machine's public address first",
    /// immediately after the one step that established it.
    remote: Mutex<Option<Arc<crate::remote::RemoteStore>>>,
    /// The loopback port to forward to. Read once, from configuration, and never
    /// from anything on the wire.
    remote_port: u16,
    /// Streams currently being proxied. An atomic rather than a field on
    /// `ConnectorStatus` because it is incremented and decremented from every
    /// forwarding task, and taking the status mutex on each would serialise them.
    live_streams: std::sync::atomic::AtomicU32,
}

impl Connector {
    pub fn open(dir: &std::path::Path, remote_port: u16) -> Result<Arc<Self>> {
        let path = dir.join("connector.json");
        let config: ConnectorConfig = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse connector.json")?
        } else {
            ConnectorConfig::default()
        };
        let status = ConnectorStatus {
            state: if !config.enabled {
                "off"
            } else if config.installation_id.is_empty() {
                "unenrolled"
            } else {
                "connecting"
            },
            hostname: config.hostname.clone(),
            public_origin: public_origin(&config.hostname),
            accepting_new_sessions: true,
            ..Default::default()
        };
        Ok(Arc::new(Self {
            dir: dir.to_path_buf(),
            config: Mutex::new(config),
            status: Mutex::new(status),
            kick: Notify::new(),
            hub: Mutex::new(None),
            remote: Mutex::new(None),
            remote_port,
            live_streams: std::sync::atomic::AtomicU32::new(0),
        }))
    }

    pub fn attach_hub(&self, hub: Arc<crate::agents::Hub>) {
        *self.hub.lock().unwrap() = Some(hub);
    }

    pub fn attach_remote(&self, remote: Arc<crate::remote::RemoteStore>) {
        *self.remote.lock().unwrap() = Some(remote);
    }

    pub fn config(&self) -> ConnectorConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn status(&self) -> ConnectorStatus {
        let mut status = self.status.lock().unwrap().clone();
        // Read live rather than trusting the stored copy: the count changes on
        // every proxied connection, and a status field updated only on state
        // transitions would show a stale number for as long as nothing else
        // happened — which is most of the time.
        status.live_streams = self
            .live_streams
            .load(std::sync::atomic::Ordering::Relaxed);
        status
    }

    fn flush(&self, config: &ConnectorConfig) -> Result<()> {
        let json = serde_json::to_string_pretty(config)?;
        let tmp = self.dir.join("connector.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.dir.join("connector.json"))?;
        Ok(())
    }

    fn set_status(&self, f: impl FnOnce(&mut ConnectorStatus)) {
        {
            let mut status = self.status.lock().unwrap();
            f(&mut status);
        }
        if let Some(hub) = self.hub.lock().unwrap().clone() {
            hub.broadcast_state("connector", None);
        }
    }

    /// Update status WITHOUT a `connector` state broadcast. For high-frequency
    /// bookkeeping (relay heartbeat byte counters, every ~10s): broadcasting
    /// those made every client refetch on each heartbeat. The settings panel
    /// polls connector.status while open, so quiet updates still surface.
    fn set_status_quiet(&self, f: impl FnOnce(&mut ConnectorStatus)) {
        let mut status = self.status.lock().unwrap();
        f(&mut status);
    }

    /// Turn remote access on or off. Off drops the session immediately.
    pub fn set_enabled(&self, enabled: bool) -> Result<ConnectorStatus> {
        {
            let mut config = self.config.lock().unwrap();
            config.enabled = enabled;
            self.flush(&config)?;
        }
        let unenrolled = self.config().installation_id.is_empty();
        self.set_status(|s| {
            s.state = if !enabled {
                "off"
            } else if unenrolled {
                "unenrolled"
            } else {
                "connecting"
            };
            s.last_error = None;
            if !enabled {
                s.connected_since = None;
            }
        });
        self.kick.notify_waiters();
        Ok(self.status())
    }

    /// This machine's Ed25519 identity, generated on first use.
    ///
    /// Stable for the life of the install: the control plane knows this key, so
    /// regenerating it would silently orphan the installation. Rotation is an
    /// explicit two-call operation against the control plane, never a side effect
    /// of a missing file.
    fn signing_key(&self) -> Result<SigningKey> {
        let path = self.dir.join("connector.key");
        if path.exists() {
            crate::store::restrict_file(&path);
            let encoded = std::fs::read_to_string(&path)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .context("decode connector key")?;
            let seed: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("connector key is not 32 bytes"))?;
            return Ok(SigningKey::from_bytes(&seed));
        }
        let mut seed = [0u8; 32];
        {
            use rand::RngCore as _;
            rand::rng().fill_bytes(&mut seed);
        }
        crate::store::write_private(
            &path,
            &base64::engine::general_purpose::STANDARD.encode(seed),
        )?;
        tracing::info!("generated this installation's connector identity");
        Ok(SigningKey::from_bytes(&seed))
    }

    fn public_key_b64(&self) -> Result<String> {
        Ok(base64::engine::general_purpose::STANDARD
            .encode(self.signing_key()?.verifying_key().to_bytes()))
    }

    /// Ask the control plane to open a device-approval request, and start
    /// watching for the answer.
    ///
    /// This is the path setup should normally take. Nothing sensitive is shown:
    /// the desktop generates its keypair, signs for it, and gets back a URL for
    /// the owner to open and a short code as a fallback for a machine with no
    /// browser. The `device_code` — the secret that collects the result — stays in
    /// this process and is never displayed.
    ///
    /// Polling happens here rather than in the UI, so closing Settings does not
    /// abandon an approval the owner is in the middle of granting.
    pub async fn begin_approval(self: &Arc<Self>, machine_name: &str) -> Result<ApprovalStatus> {
        {
            let config = self.config();
            anyhow::ensure!(
                config.installation_id.is_empty(),
                "this machine is already connected to the relay"
            );
        }

        let key = self.signing_key()?;
        let public_key = self.public_key_b64()?;
        let machine_name: String = machine_name.chars().take(64).collect();
        // Signed over the key *and* the name, so what the approver is shown is
        // bound to the key they are approving. Without the name in the message, a
        // request could be relabelled between signing and display.
        let signature = base64::engine::general_purpose::STANDARD.encode(
            key.sign(&proto::device_start_message(&public_key, &machine_name))
                .to_bytes(),
        );
        let request = proto::DeviceStartRequest {
            public_key,
            machine_name: machine_name.clone(),
            client_version: Some(env!("THREADKNOT_VERSION").to_string()),
            signature,
        };

        let resp = crate::hermes::http_client()
            .post(format!("{}/v1/connector/device/start", control_base()))
            .json(&request)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .context("could not reach the Threadknot relay control plane")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("{}", if body.trim().is_empty() { status.to_string() } else { body });
        }
        let started: proto::DeviceStartResponse =
            resp.json().await.context("bad device-start response")?;

        let expires_at = std::time::SystemTime::now()
            + Duration::from_secs(started.expires_in_secs.min(3600));
        let approval = ApprovalStatus {
            user_code: started.user_code.clone(),
            verification_uri: started.verification_uri.clone(),
            verification_uri_complete: started.verification_uri_complete.clone(),
            expires_at: crate::protocol::iso_from(expires_at),
            state: "waiting",
        };
        self.set_status(|s| s.approval = Some(approval.clone()));

        // The secret never leaves this process, so it is held in memory rather
        // than written next to the config: a request that does not survive a
        // restart is the correct behaviour, since the owner is standing here now.
        let device_code = started.device_code;
        let interval = Duration::from_secs(started.interval_secs.clamp(1, 30));
        let deadline = std::time::Instant::now()
            + Duration::from_secs(started.expires_in_secs.min(3600));
        let connector = Arc::clone(self);
        tokio::spawn(async move {
            connector.watch_approval(device_code, interval, deadline).await;
        });

        Ok(approval)
    }

    /// Poll until somebody answers, the window closes, or the request is
    /// abandoned.
    async fn watch_approval(
        self: Arc<Self>,
        device_code: String,
        interval: Duration,
        deadline: std::time::Instant,
    ) {
        loop {
            tokio::time::sleep(interval).await;

            // The owner cancelled, or an enrollment landed some other way. Either
            // way this task has nothing left to collect.
            let still_waiting = self
                .status
                .lock()
                .unwrap()
                .approval
                .as_ref()
                .is_some_and(|a| a.state == "waiting");
            if !still_waiting {
                return;
            }

            if std::time::Instant::now() >= deadline {
                self.set_status(|s| {
                    if let Some(approval) = s.approval.as_mut() {
                        approval.state = "expired";
                    }
                });
                return;
            }

            match self.poll_approval(&device_code).await {
                // A network blip must not abandon an approval the owner is about
                // to grant, so a failed poll is logged and retried until the
                // deadline rather than treated as a refusal.
                Err(err) => tracing::debug!("device approval poll failed: {err:#}"),
                Ok(proto::DevicePollResponse::Pending) => {}
                Ok(proto::DevicePollResponse::Denied) => {
                    self.set_status(|s| {
                        if let Some(approval) = s.approval.as_mut() {
                            approval.state = "denied";
                        }
                    });
                    return;
                }
                Ok(proto::DevicePollResponse::Expired) => {
                    self.set_status(|s| {
                        if let Some(approval) = s.approval.as_mut() {
                            approval.state = "expired";
                        }
                    });
                    return;
                }
                Ok(proto::DevicePollResponse::Approved(enrolled)) => {
                    if let Err(err) = self.adopt_enrollment(&enrolled) {
                        tracing::error!("could not store the approved enrollment: {err:#}");
                        self.set_status(|s| {
                            s.last_error = Some(format!("could not save the connection: {err}"));
                        });
                        return;
                    }
                    tracing::info!("enrolled by device approval as {}", enrolled.hostname);
                    return;
                }
            }
        }
    }

    async fn poll_approval(&self, device_code: &str) -> Result<proto::DevicePollResponse> {
        let key = self.signing_key()?;
        let signature = base64::engine::general_purpose::STANDARD
            .encode(key.sign(&proto::device_poll_message(device_code)).to_bytes());
        let resp = crate::hermes::http_client()
            .post(format!("{}/v1/connector/device/poll", control_base()))
            .json(&proto::DevicePollRequest {
                device_code: device_code.to_string(),
                signature,
            })
            .timeout(Duration::from_secs(15))
            .send()
            .await?;
        anyhow::ensure!(resp.status().is_success(), "poll returned {}", resp.status());
        Ok(resp.json().await?)
    }

    /// Stop watching. The request stays valid server-side until it expires — this
    /// only stops *this* machine collecting it, which is what closing the panel
    /// should mean.
    pub fn cancel_approval(&self) {
        self.set_status(|s| s.approval = None);
    }

    /// Persist an enrollment however it was obtained, and wake the supervisor.
    ///
    /// Shared by the token flow and the approval flow so there is one definition
    /// of "this machine is now enrolled". Two copies of this would be two chances
    /// to leave a machine holding an installation id it never saved.
    fn adopt_enrollment(&self, enrolled: &proto::EnrollResponse) -> Result<()> {
        {
            let mut config = self.config.lock().unwrap();
            config.installation_id = enrolled.installation_id.clone();
            config.hostname = enrolled.hostname.clone();
            config.enabled = true;
            self.flush(&config)?;
        }
        // Recorded here rather than by the caller so every enrollment path leaves
        // the machine ready to pair a phone "from anywhere" without a second step.
        if !enrolled.public_origin.is_empty() {
            if let Some(remote) = self.remote.lock().unwrap().clone() {
                if let Err(err) = remote.set(Some(true), Some(Some(enrolled.public_origin.clone()))) {
                    tracing::warn!("could not record the public origin: {err:#}");
                }
            }
        }

        let hostname = enrolled.hostname.clone();
        let public_origin = enrolled.public_origin.clone();
        self.set_status(|s| {
            s.state = "connecting";
            s.hostname = hostname.clone();
            s.public_origin = public_origin.clone();
            s.last_error = None;
            // Cleared on success: leaving a completed request on screen invites
            // someone to approve it a second time and wonder why nothing happens.
            s.approval = None;
        });
        self.kick.notify_waiters();
        Ok(())
    }

    /// Register this installation with the control plane using an enrollment
    /// token the owner copied from the console.
    ///
    /// Kept for scripted and headless installs, and for anyone holding a token
    /// already. [`begin_approval`] is the interactive path — it asks nobody to
    /// carry a secret between two applications.
    ///
    /// The hostname comes back from the server and is never proposed here
    /// (invariant 10): if a connector could ask for a name, the first thing
    /// someone would ask for is another customer's.
    pub async fn enroll(&self, enrollment_token: &str, machine_name: &str) -> Result<ConnectorStatus> {
        let enrollment_token = enrollment_token.trim();
        anyhow::ensure!(!enrollment_token.is_empty(), "paste the token from the console");
        let key = self.signing_key()?;
        let public_key = self.public_key_b64()?;
        // Signed, not merely sent: the token proves the *owner* authorised this,
        // and the signature proves the machine holds the key it is registering.
        // Without the signature, anyone who saw the token could enroll their own
        // key against this organisation.
        let signature = base64::engine::general_purpose::STANDARD.encode(
            key.sign(&proto::enroll_message(enrollment_token, &public_key))
                .to_bytes(),
        );
        let request = proto::EnrollRequest {
            enrollment_token: enrollment_token.to_string(),
            public_key,
            machine_name: machine_name.chars().take(64).collect(),
            client_version: Some(env!("THREADKNOT_VERSION").to_string()),
            signature,
        };
        let resp = crate::hermes::http_client()
            .post(format!("{}/v1/connector/enroll", control_base()))
            .json(&request)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .context("could not reach the Threadknot relay control plane")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // The control plane's message names the limit that was hit (a 26th
            // machine, a lapsed subscription), so it is shown verbatim rather
            // than replaced with a generic failure.
            anyhow::bail!("{}", if body.trim().is_empty() { status.to_string() } else { body });
        }
        let enrolled: proto::EnrollResponse = resp.json().await.context("bad enrollment response")?;
        self.adopt_enrollment(&enrolled)?;
        Ok(self.status())
    }

    /// Spawn the supervisor. Needs a runtime.
    pub fn start(self: &Arc<Self>) {
        let connector = Arc::clone(self);
        tokio::spawn(async move { connector.supervise().await });
    }

    /// Reconnect forever, with backoff and jitter.
    ///
    /// Jitter is not politeness. Without it, a relay restart brings every
    /// connector back at the same instant on the same doubling schedule, so the
    /// herd stays synchronised through every subsequent failure — which is
    /// exactly the reconnect storm the relay's capacity work is sized against.
    async fn supervise(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        // Consecutive refusals that waiting cannot fix. Counted rather than
        // acted on immediately — see `TERMINAL_REFUSALS`.
        let mut refusals = 0u32;
        loop {
            let config = self.config();
            if !config.enabled || config.installation_id.is_empty() {
                self.kick.notified().await;
                backoff = Duration::from_secs(1);
                refusals = 0;
                continue;
            }
            let outcome = self.hold_session(&config).await;
            let mut sleep_for = match outcome {
                // A clean close means the relay asked us to come back. Treat it
                // as success so a rolling relay restart does not walk every
                // connector up to a 5-minute backoff — unless the relay named a
                // delay, in which case its number wins over our own optimism.
                Ok(Reconnect::Soon { after }) => {
                    backoff = Duration::from_secs(1);
                    refusals = 0;
                    after.unwrap_or(backoff).max(backoff)
                }
                // Revoked, unsubscribed, or too old — retrying cannot fix any of
                // those. But the *first* few of these are not that: the relay
                // routes from a routing table it refreshes on a timer, so a
                // connector that dials the instant enrollment returns is
                // legitimately unknown to it for a few seconds and gets the same
                // `Unauthenticated` a revoked machine gets (invariant 12 — the
                // relay must not distinguish them, so neither can we). Parking
                // permanently on it left a machine that had just enrolled
                // successfully offline until its owner toggled remote access off
                // and on, which is the entire first run of the paid product.
                //
                // So: retry a bounded number of times on the normal doubling
                // backoff, then park. Bounded is what keeps the storm guard —
                // a genuinely revoked fleet makes a handful of attempts and
                // stops, rather than hammering the relay that revoked it.
                Ok(Reconnect::Stop(reason)) if refusals + 1 < TERMINAL_REFUSALS => {
                    refusals += 1;
                    tracing::debug!("connector refused ({refusals}/{TERMINAL_REFUSALS}): {reason}");
                    self.set_status(|s| {
                        s.state = "connecting";
                        s.last_error = Some(reason);
                        s.connected_since = None;
                    });
                    backoff = next_backoff(backoff);
                    backoff
                }
                Ok(Reconnect::Stop(reason)) => {
                    tracing::warn!("connector stopped: {reason}");
                    self.set_status(|s| {
                        s.state = "error";
                        s.last_error = Some(reason);
                        s.connected_since = None;
                    });
                    self.kick.notified().await;
                    backoff = Duration::from_secs(1);
                    refusals = 0;
                    continue;
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    tracing::debug!("connector session ended: {message}");
                    self.set_status(|s| {
                        s.state = "connecting";
                        s.last_error = Some(message);
                        s.connected_since = None;
                    });
                    refusals = 0;
                    backoff = next_backoff(backoff);
                    backoff
                }
            };
            // Up to +50% of the delay, so a herd spreads instead of
            // resynchronising on every subsequent failure.
            //
            // The `.min(1000)` this used to carry capped the multiplier at one
            // second, so the spread was at most 500 ms however far the backoff
            // had grown — 1,000 connectors all re-dialled inside half a second,
            // which is not desynchronisation, it is a slightly blurred thundering
            // herd. Measured directly by the Stage 8 load test. The relay
            // absorbed it, but the jitter has to scale with the delay or it stops
            // meaning anything at exactly the point it matters most: a long
            // backoff after a sustained outage, with a large estate.
            let jitter_ms = (sleep_for.as_millis() as u64)
                .saturating_mul(rand::random::<u64>() % 500)
                / 1000;
            sleep_for += Duration::from_millis(jitter_ms);
            tokio::select! {
                _ = self.kick.notified() => {}
                _ = tokio::time::sleep(sleep_for) => {}
            }
        }
    }

    /// One connection: dial, authenticate, then serve streams until it ends.
    // `&Arc<Self>` rather than `&self`: each forwarding task holds a stream guard
    // that keeps the connector alive for as long as the stream it counts.
    async fn hold_session(self: &Arc<Self>, config: &ConnectorConfig) -> Result<Reconnect> {
        use tokio_util::compat::TokioAsyncReadCompatExt as _;

        let (host, port, sni) = relay_endpoint();
        let tcp = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        .context("relay connect timed out")?
        .with_context(|| format!("connect {host}:{port}"))?;
        let _ = tcp.set_nodelay(true);

        // Public web PKI here, unlike the mesh: the relay presents a real
        // Let's Encrypt certificate for a name we know in advance, so there is
        // nothing to pin and no first-use problem to solve.
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_root_certs());
        // Additional trust anchor for a local relay under test only. It *adds* a
        // root; certificate verification itself is untouched, so there is no
        // "accept invalid certs" path here and there must never be one. Set by
        // `scripts/relay-smoke.sh`, which stands a throwaway CA up in a temp dir.
        // Never set this in production: the relay has a real Let's Encrypt
        // certificate, and a private root here would be a second way in.
        for anchor in extra_ca_anchors()? {
            roots.add(anchor).context("THREADKNOT_RELAY_CA_PEM is not a usable CA")?;
        }
        let tls_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let server_name = rustls_pki_types::ServerName::try_from(sni.clone())
            .with_context(|| format!("invalid relay SNI {sni}"))?;
        let mut tls = tokio_rustls::TlsConnector::from(tls_config)
            .connect(server_name, tcp)
            .await
            .context("relay TLS handshake")?;

        proto::write_frame(
            &mut tls,
            &proto::ConnectorHello {
                protocol_version: proto::PROTOCOL_VERSION,
                installation_id: config.installation_id.clone(),
                public_key: self.public_key_b64()?,
                client_version: Some(env!("THREADKNOT_VERSION").to_string()),
            },
        )
        .await
        .context("send hello")?;

        let challenge: proto::RelayChallenge =
            proto::read_frame(&mut tls).await.context("read challenge")?;
        let nonce = base64::engine::general_purpose::STANDARD
            .decode(&challenge.nonce)
            .context("relay sent a malformed nonce")?;
        let signature = base64::engine::general_purpose::STANDARD.encode(
            self.signing_key()?
                .sign(&proto::auth_message(&nonce, &config.installation_id))
                .to_bytes(),
        );
        proto::write_frame(&mut tls, &proto::ConnectorAuth { signature })
            .await
            .context("send auth")?;

        let verdict: proto::RelayVerdict =
            proto::read_frame(&mut tls).await.context("read verdict")?;
        let ready = match verdict {
            proto::RelayVerdict::Ready(ready) => ready,
            proto::RelayVerdict::Refused(refused) => {
                return Ok(if refused.code.is_retryable() {
                    // The relay sends 15s for `Disabled` and `AlreadyConnected`.
                    // Honouring it is the difference between backing off and
                    // re-dialling once a second for as long as the refusal lasts.
                    Reconnect::Soon {
                        after: refused.retry_after_secs.map(Duration::from_secs),
                    }
                } else {
                    Reconnect::Stop(refused.message)
                });
            }
        };

        tracing::info!("connector online as {}", ready.hostname);
        let hostname = ready.hostname.clone();
        self.set_status(|s| {
            s.state = "online";
            s.hostname = hostname.clone();
            s.public_origin = public_origin(&hostname);
            s.connected_since = Some(crate::protocol::now_iso());
            s.last_error = None;
            s.accepting_new_sessions = ready.accepting_new_sessions;
            s.hold_reason = ready.hold_reason.clone();
            s.trial_days_left = ready.trial_days_left;
            s.month_quota_bytes = Some(ready.limits.month_quota_bytes);
        });
        // The relay assigns the hostname, so this is also where the pairing
        // origin comes from — a remote QR must never be built from a guess.
        if config.hostname != ready.hostname {
            let mut stored = self.config.lock().unwrap();
            stored.hostname = ready.hostname.clone();
            let _ = self.flush(&stored);
        }

        // The relay is the yamux client (it opens streams); we accept them.
        let mut yamux_config = yamux::Config::default();
        yamux_config.set_max_num_streams(ready.limits.max_concurrent_streams as usize);
        // yamux speaks the `futures` IO traits; `compat()` bridges both halves,
        // since tokio-util's wrapper implements futures read AND write when the
        // inner stream implements the tokio equivalents.
        let mut session =
            yamux::Connection::new(tls.compat(), yamux_config, yamux::Mode::Server);

        // The control stream is whichever stream arrives first, by protocol
        // convention, and serving it *is* the session: when it ends, the session
        // ends, because without it there is no liveness signal and no way to
        // receive a kill switch — continuing to proxy would mean serving traffic
        // we can no longer be told to stop serving.
        //
        // It is therefore polled alongside the accept loop rather than awaited in
        // place of it, and rather than moved to a task. Awaiting it in place stops
        // this task ever polling `poll_next_inbound` again, and yamux makes no
        // progress at all unless it is polled — so every proxied stream, *and the
        // control stream itself*, silently stalls: the relay opens a stream, sees
        // zero bytes, and drops the session on a heartbeat timeout 40 seconds
        // later while the desktop still says "online". A task would work but
        // needs the control stream's outcome plumbed back to decide whether to
        // reconnect; one `select!` keeps that decision where it is made.
        // `Send`, because `supervise` runs on a spawned task and everything held
        // across an await in it has to be.
        type ControlFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Reconnect>> + Send + 'a>>;
        let mut control: ControlFuture<'_> = Box::pin(std::future::pending());
        let mut control_taken = false;
        loop {
            let inbound = tokio::select! {
                // Biased so a finished control stream wins a tie: if both are
                // ready, the session is over and a stream opened in the same
                // instant has nowhere to go.
                biased;
                outcome = &mut control => return outcome,
                inbound = futures::future::poll_fn(|cx| session.poll_next_inbound(cx)) => inbound,
            };
            let stream = match inbound {
                Some(Ok(stream)) => stream,
                Some(Err(e)) => return Err(anyhow::anyhow!("yamux: {e}")),
                // The relay closed the session cleanly.
                None => return Ok(Reconnect::Soon { after: None }),
            };
            if !control_taken {
                control_taken = true;
                control = Box::pin(self.serve_control(stream));
                continue;
            }
            let connector = ConnectorForward {
                remote_port: self.remote_port,
            };
            let guard = StreamGuard::open(self);
            tokio::spawn(async move {
                connector.forward(stream, guard).await;
            });
        }
    }

    /// The control stream: heartbeats out, entitlement and kill switches in.
    async fn serve_control(&self, stream: yamux::Stream) -> Result<Reconnect> {
        use tokio_util::compat::FuturesAsyncReadCompatExt as _;
        let mut stream = stream.compat();
        loop {
            // A missing heartbeat is how a half-open connection is noticed: the
            // TCP socket can stay "up" indefinitely after a laptop lid closes or
            // a NAT drops the mapping, so silence has to be the signal.
            let message: proto::ControlMessage =
                match tokio::time::timeout(Duration::from_secs(90), proto::read_frame(&mut stream))
                    .await
                {
                    Ok(Ok(message)) => message,
                    Ok(Err(e)) => return Err(anyhow::anyhow!("control stream: {e}")),
                    Err(_) => anyhow::bail!("relay stopped sending heartbeats"),
                };
            match message {
                proto::ControlMessage::Heartbeat {
                    bytes_in,
                    bytes_out,
                    month_bytes,
                    month_quota_bytes,
                } => {
                    self.set_status_quiet(|s| {
                        s.bytes_in = bytes_in;
                        s.bytes_out = bytes_out;
                        s.month_bytes = month_bytes;
                        if let Some(quota) = month_quota_bytes {
                            s.month_quota_bytes = Some(quota);
                        }
                    });
                    proto::write_frame(&mut stream, &proto::ControlMessage::HeartbeatAck)
                        .await
                        .context("heartbeat ack")?;
                }
                proto::ControlMessage::HeartbeatAck => {}
                proto::ControlMessage::Entitlement {
                    accepting_new_sessions,
                    hold_reason,
                    trial_days_left,
                } => {
                    // Applied WITHOUT dropping the tunnel. This is what makes
                    // "never mid-session" implementable: a trial that lapses
                    // during a turn must not sever the turn.
                    tracing::info!(
                        "connector entitlement changed: accepting_new_sessions={accepting_new_sessions}"
                    );
                    self.set_status(|s| {
                        s.accepting_new_sessions = accepting_new_sessions;
                        s.hold_reason = hold_reason.clone();
                        s.trial_days_left = trial_days_left;
                    });
                }
                proto::ControlMessage::Close {
                    code,
                    message,
                    reconnect,
                } => {
                    tracing::info!("relay closed the session: {code:?} {message}");
                    return Ok(if reconnect {
                        Reconnect::Soon { after: None }
                    } else {
                        Reconnect::Stop(message)
                    });
                }
            }
        }
    }
}

/// Ceiling on the reconnect backoff. Long enough that a relay that is down for
/// an hour is not being hammered; short enough that a laptop waking up is back
/// within a few minutes.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How many consecutive refusals-that-waiting-cannot-fix the connector rides out
/// before it parks and waits for the owner.
///
/// Sized against the relay's staleness, not picked round: it refreshes its
/// routing table every 15 seconds and backs off to 60 seconds while the control
/// plane is unreachable, so the retry schedule below has to cover roughly a
/// minute. With the doubling backoff that is what six attempts buys — see
/// `a_freshly_enrolled_connector_retries_past_a_stale_routing_table`.
const TERMINAL_REFUSALS: u32 = 6;

/// The other half of the sizing, enforced at **compile** time.
///
/// The lower bound (does the retry window cover the relay's worst-case table
/// staleness?) is a runtime test because it depends on the backoff curve. This
/// upper bound does not, and it is the one that matters for the relay: an
/// unbounded retry turns a revoked fleet into a denial of service against the
/// thing that revoked it. As a `const` assertion it cannot be optimised out, and
/// raising the constant past the bound fails the build rather than a test run.
const _: () = assert!(
    TERMINAL_REFUSALS <= 8,
    "an unbounded retry is the reconnect storm"
);

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(MAX_BACKOFF)
}

/// What to do after a session ends.
enum Reconnect {
    /// Transient: come back after a short backoff.
    ///
    /// `after` carries the relay's own `retry_after_secs` when it sent one, and
    /// it is honoured rather than advisory. Without it, a retryable refusal —
    /// `Disabled` or `AlreadyConnected`, both of which the relay answers with 15
    /// seconds — reset the backoff to one second, so a refused installation
    /// re-dialled roughly once a second for as long as the refusal lasted.
    /// Harmless for one machine, and precisely the storm the guard exists for at
    /// fleet scale.
    Soon { after: Option<Duration> },
    /// Do not retry until the owner changes something. Carries the reason shown
    /// in Settings.
    Stop(String),
}

/// The forwarding half, deliberately a separate type holding **only** a port.
///
/// It cannot reach configuration, the relay session, or anything the wire
/// influenced, which is invariant 9 enforced by the type rather than by a
/// comment asking the next person not to.
struct ConnectorForward {
    remote_port: u16,
}

/// Counts one proxied stream for as long as it lives.
///
/// A guard rather than a pair of increment/decrement calls because a forwarding
/// task can end at any await — a closed socket, a timeout, the session dropping —
/// and every early return would otherwise have to remember to decrement. One
/// missed path and the count only ever climbs, which is worse than not showing it.
struct StreamGuard(Arc<Connector>);

impl StreamGuard {
    fn open(connector: &Arc<Connector>) -> Self {
        connector
            .live_streams
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(Arc::clone(connector))
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0
            .live_streams
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ConnectorForward {
    async fn forward(&self, stream: yamux::Stream, _counted: StreamGuard) {
        use tokio_util::compat::FuturesAsyncReadCompatExt as _;
        // 127.0.0.1, always, and the port from local configuration. There is no
        // branch here and there must never be one.
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], self.remote_port));
        let local = match tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect(target),
        )
        .await
        {
            Ok(Ok(local)) => local,
            _ => {
                // The strict listener is always bound (it is loopback), so this
                // is a real fault rather than "remote access is off" — that case
                // is answered by the listener itself with a 503, which is what a
                // browser should see.
                tracing::warn!("connector: cannot reach the strict ingress on {target}");
                return;
            }
        };
        let _ = local.set_nodelay(true);
        let mut stream = stream.compat();
        let mut local = local;
        if let Err(e) = tokio::io::copy_bidirectional(&mut stream, &mut local).await {
            // Both halves closing at once is the normal end of an HTTP request.
            tracing::trace!("connector stream ended: {e}");
        }
    }
}

/// `https://<hostname>`, or empty before enrollment.
fn public_origin(hostname: &str) -> String {
    if hostname.is_empty() {
        String::new()
    } else {
        format!("https://{hostname}")
    }
}

/// The public trust anchors, for the relay's Let's Encrypt certificate.
fn webpki_root_certs() -> Vec<rustls_pki_types::TrustAnchor<'static>> {
    webpki_roots::TLS_SERVER_ROOTS.to_vec()
}

/// Extra CA certificates from `THREADKNOT_RELAY_CA_PEM`, a path to a PEM bundle.
///
/// **Testing only.** It exists so an integration test can point the connector at
/// a relay on this machine holding a throwaway self-signed certificate, which
/// the public roots above cannot verify. Unset — the production case — this
/// returns nothing and the trust store is exactly the web PKI.
fn extra_ca_anchors() -> Result<Vec<rustls_pki_types::CertificateDer<'static>>> {
    let Some(path) = std::env::var_os("THREADKNOT_RELAY_CA_PEM") else {
        return Ok(Vec::new());
    };
    let pem = std::fs::read(&path)
        .with_context(|| format!("reading THREADKNOT_RELAY_CA_PEM {:?}", path))?;
    let certs = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .context("parsing THREADKNOT_RELAY_CA_PEM")?;
    anyhow::ensure!(
        !certs.is_empty(),
        "THREADKNOT_RELAY_CA_PEM contained no CERTIFICATE block"
    );
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("threadknot-connector-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_identity_is_generated_once_and_kept_owner_only() {
        let dir = tmpdir();
        let connector = Connector::open(&dir, 42801).unwrap();
        let first = connector.public_key_b64().unwrap();
        let again = connector.public_key_b64().unwrap();
        // Regenerating would orphan the installation: the control plane knows
        // this key, and a new one authenticates as nobody.
        assert_eq!(first, again);
        // Reopening the store must find the same identity on disk.
        let reopened = Connector::open(&dir, 42801).unwrap();
        assert_eq!(reopened.public_key_b64().unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("connector.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_fresh_install_is_off_and_unenrolled_and_says_so() {
        let dir = tmpdir();
        let connector = Connector::open(&dir, 42801).unwrap();
        assert_eq!(connector.status().state, "off");
        assert!(connector.status().hostname.is_empty());
        assert!(connector.status().public_origin.is_empty());

        // Enabling without enrolling must report `unenrolled`, not `connecting`.
        // "Connecting" forever is the failure mode that generates support
        // tickets; the person needs to be told they have a step left.
        connector.set_enabled(true).unwrap();
        assert_eq!(connector.status().state, "unenrolled");
        connector.set_enabled(false).unwrap();
        assert_eq!(connector.status().state, "off");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn enabled_state_survives_a_restart_but_never_invents_a_hostname() {
        let dir = tmpdir();
        {
            let connector = Connector::open(&dir, 42801).unwrap();
            connector.set_enabled(true).unwrap();
        }
        let reopened = Connector::open(&dir, 42801).unwrap();
        assert!(reopened.config().enabled);
        // Invariant 10: the hostname is server-assigned. Nothing here may guess
        // one, so it stays empty until the control plane says otherwise.
        assert!(reopened.config().hostname.is_empty());
        assert!(reopened.status().public_origin.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn enrollment_signs_the_token_bound_to_the_key() {
        let dir = tmpdir();
        let connector = Connector::open(&dir, 42801).unwrap();
        let key = connector.signing_key().unwrap();
        let public = connector.public_key_b64().unwrap();
        let signature = key.sign(&proto::enroll_message("tok_abc", &public));

        // The control plane verifies exactly this. If the two sides disagreed
        // about the message, enrollment would fail with a signature error and no
        // indication of why — which is the whole reason the message builders live
        // in the shared crate.
        assert!(key
            .verifying_key()
            .verify_strict(&proto::enroll_message("tok_abc", &public), &signature)
            .is_ok());
        // A different token must not verify, or a captured signature would
        // enroll against any token.
        assert!(key
            .verifying_key()
            .verify_strict(&proto::enroll_message("tok_other", &public), &signature)
            .is_err());
        // And a session-auth signature must not enroll (domain separation).
        let auth = key.sign(&proto::auth_message(b"nonce", "inst"));
        assert!(key
            .verifying_key()
            .verify_strict(&proto::enroll_message("tok_abc", &public), &auth)
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_freshly_enrolled_connector_retries_past_a_stale_routing_table() {
        // The bug this guards: the relay routes from a table it refreshes every
        // 15 seconds, so a connector dialling the instant enrollment returns is
        // refused as `Unauthenticated` — byte-identical to a revocation, because
        // invariant 12 forbids the relay from telling them apart. Parking on the
        // first one left a machine that had just enrolled offline until its owner
        // toggled remote access off and on again.
        let mut backoff = Duration::from_secs(1);
        let mut window = Duration::ZERO;
        for _ in 1..TERMINAL_REFUSALS {
            backoff = next_backoff(backoff);
            window += backoff;
        }
        assert!(
            window >= Duration::from_secs(60),
            "the retry window ({window:?}) must cover the relay's worst-case table staleness: \
             a 15s refresh interval that backs off to 60s while the control plane is unreachable"
        );
        // The upper bound that keeps the storm guard is a `const` assertion at the
        // definition instead, so it is checked at compile time rather than being
        // optimised out here.
        assert_eq!(next_backoff(MAX_BACKOFF), MAX_BACKOFF, "the backoff has a ceiling");
    }

    #[test]
    fn reconnect_jitter_scales_with_the_delay_it_spreads() {
        // The property that matters is proportionality. A fixed-width jitter is
        // indistinguishable from none once the backoff is minutes long, which is
        // exactly the case a herd forms in: a sustained relay outage, every
        // connector at the ceiling, all re-dialling together.
        let jitter_for = |delay: Duration, roll: u64| {
            Duration::from_millis((delay.as_millis() as u64).saturating_mul(roll % 500) / 1000)
        };
        // At the ceiling, the spread must be tens of seconds, not half a second.
        let ceiling = MAX_BACKOFF;
        let widest = jitter_for(ceiling, 499);
        assert!(
            widest > Duration::from_secs(60),
            "jitter at the {ceiling:?} ceiling was only {widest:?} — a fixed cap is back"
        );
        // Never more than +50%, or a backoff ceiling stops being a ceiling.
        assert!(widest < ceiling / 2);
        // And it scales: ten times the delay is ten times the spread.
        assert_eq!(
            jitter_for(Duration::from_secs(10), 400).as_millis(),
            jitter_for(Duration::from_secs(1), 400).as_millis() * 10
        );
        // A zero roll must be legal — no jitter is a valid draw, not a panic.
        assert_eq!(jitter_for(ceiling, 0), Duration::ZERO);
    }

    #[test]
    fn a_relay_named_retry_delay_wins_over_our_own_optimism() {
        // The relay sends 15s for `Disabled` and `AlreadyConnected`. Treating a
        // retryable refusal as "come back in a second" — which is right for a
        // relay restart — means a refused installation re-dials once a second for
        // as long as the refusal lasts. One machine doing that is nothing; a
        // fleet doing it is the storm the guard exists for.
        let ours = Duration::from_secs(1);
        // Bound through a typed variable rather than written as `Some(x)` inline:
        // the point is to exercise the shape the supervisor actually evaluates on
        // a `Reconnect::Soon { after }`, and clippy is right that a literal
        // `Some(_).unwrap_or(_)` would be a no-op worth deleting.
        let named = |after: Option<Duration>| after.unwrap_or(ours).max(ours);

        assert_eq!(named(Some(Duration::from_secs(15))), Duration::from_secs(15));
        // And a relay that names nothing must not slow us down: a rolling restart
        // should bring every connector straight back.
        assert_eq!(named(None), ours);
        // A relay naming something *shorter* than our floor must not be able to
        // talk us into a tighter loop than we would choose ourselves.
        assert_eq!(
            named(Some(Duration::from_millis(10))),
            ours,
            "the relay may slow us down, never speed us up"
        );
    }

    #[test]
    fn live_streams_are_counted_by_a_guard_that_cannot_be_forgotten() {
        let dir = tmpdir();
        let connector = Connector::open(&dir, 42801).unwrap();
        assert_eq!(connector.status().live_streams, 0);
        {
            let _a = StreamGuard::open(&connector);
            assert_eq!(connector.status().live_streams, 1);
            let _b = StreamGuard::open(&connector);
            assert_eq!(connector.status().live_streams, 2);
        }
        // A forwarding task can end at any await, so the decrement has to happen
        // on every path. Drop is the only way to get that right — a missed
        // early return would make the count climb forever.
        assert_eq!(connector.status().live_streams, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_refusal_that_waiting_cannot_fix_stops_retrying() {
        // The reconnect-storm guard. A revoked or unsubscribed installation that
        // kept retrying would turn a fleet of disabled machines into a denial of
        // service against the relay that disabled them.
        assert!(!proto::RefusalCode::Unauthenticated.is_retryable());
        assert!(!proto::RefusalCode::Unsubscribed.is_retryable());
        assert!(!proto::RefusalCode::UnsupportedVersion.is_retryable());
        assert!(proto::RefusalCode::Unavailable.is_retryable());
        assert!(proto::RefusalCode::AlreadyConnected.is_retryable());
    }

    #[test]
    fn the_forwarder_can_only_reach_loopback() {
        // Invariant 9, asserted structurally. `ConnectorForward` holds a port and
        // nothing else, and `forward` builds `127.0.0.1` from a literal — so
        // there is no field, and no protocol message, that could redirect it.
        // If someone adds one, this test is where they should have to argue for
        // it.
        let forward = ConnectorForward { remote_port: 42801 };
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], forward.remote_port));
        assert!(target.ip().is_loopback());
        assert_eq!(std::mem::size_of::<ConnectorForward>(), std::mem::size_of::<u16>());
    }

    #[test]
    fn the_relay_endpoint_defaults_to_the_protocol_sni() {
        // Read from the shared crate rather than duplicated here: a connector
        // pointed at the wrong SNI lands on the data plane instead of the
        // connector endpoint and fails in a way that looks like an outage.
        let (host, port, sni) = relay_endpoint();
        if std::env::var("THREADKNOT_RELAY_HOST").is_err() {
            assert_eq!(host, proto::CONNECTOR_SNI);
            assert_eq!(port, 443);
            assert_eq!(sni, proto::CONNECTOR_SNI);
        }
        assert!(control_base().starts_with("https://") || control_base().starts_with("http://"));
    }
}

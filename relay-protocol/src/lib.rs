//! The wire contract between a Threadknot installation's **connector**, the
//! **relay** data plane, and the **control plane**.
//!
//! It lives in its own crate so the three cannot drift: a change that would
//! break the connector fails to compile the relay in the same `cargo build`.
//!
//! # Shape of the system
//!
//! ```text
//!  phone/browser ──TLS 443──▶ relay ──yamux stream──▶ connector ──▶ 127.0.0.1:<remote port>
//!    SNI: <hostname>.remote.threadknot.ai        (one TCP connection per stream)
//! ```
//!
//! The relay routes by **SNI only** and then pipes bytes. It never parses HTTP,
//! which is why WebSockets, streaming responses, range requests and binary
//! terminal/screencast frames all work: nothing interprets them. It is also why
//! the whole request-smuggling/desync bug class is absent.
//!
//! # What is deliberately NOT here
//!
//! No per-stream framing. Once the relay opens a yamux stream, every byte on it
//! is the proxied connection verbatim. The connector therefore cannot be told
//! *where* to connect — it always dials Threadknot's own strict loopback
//! listener. That is security invariant 9, expressed as an absence: compromising
//! the relay must not turn every installation into an SSRF proxy into its
//! owner's network, and the only way to guarantee that is for the protocol to
//! have no field that could carry a target.

use serde::{Deserialize, Serialize};

pub mod frame;
pub mod limits;

pub use frame::{read_frame, write_frame, FrameError, MAX_FRAME};

/// Bumped on any incompatible change to the handshake below. The relay refuses
/// a connector that does not match, with a message the desktop shows verbatim —
/// "update Threadknot" is actionable, a closed socket is not.
pub const PROTOCOL_VERSION: u32 = 1;

/// Domain-separation prefix for the connector's proof of key possession.
///
/// Signing a bare nonce is how a signature made for one purpose gets replayed
/// as another. Every signature in this protocol covers a constant naming what
/// it authorises, so a connector-auth signature can never be presented as, say,
/// a control-plane registration.
pub const CONNECTOR_AUTH_CONTEXT: &[u8] = b"threadknot-connector-auth-v1";

/// Same, for a connector proving key possession to the control plane during
/// enrollment.
pub const ENROLL_CONTEXT: &[u8] = b"threadknot-connector-enroll-v1";

/// Adding a key to an installation, signed by a key it already has.
pub const ROTATE_CONTEXT: &[u8] = b"threadknot-connector-rotate-v1";

/// Retiring a key, signed by a key that is still active.
pub const REVOKE_CONTEXT: &[u8] = b"threadknot-connector-revoke-v1";

/// The SNI a connector dials to reach the relay's connector endpoint.
pub const CONNECTOR_SNI: &str = "connect.remote.threadknot.ai";

/// The SNI the control plane answers on.
pub const CONTROL_SNI: &str = "remote.threadknot.ai";

/// The DNS suffix under which installations are addressed. An installation's
/// hostname is `<label>.remote.threadknot.ai`, and `label` is assigned by the
/// control plane — never chosen by the connector (invariant 10).
pub const INSTALLATION_SUFFIX: &str = ".remote.threadknot.ai";

// ---------------------------------------------------------------------------
// Connector <-> relay handshake
// ---------------------------------------------------------------------------

/// Connector's opening frame, sent immediately after the TLS handshake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorHello {
    pub protocol_version: u32,
    /// The installation this connector claims to be, as assigned at enrollment.
    pub installation_id: String,
    /// Base64 (standard, padded) Ed25519 public key, 32 bytes decoded.
    pub public_key: String,
    /// Human-readable, for the console's "last seen from" only. Advisory: the
    /// relay must never make a routing or authorisation decision on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

/// Relay's challenge. The nonce is fresh per connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayChallenge {
    /// Base64, 32 random bytes.
    pub nonce: String,
}

/// Connector's proof that it holds the private key for `public_key`.
///
/// The signed message is
/// `CONNECTOR_AUTH_CONTEXT || nonce_bytes || installation_id_bytes`, built by
/// [`auth_message`] so both sides cannot disagree about it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorAuth {
    /// Base64 Ed25519 signature, 64 bytes decoded.
    pub signature: String,
}

/// Exactly what a connector signs, and exactly what the relay verifies.
pub fn auth_message(nonce: &[u8], installation_id: &str) -> Vec<u8> {
    let mut msg = Vec::with_capacity(CONNECTOR_AUTH_CONTEXT.len() + nonce.len() + installation_id.len());
    msg.extend_from_slice(CONNECTOR_AUTH_CONTEXT);
    msg.extend_from_slice(nonce);
    msg.extend_from_slice(installation_id.as_bytes());
    msg
}

/// What a connector signs when enrolling with the control plane: the enrollment
/// token it was given, bound to the key it is registering.
pub fn enroll_message(enrollment_token: &str, public_key_b64: &str) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(ENROLL_CONTEXT);
    msg.extend_from_slice(enrollment_token.as_bytes());
    msg.extend_from_slice(public_key_b64.as_bytes());
    msg
}

/// What a connector signs to **add** a key, using a key it already holds.
pub fn rotate_message(installation_id: &str, new_public_key_b64: &str) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(ROTATE_CONTEXT);
    msg.extend_from_slice(installation_id.as_bytes());
    msg.extend_from_slice(new_public_key_b64.as_bytes());
    msg
}

/// What a connector signs to **retire** a key.
///
/// A distinct context from `rotate_message`, so a captured add-a-key signature
/// cannot be replayed as a retire-a-key one. Getting that wrong would let anyone
/// who saw a rotation revoke the installation's remaining key and take the
/// machine off the relay.
pub fn revoke_message(installation_id: &str, revoked_public_key_b64: &str) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(REVOKE_CONTEXT);
    msg.extend_from_slice(installation_id.as_bytes());
    msg.extend_from_slice(revoked_public_key_b64.as_bytes());
    msg
}

/// Relay's verdict on the handshake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RelayVerdict {
    /// Authenticated. The session now carries yamux; the relay opens streams,
    /// the connector accepts them.
    Ready(SessionReady),
    /// Refused. The connector shows `message` to the owner and applies
    /// `retry_after_secs` before trying again.
    Refused(SessionRefused),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionReady {
    /// The full public hostname, e.g. `quiet-harbor-4821.remote.threadknot.ai`.
    /// Assigned by the control plane; the connector displays it and nothing else.
    pub hostname: String,
    /// Whether the subscription currently permits *new* sessions. When false,
    /// the tunnel still carries existing traffic — a trial that lapses mid-turn
    /// must not sever the turn (Stage 5: "never mid-session").
    pub accepting_new_sessions: bool,
    /// Why not, when `accepting_new_sessions` is false. Shown verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
    /// Days left in the trial, so the desktop can warn *before* anything stops
    /// working.
    ///
    /// `hold_reason` cannot do this job: it is only populated once the hold is
    /// already in effect, so a connector that watched only that field found out
    /// on the day it broke. `None` means no trial is in play — a paid
    /// subscription, or a lapse that is no longer a trial question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_days_left: Option<i64>,
    pub limits: limits::SessionLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRefused {
    pub code: RefusalCode,
    /// Safe to show a user. Must not distinguish "no such installation" from
    /// "wrong key" (invariant 12) — both are `Unauthenticated` with one message.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RefusalCode {
    /// Protocol version mismatch — the desktop needs updating.
    UnsupportedVersion,
    /// Unknown installation, wrong key, bad signature, or revoked key. One code
    /// on purpose: distinguishing them enumerates installations.
    Unauthenticated,
    /// Killed by the operator or by the owner from the console.
    Disabled,
    /// Subscription lapsed past its grace period.
    Unsubscribed,
    /// Another connector already holds this installation's session.
    AlreadyConnected,
    /// Relay is shutting down or over capacity; retry.
    Unavailable,
}

impl RefusalCode {
    /// Whether a connector should keep retrying. A refusal it cannot fix by
    /// waiting should back off hard rather than hammer the relay.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            RefusalCode::Unavailable | RefusalCode::AlreadyConnected | RefusalCode::Disabled
        )
    }
}

// ---------------------------------------------------------------------------
// Session control (a reserved yamux stream, id 0 by convention of being first)
// ---------------------------------------------------------------------------

/// Messages on the session's control stream, which the relay opens first and
/// keeps for the life of the session. Everything here is *metadata* — the
/// control stream never carries proxied bytes, and proxied streams never carry
/// control messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlMessage {
    /// Relay -> connector. Liveness, and the relay's view of this session, so
    /// the desktop's Settings panel can show something true without polling an
    /// API. Sent on a timer.
    Heartbeat {
        /// Bytes this session has carried since it connected.
        bytes_in: u64,
        bytes_out: u64,
        /// Fair-use position, so the desktop can warn before it throttles.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        month_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        month_quota_bytes: Option<u64>,
    },
    /// Connector -> relay. Answers a heartbeat; the relay uses the gap to
    /// detect a wedged connector that has not closed its TCP socket.
    HeartbeatAck,
    /// Relay -> connector. Entitlement changed under a live session: the
    /// console flipped a kill switch, or a subscription lapsed. Applies
    /// *without* dropping the tunnel, which is what makes "never mid-session"
    /// implementable.
    Entitlement {
        accepting_new_sessions: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold_reason: Option<String>,
        /// Carried here too, so a countdown crossing a day boundary reaches a
        /// desktop that has been connected for a week without it reconnecting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trial_days_left: Option<i64>,
    },
    /// Relay -> connector. Close the session. `reconnect` distinguishes "we are
    /// restarting, come back" from "you are revoked, stop" — without it, a
    /// revoked connector reconnect-storms forever.
    Close {
        code: RefusalCode,
        message: String,
        reconnect: bool,
    },
}

// ---------------------------------------------------------------------------
// Control plane API (JSON over HTTPS at CONTROL_SNI)
// ---------------------------------------------------------------------------

/// `POST /v1/connector/enroll` — a desktop turning remote access on for the
/// first time. Authenticated by a short-lived enrollment token the owner copies
/// from the console, plus a signature proving it holds the key it registers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub enrollment_token: String,
    /// Base64 Ed25519 public key.
    pub public_key: String,
    /// Shown in the console's machine list. Advisory only.
    pub machine_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// Base64 signature over [`enroll_message`].
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub installation_id: String,
    /// Server-assigned. The connector stores and displays it; it never picks it.
    pub hostname: String,
    /// The origin to put in a remote pairing QR: `https://<hostname>`.
    pub public_origin: String,
}

// ---------------------------------------------------------------------------
// Device approval — enrolling without carrying a secret
// ---------------------------------------------------------------------------

/// How the desktop enrolls itself without anyone copying a token.
///
/// The token flow works but puts a secret on screen and asks a human to move it
/// between two applications, which is both the most confusing step of setup and
/// the only place in the product where a credential is displayed. This replaces
/// it with the device-authorization shape used by TVs and CLIs: the desktop
/// starts the request, opens the browser itself, and the person only ever presses
/// *Approve*.
///
/// The security properties that matter:
///
///   * The desktop generates its keypair first and the request is signed, so the
///     approval binds the org to **that** public key. A different key cannot be
///     substituted after the fact.
///   * `device_code` is the desktop's secret and is stored only as a hash. It is
///     required — with a signature — to collect the result, so knowing the
///     `user_code` is not enough to steal the enrollment.
///   * `user_code` is short because a human may have to read it. It is an
///     identifier for the approval page, never a credential: approving happens
///     under the approver's own Clerk session, and the page has to name the
///     machine so nobody approves a request they did not start. That last point
///     is the phishing surface of this pattern and it is a copy problem, not a
///     cryptography one.
pub const DEVICE_START_CONTEXT: &[u8] = b"threadknot-device-start-v1";
pub const DEVICE_POLL_CONTEXT: &[u8] = b"threadknot-device-poll-v1";

/// Signed by the desktop over the key it is registering, so a `start` cannot be
/// replayed to register somebody else's key.
pub fn device_start_message(public_key_b64: &str, machine_name: &str) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(DEVICE_START_CONTEXT);
    msg.extend_from_slice(public_key_b64.as_bytes());
    msg.extend_from_slice(machine_name.as_bytes());
    msg
}

/// Domain-separated from `device_start_message` so a captured start signature is
/// not also a valid poll signature.
pub fn device_poll_message(device_code: &str) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(DEVICE_POLL_CONTEXT);
    msg.extend_from_slice(device_code.as_bytes());
    msg
}

/// `POST /v1/connector/device/start`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStartRequest {
    /// Base64 Ed25519 public key, generated on the desktop moments earlier.
    pub public_key: String,
    pub machine_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// Base64 signature over [`device_start_message`].
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStartResponse {
    /// The desktop's secret for collecting the result. Never displayed.
    pub device_code: String,
    /// Short and unambiguous, for a person to read aloud or type on a phone when
    /// the desktop has no browser to open.
    pub user_code: String,
    /// Where to approve, without the code — for typing by hand.
    pub verification_uri: String,
    /// The same page with the code already filled in. This is what the desktop
    /// opens and what it renders as a QR, so the normal path involves no typing.
    pub verification_uri_complete: String,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

/// `POST /v1/connector/device/poll`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePollRequest {
    pub device_code: String,
    /// Base64 signature over [`device_poll_message`].
    pub signature: String,
}

/// One shape for every outcome, because the desktop has to distinguish "keep
/// waiting" from "stop" without guessing from an HTTP status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum DevicePollResponse {
    /// Nobody has answered yet. Wait `interval_secs` and ask again.
    Pending,
    /// Approved. The installation exists and this is its identity.
    Approved(EnrollResponse),
    /// Explicitly refused in the console. Terminal — do not retry.
    Denied,
    /// The window closed. Terminal for this code; starting a new one is fine.
    Expired,
}

/// What the console shows before anyone presses Approve. Deliberately includes
/// the machine name and when the request was made: approving is a decision about
/// a specific computer at a specific moment, and a page that cannot say which
/// computer is a page that trains people to approve anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRequestSummary {
    pub user_code: String,
    pub machine_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// RFC 3339.
    pub requested_at: String,
    pub expires_at: String,
    /// Already answered or timed out — the page shows the outcome rather than a
    /// live button.
    pub status: String,
}

/// `POST /v1/usage` — the relay reporting hourly aggregates. Loopback only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBatch {
    pub samples: Vec<UsageSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSample {
    pub installation_id: String,
    /// Start of the hour, RFC 3339 UTC.
    pub hour: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub peak_concurrent_streams: u32,
}

/// `GET /v1/relay/installations` — what the relay loads at boot and refreshes,
/// so an SNI lookup is an in-memory map read and never a database query. The
/// hot path cannot afford Postgres, and the reconnect path must not depend on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingTable {
    pub entries: Vec<RoutingEntry>,
    /// Monotonic; the relay ignores a table older than the one it holds.
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RoutingEntry {
    pub installation_id: String,
    /// The `<label>` part only — the relay appends [`INSTALLATION_SUFFIX`]
    /// itself, so a malformed row cannot claim `app.threadknot.ai`.
    pub hostname_label: String,
    /// Base64 Ed25519 public keys accepted for this installation. Plural for
    /// rotation: the new key is added, the connector reconnects, the old key is
    /// dropped — no window where the machine is unreachable.
    pub public_keys: Vec<String>,
    /// Operator or owner kill switch. A disabled installation gets the same
    /// generic offline response as an unknown one.
    pub enabled: bool,
    pub accepting_new_sessions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
    /// Passed through to the connector in `SessionReady`. The relay does not
    /// interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_days_left: Option<i64>,
    /// Bytes used this calendar month, for the fair-use throttle.
    #[serde(default)]
    pub month_bytes: u64,
    pub limits: limits::SessionLimits,
}

/// The generic body an unknown, disabled or offline installation gets.
///
/// One response for all three cases, and no installation identifier anywhere in
/// it: invariant 12. If "unknown host" and "host exists but is offline" differed
/// by a byte, the relay would be an installation enumerator.
pub const OFFLINE_BODY: &str = "This Threadknot is not reachable right now.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_message_is_domain_separated_and_binds_the_installation() {
        let a = auth_message(b"nonce-bytes", "inst-1");
        let b = auth_message(b"nonce-bytes", "inst-2");
        assert_ne!(a, b, "a signature for one installation must not verify for another");
        assert!(a.starts_with(CONNECTOR_AUTH_CONTEXT));
        // No two contexts may collide, or a signature made for one purpose is
        // replayable as another. The rotate/revoke pair is the one that matters
        // most: anyone who saw a rotation could otherwise revoke the
        // installation's last key and take the machine off the relay.
        let messages = [
            auth_message(b"x", "inst"),
            enroll_message("x", "inst"),
            rotate_message("inst", "x"),
            revoke_message("inst", "x"),
        ];
        for (i, a) in messages.iter().enumerate() {
            for b in messages.iter().skip(i + 1) {
                assert_ne!(a, b, "contexts must not collide");
            }
        }
    }

    #[test]
    fn a_revoked_connector_does_not_retry_forever() {
        // The two that mean "stop asking" must not be retryable, or a revoked
        // or unsubscribed installation becomes a reconnect storm.
        assert!(!RefusalCode::Unauthenticated.is_retryable());
        assert!(!RefusalCode::Unsubscribed.is_retryable());
        assert!(!RefusalCode::UnsupportedVersion.is_retryable());
        assert!(RefusalCode::Unavailable.is_retryable());
    }

    #[test]
    fn verdict_round_trips_as_a_tagged_enum() {
        let v = RelayVerdict::Refused(SessionRefused {
            code: RefusalCode::Unauthenticated,
            message: "not authorised".into(),
            retry_after_secs: Some(60),
        });
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<RelayVerdict>(&json).unwrap(), v);
    }
}

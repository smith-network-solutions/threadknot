//! Mesh cryptographic identity (SEC-012).
//!
//! Before this existed, a peer connection was `ws://<addr>/ws?token=<the peer's
//! MASTER token>`. Three separate problems in one string:
//!
//! 1. The credential was the peer's *master* token — fleet-level authority, the
//!    single most valuable secret on that machine.
//! 2. It was in a **URL**, which lands in proxy logs, shell history, crash
//!    reports and anything that records a request line.
//! 3. It was **plaintext**, so any observer on the LAN simply read it.
//!
//! All three are addressed here, and the fix is deliberately not "add TLS":
//!
//! - **Identity.** Each machine mints a self-signed certificate authority once
//!   and keeps it for the life of the install. Pairing exchanges the CA
//!   certificate, so from then on a peer is authenticated by the key it holds
//!   rather than by whatever answered at an address. That is what the threat
//!   model means by "authenticate peer identity independently of an address": a
//!   reused DHCP lease, an mDNS spoof, or an attacker who simply takes the
//!   address cannot present the pinned CA.
//! - **Credentials.** The token a peer presents is a dedicated per-pair secret,
//!   rotatable on its own, and each direction has its own. It is carried in an
//!   `Authorization` header, never in a URL.
//! - **Confidentiality.** The transport is TLS, verified against the pinned CA.
//!
//! # Why the leaf name is synthetic
//!
//! The certificate's subject alternative name is `<machine-id>.threadknot.mesh`
//! — a name that resolves nowhere. Addresses are DHCP-disposable hints in this
//! system and the machine id is the only durable key, so pinning a certificate
//! to an IP would break on every lease change. Instead the connecting side
//! overrides name resolution for that synthetic name to the address it is
//! currently trying. TLS then verifies "is this really machine X" while the
//! address stays free to move, which is exactly the invariant `peernet` already
//! relies on everywhere else.
//!
//! # Why server-authenticated TLS plus a bearer credential, and not mTLS
//!
//! mTLS would authenticate the client with a certificate too. It is not used
//! because the client is *already* authenticated by a per-peer credential that
//! is independently rotatable and revocable, and because a client-certificate
//! verifier needs its root set rebuilt every time a peer is added or removed —
//! a live-reload path with nothing to gain. The certificate authenticates the
//! *server*, which is the half a bearer token cannot do.

use anyhow::{Context, Result};
use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// DNS suffix for the synthetic per-machine certificate name. Resolves nowhere
/// on purpose — see the module docs.
pub const MESH_DNS_SUFFIX: &str = ".threadknot.mesh";

/// Header a peer presents its credential in. A header rather than a query
/// parameter because a URL is recorded by everything in the path.
pub const PEER_CREDENTIAL_HEADER: &str = "authorization";

/// The synthetic TLS name for a machine id.
pub fn mesh_dns_name(machine_id: &str) -> String {
    format!("{machine_id}{MESH_DNS_SUFFIX}")
}

/// Mint a fresh peer credential. 32 bytes of OS randomness, URL-safe so it can
/// never need escaping in any context it might end up in.
pub fn mint_credential() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Hash a credential for storage. Same construction as `mobile::hash_credential`
/// so there is one answer to "how do we store a bearer secret" in this codebase.
pub fn hash_credential(credential: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(credential.as_bytes());
    hex::encode(hasher.finalize())
}

/// Compare two secrets without leaking where they first differ.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// This machine's mesh identity: a self-signed CA plus the leaf it signs.
///
/// Two certificates rather than one self-signed leaf because `rustls` builds a
/// path to a *trust anchor*: a bare self-signed leaf is not usable as its own
/// anchor, so pinning one would mean writing a custom verifier and hand-rolling
/// the checks webpki already does correctly.
pub struct MeshIdentity {
    /// PEM of the CA certificate. This is what pairing hands to the other side.
    pub ca_pem: String,
    /// The leaf chain and key, in the form `rustls` wants for a server config.
    leaf_chain: Vec<rustls_pki_types::CertificateDer<'static>>,
    leaf_key: rustls_pki_types::PrivateKeyDer<'static>,
}

impl MeshIdentity {
    /// Load this machine's identity, generating it on first run.
    ///
    /// Regenerating would silently unpair every peer (they hold the old CA), so
    /// an existing identity is always reused, and a corrupt one is a hard error
    /// rather than a quiet re-mint.
    pub fn load_or_create(dir: &Path, machine_id: &str) -> Result<Self> {
        let ca_cert = dir.join("mesh-ca.pem");
        let ca_key = dir.join("mesh-ca.key");
        let leaf_cert = dir.join("mesh-leaf.pem");
        let leaf_key = dir.join("mesh-leaf.key");

        if [&ca_cert, &ca_key, &leaf_cert, &leaf_key].iter().all(|p| p.exists()) {
            // Repair permissions on every open, not only at creation: a file
            // restored from a backup or copied by hand arrives world-readable.
            crate::store::restrict_file(&ca_key);
            crate::store::restrict_file(&leaf_key);
            return Self::from_files(&ca_cert, &leaf_cert, &leaf_key);
        }

        Self::generate(machine_id, &ca_cert, &ca_key, &leaf_cert, &leaf_key)
    }

    fn generate(
        machine_id: &str,
        ca_cert_path: &Path,
        ca_key_path: &Path,
        leaf_cert_path: &Path,
        leaf_key_path: &Path,
    ) -> Result<Self> {
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
            KeyUsagePurpose,
        };

        let ca_key = KeyPair::generate().context("generate mesh CA key")?;
        let mut ca_params = CertificateParams::default();
        ca_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, format!("Threadknot mesh CA {machine_id}"));
            dn
        };
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca = ca_params.self_signed(&ca_key).context("self-sign mesh CA")?;

        let leaf_key = KeyPair::generate().context("generate mesh leaf key")?;
        let mut leaf_params = CertificateParams::new(vec![mesh_dns_name(machine_id)])
            .context("mesh leaf params")?;
        leaf_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, mesh_dns_name(machine_id));
            dn
        };
        leaf_params.use_authority_key_identifier_extension = true;
        let issuer = Issuer::new(ca_params, &ca_key);
        let leaf = leaf_params
            .signed_by(&leaf_key, &issuer)
            .context("sign mesh leaf")?;

        // Certificates are public; only the keys are secret.
        std::fs::write(ca_cert_path, ca.pem()).context("write mesh CA cert")?;
        std::fs::write(leaf_cert_path, leaf.pem()).context("write mesh leaf cert")?;
        crate::store::write_private(ca_key_path, &ca_key.serialize_pem())?;
        crate::store::write_private(leaf_key_path, &leaf_key.serialize_pem())?;

        tracing::info!("minted this machine's mesh identity ({machine_id})");
        Self::from_files(ca_cert_path, leaf_cert_path, leaf_key_path)
    }

    fn from_files(ca_cert: &Path, leaf_cert: &Path, leaf_key: &Path) -> Result<Self> {
        let ca_pem = std::fs::read_to_string(ca_cert).context("read mesh CA cert")?;
        let leaf_pem = std::fs::read(leaf_cert).context("read mesh leaf cert")?;
        let key_pem = std::fs::read(leaf_key).context("read mesh leaf key")?;

        let leaf_chain: Vec<_> = rustls_pemfile::certs(&mut leaf_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .context("parse mesh leaf cert")?;
        anyhow::ensure!(!leaf_chain.is_empty(), "mesh leaf certificate is empty");
        let leaf_key = rustls_pemfile::private_key(&mut key_pem.as_slice())
            .context("parse mesh leaf key")?
            .context("mesh leaf key file holds no private key")?;

        Ok(Self {
            ca_pem,
            leaf_chain,
            leaf_key,
        })
    }

    /// A `rustls` server config for the mesh listener.
    ///
    /// No client-certificate verifier: the peer credential authenticates the
    /// caller (see the module docs for why that is the right split).
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>> {
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())
            .context("build mesh TLS server config")?;
        Ok(Arc::new(config))
    }
}

/// A `rustls` client config that trusts **only** `ca_pem` — the certificate
/// authority pinned for one specific peer at pairing time.
///
/// Built per peer rather than from one shared store with every peer's CA in it.
/// A shared store would let peer A's certificate satisfy a connection to peer B,
/// which is cross-peer impersonation by any paired machine — and paired does not
/// mean trusted to be another machine.
pub fn client_config_for_ca(ca_pem: &str) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    let certs: Vec<_> = rustls_pemfile::certs(&mut ca_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .context("parse peer mesh CA")?;
    anyhow::ensure!(!certs.is_empty(), "peer mesh CA is empty");
    for cert in certs {
        roots.add(cert).context("trust peer mesh CA")?;
    }
    // Built-in webroots are deliberately NOT added. A peer is authenticated by
    // its own CA and nothing else; trusting a public authority as well would
    // mean any certificate for `*.threadknot.mesh` from any CA on earth
    // impersonates a peer.
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Paths that hold mesh secrets, for the store's permission sweep.
pub fn secret_files(dir: &Path) -> Vec<PathBuf> {
    vec![dir.join("mesh-ca.key"), dir.join("mesh-leaf.key")]
}

// ---------------------------------------------------------------------------
// Pairing bootstrap
// ---------------------------------------------------------------------------

/// How long a pairing challenge is good for. Long enough for a human to paste a
/// URL, short enough that a captured challenge is worthless by the time anyone
/// could use it.
const CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// Domain separation for the pairing proof, so it cannot be replayed as
/// anything else this machine signs.
const PAIRING_PROOF_CONTEXT: &[u8] = b"threadknot-peer-pair-v2";

/// SHA-256 of a PEM certificate authority, as lowercase hex. Used to bind a
/// pairing proof to the certificate the initiator actually saw.
pub fn ca_fingerprint(ca_pem: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(ca_pem.as_bytes());
    hex::encode(hasher.finalize())
}

/// Prove knowledge of a machine's master token **without transmitting it**.
///
/// This is what makes pairing safe over a network with an active attacker on it,
/// and it is the piece that would be easy to get subtly wrong, so:
///
/// - The secret is the responder's master token, used as an HMAC key. It never
///   leaves the machine that holds it. The old exchange put it in a request body
///   and then stored it forever as the peer credential; now it is used once, as
///   a key, and never sent.
/// - The message binds the **fingerprint of the certificate authority the
///   initiator saw**. An attacker who intercepts the unauthenticated identity
///   fetch and substitutes their own CA receives a proof computed over *their*
///   fingerprint. The real machine recomputes with its own and the comparison
///   fails. So the classic trust-on-first-use hole — MITM the bootstrap, harvest
///   the secret — is closed rather than accepted.
/// - The message binds the initiator's machine id, so a proof captured from one
///   pairing cannot be replayed to pair a different machine.
/// - The challenge is single-use and short-lived, so it cannot be replayed at
///   all.
pub fn pairing_proof(
    master_token: &str,
    challenge: &str,
    initiator_machine_id: &str,
    responder_ca_pem: &str,
) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(master_token.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(PAIRING_PROOF_CONTEXT);
    mac.update(challenge.as_bytes());
    mac.update(b"\x00");
    mac.update(initiator_machine_id.as_bytes());
    mac.update(b"\x00");
    mac.update(ca_fingerprint(responder_ca_pem).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Outstanding pairing challenges this machine has issued.
///
/// In memory only, and deliberately: a challenge that survived a restart would
/// be a long-lived pairing credential, which is the opposite of the point.
#[derive(Default)]
pub struct PairingChallenges {
    issued: std::sync::Mutex<Vec<(String, std::time::Instant)>>,
}

impl PairingChallenges {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a challenge for an identity fetch.
    pub fn issue(&self) -> String {
        let challenge = mint_credential();
        let mut issued = self.issued.lock().unwrap();
        issued.retain(|(_, at)| at.elapsed() < CHALLENGE_TTL);
        // Bounded so an unauthenticated endpoint cannot be used to grow memory:
        // the identity fetch is public, so a script could otherwise mint
        // challenges until the process dies.
        if issued.len() >= 64 {
            issued.remove(0);
        }
        issued.push((challenge.clone(), std::time::Instant::now()));
        challenge
    }

    /// Consume a challenge. False if it is unknown, expired, or already used —
    /// single-use is what stops a captured proof being replayed.
    pub fn redeem(&self, challenge: &str) -> bool {
        let mut issued = self.issued.lock().unwrap();
        issued.retain(|(_, at)| at.elapsed() < CHALLENGE_TTL);
        match issued.iter().position(|(c, _)| c == challenge) {
            Some(index) => {
                issued.remove(index);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("threadknot-mesh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn identity_is_generated_once_and_reused() {
        let dir = tmpdir();
        let a = MeshIdentity::load_or_create(&dir, "machine-a").unwrap();
        let b = MeshIdentity::load_or_create(&dir, "machine-a").unwrap();
        // Regenerating would silently unpair every peer, which holds the old CA.
        assert_eq!(a.ca_pem, b.ca_pem, "identity must be stable across loads");
        assert!(a.server_config().is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mesh_private_keys_are_owner_only() {
        let dir = tmpdir();
        MeshIdentity::load_or_create(&dir, "machine-a").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in secret_files(&dir) {
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{} is {mode:o}", path.display());
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_peers_ca_does_not_validate_another_peers_certificate() {
        // The cross-peer impersonation case. Two machines, two CAs; a client
        // pinned to A's CA must not accept B's chain. Checked structurally here
        // (distinct anchors, and each config trusts exactly one) — the full
        // handshake path is covered by the peernet integration test.
        let dir_a = tmpdir();
        let dir_b = tmpdir();
        let a = MeshIdentity::load_or_create(&dir_a, "machine-a").unwrap();
        let b = MeshIdentity::load_or_create(&dir_b, "machine-b").unwrap();
        assert_ne!(a.ca_pem, b.ca_pem);
        assert!(client_config_for_ca(&a.ca_pem).is_ok());
        assert!(client_config_for_ca(&b.ca_pem).is_ok());
        // Garbage must be refused rather than producing an empty trust store,
        // which would accept nothing and fail confusingly at handshake time.
        assert!(client_config_for_ca("not a certificate").is_err());
        assert!(client_config_for_ca("").is_err());
        std::fs::remove_dir_all(dir_a).unwrap();
        std::fs::remove_dir_all(dir_b).unwrap();
    }

    #[test]
    fn credentials_are_unguessable_and_stored_hashed() {
        let a = mint_credential();
        let b = mint_credential();
        assert_ne!(a, b);
        // 32 bytes of entropy, base64url unpadded.
        assert_eq!(a.len(), 43);
        assert!(!a.contains('+') && !a.contains('/') && !a.contains('='));
        // The stored form must not contain the secret.
        let hash = hash_credential(&a);
        assert_ne!(hash, a);
        assert!(!hash.contains(&a));
        assert_eq!(hash, hash_credential(&a));
        assert_ne!(hash, hash_credential(&b));
        assert!(constant_time_eq(&hash, &hash_credential(&a)));
        assert!(!constant_time_eq(&hash, &hash_credential(&b)));
        assert!(!constant_time_eq("short", "longer"));
    }

    #[test]
    fn a_pairing_proof_binds_the_certificate_the_initiator_saw() {
        // The MITM case, and the reason this is a proof rather than a
        // transmitted token. An attacker who intercepts the unauthenticated
        // identity fetch and substitutes their own CA gets a proof computed over
        // THEIR fingerprint; the real machine recomputes with its own and
        // refuses. Without this binding, pairing would be trust-on-first-use
        // with the master token as the payload.
        let real_ca = "-----BEGIN CERTIFICATE-----\nreal\n-----END CERTIFICATE-----\n";
        let attacker_ca = "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n";
        let honest = pairing_proof("master", "chal", "machine-a", real_ca);
        let mitm = pairing_proof("master", "chal", "machine-a", attacker_ca);
        assert_ne!(honest, mitm);
        assert!(!constant_time_eq(&honest, &mitm));

        // Every other input is bound too: a proof cannot be replayed with a
        // different challenge, for a different initiator, or against a machine
        // whose master token differs.
        assert_ne!(honest, pairing_proof("master", "other", "machine-a", real_ca));
        assert_ne!(honest, pairing_proof("master", "chal", "machine-b", real_ca));
        assert_ne!(honest, pairing_proof("other", "chal", "machine-a", real_ca));
        // Deterministic, or the responder could never verify it.
        assert_eq!(honest, pairing_proof("master", "chal", "machine-a", real_ca));
        // And the proof must not be the secret in disguise.
        assert!(!honest.contains("master"));
    }

    #[test]
    fn a_pairing_challenge_is_single_use_and_bounded() {
        let challenges = PairingChallenges::new();
        let a = challenges.issue();
        let b = challenges.issue();
        assert_ne!(a, b);
        assert!(challenges.redeem(&a));
        // Single use: replaying a captured proof must fail on the challenge
        // alone, before any comparison of the proof itself.
        assert!(!challenges.redeem(&a));
        assert!(!challenges.redeem("never issued"));
        assert!(challenges.redeem(&b));

        // The identity endpoint is unauthenticated, so minting must be bounded
        // or a script could grow memory until the process dies.
        let flood = PairingChallenges::new();
        let first = flood.issue();
        for _ in 0..200 {
            flood.issue();
        }
        assert!(!flood.redeem(&first), "the oldest challenge should have been evicted");
        assert!(flood.issued.lock().unwrap().len() <= 64);
    }

    #[test]
    fn the_synthetic_name_is_derived_from_the_machine_id() {
        // Identity, not address: the name must be a pure function of the
        // machine id, because the address is expected to change under DHCP.
        assert_eq!(mesh_dns_name("abc"), "abc.threadknot.mesh");
        assert_ne!(mesh_dns_name("abc"), mesh_dns_name("abd"));
    }
}

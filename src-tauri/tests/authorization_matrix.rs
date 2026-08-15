//! SEC-005: the authorization regression matrix.
//!
//! principal × capability × endpoint × payload × local/peer, driven against the
//! REAL handlers — `server::handle_request` and a bound `build_router`, not a
//! reimplementation of the policy. Every hole in
//! `docs/REMOTE-ACCESS-SECURITY.md` got here by being checked in one place and
//! forgotten in another, so a test that asks a stand-in whether it would have
//! allowed something is worth nothing.
//!
//! One data dir and one server for the whole binary: these are authorization
//! tests, they never mutate each other's subjects.

use std::sync::OnceLock;

use threadknot_lib::ingress::{self, IngressPolicy};
use threadknot_lib::mobile::{Capability, DeviceGrant, Principal};
use threadknot_lib::protocol::{
    Access, Agent, ClientRequest, Mode, Project, Thread, ThreadSettings,
};
use threadknot_lib::server::{self, ServerState};

// ---------------------------------------------------------------- harness ---

struct Harness {
    state: ServerState,
    /// `http://127.0.0.1:<port>` of a really-bound router.
    base: String,
    master_token: String,
    project: Project,
    /// A thread with no signed-in browser profile.
    plain_thread: Thread,
    /// A thread the owner bound to a signed-in browser profile.
    signed_thread: Thread,
    /// A signed-in browser profile, so profile enumeration has something to leak.
    profile_id: String,
    /// `http://127.0.0.1:<port>` of the same app behind `IngressPolicy::Remote`
    /// — what the connector, and therefore a relay, actually talks to.
    remote_base: String,
    /// The mesh listener, bound without TLS for the policy tests.
    mesh_base: String,
    /// The credential the registered peer presents to us.
    peer_credential: String,
}

fn settings(browser_profile_id: Option<&str>, claude_chrome: bool) -> ThreadSettings {
    ThreadSettings {
        model: "sonnet".into(),
        effort: None,
        wide_context: false,
        claude_chrome,
        access: Access::Read,
        mode: Mode::Build,
        browser_profile_id: browser_profile_id.map(String::from),
        hermes_agent_id: None,
    }
}

fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("threadknot-authz-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Must be set before anything resolves the data dir. Also pins the port
        // away from a running desktop app's 42800.
        std::env::set_var("THREADKNOT_DATA_DIR", &dir);
        std::env::set_var("THREADKNOT_PORT", "0");

        let (state, _) = threadknot_lib::build_server_state().expect("build state");
        let master_token = state.config.token.clone();

        let work = dir.join("project");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("README.md"), "hello").unwrap();
        let project = state
            .hub
            .store
            .create_project(work.to_string_lossy().into_owned(), Some("Test".into()))
            .unwrap();
        let plain_thread = state
            .hub
            .store
            .create_thread(project.id.clone(), Agent::Claude, settings(None, false))
            .unwrap();
        let signed_thread = state
            .hub
            .store
            .create_thread(
                project.id.clone(),
                Agent::Claude,
                settings(Some("profile-abc"), false),
            )
            .unwrap();

        let profile_id = state
            .browser_profiles
            .create("Signed in", &["https://example.com".into()])
            .unwrap()
            .id;

        // A really-bound listener: WebSocket upgrades and byte endpoints are
        // where several of these checks actually live.
        let router = server::build_router(state.clone());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let _ = axum::serve(listener, router).await;
            });
        });

        // The strict ingress, provisioned and switched on, on its own port.
        state
            .remote
            .set(None, Some(Some("https://test.remote.threadknot.app".into())))
            .unwrap();
        state.remote.set(Some(true), None).unwrap();
        let mut remote_state = state.clone();
        remote_state.policy = IngressPolicy::Remote;
        let remote_router = server::build_router(remote_state);
        let remote_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        remote_listener.set_nonblocking(true).unwrap();
        let remote_base = format!("http://{}", remote_listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(remote_listener).unwrap();
                let _ = axum::serve(listener, remote_router).await;
            });
        });

        // A registered peer, plus the mesh listener it would dial. The peer's
        // own certificate is irrelevant here — these tests drive the *server*
        // side of a peer link, so what matters is the credential we accept from
        // it and the authority it is granted.
        let peer_credential = threadknot_lib::mesh::mint_credential();
        state
            .peernet
            .registry
            .upsert(threadknot_lib::peers::Peer {
                machine_id: "peer-machine".into(),
                name: "Peer".into(),
                outbound_credential: "we-present-this".into(),
                inbound_credential_hash: threadknot_lib::mesh::hash_credential(&peer_credential),
                mesh_ca: state.mesh.ca_pem.clone(),
                mesh_port: 42802,
                port: 42800,
                addresses: vec!["127.0.0.1".into()],
                added_at: threadknot_lib::protocol::now_iso(),
                ..threadknot_lib::peers::Peer::blank()
            })
            .unwrap();

        // The mesh router, bound plain (no TLS) so tests can drive it directly.
        // TLS is the transport; the *policy* being tested is which principals the
        // door accepts, and that is independent of the encryption above it.
        let mut mesh_state = state.clone();
        mesh_state.policy = IngressPolicy::Mesh;
        let mesh_router = server::build_router(mesh_state);
        let mesh_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        mesh_listener.set_nonblocking(true).unwrap();
        let mesh_base = format!("http://{}", mesh_listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(mesh_listener).unwrap();
                let _ = axum::serve(listener, mesh_router).await;
            });
        });

        Harness {
            state,
            base,
            remote_base,
            mesh_base,
            peer_credential,
            master_token,
            project,
            plain_thread,
            signed_thread,
            profile_id,
        }
    })
}

/// A thread created for one test, so a test that legitimately MUTATES settings
/// can never change the subject another test is asserting against.
fn new_thread(signed: bool) -> Thread {
    let h = harness();
    h.state
        .hub
        .store
        .create_thread(
            h.project.id.clone(),
            Agent::Claude,
            settings(signed.then_some("profile-abc"), false),
        )
        .unwrap()
}

/// A WebSocket handshake, not a plain GET: `/ws`, `/term` and `/browser` run
/// their upgrade extractor first, so a bare GET is rejected as a bad request
/// long before it reaches the authorization we are testing.
async fn ws_status(url: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(url)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .expect("request")
        .status()
}

/// A device principal holding exactly `capabilities` — no persistence needed
/// for the RPC-level checks, which read the resolved principal.
fn device(capabilities: &[Capability]) -> Principal {
    Principal::Device(DeviceGrant {
        id: "dev-test".into(),
        capabilities: capabilities.to_vec(),
    })
}

fn device_with_all_but(missing: Capability) -> Principal {
    device(
        &Capability::ALL
            .into_iter()
            .filter(|c| *c != missing)
            .collect::<Vec<_>>(),
    )
}

/// Run one RPC and return `Err`'s rendered message.
async fn call(principal: &Principal, kind: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let req = ClientRequest {
        id: 1,
        kind: kind.into(),
        payload,
        mesh: None,
    };
    server::handle_request(&harness().state, principal, req)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Assert the request was refused *by the capability gate*, not by something
/// downstream that happened to also fail.
async fn assert_denied(principal: &Principal, kind: &str, payload: serde_json::Value, because: Capability) {
    match call(principal, kind, payload).await {
        Ok(v) => panic!("{kind} was allowed without {:?}: {v}", because),
        Err(e) => assert!(
            e.contains(because.denial()),
            "{kind} failed, but not on the {:?} gate: {e}",
            because
        ),
    }
}

/// Assert the request got PAST the capability gate. It may still fail on
/// missing arguments or absent state — that is downstream of authorization and
/// exactly what we want to distinguish.
async fn assert_passes_gate(principal: &Principal, kind: &str, payload: serde_json::Value, gate: Capability) {
    if let Err(e) = call(principal, kind, payload).await {
        assert!(
            !e.contains(gate.denial()),
            "{kind} was refused on the {:?} gate despite holding it: {e}",
            gate
        );
    }
}

// --------------------------------------------------- SEC-001: hello leaks ---

/// Every string anywhere in `value`.
fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn sec001_device_hello_never_carries_the_master_token() {
    let h = harness();
    let hello = call(&device(&Capability::ALL), "hello", serde_json::json!({}))
        .await
        .expect("hello is unprivileged");

    let mut found = Vec::new();
    strings(&hello, &mut found);
    for s in &found {
        assert!(
            !s.contains(&h.master_token),
            "a device response leaked the master token: {s}"
        );
    }
    // Specifically the field that leaked it: an address, not a credential.
    let lan_url = hello["lanUrl"].as_str().unwrap();
    assert!(!lan_url.contains("token="), "device lanUrl must be origin-only");
    assert_eq!(lan_url, h.state.lan_origin());

    // Master still gets the pasteable URL — the fix must not break the desktop
    // "copy LAN link" flow it exists for.
    let master = call(&Principal::Master, "hello", serde_json::json!({}))
        .await
        .unwrap();
    assert!(master["lanUrl"].as_str().unwrap().contains(&h.master_token));
    assert_eq!(master["principal"], "master");
    assert_eq!(hello["principal"], "device");
}

/// The same negative assertion over the responses a device actually receives on
/// its own management endpoints — a peer's master token is just as fatal.
#[tokio::test]
async fn sec001_no_device_visible_response_serializes_a_peer_or_master_secret() {
    let h = harness();
    let dev = device(&Capability::ALL);
    for (kind, payload) in [
        ("hello", serde_json::json!({})),
        ("device.info", serde_json::json!({})),
        ("project.list", serde_json::json!({})),
        ("peer.list", serde_json::json!({})),
        ("workspace.list", serde_json::json!({})),
    ] {
        let Ok(value) = call(&dev, kind, payload).await else {
            continue; // Kind may not exist on this build; the leak test is the point.
        };
        let mut found = Vec::new();
        strings(&value, &mut found);
        for s in found {
            assert!(!s.contains(&h.master_token), "{kind} leaked the master token");
            for peer in h.state.peernet.registry.list() {
                assert!(
                    peer.outbound_credential.is_empty() || !s.contains(&peer.outbound_credential),
                    "{kind} leaked a peer credential"
                );
                assert!(
                    peer.mesh_ca.is_empty() || !s.contains(&peer.mesh_ca),
                    "{kind} leaked a peer certificate"
                );
            }
        }
    }
}

// ------------------------------------- SEC-004: the capability gate itself ---

#[tokio::test]
async fn master_holds_every_capability_implicitly() {
    for capability in Capability::ALL {
        assert!(Principal::Master.can(capability));
        assert!(Principal::Master.require(capability).is_ok());
    }
    let none = device(&[]);
    for capability in Capability::ALL {
        assert!(!none.can(capability), "an empty grant set grants nothing");
    }
}

/// One row per (kind, capability): the device that lacks it is refused, the
/// device that holds it is not refused *on that gate*.
#[tokio::test]
async fn sec004_request_kinds_require_their_capability() {
    let h = harness();
    let rows: &[(&str, Capability, serde_json::Value)] = &[
        ("term.list", Capability::Terminal, serde_json::json!({ "projectId": h.project.id })),
        ("term.create", Capability::Terminal, serde_json::json!({ "projectId": h.project.id })),
        ("term.delete", Capability::Terminal, serde_json::json!({ "projectId": h.project.id, "termId": "t" })),
        ("fs.listDir", Capability::Files, serde_json::json!({ "projectId": h.project.id, "path": "" })),
        ("fs.mkdir", Capability::Files, serde_json::json!({ "path": format!("{}/tk-auth-matrix-mkdir", std::env::temp_dir().display()) })),
        ("fs.read", Capability::Files, serde_json::json!({ "projectId": h.project.id, "path": "x" })),
        ("fs.tree", Capability::Files, serde_json::json!({ "projectId": h.project.id })),
        ("artifacts.list", Capability::Files, serde_json::json!({ "threadId": h.plain_thread.id })),
        ("git.status", Capability::Git, serde_json::json!({ "repoId": h.project.id })),
        ("git.repos", Capability::Git, serde_json::json!({ "projectId": h.project.id })),
        ("thread.list", Capability::Threads, serde_json::json!({ "projectId": h.project.id })),
        ("thread.get", Capability::Threads, serde_json::json!({ "threadId": h.plain_thread.id })),
        // A thread id that does not exist: the grant check is independent of it,
        // and a real one would start an actual agent turn.
        ("turn.start", Capability::Threads, serde_json::json!({ "threadId": "no-such-thread", "text": "hi" })),
        ("approval.respond", Capability::Threads, serde_json::json!({ "threadId": "no-such-thread", "requestId": "r", "decision": "deny" })),
        ("schedule.list", Capability::Threads, serde_json::json!({})),
    ];

    for (kind, capability, payload) in rows {
        assert_denied(&device_with_all_but(*capability), kind, payload.clone(), *capability).await;
        assert_passes_gate(&device(&Capability::ALL), kind, payload.clone(), *capability).await;
        assert_passes_gate(&Principal::Master, kind, payload.clone(), *capability).await;
    }
}

/// Unprivileged kinds must stay reachable — a gate that refuses everything is
/// not a fix, it is an outage.
#[tokio::test]
async fn unprivileged_kinds_stay_open_to_a_bare_device() {
    let none = device(&[]);
    for kind in ["hello", "device.info", "project.list", "app.changelog"] {
        call(&none, kind, serde_json::json!({}))
            .await
            .unwrap_or_else(|e| panic!("{kind} must not need a grant: {e}"));
    }
}

/// Invariant 12: unauthorized callers get a non-enumerating answer where that
/// is practical. The profile id list is exactly what a caller needs in order to
/// smuggle a signed-in profile into a ThreadSettings blob, so a device without
/// the grant is told it has none rather than being handed the ids.
#[tokio::test]
async fn signed_in_profiles_are_not_enumerable_without_the_grant() {
    let h = harness();

    let hidden = call(&device_with_all_but(Capability::SignedBrowser), "browser.profile.list", serde_json::json!({}))
        .await
        .expect("the picker must load, not error");
    assert_eq!(
        hidden["profiles"].as_array().map(Vec::len),
        Some(0),
        "profile ids must not reach a device that may not use them"
    );

    for allowed in [device(&Capability::ALL), Principal::Master] {
        let listed = call(&allowed, "browser.profile.list", serde_json::json!({}))
            .await
            .unwrap();
        let ids: Vec<&str> = listed["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert!(ids.contains(&h.profile_id.as_str()), "granted callers still see them");
    }
}

// --------------------------------------------- SEC-002 / invariant 4: mesh ---

/// Routing to a peer needs `mesh` — checked BEFORE the forward, because the
/// forward swaps in the peer's master token and arrives there as its owner.
#[tokio::test]
async fn sec002_mesh_routing_is_refused_before_the_credential_swap() {
    let h = harness();
    let peer = serde_json::json!({ "machineId": "some-other-machine", "projectId": h.project.id });

    assert_denied(&device_with_all_but(Capability::Mesh), "thread.list", peer.clone(), Capability::Mesh).await;
    assert_denied(&device_with_all_but(Capability::Mesh), "term.create", peer.clone(), Capability::Mesh).await;
    assert_denied(&device_with_all_but(Capability::Mesh), "git.status", peer.clone(), Capability::Mesh).await;

    // With `mesh`, the request gets past the gate and fails on the unknown
    // peer instead — proof the refusal above was the gate, not the peer.
    let err = call(&device(&Capability::ALL), "thread.list", peer.clone())
        .await
        .expect_err("no such peer");
    assert!(!err.contains(Capability::Mesh.denial()), "{err}");

    // A device that may reach peers but may not open terminals is still
    // refused a peer terminal: `mesh` never widens another grant.
    assert_denied(
        &device(&[Capability::Threads, Capability::Mesh]),
        "term.create",
        peer,
        Capability::Terminal,
    )
    .await;
}

// ----------------------------- SEC-003: privileged payload fields ---

/// `browserProfileId` / `claudeChrome` inside a ThreadSettings blob carry the
/// owner's logged-in browser identity, and must be checked exactly like the
/// master-only `browser.profile.*` kinds are.
#[tokio::test]
async fn sec003_settings_fields_need_the_signed_browser_grant() {
    let h = harness();
    let no_signed = device_with_all_but(Capability::SignedBrowser);
    let signed = device(&Capability::ALL);

    let claims: [serde_json::Value; 2] = [
        serde_json::json!({ "model": "sonnet", "access": "read", "mode": "build", "browserProfileId": "profile-abc" }),
        serde_json::json!({ "model": "sonnet", "access": "read", "mode": "build", "claudeChrome": true }),
    ];

    for claim in &claims {
        for (kind, extra) in [
            ("thread.create", serde_json::json!({ "projectId": h.project.id })),
            ("thread.setAgent", serde_json::json!({ "threadId": new_thread(false).id, "agent": "claude" })),
            ("thread.setSettings", serde_json::json!({ "threadId": new_thread(false).id })),
            ("schedule.create", serde_json::json!({ "projectId": h.project.id, "agent": "claude", "prompt": "go", "cadence": { "kind": "daily", "hour": 9, "minute": 0 } })),
            ("schedule.update", serde_json::json!({ "scheduleId": "s-1", "agent": "claude" })),
        ] {
            let mut payload = extra.clone();
            payload["settings"] = claim.clone();

            // Local.
            assert_denied(&no_signed, kind, payload.clone(), Capability::SignedBrowser).await;
            // Routed at a peer — refused HERE, before the request could arrive
            // on that machine as its owner.
            let mut routed = payload.clone();
            routed["machineId"] = serde_json::json!("some-other-machine");
            assert_denied(&no_signed, kind, routed, Capability::SignedBrowser).await;

            // Granted, the same payload gets past this gate.
            assert_passes_gate(&signed, kind, payload, Capability::SignedBrowser).await;
        }
    }
}

/// A schedule that DISPATCHES runs code on this machine and on others, on a
/// timer, unattended. `Capability::Threads` — what the table maps every
/// `schedule.*` kind to — is the right grant for a schedule that starts a turn
/// and nowhere near enough for one that delegates, so a dispatching schedule
/// needs `Terminal` as well. Without this a `Threads`-only phone could mint
/// terminal authority for itself by saving a schedule.
#[tokio::test]
async fn a_dispatching_schedule_also_needs_the_terminal_grant() {
    let h = harness();
    let no_terminal = device_with_all_but(Capability::Terminal);
    let all = device(&Capability::ALL);

    let settings =
        serde_json::json!({ "model": "sonnet", "access": "read", "mode": "build" });
    let dispatch = serde_json::json!({ "machines": [], "syncRef": false });

    let create = serde_json::json!({
        "projectId": h.project.id,
        "agent": "claude",
        "settings": settings,
        "prompt": "build it",
        "cadence": { "type": "daily", "time": "09:00" },
        "dispatch": dispatch,
    });
    let update = serde_json::json!({
        "scheduleId": "no-such-schedule",
        "dispatch": dispatch,
    });

    for (kind, payload) in [("schedule.create", &create), ("schedule.update", &update)] {
        assert_denied(&no_terminal, kind, payload.clone(), Capability::Terminal).await;
        assert_passes_gate(&all, kind, payload.clone(), Capability::Terminal).await;
        assert_passes_gate(&Principal::Master, kind, payload.clone(), Capability::Terminal).await;
    }

    // The same kinds WITHOUT a dispatch block stay on the Threads grant alone —
    // the second gate must not become a tax on ordinary scheduled runs.
    let mut plain = create.clone();
    plain.as_object_mut().unwrap().remove("dispatch");
    assert_passes_gate(&no_terminal, "schedule.create", plain, Capability::Terminal).await;
}

/// Firing a saved dispatching schedule is the same authority as saving one:
/// otherwise the grant is checked at write time and bypassed at "Run now".
#[tokio::test]
async fn running_a_dispatching_schedule_needs_the_terminal_grant() {
    let h = harness();
    let created = call(
        &Principal::Master,
        "schedule.create",
        serde_json::json!({
            "projectId": h.project.id,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
            "prompt": "build it",
            "cadence": { "type": "daily", "time": "09:00" },
            "dispatch": { "machines": [], "syncRef": false },
        }),
    )
    .await
    .expect("master may create a dispatching schedule");
    let id = created["id"].as_str().expect("schedule id");

    assert_denied(
        &device_with_all_but(Capability::Terminal),
        "schedule.run",
        serde_json::json!({ "scheduleId": id }),
        Capability::Terminal,
    )
    .await;
}

/// Settings with no signed-browser claim must still work for an ordinary
/// device: the check is on the authority, not on the shape of the payload.
#[tokio::test]
async fn sec003_ordinary_settings_are_unaffected() {
    let h = harness();
    let dev = device_with_all_but(Capability::SignedBrowser);
    let thread = call(
        &dev,
        "thread.create",
        serde_json::json!({
            "projectId": h.project.id,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
        }),
    )
    .await
    .expect("a plain chat needs no browser authority");
    assert!(thread["id"].is_string());
}

/// Ingestion is only half of it: a thread the OWNER already bound to a
/// signed-in profile hands that identity to whoever starts its next turn.
#[tokio::test]
async fn sec003_running_a_signed_thread_needs_the_grant_too() {
    let h = harness();
    let no_signed = device_with_all_but(Capability::SignedBrowser);

    for kind in ["turn.start", "turn.steer", "thread.review"] {
        assert_denied(
            &no_signed,
            kind,
            serde_json::json!({ "threadId": h.signed_thread.id, "text": "go", "agent": "claude" }),
            Capability::SignedBrowser,
        )
        .await;
        // A chat with no such binding is not blocked by this gate. Deliberately
        // an id with no thread behind it: passing the gate on a REAL thread
        // would start a real agent turn.
        assert_passes_gate(
            &no_signed,
            kind,
            serde_json::json!({ "threadId": "no-such-thread", "text": "go", "agent": "claude" }),
            Capability::SignedBrowser,
        )
        .await;
    }
}

// --------------------------------------------- HTTP + WebSocket boundaries ---

async fn get(url: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(url)
        .send()
        .await
        .expect("request")
        .status()
}

/// Byte endpoints are a separate boundary from the RPC dispatcher and were
/// authenticated but never authorized.
#[tokio::test]
async fn byte_endpoints_enforce_their_grant() {
    let h = harness();
    let files_denied = paired(&h.state, "no-files", &[Capability::Threads]).await;
    let files_ok = paired(&h.state, "files", &[Capability::Files, Capability::Threads]).await;

    let file_url = |token: &str| {
        format!(
            "{}/file?project={}&path=README.md&token={token}",
            h.base, h.project.id
        )
    };
    assert_eq!(get(&file_url("nonsense")).await, 401, "unknown token");
    assert_eq!(get(&file_url(&files_denied)).await, 403, "no files grant");
    assert_eq!(get(&file_url(&files_ok)).await, 200, "granted device reads the file");

    // Peer byte proxying is mesh-gated on THIS side, before the peer's master
    // token is attached to the outbound request.
    let peer_url = format!(
        "{}/file?project={}&path=README.md&machineId=elsewhere&token={}",
        h.base, h.project.id, files_ok
    );
    assert_eq!(get(&peer_url).await, 403, "no mesh grant");
}

/// Pair a real device so its credential authenticates over HTTP.
async fn paired(state: &ServerState, name: &str, capabilities: &[Capability]) -> String {
    let (_, credential) = state
        .mobile
        .pair(name.into(), "test".into(), capabilities.to_vec())
        .unwrap();
    credential
}

#[tokio::test]
async fn sec002_term_socket_checks_the_principal_not_just_the_token() {
    let h = harness();
    let no_term = paired(&h.state, "no-term", &[Capability::Threads, Capability::Mesh]).await;
    let term_no_mesh = paired(&h.state, "term-local", &[Capability::Terminal]).await;

    let url = |token: &str, extra: &str| {
        format!("{}/term?project={}&term=t1&token={token}{extra}", h.base, h.project.id)
    };
    // Authenticated, but not allowed a shell here.
    assert_eq!(ws_status(&url(&no_term, "")).await, 403);
    // Allowed a shell here, but not on another machine — this is the splice
    // that used to accept any authenticated credential and then reconnect as
    // the peer's master.
    assert_eq!(ws_status(&url(&term_no_mesh, "&machineId=elsewhere")).await, 403);
    // And an unknown credential still stops at authentication.
    assert_eq!(ws_status(&url("nonsense", "")).await, 401);
}

#[tokio::test]
async fn browser_socket_gates_signed_in_sessions() {
    let h = harness();
    let plain = paired(&h.state, "browser-plain", &[Capability::Browser]).await;
    let none = paired(&h.state, "browser-none", &[Capability::Threads]).await;

    let url = |token: &str, session: &str| {
        format!("{}/browser?session={session}&token={token}", h.base, )
    };
    assert_eq!(ws_status(&url(&none, &h.plain_thread.id)).await, 403, "no browser grant");
    // A chat bound to a signed-in profile needs the separate grant, which no
    // default pairing carries.
    assert_eq!(
        ws_status(&url(&plain, &h.signed_thread.id)).await,
        403,
        "signed-in session needs signedBrowser"
    );
    // Another machine's browser stays master-only.
    assert_eq!(
        ws_status(&format!(
            "{}/browser?session={}&machineId=elsewhere&token={}",
            h.base, h.plain_thread.id, plain
        ))
        .await,
        403
    );
}

/// SEC-009 (partial): a page on another site must not be able to open one of
/// these sockets from a browser that can reach the LAN.
#[tokio::test]
async fn websocket_upgrades_validate_the_origin() {
    let h = harness();
    let client = reqwest::Client::new();
    let ws = format!("{}/ws?token={}", h.base, h.master_token);

    let hostile = client
        .get(&ws)
        .header("Origin", "http://evil.example")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(hostile.status(), 403, "cross-origin upgrade must be refused");

    // The page Threadknot actually serves is same-origin, and native clients
    // send no Origin at all.
    for origin in [Some(h.base.as_str()), None, Some("tauri://localhost")] {
        let mut req = client
            .get(&ws)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(origin) = origin {
            req = req.header("Origin", origin);
        }
        assert_eq!(
            req.send().await.unwrap().status(),
            101,
            "legitimate origin {origin:?} must still connect"
        );
    }
}

// ------------------------------------- SEC-008: revocation closes sockets ---

/// Revoking a device used to stop only its NEXT connection. The socket it was
/// already holding stayed open and fully usable.
#[tokio::test]
async fn sec008_revoking_a_device_closes_the_socket_it_already_holds() {
    use futures_util::StreamExt as _;
    let h = harness();
    let (device_record, credential) = h
        .state
        .mobile
        .pair("revoke-me".into(), "test".into(), vec![Capability::Threads])
        .unwrap();

    let url = format!(
        "ws://{}/ws?token={credential}",
        h.base.trim_start_matches("http://")
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");

    // Wait for the server to register the session before pulling the rug.
    for _ in 0..100 {
        if h.state.sessions.device_count(&device_record.id) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(h.state.sessions.device_count(&device_record.id), 1);

    h.state.mobile.revoke(&device_record.id).unwrap();
    h.state.close_device_sessions(&device_record.id);

    // The socket must end on its own, without the client saying anything.
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(msg) = socket.next().await {
            if msg.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "a revoked device's live socket must be closed");
}

/// Narrowing grants is a partial revocation and gets the same treatment.
#[tokio::test]
async fn sec008_narrowing_grants_also_drops_live_sockets() {
    use futures_util::StreamExt as _;
    let h = harness();
    let (record, credential) = h
        .state
        .mobile
        .pair("narrow-me".into(), "test".into(), threadknot_lib::mobile::default_capabilities())
        .unwrap();
    let url = format!(
        "ws://{}/ws?token={credential}",
        h.base.trim_start_matches("http://")
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
    for _ in 0..100 {
        if h.state.sessions.device_count(&record.id) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    server::handle_request(
        &h.state,
        &Principal::Master,
        ClientRequest {
            id: 1,
            kind: "mobile.device.setCapabilities".into(),
            payload: serde_json::json!({ "deviceId": record.id, "capabilities": ["threads"] }),
            mesh: None,
        },
    )
    .await
    .expect("master may edit grants");

    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(msg) = socket.next().await {
            if msg.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "a reduced device's live socket must be closed");

    // And the reduction is what a reconnect would now resolve to.
    let grant = h.state.mobile.authenticate(&credential).unwrap();
    assert_eq!(grant.capabilities, vec![Capability::Threads]);
}

/// A device cannot widen itself: capability editing is master-only, and the
/// pairing endpoints take grants only from a master-authenticated caller.
#[tokio::test]
async fn sec004_a_device_cannot_grant_itself_anything() {
    let h = harness();
    let (record, _) = h
        .state
        .mobile
        .pair("self-promoter".into(), "test".into(), vec![Capability::Threads])
        .unwrap();
    let dev = device(&Capability::ALL);

    for kind in [
        "mobile.device.setCapabilities",
        "mobile.device.list",
        "mobile.device.revoke",
        "mobile.pair.begin",
    ] {
        let err = call(
            &dev,
            kind,
            serde_json::json!({ "deviceId": record.id, "capabilities": ["signedBrowser", "mesh"] }),
        )
        .await
        .expect_err("device management is master-only");
        assert!(err.contains("master token") || err.contains("desktop app"), "{kind}: {err}");
    }
    // Unchanged.
    assert_eq!(
        h.state.mobile.device(&record.id).unwrap().capabilities,
        vec![Capability::Threads]
    );
}

// ------------------------------------------------ SEC-013: file permissions ---

#[cfg(unix)]
#[tokio::test]
async fn sec013_secret_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let h = harness();
    let dir = h.state.hub.store.dir();

    let mode = |path: std::path::PathBuf| {
        std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(dir.join("server.json")), 0o600, "the master token at rest");
    assert_eq!(mode(dir.to_path_buf()), 0o700, "the directory holding it");

    // mobile.json carries credential hashes and the grant set.
    h.state
        .mobile
        .pair("perm-check".into(), "test".into(), vec![Capability::Threads])
        .unwrap();
    assert_eq!(mode(dir.join("mobile.json")), 0o600);
}

// ============================================================ strict ingress ==
//
// SEC-006 / SEC-007 / SEC-009 / SEC-010: what the loopback listener the
// connector dials will and will not accept. These run against the same app
// behind `IngressPolicy::Remote`, because the whole design claim is that the
// policy is a property of the socket rather than of the handler.

/// Outstanding pairing codes are capped (a stuck client must not be able to
/// grow the set of live secrets), and this binary's tests share one server. Hold
/// this across a mint -> redeem window so parallel tests cannot evict each
/// other's code and fail as if the endpoint were broken.
fn pairing_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

/// Pair a device and return its bearer credential.
async fn remote_device(name: &str, capabilities: &[Capability]) -> String {
    let (_, credential) = harness()
        .state
        .mobile
        .pair(name.into(), "test".into(), capabilities.to_vec())
        .unwrap();
    credential
}

#[tokio::test]
async fn sec006_remote_refuses_credentials_in_the_url() {
    let h = harness();
    let credential = remote_device("url-cred", &[Capability::Files]).await;
    let client = reqwest::Client::new();

    // The exact URL that works on the LAN is a 400 here — the point is that
    // credentials stop existing in URLs, not that they are checked harder.
    let url = format!(
        "{}/file?project={}&path=README.md&token={credential}",
        h.remote_base, h.project.id
    );
    assert_eq!(client.get(&url).send().await.unwrap().status(), 400);

    // Same request, credential in the header instead: fine.
    let ok = client
        .get(format!(
            "{}/file?project={}&path=README.md",
            h.remote_base, h.project.id
        ))
        .header("Authorization", format!("Bearer {credential}"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
}

#[tokio::test]
async fn sec007_remote_refuses_the_master_credential() {
    let h = harness();
    let client = reqwest::Client::new();

    let by_header = client
        .get(format!("{}/api/server-info", h.remote_base))
        .header("Authorization", format!("Bearer {}", h.master_token))
        .send()
        .await
        .unwrap();
    assert_eq!(by_header.status(), 403, "a valid master token is still refused");

    let by_query = client
        .get(format!("{}/api/server-info?token={}", h.remote_base, h.master_token))
        .send()
        .await
        .unwrap();
    assert_eq!(by_query.status(), 400, "and never reaches the check at all");

    // On the LAN listener the same token is exactly how the desktop connects.
    let lan = client
        .get(format!("{}/api/server-info?token={}", h.base, h.master_token))
        .send()
        .await
        .unwrap();
    assert_eq!(lan.status(), 200, "the LAN product must be untouched");
}

/// The machine-administration surface is not merely guarded on the strict
/// router — it is not mounted on it.
#[tokio::test]
async fn sec007_local_only_routes_are_absent_from_the_remote_router() {
    let h = harness();
    let client = reqwest::Client::new();
    for path in ["/mcp", "/api/peer/pair"] {
        let remote = client
            .post(format!("{}{path}", h.remote_base))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        // Unmounted paths fall through to the static file service, which
        // serves GET/HEAD only — so "not a POST endpoint here" is the shape of
        // a route that does not exist.
        assert!(
            remote.status() == 404 || remote.status() == 405,
            "{path} must not exist remotely, got {}",
            remote.status()
        );
    }
    // The pairing *bootstrap* is still mounted on the LAN listener, and is
    // deliberately unauthenticated — everything it returns is public (a machine
    // id, a display name, a certificate authority, a port). It is what a machine
    // with nothing to verify against yet needs in order to get something to
    // verify against.
    let identity = client
        .get(format!("{}/api/peer/identity", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(identity.status(), 200, "the pairing bootstrap exists on the LAN");
    let body: serde_json::Value = identity.json().await.unwrap();
    assert!(
        body["meshCa"].as_str().is_some_and(|ca| ca.contains("BEGIN CERTIFICATE")),
        "identity must carry this machine's certificate authority"
    );
    assert!(body["challenge"].as_str().is_some_and(|c| !c.is_empty()));
    // And it must not carry a secret. This endpoint is open to anyone who can
    // reach the LAN port, so anything sensitive here is simply published.
    let mut found = Vec::new();
    strings(&body, &mut found);
    for value in found {
        assert!(
            !value.contains(&h.master_token),
            "the pairing bootstrap leaked the master token"
        );
    }

    // But the *exchange* is not on the LAN listener at all. It carries the proof
    // and both freshly minted credentials, so it only exists inside TLS on the
    // mesh listener.
    let bootstrap_on_lan = client
        .post(format!("{}/api/peer/pair", h.base))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert!(
        bootstrap_on_lan.status() == 404 || bootstrap_on_lan.status() == 405,
        "the pairing exchange must not be reachable over plain HTTP, got {}",
        bootstrap_on_lan.status()
    );
}

#[tokio::test]
async fn sec006_a_pairing_code_becomes_a_cookie_not_a_credential() {
    let h = harness();
    let _mint = pairing_lock().lock().await;
    let code = h.state.mobile.begin_pairing(vec![Capability::Threads]);
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/session", h.remote_base))
        .json(&serde_json::json!({ "pairingCode": code, "deviceName": "Remote browser" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .expect("a session cookie")
        .to_string();
    for attribute in ["HttpOnly", "Secure", "SameSite=Strict"] {
        assert!(set_cookie.contains(attribute), "cookie missing {attribute}");
    }
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["csrf"].is_string(), "the page needs its double-submit half");
    assert_eq!(body["capabilities"], serde_json::json!(["threads"]));
    // The browser is never handed a bearer credential it could leak.
    let rendered = body.to_string();
    assert!(!rendered.contains("amd_"), "no device credential in the response");

    // And the cookie now authenticates on its own.
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    let hello = client
        .get(format!("{}/api/server-info", h.remote_base))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(hello.status(), 200);

    // A code is single use, so the same one cannot mint a second session.
    let replay = client
        .post(format!("{}/api/session", h.remote_base))
        .json(&serde_json::json!({ "pairingCode": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 401);
}

#[tokio::test]
async fn sec006_a_master_token_never_becomes_a_browser_session() {
    let h = harness();
    let resp = reqwest::Client::new()
        .post(format!("{}/api/session", h.remote_base))
        .header("Authorization", format!("Bearer {}", h.master_token))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert!(resp.headers().get("set-cookie").is_none());
}

/// Bootstrap is a remote-ingress concept. On the LAN the cookie would be
/// `Secure` over plain http and silently dropped, so the LAN keeps its token.
#[tokio::test]
async fn session_bootstrap_is_not_offered_on_the_lan_listener() {
    let h = harness();
    let resp = reqwest::Client::new()
        .post(format!("{}/api/session", h.base))
        .json(&serde_json::json!({ "pairingCode": "XXXXXXXXXX" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn sec009_cookie_state_changes_need_the_csrf_token() {
    let h = harness();
    let _mint = pairing_lock().lock().await;
    let code = h.state.mobile.begin_pairing(vec![Capability::Threads]);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/session", h.remote_base))
        .json(&serde_json::json!({ "pairingCode": code }))
        .send()
        .await
        .unwrap();
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let body: serde_json::Value = resp.json().await.unwrap();
    let csrf = body["csrf"].as_str().unwrap().to_string();

    // Unpairing is a state change. Cookie alone is not enough — that is exactly
    // what a page on another origin would be able to produce.
    let without = client
        .post(format!("{}/api/mobile/unpair", h.remote_base))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(without.status(), 403);

    let wrong = client
        .post(format!("{}/api/mobile/unpair", h.remote_base))
        .header("Cookie", &cookie)
        .header("X-Threadknot-Csrf", "not-it")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 403);

    let with = client
        .post(format!("{}/api/mobile/unpair", h.remote_base))
        .header("Cookie", &cookie)
        .header("X-Threadknot-Csrf", &csrf)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(with.status(), 200);

    // Unpairing dropped the device, so the cookie it minted is dead too.
    let after = client
        .get(format!("{}/api/server-info", h.remote_base))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), 401, "a session cannot outlive its device");
}

#[tokio::test]
async fn sec009_the_public_origin_ships_its_headers() {
    let h = harness();
    let resp = reqwest::Client::new()
        .get(format!("{}/api/server-info", h.remote_base))
        .send()
        .await
        .unwrap();
    let headers = resp.headers().clone();
    let csp = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .expect("a CSP on the public origin");
    assert!(csp.contains("frame-ancestors 'none'"), "clickjacking: {csp}");
    assert!(csp.contains("object-src 'none'"), "{csp}");
    assert!(headers.contains_key("strict-transport-security"));
    assert_eq!(
        headers.get("referrer-policy").and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );
    // No CORS headers at all: another origin's script must not read this.
    assert!(
        headers.get("access-control-allow-origin").is_none(),
        "the strict ingress must not opt into cross-origin reads"
    );
}

#[tokio::test]
async fn remote_access_off_refuses_everything_on_the_strict_ingress() {
    let h = harness();
    // A clone of the running app whose remote access was never switched on.
    let dir = std::env::temp_dir().join(format!("tk-off-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut off = h.state.clone();
    off.policy = IngressPolicy::Remote;
    off.remote = std::sync::Arc::new(threadknot_lib::remote::RemoteStore::open(&dir, 42800).unwrap());

    let credential = remote_device("would-work", &[Capability::Files]).await;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {credential}").parse().unwrap(),
    );
    let rejection = off
        .authenticate_ingress(&headers, &std::collections::HashMap::new())
        .err()
        .expect("a credential that works is still refused while remote access is off");
    assert_eq!(rejection.status, 503);

    // The same credential on the enabled ingress does authenticate.
    let mut on = h.state.clone();
    on.policy = IngressPolicy::Remote;
    assert!(on
        .authenticate_ingress(&headers, &std::collections::HashMap::new())
        .is_ok());

    // "Off" must mean off for every byte, not only for authenticated ones. The
    // static bundle is served by `fallback_service`, which never calls
    // `authenticate_ingress` — it cannot, because the shell has to load before
    // anyone can sign in — so a check only at authentication time still handed
    // out 5 MB of JavaScript to anyone who resolved the hostname, and charged it
    // to the owner's fair-use allowance. Driven through a really-bound router,
    // since the whole point is that this path bypasses the authentication call.
    let router = server::build_router(off);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let _ = axum::serve(listener, router).await;
        });
    });
    let client = reqwest::Client::new();
    for path in ["/", "/index.html", "/assets/whatever.js", "/api/server-info"] {
        let resp = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            503,
            "{path} must be refused while remote access is off"
        );
        let body = resp.text().await.unwrap_or_default();
        assert!(
            !body.contains("<script") && !body.contains("threadknot"),
            "{path} leaked bundle content while remote access is off"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// SEC-010: the remote QR comes from stored configuration. If it came from the
/// request `Host`, anyone who can reach the server could have a QR minted that
/// points a phone at a host they chose.
#[tokio::test]
async fn sec010_the_remote_qr_uses_the_provisioned_origin() {
    let h = harness();
    let _mint = pairing_lock().lock().await;
    let lan = call(&Principal::Master, "mobile.pair.begin", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(lan["url"], serde_json::json!(h.state.lan_origin()));

    let remote = call(
        &Principal::Master,
        "mobile.pair.begin",
        serde_json::json!({ "target": "remote" }),
    )
    .await
    .unwrap();
    assert_eq!(remote["url"], "https://test.remote.threadknot.app");
    assert!(
        remote["payload"].as_str().unwrap().contains("test.remote.threadknot.app"),
        "the QR itself must carry it"
    );
}

#[tokio::test]
async fn remote_settings_are_master_only() {
    let dev = device(&Capability::ALL);
    for kind in ["remote.get", "remote.set"] {
        let err = call(&dev, kind, serde_json::json!({ "enabled": true }))
            .await
            .expect_err("remote access is machine administration");
        assert!(err.contains("desktop app"), "{kind}: {err}");
    }
}

/// The strict ingress carries the same capability model as everything else —
/// the policy narrows how you authenticate, never what a grant means.
#[tokio::test]
async fn capabilities_still_apply_on_the_strict_ingress() {
    let h = harness();
    let no_files = remote_device("remote-no-files", &[Capability::Threads]).await;
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/file?project={}&path=README.md",
            h.remote_base, h.project.id
        ))
        .header("Authorization", format!("Bearer {no_files}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

/// A session resolves its authority through the device on every request, so a
/// grant taken away applies to a cookie already in a browser's jar.
#[tokio::test]
async fn narrowing_grants_reaches_an_existing_cookie_session() {
    let h = harness();
    let _mint = pairing_lock().lock().await;
    let code = h
        .state
        .mobile
        .begin_pairing(vec![Capability::Threads, Capability::Files]);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/session", h.remote_base))
        .json(&serde_json::json!({ "pairingCode": code }))
        .send()
        .await
        .unwrap();
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let device_id = resp.json::<serde_json::Value>().await.unwrap()["deviceId"]
        .as_str()
        .unwrap()
        .to_string();

    let file_url = format!(
        "{}/file?project={}&path=README.md",
        h.remote_base, h.project.id
    );
    assert_eq!(
        client.get(&file_url).header("Cookie", &cookie).send().await.unwrap().status(),
        200
    );

    h.state
        .mobile
        .set_capabilities(&device_id, vec![Capability::Threads])
        .unwrap();
    assert_eq!(
        client.get(&file_url).header("Cookie", &cookie).send().await.unwrap().status(),
        403,
        "the cookie carries no authority of its own"
    );
}

#[tokio::test]
async fn ingress_policy_does_not_leak_into_the_lan_listener() {
    let h = harness();
    // Everything the strict ingress refuses is still how the LAN works, which
    // is the whole reason there are two sockets.
    let credential = remote_device("lan-still-works", &[Capability::Files]).await;
    let lan = reqwest::Client::new()
        .get(format!(
            "{}/file?project={}&path=README.md&token={credential}",
            h.base, h.project.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(lan.status(), 200, "a token in the URL is still the LAN product");
    assert_eq!(h.state.policy, IngressPolicy::Compat);
    assert!(ingress::IngressPolicy::Remote.is_remote());
}

// ----------------------------- SEC-012: the mesh is a door of its own ---
//
// Before these, a paired machine authenticated with the other machine's MASTER
// token, carried in a plaintext `ws://` URL. Every one of the cases below was
// therefore either impossible to express or silently true in the wrong
// direction. They are the peer half of the matrix.

/// A peer principal narrowed to exactly `capabilities`, as an authenticated
/// mesh request would resolve to.
fn peer(capabilities: Option<&[Capability]>) -> Principal {
    Principal::Peer(threadknot_lib::mobile::PeerPrincipal {
        machine_id: "peer-machine".into(),
        on_behalf_of: capabilities.map(<[Capability]>::to_vec),
    })
}

#[tokio::test]
async fn sec012_a_peer_credential_resolves_to_a_peer_not_to_master() {
    let h = harness();
    // The credential says WHICH machine, and nothing about authority. Resolving
    // it to Master here is precisely the old bug: every routed request then
    // arrived as this machine's owner.
    let resolved = h.state.authenticate(&h.peer_credential).expect("peer authenticates");
    assert_eq!(resolved.peer_machine_id(), Some("peer-machine"));
    assert!(!resolved.is_local_master(), "a peer is never this machine's master");
    assert!(!resolved.is_owner(), "a bare peer credential asserts no authority at all");
    for capability in Capability::ALL {
        assert!(
            !resolved.can(capability),
            "a peer credential alone must grant nothing; the frame is the authority"
        );
    }
    // And the master token still resolves to Master, unchanged.
    assert!(h.state.authenticate(&h.master_token).unwrap().is_local_master());
}

#[tokio::test]
async fn sec012_the_mesh_door_accepts_only_a_peer_credential() {
    let h = harness();
    let client = reqwest::Client::new();
    let get = async |token: Option<&str>, query: &str| {
        let mut request = client.get(format!("{}/api/server-info{query}", h.mesh_base));
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request.send().await.expect("request").status().as_u16()
    };

    assert_eq!(get(Some(&h.peer_credential), "").await, 200, "a peer gets in");
    // The master token is refused even though it is perfectly valid — the same
    // rule the strict ingress applies, for the same reason: one door that
    // accepts two kinds of principal is a door whose policy has to be
    // re-derived at every handler.
    assert_eq!(get(Some(&h.master_token), "").await, 403);
    assert_eq!(get(Some("nonsense"), "").await, 401);
    assert_eq!(get(None, "").await, 401, "anonymous is not a peer");
    // A credential in the URL is refused outright. It is valid, and that is not
    // the point: SEC-012 exists to stop credentials being in URLs at all.
    assert_eq!(get(None, &format!("?token={}", h.peer_credential)).await, 400);
    assert_eq!(
        get(Some(&h.peer_credential), &format!("?token={}", h.peer_credential)).await,
        400,
        "a URL credential is refused even alongside a good header"
    );
}

#[tokio::test]
async fn sec012_a_peer_credential_is_refused_on_the_relay_ingress() {
    let h = harness();
    // A peer credential is scoped to one machine-to-machine link. If the relay
    // could carry one, a compromised relay that observed one would arrive as a
    // paired machine.
    let status = reqwest::Client::new()
        .get(format!("{}/api/server-info", h.remote_base))
        .header("Authorization", format!("Bearer {}", h.peer_credential))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 403);
}

#[tokio::test]
async fn sec012_the_grants_header_narrows_and_cannot_widen() {
    use threadknot_lib::ingress::{mesh_grants, MESH_GRANTS_HEADER};
    let header = |value: &str| {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(MESH_GRANTS_HEADER, value.parse().unwrap());
        headers
    };
    // Absent means "that machine's owner" — the plumbing case.
    assert_eq!(mesh_grants(&axum::http::HeaderMap::new()), None);
    // Present and empty means a caller with no grants, which is NOT the same as
    // absent. Collapsing the two would turn every narrowed request into an
    // owner-authority one.
    assert_eq!(mesh_grants(&header("")), Some(vec![]));
    assert_eq!(
        mesh_grants(&header("threads,files")),
        Some(vec![Capability::Threads, Capability::Files])
    );
    // An unknown name is DROPPED, never honoured: a newer peer must not be able
    // to widen an older machine by naming a capability it does not understand,
    // and must not be able to break it by mentioning one either.
    assert_eq!(
        mesh_grants(&header("threads,rootShell,files")),
        Some(vec![Capability::Threads, Capability::Files])
    );
    assert_eq!(mesh_grants(&header("rootShell")), Some(vec![]));
}

#[tokio::test]
async fn sec012_a_peer_cannot_exceed_the_grants_it_carries() {
    // A phone denied `terminal` locally used to be able to ask a peer to open
    // one, and the peer honoured it as its own owner (SEC-002). Now the caller's
    // grants travel with the request and this side enforces them.
    let narrow = peer(Some(&[Capability::Threads]));
    assert!(narrow.can(Capability::Threads));
    assert!(!narrow.can(Capability::Terminal));
    assert!(!narrow.can(Capability::SignedBrowser));
    assert!(!narrow.is_owner());
    assert!(narrow.require(Capability::Terminal).is_err());

    let refused = call(&narrow, "term.create", serde_json::json!({ "project": "x" }))
        .await
        .expect_err("a peer without `terminal` cannot open one here either");
    assert!(
        refused.contains(Capability::Terminal.denial()),
        "expected the terminal denial, got {refused}"
    );

    // A peer acting for its own owner keeps full authority — the fleet view has
    // always been able to administer another machine, and that is unchanged.
    let owner = peer(None);
    assert!(owner.is_owner());
    for capability in Capability::ALL {
        assert!(owner.can(capability));
    }
    // But it is still not THIS machine's master, so it never receives our token.
    assert!(!owner.is_local_master());
}

#[tokio::test]
async fn sec012_a_peer_never_receives_this_machines_master_token() {
    // SEC-001 across the mesh instead of across a pairing. A peer acting for its
    // own owner has owner authority, and must still get the address only.
    for principal in [peer(None), peer(Some(&Capability::ALL))] {
        let hello = call(&principal, "hello", serde_json::json!({}))
            .await
            .expect("hello");
        let mut found = Vec::new();
        strings(&hello, &mut found);
        for value in found {
            assert!(
                !value.contains(&harness().master_token),
                "a peer must never be handed this machine's master token"
            );
        }
        assert_eq!(hello["principal"], "peer");
    }
}

#[tokio::test]
async fn sec012_replication_plumbing_is_peer_only() {
    // "mesh calls are peer-only" was previously satisfied by a local master
    // token, because peers authenticated as one. Now the check is on the
    // transport, so it means what it says.
    let payload = serde_json::json!({ "id": "nope", "deletedAt": "2026-01-01T00:00:00Z" });
    for principal in [Principal::Master, device(&Capability::ALL)] {
        let err = call(&principal, "mesh.workspaceDelete", payload.clone())
            .await
            .expect_err("only a peer link carries replication");
        assert!(err.contains("peer-only"), "got {err}");
    }
    // A peer link is accepted (the tombstone itself is a no-op for an unknown id).
    call(&peer(None), "mesh.workspaceDelete", payload)
        .await
        .expect("a peer may replicate");
}

#[tokio::test]
async fn sec012_a_frame_assertion_is_ignored_unless_the_connection_is_a_peer() {
    use threadknot_lib::protocol::MeshAssertion;
    // The security property of `effective_principal`: a phone can put any
    // assertion it likes in a frame. Only a socket that authenticated with a
    // peer credential has its assertion read.
    let forged = MeshAssertion { on_behalf_of: None }; // "I am the owner"
    let req = ClientRequest {
        id: 1,
        kind: "term.create".into(),
        payload: serde_json::json!({ "project": "x" }),
        mesh: Some(forged.clone()),
    };
    let phone = device(&[Capability::Threads]);
    let err = server::handle_request(&harness().state, &phone, req)
        .await
        .expect_err("a device cannot promote itself with a frame field");
    assert!(err.to_string().contains(Capability::Terminal.denial()));

    // The same forged frame from a device is not merely denied for `terminal` —
    // it must not have widened anything at all.
    let req = ClientRequest {
        id: 2,
        kind: "hello".into(),
        payload: serde_json::json!({}),
        mesh: Some(forged),
    };
    let hello = server::handle_request(&harness().state, &phone, req)
        .await
        .expect("hello");
    assert_eq!(hello["principal"], "device");
    assert_eq!(
        hello["capabilities"],
        serde_json::json!(["threads"]),
        "a frame assertion must not widen a device's grants"
    );
}

#[tokio::test]
async fn sec012_no_peer_credential_appears_in_any_url_we_build() {
    // The finding, restated as a test. Nothing this machine constructs to reach
    // a peer may carry a credential in the URL, because the far side refuses one
    // and because a URL is published by everything that logs it.
    let h = harness();
    let stored = h.state.peernet.registry.peer("peer-machine").unwrap();
    assert!(!stored.outbound_credential.is_empty());

    // The mesh door proves the negative from the other side: a URL credential is
    // a 400 there, so any code path that put one in a URL would fail outright
    // rather than working insecurely.
    let status = reqwest::Client::new()
        .get(format!(
            "{}/api/server-info?token={}",
            h.mesh_base, stored.outbound_credential
        ))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 400);

    // And the peer list a client sees carries neither credential nor certificate.
    let listed = call(&Principal::Master, "peer.list", serde_json::json!({}))
        .await
        .expect("peer.list");
    let mut found = Vec::new();
    strings(&listed, &mut found);
    for value in found {
        assert!(!value.contains(&stored.outbound_credential), "credential in peer.list");
        assert!(!value.contains(&stored.inbound_credential_hash), "hash in peer.list");
        assert!(!value.contains("BEGIN CERTIFICATE"), "certificate in peer.list");
    }
}

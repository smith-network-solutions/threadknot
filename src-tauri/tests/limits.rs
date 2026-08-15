//! SEC-014: bounded queues, defined slow-client behaviour, and the resource
//! caps, driven against the real handlers and a really-bound router.
//!
//! The unit tests next to the code cover the enqueue *policy* per frame class
//! (`limits.rs`), the per-principal connection budget (`sessions.rs`) and the
//! peer request queue (`peernet.rs`). What can only be checked here is that the
//! policy is actually wired to a socket: that an oversized frame is refused by
//! the door rather than by a comment, that a client which never reads is hung up
//! on instead of buffered, and that the caps produce an HTTP refusal a person
//! can read.
//!
//! One data dir and one server for the whole binary, as the authorization matrix
//! does.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use threadknot_lib::ingress::IngressPolicy;
use threadknot_lib::limits;
use threadknot_lib::mobile::{Capability, DeviceGrant, Principal};
use threadknot_lib::protocol::{Access, Agent, Mode, Project, ServerMessage, Thread, ThreadSettings};
use threadknot_lib::sessions::SessionKind;
use tokio_tungstenite::tungstenite::Message;

struct Harness {
    state: threadknot_lib::server::ServerState,
    /// `127.0.0.1:<port>` of a really-bound router (no scheme — both `http://`
    /// and `ws://` are built from it).
    authority: String,
    master_token: String,
    project: Project,
    thread: Thread,
    /// A persisted terminal record, so a `/term` request reaches the quota check
    /// instead of stopping at "unknown terminal".
    term_id: String,
}

fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("threadknot-limits-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
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
        let thread = state
            .hub
            .store
            .create_thread(
                project.id.clone(),
                Agent::Claude,
                ThreadSettings {
                    model: "sonnet".into(),
                    effort: None,
                    wide_context: false,
                    claude_chrome: false,
                    access: Access::Read,
                    mode: Mode::Build,
                    browser_profile_id: None,
                    hermes_agent_id: None,
                },
            )
            .unwrap();
        let term_id = state
            .hub
            .store
            .create_terminal(project.id.clone(), Some("Shell".into()))
            .unwrap()
            .id;

        let router = threadknot_lib::server::build_router(state.clone());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let _ = axum::serve(listener, router).await;
            });
        });

        Harness {
            state,
            authority,
            master_token,
            project,
            thread,
            term_id,
        }
    })
}

type Socket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// The hub's event broadcast is shared by every socket in this process, so the
/// tests that deliberately flood it, or that depend on nobody else doing so, take
/// turns. Without this a test asserting "no response was dropped" would be racing
/// another test that is trying to wedge a socket.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await
}

async fn connect_app(h: &Harness) -> Socket {
    connect_with(h, &h.master_token).await
}

async fn connect_with(h: &Harness, token: &str) -> Socket {
    let url = format!("ws://{}/ws?token={token}", h.authority);
    tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect /ws")
        .0
}

/// Read until the response to `id` arrives, skipping the broadcast frames every
/// client receives.
async fn await_response(socket: &mut Socket, id: u64) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(Ok(msg)) = socket.next().await {
            let Ok(text) = msg.into_text() else { continue };
            let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if frame["type"] == "response" && frame["id"].as_u64() == Some(id) {
                return frame;
            }
        }
        panic!("socket closed before response {id}");
    })
    .await
    .unwrap_or_else(|_| panic!("no response to {id}"))
}

fn request(id: u64, kind: &str, payload: serde_json::Value) -> Message {
    Message::Text(
        serde_json::json!({ "id": id, "type": kind, "payload": payload })
            .to_string()
            .into(),
    )
}

/// An HTTP WebSocket handshake without completing it, so the status a handler
/// returns before the upgrade is observable.
async fn upgrade_status(url: &str) -> (reqwest::StatusCode, String) {
    let resp = reqwest::Client::new()
        .get(url)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .expect("request");
    let status = resp.status();
    (status, resp.text().await.unwrap_or_default())
}

// ------------------------------------------------------------ frame size ---

/// A frame over the cap is refused from its length header, before its payload is
/// read — the whole point being that it costs a disconnect and not an allocation.
/// The socket is proved working first, so a green assertion cannot come from the
/// connection having been broken all along.
#[tokio::test]
async fn an_oversized_ws_frame_is_refused_instead_of_allocated() {
    let h = harness();
    let _turn = exclusive().await;
    let mut socket = connect_app(h).await;

    socket.send(request(1, "hello", serde_json::json!({}))).await.unwrap();
    assert_eq!(await_response(&mut socket, 1).await["ok"], true);

    // One byte over, sent as a single frame the way a browser sends a message.
    let oversized = "x".repeat(limits::MAX_WS_MESSAGE_BYTES + 1);
    let _ = socket
        .send(Message::Text(
            serde_json::json!({ "id": 2, "type": "hello", "payload": { "pad": oversized } })
                .to_string()
                .into(),
        ))
        .await;

    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(msg) = socket.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => return,
                Ok(_) => continue,
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "an oversized frame must end the socket rather than be buffered"
    );
}

// ----------------------------------------------------------- slow clients ---

/// Every request id gets exactly one response, even when the client falls behind
/// its own socket. A dropped response would leave the client waiting on an id
/// that no longer exists anywhere — which is why responses are the one class that
/// cannot be dropped.
///
/// The burst deliberately exceeds the rate limit, so this also asserts that being
/// throttled is an *answer*, naming the limit, rather than silence.
#[tokio::test]
async fn no_response_is_dropped_while_the_client_is_behind() {
    let h = harness();
    let _turn = exclusive().await;
    let mut socket = connect_app(h).await;

    let count = (limits::WS_REQUEST_BURST as u64) + 64;
    for id in 1..=count {
        socket
            .send(request(id, "thread.get", serde_json::json!({ "threadId": h.thread.id })))
            .await
            .unwrap();
    }

    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut throttled = 0usize;
    tokio::time::timeout(Duration::from_secs(60), async {
        while seen.len() < count as usize {
            let Some(Ok(msg)) = socket.next().await else { break };
            let Ok(text) = msg.into_text() else { continue };
            let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if frame["type"] != "response" {
                continue;
            }
            let id = frame["id"].as_u64().expect("a response carries its id");
            assert!(seen.insert(id), "response {id} arrived twice");
            if frame["error"]
                .as_str()
                .is_some_and(|e| e.contains("rate limit"))
            {
                throttled += 1;
                assert!(
                    frame["error"]
                        .as_str()
                        .unwrap()
                        .contains(&(limits::WS_REQUESTS_PER_SECOND as u64).to_string()),
                    "a throttled request must be told the limit: {frame}"
                );
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "only {} of {count} responses arrived — the rest were dropped",
            seen.len()
        )
    });

    assert_eq!(seen.len(), count as usize);
    assert!(
        throttled > 0,
        "a burst past the rate limit must be refused, not silently absorbed"
    );
}

/// A client that stops reading entirely is disconnected rather than buffered.
///
/// The bound itself is structural — the outbound channel is created with
/// `OUTBOUND_QUEUE_FRAMES` capacity, and `limits.rs` tests what happens when it
/// is full. What can only be observed from out here is the consequence: once the
/// socket, the kernel's buffers and the queue are all full, an undroppable frame
/// waits out its budget and the connection ends on its own.
#[tokio::test]
async fn a_client_that_stops_reading_is_disconnected_not_buffered_forever() {
    let h = harness();
    let _turn = exclusive().await;

    // A paired device rather than master, so the disconnect is observable from
    // the session registry by id. It has to be observed from *this* side: reading
    // the socket to find out whether it closed is what would stop it being a slow
    // client in the first place.
    let (record, credential) = h
        .state
        .mobile
        .pair("wedged".into(), "test".into(), vec![Capability::Threads])
        .unwrap();
    let socket = connect_with(h, &credential).await;
    for _ in 0..200 {
        if h.state.sessions.device_count(&record.id) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(h.state.sessions.device_count(&record.id), 1);

    // Held and never polled from here on: the client's receive buffer fills, then
    // the server's sink blocks, then its bounded queue fills.
    let _held = socket;

    // Persisted events, pushed straight onto the hub so this needs no agent.
    // Comfortably more bytes than the kernel's buffers and the queue can hold
    // between them, and every one of them is a class that must not be dropped.
    let filler = "f".repeat(48 * 1024);
    for seq in 0..(limits::OUTBOUND_QUEUE_FRAMES as i64 * 3) {
        let _ = h.state.hub.broadcast.send(ServerMessage::Event {
            thread_id: h.thread.id.clone(),
            seq,
            ts: threadknot_lib::protocol::now_iso(),
            speaker: None,
            event: threadknot_lib::protocol::AgentEvent::AssistantMessage {
                text: filler.clone(),
            },
            notice: None,
        });
    }

    // The queue is bounded structurally — it is created with
    // `OUTBOUND_QUEUE_FRAMES` capacity — so what is left to observe is the
    // consequence: the connection ends on its own once an undroppable frame has
    // waited out its budget, instead of the server holding the backlog forever.
    let deadline = std::time::Instant::now() + limits::EVENT_ENQUEUE_TIMEOUT + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if h.state.sessions.device_count(&record.id) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("a client that never drains was buffered instead of being hung up on");
}

/// The two classes are treated differently on a real socket, not just in the
/// enqueue policy: a delta is discardable and a persisted event is not. The
/// observable form of that here is the pre-existing filter this classification is
/// built on — a delta for a thread the client is not looking at never leaves the
/// machine, while a persisted event for the same thread always does, because it
/// carries status and badges the client has no other way to learn.
#[tokio::test]
async fn a_delta_is_discardable_where_a_persisted_event_is_not() {
    let h = harness();
    let _turn = exclusive().await;
    let mut socket = connect_app(h).await;
    socket.send(request(1, "hello", serde_json::json!({}))).await.unwrap();
    await_response(&mut socket, 1).await;

    let push = |seq: i64, text: &str| {
        let _ = h.state.hub.broadcast.send(ServerMessage::Event {
            thread_id: h.thread.id.clone(),
            seq,
            ts: threadknot_lib::protocol::now_iso(),
            speaker: None,
            event: threadknot_lib::protocol::AgentEvent::AssistantMessage { text: text.into() },
            notice: None,
        });
    };
    // Nothing has been opened on this socket, so it is viewing nothing.
    push(-1, "unwatched-delta");
    push(11, "unwatched-persisted");

    let mut saw_delta = false;
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(Ok(msg)) = socket.next().await {
            let Ok(text) = msg.into_text() else { continue };
            if text.contains("unwatched-delta") {
                saw_delta = true;
            }
            if text.contains("unwatched-persisted") {
                return;
            }
        }
        panic!("the socket closed before the persisted event arrived");
    })
    .await
    .expect("a persisted event is never dropped");
    assert!(
        !saw_delta,
        "a delta for a thread nobody is watching must not be sent at all"
    );

    // And once the client says what it is looking at, its deltas do arrive —
    // "droppable" is not "never delivered".
    socket
        .send(request(2, "thread.get", serde_json::json!({ "threadId": h.thread.id })))
        .await
        .unwrap();
    await_response(&mut socket, 2).await;
    push(-1, "watched-delta");
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(Ok(msg)) = socket.next().await {
            if msg.into_text().is_ok_and(|t| t.contains("watched-delta")) {
                return;
            }
        }
        panic!("the socket closed before the delta arrived");
    })
    .await
    .expect("the viewed thread's deltas still stream");
}

// ------------------------------------------------------- connection caps ---

/// The N+1th `/ws` socket for one principal is refused, before the upgrade, with
/// a message naming the limit. `/term` and `/browser` go through the same
/// `try_register` choke point with their own numbers — see below.
#[tokio::test]
async fn the_n_plus_first_app_socket_is_refused_with_a_legible_message() {
    let h = harness();
    let device = Principal::Device(DeviceGrant {
        id: "cap-app".into(),
        capabilities: Capability::ALL.to_vec(),
    });
    // Fill this principal's budget from the registry the handler consults, rather
    // than by opening 32 real sockets: same code path, no dependence on how fast
    // a socket happens to be established.
    let held: Vec<_> = (0..limits::MAX_APP_SOCKETS_PER_PRINCIPAL)
        .map(|_| {
            h.state
                .sessions
                .try_register(&device, IngressPolicy::Compat, SessionKind::App)
                .expect("under the limit")
        })
        .collect();

    let refused = h
        .state
        .sessions
        .try_register(&device, IngressPolicy::Compat, SessionKind::App);
    let message = format!("{:#}", refused.err().expect("the N+1th is refused"));
    assert!(message.contains("app"), "{message}");
    assert!(
        message.contains(&limits::MAX_APP_SOCKETS_PER_PRINCIPAL.to_string()),
        "the refusal must name the limit: {message}"
    );
    drop(held);
}

/// The `/term` and `/browser` doors refuse the N+1th socket for one principal
/// with a 429 and a readable reason, rather than upgrading and then closing —
/// after the upgrade the only vocabulary left is a close frame.
///
/// Master's budget is filled from the registry, because filling it with real
/// sockets would mean spawning two dozen login shells and eight Chromes to prove
/// a counter.
#[tokio::test]
async fn the_n_plus_first_terminal_and_browser_sockets_are_refused_by_the_door() {
    let h = harness();

    let terminals: Vec<_> = (0..limits::MAX_TERMINAL_SOCKETS_PER_PRINCIPAL)
        .map(|_| {
            h.state
                .sessions
                .try_register(&Principal::Master, IngressPolicy::Compat, SessionKind::Terminal)
                .expect("under the limit")
        })
        .collect();
    let (status, body) = upgrade_status(&format!(
        "http://{}/term?token={}&project={}&term={}",
        h.authority, h.master_token, h.project.id, h.term_id
    ))
    .await;
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(body.contains("terminal"), "{body}");
    assert!(
        body.contains(&limits::MAX_TERMINAL_SOCKETS_PER_PRINCIPAL.to_string()),
        "the refusal must name the limit: {body}"
    );
    drop(terminals);

    let browsers: Vec<_> = (0..limits::MAX_BROWSER_SOCKETS_PER_PRINCIPAL)
        .map(|_| {
            h.state
                .sessions
                .try_register(&Principal::Master, IngressPolicy::Compat, SessionKind::Browser)
                .expect("under the limit")
        })
        .collect();
    let (status, body) = upgrade_status(&format!(
        "http://{}/browser?token={}&session={}",
        h.authority, h.master_token, h.thread.id
    ))
    .await;
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(body.contains("browser"), "{body}");
    assert!(
        body.contains(&limits::MAX_BROWSER_SOCKETS_PER_PRINCIPAL.to_string()),
        "the refusal must name the limit: {body}"
    );
    drop(browsers);

    // With the budget released the same request is admitted again — a cap that
    // never lets go is an outage with a nicer error message.
    let (status, body) = upgrade_status(&format!(
        "http://{}/term?token={}&project={}&term={}",
        h.authority, h.master_token, h.project.id, h.term_id
    ))
    .await;
    assert_ne!(status, reqwest::StatusCode::TOO_MANY_REQUESTS, "{body}");
}

// --------------------------------------------------------- byte endpoints ---

/// The byte endpoints stream from disk instead of reading the whole file into
/// memory, so a large download costs one chunk of RSS rather than the file. The
/// observable half of that is the response still being correct and still carrying
/// a `Content-Length` — a streamed body has none of its own, and without it a
/// download shows no size and no progress.
#[tokio::test]
async fn a_large_file_is_streamed_with_a_declared_length() {
    let h = harness();
    let big = 3 * limits::FILE_STREAM_CHUNK + 7;
    let path = std::path::Path::new(&h.project.path).join("big.bin");
    std::fs::write(&path, vec![7u8; big]).unwrap();

    let resp = reqwest::get(format!(
        "http://{}/file?token={}&project={}&path=big.bin",
        h.authority, h.master_token, h.project.id
    ))
    .await
    .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(big.to_string().as_str()),
        "a streamed response still has to declare its length"
    );
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), big, "every chunk must arrive, exactly once");
    assert!(body.iter().all(|b| *b == 7));
}

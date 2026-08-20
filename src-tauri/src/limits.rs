//! Every number that bounds what one client can cost this machine, and the
//! small mechanisms that enforce them. SEC-014.
//!
//! The thing being protected is not a datacentre process. It is the desktop the
//! owner is sitting at, running their editor and their agents, so "the queue
//! grew until the kernel picked a victim" costs them live work rather than a pod
//! restart. A slow *remote* client — a phone on a train, a relay hop, a tab the
//! OS froze — is therefore a memory problem on the wrong machine, which is why
//! every per-connection queue here is bounded and every bound has a defined
//! behaviour for the client that hits it.
//!
//! Each constant carries what failure it prevents and roughly what a legitimate
//! heavy user looks like against it. A limit nobody can justify is a limit
//! someone raises to infinity the first time it fires.
//!
//! Deliberately *not* listed here, because they are already bounded elsewhere:
//! the hub's event broadcast and the peer relay broadcast (4096 frames each,
//! shared by all subscribers, oldest dropped on lag), the pty output fan-out
//! (1024 messages), the browser screencast fan-out (128), and terminal
//! scrollback (512 KB per session, front-trimmed).

use std::time::Duration;

// ------------------------------------------------------- outbound queueing ---

/// Frames one `/ws` socket may have queued for writing.
///
/// Prevents: a client that stops reading its socket while the hub keeps
/// producing. Before this the writer channel was unbounded, so a phone whose
/// screen locked mid-turn grew this process's heap for as long as the agent kept
/// talking.
///
/// A legitimate heavy user: the boot fan-out is a few dozen requests and their
/// responses, and a busy turn emits tens of events a second. 256 frames is
/// therefore seconds of slack for a client that is merely slow, and at a few KB
/// a frame it caps one socket's backlog near 1 MB rather than at the size of the
/// whole transcript.
pub const OUTBOUND_QUEUE_FRAMES: usize = 256;

/// How long a *response* may wait for room in that queue before the socket is
/// considered gone.
///
/// Responses cannot be dropped: the client is blocked on a request id and would
/// wait forever for a frame that no longer exists. So the only two options are
/// "wait" and "disconnect", and a connection that has not accepted a single
/// frame in 30 seconds — with a full queue behind it — is not a slow client, it
/// is a dead one. Disconnecting is recoverable; the client reconnects, replays
/// and reissues. A silently dropped response is not.
pub const RESPONSE_ENQUEUE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a *persisted* event (`seq >= 0`) may wait for room.
///
/// Shorter than a response's budget because there is nothing on the other end
/// blocked on this specific frame, but it still cannot be dropped: persisted
/// events drive thread status, attention badges and notifications for threads
/// the client is not looking at, so losing one leaves the UI quietly wrong until
/// the next full reload. Blocking briefly and then disconnecting trades a wrong
/// UI for a reconnect that rebuilds it.
pub const EVENT_ENQUEUE_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest `/ws` message this machine will accept, and the largest single frame
/// it will read into memory.
///
/// Prevents: one text frame declaring a body larger than RAM. Both halves are
/// needed — the frame cap is checked against the length in the frame header
/// before the payload is read, so it refuses rather than allocates; the message
/// cap stops the same thing arriving as a thousand fragments.
///
/// A legitimate heavy user: the largest real frame is a `turn.start` carrying
/// base64 attachments, which costs 4/3 of the file bytes. 32 MiB therefore
/// covers about 24 MB of attachments in one turn — several large photos, a big
/// PDF, or a short screen recording. tungstenite's default is 64 MiB, so this
/// halves the worst case a flood of sockets can pin.
pub const MAX_WS_MESSAGE_BYTES: usize = 32 << 20;

// HTTP request bodies need no constant here: axum's `DefaultBodyLimit` already
// caps them at 2 MiB, it applies to every extractor in this app that reads a body
// (`axum::Json`, on the pairing, push, session and peer routes) and it is nowhere
// disabled. Adding a layer would only create a second number to keep in
// agreement. The bodies that arrive on those routes — pairing codes, push tokens,
// peer handshakes — are hundreds of bytes.

/// The same cap for `/term` and `/browser`, whose client frames are JSON control
/// messages rather than payload carriers.
///
/// A legitimate heavy user: pasting a file's contents into a terminal, which
/// arrives as one `input` frame. 8 MiB of pasted text is already far past what
/// a shell will do anything sensible with.
pub const MAX_CONTROL_WS_MESSAGE_BYTES: usize = 8 << 20;

/// Apply the control-socket frame caps to a `/term` or `/browser` upgrade.
///
/// Inbound only, and that is the half that matters: the cap is checked against
/// the frame's length header, so an oversized frame is refused before its payload
/// is read. Outbound pty output and screencast frames are unaffected — they are
/// bounded by each session's own broadcast ring.
pub fn control_frame_caps(
    ws: axum::extract::ws::WebSocketUpgrade,
) -> axum::extract::ws::WebSocketUpgrade {
    ws.max_message_size(MAX_CONTROL_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_CONTROL_WS_MESSAGE_BYTES)
}

// --------------------------------------------------------- request pressure ---

/// Sustained requests per second one `/ws` connection may issue.
///
/// Per *connection*, not per principal, deliberately: a shared per-principal
/// bucket would put a mutex on the hottest path in the process, and the
/// connection caps below already bound how many buckets one principal can hold.
/// The pairing is the point — rate × connections is the real ceiling.
pub const WS_REQUESTS_PER_SECOND: f64 = 50.0;

/// Requests one `/ws` connection may issue back-to-back before the sustained
/// rate applies.
///
/// A legitimate heavy user: the client's boot fan-out is the burstiest thing it
/// ever does — hello, projects, threads, workspaces, git status per project,
/// usage — and lands in the low dozens. 200 leaves an order of magnitude, while
/// still stopping a wedged client from spinning requests at CPU speed.
pub const WS_REQUEST_BURST: f64 = 200.0;

// ------------------------------------------------------------- connections ---

/// Live `/ws` sockets one principal may hold at once.
///
/// Prevents: a reconnect storm, or a device that simply opens sockets, costing
/// one queue and one broadcast subscription each.
///
/// A legitimate heavy user: the desktop webview, a couple of LAN browser tabs
/// and two phones is five. 32 covers a machine whose owner leaves tabs open
/// everywhere, plus the window where a reconnect overlaps the socket it
/// replaces.
pub const MAX_APP_SOCKETS_PER_PRINCIPAL: usize = 32;

/// Live `/term` sockets one principal may hold at once.
///
/// A legitimate heavy user: one socket per visible terminal tab. Two dozen open
/// terminals is already an unusual desktop.
pub const MAX_TERMINAL_SOCKETS_PER_PRINCIPAL: usize = 24;

/// Live `/browser` sockets one principal may hold at once. Lower than terminals
/// because each attachment is a screencast, which is the most expensive stream
/// this app produces.
pub const MAX_BROWSER_SOCKETS_PER_PRINCIPAL: usize = 8;

// -------------------------------------------------------- backing resources ---

/// Live pty sessions on this machine, across every principal.
///
/// The socket cap above is not enough on its own: a pty **outlives its socket**
/// on purpose, so attaching and detaching in a loop would leave a shell, two
/// threads and 512 KB of scrollback behind on every pass.
///
/// A legitimate heavy user: a terminal per project plus a few scratch shells.
/// 32 live shells is generous; the message says so when it refuses.
pub const MAX_LIVE_TERMINAL_SESSIONS: usize = 32;

/// Live browser sessions on this machine, across every principal.
///
/// Each one is a Chrome with its own profile directory — hundreds of MB of RSS
/// and a real process tree — and the session key is a caller-supplied string, so
/// without this an authenticated client could ask for a thousand of them.
///
/// A legitimate heavy user: browser-driving threads open one each, and the idle
/// reaper closes any session nobody is watching. Eight simultaneous Chromes is
/// already ~2.5 GB, which is past the point where the desktop stops being
/// pleasant to use.
pub const MAX_LIVE_BROWSER_SESSIONS: usize = 8;

// ------------------------------------------------------------- dispatch ---

/// Hops from a human-driven thread that a dispatch may travel. `1` means a
/// person's thread can dispatch workers, and those workers cannot dispatch
/// further.
///
/// Prevents: the fork bomb. An agent that can delegate to agents that can
/// delegate is unbounded work against the owner's subscription, spread across
/// every machine they own, with nobody in the loop. Raising this to 2 buys
/// planner → lead → workers and doubles the blast radius; it is deliberately
/// not a setting.
pub const MAX_DISPATCH_DEPTH: u8 = 1;

/// Workers one parent thread may have out at once.
///
/// A legitimate heavy user: a build fanned out to every machine in the fleet,
/// plus a couple of parallel implementation jobs. Eight covers that; more is a
/// planner in a loop.
pub const MAX_LIVE_DISPATCHES_PER_PARENT: usize = 8;

/// Workers running on one machine, across every parent.
///
/// Each is a full agent CLI with its own context and its own compiler; four is
/// already more than a laptop enjoys.
pub const MAX_LIVE_DISPATCHES_PER_MACHINE: usize = 4;

/// Finished dispatches kept in the ledger. Live ones are never trimmed.
pub const MAX_REMEMBERED_DISPATCHES: usize = 200;

/// Longest summary a worker can report back. The summary is the whole channel
/// between the two agents, so it has to be generous — but it lands in the
/// parent's context, so it cannot be a transcript.
pub const DISPATCH_SUMMARY_MAX: usize = 8_000;

/// How often the parent's reconcile loop wakes.
///
/// This is a *tick*, not a poll rate: on each tick it only contacts workers
/// whose push channel has gone quiet (see `DISPATCH_PUSH_SILENCE`). While
/// pushes are arriving — the normal case — the loop costs nothing but a wakeup,
/// so this can be fast enough that the degraded path still feels live.
pub const DISPATCH_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

/// How long the parent waits to hear from a worker before it starts asking.
///
/// Deliberately a small multiple of `DISPATCH_PROGRESS_INTERVAL`: one missed
/// push is jitter, three in a row means the worker cannot reach us and polling
/// is the only channel left. Too low and every healthy dispatch gets polled as
/// well as pushed; too high and a one-way-reachable worker looks frozen.
pub const DISPATCH_PUSH_SILENCE: Duration = Duration::from_secs(7);

/// How often a running worker's activity is forwarded to its parent.
///
/// Progress is a reassurance, not a transcript. It is also *transient* — never
/// persisted, so it never lands in the parent model's context — which means the
/// only cost of being prompt is a small frame on an already-open socket. Two
/// seconds reads as live; slower reads as stalled.
pub const DISPATCH_PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

// ------------------------------------------------------------ exec jobs ---

/// Commands running at once on this machine, across every caller.
///
/// Prevents: a fan-out that dispatches a build to every root in a workspace and
/// discovers the machine has eleven of them. Each job is a shell plus whatever
/// it spawned — a compiler saturates the box on its own.
///
/// A legitimate heavy user: a build, a test run and a couple of quick queries
/// in flight together. Eight is comfortably past that and still leaves the
/// desktop usable.
pub const MAX_LIVE_EXEC_JOBS: usize = 8;

/// Finished jobs kept so their output can still be fetched. Running jobs are
/// never evicted; these are the corpses, and 64 of them is a few hundred KB.
pub const MAX_REMEMBERED_EXEC_JOBS: usize = 64;

/// Captured output per stream, per job, front-trimmed like terminal scrollback.
/// A build that prints a million lines must not become this process's memory
/// problem, and the *end* of a log is the part that says what went wrong.
pub const EXEC_STREAM_BUFFER: usize = 256 * 1024;

/// How long `exec.start` waits before answering, so a command that takes 40 ms
/// comes back complete in one round trip instead of forcing a poll. Must stay
/// far below `PEER_REQUEST_TIMEOUT` — a routed start that outlives it wedges a
/// link shared with every other user of that machine.
pub const EXEC_START_GRACE: Duration = Duration::from_millis(1500);

/// The longest a single `exec.status` may park server-side. Also bounded by the
/// peer request timeout, for the same reason.
pub const EXEC_MAX_ROUTED_WAIT: Duration = Duration::from_secs(20);

/// Default wall-clock ceiling for one command.
pub const EXEC_DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// The most a caller may ask for. A release build of a Tauri app on a cold
/// cache genuinely takes tens of minutes, so this is high on purpose; the job
/// is killed rather than left to run forever.
pub const EXEC_MAX_TIMEOUT: Duration = Duration::from_secs(3 * 3600);

/// Grace between SIGTERM and SIGKILL when cancelling a job's process group.
pub const EXEC_KILL_GRACE: Duration = Duration::from_secs(5);

/// Requests that may be queued for one connected peer before the queue is
/// treated as evidence that the peer is not draining it.
///
/// Prevents: a paired machine that accepts TCP but stops reading, which used to
/// grow an unbounded queue on *this* side for as long as callers kept asking.
///
/// A legitimate heavy user: the fleet view fans out a handful of requests per
/// peer, and a remote file tree a few dozen thumbnails. 64 outstanding requests
/// to one machine means the far side has stopped answering, which is what the
/// error says.
pub const PEER_REQUEST_QUEUE: usize = 64;

/// Remote servers this machine may be a guest on at once (`servers.rs`).
///
/// Each record holds an open socket and a reconnect loop, so the cost is real
/// but small; the cap is here because a runaway "add server" loop should fail
/// loudly rather than open connections until the file descriptors run out.
/// Being a guest on more than a handful of other people's machines is not a
/// workflow anyone has, so the number is deliberately unambitious.
pub const MAX_REMOTE_SERVERS: usize = 8;

/// Bytes read per chunk when streaming a file response.
///
/// The size of the *whole* buffer for a download: `Body::from_stream` is polled
/// by hyper as it writes, so a slow client stalls the stream instead of
/// accumulating the file in memory. That is what makes a per-file size cap
/// unnecessary — a 4 GB video costs 64 KB of RSS, and it used to cost 4 GB.
pub const FILE_STREAM_CHUNK: usize = 64 * 1024;

// ----------------------------------------------------------- frame classing ---

/// What a frame costs to lose, which is what decides how hard we try to send it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameClass {
    /// A reply to a request id. Never droppable — see `RESPONSE_ENQUEUE_TIMEOUT`.
    Response,
    /// A persisted event (`seq >= 0`), a state change, usage, presence. Never
    /// droppable — see `EVENT_ENQUEUE_TIMEOUT`.
    Persisted,
    /// A streaming delta (`seq < 0`). Droppable, and already dropped today for
    /// any thread the client is not viewing: the text it carries is superseded
    /// by the persisted event that closes the message.
    Delta,
}

impl FrameClass {
    /// How long to wait for queue space, or `None` for "never wait, drop it".
    pub fn budget(self) -> Option<Duration> {
        match self {
            FrameClass::Response => Some(RESPONSE_ENQUEUE_TIMEOUT),
            FrameClass::Persisted => Some(EVENT_ENQUEUE_TIMEOUT),
            FrameClass::Delta => None,
        }
    }
}

/// What happened to a frame handed to [`enqueue`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
    Sent,
    /// Droppable and dropped, because the client is behind.
    Dropped,
    /// The frame could not be placed and was not droppable: close the socket.
    Hangup,
}

/// Put one frame on a socket's bounded outbound queue under its class's policy.
///
/// One FIFO queue for every class rather than a priority lane per class. A lane
/// that let responses overtake queued deltas would deliver a delta *after* the
/// persisted event that supersedes it, and the client applies deltas
/// unconditionally — it would re-render text it has already replaced. Ordering
/// is worth more than the few hundred milliseconds a separate lane would save,
/// and a full queue stops accepting deltas anyway, so it drains.
pub async fn enqueue(
    tx: &tokio::sync::mpsc::Sender<String>,
    frame: String,
    class: FrameClass,
) -> Enqueued {
    enqueue_within(tx, frame, class.budget()).await
}

/// [`enqueue`] with the budget stated outright, so the timeout path can be
/// exercised in a test without waiting out a production budget.
async fn enqueue_within(
    tx: &tokio::sync::mpsc::Sender<String>,
    frame: String,
    budget: Option<Duration>,
) -> Enqueued {
    match budget {
        None => match tx.try_send(frame) {
            Ok(()) => Enqueued::Sent,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Enqueued::Dropped,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Enqueued::Hangup,
        },
        Some(budget) => match tx.send_timeout(frame, budget).await {
            Ok(()) => Enqueued::Sent,
            // Timed out, or the writer is gone. Both mean this socket cannot be
            // served correctly any more.
            Err(_) => Enqueued::Hangup,
        },
    }
}

// -------------------------------------------------------------- rate limiter ---

/// A token bucket owned by exactly one task.
///
/// No interior mutability and no shared state on purpose: the caller holds it in
/// its own loop, so rate limiting costs an `Instant::now()` and no
/// synchronisation at all on the hot path.
pub struct TokenBucket {
    tokens: f64,
    per_second: f64,
    burst: f64,
    last: std::time::Instant,
}

impl TokenBucket {
    pub fn new(per_second: f64, burst: f64) -> Self {
        Self {
            tokens: burst,
            per_second,
            burst,
            last: std::time::Instant::now(),
        }
    }

    /// Refill for elapsed time and take one token. `false` means the caller is
    /// over its rate.
    pub fn take(&mut self) -> bool {
        let now = std::time::Instant::now();
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_is_never_droppable_and_a_delta_always_is() {
        assert!(FrameClass::Response.budget().is_some());
        assert!(FrameClass::Persisted.budget().is_some());
        assert!(FrameClass::Delta.budget().is_none());
        // A response is what a request id is waiting on, so it gets at least as
        // long as an event does.
        assert!(FrameClass::Response.budget() >= FrameClass::Persisted.budget());
    }

    #[tokio::test]
    async fn a_full_queue_drops_deltas_and_hangs_up_on_the_rest() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1);
        assert_eq!(
            enqueue(&tx, "first".into(), FrameClass::Delta).await,
            Enqueued::Sent
        );
        // Now full.
        assert_eq!(
            enqueue(&tx, "delta".into(), FrameClass::Delta).await,
            Enqueued::Dropped
        );
    }

    /// The asymmetry SEC-014 asks for, in one place: with the queue full, the
    /// droppable class is dropped immediately and the undroppable class waits for
    /// the client to catch up and is then delivered.
    #[tokio::test]
    async fn under_pressure_a_delta_is_dropped_and_a_persisted_event_waits() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
        tx.send("already queued".into()).await.unwrap();

        assert_eq!(
            enqueue(&tx, "delta".into(), FrameClass::Delta).await,
            Enqueued::Dropped,
            "a delta must never wait for a slow client"
        );

        // The client drains a moment later, as a merely-slow one does.
        let drain = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let first = rx.recv().await;
            let second = rx.recv().await;
            (first, second)
        });
        assert_eq!(
            enqueue(&tx, "persisted".into(), FrameClass::Persisted).await,
            Enqueued::Sent,
            "a persisted event must be delivered, not dropped"
        );
        let (first, second) = drain.await.unwrap();
        assert_eq!(first.as_deref(), Some("already queued"));
        assert_eq!(
            second.as_deref(),
            Some("persisted"),
            "and the delta must not have taken its place in the queue"
        );
    }

    /// A client that never drains is hung up on rather than buffered forever.
    /// Driven with a short budget so the test does not sit out a production one;
    /// what the budget *is* for each class is asserted above.
    #[tokio::test]
    async fn a_client_that_never_drains_is_hung_up_on_rather_than_buffered() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1);
        tx.send("wedged".into()).await.unwrap();
        let budget = Some(Duration::from_millis(30));
        assert_eq!(
            enqueue_within(&tx, "event".into(), budget).await,
            Enqueued::Hangup
        );
        assert_eq!(
            enqueue_within(&tx, "response".into(), budget).await,
            Enqueued::Hangup
        );
        // The receiver is still alive and the queue still holds exactly what it
        // held: the frames that could not be placed were not smuggled in behind
        // the bound.
        assert_eq!(tx.capacity(), 0, "the bound is still the bound");
    }

    #[tokio::test]
    async fn a_dead_writer_hangs_up_rather_than_blocking_forever() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
        drop(rx);
        assert_eq!(
            enqueue(&tx, "r".into(), FrameClass::Response).await,
            Enqueued::Hangup
        );
        assert_eq!(
            enqueue(&tx, "d".into(), FrameClass::Delta).await,
            Enqueued::Hangup
        );
    }

    #[test]
    fn the_bucket_allows_a_burst_then_throttles() {
        let mut bucket = TokenBucket::new(10.0, 3.0);
        assert!(bucket.take());
        assert!(bucket.take());
        assert!(bucket.take());
        assert!(!bucket.take(), "the burst is spent");
    }

    #[test]
    fn the_bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(100.0, 1.0);
        assert!(bucket.take());
        assert!(!bucket.take(), "spent, and no time has passed");
        std::thread::sleep(Duration::from_millis(50));
        assert!(bucket.take(), "elapsed time must refill");
    }
}

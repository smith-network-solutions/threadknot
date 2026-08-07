//! Interactive pty terminals (portable-pty) exposed over a dedicated
//! WebSocket: binary frames = pty output, text frames = JSON control.
//! Sessions are keyed by (projectId, termId), survive client disconnects,
//! and replay a scrollback ring buffer on attach.

use crate::server::ServerState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Scrollback cap per session: trimmed from the front once exceeded.
const SCROLLBACK_CAP: usize = 512 * 1024;
const READ_CHUNK: usize = 8 * 1024;
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
/// Minimum wall-clock gap between scrollback disk flushes while output streams.
const FLUSH_INTERVAL: Duration = Duration::from_millis(2000);
/// Shown once when a fresh shell is spawned over a restored scrollback buffer.
const RESTORE_MARKER: &[u8] = b"\r\n\x1b[2m\xe2\x94\x80\xe2\x94\x80 previous session restored \xe2\x94\x80\xe2\x94\x80\x1b[0m\r\n";

/// A message fanned out to every attached client of a session.
#[derive(Clone)]
enum TermMsg {
    /// Raw pty output bytes.
    Output(Vec<u8>),
    /// The shell exited with this code (None if unknown).
    Exit(Option<i32>),
    /// This client id is now the session's sole answerer of terminal queries
    /// (see the `responder` note on `bridge`).
    Responder(u64),
}

/// One live (or recently-dead) pty-backed shell.
struct Session {
    /// Writer half of the pty master — bytes typed by the client.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Master pty, kept for `resize`.
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    /// Child shell handle, kept for `kill` / `wait`.
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    /// Fan-out of pty output + exit notice to all attached sockets.
    tx: tokio::sync::broadcast::Sender<TermMsg>,
    /// Replay buffer (front-trimmed ring). Also guards `tx` sends so a
    /// freshly-attaching client can snapshot + subscribe without gaps/dupes.
    scrollback: Mutex<VecDeque<u8>>,
    /// Where the ring buffer is mirrored to disk, so it survives an app restart
    /// (the pty process itself cannot — a fresh shell is spawned on reopen).
    scrollback_path: PathBuf,
    /// Where the shell's last-known working directory is mirrored, so a restored
    /// terminal reopens in the same directory the user had `cd`'d into.
    cwd_path: PathBuf,
    /// The shell's process id, used to read its live cwd (Linux `/proc`).
    pid: Option<u32>,
    /// Set on new output, cleared on flush — lets the flusher thread skip
    /// writing an unchanged buffer.
    dirty: AtomicBool,
    /// False once the shell has exited.
    alive: AtomicBool,
    /// Attached client ids in attach order (newest last) — the newest is the
    /// query responder. See `bridge`.
    clients: Mutex<Vec<u64>>,
    /// Hands out the per-attach client ids above.
    next_client: AtomicU64,
}

impl Session {
    /// Append output to the ring buffer and broadcast it — done together
    /// under the scrollback lock so `attach` sees no gap or duplicate.
    fn push_output(&self, bytes: Vec<u8>) {
        {
            let mut sb = self.scrollback.lock().unwrap();
            sb.extend(bytes.iter().copied());
            let overflow = sb.len().saturating_sub(SCROLLBACK_CAP);
            if overflow > 0 {
                sb.drain(0..overflow);
            }
            let _ = self.tx.send(TermMsg::Output(bytes));
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Mirror the current ring buffer to disk (atomic rename). Best-effort:
    /// terminal history is a nicety, so any IO error is silently ignored.
    fn persist_scrollback(&self) {
        // Clear the flag first so output arriving mid-write re-marks it dirty.
        self.dirty.store(false, Ordering::Relaxed);
        let bytes: Vec<u8> = {
            let sb = self.scrollback.lock().unwrap();
            sb.iter().copied().collect()
        };
        if let Some(parent) = self.scrollback_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.scrollback_path.with_extension("scrollback.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &self.scrollback_path);
        }
    }

    /// Record the shell's current working directory to disk, so a restored
    /// terminal can reopen there. Best-effort and Linux-only for now (reads
    /// `/proc/<pid>/cwd`); other platforms fall back to the project root.
    fn capture_cwd(&self) {
        if let Some(dir) = self.pid.and_then(proc_cwd) {
            let _ = std::fs::write(&self.cwd_path, dir);
        }
    }
}

/// The live working directory of a process, or `None` if it can't be read on
/// this platform. Linux: the `/proc/<pid>/cwd` symlink.
fn proc_cwd(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// A previously-saved cwd for a terminal, if it still points at a real dir.
fn saved_cwd(cwd_path: &Path) -> Option<String> {
    let dir = std::fs::read_to_string(cwd_path).ok()?;
    let dir = dir.trim().to_string();
    (!dir.is_empty() && Path::new(&dir).is_dir()).then_some(dir)
}

/// Live pty sessions. Constructed once in `build_server_state`. The persisted
/// *record* of each terminal (id, name) lives in the `Store`; this registry
/// only owns the running processes and their on-disk scrollback mirrors.
pub struct TermRegistry {
    sessions: Mutex<HashMap<(String, String), Arc<Session>>>,
    /// Directory holding `<termId>.scrollback` files (`~/.threadknot/terminals`).
    dir: PathBuf,
}

impl TermRegistry {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            sessions: Mutex::new(HashMap::new()),
            dir,
        }
    }

    /// On-disk scrollback path for a terminal id.
    fn scrollback_path(&self, term_id: &str) -> PathBuf {
        self.dir.join(format!("{}.scrollback", safe_segment(term_id)))
    }

    /// On-disk saved-cwd path for a terminal id.
    fn cwd_path(&self, term_id: &str) -> PathBuf {
        self.dir.join(format!("{}.cwd", safe_segment(term_id)))
    }

    /// Whether a live (running) session currently exists for this terminal.
    pub fn alive(&self, project_id: &str, term_id: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(&(project_id.to_string(), term_id.to_string()))
            .map(|s| s.alive.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Permanently remove a terminal: kill any live shell, drop its session,
    /// and delete its persisted scrollback. Idempotent.
    pub fn delete(&self, project_id: &str, term_id: &str) {
        let key = (project_id.to_string(), term_id.to_string());
        if let Some(session) = self.sessions.lock().unwrap().remove(&key) {
            if let Ok(mut c) = session.child.lock() {
                let _ = c.kill();
            }
            session.alive.store(false, Ordering::Relaxed);
        }
        let _ = std::fs::remove_file(self.scrollback_path(term_id));
        let _ = std::fs::remove_file(self.cwd_path(term_id));
    }

    /// Get a live session for `(project, term)`, spawning a fresh shell if
    /// none exists or the previous one has died. Sync (no await): safe to call
    /// while nothing else is held. A fresh spawn seeds its ring buffer from any
    /// persisted scrollback so reopening a terminal shows its prior history.
    fn get_or_spawn(
        self: &Arc<Self>,
        key: (String, String),
        cwd: &str,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Arc<Session>> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(existing) = sessions.get(&key) {
            if existing.alive.load(Ordering::Relaxed) {
                return Ok(Arc::clone(existing));
            }
            // Dead session — drop it and spawn a replacement (restart on reconnect).
            sessions.remove(&key);
        }
        let scrollback_path = self.scrollback_path(&key.1);
        let cwd_path = self.cwd_path(&key.1);
        // Reopen in the shell's last-known directory if we have one; otherwise
        // fall back to the project root.
        let start_cwd = saved_cwd(&cwd_path).unwrap_or_else(|| cwd.to_string());
        let session = spawn_session(&start_cwd, cols, rows, scrollback_path, cwd_path)?;
        sessions.insert(key.clone(), Arc::clone(&session));
        drop(sessions);

        // Reader thread: pty output is blocking, so read on a dedicated std
        // thread. On EOF, reap the child and broadcast an exit notice.
        let reader_session = Arc::clone(&session);
        let mut reader = session.master.lock().unwrap().try_clone_reader()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => reader_session.push_output(buf[..n].to_vec()),
                }
            }
            reader_session.alive.store(false, Ordering::Relaxed);
            // Persist the final buffer so the last output survives a restart.
            reader_session.persist_scrollback();
            let code = reader_session
                .child
                .lock()
                .unwrap()
                .wait()
                .ok()
                .map(|st| st.exit_code() as i32);
            let _ = reader_session.tx.send(TermMsg::Exit(code));
            // Keep the dead session in the registry so a late attach can still
            // replay the buffer; it is replaced on the next attach with the
            // same key.
        });

        // Flusher thread: periodically mirror the ring buffer to disk while the
        // shell lives, so history survives even a hard kill of the app. Exits
        // once the shell has died (the reader thread wrote the final buffer).
        let flush_session = Arc::clone(&session);
        std::thread::spawn(move || loop {
            std::thread::sleep(FLUSH_INTERVAL);
            let alive = flush_session.alive.load(Ordering::Relaxed);
            if alive {
                // Capture cwd while the process is live (it vanishes on exit).
                flush_session.capture_cwd();
            }
            if flush_session.dirty.load(Ordering::Relaxed) {
                flush_session.persist_scrollback();
            }
            if !alive {
                break;
            }
        });

        Ok(session)
    }
}

/// The user's interactive shell for this platform.
/// Unix: `$SHELL -l` (fallback `/bin/bash`). Windows: PowerShell 7 if
/// installed, else Windows PowerShell (no login flag — not a concept there).
fn default_shell() -> CommandBuilder {
    #[cfg(windows)]
    {
        let shell = if which::which("pwsh.exe").is_ok() {
            "pwsh.exe"
        } else {
            "powershell.exe"
        };
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(["-NoLogo"]);
        cmd
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd
    }
}

/// Spawn a login shell wired to a fresh pty of the requested size. If a
/// persisted scrollback exists at `scrollback_path`, seed the ring buffer with
/// it (plus a "restored" marker) so reopening a terminal shows prior history.
fn spawn_session(
    cwd: &str,
    cols: u16,
    rows: u16,
    scrollback_path: PathBuf,
    cwd_path: PathBuf,
) -> anyhow::Result<Arc<Session>> {
    let pty = native_pty_system().openpty(PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = default_shell();
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = pty.slave.spawn_command(cmd)?;
    let pid = child.process_id();
    // Drop the slave so the master reader sees EOF when the child exits.
    drop(pty.slave);

    let writer = pty.master.take_writer()?;
    let (tx, _rx) = tokio::sync::broadcast::channel::<TermMsg>(1024);

    // Seed the ring from disk (trimmed to the cap, keeping the most recent).
    let mut scrollback: VecDeque<u8> = VecDeque::new();
    if let Ok(mut saved) = std::fs::read(&scrollback_path) {
        if saved.len() > SCROLLBACK_CAP {
            saved.drain(0..saved.len() - SCROLLBACK_CAP);
        }
        if !saved.is_empty() {
            scrollback.extend(saved);
            scrollback.extend(RESTORE_MARKER.iter().copied());
        }
    }

    Ok(Arc::new(Session {
        writer: Mutex::new(writer),
        master: Mutex::new(pty.master),
        child: Mutex::new(child),
        tx,
        scrollback: Mutex::new(scrollback),
        scrollback_path,
        cwd_path,
        pid,
        dirty: AtomicBool::new(false),
        alive: AtomicBool::new(true),
        clients: Mutex::new(Vec::new()),
        next_client: AtomicU64::new(0),
    }))
}

/// Sanitize a terminal id for use as a filename (uuids already qualify).
fn safe_segment(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// Client -> server control frames (WS Text, JSON).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Control {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Kill,
    /// "I'm the view being looked at" — take over answering terminal queries.
    Claim,
}

fn param<'a>(params: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    params.get(key).map(String::as_str)
}

/// `GET /term?token=…&project=…&term=…&cols=…&rows=…` — attach (or spawn)
/// the pty session and bridge it to the socket.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<ServerState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    if state.authenticate(param(&params, "token").unwrap_or("")).is_none() {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    // Remote-machine terminal: splice this socket onto the owner's /term.
    if let Some(mid) = param(&params, "machineId") {
        if mid != state.device.machine_id {
            let mid = mid.to_string();
            return ws.on_upgrade(move |socket| async move {
                crate::peernet::splice_term(socket, state, mid, params).await;
            });
        }
    }
    let Some(project_id) = param(&params, "project").map(String::from) else {
        return (StatusCode::BAD_REQUEST, "missing project").into_response();
    };
    let Some(project) = state.hub.store.project(&project_id) else {
        return (StatusCode::NOT_FOUND, "unknown project").into_response();
    };
    let Some(term_id) = param(&params, "term").map(String::from) else {
        return (StatusCode::BAD_REQUEST, "missing term").into_response();
    };
    // Only attach to terminals that have a persisted record for this project;
    // this prevents orphan pty sessions / scrollback files with no owning tab.
    let owned = state
        .hub
        .store
        .terminal(&term_id)
        .is_some_and(|t| t.project_id == project_id);
    if !owned {
        return (StatusCode::NOT_FOUND, "unknown terminal").into_response();
    }
    let cols = param(&params, "cols").and_then(|c| c.parse().ok()).unwrap_or(DEFAULT_COLS);
    let rows = param(&params, "rows").and_then(|r| r.parse().ok()).unwrap_or(DEFAULT_ROWS);

    let registry = Arc::clone(&state.terms);
    let session = match registry.get_or_spawn(
        (project_id, term_id.clone()),
        &project.path,
        cols,
        rows,
    ) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("spawn failed: {e:#}"))
                .into_response();
        }
    };

    ws.on_upgrade(move |socket| bridge(socket, session, cols, rows))
}

/// Bridge one WebSocket client to a session: replay buffer, fan-out output,
/// forward control frames. The session outlives the socket.
///
/// Two things here exist to keep xterm's *answers* out of the pty. A terminal
/// emulator replies to the queries it parses (cursor position `ESC[6n`, device
/// attributes `ESC[c`, OSC 10/11 colour) and those replies enter the pty as if
/// typed. So:
///  * the scrollback replay is announced with a `replay` frame, letting the
///    client mute itself while it re-parses history — otherwise every attach
///    re-answers questions that programs asked (and consumed) long ago, and the
///    answers pile up at the shell prompt as `;1R10;rgb:d8d8/dddd/e9e911…`;
///  * one attached client (the newest) is named `responder` so a session viewed
///    from two places doesn't answer live queries twice, with the loser's copy
///    landing at the prompt as the same gibberish.
async fn bridge(socket: WebSocket, session: Arc<Session>, cols: u16, rows: u16) {
    let (mut sink, mut stream) = socket.split();

    // Resize the pty to this client's viewport (last-resize-wins).
    resize(&session, cols, rows);

    // Snapshot the replay buffer and subscribe atomically under the scrollback
    // lock so no bytes are missed or double-sent between the two.
    let (replay, mut rx) = {
        let sb = session.scrollback.lock().unwrap();
        let rx = session.tx.subscribe();
        (Vec::from_iter(sb.iter().copied()), rx)
    };

    // Attaching last makes us the responder. Broadcast after subscribing above
    // so our own out task sees the election and tells us we won.
    let me = session.next_client.fetch_add(1, Ordering::Relaxed);
    session.clients.lock().unwrap().push(me);
    let _ = session.tx.send(TermMsg::Responder(me));

    if !replay.is_empty() {
        let announce = json!({ "type": "replay", "bytes": replay.len() }).to_string();
        if sink.send(Message::Text(announce.into())).await.is_err() {
            detach(&session, me);
            return;
        }
        if sink.send(Message::Binary(replay.into())).await.is_err() {
            detach(&session, me);
            return;
        }
    }
    // If the shell already died, tell the client right away.
    if !session.alive.load(Ordering::Relaxed) {
        let _ = sink
            .send(Message::Text(json!({ "type": "exit", "code": Value::Null }).to_string().into()))
            .await;
    }

    // Output task: broadcast -> WS.
    let out = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(TermMsg::Output(bytes)) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Ok(TermMsg::Exit(code)) => {
                    let frame = json!({ "type": "exit", "code": code }).to_string();
                    let _ = sink.send(Message::Text(frame.into())).await;
                    break;
                }
                Ok(TermMsg::Responder(id)) => {
                    let frame = json!({ "type": "role", "responder": id == me }).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Input loop: WS control frames -> pty. Ends on client disconnect; the
    // session is intentionally NOT torn down (reattach is the whole point).
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(ctrl) = serde_json::from_str::<Control>(&text) else { continue };
        match ctrl {
            Control::Input { data } => {
                if let Ok(mut w) = session.writer.lock() {
                    let _ = w.write_all(data.as_bytes());
                    let _ = w.flush();
                }
            }
            Control::Resize { cols, rows } => resize(&session, cols, rows),
            Control::Kill => {
                // Kill the shell but KEEP the session and tab: the reader thread
                // observes EOF, persists the buffer, and broadcasts `exit`, so
                // the client shows a restart prompt. A permanent removal is a
                // separate `term.delete` request. (Client stays attached.)
                if let Ok(mut c) = session.child.lock() {
                    let _ = c.kill();
                }
            }
            Control::Claim => {
                // A backgrounded tab can hold its socket open while its JS is
                // frozen, so "newest attach" alone could leave a query
                // unanswered. The visible view claims the role instead.
                let already = {
                    let mut clients = session.clients.lock().unwrap();
                    let already = clients.last() == Some(&me);
                    if !already {
                        clients.retain(|c| *c != me);
                        clients.push(me);
                    }
                    already
                };
                if !already {
                    let _ = session.tx.send(TermMsg::Responder(me));
                }
            }
        }
    }

    out.abort();
    detach(&session, me);
}

/// Drop a client from the session and re-elect the responder (newest remaining
/// attach), so the last view standing always answers terminal queries again.
fn detach(session: &Session, me: u64) {
    let next = {
        let mut clients = session.clients.lock().unwrap();
        clients.retain(|c| *c != me);
        clients.last().copied()
    };
    if let Some(next) = next {
        let _ = session.tx.send(TermMsg::Responder(next));
    }
}

/// Resize the pty master, ignoring errors (a dead pty just no-ops).
fn resize(session: &Session, cols: u16, rows: u16) {
    if let Ok(master) = session.master.lock() {
        let _ = master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

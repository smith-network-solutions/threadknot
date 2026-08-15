//! Kimi Code driver: speaks the official Agent Client Protocol (`kimi acp`)
//! over newline-delimited JSON-RPC 2.0 on stdio.
//!
//! Kimi's OAuth login and subscription accounting stay entirely inside the
//! installed CLI. Threadknot never reads a credential or calls the model API.

use super::{AgentCommand, AttachmentRef, DriverCtx};
use crate::protocol::*;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

pub const DEFAULT_MODEL: &str = "kimi-code/k3";

/// Kimi's managed catalog is refreshed during login and exposed only after a
/// session opens. Keep a useful pre-login catalog so Kimi can be selected and
/// configured before its first authenticated Threadknot turn.
pub fn builtin_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: DEFAULT_MODEL.into(),
            name: "K3".into(),
            image: None,
            supports_wide_context: None,
            fixed_context_window: Some(1_048_576),
            efforts: Some(vec!["low".into(), "high".into(), "max".into()]),
            default_effort: Some("high".into()),
        },
        ModelInfo {
            id: "kimi-code/k3-256k".into(),
            name: "K3 256K".into(),
            image: None,
            supports_wide_context: None,
            fixed_context_window: Some(262_144),
            efforts: Some(vec!["low".into(), "high".into(), "max".into()]),
            default_effort: Some("high".into()),
        },
        ModelInfo {
            id: "kimi-code/kimi-for-coding".into(),
            name: "K2.7 Code".into(),
            image: None,
            supports_wide_context: None,
            fixed_context_window: Some(262_144),
            efforts: None,
            default_effort: None,
        },
        ModelInfo {
            id: "kimi-code/kimi-for-coding-highspeed".into(),
            name: "K2.7 Code HighSpeed".into(),
            image: None,
            supports_wide_context: None,
            fixed_context_window: Some(262_144),
            efforts: None,
            default_effort: None,
        },
    ]
}

struct Rpc {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicI64,
}

enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Eof,
}

impl Rpc {
    async fn write_line(&self, value: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{value}\n").as_bytes())
            .await
            .context("write to Kimi ACP stdin")?;
        stdin.flush().await.context("flush Kimi ACP stdin")
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, Duration::from_secs(120))
            .await
    }

    async fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(error) = self
            .write_line(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .await
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        let result = tokio::time::timeout(timeout, rx)
            .await
            .with_context(|| format!("Kimi ACP request `{method}` timed out"))?
            .with_context(|| format!("Kimi ACP exited during `{method}`"))?;
        result.map_err(|e| anyhow::anyhow!("Kimi error on `{method}`: {e}"))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .await
    }

    async fn respond_error(&self, id: Value, code: i64, message: String) -> Result<()> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }))
        .await
    }
}

fn spawn_reader(child: &mut Child, rpc: Arc<Rpc>, msg_tx: mpsc::UnboundedSender<Incoming>) {
    let stdout = child.stdout.take().expect("Kimi ACP stdout");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                tracing::debug!(line, "ignored non-JSON Kimi ACP stdout");
                continue;
            };
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .map(String::from);
            let id = value.get("id").cloned();
            match (method, id) {
                (Some(method), Some(id)) => {
                    let _ = msg_tx.send(Incoming::Request {
                        id,
                        method,
                        params: value.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
                (Some(method), None) => {
                    let _ = msg_tx.send(Incoming::Notification {
                        method,
                        params: value.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
                (None, Some(id)) => {
                    if let Some(id) = id.as_i64() {
                        if let Some(tx) = rpc.pending.lock().unwrap().remove(&id) {
                            let outcome = if let Some(error) = value.get("error") {
                                let code = error.get("code").and_then(Value::as_i64);
                                let message = error
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown error");
                                Err(match code {
                                    Some(code) => format!("{message} (code {code})"),
                                    None => message.to_string(),
                                })
                            } else {
                                Ok(value.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = tx.send(outcome);
                        }
                    }
                }
                _ => {}
            }
        }
        let _ = msg_tx.send(Incoming::Eof);
    });
}

fn spawn_kimi(
    cwd: &str,
) -> Result<(Child, Arc<Rpc>, mpsc::UnboundedReceiver<Incoming>)> {
    let bin = super::resolve_bin("kimi")
        .ok_or_else(|| anyhow::anyhow!("Kimi Code CLI not found — install it, then run `kimi login`"))?;
    let mut cmd = Command::new(bin);
    cmd.arg("acp")
        .env("PATH", super::agent_path())
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    super::no_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .context("failed to spawn `kimi acp` — is Kimi Code installed?")?;

    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(line, "Kimi ACP stderr");
            }
        });
    }

    let rpc = Arc::new(Rpc {
        stdin: tokio::sync::Mutex::new(child.stdin.take().expect("Kimi ACP stdin")),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(1),
    });
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    spawn_reader(&mut child, Arc::clone(&rpc), msg_tx);
    Ok((child, rpc, msg_rx))
}

async fn handshake(rpc: &Rpc) -> Result<()> {
    rpc.request(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {
                "name": "threadknot",
                "title": "Threadknot",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )
    .await?;
    rpc.request("authenticate", json!({ "methodId": "login" }))
        .await
        .context("Kimi is not authenticated — run `kimi login` in a terminal")?;
    Ok(())
}

#[derive(Default)]
struct ToolState {
    name: String,
    ended: bool,
    subagent: Option<KimiSubagentState>,
}

struct KimiSubagentState {
    task_id: String,
    completed: bool,
    stop_watcher: Option<oneshot::Sender<()>>,
}

struct PendingQuestion {
    rpc_id: Value,
    question_id: String,
    option_ids: HashMap<String, String>,
    skip_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentKind {
    Assistant,
    Thinking,
}

#[derive(Default)]
struct BufferedContent {
    kind: Option<ContentKind>,
    text: String,
}

impl BufferedContent {
    /// Add one ACP chunk, returning the preceding segment when the provider
    /// switches from thought to narration (or vice versa).
    fn push(&mut self, kind: ContentKind, delta: &str) -> Option<(ContentKind, String)> {
        if delta.is_empty() {
            return None;
        }
        let completed = (self.kind.is_some() && self.kind != Some(kind))
            .then(|| self.take())
            .flatten();
        self.kind = Some(kind);
        self.text.push_str(delta);
        completed
    }

    fn take(&mut self) -> Option<(ContentKind, String)> {
        let kind = self.kind.take()?;
        let text = std::mem::take(&mut self.text);
        (!text.trim().is_empty()).then_some((kind, text))
    }

    fn clear(&mut self) {
        self.kind = None;
        self.text.clear();
    }
}

struct SessionState {
    session_id: Option<String>,
    seed: Option<String>,
    active: bool,
    current_model: String,
    current_effort: Option<String>,
    current_mode: String,
    settings: Option<ThreadSettings>,
    content: BufferedContent,
    tools: HashMap<String, ToolState>,
    next_subagent_index: u64,
    pending_approvals: HashMap<String, Value>,
    pending_questions: HashMap<String, PendingQuestion>,
    /// ACP exposes no mid-turn steer request. Messages submitted while a
    /// prompt is active wait here and become prompts at provider boundaries.
    queued_followups: VecDeque<String>,
}

impl SessionState {
    fn new(seed: Option<String>) -> Self {
        Self {
            session_id: None,
            seed,
            active: false,
            current_model: String::new(),
            current_effort: None,
            current_mode: String::new(),
            settings: None,
            content: BufferedContent::default(),
            tools: HashMap::new(),
            next_subagent_index: 0,
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            queued_followups: VecDeque::new(),
        }
    }
}

async fn launch_prompt(
    ctx: &DriverCtx,
    rpc: &Arc<Rpc>,
    st: &mut SessionState,
    turn_tx: &mpsc::UnboundedSender<Result<Value, String>>,
    text: &str,
    settings: &ThreadSettings,
    attachments: &[AttachmentRef],
) -> Result<()> {
    anyhow::ensure!(!st.active, "Kimi already has a turn in progress");
    let prompt = prepare_turn(ctx, rpc, st, text, settings, attachments).await?;
    st.active = true;
    let rpc = Arc::clone(rpc);
    let tx = turn_tx.clone();
    let session_id = st.session_id.clone().context("Kimi session is not open")?;
    tokio::spawn(async move {
        let result = rpc
            .request_timeout(
                "session/prompt",
                json!({ "sessionId": session_id, "prompt": prompt }),
                Duration::from_secs(12 * 60 * 60),
            )
            .await
            .map_err(|error| format!("{error:#}"));
        let _ = tx.send(result);
    });
    Ok(())
}

pub async fn run(ctx: DriverCtx, mut cmd_rx: mpsc::UnboundedReceiver<AgentCommand>) -> Result<()> {
    let (mut child, rpc, mut msg_rx) = spawn_kimi(&ctx.cwd)?;
    handshake(&rpc).await?;

    let mut st = SessionState::new(ctx.seed.clone());
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel::<Result<Value, String>>();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCommand::User { text, settings, attachments } => {
                        if let Err(error) = launch_prompt(
                            &ctx,
                            &rpc,
                            &mut st,
                            &turn_tx,
                            &text,
                            &settings,
                            &attachments,
                        )
                        .await
                        {
                            ctx.emit(AgentEvent::Error {
                                message: format!("{error:#}"),
                            });
                        }
                    }
                    AgentCommand::Settings { settings } => {
                        st.settings = Some(settings.clone());
                        if st.session_id.is_some() && !st.active {
                            if let Err(error) = apply_settings(&rpc, &mut st, &settings).await {
                                ctx.emit(AgentEvent::Status {
                                    text: format!("Kimi settings will retry on the next turn: {error:#}"),
                                });
                            }
                        }
                    }
                    AgentCommand::Retire => break,
                    AgentCommand::Steer { text } => {
                        if st.active {
                            st.queued_followups.push_back(text);
                            ctx.emit(AgentEvent::Status {
                                text: "Kimi queued your follow-up for the next turn boundary".into(),
                            });
                        } else if let Some(settings) = st.settings.clone() {
                            // Hub saw a busy thread, but the ACP prompt ended
                            // before this command reached the driver. Promote it
                            // immediately instead of losing the user's note.
                            ctx.hub.capture_artifact_baseline(&ctx.thread_id);
                            if let Err(error) = launch_prompt(
                                &ctx,
                                &rpc,
                                &mut st,
                                &turn_tx,
                                &text,
                                &settings,
                                &[],
                            )
                            .await
                            {
                                ctx.emit(AgentEvent::Error {
                                    message: format!("Kimi could not deliver the follow-up: {error:#}"),
                                });
                            }
                        }
                    }
                    AgentCommand::Interrupt => {
                        if st.active {
                            if let Some(session_id) = &st.session_id {
                                let _ = rpc
                                    .notify("session/cancel", json!({ "sessionId": session_id }))
                                    .await;
                            }
                        }
                    }
                    AgentCommand::Approval { approval_id, option_id } => {
                        if let Some(rpc_id) = st.pending_approvals.remove(&approval_id) {
                            let _ = rpc.respond(
                                rpc_id,
                                json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                            ).await;
                        }
                        ctx.emit(AgentEvent::ApprovalResolved { approval_id, option_id });
                    }
                    AgentCommand::Question { request_id, answers } => {
                        if let Some(pending) = st.pending_questions.remove(&request_id) {
                            let selected_label = answers
                                .get(&pending.question_id)
                                .and_then(|values| {
                                    values.iter().find(|value| !value.trim().is_empty())
                                })
                                .or_else(|| {
                                    answers
                                        .values()
                                        .flatten()
                                        .find(|value| !value.trim().is_empty())
                                });
                            let option_id = selected_label
                                .and_then(|label| pending.option_ids.get(label))
                                .cloned()
                                .or(pending.skip_id);
                            let result = match option_id {
                                Some(option_id) => json!({
                                    "outcome": { "outcome": "selected", "optionId": option_id },
                                }),
                                None => json!({ "outcome": { "outcome": "cancelled" } }),
                            };
                            let _ = rpc.respond(pending.rpc_id, result).await;
                        }
                        ctx.emit(AgentEvent::QuestionResolved {
                            request_id,
                            answers: Some(answers),
                        });
                    }
                }
            }
            msg = msg_rx.recv() => {
                match msg {
                    Some(Incoming::Notification { method, params }) => {
                        handle_notification(&ctx, &mut st, &method, params);
                    }
                    Some(Incoming::Request { id, method, params }) => {
                        handle_server_request(&ctx, &rpc, &mut st, id, &method, params).await;
                    }
                    Some(Incoming::Eof) | None => {
                        anyhow::bail!("`kimi acp` exited unexpectedly");
                    }
                }
            }
            result = turn_rx.recv(), if st.active => {
                if let Some(result) = result {
                    let has_followup = !st.queued_followups.is_empty();
                    finish_turn(&ctx, &mut st, result, has_followup);
                    if let Some(text) = st.queued_followups.pop_front() {
                        // A held completion boundary still needs its artifact
                        // diff, then the queued provider turn needs a fresh
                        // baseline. Its user message was persisted at enqueue
                        // time, so promotion must not emit a duplicate.
                        ctx.hub.detect_artifacts(&ctx.thread_id);
                        ctx.hub.capture_artifact_baseline(&ctx.thread_id);
                        if let Some(settings) = st.settings.clone() {
                            if let Err(error) = launch_prompt(
                                &ctx,
                                &rpc,
                                &mut st,
                                &turn_tx,
                                &text,
                                &settings,
                                &[],
                            )
                            .await
                            {
                                ctx.emit(AgentEvent::Error {
                                    message: format!("Kimi could not deliver the queued follow-up: {error:#}"),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = child.kill().await;
    Ok(())
}

/// ACP `mcpServers` for `session/new`: Threadknot's own browser server first, then
/// every enabled server from the user's Library (`library.rs`). ACP carries
/// headers and environment as name/value pair arrays, not objects.
fn mcp_servers(ctx: &DriverCtx) -> Value {
    let mut servers = vec![json!({
        "type": "http",
        "name": crate::library::RESERVED_MCP_NAME,
        "url": ctx.mcp_endpoint,
        "headers": [{
            "name": "Authorization",
            "value": format!("Bearer {}", ctx.mcp_token),
        }],
    })];
    servers.extend(
        ctx.hub
            .library
            .for_agent(ctx.agent)
            .iter()
            .map(crate::library::kimi_entry),
    );
    Value::Array(servers)
}

async fn ensure_session(ctx: &DriverCtx, rpc: &Rpc, st: &mut SessionState) -> Result<()> {
    if st.session_id.is_some() {
        return Ok(());
    }
    let params = json!({
        "cwd": ctx.cwd,
        "mcpServers": mcp_servers(ctx),
    });
    let (session_id, restored) = match &ctx.resume_session_id {
        Some(existing) => {
            let mut resume = params.clone();
            resume["sessionId"] = json!(existing);
            match rpc.request("session/resume", resume).await {
                Ok(_) => (existing.clone(), true),
                Err(error) => {
                    tracing::warn!(
                        "unable to resume Kimi session {existing}; restoring from Threadknot history: {error:#}"
                    );
                    st.seed = ctx.resume_fallback_seed.clone();
                    ctx.emit(AgentEvent::Status {
                        text: "Kimi session was unavailable; restored from Threadknot history".into(),
                    });
                    let result = rpc.request("session/new", params).await?;
                    let id = result
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .context("Kimi session/new returned no sessionId")?
                        .to_string();
                    (id, false)
                }
            }
        }
        None => {
            let result = rpc.request("session/new", params).await?;
            let id = result
                .get("sessionId")
                .and_then(Value::as_str)
                .context("Kimi session/new returned no sessionId")?
                .to_string();
            (id, false)
        }
    };
    if restored {
        tracing::debug!(session_id, "resumed Kimi ACP session");
    }
    st.session_id = Some(session_id.clone());
    st.next_subagent_index = next_kimi_subagent_index(&session_id);
    ctx.emit(AgentEvent::SessionStarted {
        provider_session_id: session_id,
        model: st.current_model.clone(),
        agent: Some(Agent::Kimi),
    });
    Ok(())
}

fn kimi_mode(settings: &ThreadSettings) -> &'static str {
    if settings.mode == Mode::Plan {
        "plan"
    } else if settings.access == Access::Full {
        "yolo"
    } else {
        "default"
    }
}

async fn set_config(rpc: &Rpc, session_id: &str, config_id: &str, value: &str) -> Result<()> {
    rpc.request(
        "session/set_config_option",
        json!({
            "sessionId": session_id,
            "configId": config_id,
            "value": value,
        }),
    )
    .await?;
    Ok(())
}

async fn apply_settings(
    rpc: &Rpc,
    st: &mut SessionState,
    settings: &ThreadSettings,
) -> Result<()> {
    let session_id = st.session_id.clone().context("Kimi session is not open")?;
    if st.current_model != settings.model {
        set_config(rpc, &session_id, "model", &settings.model).await?;
        st.current_model = settings.model.clone();
        // A model change reselects that model's own default thinking effort.
        st.current_effort = None;
    }
    if settings.effort != st.current_effort {
        if let Some(effort) = &settings.effort {
            set_config(rpc, &session_id, "thinking", effort).await?;
        }
        st.current_effort = settings.effort.clone();
    }
    let mode = kimi_mode(settings);
    if st.current_mode != mode {
        set_config(rpc, &session_id, "mode", mode).await?;
        st.current_mode = mode.into();
    }
    st.settings = Some(settings.clone());
    Ok(())
}

async fn prepare_turn(
    ctx: &DriverCtx,
    rpc: &Rpc,
    st: &mut SessionState,
    text: &str,
    settings: &ThreadSettings,
    attachments: &[AttachmentRef],
) -> Result<Vec<Value>> {
    // Set this before session/new so SessionStarted always records the selected
    // model, even though the ACP config call follows session creation.
    st.current_model = settings.model.clone();
    let opened_session = st.session_id.is_none();
    ensure_session(ctx, rpc, st).await?;
    // Force the first post-create model call: session/new used the CLI default.
    if opened_session {
        st.current_model.clear();
    }
    apply_settings(rpc, st, settings).await?;

    let text = super::transcript::seeded_message(st.seed.take().as_deref(), text);
    let docs = super::materialize_docs(&ctx.cwd, attachments);
    let text = format!("{text}{}", super::attachment_footer(&docs));
    let mut prompt = Vec::new();
    if !text.trim().is_empty() {
        prompt.push(json!({ "type": "text", "text": text }));
    }
    for attachment in attachments {
        if let Some(image) = image_content_block(attachment) {
            prompt.push(image);
        }
    }
    anyhow::ensure!(!prompt.is_empty(), "Kimi prompt has no valid text or image content");

    st.content.clear();
    st.tools.clear();
    ctx.emit(AgentEvent::TurnStarted {
        agent: Some(Agent::Kimi),
        model: Some(settings.model.clone()),
    });
    Ok(prompt)
}

fn image_content_block(attachment: &AttachmentRef) -> Option<Value> {
    if !attachment.mime_type.starts_with("image/") {
        return None;
    }
    let bytes = std::fs::read(&attachment.path).ok()?;
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(json!({
        "type": "image",
        "data": data,
        "mimeType": attachment.mime_type,
    }))
}

fn finish_turn(
    ctx: &DriverCtx,
    st: &mut SessionState,
    result: Result<Value, String>,
    hold_completed_boundary: bool,
) {
    st.active = false;
    flush_content(ctx, st);
    finish_unresolved_subagents(ctx, st);

    match result {
        Err(message) => ctx.emit(AgentEvent::Error { message }),
        Ok(value) => {
            let usage = usage_from_prompt_response(&value);
            if let Some(usage) = usage.clone() {
                ctx.emit(AgentEvent::ContextUsage { usage });
            }
            match value
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("end_turn")
            {
                "cancelled" => ctx.emit(AgentEvent::TurnAborted),
                "refusal" => ctx.emit(AgentEvent::Error {
                    message: "Kimi refused the request".into(),
                }),
                "max_tokens" => {
                    ctx.emit(AgentEvent::Status {
                        text: "Kimi reached its output-token limit".into(),
                    });
                    if !hold_completed_boundary {
                        ctx.emit(AgentEvent::TurnCompleted { usage });
                    }
                }
                "max_turn_requests" => {
                    ctx.emit(AgentEvent::Status {
                        text: "Kimi reached its tool-turn limit".into(),
                    });
                    if !hold_completed_boundary {
                        ctx.emit(AgentEvent::TurnCompleted { usage });
                    }
                }
                _ if !hold_completed_boundary => ctx.emit(AgentEvent::TurnCompleted { usage }),
                _ => {}
            }
        }
    }
    st.pending_approvals.clear();
    st.pending_questions.clear();
}

fn usage_from_prompt_response(value: &Value) -> Option<Usage> {
    let raw = value.get("usage")?;
    let input = raw.get("inputTokens").and_then(Value::as_u64);
    let output = raw.get("outputTokens").and_then(Value::as_u64);
    let used = raw
        .get("totalTokens")
        .and_then(Value::as_u64)
        .or_else(|| match (input, output) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        });
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        used_tokens: used,
        max_tokens: None,
        context_pct: None,
        cost_usd: None,
    })
}

fn handle_notification(ctx: &DriverCtx, st: &mut SessionState, method: &str, params: Value) {
    if method != "session/update" {
        return;
    }
    if let (Some(expected), Some(actual)) = (
        st.session_id.as_deref(),
        params.get("sessionId").and_then(Value::as_str),
    ) {
        if expected != actual {
            return;
        }
    }
    let Some(update) = params.get("update") else {
        return;
    };
    match update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "agent_message_chunk" => {
            if let Some(delta) = content_block_text(update.get("content")) {
                push_content(ctx, st, ContentKind::Assistant, delta);
            }
        }
        "agent_thought_chunk" => {
            if let Some(delta) = content_block_text(update.get("content")) {
                push_content(ctx, st, ContentKind::Thinking, delta);
            }
        }
        "tool_call" | "tool_call_update" => handle_tool_update(ctx, st, update),
        "plan" => {
            let entries = update
                .get("entries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry.get("content").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if !entries.is_empty() {
                ctx.emit(AgentEvent::Status {
                    text: format!("Kimi plan updated: {entries}"),
                });
            }
        }
        "usage_update" => {
            if let Some(usage) = usage_from_update(update) {
                ctx.emit(AgentEvent::ContextUsage { usage });
            }
        }
        _ => {}
    }
}

fn emit_completed_content(ctx: &DriverCtx, completed: Option<(ContentKind, String)>) {
    match completed {
        Some((ContentKind::Assistant, text)) => ctx.emit(AgentEvent::AssistantMessage { text }),
        Some((ContentKind::Thinking, text)) => ctx.emit(AgentEvent::Thinking { text }),
        None => {}
    }
}

fn push_content(ctx: &DriverCtx, st: &mut SessionState, kind: ContentKind, delta: String) {
    if delta.is_empty() {
        return;
    }
    let completed = st.content.push(kind, &delta);
    emit_completed_content(ctx, completed);
    ctx.emit(match kind {
        ContentKind::Assistant => AgentEvent::AssistantDelta { text: delta },
        ContentKind::Thinking => AgentEvent::ThinkingDelta { text: delta },
    });
}

fn flush_content(ctx: &DriverCtx, st: &mut SessionState) {
    let completed = st.content.take();
    emit_completed_content(ctx, completed);
}

fn content_block_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    None
}

fn usage_from_update(update: &Value) -> Option<Usage> {
    let used = update
        .get("used")
        .and_then(Value::as_u64)
        .or_else(|| update.get("totalTokens").and_then(Value::as_u64));
    let max = update
        .get("size")
        .and_then(Value::as_u64)
        .or_else(|| update.get("maxTokens").and_then(Value::as_u64));
    if used.is_none() && max.is_none() {
        return None;
    }
    Some(Usage {
        input_tokens: None,
        output_tokens: None,
        used_tokens: used,
        max_tokens: max,
        context_pct: match (used, max) {
            (Some(used), Some(max)) if max > 0 => Some(used as f64 / max as f64 * 100.0),
            _ => None,
        },
        cost_usd: None,
    })
}

fn tool_name(update: &Value) -> String {
    update
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| *kind != "other")
        .or_else(|| update.get("title").and_then(Value::as_str))
        .unwrap_or("tool")
        .to_string()
}

fn tool_detail(update: &Value) -> String {
    if let Some(input) = update.get("rawInput") {
        if !input.is_null() {
            return match input {
                Value::String(text) => text.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
        }
    }
    content_array_text(update.get("content"))
}

fn content_array_text(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(Value::as_str) == Some("content") {
                        content_block_text(item.get("content"))
                    } else {
                        content_block_text(Some(item))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn tool_output(update: &Value) -> Option<String> {
    let content = content_array_text(update.get("content"));
    if !content.is_empty() {
        return Some(truncate_output(&content));
    }
    update.get("rawOutput").and_then(|output| {
        if output.is_null() {
            None
        } else {
            Some(truncate_output(match output {
                Value::String(text) => text,
                other => return Some(serde_json::to_string_pretty(other).unwrap_or_default()),
            }))
        }
    })
}

fn kimi_code_home() -> Option<PathBuf> {
    std::env::var_os("KIMI_CODE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".kimi-code")))
}

fn kimi_session_dir(session_id: &str) -> Option<PathBuf> {
    let sessions = kimi_code_home()?.join("sessions");
    let expected = kimi_session_dir_name(session_id);
    std::fs::read_dir(sessions)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path().join(&expected))
        .find(|path| path.is_dir())
}

fn kimi_session_dir_name(session_id: &str) -> String {
    if session_id.starts_with("session_") {
        session_id.to_string()
    } else {
        format!("session_{session_id}")
    }
}

fn next_kimi_subagent_index(session_id: &str) -> u64 {
    let Some(agents) = kimi_session_dir(session_id).map(|dir| dir.join("agents")) else {
        return 0;
    };
    std::fs::read_dir(agents)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.strip_prefix("agent-")?.parse().ok())
        .max()
        .map(|index: u64| index + 1)
        .unwrap_or(0)
}

fn clipped(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn subagent_wire_activity(value: &Value) -> Option<(&'static str, String)> {
    let event = value.get("event")?;
    match event.get("type").and_then(Value::as_str)? {
        "content.part" => {
            let part = event.get("part")?;
            match part.get("type").and_then(Value::as_str)? {
                "think" => part
                    .get("think")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| ("thinking", clipped(text, 900))),
                "text" => part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| ("text", clipped(text, 900))),
                _ => None,
            }
        }
        "tool.call" => {
            let name = event.get("name").and_then(Value::as_str).unwrap_or("tool");
            let detail = event
                .get("description")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(String::from)
                .or_else(|| {
                    event.get("args").filter(|args| !args.is_null()).map(|args| {
                        serde_json::to_string(args).unwrap_or_default()
                    })
                })
                .unwrap_or_default();
            Some((
                "tool",
                clipped(
                    &if detail.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name}: {detail}")
                    },
                    900,
                ),
            ))
        }
        _ => None,
    }
}

async fn watch_kimi_subagent(
    hub: std::sync::Arc<super::Hub>,
    thread_id: String,
    participant_id: String,
    session_id: String,
    agent_index: u64,
    task_id: String,
    mut stop: oneshot::Receiver<()>,
) {
    let mut offset = 0_u64;
    let mut partial = String::new();
    loop {
        tokio::select! {
            _ = &mut stop => return,
            _ = tokio::time::sleep(Duration::from_millis(650)) => {}
        }
        let Some(path) = kimi_session_dir(&session_id).map(|dir| {
            dir.join("agents")
                .join(format!("agent-{agent_index}"))
                .join("wire.jsonl")
        }) else {
            continue;
        };
        let Ok(mut file) = tokio::fs::File::open(path).await else {
            continue;
        };
        if file.seek(SeekFrom::Start(offset)).await.is_err() {
            continue;
        }
        let mut chunk = String::new();
        let Ok(read) = file.read_to_string(&mut chunk).await else {
            continue;
        };
        if read == 0 {
            continue;
        }
        offset += read as u64;
        partial.push_str(&chunk);
        while let Some(newline) = partial.find('\n') {
            let line = partial[..newline].to_string();
            partial.drain(..=newline);
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some((activity, text)) = subagent_wire_activity(&value) else {
                continue;
            };
            hub.emit_as(
                &thread_id,
                Some(&participant_id),
                AgentEvent::SubagentProgress {
                    task_id: task_id.clone(),
                    activity: activity.into(),
                    text,
                },
            );
        }
    }
}

fn maybe_start_kimi_subagent(
    ctx: &DriverCtx,
    st: &mut SessionState,
    call_id: &str,
    update: &Value,
) {
    let Some(input) = update.get("rawInput").and_then(Value::as_object) else {
        return;
    };
    let Some(tool) = st.tools.get_mut(call_id) else {
        return;
    };
    let title = update.get("title").and_then(Value::as_str).unwrap_or("");
    if tool.subagent.is_some()
        || !(tool.name == "Agent" || title.starts_with("Launching ") && title.contains(" agent"))
    {
        return;
    }
    let task_id = format!("kimi:{call_id}");
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("Kimi child agent")
        .to_string();
    let subagent_type = input
        .get("subagent_type")
        .and_then(Value::as_str)
        .unwrap_or("agent")
        .to_string();
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(String::from);
    let background = input
        .get("run_in_background")
        .or_else(|| input.get("background"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agent_index = st.next_subagent_index;
    st.next_subagent_index += 1;
    let (stop_watcher, stop_rx) = oneshot::channel();
    tool.subagent = Some(KimiSubagentState {
        task_id: task_id.clone(),
        completed: false,
        stop_watcher: Some(stop_watcher),
    });
    ctx.emit(AgentEvent::SubagentStarted {
        task_id: task_id.clone(),
        tool_use_id: call_id.to_string(),
        description,
        subagent_type,
        background,
        prompt,
        dispatch: None,
    });
    ctx.emit(AgentEvent::SubagentProgress {
        task_id: task_id.clone(),
        activity: "status".into(),
        text: "Child agent launched; waiting for its first recorded activity.".into(),
    });
    if let Some(session_id) = st.session_id.clone() {
        tokio::spawn(watch_kimi_subagent(
            std::sync::Arc::clone(&ctx.hub),
            ctx.thread_id.clone(),
            ctx.participant_id.clone(),
            session_id,
            agent_index,
            task_id,
            stop_rx,
        ));
    }
}

fn subagent_result(output: Option<&str>, failed: bool) -> (String, Option<String>) {
    let output = output.unwrap_or("");
    let status = output
        .lines()
        .find_map(|line| line.strip_prefix("status:"))
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .unwrap_or(if failed { "failed" } else { "completed" })
        .to_string();
    let summary = output
        .split_once("[summary]")
        .map(|(_, summary)| summary.trim())
        .filter(|summary| !summary.is_empty())
        .map(|summary| clipped(summary, 8_000));
    (status, summary)
}

fn finish_kimi_subagent(
    ctx: &DriverCtx,
    st: &mut SessionState,
    call_id: &str,
    update: &Value,
    failed: bool,
) {
    let Some(subagent) = st
        .tools
        .get_mut(call_id)
        .and_then(|tool| tool.subagent.as_mut())
    else {
        return;
    };
    if subagent.completed {
        return;
    }
    subagent.completed = true;
    let _ = subagent.stop_watcher.take().map(|stop| stop.send(()));
    let raw_output = update
        .get("rawOutput")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            update
                .get("content")
                .map(|content| content_array_text(Some(content)))
                .filter(|content| !content.is_empty())
        });
    let (status, summary) = subagent_result(raw_output.as_deref(), failed);
    ctx.emit(AgentEvent::SubagentCompleted {
        task_id: subagent.task_id.clone(),
        status,
        summary,
    });
}

fn finish_unresolved_subagents(ctx: &DriverCtx, st: &mut SessionState) {
    let mut unresolved = Vec::new();
    for tool in st.tools.values_mut() {
        let Some(subagent) = tool.subagent.as_mut().filter(|subagent| !subagent.completed) else {
            continue;
        };
        subagent.completed = true;
        let _ = subagent.stop_watcher.take().map(|stop| stop.send(()));
        unresolved.push(subagent.task_id.clone());
    }
    for task_id in unresolved {
        ctx.emit(AgentEvent::SubagentCompleted {
            task_id,
            status: "failed".into(),
            summary: Some("The turn ended before Kimi returned this child agent's result.".into()),
        });
    }
}

fn handle_tool_update(ctx: &DriverCtx, st: &mut SessionState, update: &Value) {
    let Some(call_id) = update.get("toolCallId").and_then(Value::as_str) else {
        return;
    };
    let call_id = call_id.to_string();
    if !st.tools.contains_key(&call_id) {
        // ACP has no separate "message completed" notification. A new tool is
        // the hard chronological boundary for the narration that introduced
        // it, so persist that segment before persisting the tool card.
        flush_content(ctx, st);
        let name = tool_name(update);
        ctx.emit(AgentEvent::ToolStart {
            call_id: call_id.clone(),
            name: name.clone(),
            detail: tool_detail(update),
        });
        st.tools.insert(
            call_id.clone(),
            ToolState {
                name,
                ended: false,
                subagent: None,
            },
        );
    }
    emit_file_diffs(ctx, update);
    maybe_start_kimi_subagent(ctx, st, &call_id, update);

    let status = update.get("status").and_then(Value::as_str);
    if matches!(status, Some("completed" | "failed")) {
        finish_kimi_subagent(ctx, st, &call_id, update, status == Some("failed"));
        let tool = st.tools.get_mut(&call_id).expect("inserted Kimi tool");
        if !tool.ended {
            tool.ended = true;
            ctx.emit(AgentEvent::ToolEnd {
                call_id,
                name: tool.name.clone(),
                output: tool_output(update),
                is_error: status == Some("failed"),
                truncated: false,
            });
        }
    }
}

fn emit_file_diffs(ctx: &DriverCtx, update: &Value) {
    let Some(items) = update.get("content").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("diff") {
            continue;
        }
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("file");
        let old = item.get("oldText").and_then(Value::as_str).unwrap_or("");
        let new = item.get("newText").and_then(Value::as_str).unwrap_or("");
        ctx.emit(AgentEvent::FileDiff {
            path: path.to_string(),
            unified: simple_unified_diff(path, old, new),
        });
    }
}

fn simple_unified_diff(path: &str, old: &str, new: &str) -> String {
    let mut out = format!("--- a/{path}\n+++ b/{path}\n@@\n");
    for line in old.lines() {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in new.lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn truncate_output(text: &str) -> String {
    const MAX: usize = 20_000;
    if text.len() <= MAX {
        text.to_string()
    } else {
        let mut end = MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n… [truncated {} bytes]",
            &text[..end],
            text.len() - end
        )
    }
}

fn permission_options(params: &Value) -> Vec<(String, String, String)> {
    params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = option.get("optionId").and_then(Value::as_str)?;
                    let label = option
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(id);
                    let kind = option
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("reject_once");
                    Some((id.to_string(), label.to_string(), kind.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn permission_detail(tool_call: &Value) -> String {
    let content = content_array_text(tool_call.get("content"));
    if !content.is_empty() {
        return content;
    }
    tool_call
        .get("rawInput")
        .filter(|value| !value.is_null())
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        })
        .unwrap_or_default()
}

fn auto_approval(
    settings: Option<&ThreadSettings>,
    tool_call: &Value,
    options: &[(String, String, String)],
) -> Option<String> {
    let settings = settings?;
    let is_plan_review = options.iter().any(|(id, _, _)| id.starts_with("plan_"));
    if is_plan_review {
        return None;
    }
    let kind = tool_call
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("other");
    let allowed = settings.access == Access::Full
        || (settings.access == Access::Edits && matches!(kind, "edit" | "delete" | "move"));
    if !allowed {
        return None;
    }
    options
        .iter()
        .find(|(_, _, kind)| kind == "allow_always" && settings.access == Access::Full)
        .or_else(|| options.iter().find(|(_, _, kind)| kind == "allow_once"))
        .map(|(id, _, _)| id.clone())
}

async fn handle_server_request(
    ctx: &DriverCtx,
    rpc: &Rpc,
    st: &mut SessionState,
    id: Value,
    method: &str,
    params: Value,
) {
    if method != "session/request_permission" {
        let _ = rpc
            .respond_error(id, -32601, format!("method not found: {method}"))
            .await;
        return;
    }
    let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    let options = permission_options(&params);
    let title = tool_call
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Kimi tool");

    if title == "AskUserQuestion" {
        handle_question(ctx, rpc, st, id, &tool_call, &options).await;
        return;
    }
    if let Some(option_id) = auto_approval(st.settings.as_ref(), &tool_call, &options) {
        let _ = rpc
            .respond(
                id,
                json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
            )
            .await;
        return;
    }

    let approval_id = new_id();
    st.pending_approvals.insert(approval_id.clone(), id);
    let is_plan = options.iter().any(|(id, _, _)| id.starts_with("plan_"));
    let kind = tool_call
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("other");
    let approval_options = options
        .into_iter()
        .map(|(id, label, kind)| ApprovalOption {
            id,
            label,
            tone: match kind.as_str() {
                "allow_always" => "allowAlways",
                "allow_once" => "allow",
                _ => "deny",
            }
            .into(),
        })
        .collect();
    ctx.emit(AgentEvent::ApprovalRequest {
        approval_id,
        approval_kind: if is_plan {
            "plan"
        } else if kind == "execute" {
            "exec"
        } else if matches!(kind, "edit" | "delete" | "move") {
            "patch"
        } else {
            "tool"
        }
        .into(),
        title: title.to_string(),
        detail: permission_detail(&tool_call),
        options: approval_options,
    });
}

async fn handle_question(
    ctx: &DriverCtx,
    rpc: &Rpc,
    st: &mut SessionState,
    id: Value,
    tool_call: &Value,
    options: &[(String, String, String)],
) {
    let mut option_ids = HashMap::new();
    let mut question_options = Vec::new();
    let mut skip_id = None;
    for (option_id, label, _) in options {
        if option_id.ends_with("_skip") {
            skip_id = Some(option_id.clone());
        } else {
            option_ids.insert(label.clone(), option_id.clone());
            question_options.push(QuestionOption {
                label: label.clone(),
                description: String::new(),
            });
        }
    }
    if question_options.is_empty() {
        let result = skip_id
            .map(|option_id| {
                json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
            })
            .unwrap_or_else(|| json!({ "outcome": { "outcome": "cancelled" } }));
        let _ = rpc.respond(id, result).await;
        return;
    }
    let request_id = new_id();
    let question_id = "q0".to_string();
    st.pending_questions.insert(
        request_id.clone(),
        PendingQuestion {
            rpc_id: id,
            question_id: question_id.clone(),
            option_ids,
            skip_id,
        },
    );
    ctx.emit(AgentEvent::QuestionRequest {
        request_id,
        questions: vec![Question {
            id: question_id,
            header: "Kimi".into(),
            question: permission_detail(tool_call),
            options: question_options,
            multi_select: false,
            allow_other: false,
            is_secret: false,
        }],
    });
}

/// One-shot availability probe used by `agents.info`. It initializes ACP and
/// checks the CLI's own OAuth status without creating a durable Kimi session.
pub async fn probe(cwd: &str) -> Result<bool> {
    let (mut child, rpc, mut msg_rx) = spawn_kimi(cwd)?;
    tokio::spawn(async move { while msg_rx.recv().await.is_some() {} });
    let result = handshake(&rpc).await.map(|_| true);
    let _ = child.kill().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(access: Access, mode: Mode) -> ThreadSettings {
        ThreadSettings {
            model: DEFAULT_MODEL.into(),
            effort: Some("high".into()),
            wide_context: false,
            claude_chrome: false,
            access,
            mode,
            browser_profile_id: None,
            hermes_agent_id: None,
        }
    }

    #[test]
    fn maps_threadknot_access_and_mode_to_kimi_modes() {
        assert_eq!(kimi_mode(&settings(Access::Read, Mode::Plan)), "plan");
        assert_eq!(kimi_mode(&settings(Access::Edits, Mode::Build)), "default");
        assert_eq!(kimi_mode(&settings(Access::Full, Mode::Build)), "yolo");
    }

    #[test]
    fn k3_models_offer_the_authenticated_effort_catalog() {
        let models = builtin_models();
        assert_eq!(
            models
                .iter()
                .find(|model| model.id == DEFAULT_MODEL)
                .and_then(|model| model.fixed_context_window),
            Some(1_048_576)
        );
        for model in models
            .into_iter()
            .filter(|model| model.id == DEFAULT_MODEL || model.id == "kimi-code/k3-256k")
        {
            assert_eq!(
                model.efforts,
                Some(vec!["low".into(), "high".into(), "max".into()])
            );
            assert_eq!(model.default_effort.as_deref(), Some("high"));
        }
    }

    #[test]
    fn edits_access_only_auto_approves_file_mutations() {
        let options = vec![
            ("approve_once".into(), "Approve once".into(), "allow_once".into()),
            ("reject".into(), "Reject".into(), "reject_once".into()),
        ];
        assert_eq!(
            auto_approval(
                Some(&settings(Access::Edits, Mode::Build)),
                &json!({ "kind": "edit" }),
                &options,
            ),
            Some("approve_once".into())
        );
        assert_eq!(
            auto_approval(
                Some(&settings(Access::Edits, Mode::Build)),
                &json!({ "kind": "execute" }),
                &options,
            ),
            None
        );
    }

    #[test]
    fn long_unicode_tool_output_truncates_on_a_character_boundary() {
        let output = format!("{}走", "a".repeat(19_999));
        let truncated = truncate_output(&output);
        assert!(truncated.starts_with(&"a".repeat(19_999)));
        assert!(truncated.contains("[truncated 3 bytes]"));
    }

    #[test]
    fn buffered_content_preserves_segment_boundaries_and_ignores_empty_chunks() {
        let mut content = BufferedContent::default();
        assert!(content.push(ContentKind::Thinking, "considering").is_none());
        assert!(content.push(ContentKind::Thinking, " options").is_none());
        assert_eq!(
            content.push(ContentKind::Assistant, "I will inspect it."),
            Some((ContentKind::Thinking, "considering options".into()))
        );
        assert!(content.push(ContentKind::Thinking, "").is_none());
        assert_eq!(
            content.take(),
            Some((ContentKind::Assistant, "I will inspect it.".into()))
        );
        assert!(content.take().is_none());
    }

    #[test]
    fn parses_kimi_child_wire_activity() {
        let tool = json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.call",
                "name": "Read",
                "description": "Read src/main.rs"
            }
        });
        assert_eq!(
            subagent_wire_activity(&tool),
            Some(("tool", "Read: Read src/main.rs".into()))
        );
        let text = json!({
            "event": {
                "type": "content.part",
                "part": { "type": "text", "text": "Found the driver." }
            }
        });
        assert_eq!(
            subagent_wire_activity(&text),
            Some(("text", "Found the driver.".into()))
        );
    }

    #[test]
    fn parses_kimi_child_result_summary_and_failure() {
        let output = "agent_id: agent-3\nactual_subagent_type: coder\nstatus: completed\n\n[summary]\nImplemented the requested change.";
        assert_eq!(
            subagent_result(Some(output), false),
            ("completed".into(), Some("Implemented the requested change.".into()))
        );
        assert_eq!(subagent_result(None, true), ("failed".into(), None));
    }

    #[test]
    fn normalizes_kimi_session_directory_names_once() {
        assert_eq!(kimi_session_dir_name("abc"), "session_abc");
        assert_eq!(kimi_session_dir_name("session_abc"), "session_abc");
    }
}

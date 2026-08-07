//! Codex driver: speaks the `codex app-server` JSON-RPC-over-stdio protocol.
//!
//! Wire format (matches codex, ported from t3code's integration):
//! newline-delimited JSON; requests are `{id, method, params?}` with integer ids
//! starting at 1 and NO `jsonrpc` field; server->client requests (approvals) are
//! answered with `{id, result}`.

use super::{AgentCommand, AttachmentRef, DriverCtx};
use crate::protocol::*;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

const APPROVAL_OPTIONS: &[(&str, &str, &str)] = &[
    ("accept", "Approve once", "allow"),
    (
        "acceptForSession",
        "Always allow this session",
        "allowAlways",
    ),
    ("decline", "Decline", "deny"),
    ("cancel", "Cancel turn", "deny"),
];

struct Rpc {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicI64,
}

enum Incoming {
    /// Server -> client request (approvals): must be answered by id.
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
            .context("write to codex stdin")?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut frame = json!({ "id": id, "method": method });
        if !params.is_null() {
            frame["params"] = params;
        }
        self.write_line(&frame).await?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), rx)
            .await
            .context("codex request timed out")?
            .context("codex exited before responding")?;
        result.map_err(|e| anyhow::anyhow!("codex error on {method}: {e}"))
    }

    async fn notify(&self, method: &str) -> Result<()> {
        self.write_line(&json!({ "method": method })).await
    }

    async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.write_line(&json!({ "id": id, "result": result }))
            .await
    }
}

fn spawn_reader(child: &mut Child, rpc: Arc<Rpc>, msg_tx: mpsc::UnboundedSender<Incoming>) {
    let stdout = child.stdout.take().expect("codex stdout");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let method = v.get("method").and_then(|m| m.as_str()).map(String::from);
            let id = v.get("id").cloned();
            match (method, id) {
                (Some(method), Some(id)) => {
                    let _ = msg_tx.send(Incoming::Request {
                        id,
                        method,
                        params: v.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
                (Some(method), None) => {
                    let _ = msg_tx.send(Incoming::Notification {
                        method,
                        params: v.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
                (None, Some(id)) => {
                    if let Some(id) = id.as_i64() {
                        if let Some(tx) = rpc.pending.lock().unwrap().remove(&id) {
                            let outcome = if let Some(err) = v.get("error") {
                                Err(err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown error")
                                    .to_string())
                            } else {
                                Ok(v.get("result").cloned().unwrap_or(Value::Null))
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

/// `mcp`: `Some((endpoint, token))` wires the agent-driven browser MCP server
/// via codex's `-c mcp_servers.*` config (see mcp.rs); `None` for one-off probes.
/// `library` adds the user's installed MCP servers the same way — a probe passes
/// an empty slice so it stays cheap and cannot be broken by a bad install.
fn spawn_codex(
    cwd: &str,
    mcp: Option<(&str, &str)>,
    library: &[crate::library::McpServer],
) -> Result<(Child, Arc<Rpc>, mpsc::UnboundedReceiver<Incoming>)> {
    let bin = super::resolve_bin("codex")
        .ok_or_else(|| anyhow::anyhow!("codex CLI not found on PATH"))?;
    let mut cmd = Command::new(bin);
    cmd.env("PATH", super::agent_path());
    if let Some((endpoint, token)) = mcp {
        // Global `-c` overrides must precede the `app-server` subcommand. The
        // token is passed by env var (codex reads it via bearer_token_env_var).
        cmd.arg("-c")
            .arg(format!("mcp_servers.threadknot-browser.url={endpoint}"))
            .arg("-c")
            .arg("mcp_servers.threadknot-browser.bearer_token_env_var=\"THREADKNOT_MCP_TOKEN\"")
            .env("THREADKNOT_MCP_TOKEN", token);
        for server in library {
            let (overrides, env) = crate::library::codex_overrides(server);
            for value in overrides {
                cmd.arg("-c").arg(value);
            }
            for (key, value) in env {
                cmd.env(key, value);
            }
            // Codex's HTTP transport takes a bearer token, not arbitrary
            // headers. Say so once at spawn rather than letting the server
            // 401 and look broken.
            let dropped = crate::library::codex_unsupported_headers(server);
            if !dropped.is_empty() {
                tracing::warn!(
                    server = server.name,
                    headers = dropped.join(", "),
                    "codex cannot send these MCP headers; only Authorization: Bearer is supported"
                );
            }
        }
    }
    cmd.arg("app-server")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    super::no_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .context("failed to spawn `codex app-server` — is codex installed and on PATH?")?;
    let rpc = Arc::new(Rpc {
        stdin: tokio::sync::Mutex::new(child.stdin.take().expect("codex stdin")),
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
            "clientInfo": { "name": "threadknot", "title": "Threadknot", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true }
        }),
    )
    .await?;
    rpc.notify("initialized").await
}

fn policies(settings: &ThreadSettings) -> (&'static str, &'static str, Value) {
    // (approvalPolicy, sandbox, turn-level sandboxPolicy)
    match settings.access {
        Access::Read => ("untrusted", "read-only", json!({ "type": "readOnly" })),
        Access::Edits => (
            "on-request",
            "workspace-write",
            json!({ "type": "workspaceWrite" }),
        ),
        Access::Full => (
            "never",
            "danger-full-access",
            json!({ "type": "dangerFullAccess" }),
        ),
    }
}

struct SessionState {
    provider_thread_id: Option<String>,
    active_turn_id: Option<String>,
    /// Handoff seed for the FIRST turn of this driver (agent switch / catch-up).
    seed: Option<String>,
    /// Model of the in-flight turn, for TurnStarted provenance.
    current_model: String,
    /// Settings used by the current/last turn. A steer that races the provider
    /// boundary degrades to a fresh follow-up with these settings.
    current_settings: Option<ThreadSettings>,
    /// Steers rejected because the provider crossed or cannot accept the active
    /// boundary. Deliver them as ordinary follow-ups after that turn ends.
    pending_followups: VecDeque<String>,
    last_usage: Option<Usage>,
    /// our approvalId -> jsonrpc id to answer
    pending_approvals: HashMap<String, Value>,
    /// our requestId -> jsonrpc id for a pending requestUserInput
    pending_questions: HashMap<String, Value>,
}

impl SessionState {
    fn new(seed: Option<String>) -> Self {
        Self {
            // Each spawned app-server starts with no active threads. A saved
            // provider id must be activated by thread/resume first.
            provider_thread_id: None,
            active_turn_id: None,
            seed,
            current_model: String::new(),
            current_settings: None,
            pending_followups: VecDeque::new(),
            last_usage: None,
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
        }
    }
}

pub async fn run(ctx: DriverCtx, mut cmd_rx: mpsc::UnboundedReceiver<AgentCommand>) -> Result<()> {
    let (mut child, rpc, mut msg_rx) = spawn_codex(
        &ctx.cwd,
        Some((ctx.mcp_endpoint.as_str(), ctx.mcp_token.as_str())),
        &ctx.hub.library.for_agent(ctx.agent),
    )?;
    handshake(&rpc).await?;

    let mut st = SessionState::new(ctx.seed.clone());

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCommand::User { text, settings, attachments } => {
                        if let Err(e) = start_turn(&ctx, &rpc, &mut st, &text, &settings, &attachments).await {
                            ctx.emit(AgentEvent::Error { message: format!("{e:#}") });
                        }
                    }
                    // Codex policies are turn-level. The stored settings are
                    // supplied with the next User command, so no live RPC is
                    // needed here.
                    AgentCommand::Settings { settings } => {
                        st.current_settings = Some(settings);
                    }
                    AgentCommand::Retire => break,
                    AgentCommand::Steer { text } => {
                        if let (Some(thread_id), Some(turn_id)) =
                            (&st.provider_thread_id, &st.active_turn_id)
                        {
                            let result = rpc
                                .request(
                                    "turn/steer",
                                    json!({
                                        "threadId": thread_id,
                                        "expectedTurnId": turn_id,
                                        "input": [{ "type": "text", "text": text }],
                                    }),
                                )
                                .await;
                            if let Err(error) = result {
                                // The active-turn precondition can lose a race
                                // with turn/completed, and a few special turns
                                // are intentionally non-steerable. Preserve the
                                // note as a follow-up instead of dropping it.
                                tracing::debug!("Codex steer raced the turn boundary: {error:#}");
                                st.pending_followups.push_back(text);
                                ctx.emit(AgentEvent::Status {
                                    text: "Codex queued the note for the next turn boundary".into(),
                                });
                            }
                        } else if let Some(settings) = st.current_settings.clone() {
                            ctx.hub.capture_artifact_baseline(&ctx.thread_id);
                            if let Err(error) =
                                start_turn(&ctx, &rpc, &mut st, &text, &settings, &[]).await
                            {
                                ctx.emit(AgentEvent::Error {
                                    message: format!("Codex could not deliver the follow-up: {error:#}"),
                                });
                            }
                        }
                    }
                    AgentCommand::Interrupt => {
                        if let (Some(tid), Some(turn)) = (&st.provider_thread_id, &st.active_turn_id) {
                            let _ = rpc
                                .request("turn/interrupt", json!({ "threadId": tid, "turnId": turn }))
                                .await;
                        }
                    }
                    AgentCommand::Approval { approval_id, option_id } => {
                        if let Some(rpc_id) = st.pending_approvals.remove(&approval_id) {
                            let _ = rpc.respond(rpc_id, json!({ "decision": option_id })).await;
                        }
                        // Unknown id: a stale card from a dead predecessor
                        // driver — resolving it anyway un-sticks the UI.
                        ctx.emit(AgentEvent::ApprovalResolved { approval_id, option_id });
                    }
                    AgentCommand::Question { request_id, answers } => {
                        if let Some(rpc_id) = st.pending_questions.remove(&request_id) {
                            // Codex wants { answers: { <questionId>: { answers: [..] } } }
                            let map: serde_json::Map<String, Value> = answers
                                .iter()
                                .map(|(k, v)| (k.clone(), json!({ "answers": v })))
                                .collect();
                            let _ = rpc.respond(rpc_id, json!({ "answers": map })).await;
                        }
                        // Resolve even for unknown ids (stale card) — see Approval.
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
                        let turn_ended = matches!(method.as_str(), "turn/completed" | "turn/aborted");
                        handle_notification(&ctx, &mut st, &method, params);
                        if turn_ended {
                            if let (Some(text), Some(settings)) = (
                                st.pending_followups.pop_front(),
                                st.current_settings.clone(),
                            ) {
                                ctx.hub.capture_artifact_baseline(&ctx.thread_id);
                                if let Err(error) =
                                    start_turn(&ctx, &rpc, &mut st, &text, &settings, &[]).await
                                {
                                    ctx.emit(AgentEvent::Error {
                                        message: format!("Codex could not deliver the follow-up: {error:#}"),
                                    });
                                }
                            }
                        }
                    }
                    Some(Incoming::Request { id, method, params }) => {
                        handle_server_request(&ctx, &rpc, &mut st, id, &method, params).await;
                    }
                    Some(Incoming::Eof) | None => {
                        anyhow::bail!("codex app-server exited unexpectedly");
                    }
                }
            }
        }
    }

    let _ = child.kill().await;
    Ok(())
}

async fn start_turn(
    ctx: &DriverCtx,
    rpc: &Rpc,
    st: &mut SessionState,
    text: &str,
    settings: &ThreadSettings,
    attachments: &[AttachmentRef],
) -> Result<()> {
    let (approval_policy, sandbox, sandbox_policy) = policies(settings);

    if st.provider_thread_id.is_none() {
        let params = json!({
            "cwd": ctx.cwd,
            "approvalPolicy": approval_policy,
            "sandbox": sandbox,
            "model": settings.model,
        });
        let result = match &ctx.resume_session_id {
            Some(existing) => {
                let mut p = params.clone();
                p["threadId"] = json!(existing);
                match rpc.request("thread/resume", p).await {
                    Ok(r) => r,
                    // Stale/unknown native history must not brick the durable
                    // Threadknot thread. Start a replacement and seed it with the
                    // complete persisted transcript on its first turn.
                    Err(error) => {
                        tracing::warn!(
                            "unable to resume Codex thread {existing}; restoring from Threadknot history: {error:#}"
                        );
                        st.seed = ctx.resume_fallback_seed.clone();
                        ctx.emit(AgentEvent::Status {
                            text: "Codex session was unavailable; restored from Threadknot history"
                                .into(),
                        });
                        rpc.request("thread/start", params).await?
                    }
                }
            }
            None => rpc.request("thread/start", params).await?,
        };
        let tid = result
            .pointer("/thread/id")
            .and_then(|v| v.as_str())
            .context("thread start/resume returned no thread id")?
            .to_string();
        ctx.emit(AgentEvent::SessionStarted {
            provider_session_id: tid.clone(),
            model: settings.model.clone(),
            agent: Some(Agent::Codex),
        });
        st.provider_thread_id = Some(tid);
    }

    st.current_model = settings.model.clone();
    st.current_settings = Some(settings.clone());
    let text = super::transcript::seeded_message(st.seed.take().as_deref(), text);
    // Codex has no document/file input item, so non-image attachments are copied
    // into the workspace and referenced by path; Codex opens them via its shell.
    let docs = super::materialize_docs(&ctx.cwd, attachments);
    let text = format!("{text}{}", super::attachment_footer(&docs));
    let tid = st.provider_thread_id.clone().unwrap();
    // Codex's app-server accepts inline images as data-url items in `input`.
    // OpenAI currently tolerates an empty text item, but Threadknot must never emit
    // one: the same serializer shape is shared conceptually with Claude (which
    // hard-rejects it), and an image-only turn should carry only its image.
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({ "type": "text", "text": text }));
    }
    for att in attachments {
        if let Some(item) = image_input_item(att) {
            input.push(item);
        }
    }
    // Reject a turn with no valid content locally instead of sending an empty
    // input array. Preserves all valid image items via the shared guard.
    let input = super::content::sanitize_user_content(
        &super::content::SanitizeCtx {
            provider: "codex",
            model: &settings.model,
            attachment_count: attachments.len(),
        },
        input,
    )?;
    let mut params = json!({
        "threadId": tid,
        "input": input,
        "approvalPolicy": approval_policy,
        "sandboxPolicy": sandbox_policy,
        "model": settings.model,
    });
    if let Some(effort) = &settings.effort {
        params["effort"] = json!(effort);
    }
    if settings.mode == Mode::Plan {
        params["collaborationMode"] = json!({
            "mode": "plan",
            "settings": { "model": settings.model }
        });
    }
    let result = rpc.request("turn/start", params).await?;
    st.active_turn_id = result
        .pointer("/turn/id")
        .and_then(|v| v.as_str())
        .map(String::from);
    Ok(())
}

/// Read a stored image attachment into a Codex `input` image item (data URL).
fn image_input_item(att: &AttachmentRef) -> Option<Value> {
    if !att.mime_type.starts_with("image/") {
        return None;
    }
    let bytes = std::fs::read(&att.path).ok()?;
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(json!({
        "type": "image",
        "url": format!("data:{};base64,{}", att.mime_type, data),
    }))
}

fn item_field<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key).and_then(|v| v.as_str())
}

fn item_type(item: &Value) -> &str {
    item_field(item, "type")
        .or_else(|| item_field(item, "itemType"))
        .unwrap_or("unknown")
}

fn command_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn handle_notification(ctx: &DriverCtx, st: &mut SessionState, method: &str, params: Value) {
    match method {
        "turn/started" => {
            st.active_turn_id = params
                .pointer("/turn/id")
                .and_then(|v| v.as_str())
                .map(String::from);
            ctx.emit(AgentEvent::TurnStarted {
                agent: Some(Agent::Codex),
                model: (!st.current_model.is_empty()).then(|| st.current_model.clone()),
            });
        }
        "turn/completed" => {
            st.active_turn_id = None;
            let status = params
                .pointer("/turn/status")
                .and_then(|v| v.as_str())
                .unwrap_or("completed");
            match status {
                "interrupted" => ctx.emit(AgentEvent::TurnAborted),
                "failed" => ctx.emit(AgentEvent::Error {
                    message: params
                        .pointer("/turn/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("turn failed")
                        .to_string(),
                }),
                _ => ctx.emit(AgentEvent::TurnCompleted {
                    usage: st.last_usage.clone(),
                }),
            }
        }
        "turn/aborted" => {
            st.active_turn_id = None;
            ctx.emit(AgentEvent::TurnAborted);
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                ctx.emit(AgentEvent::AssistantDelta {
                    text: delta.to_string(),
                });
            }
        }
        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                ctx.emit(AgentEvent::ThinkingDelta {
                    text: delta.to_string(),
                });
            }
        }
        "item/plan/delta" => {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                ctx.emit(AgentEvent::AssistantDelta {
                    text: delta.to_string(),
                });
            }
        }
        "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
            let call_id = params
                .get("itemId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                ctx.emit(AgentEvent::ToolOutputDelta {
                    call_id,
                    text: delta.to_string(),
                });
            }
        }
        "item/started" => {
            let Some(item) = params.get("item") else {
                return;
            };
            let call_id = item_field(item, "id").unwrap_or("").to_string();
            match item_type(item) {
                "commandExecution" => ctx.emit(AgentEvent::ToolStart {
                    call_id,
                    name: "shell".into(),
                    detail: command_string(item.get("command")),
                }),
                "fileChange" => {
                    let paths: Vec<&str> = item
                        .get("changes")
                        .and_then(|c| c.as_array())
                        .map(|arr| arr.iter().filter_map(|ch| item_field(ch, "path")).collect())
                        .unwrap_or_default();
                    ctx.emit(AgentEvent::ToolStart {
                        call_id,
                        name: "edit".into(),
                        detail: paths.join(", "),
                    });
                }
                "webSearch" => ctx.emit(AgentEvent::ToolStart {
                    call_id,
                    name: "web_search".into(),
                    detail: item_field(item, "query").unwrap_or("").to_string(),
                }),
                "mcpToolCall" => ctx.emit(AgentEvent::ToolStart {
                    call_id,
                    name: "mcp".into(),
                    detail: format!(
                        "{}/{}",
                        item_field(item, "server").unwrap_or(""),
                        item_field(item, "tool").unwrap_or("")
                    ),
                }),
                _ => {}
            }
        }
        "item/completed" => {
            let Some(item) = params.get("item") else {
                return;
            };
            let call_id = item_field(item, "id").unwrap_or("").to_string();
            match item_type(item) {
                "agentMessage" => ctx.emit(AgentEvent::AssistantMessage {
                    text: item_field(item, "text").unwrap_or("").to_string(),
                }),
                "plan" => {
                    let text = item_field(item, "text")
                        .or_else(|| item_field(item, "plan"))
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        ctx.emit(AgentEvent::AssistantMessage { text });
                    }
                }
                "reasoning" => {
                    let text = extract_reasoning_text(item);
                    if !text.is_empty() {
                        ctx.emit(AgentEvent::Thinking { text });
                    }
                }
                "commandExecution" => {
                    let exit = item.get("exitCode").and_then(|v| v.as_i64());
                    ctx.emit(AgentEvent::ToolEnd {
                        call_id,
                        name: "shell".into(),
                        output: item_field(item, "aggregatedOutput").map(truncate_output),
                        is_error: exit.map(|c| c != 0).unwrap_or(false),
                        truncated: false,
                    });
                }
                "fileChange" => {
                    ctx.emit(AgentEvent::ToolEnd {
                        call_id,
                        name: "edit".into(),
                        output: None,
                        is_error: item_field(item, "status") == Some("failed"),
                        truncated: false,
                    });
                    if let Some(changes) = item.get("changes").and_then(|c| c.as_array()) {
                        for ch in changes {
                            if let (Some(path), Some(diff)) =
                                (item_field(ch, "path"), item_field(ch, "diff"))
                            {
                                ctx.emit(AgentEvent::FileDiff {
                                    path: path.to_string(),
                                    unified: diff.to_string(),
                                });
                            }
                        }
                    }
                }
                "webSearch" | "mcpToolCall" => ctx.emit(AgentEvent::ToolEnd {
                    call_id,
                    name: item_type(item).to_string(),
                    output: None,
                    is_error: false,
                    truncated: false,
                }),
                "error" => ctx.emit(AgentEvent::Error {
                    message: item_field(item, "message")
                        .unwrap_or("agent error")
                        .to_string(),
                }),
                _ => {}
            }
        }
        "thread/tokenUsage/updated" => {
            let last = params.pointer("/tokenUsage/last");
            let total = params
                .pointer("/tokenUsage/total/totalTokens")
                .and_then(|v| v.as_u64());
            let window = params
                .pointer("/tokenUsage/modelContextWindow")
                .and_then(|v| v.as_u64());
            let used = last
                .and_then(|l| l.get("totalTokens"))
                .and_then(|v| v.as_u64());
            st.last_usage = Some(Usage {
                input_tokens: last
                    .and_then(|l| l.get("inputTokens"))
                    .and_then(|v| v.as_u64()),
                output_tokens: last
                    .and_then(|l| l.get("outputTokens"))
                    .and_then(|v| v.as_u64()),
                used_tokens: used,
                max_tokens: window,
                context_pct: match (used, window) {
                    (Some(used), Some(max)) if max > 0 => Some(used as f64 / max as f64 * 100.0),
                    _ => None,
                },
                cost_usd: None,
            });
            if let Some(usage) = st.last_usage.clone() {
                ctx.emit(AgentEvent::ContextUsage { usage });
            }
            let _ = total;
        }
        "thread/compacted" => ctx.emit(AgentEvent::Status {
            text: "Context compacted".into(),
        }),
        // Free mid-session freshness for the header usage meter.
        "account/rateLimits/updated" => {
            if let Some(u) = crate::usage::parse_codex_rate_limits(&params["rateLimits"]) {
                crate::usage::publish(&ctx.hub, u);
            }
        }
        "error" => {
            let message = params
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("codex error")
                .to_string();
            let will_retry = params
                .get("willRetry")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if will_retry {
                ctx.emit(AgentEvent::Status {
                    text: format!("Retrying: {message}"),
                });
            } else {
                ctx.emit(AgentEvent::Error { message });
            }
        }
        _ => {}
    }
}

fn extract_reasoning_text(item: &Value) -> String {
    if let Some(text) = item_field(item, "text") {
        return text.to_string();
    }
    for key in ["summary", "content"] {
        if let Some(arr) = item.get(key).and_then(|v| v.as_array()) {
            let joined: Vec<String> = arr
                .iter()
                .filter_map(|p| {
                    p.as_str()
                        .map(String::from)
                        .or_else(|| p.get("text").and_then(|t| t.as_str()).map(String::from))
                })
                .collect();
            if !joined.is_empty() {
                return joined.join("\n\n");
            }
        }
    }
    String::new()
}

fn truncate_output(s: &str) -> String {
    const MAX: usize = 20_000;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n… [truncated {} bytes]", &s[..end], s.len() - end)
    }
}

async fn handle_server_request(
    ctx: &DriverCtx,
    rpc: &Rpc,
    st: &mut SessionState,
    id: Value,
    method: &str,
    params: Value,
) {
    let options: Vec<ApprovalOption> = APPROVAL_OPTIONS
        .iter()
        .map(|(id, label, tone)| ApprovalOption {
            id: id.to_string(),
            label: label.to_string(),
            tone: tone.to_string(),
        })
        .collect();

    match method {
        "item/commandExecution/requestApproval" => {
            let approval_id = new_id();
            st.pending_approvals.insert(approval_id.clone(), id);
            let cmd = command_string(params.get("command"));
            let detail = if cmd.is_empty() {
                params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                cmd
            };
            ctx.emit(AgentEvent::ApprovalRequest {
                approval_id,
                approval_kind: "exec".into(),
                title: "Codex wants to run a command".into(),
                detail,
                options,
            });
        }
        "item/fileChange/requestApproval" => {
            let approval_id = new_id();
            st.pending_approvals.insert(approval_id.clone(), id);
            ctx.emit(AgentEvent::ApprovalRequest {
                approval_id,
                approval_kind: "patch".into(),
                title: "Codex wants to change files".into(),
                detail: params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Apply proposed file changes")
                    .to_string(),
                options,
            });
        }
        "item/tool/requestUserInput" => {
            let request_id = new_id();
            let questions: Vec<Question> = params
                .get("questions")
                .and_then(|q| q.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|q| Question {
                            id: q
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            header: q
                                .get("header")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            question: q
                                .get("question")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            options: q
                                .get("options")
                                .and_then(|v| v.as_array())
                                .map(|opts| {
                                    opts.iter()
                                        .map(|o| QuestionOption {
                                            label: o
                                                .get("label")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                            description: o
                                                .get("description")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            multi_select: false,
                            allow_other: q
                                .get("isOther")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            is_secret: q.get("isSecret").and_then(|v| v.as_bool()).unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if questions.is_empty() {
                let _ = rpc.respond(id, json!({ "answers": {} })).await;
                return;
            }
            st.pending_questions.insert(request_id.clone(), id);
            ctx.emit(AgentEvent::QuestionRequest {
                request_id,
                questions,
            });
        }
        _ => {
            let _ = rpc
                .write_line(&json!({
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {method}") }
                }))
                .await;
        }
    }
}

/// One-shot probe used for agents.info: auth status + model list.
pub async fn probe(cwd: &str) -> Result<(bool, Vec<ModelInfo>, String)> {
    let (mut child, rpc, mut msg_rx) = spawn_codex(cwd, None, &[])?;
    // Drain notifications so the reader channel never backs up.
    tokio::spawn(async move { while msg_rx.recv().await.is_some() {} });

    let out = async {
        handshake(&rpc).await?;
        let account = rpc.request("account/read", json!({})).await?;
        let authed = account
            .get("account")
            .map(|a| !a.is_null())
            .unwrap_or(false)
            || !account
                .get("requiresOpenaiAuth")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

        let mut models = Vec::new();
        let mut default_model = String::from("gpt-5.4");
        let mut cursor: Option<String> = None;
        loop {
            let mut params = json!({});
            if let Some(c) = &cursor {
                params["cursor"] = json!(c);
            }
            let resp = rpc.request("model/list", params).await?;
            if let Some(items) = resp.get("data").and_then(|v| v.as_array()) {
                for m in items {
                    let id = m.get("model").and_then(|v| v.as_str()).unwrap_or("");
                    let hidden = m.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
                    if id.is_empty() || hidden {
                        continue;
                    }
                    if m.get("isDefault")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        default_model = id.to_string();
                    }
                    let efforts: Vec<String> = m
                        .get("supportedReasoningEfforts")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|e| e.get("reasoningEffort").and_then(|r| r.as_str()))
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    models.push(ModelInfo {
                        id: id.to_string(),
                        name: m
                            .get("displayName")
                            .and_then(|v| v.as_str())
                            .unwrap_or(id)
                            .to_string(),
                        image: None,
                        supports_wide_context: None,
                        fixed_context_window: None,
                        efforts: (!efforts.is_empty()).then_some(efforts),
                        default_effort: m
                            .get("defaultReasoningEffort")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
            }
            cursor = resp
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        if models.is_empty() {
            models.push(ModelInfo {
                id: "gpt-5.5".into(),
                name: "GPT-5.5".into(),
                image: None,
                supports_wide_context: None,
                fixed_context_window: None,
                efforts: None,
                default_effort: None,
            });
        }
        default_model = apply_threadknot_defaults(&mut models, default_model);
        Ok::<_, anyhow::Error>((authed, models, default_model))
    }
    .await;

    let _ = child.kill().await;
    out
}

/// Threadknot's opinionated Codex starting point. The app-server catalog remains
/// authoritative for availability and effort options, but its own Sol default
/// is deliberately "low"; new Threadknot Codex chats should start at high.
fn apply_threadknot_defaults(models: &mut [ModelInfo], discovered_default: String) -> String {
    const PREFERRED_MODEL: &str = "gpt-5.6-sol";
    const PREFERRED_EFFORT: &str = "high";

    if let Some(sol) = models.iter_mut().find(|model| model.id == PREFERRED_MODEL) {
        if sol
            .efforts
            .as_ref()
            .is_some_and(|efforts| efforts.iter().any(|effort| effort == PREFERRED_EFFORT))
        {
            sol.default_effort = Some(PREFERRED_EFFORT.into());
        }
        return PREFERRED_MODEL.into();
    }

    if models.iter().any(|model| model.id == discovered_default) {
        discovered_default
    } else {
        models
            .first()
            .map(|model| model.id.clone())
            .unwrap_or(discovered_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_app_server_never_treats_a_saved_thread_as_active() {
        let state = SessionState::new(Some("missed transcript".into()));

        assert!(state.provider_thread_id.is_none());
        assert_eq!(state.seed.as_deref(), Some("missed transcript"));
    }

    #[test]
    fn sol_is_the_codex_default_at_high_effort() {
        let mut models = vec![
            ModelInfo {
                id: "gpt-5.4".into(),
                name: "GPT-5.4".into(),
                image: None,
                supports_wide_context: None,
                fixed_context_window: None,
                efforts: Some(vec!["low".into(), "high".into()]),
                default_effort: Some("medium".into()),
            },
            ModelInfo {
                id: "gpt-5.6-sol".into(),
                name: "GPT-5.6 Sol".into(),
                image: None,
                supports_wide_context: None,
                fixed_context_window: None,
                efforts: Some(vec!["low".into(), "medium".into(), "high".into()]),
                default_effort: Some("low".into()),
            },
        ];

        let default_model = apply_threadknot_defaults(&mut models, "gpt-5.4".into());

        assert_eq!(default_model, "gpt-5.6-sol");
        assert_eq!(models[1].default_effort.as_deref(), Some("high"));
    }
}

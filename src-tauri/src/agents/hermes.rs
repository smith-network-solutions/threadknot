//! Hermes driver: drives a remote hermes-agent gateway over its Runs API.
//!
//! Wire format (verified against hermes-agent 0.17):
//! - `POST {base}/api/sessions` once per thread → a server-side session row.
//!   The gateway persists every turn into this session's SessionDB transcript,
//!   but — crucially — the runs endpoint below NEVER reloads it (see next).
//! - `POST {base}/v1/runs {input, session_id, conversation_history}` per turn →
//!   `{run_id}`. **`/v1/runs` is stateless by contract**: `session_id` scopes
//!   long-term memory + where the turn is persisted, but the gateway builds the
//!   turn's context ONLY from the request body — it does not read prior turns
//!   back from the session (only `/api/sessions/{id}/chat` does that). So unlike
//!   the claude/codex drivers (native `--resume`), Threadknot must resend the whole
//!   conversation as `conversation_history` (`[{role, content}]`) every turn, or
//!   the agent sees each message as a brand-new chat. Threadknot's event log is the
//!   source of truth (matching its "provider sessions are disposable views"
//!   design); the gateway dedups the resent history by object identity, so its
//!   own transcript stays clean — no duplication.
//!   `input` is a plain string, or a messages array whose last message may
//!   carry OpenAI-style content parts — inline images ride as
//!   `{"type":"image_url","image_url":{"url":"data:image/..;base64,.."}}`.
//!   The runs endpoint forwards that content to the agent core UNVALIDATED,
//!   so only the canonical normalized shape above is safe (it is what the
//!   gateway's chat_completions normalizer emits). Files/documents are
//!   rejected by the gateway; request bodies cap at 10 MB.
//! - `GET  {base}/v1/runs/{id}/events` — SSE stream of `data:` JSON frames:
//!   `message.delta`, `reasoning.available`, `tool.started`, `tool.completed`,
//!   `approval.request`, `approval.responded`, `run.completed{output,usage}`,
//!   `run.failed{error}`, `run.cancelled`.
//! - `POST {base}/v1/runs/{id}/stop` — interrupt.
//! - `POST {base}/v1/runs/{id}/approval {choice}` — resolve an approval gate
//!   (`once` | `session` | `always` | `deny`).
//!
//! Unlike the claude/codex drivers there is no child process: the "session" is
//! HTTP state on the remote gateway. Which gateway a turn targets rides in
//! `ThreadSettings.model` (the registry entry id — see `crate::hermes`).

use super::{AgentCommand, AttachmentRef, DriverCtx};
use crate::hermes::HermesAgent;
use crate::protocol::*;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::mpsc;

const APPROVAL_CHOICES: &[(&str, &str, &str)] = &[
    ("once", "Allow once", "allow"),
    ("session", "Allow for this session", "allowAlways"),
    ("always", "Always allow", "allowAlways"),
    ("deny", "Deny", "deny"),
];

struct SessionState {
    /// Remote hermes session id, plus the registry agent it lives on — a
    /// session on chip's gateway means nothing to carlos's.
    session: Option<(String, String)>, // (agent_id, session_id)
    active_run: Option<String>,
    /// Registry agent the active run targets (for stop/approval calls).
    active_agent: Option<HermesAgent>,
    /// our approvalId -> run_id whose gate it resolves
    pending_approvals: HashMap<String, String>,
    /// Synthetic ToolStart call ids not yet closed, per tool name (hermes
    /// events carry no call id, so we pair started/completed by name, LIFO).
    open_tools: HashMap<String, Vec<String>>,
    tool_seq: u64,
}

/// `resume_session_id` anchors are stored as "agentId/sessionId" so a
/// mid-thread switch to a different gateway never resumes a foreign session.
fn parse_anchor(anchor: &str) -> Option<(String, String)> {
    anchor
        .split_once('/')
        .map(|(a, s)| (a.to_string(), s.to_string()))
}

pub async fn run(ctx: DriverCtx, mut cmd_rx: mpsc::UnboundedReceiver<AgentCommand>) -> Result<()> {
    let client = crate::hermes::http_client();
    let mut st = SessionState {
        session: ctx
            .resume_session_id
            .as_deref()
            .and_then(parse_anchor),
        active_run: None,
        active_agent: None,
        pending_approvals: HashMap::new(),
        open_tools: HashMap::new(),
        tool_seq: 0,
    };
    // Events parsed off the active run's SSE stream.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<StreamEvent>();

    if let Some(run_id) = ctx.recover_provider_run_id.clone() {
        if let Err(error) =
            recover_remote_run(&ctx, &client, &mut st, &ev_tx, run_id).await
        {
            finish_turn(&ctx, &mut st);
            ctx.emit(AgentEvent::Error {
                message: format!("could not reattach Hermes run after restart: {error:#}"),
            });
        }
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCommand::User { text, settings, attachments } => {
                        if attachments.iter().any(|a| !a.mime_type.starts_with("image/")) {
                            ctx.emit(AgentEvent::Status {
                                text: "Remote (Hermes) agents can't receive document attachments yet — only images were sent. Non-image files were skipped.".into(),
                            });
                        }
                        if let Err(e) = start_turn(&ctx, &client, &mut st, &ev_tx, &text, &settings, &attachments).await {
                            ctx.emit(AgentEvent::Error { message: format!("{e:#}") });
                        }
                    }
                    // Approvals and model routing are per-turn on the runs API;
                    // nothing to push to a live process.
                    AgentCommand::Settings { .. } => {}
                    AgentCommand::Retire => break,
                    // Hub gates turn.steer to Claude agents; unreachable here.
                    AgentCommand::Steer { .. } => {
                        tracing::warn!("steer not supported for hermes; ignored");
                    }
                    AgentCommand::Interrupt => {
                        if let (Some(run), Some(agent)) = (&st.active_run, &st.active_agent) {
                            let _ = client
                                .post(format!("{}/v1/runs/{run}/stop", agent.base_url))
                                .bearer_auth(&agent.api_key)
                                .timeout(Duration::from_secs(10))
                                .send()
                                .await;
                            // The stream delivers run.cancelled / a terminal
                            // event; the boundary is emitted there.
                        }
                    }
                    AgentCommand::Approval { approval_id, option_id } => {
                        if let (Some(run), Some(agent)) =
                            (st.pending_approvals.remove(&approval_id), &st.active_agent)
                        {
                            let _ = client
                                .post(format!("{}/v1/runs/{run}/approval", agent.base_url))
                                .bearer_auth(&agent.api_key)
                                .timeout(Duration::from_secs(10))
                                .json(&json!({ "choice": option_id }))
                                .send()
                                .await;
                        }
                        // Resolve even for unknown ids (stale card) — un-sticks the UI.
                        ctx.emit(AgentEvent::ApprovalResolved { approval_id, option_id });
                    }
                    AgentCommand::Question { request_id, answers } => {
                        // Hermes has no question protocol; only stale cards land here.
                        ctx.emit(AgentEvent::QuestionResolved {
                            request_id,
                            answers: Some(answers),
                        });
                    }
                }
            }
            ev = ev_rx.recv() => {
                let Some(ev) = ev else { break };
                handle_stream_event(&ctx, &client, &mut st, ev).await;
            }
        }
    }
    Ok(())
}

async fn start_turn(
    ctx: &DriverCtx,
    client: &reqwest::Client,
    st: &mut SessionState,
    ev_tx: &mpsc::UnboundedSender<StreamEvent>,
    text: &str,
    settings: &ThreadSettings,
    attachments: &[AttachmentRef],
) -> Result<()> {
    let agent = ctx
        .hub
        .hermes
        .agent(&settings.model)
        .with_context(|| "this Hermes agent was removed — pick another in the composer")?;

    // A saved session only counts if it lives on the gateway this turn targets.
    match &st.session {
        Some((aid, _)) if *aid == agent.id => {}
        Some(_) => {
            // Mid-thread switch to a different gateway: start a fresh session
            // there. Context is not lost — every turn resends the full
            // conversation as `conversation_history` (built below).
            st.session = None;
            ctx.emit(AgentEvent::Status {
                text: format!("Switched to {} — starting a new session there", agent.name),
            });
        }
        None => {}
    }

    if st.session.is_none() {
        let title = ctx
            .hub
            .store
            .thread(&ctx.thread_id)
            .map(|t| t.title)
            .unwrap_or_else(|| "Threadknot thread".into());
        let sid = create_session(client, &agent, &title, &ctx.thread_id).await?;
        ctx.emit(AgentEvent::SessionStarted {
            // Anchor format: agentId/sessionId (see parse_anchor).
            provider_session_id: format!("{}/{}", agent.id, sid),
            model: agent.name.clone(),
            agent: Some(Agent::Hermes),
        });
        st.session = Some((agent.id.clone(), sid));
    }
    let (_, sid) = st.session.clone().unwrap();

    // The Hermes runs API keeps NO server-side turn context: `session_id` scopes
    // long-term memory + transcript persistence, but `/v1/runs` never reloads
    // prior turns from it. So unlike the claude/codex drivers (native
    // `--resume`), every Hermes turn must resend the whole conversation. Threadknot's
    // event log is the source of truth; the current user message was already
    // persisted to it, so it is dropped here (it rides in `input` instead).
    let events = ctx.hub.store.read_events(&ctx.thread_id);
    let history: Vec<Value> = super::transcript::render_messages(&events, Some(text))
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();

    let input = runs_input(text, attachments);
    let mut req_body = json!({ "input": input, "session_id": sid });
    if !history.is_empty() {
        req_body["conversation_history"] = Value::Array(history);
    }
    let resp = client
        .post(format!("{}/v1/runs", agent.base_url))
        .bearer_auth(&agent.api_key)
        // Long-term memory scope: stable per Threadknot thread.
        .header("X-Hermes-Session-Key", format!("threadknot-{}", ctx.thread_id))
        .timeout(Duration::from_secs(30))
        .json(&req_body)
        .send()
        .await
        .with_context(|| format!("cannot reach {} at {}", agent.name, agent.base_url))?;
    anyhow::ensure!(
        resp.status().is_success(),
        "{} rejected the turn: {} {}",
        agent.name,
        resp.status(),
        resp.text().await.unwrap_or_default().chars().take(300).collect::<String>()
    );
    let body: Value = resp.json().await.context("parse run submission")?;
    let run_id = body
        .get("run_id")
        .and_then(|v| v.as_str())
        .context("run submission returned no run_id")?
        .to_string();

    // Persist immediately after submission. The remote gateway keeps running
    // if Threadknot exits, so the next process needs this id to reconnect rather
    // than submit a duplicate continuation.
    ctx.hub.store.update_thread(&ctx.thread_id, |thread| {
        thread.provider_run_id = Some(run_id.clone())
    })?;
    ctx.emit(AgentEvent::TurnStarted {
        agent: Some(Agent::Hermes),
        model: Some(agent.name.clone()),
    });
    st.active_run = Some(run_id.clone());
    st.active_agent = Some(agent.clone());
    st.open_tools.clear();
    spawn_event_stream(client.clone(), agent, run_id, ev_tx.clone());
    Ok(())
}

/// Reconnect a freshly-started Threadknot process to the exact remote run that
/// outlived it. A terminal run is finalized from the status endpoint; an
/// active run resumes its SSE stream and retains any unresolved approval ids.
async fn recover_remote_run(
    ctx: &DriverCtx,
    client: &reqwest::Client,
    st: &mut SessionState,
    ev_tx: &mpsc::UnboundedSender<StreamEvent>,
    run_id: String,
) -> Result<()> {
    let thread = ctx
        .hub
        .store
        .thread(&ctx.thread_id)
        .context("thread disappeared during restart recovery")?;
    let agent = ctx
        .hub
        .hermes
        .agent(&thread.settings.model)
        .with_context(|| "the Hermes agent for this run is no longer registered")?;
    let resp = client
        .get(format!("{}/v1/runs/{run_id}", agent.base_url))
        .bearer_auth(&agent.api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("cannot reach {} at {}", agent.name, agent.base_url))?;
    anyhow::ensure!(
        resp.status().is_success(),
        "{} could not find run {} ({})",
        agent.name,
        run_id,
        resp.status()
    );
    let body: Value = resp.json().await.context("parse recovered run status")?;

    st.active_run = Some(run_id.clone());
    st.active_agent = Some(agent.clone());
    st.open_tools.clear();
    for approval_id in unresolved_approval_ids(&ctx.hub.store.read_events(&ctx.thread_id)) {
        st.pending_approvals
            .insert(approval_id, run_id.clone());
    }

    if let Some(terminal) = terminal_run_event(&body) {
        handle_run_event(ctx, st, &terminal);
    } else {
        ctx.emit(AgentEvent::Status {
            text: format!("Reconnected to {} after Threadknot restarted", agent.name),
        });
        spawn_event_stream(client.clone(), agent, run_id, ev_tx.clone());
    }
    Ok(())
}

fn unresolved_approval_ids(events: &[PersistedEvent]) -> Vec<String> {
    let mut pending = HashSet::new();
    for record in events {
        match &record.event {
            AgentEvent::ApprovalRequest { approval_id, .. } => {
                pending.insert(approval_id.clone());
            }
            AgentEvent::ApprovalResolved { approval_id, .. } => {
                pending.remove(approval_id);
            }
            _ => {}
        }
    }
    pending.into_iter().collect()
}

/// Build the `/v1/runs` `input` payload: a plain string for text-only turns,
/// or a single-message array whose content carries OpenAI-style parts when
/// image attachments ride along (see the module docs for why the shape must
/// be exactly the normalized `text` / `image_url` form).
fn runs_input(text: &str, attachments: &[AttachmentRef]) -> Value {
    let images: Vec<Value> = attachments.iter().filter_map(image_part).collect();
    if images.is_empty() {
        return json!(text);
    }
    let mut parts = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    parts.extend(images);
    json!([{ "role": "user", "content": parts }])
}

/// Read a stored image attachment into an inline `image_url` data-URL part.
fn image_part(att: &AttachmentRef) -> Option<Value> {
    if !att.mime_type.starts_with("image/") {
        return None;
    }
    let bytes = std::fs::read(&att.path).ok()?;
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{};base64,{}", att.mime_type, data) },
    }))
}

/// The gateway enforces GLOBALLY unique session titles, and its session rows
/// never expire. Its store is shared by every machine pointed at it, so a bare
/// thread title ("hi", "test", the fallback below) 400s forever once anyone has
/// used it. That reads as "Hermes is down" but is only a name collision.
/// So qualify the title with the thread id (stable, so repeat sessions for one
/// thread still group visibly on the gateway) and fall back to a timestamped
/// title if even that is taken.
async fn create_session(
    client: &reqwest::Client,
    agent: &HermesAgent,
    title: &str,
    thread_id: &str,
) -> Result<String> {
    let short: String = thread_id.chars().take(8).collect();
    if let Some(sid) = try_create_session(client, agent, &format!("{title} [{short}]")).await? {
        return Ok(sid);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    try_create_session(client, agent, &format!("{title} [{short}-{stamp}]"))
        .await?
        .context("gateway rejected every generated session title as a duplicate")
}

/// `Ok(None)` means the gateway rejected the title as already in use; the
/// caller retries under another name.
async fn try_create_session(
    client: &reqwest::Client,
    agent: &HermesAgent,
    title: &str,
) -> Result<Option<String>> {
    let resp = client
        .post(format!("{}/api/sessions", agent.base_url))
        .bearer_auth(&agent.api_key)
        .timeout(Duration::from_secs(15))
        .json(&json!({ "title": title }))
        .send()
        .await
        .with_context(|| format!("cannot reach {} at {}", agent.name, agent.base_url))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return body
            .pointer("/session/id")
            .and_then(|v| v.as_str())
            .map(|id| Some(id.to_string()))
            .context("session create returned no id");
    }
    if body.pointer("/error/code").and_then(|v| v.as_str()) == Some("invalid_title") {
        return Ok(None);
    }
    // Report what the gateway actually said; a bare status code hides the cause.
    let detail = body
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| text.trim());
    anyhow::bail!(
        "{} session create failed: {status}{}",
        agent.name,
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )
}

/// One parsed frame off a run's SSE stream, or its termination marker.
enum StreamEvent {
    Frame { run_id: String, data: Value },
    /// Stream closed without a terminal run event (network drop, gateway
    /// restart) — the driver polls the run's status to find the outcome.
    Lost { run_id: String },
}

fn spawn_event_stream(
    client: reqwest::Client,
    agent: HermesAgent,
    run_id: String,
    ev_tx: mpsc::UnboundedSender<StreamEvent>,
) {
    tokio::spawn(async move {
        let mut terminal = false;
        let resp = client
            .get(format!("{}/v1/runs/{run_id}/events", agent.base_url))
            .bearer_auth(&agent.api_key)
            .send()
            .await;
        match resp {
            Ok(resp) if resp.status().is_success() => {
                let mut stream = resp.bytes_stream();
                let mut buf: Vec<u8> = Vec::new();
                'outer: while let Some(chunk) = stream.next().await {
                    let Ok(chunk) = chunk else { break };
                    buf.extend_from_slice(&chunk);
                    // SSE frames are blank-line separated; runs streams carry
                    // bare `data:` lines (no `event:` name) plus `:` keepalives.
                    while let Some(pos) = find_frame_end(&buf) {
                        let frame: Vec<u8> = buf.drain(..pos + 2).collect();
                        let Ok(textf) = std::str::from_utf8(&frame) else { continue };
                        for line in textf.lines() {
                            let Some(data) = line.strip_prefix("data:") else { continue };
                            let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                                continue;
                            };
                            let is_terminal = matches!(
                                value.get("event").and_then(|v| v.as_str()),
                                Some("run.completed" | "run.failed" | "run.cancelled")
                            );
                            if ev_tx
                                .send(StreamEvent::Frame {
                                    run_id: run_id.clone(),
                                    data: value,
                                })
                                .is_err()
                            {
                                break 'outer;
                            }
                            if is_terminal {
                                terminal = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if !terminal {
            let _ = ev_tx.send(StreamEvent::Lost { run_id });
        }
    });
}

/// Index just past the first `\n\n` frame boundary (tolerates `\r\n\r\n`).
fn find_frame_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n").or_else(|| {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 2)
    })
}

async fn handle_stream_event(
    ctx: &DriverCtx,
    client: &reqwest::Client,
    st: &mut SessionState,
    ev: StreamEvent,
) {
    match ev {
        StreamEvent::Frame { run_id, data } => {
            // A stale stream from an already-finished run must not corrupt the
            // current turn.
            if st.active_run.as_deref() != Some(run_id.as_str()) {
                return;
            }
            handle_run_event(ctx, st, &data);
        }
        StreamEvent::Lost { run_id } => {
            if st.active_run.as_deref() != Some(run_id.as_str()) {
                return;
            }
            // Stream died mid-run: ask the gateway how the run actually ended.
            let outcome = match &st.active_agent {
                Some(agent) => poll_run_outcome(client, agent, &run_id).await,
                None => None,
            };
            match outcome {
                Some(data) => handle_run_event(ctx, st, &data),
                None => {
                    finish_turn(ctx, st);
                    ctx.emit(AgentEvent::Error {
                        message: "lost connection to the Hermes gateway mid-turn".into(),
                    });
                }
            }
        }
    }
}

/// Fetch a finished run's status and synthesize its terminal event.
async fn poll_run_outcome(
    client: &reqwest::Client,
    agent: &HermesAgent,
    run_id: &str,
) -> Option<Value> {
    let resp = client
        .get(format!("{}/v1/runs/{run_id}", agent.base_url))
        .bearer_auth(&agent.api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let body: Value = resp.json().await.ok()?;
    terminal_run_event(&body)
}

fn terminal_run_event(body: &Value) -> Option<Value> {
    match body.get("status").and_then(|v| v.as_str())? {
        "completed" => Some(json!({
            "event": "run.completed",
            "output": body.get("output").cloned().unwrap_or(json!("")),
            "usage": body.get("usage").cloned().unwrap_or(Value::Null),
        })),
        "cancelled" => Some(json!({ "event": "run.cancelled" })),
        "failed" => Some(json!({
            "event": "run.failed",
            "error": body.get("error").cloned().unwrap_or(json!("run failed")),
        })),
        _ => None,
    }
}

fn finish_turn(ctx: &DriverCtx, st: &mut SessionState) {
    st.active_run = None;
    let _ = ctx.hub.store.update_thread(&ctx.thread_id, |thread| {
        thread.provider_run_id = None
    });
    st.active_agent = None;
    st.pending_approvals.clear();
    st.open_tools.clear();
}

fn handle_run_event(ctx: &DriverCtx, st: &mut SessionState, data: &Value) {
    let kind = data.get("event").and_then(|v| v.as_str()).unwrap_or("");
    let text_field = |key: &str| data.get(key).and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "message.delta" => {
            let delta = text_field("delta");
            if !delta.is_empty() {
                ctx.emit(AgentEvent::AssistantDelta {
                    text: delta.to_string(),
                });
            }
        }
        "reasoning.available" => {
            let text = text_field("text");
            if !text.is_empty() {
                ctx.emit(AgentEvent::Thinking {
                    text: text.to_string(),
                });
            }
        }
        "tool.started" => {
            let name = text_field("tool").to_string();
            st.tool_seq += 1;
            let call_id = format!("hrm-{}-{}", st.tool_seq, name);
            st.open_tools
                .entry(name.clone())
                .or_default()
                .push(call_id.clone());
            ctx.emit(AgentEvent::ToolStart {
                call_id,
                name,
                detail: text_field("preview").to_string(),
            });
        }
        "tool.completed" => {
            let name = text_field("tool").to_string();
            let call_id = st
                .open_tools
                .get_mut(&name)
                .and_then(|v| v.pop())
                .unwrap_or_else(|| {
                    st.tool_seq += 1;
                    format!("hrm-{}-{}", st.tool_seq, name)
                });
            ctx.emit(AgentEvent::ToolEnd {
                call_id,
                name,
                output: None,
                is_error: data.get("error").and_then(|v| v.as_bool()).unwrap_or(false),
                truncated: false,
            });
        }
        "approval.request" => {
            let approval_id = new_id();
            if let Some(run) = &st.active_run {
                st.pending_approvals.insert(approval_id.clone(), run.clone());
            }
            let offered: Vec<String> = data
                .get("choices")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.as_str())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_else(|| vec!["once".into(), "deny".into()]);
            let options: Vec<ApprovalOption> = APPROVAL_CHOICES
                .iter()
                .filter(|(id, _, _)| offered.iter().any(|o| o == id))
                .map(|(id, label, tone)| ApprovalOption {
                    id: id.to_string(),
                    label: label.to_string(),
                    tone: tone.to_string(),
                })
                .collect();
            let command = text_field("command");
            let description = text_field("description");
            let detail = match (command.is_empty(), description.is_empty()) {
                (false, false) => format!("{command}\n{description}"),
                (false, true) => command.to_string(),
                _ => description.to_string(),
            };
            let who = st
                .active_agent
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "Hermes".into());
            ctx.emit(AgentEvent::ApprovalRequest {
                approval_id,
                approval_kind: "exec".into(),
                title: format!("{who} wants to run a command"),
                detail,
                options,
            });
        }
        // The card is resolved when the user's Approval command is handled;
        // the echo only matters for approvals answered outside Threadknot.
        "approval.responded" => {}
        "run.completed" => {
            let output = text_field("output").to_string();
            if !output.is_empty() {
                ctx.emit(AgentEvent::AssistantMessage { text: output });
            }
            let usage = data.get("usage").map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
                used_tokens: None,
                max_tokens: None,
                context_pct: None,
                cost_usd: None,
            });
            finish_turn(ctx, st);
            ctx.emit(AgentEvent::TurnCompleted { usage });
        }
        "run.cancelled" => {
            finish_turn(ctx, st);
            ctx.emit(AgentEvent::TurnAborted);
        }
        "run.failed" => {
            let message = text_field("error").to_string();
            finish_turn(ctx, st);
            ctx.emit(AgentEvent::Error {
                message: if message.is_empty() {
                    "Hermes run failed".into()
                } else {
                    message
                },
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_scoped_to_a_gateway() {
        assert_eq!(
            parse_anchor("agent-1/api_123"),
            Some(("agent-1".into(), "api_123".into()))
        );
        assert_eq!(parse_anchor("legacy-unscoped"), None);
    }

    #[test]
    fn runs_input_stays_a_string_without_images() {
        let v = runs_input("hi", &[]);
        assert_eq!(v, json!("hi"));
        // Unreadable/non-image attachments degrade to the plain-string form.
        let v = runs_input(
            "hi",
            &[AttachmentRef {
                name: "doc.pdf".into(),
                mime_type: "application/pdf".into(),
                path: "/nonexistent".into(),
            }],
        );
        assert_eq!(v, json!("hi"));
    }

    #[test]
    fn runs_input_inlines_images_as_normalized_parts() {
        let dir = std::env::temp_dir().join("threadknot-hermes-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("px.png");
        std::fs::write(&path, [0x89u8, 0x50, 0x4e, 0x47]).unwrap();
        let v = runs_input(
            "look",
            &[AttachmentRef {
                name: "px.png".into(),
                mime_type: "image/png".into(),
                path,
            }],
        );
        let content = &v[0]["content"];
        assert_eq!(v[0]["role"], "user");
        assert_eq!(content[0], json!({ "type": "text", "text": "look" }));
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn sse_frame_boundaries() {
        assert_eq!(find_frame_end(b"data: {}\n\nrest"), Some(8));
        assert_eq!(find_frame_end(b"data: {}\r\n\r\nrest"), Some(10));
        assert_eq!(find_frame_end(b"data: {"), None);
    }

    #[test]
    fn restart_recovery_restores_only_unresolved_approval_ids() {
        let event = |seq, event| PersistedEvent {
            seq,
            ts: "2026-07-23T12:00:00Z".into(),
            speaker: None,
            event,
        };
        let request = |id: &str| AgentEvent::ApprovalRequest {
            approval_id: id.into(),
            approval_kind: "exec".into(),
            title: "run command".into(),
            detail: "true".into(),
            options: vec![],
        };
        let events = vec![
            event(1, request("resolved")),
            event(
                2,
                AgentEvent::ApprovalResolved {
                    approval_id: "resolved".into(),
                    option_id: "once".into(),
                },
            ),
            event(3, request("still-waiting")),
        ];

        assert_eq!(
            unresolved_approval_ids(&events),
            vec!["still-waiting".to_string()]
        );
    }

    #[test]
    fn recovered_run_status_maps_terminal_outcomes() {
        let completed = terminal_run_event(&json!({
            "status": "completed",
            "output": "done",
            "usage": { "input_tokens": 2, "output_tokens": 3 }
        }))
        .unwrap();
        assert_eq!(completed["event"], "run.completed");
        assert_eq!(completed["output"], "done");
        assert_eq!(
            terminal_run_event(&json!({ "status": "cancelled" })).unwrap()["event"],
            "run.cancelled"
        );
        assert!(terminal_run_event(&json!({ "status": "running" })).is_none());
    }
}

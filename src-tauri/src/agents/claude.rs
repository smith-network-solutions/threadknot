//! Claude Code driver: speaks the claude CLI's stream-json wire protocol directly
//! (the same channel the Claude Agent SDK uses), so the user's subscription login
//! (`claude login` OAuth) applies with no API key involved.
//!
//! Spawn: `claude --output-format stream-json --verbose --input-format stream-json
//!         --include-partial-messages --permission-prompt-tool stdio ...`
//! stdin:  `{"type":"user",...}` turns and `{"type":"control_request",...}` ops.
//! stdout: NDJSON SDK messages plus `control_request`/`can_use_tool` permission
//!         prompts which we answer with `control_response` frames.

use super::{AgentCommand, AttachmentRef, DriverCtx};
use crate::claudex::ClaudexProfile;
use crate::protocol::*;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// After a turn that used background agents winds down, hold this long before
/// declaring it done. A real continuation re-inits within ~10ms (which cancels
/// the finalize), so this only has to outlast that latency with margin for a
/// loaded machine; the last result — the one with no `init` after it — survives
/// the window and fires a single TurnCompleted. Only applies to turns that
/// actually spawned background agents, so plain turns end with no added latency.
const FINALIZE_GRACE: Duration = Duration::from_millis(750);

/// How many trailing stderr lines to retain for diagnostics. The CLI's stderr is
/// otherwise discarded, so a startup failure (unknown flag on an outdated CLI,
/// node-version abort, auth error) leaves the turn spinning with no clue. We keep
/// the tail and fold it into the exit error so the failure is legible in the UI.
const STDERR_TAIL_LINES: usize = 40;

/// A healthy Claude turn normally produces its first model stream frame within
/// seconds. The CLI's HTTP client can occasionally leave one TCP flow in the
/// kernel retransmit backoff for many minutes while the process and stdin pipe
/// both still look alive. Only guard the pre-response phase: after any model or
/// tool activity, an automatic replay could duplicate real work.
const FIRST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);

/// Stop is a process-health boundary. Give the CLI a brief chance to persist
/// its interrupted result, then retire it even if it still looks alive.
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);

/// One transparent reconnect fixes a bad TCP flow. A second silent failure
/// becomes an explicit error instead of retrying forever.
const MAX_AUTO_RECONNECTS: u8 = 1;

/// Compaction — `/compact`, or the automatic one Claude Code runs when the
/// window fills — is a single long summarization request that emits no model or
/// tool traffic while it works, only a `compacting` status. Against the ordinary
/// 90s budget it always reads as a stall, so the watchdog kills the CLI
/// mid-summary and the compaction is silently lost. Summarizing a full 1M-token
/// window is the slow case this has to cover.
const COMPACTION_TIMEOUT: Duration = Duration::from_secs(600);

/// Shared ring buffer of the CLI's most recent stderr lines.
type StderrTail = Arc<Mutex<VecDeque<String>>>;

/// Resources owned by one Claude CLI process.
type SpawnedClaude = (
    Child,
    Session,
    mpsc::UnboundedReceiver<Value>,
    StderrTail,
    tokio::task::JoinHandle<()>,
    Option<McpConfigLease>,
);

/// Keeps a file-backed MCP configuration alive for one Claude process.
///
/// Claude's native Windows executable does not reliably preserve the quotes in
/// inline JSON passed through Rust's Windows command-line encoder. When that
/// happens it treats the mangled JSON as a relative filename and aborts before
/// the stream starts. A real JSON file avoids the quoting boundary entirely.
/// The Windows handle is opened delete-on-close so an app crash cannot strand
/// the browser bearer token (or Library MCP credentials) in the temp folder.
struct McpConfigLease {
    path: PathBuf,
    _file: std::fs::File,
}

impl Drop for McpConfigLease {
    fn drop(&mut self) {
        // On Windows the open handle's delete-on-close flag is the final
        // backstop. This explicit removal handles ordinary shutdown on every
        // platform and is intentionally best-effort during process teardown.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Return the value handed to `--mcp-config` and, when needed, the lease that
/// keeps its backing file alive until the Claude child retires.
fn mcp_config_for_spawn(json: String) -> Result<(String, Option<McpConfigLease>)> {
    #[cfg(not(windows))]
    {
        Ok((json, None))
    }

    #[cfg(windows)]
    {
        use std::io::Write as _;
        use std::os::windows::fs::OpenOptionsExt as _;

        // WinBase FILE_FLAG_DELETE_ON_CLOSE. std's Windows OpenOptions shares
        // read/write/delete access, so Claude can still open the file by name.
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;

        let path = std::env::temp_dir().join(format!(
            "threadknot-claude-mcp-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        let mut file = options
            .open(&path)
            .with_context(|| format!("create temporary Claude MCP config {}", path.display()))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("write temporary Claude MCP config {}", path.display()))?;
        file.flush()
            .with_context(|| format!("flush temporary Claude MCP config {}", path.display()))?;

        let arg = path.to_string_lossy().into_owned();
        Ok((arg, Some(McpConfigLease { path, _file: file })))
    }
}

/// Render the retained stderr tail as a single string, or a placeholder when the
/// process died without printing anything.
fn stderr_summary(tail: &StderrTail) -> String {
    let lines = tail.lock().unwrap();
    if lines.is_empty() {
        "(no stderr output)".to_string()
    } else {
        lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

#[derive(Debug, Clone, Copy)]
struct DriverPolicy {
    first_response_timeout: Duration,
    compaction_timeout: Duration,
    interrupt_grace: Duration,
    max_auto_reconnects: u8,
}

impl DriverPolicy {
    fn production() -> Self {
        Self {
            first_response_timeout: FIRST_RESPONSE_TIMEOUT,
            compaction_timeout: COMPACTION_TIMEOUT,
            interrupt_grace: INTERRUPT_GRACE,
            max_auto_reconnects: MAX_AUTO_RECONNECTS,
        }
    }
}

/// Deterministic state machine for the only phase that is safe to replay: a
/// user message was accepted by Threadknot, but Claude has not produced model/tool
/// output yet.
#[derive(Debug)]
struct FirstResponseWatchdog {
    timeout: Duration,
    compaction_timeout: Duration,
    max_reconnects: u8,
    reconnects: u8,
    deadline: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallAction {
    Reconnect,
    Fail,
}

impl FirstResponseWatchdog {
    fn new(policy: DriverPolicy) -> Self {
        Self {
            timeout: policy.first_response_timeout,
            compaction_timeout: policy.compaction_timeout,
            max_reconnects: policy.max_auto_reconnects,
            reconnects: 0,
            deadline: None,
        }
    }

    fn start_turn(&mut self, now: Instant) {
        self.reconnects = 0;
        self.deadline = Some(now + self.timeout);
    }

    fn provider_progress(&mut self) {
        self.deadline = None;
    }

    /// The CLI has begun compacting: give it a compaction-sized budget instead
    /// of the pre-response one. Safe to bound rather than disarm because a real
    /// compaction ends in `compact_boundary`, which counts as progress and
    /// clears the deadline outright.
    ///
    /// Only ever *extends* an already-armed deadline. An auto-compaction that
    /// fires mid-turn (after real output) must not newly arm the watchdog: its
    /// recovery replays the user message, which is only safe before any
    /// assistant or tool work has happened.
    fn compacting(&mut self, now: Instant) {
        if self.deadline.is_some() {
            self.deadline = Some(now + self.compaction_timeout);
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn timed_out(&mut self, now: Instant) -> StallAction {
        if self.reconnects < self.max_reconnects {
            self.reconnects += 1;
            self.deadline = Some(now + self.timeout);
            StallAction::Reconnect
        } else {
            self.deadline = None;
            StallAction::Fail
        }
    }
}

/// Frames proving the provider request itself is moving. CLI-local init/status
/// and context-meter control responses deliberately do not count: both were
/// observed while a real API TCP connection remained black-holed.
fn is_provider_progress(frame: &Value) -> bool {
    let from_subagent = frame
        .get("parent_tool_use_id")
        .map(|parent| !parent.is_null())
        .unwrap_or(false);
    match frame.get("type").and_then(Value::as_str).unwrap_or("") {
        "stream_event" | "result" | "control_request" => true,
        "assistant" | "user" => !from_subagent,
        "system" => matches!(
            frame.get("subtype").and_then(Value::as_str),
            Some(
                "background_tasks_changed"
                    | "task_started"
                    | "task_notification"
                    | "compact_boundary"
            )
        ),
        _ => false,
    }
}

/// The CLI announcing that it has started compacting. This rides the same
/// `system`/`status` channel [`is_provider_progress`] deliberately ignores, so
/// it is matched on its exact status text rather than by loosening that rule.
fn is_compaction_start(frame: &Value) -> bool {
    frame.get("type").and_then(Value::as_str) == Some("system")
        && frame.get("subtype").and_then(Value::as_str) == Some("status")
        && frame.get("status").and_then(Value::as_str) == Some("compacting")
}

/// A message that is nothing but a slash command invocation (`/compact`,
/// `/cost`, `/my-command with args`) — the only form the CLI intercepts locally.
fn is_slash_command(text: &str) -> bool {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return false;
    };
    if trimmed.contains('\n') {
        return false;
    }
    // Split on whitespace rather than skipping it: `/ some words` is prose that
    // happens to open with a slash, not an invocation of a command named `some`.
    let name = rest.split(char::is_whitespace).next().unwrap_or("");
    name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

fn reconnect_message(original: &str) -> String {
    // A slash command has to be replayed verbatim: the CLI only intercepts one
    // when the message *is* the command, so a preamble silently demotes
    // `/compact` (or any `.claude/commands` entry) to prose the model merely
    // comments on. The framing below would be wrong for it anyway — a local
    // command repeats no work.
    if is_slash_command(original) {
        return original.to_string();
    }
    format!(
        "[Threadknot automatically replaced a stalled Claude connection before any \
         assistant or tool output was received. Continue the request below now. \
         Inspect current state first and do not repeat completed actions.]\n\n{original}"
    )
}

/// Models with a 1M-token context variant (selected via the `[1m]` model-id suffix).
/// Opus 5 is deliberately absent: its only context window is 1M and its
/// canonical model id must not receive a suffix.
pub const WIDE_CONTEXT_MODELS: &[&str] = &["claude-fable-5", "claude-sonnet-5"];

pub fn builtin_models() -> Vec<ModelInfo> {
    let efforts = || {
        Some(vec![
            "low".into(),
            "medium".into(),
            "high".into(),
            "xhigh".into(),
            "max".into(),
        ])
    };
    let m = |id: &str, name: &str, wide: bool, fixed_context_window: Option<u64>| ModelInfo {
        id: id.into(),
        name: name.into(),
        image: None,
        supports_wide_context: wide.then_some(true),
        fixed_context_window,
        efforts: efforts(),
        default_effort: Some("high".into()),
    };
    let mut models = vec![
        m("claude-fable-5", "Claude Fable 5", true, None),
        m("claude-opus-5", "Claude Opus 5", false, Some(1_000_000)),
        m("claude-sonnet-5", "Claude Sonnet 5", true, None),
    ];
    models.push(ModelInfo {
        id: "claude-haiku-4-5".into(),
        name: "Claude Haiku 4.5".into(),
        image: None,
        supports_wide_context: None,
        fixed_context_window: None,
        efforts: None,
        default_effort: None,
    });
    models
}

pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// Model id passed to `--model`.
///
/// For a Claudex profile the thread's `model` setting is the PROFILE id, and
/// the real model is whatever the gateway calls it — no `[1m]` suffix, which
/// is a Claude-client hint that does not widen a non-Anthropic window.
fn api_model_id(settings: &ThreadSettings, profile: Option<&ClaudexProfile>) -> String {
    if let Some(profile) = profile {
        return profile.model.clone();
    }
    if settings.wide_context && WIDE_CONTEXT_MODELS.contains(&settings.model.as_str()) {
        format!("{}[1m]", settings.model)
    } else {
        settings.model.clone()
    }
}

/// Static context-window fallback. The `[1m]` suffix and fixed-window Opus
/// models select 1M; the authoritative control response supersedes this as
/// soon as it arrives. A Claudex profile states its own upstream window
/// because neither default describes the model behind the gateway.
fn context_window(api_model: &str, profile: Option<&ClaudexProfile>) -> u64 {
    if let Some(window) = profile.and_then(|p| p.context_window) {
        return window;
    }
    if api_model.ends_with("[1m]") || matches!(api_model, "claude-opus-5" | "claude-opus-4-8") {
        1_000_000
    } else {
        200_000
    }
}

/// `--permission-mode` value for the given settings (None = CLI "default").
fn permission_mode(settings: &ThreadSettings) -> Option<&'static str> {
    if settings.mode == Mode::Plan {
        return Some("plan");
    }
    match settings.access {
        Access::Read => None,
        Access::Edits => Some("acceptEdits"),
        Access::Full => Some("bypassPermissions"),
    }
}

/// Mode to restore after a plan is approved (base mode for the access level).
fn base_permission_mode(settings: &ThreadSettings) -> &'static str {
    match settings.access {
        Access::Read => "default",
        Access::Edits => "acceptEdits",
        Access::Full => "bypassPermissions",
    }
}

struct PendingApproval {
    request_id: Value,
    tool_name: String,
    input: Value,
    suggestions: Option<Value>,
}

struct PendingQuestion {
    /// The `can_use_tool` control_request id to answer.
    request_id: Value,
    /// The original `questions` array — echoed back verbatim in updatedInput.
    original: Value,
    /// question text -> multiSelect, so we can shape the answer value.
    multi: HashMap<String, bool>,
}

struct Session {
    stdin: ChildStdin,
    /// Workspace root, so non-image attachments materialized here can be
    /// referenced by relative path in the prompt.
    cwd: String,
    control_seq: u64,
    current_model: String,
    current_mode: String,
    /// Marks the next result frame as a user cancellation. Claude reports an
    /// interrupt issued during `can_use_tool` as `error_during_execution`
    /// without an explanatory result string, so payload text alone is not
    /// enough to distinguish it from a real failure.
    interrupt_requested: bool,
    /// call_id -> tool name, for labeling tool_result events.
    tool_names: HashMap<String, String>,
    /// Input-side context tokens of the most recent main-thread API call. The
    /// `result` frame's `usage` is summed across every API call in
    /// the turn (cache reads counted once per step), so it wildly overstates
    /// context; only a single call's input-side usage reflects the live window.
    last_context_tokens: Option<u64>,
    /// Current model's denominator. Never derive this by taking the maximum of
    /// session-cumulative `modelUsage`, which makes a prior 1M model sticky.
    context_window: u64,
    /// Avoid persisting identical snapshots repeated by message_delta and the
    /// corresponding assistant frame.
    last_emitted_context: Option<(u64, u64)>,
    /// In-flight authoritative `get_context_usage` control request.
    pending_context_request: Option<String>,
    /// A newer lifecycle/model change asked for a refresh while one was in
    /// flight. Send it after the current response so stale model data cannot
    /// become the final snapshot.
    context_refresh_queued: bool,
    pending: HashMap<String, PendingApproval>,
    pending_questions: HashMap<String, PendingQuestion>,
    /// Detects the true end of a turn when background subagents are running.
    burst: BurstTracker,
    /// Debounced turn completion. When a turn that used background agents winds
    /// down, hold `(deadline, usage)` briefly and fire exactly one
    /// `TurnCompleted` once the stream goes quiet — coalescing any back-to-back
    /// completion results and never hanging on a wake miscount.
    pending_finalize: Option<(Instant, Usage)>,
    /// tool_use_id -> task_id for every launched subagent, so inline subagent
    /// frames (carrying `parent_tool_use_id`) can be attributed to their task.
    tool_use_to_task: HashMap<String, String>,
    /// task_ids that are actual subagents (`task_type == "local_agent"`), so the
    /// completion notification only surfaces a `SubagentCompleted` for those.
    /// Background *tool* tasks (e.g. `local_bash`) also flow through the task
    /// lifecycle but are NOT subagents — they stay ordinary tool cards.
    subagent_task_ids: HashSet<String>,
    /// Session id last announced via `SessionStarted`. Each background wake
    /// re-emits `system/init` with the SAME id; dedupe so the chat isn't spammed
    /// with "session started" notes mid-burst.
    announced_session: Option<String>,
    /// Whether a turn is live from this driver's point of view. Guards the
    /// steer path: the hub's busy check races the terminal `result` frame, so a
    /// `Steer` that lands after the turn ended is promoted to a fresh turn
    /// instead of injecting into a CLI that's idle.
    turn_active: bool,
}

/// Tracks background subagent activity within a turn. A background agent
/// finishing injects a follow-up "turn" whose `result` is tagged
/// `origin.kind = task-notification`, and several agents can finish close enough
/// that their completions arrive back-to-back. Rather than trying to match each
/// completion `result` to a departed task 1:1 — a count that silently breaks
/// when completions coalesce and hangs the turn forever — we hold the turn
/// Running while any agent is outstanding, then debounce the end (see
/// [`Session::pending_finalize`] / [`FINALIZE_GRACE`]).
#[derive(Default)]
struct BurstTracker {
    /// Live set of outstanding background task ids — authoritative, straight
    /// from `background_tasks_changed`.
    bg_tasks: HashSet<String>,
    /// Whether this turn ever spawned a background agent. Gates the finalize
    /// debounce so plain turns (the common case) end with no added latency.
    saw_background: bool,
}

impl BurstTracker {
    /// Reconcile the live background-task set.
    fn set_background(&mut self, ids: HashSet<String>) {
        if !ids.is_empty() {
            self.saw_background = true;
        }
        self.bg_tasks = ids;
    }

    fn is_background(&self, task_id: &str) -> bool {
        self.bg_tasks.contains(task_id)
    }

    fn outstanding(&self) -> usize {
        self.bg_tasks.len()
    }

    fn saw_background(&self) -> bool {
        self.saw_background
    }

    /// Start of a fresh user turn: forget prior background history, but keep any
    /// genuinely still-outstanding agents authoritative.
    fn begin_turn(&mut self) {
        self.saw_background = !self.bg_tasks.is_empty();
    }

    fn reset(&mut self) {
        self.bg_tasks.clear();
        self.saw_background = false;
    }
}

impl Session {
    async fn write(&mut self, frame: &Value) -> Result<()> {
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .context("write to claude stdin")?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn control_with_id(&mut self, request: Value) -> Result<String> {
        self.control_seq += 1;
        let request_id = format!("threadknot-{}", self.control_seq);
        let frame = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        });
        self.write(&frame).await?;
        Ok(request_id)
    }

    async fn control(&mut self, request: Value) -> Result<()> {
        self.control_with_id(request).await.map(|_| ())
    }

    async fn request_context_usage(&mut self) -> Result<()> {
        if self.pending_context_request.is_some() {
            self.context_refresh_queued = true;
            return Ok(());
        }
        let request_id = self
            .control_with_id(json!({ "subtype": "get_context_usage" }))
            .await?;
        self.pending_context_request = Some(request_id);
        Ok(())
    }

    async fn control_response(&mut self, request_id: &Value, response: Value) -> Result<()> {
        let frame = json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        });
        self.write(&frame).await
    }

    async fn user_message(&mut self, text: &str, attachments: &[AttachmentRef]) -> Result<()> {
        // Non-image files are copied into the workspace and referenced by path;
        // Claude opens them with its Read tool (handles PDF/text/csv/etc.).
        let docs = super::materialize_docs(&self.cwd, attachments);
        let text = format!("{text}{}", super::attachment_footer(&docs));
        // A text block only when the text has meaningful (non-whitespace)
        // content: an empty one makes Anthropic reject the whole request with
        // "text content blocks must be non-empty", and the CLI replays it on
        // every resume. Image-only messages send just their image block.
        let mut content = Vec::new();
        if !text.trim().is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        for att in attachments {
            if let Some(block) = image_block(att) {
                content.push(block);
            }
        }
        // Final guard before the request leaves Threadknot: strip any empty text
        // block that slipped through and reject a message with no valid blocks
        // locally, rather than letting Anthropic 400 on a replayed transcript.
        let content = super::content::sanitize_user_content(
            &super::content::SanitizeCtx {
                provider: "claude",
                model: &self.current_model,
                attachment_count: attachments.len(),
            },
            content,
        )?;
        let frame = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "parent_tool_use_id": null,
            "session_id": "",
        });
        self.write(&frame).await
    }
}

/// Read a stored image attachment into a Claude base64 image content block.
/// Non-image or unsupported mime types are skipped (returns None).
fn image_block(att: &AttachmentRef) -> Option<Value> {
    const SUPPORTED: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
    if !SUPPORTED.contains(&att.mime_type.as_str()) {
        return None;
    }
    let bytes = std::fs::read(&att.path).ok()?;
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(json!({
        "type": "image",
        "source": { "type": "base64", "media_type": att.mime_type, "data": data },
    }))
}

/// Inline `--mcp-config` JSON wiring the agent to Threadknot's browser tools over
/// streamable HTTP, authenticated with the thread's bearer token — plus every
/// enabled server from the user's Library (`library.rs`).
///
/// The browser entry is written first and the library cannot shadow it: the
/// registry refuses `threadknot-browser` as a name, so no install can cost a thread
/// its browser.
fn mcp_config_json(endpoint: &str, token: &str, library: &[crate::library::McpServer]) -> String {
    let mut servers = serde_json::Map::new();
    servers.insert(
        crate::library::RESERVED_MCP_NAME.into(),
        json!({
            "type": "http",
            "url": endpoint,
            "headers": { "Authorization": format!("Bearer {token}") }
        }),
    );
    for server in library {
        servers.insert(server.name.clone(), crate::library::claude_entry(server));
    }
    json!({ "mcpServers": servers }).to_string()
}

#[cfg(test)]
fn command_args(
    settings: &ThreadSettings,
    profile: Option<&ClaudexProfile>,
    resume_session_id: Option<&str>,
    mcp_endpoint: &str,
    mcp_token: &str,
    library: &[crate::library::McpServer],
) -> Vec<String> {
    command_args_with_mcp_config(
        settings,
        profile,
        resume_session_id,
        mcp_config_json(mcp_endpoint, mcp_token, library),
    )
}

fn command_args_with_mcp_config(
    settings: &ThreadSettings,
    profile: Option<&ClaudexProfile>,
    resume_session_id: Option<&str>,
    mcp_config: String,
) -> Vec<String> {
    let model = api_model_id(settings, profile);
    let mut args = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
        "--setting-sources",
        "user,project,local",
        "--model",
        &model,
        // This only permits a later control-channel switch to
        // `bypassPermissions`; it does not bypass anything unless full access
        // is actually selected. Without it, a session started in a safer mode
        // silently keeps prompting after the user promotes it to full access.
        "--allow-dangerously-skip-permissions",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    // Wire the agent-driven browser MCP server (see mcp.rs) plus the Library.
    args.extend(["--mcp-config".into(), mcp_config]);
    if let Some(mode) = permission_mode(settings) {
        args.extend(["--permission-mode".into(), mode.into()]);
    }
    if let Some(effort) = &settings.effort {
        args.extend(["--effort".into(), effort.clone()]);
    }
    // Claudex uses the same CLI harness, but its alternate backend is not a
    // native Claude session and must not inherit Anthropic's Chrome bridge.
    if profile.is_none() && settings.claude_chrome {
        args.push("--chrome".into());
    }
    if let Some(sid) = resume_session_id {
        args.extend(["--resume".into(), sid.into()]);
    }
    args
}

fn spawn_claude(
    ctx: &DriverCtx,
    settings: &ThreadSettings,
    resume_session_id: Option<&str>,
) -> Result<SpawnedClaude> {
    let profile = ctx.claudex.as_ref();
    let model = api_model_id(settings, profile);
    let model_context_window = context_window(&model, profile);
    let bin = super::resolve_bin("claude")
        .ok_or_else(|| anyhow::anyhow!("claude CLI not found on PATH"))?;
    // A bridged profile keeps its own CLAUDE_CONFIG_DIR: its transcripts,
    // session ids and (absent) login all live there, apart from ~/.claude.
    let config_dir = profile.map(|p| p.config_dir(ctx.hub.store.dir()));
    if let Some(dir) = &config_dir {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create Claudex config dir {}", dir.display()))?;
    }
    if let Some(session_id) = resume_session_id {
        let repair = match &config_dir {
            Some(dir) => super::repair::repair_session_transcript_in(dir, session_id),
            None => super::repair::repair_session_transcript(session_id),
        };
        match repair {
            Ok(Some(report)) if report.changed() => {
                tracing::warn!(
                    session_id,
                    records = report.records_repaired,
                    blocks = report.blocks_removed,
                    "repaired Claude transcript before resume"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    session_id,
                    %error,
                    "could not inspect Claude transcript before resume"
                );
            }
        }
    }
    let mcp_json = mcp_config_json(
        &ctx.mcp_endpoint,
        &ctx.mcp_token,
        &ctx.hub.library.for_agent(ctx.agent),
    );
    let (mcp_config, mcp_config_lease) = mcp_config_for_spawn(mcp_json)?;

    let mut cmd = Command::new(bin);
    cmd.env("PATH", super::agent_path());
    if let Some(profile) = profile {
        for (name, value) in profile.env(ctx.hub.store.dir()) {
            cmd.env(name, value);
        }
    }
    cmd.args(command_args_with_mcp_config(
        settings,
        profile,
        resume_session_id,
        mcp_config,
    ));
    super::no_console(&mut cmd);
    let mode = permission_mode(settings);
    let mut child = cmd
        .current_dir(&ctx.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Capture stderr (rather than discarding it) so a startup failure is
        // reportable instead of presenting as an eternal "working" spinner.
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn `claude` — is Claude Code installed and on PATH?")?;

    let stdout = child.stdout.take().expect("claude stdout");
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });

    // Drain stderr into a bounded ring buffer; also mirror to the tracing log so
    // the detail is captured even for a session that ends some other way.
    let stderr_tail: StderrTail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let stderr_task = {
        let tail = Arc::clone(&stderr_tail);
        let stderr = child.stderr.take();
        tokio::spawn(async move {
            let Some(stderr) = stderr else { return };
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "claude_stderr", "{line}");
                let mut buf = tail.lock().unwrap();
                if buf.len() == STDERR_TAIL_LINES {
                    buf.pop_front();
                }
                buf.push_back(line);
            }
        })
    };

    let session = Session {
        stdin: child.stdin.take().expect("claude stdin"),
        cwd: ctx.cwd.clone(),
        control_seq: 0,
        current_model: model,
        current_mode: mode.unwrap_or("default").to_string(),
        interrupt_requested: false,
        tool_names: HashMap::new(),
        last_context_tokens: None,
        context_window: model_context_window,
        last_emitted_context: None,
        pending_context_request: None,
        context_refresh_queued: false,
        pending: HashMap::new(),
        pending_questions: HashMap::new(),
        burst: BurstTracker::default(),
        pending_finalize: None,
        tool_use_to_task: HashMap::new(),
        subagent_task_ids: HashSet::new(),
        announced_session: None,
        // Every spawn is immediately followed by a user message that opens a
        // turn (first message, or the mid-turn reconnect replay).
        turn_active: true,
    };
    Ok((
        child,
        session,
        rx,
        stderr_tail,
        stderr_task,
        mcp_config_lease,
    ))
}

pub async fn run(ctx: DriverCtx, mut cmd_rx: mpsc::UnboundedReceiver<AgentCommand>) -> Result<()> {
    run_with_policy(&ctx, &mut cmd_rx, DriverPolicy::production()).await
}

async fn run_with_policy(
    ctx: &DriverCtx,
    cmd_rx: &mut mpsc::UnboundedReceiver<AgentCommand>,
    policy: DriverPolicy,
) -> Result<()> {
    // Wait for the first user message before spawning so the process always has work.
    let first = loop {
        match cmd_rx.recv().await {
            Some(AgentCommand::User {
                text,
                settings,
                attachments,
            }) => break (text, settings, attachments),
            Some(AgentCommand::Retire) => return Ok(()),
            Some(_) => continue,
            None => return Ok(()),
        }
    };

    // A Claudex thread whose profile no longer resolves (deleted, or never
    // picked) must fail loudly. Falling through would spawn an ordinary
    // Anthropic-authenticated Claude — the user's real plan, silently billed,
    // under an agent that says it is running something else.
    anyhow::ensure!(
        ctx.agent != Agent::Claudex || ctx.claudex.is_some(),
        "this chat's Claudex profile no longer exists — pick one in the composer"
    );

    // A bridged profile is only usable once something answers at its base URL.
    // Do this after the first message (same point the CLI is spawned) so an
    // idle picker selection never starts a process.
    if let Some(profile) = &ctx.claudex {
        ctx.emit(AgentEvent::Status {
            text: format!("Connecting to {} via {}…", profile.model, profile.name),
        });
        ctx.hub
            .claudex_sidecars
            .ensure(profile)
            .await
            .with_context(|| format!("Claudex profile \"{}\" is unavailable", profile.name))?;
    }

    let mut active_text = super::transcript::seeded_message(ctx.seed.as_deref(), &first.0);
    let mut active_attachments = first.2;
    let (
        mut child,
        mut session,
        mut out_rx,
        mut stderr_tail,
        mut stderr_task,
        mut _mcp_config_lease,
    ) = spawn_claude(ctx, &first.1, ctx.resume_session_id.as_deref())?;
    // A handoff seed (mid-thread agent switch) rides in the first message: a
    // standalone user frame would itself start a turn.
    session
        .user_message(&active_text, &active_attachments)
        .await?;
    ctx.emit(AgentEvent::TurnStarted {
        agent: Some(ctx.agent),
        model: Some(first.1.model.clone()),
    });
    // Ask Claude Code for its own meter value as soon as it has the first user
    // message. Streaming usage remains the fallback if this optional control
    // request is unavailable in a future CLI version.
    if let Err(error) = session.request_context_usage().await {
        tracing::debug!("Claude context query failed: {error:#}");
    }
    let mut current_settings = first.1;
    let mut watchdog = FirstResponseWatchdog::new(policy);
    watchdog.start_turn(Instant::now());

    loop {
        // Copy the deadline out so the timer future borrows nothing from
        // `session` (the branch handlers need `&mut session`).
        let finalize_deadline = session.pending_finalize.as_ref().map(|(d, _)| *d);
        let response_deadline = watchdog.deadline();
        tokio::select! {
            _ = async {
                match finalize_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    // No finalize pending: never resolve this branch.
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Some((_, usage)) = session.pending_finalize.take() {
                    session.turn_active = false;
                    ctx.emit(AgentEvent::TurnCompleted { usage: Some(usage) });
                    session.burst.reset();
                }
            }
            _ = async {
                match response_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                match watchdog.timed_out(Instant::now()) {
                    StallAction::Reconnect => {
                        ctx.emit(AgentEvent::Status {
                            text: "Claude stopped responding before it began work — reconnecting automatically…".into(),
                        });

                        // Best effort only: the exact failure this guards can
                        // prevent the CLI from processing its control channel.
                        let _ = tokio::time::timeout(
                            Duration::from_millis(500),
                            interrupt_session(ctx, &mut session),
                        )
                        .await;
                        retire_child(&mut child, &mut stderr_task).await;

                        let resume_session_id = ctx
                            .hub
                            .store
                            .thread(&ctx.thread_id)
                            .and_then(|thread| {
                                thread
                                    .session_anchors
                                    .get(&ctx.participant_id)
                                    .map(|anchor| anchor.session_id.clone())
                            })
                            .or_else(|| ctx.resume_session_id.clone());
                        (
                            child,
                            session,
                            out_rx,
                            stderr_tail,
                            stderr_task,
                            _mcp_config_lease,
                        ) = spawn_claude(
                            ctx,
                            &current_settings,
                            resume_session_id.as_deref(),
                        )?;
                        session
                            .user_message(
                                &reconnect_message(&active_text),
                                &active_attachments,
                            )
                            .await?;
                        ctx.emit(AgentEvent::Status {
                            text: "Claude reconnected; continuing the same request".into(),
                        });
                        if let Err(error) = session.request_context_usage().await {
                            tracing::debug!("Claude context query after reconnect failed: {error:#}");
                        }
                    }
                    StallAction::Fail => {
                        cmd_rx.close();
                        retire_child(&mut child, &mut stderr_task).await;
                        anyhow::bail!(
                            "Claude did not respond after Threadknot automatically reconnected once. \
                             The unresponsive process was retired; send the message again to retry. \
                             stderr:\n{}",
                            stderr_summary(&stderr_tail)
                        );
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCommand::User { text, settings, attachments } => {
                        apply_settings_change(ctx, &mut session, &settings).await?;
                        current_settings = settings;
                        active_text = text;
                        active_attachments = attachments;
                        // Fresh turn: drop any stale interrupt flag / pending
                        // finalize so a leftover from a prior turn can't swallow
                        // or prematurely end this one.
                        session.interrupt_requested = false;
                        session.pending_finalize = None;
                        session.burst.begin_turn();
                        session.turn_active = true;
                        session
                            .user_message(&active_text, &active_attachments)
                            .await?;
                        watchdog.start_turn(Instant::now());
                        ctx.emit(AgentEvent::TurnStarted {
                            agent: Some(ctx.agent),
                            model: Some(current_settings.model.clone()),
                        });
                        if let Err(error) = session.request_context_usage().await {
                            tracing::debug!("Claude context query failed: {error:#}");
                        }
                    }
                    AgentCommand::Steer { text } => {
                        if session.turn_active {
                            // Injecting continues the turn: a pending
                            // background-debounce finalize must not fire 750ms
                            // later while the CLI works on the note — the
                            // continuation's own result re-runs turn-end logic.
                            // If the CLI never answers the note, the thread
                            // stays Running with no watchdog; Stop is the
                            // escape hatch. Never re-arm the watchdog here:
                            // its stall replay is only safe pre-first-output.
                            session.pending_finalize = None;
                            // Keep the note in the reconnect replay text so the
                            // one pre-first-output reconnect can't drop it.
                            active_text.push_str(&format!("\n\n[Note added mid-turn]: {text}"));
                            session.user_message(&text, &[]).await?;
                        } else {
                            // The turn's result beat the hub's busy check:
                            // promote the note to a fresh turn (same settings).
                            active_text = text;
                            active_attachments = Vec::new();
                            session.interrupt_requested = false;
                            session.pending_finalize = None;
                            session.burst.begin_turn();
                            session.turn_active = true;
                            session.user_message(&active_text, &[]).await?;
                            watchdog.start_turn(Instant::now());
                            ctx.emit(AgentEvent::TurnStarted {
                                agent: Some(ctx.agent),
                                model: Some(current_settings.model.clone()),
                            });
                        }
                    }
                    AgentCommand::Settings { settings } => {
                        apply_settings_change(ctx, &mut session, &settings).await?;
                        current_settings = settings;
                    }
                    AgentCommand::Retire => {
                        retire_child(&mut child, &mut stderr_task).await;
                        return Ok(());
                    }
                    AgentCommand::Interrupt => {
                        // Stop is also a health boundary: never feed a new turn
                        // into a process that may still be blocked on the old
                        // request. Close the receiver first so session_cmd_tx()
                        // sees the handle as dead and can safely spawn a fresh
                        // process as soon as TurnAborted makes the thread idle.
                        session.pending_finalize = None;
                        let _ = tokio::time::timeout(
                            Duration::from_millis(500),
                            interrupt_session(ctx, &mut session),
                        )
                        .await;
                        session.burst.reset();
                        cmd_rx.close();
                        let _ = tokio::time::timeout(policy.interrupt_grace, async {
                            while let Some(frame) = out_rx.recv().await {
                                if frame.get("type").and_then(Value::as_str) == Some("result") {
                                    break;
                                }
                            }
                        })
                        .await;
                        retire_child(&mut child, &mut stderr_task).await;
                        ctx.emit(AgentEvent::TurnAborted);
                        return Ok(());
                    }
                    AgentCommand::Approval { approval_id, option_id } => {
                        handle_approval(ctx, &mut session, &current_settings, approval_id, option_id).await?;
                    }
                    AgentCommand::Question { request_id, answers } => {
                        handle_question_answer(ctx, &mut session, request_id, answers).await?;
                    }
                }
            }
            msg = out_rx.recv() => {
                match msg {
                    Some(v) => {
                        if is_provider_progress(&v) {
                            watchdog.provider_progress();
                        } else if is_compaction_start(&v) {
                            watchdog.compacting(Instant::now());
                        }
                        handle_message(ctx, &mut session, v).await?
                    },
                    // stdout closed: the CLI exited. Surface *why* — exit status
                    // plus the stderr tail — instead of a bare "exited", so an
                    // outdated CLI (rejecting a flag like `--effort`), a
                    // node-version abort, or an auth failure is legible in the UI
                    // rather than a spinner that never resolves.
                    None => {
                        let status = child.wait().await.ok();
                        // Let the stderr drainer finish (it hits EOF once the
                        // process is gone) so the tail we report is complete.
                        let _ = tokio::time::timeout(
                            Duration::from_millis(500),
                            stderr_task,
                        )
                        .await;
                        let code = status
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "unknown status".to_string());
                        anyhow::bail!(
                            "claude exited ({code}) without finishing the turn — \
                             this usually means the CLI is outdated or misconfigured. \
                             Try `claude update`. stderr:\n{}",
                            stderr_summary(&stderr_tail)
                        );
                    }
                }
            }
        }
    }

    let _ = child.kill().await;
    Ok(())
}

async fn retire_child(child: &mut Child, stderr_task: &mut tokio::task::JoinHandle<()>) {
    let _ = child.kill().await;
    let _ = child.wait().await;
    stderr_task.abort();
}

async fn apply_settings_change(
    ctx: &DriverCtx,
    session: &mut Session,
    settings: &ThreadSettings,
) -> Result<()> {
    let model = api_model_id(settings, ctx.claudex.as_ref());
    if model != session.current_model {
        session
            .control(json!({ "subtype": "set_model", "model": model }))
            .await?;
        session.current_model = model;
        session.context_window = context_window(&session.current_model, ctx.claudex.as_ref());
        emit_last_context(ctx, session);
        if let Err(error) = session.request_context_usage().await {
            tracing::debug!("Claude context query after model change failed: {error:#}");
        }
    }
    let mode = permission_mode(settings).unwrap_or("default");
    if mode != session.current_mode {
        session
            .control(json!({ "subtype": "set_permission_mode", "mode": mode }))
            .await?;
        session.current_mode = mode.to_string();
    }
    Ok(())
}

/// Active context from one Claude API response. Claude Code's statusline uses
/// the input side of the latest response: uncached input + cache writes + cache
/// reads. Output is turn accounting, not part of this fallback numerator.
fn active_context_tokens(usage: &Value) -> Option<u64> {
    let keys = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ];
    let seen = keys
        .iter()
        .any(|key| usage.get(key).and_then(Value::as_u64).is_some());
    seen.then(|| {
        keys.iter()
            .filter_map(|key| usage.get(key).and_then(Value::as_u64))
            .sum()
    })
}

fn context_usage(used: u64, max: u64) -> Usage {
    Usage {
        used_tokens: Some(used),
        max_tokens: Some(max),
        context_pct: (max > 0).then(|| used as f64 / max as f64 * 100.0),
        ..Usage::default()
    }
}

/// The context window to trust.
///
/// A Claudex profile's stated window is authoritative: the CLI reports an
/// Anthropic-shaped guess for a model it has never seen (200K), and the gateway
/// does not correct it — verified live, where a 272K Sol profile's ring
/// reverted to 200K the moment the first usage report arrived.
fn effective_window(profile: Option<&ClaudexProfile>, reported: u64) -> u64 {
    profile
        .and_then(|profile| profile.context_window)
        .unwrap_or(reported)
}

fn emit_context(ctx: &DriverCtx, session: &mut Session, used: u64, max: u64) {
    // Every path in — control responses, streaming usage, compaction, model
    // changes — funnels through here, so this is the one place to pin it.
    let max = effective_window(ctx.claudex.as_ref(), max);
    if max == 0 {
        return;
    }
    session.last_context_tokens = Some(used);
    session.context_window = max;
    if session.last_emitted_context == Some((used, max)) {
        return;
    }
    session.last_emitted_context = Some((used, max));
    ctx.emit(AgentEvent::ContextUsage {
        usage: context_usage(used, max),
    });
}

fn emit_last_context(ctx: &DriverCtx, session: &mut Session) {
    if let Some(used) = session.last_context_tokens {
        emit_context(ctx, session, used, session.context_window);
    }
}

/// `modelUsage` is cumulative for the session. Select the current model only;
/// taking the maximum across all entries makes a previous 1M model permanent.
fn reported_context_window(result: &Value, current_model: &str) -> Option<u64> {
    let models = result.get("modelUsage")?.as_object()?;
    let base_model = current_model.strip_suffix("[1m]").unwrap_or(current_model);
    models
        .get(current_model)
        .or_else(|| models.get(base_model))
        .and_then(|usage| usage.get("contextWindow"))
        .and_then(Value::as_u64)
        .or_else(|| {
            (models.len() == 1)
                .then(|| models.values().next())
                .flatten()
                .and_then(|usage| usage.get("contextWindow"))
                .and_then(Value::as_u64)
        })
}

fn compact_post_tokens(message: &Value) -> Option<u64> {
    message
        .pointer("/compact_metadata/post_tokens")
        .or_else(|| message.pointer("/compactMetadata/postTokens"))
        .and_then(Value::as_u64)
}

fn control_context_usage(message: &Value) -> Option<(u64, u64)> {
    let response = message.get("response")?;
    if response.get("subtype").and_then(Value::as_str) != Some("success") {
        return None;
    }
    let usage = response.get("response")?;
    Some((
        usage.get("totalTokens")?.as_u64()?,
        usage.get("maxTokens")?.as_u64()?,
    ))
}

fn same_model(reported: &str, current: &str) -> bool {
    let current = current.strip_suffix("[1m]").unwrap_or(current);
    reported == current
        || reported
            .strip_prefix(current)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

/// Claude waits synchronously for a response to `can_use_tool`. An interrupt
/// control request alone is not processed while that response is outstanding,
/// so cancel pending UI requests first and then interrupt the turn.
async fn interrupt_session(ctx: &DriverCtx, session: &mut Session) -> Result<()> {
    session.interrupt_requested = true;
    let approvals = session.pending.drain().collect::<Vec<_>>();
    for (approval_id, pending) in approvals {
        session
            .control_response(
                &pending.request_id,
                json!({ "behavior": "deny", "message": "User interrupted the turn." }),
            )
            .await?;
        ctx.emit(AgentEvent::ApprovalResolved {
            approval_id,
            option_id: "cancel".into(),
        });
    }

    let questions = session.pending_questions.drain().collect::<Vec<_>>();
    for (request_id, pending) in questions {
        session
            .control_response(
                &pending.request_id,
                json!({ "behavior": "deny", "message": "User interrupted the turn." }),
            )
            .await?;
        ctx.emit(AgentEvent::QuestionResolved {
            request_id,
            answers: None,
        });
    }

    session.control(json!({ "subtype": "interrupt" })).await
}

fn tool_detail(name: &str, input: &Value) -> String {
    let get = |k: &str| input.get(k).and_then(|v| v.as_str()).map(String::from);
    match name {
        "Bash" => get("command").unwrap_or_default(),
        "Edit" | "Write" | "Read" | "NotebookEdit" => get("file_path").unwrap_or_default(),
        "Grep" | "Glob" => get("pattern").unwrap_or_default(),
        "WebFetch" | "WebSearch" => get("url").or_else(|| get("query")).unwrap_or_default(),
        "Task" => get("description").unwrap_or_default(),
        _ => {
            let s = input.to_string();
            if s.len() > 200 {
                format!("{}…", &s[..200])
            } else {
                s
            }
        }
    }
}

fn approval_kind(tool_name: &str) -> &'static str {
    match tool_name {
        "Bash" => "exec",
        "Edit" | "Write" | "NotebookEdit" => "patch",
        _ => "tool",
    }
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

/// Whether a `task_started` frame is a real subagent (`task_type == "local_agent"`)
/// rather than a background *tool* task (`local_bash`, …). The task lifecycle
/// fires for both; only agents belong in the subagent UI.
fn is_subagent_task(v: &Value) -> bool {
    str_field(v, "task_type") == "local_agent"
}

/// Task ids in a `background_tasks_changed` frame — the live set of outstanding
/// background subagents.
fn background_task_ids(v: &Value) -> HashSet<String> {
    v.get("tasks")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("task_id").and_then(|s| s.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Reconcile the authoritative live set of background subagents. While any agent
/// is outstanding the turn is held Running; a fresh non-empty set also cancels a
/// pending finalize (more work arrived during the grace window).
fn track_background_tasks(session: &mut Session, v: &Value) {
    let ids = background_task_ids(v);
    if !ids.is_empty() {
        session.pending_finalize = None;
    }
    session.burst.set_background(ids);
}

/// A subagent (synchronous `Task` or background `Agent`) started: record the
/// tool_use → task mapping so its inline frames can be attributed, and surface
/// a card. `background` is inferred from whether it is in the live background
/// set (populated by [`track_background_tasks`], which precedes this event).
fn handle_task_started(ctx: &DriverCtx, session: &mut Session, v: &Value) {
    // Only real subagents are surfaced in the subagent UI. The task lifecycle
    // also fires for background *tool* tasks (`local_bash`, …); those are not
    // agents — they remain ordinary tool cards and are counted only by the
    // turn-end debounce, never in the agent HUD.
    if !is_subagent_task(v) {
        return;
    }
    let task_id = str_field(v, "task_id");
    if task_id.is_empty() {
        return;
    }
    session.subagent_task_ids.insert(task_id.clone());
    let tool_use_id = str_field(v, "tool_use_id");
    if !tool_use_id.is_empty() {
        session
            .tool_use_to_task
            .insert(tool_use_id.clone(), task_id.clone());
    }
    let background = session.burst.is_background(&task_id);
    ctx.emit(AgentEvent::SubagentStarted {
        task_id,
        tool_use_id,
        description: str_field(v, "description"),
        subagent_type: str_field(v, "subagent_type"),
        background,
        prompt: None,
        // A provider's own subagent runs in this process on this machine, so
        // none of the dispatch attribution applies.
        dispatch: None,
    });
}

/// A subagent finished — surface its status + one-line summary. Fires for both
/// synchronous and background tasks; background-task accounting for turn-end
/// detection is handled separately by [`track_background_tasks`].
fn handle_task_notification(ctx: &DriverCtx, session: &mut Session, v: &Value) {
    let task_id = str_field(v, "task_id");
    // Only surface completion for tasks we surfaced as subagents (local_agent).
    if !session.subagent_task_ids.remove(&task_id) {
        return;
    }
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("completed")
        .to_string();
    let summary = v
        .get("summary")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    ctx.emit(AgentEvent::SubagentCompleted {
        task_id,
        status,
        summary,
    });
}

/// Attribute a synchronous subagent's own assistant text / thinking / tool use
/// (frames carrying `parent_tool_use_id`) to its task, as a live progress feed.
fn emit_subagent_progress(ctx: &DriverCtx, session: &Session, v: &Value) {
    let Some(parent) = v.get("parent_tool_use_id").and_then(|p| p.as_str()) else {
        return;
    };
    let Some(task_id) = session.tool_use_to_task.get(parent) else {
        return;
    };
    let blocks = v
        .pointer("/message/content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    for block in blocks {
        let (kind, text) = match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" => ("text", str_field(&block, "text")),
            "thinking" => ("thinking", str_field(&block, "thinking")),
            "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let input = block.get("input").cloned().unwrap_or(json!({}));
                let detail = tool_detail(name, &input);
                (
                    "tool",
                    if detail.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name}: {detail}")
                    },
                )
            }
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        ctx.emit(AgentEvent::SubagentProgress {
            task_id: task_id.clone(),
            activity: kind.to_string(),
            text,
        });
    }
}

async fn handle_message(ctx: &DriverCtx, session: &mut Session, v: Value) -> Result<()> {
    let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    // Skip subagent traffic (surfaces via the Task tool result instead).
    let from_subagent = v
        .get("parent_tool_use_id")
        .map(|p| !p.is_null())
        .unwrap_or(false);

    match msg_type {
        "system" if !from_subagent => match v.get("subtype").and_then(|s| s.as_str()).unwrap_or("")
        {
            "init" => {
                // A fresh `init` means another turn is starting right now — for
                // background bursts, the CLI re-inits within ~10ms of each
                // wake's result. That is the definitive "the turn is not over"
                // signal, so cancel any armed finalize; the next result re-arms
                // it, and only the LAST result (no init follows) survives the
                // grace window to fire a single TurnCompleted.
                session.pending_finalize = None;
                if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
                    // Each background wake re-emits init with the same session
                    // id; announce a session only once per id so the chat isn't
                    // spammed mid-burst.
                    if session.announced_session.as_deref() != Some(sid) {
                        session.announced_session = Some(sid.to_string());
                        ctx.emit(AgentEvent::SessionStarted {
                            provider_session_id: sid.to_string(),
                            model: v
                                .get("model")
                                .and_then(|m| m.as_str())
                                .unwrap_or(&session.current_model)
                                .to_string(),
                            agent: Some(ctx.agent),
                        });
                    }
                }
            }
            "background_tasks_changed" => track_background_tasks(session, &v),
            "task_started" => handle_task_started(ctx, session, &v),
            "task_notification" => handle_task_notification(ctx, session, &v),
            "compact_boundary" => {
                if let Some(post_tokens) = compact_post_tokens(&v) {
                    emit_context(ctx, session, post_tokens, session.context_window);
                }
                ctx.emit(AgentEvent::Status {
                    text: "Context compacted".into(),
                });
            }
            "status" => {
                if let Some(s) = v.get("status").and_then(|s| s.as_str()) {
                    ctx.emit(AgentEvent::Status {
                        text: s.to_string(),
                    });
                }
            }
            _ => {}
        },
        "stream_event" if !from_subagent => {
            if let Some(event) = v.get("event") {
                match event.get("type").and_then(|t| t.as_str()) {
                    Some("message_delta") => {
                        if let Some(used) = event.get("usage").and_then(active_context_tokens) {
                            emit_context(ctx, session, used, session.context_window);
                        }
                    }
                    Some("content_block_delta") => {
                        match event.pointer("/delta/type").and_then(|t| t.as_str()) {
                            Some("text_delta") => {
                                if let Some(text) =
                                    event.pointer("/delta/text").and_then(|t| t.as_str())
                                {
                                    ctx.emit(AgentEvent::AssistantDelta { text: text.into() });
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(text) =
                                    event.pointer("/delta/thinking").and_then(|t| t.as_str())
                                {
                                    ctx.emit(AgentEvent::ThinkingDelta { text: text.into() });
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        "assistant" if !from_subagent => {
            if let Some(u) = v.pointer("/message/usage") {
                if let Some(used) = active_context_tokens(u) {
                    emit_context(ctx, session, used, session.context_window);
                }
            }
            let blocks = v
                .pointer("/message/content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                ctx.emit(AgentEvent::AssistantMessage { text: text.into() });
                            }
                        }
                    }
                    "thinking" => {
                        if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                ctx.emit(AgentEvent::Thinking { text: text.into() });
                            }
                        }
                    }
                    "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                        let call_id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let input = block.get("input").cloned().unwrap_or(json!({}));
                        session.tool_names.insert(call_id.clone(), name.clone());
                        ctx.emit(AgentEvent::ToolStart {
                            call_id,
                            detail: tool_detail(&name, &input),
                            name,
                        });
                    }
                    _ => {}
                }
            }
        }
        // Inline subagent activity (synchronous `Task`): attribute the
        // subagent's own assistant text / tool use to its task so the UI can
        // show a live nested feed instead of a silent "Agent" card.
        "assistant" => emit_subagent_progress(ctx, session, &v),
        "user" if !from_subagent => {
            let blocks = v
                .pointer("/message/content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    let call_id = block
                        .get("tool_use_id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = session
                        .tool_names
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "tool".into());
                    let output = block.get("content").map(result_text).unwrap_or_default();
                    let is_error = block
                        .get("is_error")
                        .and_then(|e| e.as_bool())
                        .unwrap_or(false);
                    ctx.emit(AgentEvent::ToolEnd {
                        call_id,
                        name,
                        output: (!output.is_empty()).then(|| {
                            if output.len() > 20_000 {
                                format!("{}…", &output[..20_000])
                            } else {
                                output
                            }
                        }),
                        is_error,
                        truncated: false,
                    });
                }
            }
        }
        "result" => {
            let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
            let errors_text = v.get("result").and_then(|r| r.as_str()).unwrap_or("");
            let interrupt_requested = std::mem::take(&mut session.interrupt_requested);
            let interrupted = errors_text.to_lowercase().contains("interrupt")
                || errors_text.to_lowercase().contains("aborted");
            if let Some(max) = reported_context_window(&v, &session.current_model) {
                session.context_window = effective_window(ctx.claudex.as_ref(), max);
            }
            emit_last_context(ctx, session);

            let outstanding = session.burst.outstanding();

            if interrupt_requested {
                // Stop already emitted TurnAborted and reset burst when the user
                // pressed it; just consume the CLI's interrupted result so it
                // isn't mistaken for a fresh turn end.
                session.pending_finalize = None;
                session.turn_active = false;
            } else if interrupted {
                // CLI-initiated abort (no user stop outstanding).
                session.burst.reset();
                session.pending_finalize = None;
                session.turn_active = false;
                ctx.emit(AgentEvent::TurnAborted);
            } else if outstanding > 0 {
                // Background agents are still running: hold the thread Running.
                // A result arriving now is either the launching turn pausing or a
                // per-agent wake while others work — never the true end.
                session.pending_finalize = None;
                ctx.emit(AgentEvent::Status {
                    text: if subtype == "success" {
                        format!(
                            "Waiting on {outstanding} background agent{}",
                            if outstanding == 1 { "" } else { "s" }
                        )
                    } else {
                        format!(
                            "background turn error: {}",
                            if errors_text.is_empty() { subtype } else { errors_text }
                        )
                    },
                });
            } else if subtype == "success" {
                let usage = v.get("usage");
                // The result usage is summed across every API call in the turn,
                // so cache reads repeat once per step — count only new tokens
                // (fresh input + cache writes) as "in".
                let input_tokens = usage.map(|u| {
                    ["input_tokens", "cache_creation_input_tokens"]
                        .iter()
                        .filter_map(|k| u.get(k).and_then(|v| v.as_u64()))
                        .sum::<u64>()
                });
                let max = session.context_window;
                // Live context = the last single API call's input side, not the
                // turn-cumulative result sum.
                let used = session.last_context_tokens;
                let completion = Usage {
                    input_tokens,
                    output_tokens: usage
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(|t| t.as_u64()),
                    used_tokens: used,
                    max_tokens: Some(max),
                    context_pct: used.map(|u| u as f64 / max as f64 * 100.0),
                    cost_usd: v.get("total_cost_usd").and_then(|c| c.as_f64()),
                };
                if session.burst.saw_background() {
                    // A turn that used background agents is winding down. Several
                    // completions can coalesce into back-to-back results (dropping
                    // multiple task ids in one `background_tasks_changed`), so
                    // debounce: (re)arm a short timer and fire exactly one
                    // TurnCompleted once the stream goes quiet. This can never
                    // hang the turn the way per-wake counting did.
                    session.pending_finalize = Some((Instant::now() + FINALIZE_GRACE, completion));
                } else {
                    // Plain turn — end immediately, no added latency.
                    session.pending_finalize = None;
                    session.turn_active = false;
                    ctx.emit(AgentEvent::TurnCompleted {
                        usage: Some(completion),
                    });
                    session.burst.reset();
                }
            } else {
                // Non-success terminal result, no agents outstanding.
                session.pending_finalize = None;
                session.burst.reset();
                session.turn_active = false;
                ctx.emit(AgentEvent::Error {
                    message: if errors_text.is_empty() {
                        format!("turn ended: {subtype}")
                    } else {
                        errors_text.to_string()
                    },
                });
            }
            // The direct control response is the same authoritative source as
            // Claude Code's own context UI. It corrects the event-based
            // fallback after every result subtype without delaying completion.
            if let Err(error) = session.request_context_usage().await {
                tracing::debug!("Claude post-turn context query failed: {error:#}");
            }
        }
        "control_response" => {
            let response = v.get("response").unwrap_or(&Value::Null);
            let request_id = response
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if session.pending_context_request.as_deref() == Some(request_id) {
                session.pending_context_request = None;
                let reported_model = response.pointer("/response/model").and_then(Value::as_str);
                let is_current_model = reported_model
                    .map(|model| same_model(model, &session.current_model))
                    .unwrap_or(true);
                if is_current_model {
                    if let Some((used, max)) = control_context_usage(&v) {
                        emit_context(ctx, session, used, max);
                    }
                }
                if std::mem::take(&mut session.context_refresh_queued) {
                    if let Err(error) = session.request_context_usage().await {
                        tracing::debug!("Claude queued context query failed: {error:#}");
                    }
                }
            }
        }
        "control_request" => {
            let request_id = v.get("request_id").cloned().unwrap_or(Value::Null);
            let request = v.get("request").cloned().unwrap_or(Value::Null);
            if request.get("subtype").and_then(|s| s.as_str()) == Some("can_use_tool") {
                handle_can_use_tool(ctx, session, request_id, request).await?;
            } else {
                // Politely refuse control requests we don't implement.
                let sub = request
                    .get("subtype")
                    .and_then(|s| s.as_str())
                    .unwrap_or("?")
                    .to_string();
                session
                    .write(&json!({
                        "type": "control_response",
                        "response": { "subtype": "error", "request_id": request_id,
                                      "error": format!("unsupported control request: {sub}") },
                    }))
                    .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_can_use_tool(
    ctx: &DriverCtx,
    session: &mut Session,
    request_id: Value,
    request: Value,
) -> Result<()> {
    let tool_name = request
        .get("tool_name")
        .and_then(|t| t.as_str())
        .unwrap_or("tool")
        .to_string();
    let input = request.get("input").cloned().unwrap_or(json!({}));

    if tool_name == "AskUserQuestion" {
        let raw = input.get("questions").cloned().unwrap_or(json!([]));
        let mut multi = HashMap::new();
        let questions: Vec<Question> = raw
            .as_array()
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .map(|(i, q)| {
                        // id MUST equal the question text — the Claude SDK looks
                        // answers up by question text.
                        let text = q
                            .get("question")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .unwrap_or_else(|| format!("q-{i}"));
                        let multi_select = q
                            .get("multiSelect")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        multi.insert(text.clone(), multi_select);
                        Question {
                            id: text.clone(),
                            header: q
                                .get("header")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            question: text,
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
                            multi_select,
                            allow_other: true,
                            is_secret: false,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        if questions.is_empty() {
            session
                .control_response(
                    &request_id,
                    json!({ "behavior": "allow", "updatedInput": input }),
                )
                .await?;
            return Ok(());
        }
        let request_key = new_id();
        session.pending_questions.insert(
            request_key.clone(),
            PendingQuestion {
                request_id,
                original: raw,
                multi,
            },
        );
        ctx.emit(AgentEvent::QuestionRequest {
            request_id: request_key,
            questions,
        });
        return Ok(());
    }

    let approval_id = new_id();

    if tool_name == "ExitPlanMode" {
        let plan = input
            .get("plan")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        session.pending.insert(
            approval_id.clone(),
            PendingApproval {
                request_id,
                tool_name,
                input,
                suggestions: None,
            },
        );
        ctx.emit(AgentEvent::ApprovalRequest {
            approval_id,
            approval_kind: "plan".into(),
            title: "Claude has a plan ready".into(),
            detail: plan,
            options: vec![
                ApprovalOption {
                    id: "approve_build".into(),
                    label: "Approve & build".into(),
                    tone: "allow".into(),
                },
                ApprovalOption {
                    id: "keep_planning".into(),
                    label: "Keep planning".into(),
                    tone: "deny".into(),
                },
            ],
        });
        return Ok(());
    }

    let detail = tool_detail(&tool_name, &input);
    let suggestions = request.get("permission_suggestions").cloned();
    session.pending.insert(
        approval_id.clone(),
        PendingApproval {
            request_id,
            tool_name: tool_name.clone(),
            input,
            suggestions,
        },
    );
    ctx.emit(AgentEvent::ApprovalRequest {
        approval_id,
        approval_kind: approval_kind(&tool_name).into(),
        title: format!("Claude wants to use {tool_name}"),
        detail,
        options: vec![
            ApprovalOption {
                id: "accept".into(),
                label: "Approve once".into(),
                tone: "allow".into(),
            },
            ApprovalOption {
                id: "acceptForSession".into(),
                label: "Always allow this session".into(),
                tone: "allowAlways".into(),
            },
            ApprovalOption {
                id: "decline".into(),
                label: "Decline".into(),
                tone: "deny".into(),
            },
        ],
    });
    Ok(())
}

async fn handle_approval(
    ctx: &DriverCtx,
    session: &mut Session,
    settings: &ThreadSettings,
    approval_id: String,
    option_id: String,
) -> Result<()> {
    let Some(pending) = session.pending.remove(&approval_id) else {
        // Stale card from a dead predecessor process — its control request no
        // longer exists, so just resolve the card to un-stick the UI.
        ctx.emit(AgentEvent::ApprovalResolved {
            approval_id,
            option_id,
        });
        return Ok(());
    };

    if pending.tool_name == "ExitPlanMode" {
        match option_id.as_str() {
            "approve_build" => {
                // Deny the tool (nothing executes from it) but tell the model the plan
                // is approved, and lift the plan-mode restriction so it can implement.
                let base = base_permission_mode(settings);
                session
                    .control_response(
                        &pending.request_id,
                        json!({ "behavior": "deny",
                                "message": "The user approved your plan. Exit plan mode and implement it now." }),
                    )
                    .await?;
                session
                    .control(json!({ "subtype": "set_permission_mode", "mode": base }))
                    .await?;
                session.current_mode = base.to_string();
                let _ = ctx
                    .hub
                    .store
                    .update_thread(&ctx.thread_id, |t| t.settings.mode = Mode::Build);
                ctx.hub.broadcast_state("threads", None);
                ctx.emit(AgentEvent::Status {
                    text: "Plan approved — switched to build mode".into(),
                });
            }
            _ => {
                session
                    .control_response(
                        &pending.request_id,
                        json!({ "behavior": "deny",
                                "message": "The user wants to keep planning. Wait for their next message before doing anything else." }),
                    )
                    .await?;
            }
        }
        ctx.emit(AgentEvent::ApprovalResolved {
            approval_id,
            option_id,
        });
        return Ok(());
    }

    let response = match option_id.as_str() {
        "accept" => json!({ "behavior": "allow", "updatedInput": pending.input }),
        "acceptForSession" => {
            let mut r = json!({ "behavior": "allow", "updatedInput": pending.input });
            if let Some(sugg) = pending.suggestions {
                if !sugg.is_null() {
                    r["updatedPermissions"] = sugg;
                }
            }
            r
        }
        _ => json!({ "behavior": "deny", "message": "User declined tool execution." }),
    };
    session
        .control_response(&pending.request_id, response)
        .await?;
    ctx.emit(AgentEvent::ApprovalResolved {
        approval_id,
        option_id,
    });
    Ok(())
}

async fn handle_question_answer(
    ctx: &DriverCtx,
    session: &mut Session,
    request_id: String,
    answers: HashMap<String, Vec<String>>,
) -> Result<()> {
    let Some(pending) = session.pending_questions.remove(&request_id) else {
        // Stale card from a dead predecessor process — resolve to un-stick the UI.
        ctx.emit(AgentEvent::QuestionResolved {
            request_id,
            answers: Some(answers),
        });
        return Ok(());
    };
    let answers_for_log = answers.clone();
    // Build the answers map the Claude SDK expects: keyed by question text,
    // value is a bare string for single-select, an array for multi-select.
    let mut answer_map = serde_json::Map::new();
    for (key, labels) in answers {
        let multi = pending.multi.get(&key).copied().unwrap_or(false);
        let value = if multi {
            json!(labels)
        } else {
            json!(labels.into_iter().next().unwrap_or_default())
        };
        answer_map.insert(key, value);
    }
    session
        .control_response(
            &pending.request_id,
            json!({
                "behavior": "allow",
                "updatedInput": { "questions": pending.original, "answers": answer_map },
            }),
        )
        .await?;
    ctx.emit(AgentEvent::QuestionResolved {
        request_id,
        answers: Some(answers_for_log),
    });
    Ok(())
}

/// Availability probe for agents.info: checks the CLI exists (auth is checked lazily
/// — the CLI reports auth errors in-stream on first use).
pub fn probe() -> (bool, Option<String>) {
    match super::resolve_bin("claude") {
        Some(_) => (true, None),
        None => (
            false,
            Some("Claude Code CLI not found — install it and run `claude login`".into()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Short pre-response budget so a stall is easy to assert; the compaction
    /// budget stays at the production value the timing assertions name.
    fn test_policy() -> DriverPolicy {
        DriverPolicy {
            first_response_timeout: Duration::from_secs(5),
            compaction_timeout: COMPACTION_TIMEOUT,
            interrupt_grace: Duration::ZERO,
            max_auto_reconnects: 1,
        }
    }

    fn settings(access: Access) -> ThreadSettings {
        ThreadSettings {
            model: "claude-fable-5".into(),
            effort: Some("high".into()),
            wide_context: false,
            claude_chrome: false,
            access,
            mode: Mode::Build,
            browser_profile_id: None,
        }
    }

    #[test]
    fn every_spawn_allows_a_later_full_access_switch_without_enabling_it() {
        let args = command_args(
            &settings(Access::Read),
            None,
            None,
            "http://127.0.0.1:42800/mcp",
            "tok",
            &[],
        );
        assert!(args
            .iter()
            .any(|arg| arg == "--allow-dangerously-skip-permissions"));
        assert!(!args.iter().any(|arg| arg == "bypassPermissions"));
    }

    #[test]
    fn native_claude_can_launch_with_chrome() {
        let mut enabled = settings(Access::Full);
        enabled.claude_chrome = true;
        let native = command_args(&enabled, None, None, "http://127.0.0.1:42800/mcp", "tok", &[]);
        assert!(native.iter().any(|arg| arg == "--chrome"));

        let profile = claudex_profile();
        let bridged = command_args(
            &enabled,
            Some(&profile),
            None,
            "http://127.0.0.1:42800/mcp",
            "tok",
            &[],
        );
        assert!(!bridged.iter().any(|arg| arg == "--chrome"));
    }

    #[test]
    fn full_access_starts_in_bypass_mode() {
        let args = command_args(
            &settings(Access::Full),
            None,
            None,
            "http://127.0.0.1:42800/mcp",
            "tok",
            &[],
        );
        let permission = args
            .windows(2)
            .find(|pair| pair[0] == "--permission-mode")
            .map(|pair| pair[1].as_str());
        assert_eq!(permission, Some("bypassPermissions"));
    }

    #[test]
    fn mcp_config_spawn_argument_matches_platform_contract() {
        let json = r#"{"mcpServers":{"threadknot-browser":{"type":"http"}}}"#.to_string();
        let (arg, lease) = mcp_config_for_spawn(json.clone()).unwrap();

        #[cfg(windows)]
        {
            let lease = lease.expect("Windows should use a file-backed MCP config");
            assert_eq!(std::fs::read_to_string(&arg).unwrap(), json);
            let path = lease.path.clone();
            drop(lease);
            assert!(!path.exists(), "temporary MCP config should be removed");
        }

        #[cfg(not(windows))]
        {
            assert_eq!(arg, json);
            assert!(lease.is_none());
        }
    }

    #[test]
    fn provider_default_omits_the_effort_flag() {
        let mut default = settings(Access::Read);
        default.effort = None;
        let args = command_args(&default, None, None, "http://127.0.0.1:42800/mcp", "tok", &[]);
        assert!(!args.iter().any(|arg| arg == "--effort"));
    }

    #[test]
    fn explicit_effort_still_reaches_claude() {
        let args = command_args(
            &settings(Access::Read),
            None,
            None,
            "http://127.0.0.1:42800/mcp",
            "tok",
            &[],
        );
        let effort = args
            .windows(2)
            .find(|pair| pair[0] == "--effort")
            .map(|pair| pair[1].as_str());
        assert_eq!(effort, Some("high"));
    }

    #[test]
    fn wide_context_suffix_only_applies_to_capable_models() {
        let mut capable = settings(Access::Read);
        capable.wide_context = true;
        assert_eq!(api_model_id(&capable, None), "claude-fable-5[1m]");

        // Opus 5 is always 1M and uses its canonical id without a suffix.
        capable.model = "claude-opus-5".into();
        assert_eq!(api_model_id(&capable, None), "claude-opus-5");

        // Haiku is 200K-only, so the suffix must not be applied.
        capable.model = "claude-haiku-4-5".into();
        assert_eq!(api_model_id(&capable, None), "claude-haiku-4-5");
    }

    fn claudex_profile() -> ClaudexProfile {
        ClaudexProfile {
            id: "profile-1".into(),
            name: "GPT-5.6 Sol".into(),
            avatar: None,
            base_url: "http://127.0.0.1:18765".into(),
            model: "gpt-5.6-sol".into(),
            small_model: Some("gpt-5.6-luna".into()),
            context_window: Some(272_000),
            efforts: vec!["low".into(), "high".into()],
            default_effort: Some("high".into()),
            auth_token: String::new(),
            env: Vec::new(),
            sidecar: None,
            created_at: "2026-07-26T00:00:00Z".into(),
        }
    }

    /// A bridged thread's `model` setting is the PROFILE id, so the CLI must be
    /// told the gateway's model instead — and never given the `[1m]` suffix,
    /// which is a Claude-client hint that widens nothing upstream.
    #[test]
    fn a_claudex_profile_supplies_the_model_and_the_real_window() {
        let profile = claudex_profile();
        let mut settings = settings(Access::Full);
        settings.model = profile.id.clone();
        settings.wide_context = true;

        let model = api_model_id(&settings, Some(&profile));
        assert_eq!(model, "gpt-5.6-sol");
        assert!(!model.ends_with("[1m]"));
        assert_eq!(context_window(&model, Some(&profile)), 272_000);

        let args = command_args(
            &settings,
            Some(&profile),
            None,
            "http://127.0.0.1:42800/mcp",
            "tok",
            &[],
        );
        let passed = args
            .windows(2)
            .find(|pair| pair[0] == "--model")
            .map(|pair| pair[1].as_str());
        assert_eq!(passed, Some("gpt-5.6-sol"));
    }

    /// Without a stated window we fall back to Claude's 200K default rather
    /// than inventing one — the control response corrects it when it arrives.
    #[test]
    fn a_windowless_profile_falls_back_to_the_conservative_default() {
        let mut profile = claudex_profile();
        profile.context_window = None;
        assert_eq!(context_window("gpt-5.6-sol", Some(&profile)), 200_000);
    }

    /// The CLI reports 200K for a bridged model it knows nothing about. Left
    /// alone that overwrites the profile's real window on the first usage
    /// report — observed live against a 272K Sol profile.
    #[test]
    fn a_profile_window_outranks_whatever_the_cli_reports() {
        let profile = claudex_profile();
        assert_eq!(effective_window(Some(&profile), 200_000), 272_000);
        // Plain Claude still trusts the CLI — it knows its own models.
        assert_eq!(effective_window(None, 200_000), 200_000);
        // So does a profile that declined to state a window.
        let mut windowless = claudex_profile();
        windowless.context_window = None;
        assert_eq!(effective_window(Some(&windowless), 1_000_000), 1_000_000);
    }

    #[test]
    fn builtin_models_replace_opus_48_with_fixed_window_opus_5() {
        let models = builtin_models();
        assert_eq!(DEFAULT_MODEL, "claude-opus-5");
        assert!(models.iter().any(|model| model.id == DEFAULT_MODEL));
        assert!(!models.iter().any(|model| model.id == "claude-opus-4-8"));
        let opus = models
            .iter()
            .find(|model| model.id == "claude-opus-5")
            .expect("Opus 5 model");
        assert_eq!(opus.name, "Claude Opus 5");
        assert_eq!(opus.fixed_context_window, Some(1_000_000));
        assert_eq!(opus.supports_wide_context, None);
    }

    #[test]
    fn fallback_context_counts_latest_input_side_but_not_output() {
        let usage = json!({
            "input_tokens": 4,
            "cache_creation_input_tokens": 2_715,
            "cache_read_input_tokens": 21_144,
            "output_tokens": 679
        });
        assert_eq!(active_context_tokens(&usage), Some(23_863));
    }

    #[test]
    fn result_window_uses_current_model_instead_of_historical_maximum() {
        let result = json!({
            "modelUsage": {
                "claude-fable-5": { "contextWindow": 200_000 },
                "claude-sonnet-5": { "contextWindow": 1_000_000 }
            }
        });
        assert_eq!(
            reported_context_window(&result, "claude-fable-5"),
            Some(200_000)
        );
        assert_eq!(
            reported_context_window(&result, "claude-sonnet-5[1m]"),
            Some(1_000_000)
        );
    }

    #[test]
    fn compact_and_control_snapshots_parse_wire_casing() {
        let compact = json!({
            "type": "system",
            "subtype": "compact_boundary",
            "compact_metadata": { "pre_tokens": 419_604, "post_tokens": 19_066 }
        });
        assert_eq!(compact_post_tokens(&compact), Some(19_066));

        let response = json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "threadknot-3",
                "response": { "totalTokens": 24_542, "maxTokens": 200_000 }
            }
        });
        assert_eq!(control_context_usage(&response), Some((24_542, 200_000)));
    }

    #[test]
    fn opus_5_has_a_fixed_one_million_token_window() {
        assert_eq!(context_window("claude-opus-5", None), 1_000_000);
        assert_eq!(context_window("claude-fable-5", None), 200_000);
    }

    fn ids(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// A plain turn never touches the background set, so it is not debounced.
    #[test]
    fn plain_turn_saw_no_background() {
        let b = BurstTracker::default();
        assert_eq!(b.outstanding(), 0);
        assert!(!b.saw_background());
    }

    /// While any agent is outstanding the turn is held; once the set empties the
    /// turn is flagged as having used background agents (so the caller debounces
    /// the end rather than firing on the first cleared result).
    #[test]
    fn background_agents_track_outstanding_and_history() {
        let mut b = BurstTracker::default();
        b.set_background(ids(&["a", "b"]));
        assert_eq!(b.outstanding(), 2);
        assert!(b.saw_background());
        // One finishes — still one outstanding, still Running.
        b.set_background(ids(&["b"]));
        assert_eq!(b.outstanding(), 1);
        // Both gone — nothing outstanding, but the turn is remembered as a burst.
        b.set_background(ids(&[]));
        assert_eq!(b.outstanding(), 0);
        assert!(b.saw_background());
    }

    /// Coalesced completions (two ids leaving the set at once) still leave the
    /// set empty and the turn flagged — the caller finalizes exactly once via the
    /// debounce, with no leftover wake count to hang on.
    #[test]
    fn coalesced_completion_empties_cleanly() {
        let mut b = BurstTracker::default();
        b.set_background(ids(&["a", "b"]));
        // Both finish reported in a single background_tasks_changed frame.
        b.set_background(ids(&[]));
        assert_eq!(b.outstanding(), 0);
        assert!(b.saw_background());
    }

    /// A fresh turn forgets prior background history but keeps genuinely
    /// still-outstanding agents authoritative.
    #[test]
    fn begin_turn_resets_history_but_keeps_live_agents() {
        let mut b = BurstTracker::default();
        b.set_background(ids(&["a"]));
        b.set_background(ids(&[]));
        assert!(b.saw_background());
        // Next user turn, no agents live -> history cleared.
        b.begin_turn();
        assert!(!b.saw_background());
        // But a turn that starts with an agent still running stays flagged.
        b.set_background(ids(&["x"]));
        b.begin_turn();
        assert!(b.saw_background());
        assert_eq!(b.outstanding(), 1);
    }

    /// Field paths, checked against frames captured from the real claude CLI
    /// (2.1.212, stream-json). A drift in the wire shape breaks these.
    #[test]
    fn parses_real_captured_task_frames() {
        // system/background_tasks_changed with one outstanding background agent.
        let changed = json!({
            "type": "system", "subtype": "background_tasks_changed",
            "tasks": [{ "task_id": "a89f4c7c7a3e4d8be", "task_type": "local_agent",
                        "description": "Reply with WORLD" }]
        });
        assert_eq!(background_task_ids(&changed), ids(&["a89f4c7c7a3e4d8be"]));
        // …and the empty set when the last one finishes.
        let cleared = json!({
            "type": "system", "subtype": "background_tasks_changed", "tasks": []
        });
        assert!(background_task_ids(&cleared).is_empty());
    }

    /// Only `local_agent` tasks are subagents. Background tool tasks (`local_bash`)
    /// share the task lifecycle but must NOT show up as subagents in the UI — they
    /// stay ordinary tool cards. (Field values captured from claude CLI 2.1.212.)
    #[test]
    fn only_local_agent_tasks_count_as_subagents() {
        let agent = json!({
            "type": "system", "subtype": "task_started",
            "task_id": "a1", "task_type": "local_agent",
            "subagent_type": "general-purpose", "description": "Reply DONE"
        });
        let bash = json!({
            "type": "system", "subtype": "task_started",
            "task_id": "b1", "task_type": "local_bash",
            "description": "Sleep for 6 seconds in background"
        });
        assert!(is_subagent_task(&agent));
        assert!(!is_subagent_task(&bash));
    }

    /// An interrupt clears burst state so a later turn is not wrongly deferred.
    #[test]
    fn reset_clears_outstanding_background_state() {
        let mut b = BurstTracker::default();
        b.set_background(ids(&["a", "b"]));
        assert_eq!(b.outstanding(), 2);
        assert!(b.saw_background());
        b.reset();
        assert_eq!(b.outstanding(), 0);
        assert!(!b.saw_background());
    }

    #[test]
    fn authoritative_model_matching_accepts_wide_and_dated_ids_only() {
        assert!(same_model("claude-fable-5", "claude-fable-5[1m]"));
        assert!(same_model("claude-haiku-4-5-20251001", "claude-haiku-4-5"));
        assert!(!same_model("claude-fable-5", "claude-sonnet-5[1m]"));
    }

    #[test]
    fn local_cli_frames_do_not_mask_a_stalled_provider_request() {
        let init = json!({
            "type": "system",
            "subtype": "init",
            "session_id": "session-1"
        });
        let requesting = json!({
            "type": "system",
            "subtype": "status",
            "status": "requesting"
        });
        let context = json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "threadknot-1",
                "response": { "totalTokens": 42, "maxTokens": 200_000 }
            }
        });
        assert!(!is_provider_progress(&init));
        assert!(!is_provider_progress(&requesting));
        assert!(!is_provider_progress(&context));
    }

    #[test]
    fn model_and_tool_frames_disarm_the_pre_response_watchdog() {
        let stream = json!({
            "type": "stream_event",
            "event": { "type": "message_start" }
        });
        let permission = json!({
            "type": "control_request",
            "request": { "subtype": "can_use_tool", "tool_name": "Bash" }
        });
        let result = json!({ "type": "result", "subtype": "success" });
        assert!(is_provider_progress(&stream));
        assert!(is_provider_progress(&permission));
        assert!(is_provider_progress(&result));

        let policy = test_policy();
        let mut watchdog = FirstResponseWatchdog::new(policy);
        watchdog.start_turn(Instant::now());
        assert!(watchdog.deadline().is_some());
        watchdog.provider_progress();
        assert!(watchdog.deadline().is_none());
    }

    /// The regression behind "Claude stopped responding before it began work"
    /// on `/compact`: compaction is one long silent summarization whose only
    /// wire signal is a status frame the progress rule ignores, so the 90s
    /// pre-response budget killed the CLI mid-summary and lost the compaction.
    #[test]
    fn compaction_gets_its_own_budget_instead_of_reading_as_a_stall() {
        let compacting = json!({
            "type": "system",
            "subtype": "status",
            "status": "compacting"
        });
        assert!(is_compaction_start(&compacting));
        // Still not "progress" — the black-holed-TCP guard stays intact for
        // every other status the CLI emits.
        assert!(!is_provider_progress(&compacting));
        assert!(!is_compaction_start(&json!({
            "type": "system", "subtype": "status", "status": "requesting"
        })));

        let now = Instant::now();
        let mut watchdog = FirstResponseWatchdog::new(test_policy());
        watchdog.start_turn(now);
        watchdog.compacting(now);
        // Well past the pre-response budget, still inside the compaction one.
        assert!(watchdog.deadline().unwrap() > now + Duration::from_secs(5));
        assert_eq!(watchdog.deadline(), Some(now + Duration::from_secs(600)));

        // Compaction finishing is ordinary progress and disarms outright.
        assert!(is_provider_progress(&json!({
            "type": "system", "subtype": "compact_boundary"
        })));
    }

    /// An auto-compaction can fire mid-turn, after real output has already
    /// disarmed the watchdog. Re-arming there would make a later stall replay
    /// the user message on top of completed work.
    #[test]
    fn compaction_never_rearms_a_watchdog_that_progress_already_disarmed() {
        let now = Instant::now();
        let mut watchdog = FirstResponseWatchdog::new(test_policy());
        watchdog.start_turn(now);
        watchdog.provider_progress();
        watchdog.compacting(now);
        assert!(watchdog.deadline().is_none());
    }

    /// The second half of the same bug: the stall recovery wrapped the replayed
    /// text in a preamble, which stopped the CLI intercepting `/compact` at all
    /// — the model just described the command instead of running it.
    #[test]
    fn reconnect_replays_a_slash_command_verbatim() {
        assert_eq!(reconnect_message("/compact"), "/compact");
        assert_eq!(reconnect_message("  /compact  "), "  /compact  ");
        assert_eq!(reconnect_message("/my-command some args"), "/my-command some args");

        // Ordinary prose keeps the do-not-repeat-work framing.
        let wrapped = reconnect_message("fix the parser");
        assert!(wrapped.starts_with("[Threadknot automatically replaced"));
        assert!(wrapped.ends_with("fix the parser"));

        // Not commands: a path, prose that merely opens with a slash, and a
        // command that picked up a mid-turn steer note (no longer bare).
        assert!(!is_slash_command("/srv/projects/threadknot"));
        assert!(!is_slash_command("/ what does this do"));
        assert!(!is_slash_command("/compact\n\n[Note added mid-turn]: also check the logs"));
        assert!(!is_slash_command("read src/main.rs"));
    }

    #[test]
    fn watchdog_reconnects_once_then_fails_closed() {
        let now = Instant::now();
        let mut watchdog = FirstResponseWatchdog::new(test_policy());
        watchdog.start_turn(now);
        assert_eq!(
            watchdog.timed_out(now + Duration::from_secs(5)),
            StallAction::Reconnect
        );
        assert!(watchdog.deadline().is_some());
        assert_eq!(
            watchdog.timed_out(now + Duration::from_secs(10)),
            StallAction::Fail
        );
        assert!(watchdog.deadline().is_none());

        // A distinct user turn gets its own single recovery budget.
        watchdog.start_turn(now + Duration::from_secs(11));
        assert_eq!(
            watchdog.timed_out(now + Duration::from_secs(16)),
            StallAction::Reconnect
        );
    }

    #[tokio::test]
    async fn closing_driver_receiver_quarantines_the_old_session_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentCommand>();
        assert!(!tx.is_closed());
        rx.close();
        assert!(tx.is_closed());
        assert!(tx.send(AgentCommand::Interrupt).is_err());
    }
}

//! The fleet half of the agent tool surface: what machines exist, how to run a
//! command on one of them, and (Phase 1+) how to hand a whole brief to another
//! agent. Split out of `mcp.rs` because that file is one enormous `json!`
//! literal for the browser tools and these have nothing to do with a browser.
//!
//! Two rules shape everything here.
//!
//! **Nothing blocks for long.** A routed request must answer inside
//! `PEER_REQUEST_TIMEOUT` or it wedges a link shared with every other user of
//! that machine, and an MCP tool call must answer inside whatever timeout the
//! driving harness happens to use — which differs per provider and is not ours
//! to choose. So the wire operations start work and return a handle, and the
//! *waiting* happens here, in-process, for a bounded window. A command that
//! finishes quickly still looks synchronous to the agent; a release build
//! degrades to polling instead of to a lie.
//!
//! **Authority narrows, never widens.** These tools run with the thread's own
//! access level, and the MCP channel has no way to raise an approval card — so
//! a tool that executes code refuses outright on a thread whose access would
//! have made the harness ask. See `require_execute_access`.

use crate::protocol::{Access, Agent};
use crate::server::ServerState;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Output kept per stream when reporting a finished command back to the model.
/// The tail, not the head: the end of a build log is the part that says what
/// went wrong, and the first 12 KB of a cargo build is a list of crate names.
const TOOL_OUTPUT_TAIL: usize = 12_000;

/// The ceiling on how long ANY tool here will sit before answering.
///
/// This was 90s, on the theory that it was "comfortably inside every harness's
/// tool timeout". It is not: a real Claude Code session driving this fleet had
/// `run_on_machine`, `exec_status` and `dispatch_wait` calls killed by the
/// harness before the tool answered. The job survived every time — which is the
/// property that matters and the one Dispatch was designed for — but the model
/// lost the handle and had to go looking for it in `exec.list`.
///
/// So: the WORK may take three hours, and the ANSWER must arrive in seconds.
/// Those are different clocks, and only the second one is negotiable. 25s is
/// short enough for a harness that gives a tool 30s and still long enough that
/// a quick build or a fast worker is usually reported inline rather than as a
/// handle to poll.
const TOOL_ANSWER_BUDGET: u64 = 25;

/// How long `run_on_machine` will sit on a job before handing back a handle.
const RUN_WAIT_BUDGET: Duration = Duration::from_secs(TOOL_ANSWER_BUDGET);

/// One poll of a running job. Bounded by the peer request timeout because this
/// same call is what crosses the mesh.
const POLL_SLICE: u64 = 15;

// ------------------------------------------------------------- resolution ---

pub struct Target {
    pub machine_id: String,
    pub name: String,
    pub is_self: bool,
    pub online: bool,
}

/// Resolve a machine the way a model will name it: by friendly name far more
/// often than by uuid. Accepting only the id would be technically defensible
/// and practically useless — the roster tool prints names, so the model uses
/// names.
pub fn resolve_machine(state: &ServerState, spec: Option<&str>) -> Result<Target> {
    let me = Target {
        machine_id: state.device.machine_id.clone(),
        name: state.device.friendly_name(),
        is_self: true,
        online: true,
    };
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(me);
    };
    if spec == me.machine_id || spec.eq_ignore_ascii_case(&me.name) || spec == "self" {
        return Ok(me);
    }
    let peers = state.peernet.registry.list();
    let found = peers
        .iter()
        .find(|p| p.machine_id == spec)
        .or_else(|| peers.iter().find(|p| p.name.eq_ignore_ascii_case(spec)))
        // A model that saw "spencer_windows" may well type "windows".
        .or_else(|| {
            let needle = spec.to_ascii_lowercase();
            let mut hits = peers
                .iter()
                .filter(|p| p.name.to_ascii_lowercase().contains(&needle));
            let first = hits.next();
            // Ambiguity must fail loudly; picking one at random is how a build
            // lands on the wrong machine and nobody notices for a week.
            match hits.next() {
                Some(_) => None,
                None => first,
            }
        });
    let peer = found.with_context(|| {
        let known: Vec<String> = std::iter::once(me.name.clone())
            .chain(peers.iter().map(|p| p.name.clone()))
            .collect();
        format!(
            "no machine matches \"{spec}\". Paired machines: {}. Call `machines` for the roster.",
            known.join(", ")
        )
    })?;
    anyhow::ensure!(
        !peer.needs_upgrade(),
        "{} was paired before Threadknot encrypted mesh connections — update Threadknot there \
         and pair the two again",
        peer.name
    );
    Ok(Target {
        online: state.peernet.is_online(&peer.machine_id),
        machine_id: peer.machine_id.clone(),
        name: peer.name.clone(),
        is_self: false,
    })
}

/// The roots a given machine contributes to the workspace the calling thread
/// belongs to. Roots-only, deliberately: the same rule thread creation follows.
pub fn roots_for(state: &ServerState, thread_id: &str, machine_id: &str) -> Vec<Value> {
    let store = &state.hub.store;
    let Some(thread) = store.thread(thread_id) else {
        return Vec::new();
    };
    let Some(workspace_id) = store.workspace_for_project(&thread.project_id) else {
        return Vec::new();
    };
    let Some(workspace) = store.workspace(&workspace_id) else {
        return Vec::new();
    };
    workspace
        .members
        .iter()
        .filter(|m| m.machine_id == machine_id)
        .map(|m| {
            // A local member's live project record beats the snapshot taken when
            // the root was attached; a remote one only has the snapshot.
            let (name, path) = match store.project(&m.project_id) {
                Some(p) if m.machine_id == store.local_machine_id() => (Some(p.name), Some(p.path)),
                _ => (m.name.clone(), m.path.clone()),
            };
            json!({ "projectId": m.project_id, "name": name, "path": path })
        })
        .collect()
}

/// Which project a command or a dispatch runs in. `spec` may be a project id,
/// a root name, or a path fragment.
pub fn resolve_root(
    state: &ServerState,
    thread_id: &str,
    target: &Target,
    spec: Option<&str>,
) -> Result<String> {
    let roots = roots_for(state, thread_id, &target.machine_id);
    let label = |r: &Value| {
        r.get("name")
            .and_then(Value::as_str)
            .or_else(|| r.get("path").and_then(Value::as_str))
            .unwrap_or("(unnamed)")
            .to_string()
    };
    let id_of = |r: &Value| r["projectId"].as_str().unwrap_or_default().to_string();

    if let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) {
        // An explicit project id on the target machine is authoritative even if
        // it is not a workspace member — the caller knows something we don't.
        if let Some(hit) = roots.iter().find(|r| id_of(r) == spec) {
            return Ok(id_of(hit));
        }
        let needle = spec.to_ascii_lowercase();
        let mut hits = roots.iter().filter(|r| {
            label(r).to_ascii_lowercase().contains(&needle)
                || r.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|p| p.to_ascii_lowercase().contains(&needle))
        });
        if let Some(hit) = hits.next() {
            anyhow::ensure!(
                hits.next().is_none(),
                "\"{spec}\" matches more than one root on {} — name it exactly",
                target.name
            );
            return Ok(id_of(hit));
        }
        if target.is_self && state.hub.store.project(spec).is_some() {
            return Ok(spec.to_string());
        }
        anyhow::bail!(
            "no root called \"{spec}\" on {}. Roots there: {}",
            target.name,
            root_labels(&roots)
        );
    }

    // No hint. One root is unambiguous; the thread's own project is the obvious
    // answer on this machine; anything else has to be asked for.
    if roots.len() == 1 {
        return Ok(id_of(&roots[0]));
    }
    if target.is_self {
        if let Some(thread) = state.hub.store.thread(thread_id) {
            return Ok(thread.project_id);
        }
    }
    anyhow::bail!(
        "{} has {} roots in this workspace — say which one: {}",
        target.name,
        roots.len(),
        root_labels(&roots)
    );
}

fn root_labels(roots: &[Value]) -> String {
    if roots.is_empty() {
        return "none — attach one in the workspace's roots first".to_string();
    }
    roots
        .iter()
        .map(|r| {
            r.get("name")
                .and_then(Value::as_str)
                .unwrap_or("(unnamed)")
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Run an RPC here or on a peer, whichever the target names.
///
/// `on_behalf_of: None` means "that machine's own owner", which is right: the
/// agent is driving a thread on this machine under this machine's authority,
/// exactly as the desktop owner's own click would. The narrowing that matters
/// happened before the call, in `require_execute_access`.
pub async fn rpc(state: &ServerState, target: &Target, kind: &str, payload: Value) -> Result<Value> {
    if target.is_self {
        return route_local(state, kind, &payload).await;
    }
    anyhow::ensure!(
        target.online,
        "{} is offline — Threadknot cannot reach it right now",
        target.name
    );
    state
        .peernet
        .request(&target.machine_id, kind, payload, None)
        .await
}

async fn route_local(state: &ServerState, kind: &str, payload: &Value) -> Result<Value> {
    match kind {
        k if k.starts_with("exec.") => crate::exec::handle(state, k, payload).await,
        "device.info" => Ok(state.device.info()),
        _ => anyhow::bail!("unroutable request: {kind}"),
    }
}

/// Executing code through the MCP channel bypasses the harness's own permission
/// prompt, so the thread's access level has to be checked here instead.
///
/// `full` is the only level that already means "act without asking". On `read`
/// or `edits` the harness would have raised an approval card for a shell
/// command, and there is no card to raise from inside a tool call — so the
/// honest answer is a refusal that explains how to change it, not a silent
/// escalation.
fn require_execute_access(state: &ServerState, thread_id: &str) -> Result<()> {
    let thread = state
        .hub
        .store
        .thread(thread_id)
        .context("unknown thread")?;
    anyhow::ensure!(
        thread.settings.access == Access::Full,
        "this chat runs with {} access, so it cannot run commands unattended. \
         Switch it to Full access beside the message box to use the fleet tools.",
        match thread.settings.access {
            Access::Read => "read-only",
            Access::Edits => "edits-only",
            Access::Full => "full",
        }
    );
    Ok(())
}

// ------------------------------------------------------------------ tools ---

/// The fleet tools this particular thread should see.
///
/// Filtered rather than uniform, because an unusable tool in the list is worse
/// than an absent one: a worker offered `dispatch` will eventually try to
/// delegate, burn a turn, and be refused by the depth cap — and a planner
/// offered `report_result` will try to "finish" with it.
pub fn tool_specs_for(state: &ServerState, thread_id: &str) -> Vec<Value> {
    let thread = state.hub.store.thread(thread_id);
    let is_worker = thread.as_ref().is_some_and(|t| t.dispatch.is_some());
    // Depth is the real gate on delegating further; with the cap at 1 a worker
    // can never dispatch, but the check reads from the limit rather than
    // assuming its value.
    let may_dispatch = thread
        .as_ref()
        .is_some_and(|t| crate::dispatch::depth_for(t, &state.hub.dispatch).is_ok());

    tool_specs()
        .into_iter()
        .filter(|spec| {
            let name = spec.get("name").and_then(Value::as_str).unwrap_or("");
            match name {
                "report_result" => is_worker,
                "dispatch" | "dispatch_wait" | "dispatch_status" | "dispatch_cancel" => {
                    may_dispatch
                }
                _ => true,
            }
        })
        .collect()
}

pub fn tool_specs() -> Vec<Value> {
    let obj = |props: Value, required: Value| {
        json!({ "type": "object", "properties": props, "required": required })
    };
    vec![
        json!({
            "name": "machines",
            "description":
                "List the machines in this Threadknot fleet — this one and every paired machine — \
                 with their OS, whether they are reachable right now, which coding agents are \
                 installed on each, and which of this workspace's root folders live on each. \
                 Call this FIRST whenever work might belong on another machine (a Windows or \
                 macOS build, a platform-specific test). Machine names from this list are what \
                 `run_on_machine` and `dispatch` accept.",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "run_on_machine",
            "description":
                "Run a shell command on any machine in the fleet and get its exit code and output. \
                 Use this for deterministic work with a known command — builds, test suites, \
                 packaging, git operations — especially on another machine (`cargo build` on the \
                 Mac, an MSI build on Windows). For work that needs judgement or may have to fix \
                 its own failures, use `dispatch` instead. The command runs in one of the \
                 workspace's root folders, through `sh` on Linux/macOS and PowerShell on Windows \
                 — check the machine's OS with `machines` before writing shell syntax. Long \
                 commands are not truncated by a timeout on your side: if the command outlives \
                 the wait window you get a jobId back, and you poll it with `exec_status`.",
            "inputSchema": obj(
                json!({
                    "command": { "type": "string", "description": "The command line to run" },
                    "machine": { "type": "string", "description": "Machine name or id from `machines`. Omit for this machine." },
                    "root": { "type": "string", "description": "Which workspace root to run in (name or project id). Omit when the machine has only one." },
                    "subdir": { "type": "string", "description": "Optional path relative to that root" },
                    "timeoutSeconds": { "type": "number", "description": "Kill the command after this long (default 600, max 10800)" }
                }),
                json!(["command"]),
            ),
        }),
        json!({
            "name": "exec_status",
            "description":
                "Check on a command started by `run_on_machine` that had not finished yet. Returns \
                 any new output since your last check plus the exit code once it lands. \
                 \"running\" is a normal answer — wait and ask again.",
            "inputSchema": obj(
                json!({
                    "jobId": { "type": "string" },
                    "machine": { "type": "string", "description": "The machine it is running on (same value you passed to run_on_machine)" },
                    "waitSeconds": { "type": "number", "description": "Wait up to this long for it to finish before answering (default 25, max 25)" }
                }),
                json!(["jobId"]),
            ),
        }),
        json!({
            "name": "exec_cancel",
            "description": "Stop a running command started by `run_on_machine`, killing the whole process tree.",
            "inputSchema": obj(
                json!({
                    "jobId": { "type": "string" },
                    "machine": { "type": "string" }
                }),
                json!(["jobId"]),
            ),
        }),
        json!({
            "name": "dispatch",
            "description":
                "Hand a piece of work to ANOTHER coding agent — a different model or harness \
                 (claude, codex, kimi), on this machine or any machine in the fleet — and get a \
                 report back. Use this when you are planning and driving work that somebody else \
                 should implement, or when a job needs judgement rather than a fixed command \
                 (build this feature, get the macOS build working and fix what breaks). For a \
                 known command with no judgement involved, `run_on_machine` is cheaper and more \
                 predictable. \
                 \n\nThe worker gets its OWN thread, its own context and its own working \
                 directory; it cannot see this conversation and cannot ask you questions, so the \
                 brief must be self-contained — say what to do, what done looks like, and what \
                 to report. \
                 \n\nThis returns IMMEDIATELY with dispatch ids; the work continues in the \
                 background. Use `dispatch_wait` to collect results. Pass several machines to \
                 fan the same brief out to all of them (one worker each) — that is how you get \
                 a build for every platform at once.",
            "inputSchema": obj(
                json!({
                    "brief": { "type": "string", "description": "Self-contained instructions for the worker" },
                    "label": { "type": "string", "description": "Short label for the status row, e.g. \"build: macOS\"" },
                    "agent": { "type": "string", "enum": ["claude", "codex", "kimi"], "description": "Which harness does the work. Defaults to the one running this thread." },
                    "model": { "type": "string", "description": "Model for the worker; defaults to that agent's default" },
                    "effort": { "type": "string", "description": "Reasoning effort, if the agent supports one" },
                    "machine": { "type": "string", "description": "Machine name from `machines`. Omit for this machine." },
                    "machines": { "type": "array", "items": { "type": "string" }, "description": "Fan the same brief out to several machines, one worker each" },
                    "root": { "type": "string", "description": "Which workspace root to work in. Omit when the machine has only one." },
                    "access": { "type": "string", "enum": ["read", "edits", "full"], "description": "Worker's permissions. Never exceeds this chat's own. Note that a worker has NOBODY to approve anything: at 'read' every command it runs is refused, and at 'edits' every write is, so it will report back a list of things it could not do. Use 'full' for work you actually want done unattended, and keep the brief itself read-only if that is what you meant." },
                    "syncRef": { "type": "boolean", "description": "Before starting, bring the remote machine's checkout to the SAME commit as this one, and refuse the dispatch if it cannot (usually: the commit is not pushed yet). Use this for any build or test you intend to compare across machines — without it you can get a build of stale code that looks entirely successful." }
                }),
                json!(["brief", "label"]),
            ),
        }),
        json!({
            "name": "dispatch_wait",
            "description":
                "Wait for dispatched workers to report, and return what they said. \"running\" is \
                 a NORMAL answer, not an error or a timeout: the work is still going and you \
                 should call this again. Real work takes minutes — a build can take half an \
                 hour — so expect to call this several times. Omit dispatchIds to wait on \
                 everything this chat has out.",
            "inputSchema": obj(
                json!({
                    "dispatchIds": { "type": "array", "items": { "type": "string" } },
                    "timeoutSeconds": { "type": "number", "description": "How long to wait before answering (default 25, max 25)" }
                }),
                json!([]),
            ),
        }),
        json!({
            "name": "dispatch_status",
            "description": "Snapshot of this chat's dispatched workers without waiting.",
            "inputSchema": obj(
                json!({ "dispatchIds": { "type": "array", "items": { "type": "string" } } }),
                json!([]),
            ),
        }),
        json!({
            "name": "dispatch_cancel",
            "description": "Stop dispatched workers that are no longer needed or have gone wrong. \
                Pass `dispatchIds` to stop several at once — unwinding a fan-out is the common \
                case. Cancelling is final: the worker's turn is interrupted and the dispatch is \
                recorded as cancelled, so a late report cannot resurrect it.",
            "inputSchema": obj(
                json!({
                    "dispatchId": { "type": "string", "description": "A single worker to stop" },
                    "dispatchIds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Several workers to stop, e.g. every target of one fan-out"
                    }
                }),
                json!([]),
            ),
        }),
        json!({
            "name": "report_result",
            "description":
                "Report the outcome of the work you were dispatched to do. Available only when \
                 another thread dispatched this one. Call it EXACTLY ONCE, as your final action. \
                 This report is the only thing the sender receives — it does not read your \
                 thread — so say what you did, what you verified, and what you could not do. \
                 Reporting failure honestly is far more useful than an optimistic summary.",
            "inputSchema": obj(
                json!({
                    "status": { "type": "string", "enum": ["succeeded", "failed"] },
                    "summary": { "type": "string", "description": "What happened, and what the sender needs to know to continue" },
                    "changed": { "type": "array", "items": { "type": "string" }, "description": "Files you changed" }
                }),
                json!(["status", "summary"]),
            ),
        }),
    ]
}

/// Handle a fleet tool, or return `None` when the name belongs to someone else.
pub async fn call(
    state: &ServerState,
    thread_id: &str,
    name: &str,
    args: &Value,
) -> Option<Result<String, String>> {
    let result = match name {
        "machines" => machines(state, thread_id).await,
        "run_on_machine" => run_on_machine(state, thread_id, args).await,
        "exec_status" => exec_status(state, thread_id, args).await,
        "exec_cancel" => exec_cancel(state, thread_id, args).await,
        "dispatch" => dispatch(state, thread_id, args).await,
        "dispatch_wait" => dispatch_wait(state, thread_id, args).await,
        "dispatch_status" => dispatch_snapshot(state, thread_id, args),
        "dispatch_cancel" => dispatch_cancel(state, thread_id, args).await,
        "report_result" => report_result(state, thread_id, args),
        _ => return None,
    };
    Some(result.map_err(|e| format!("{e:#}")))
}

// ---------------------------------------------------------------- dispatch ---

/// Send one brief to one or more machines. Deliberately does NOT go through
/// `require_execute_access`: a dispatch does not execute anything here, and the
/// worker's own access is narrowed to the parent's by `dispatch::narrow` — so a
/// read-only chat gets a read-only worker rather than a refusal.
async fn dispatch(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let brief = args
        .get("brief")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .context("missing required argument: brief")?;
    let label = args
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .context("missing required argument: label")?;

    // One machine or many; a single `machine` is just a fan-out of one.
    let mut targets: Vec<String> = args
        .get("machines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if targets.is_empty() {
        targets.push(
            args.get("machine")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
    }

    let mut sent = Vec::new();
    let mut refused = Vec::new();
    for spec in targets {
        let spec = spec.trim().to_string();
        let outcome = send_one(state, thread_id, args, brief, label, &spec).await;
        match outcome {
            Ok(record) => sent.push(record),
            // One machine being asleep must not silently cancel the fan-out to
            // the others, and must not be reported as success either.
            Err(e) => refused.push(json!({ "machine": spec, "error": format!("{e:#}") })),
        }
    }
    anyhow::ensure!(
        !sent.is_empty(),
        "no worker could be started: {}",
        refused
            .iter()
            .map(|r| format!(
                "{}: {}",
                r["machine"].as_str().unwrap_or("(this machine)"),
                r["error"].as_str().unwrap_or("")
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );

    let mut out = json!({
        "dispatched": sent,
        "note": "Workers are running in the background. Call `dispatch_wait` to collect their \
                 reports — expect to call it more than once for real work.",
    });
    if !refused.is_empty() {
        out["refused"] = json!(refused);
        out["warning"] = json!(
            "Some machines did not take the work. They are listed under `refused` and are NOT \
             running — do not report them as done."
        );
    }
    Ok(out.to_string())
}

async fn send_one(
    state: &ServerState,
    thread_id: &str,
    args: &Value,
    brief: &str,
    label: &str,
    machine_spec: &str,
) -> Result<Value> {
    let target = resolve_machine(state, Some(machine_spec).filter(|s| !s.is_empty()))?;
    let project_id = resolve_root(
        state,
        thread_id,
        &target,
        args.get("root").and_then(Value::as_str),
    )?;
    let label = if machine_spec.is_empty() {
        label.to_string()
    } else {
        // A fan-out's rows are unreadable if every one of them says "build".
        format!("{label} · {}", target.name)
    };

    let mut payload = json!({
        "parentThreadId": thread_id,
        "brief": brief,
        "label": label,
        "machineId": target.machine_id,
        "projectId": project_id,
    });
    for key in ["agent", "model", "effort", "access", "syncRef"] {
        if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
            payload[key] = v.clone();
        }
    }
    let record = crate::dispatch::handle(
        state,
        &crate::mobile::Principal::Master,
        "dispatch.create",
        &payload,
    )
    .await?;
    Ok(json!({
        "dispatchId": record["id"],
        "machine": target.name,
        "agent": record["agent"],
        "access": record["settings"]["access"],
        "childThreadId": record["childThreadId"],
        "status": record["status"],
    }))
}

/// Which dispatches a tool call is talking about: the named ones, or everything
/// this thread has out.
fn selected(state: &ServerState, thread_id: &str, args: &Value) -> Vec<crate::dispatch::DispatchRecord> {
    let ledger = &state.hub.dispatch;
    let named: Vec<String> = args
        .get("dispatchIds")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if named.is_empty() {
        return ledger.for_parent(thread_id);
    }
    named.iter().filter_map(|id| ledger.get(id)).collect()
}

fn render_dispatches(records: &[crate::dispatch::DispatchRecord]) -> Value {
    json!(records
        .iter()
        .map(|r| {
            let mut v = json!({
                "dispatchId": r.id,
                "label": r.label,
                "machineId": r.machine_id,
                "agent": r.agent,
                "status": r.status,
                "childThreadId": r.child_thread_id,
            });
            if let Some(result) = &r.result {
                v["summary"] = json!(result.summary);
                if !result.changed.is_empty() {
                    v["changed"] = json!(result.changed);
                }
                if !result.artifacts.is_empty() {
                    v["artifacts"] = json!(result
                        .artifacts
                        .iter()
                        .map(|a| json!({ "name": a.name, "artifactId": a.id, "machineId": a.machine_id }))
                        .collect::<Vec<_>>());
                }
                if result.inferred {
                    v["reportWasInferred"] = json!(true);
                    v["note"] = json!(
                        "This worker never filed a structured report; the summary is its last \
                         message and may not describe the real outcome. Check its thread before \
                         relying on it."
                    );
                }
            }
            v
        })
        .collect::<Vec<_>>())
}

async fn dispatch_wait(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let budget = Duration::from_secs(
        args.get("timeoutSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(TOOL_ANSWER_BUDGET)
            .clamp(1, TOOL_ANSWER_BUDGET),
    );
    let ids: Vec<String> = selected(state, thread_id, args)
        .into_iter()
        .map(|r| r.id)
        .collect();
    anyhow::ensure!(!ids.is_empty(), "this chat has not dispatched anything");

    // Poll the local ledger: the worker pushes its report here, so "waiting" is
    // watching our own state rather than holding anything open across a link.
    let began = Instant::now();
    loop {
        let records: Vec<_> = ids
            .iter()
            .filter_map(|id| state.hub.dispatch.get(id))
            .collect();
        let pending = records.iter().filter(|r| !r.status.is_done()).count();
        if pending == 0 || began.elapsed() >= budget {
            let mut out = json!({ "dispatches": render_dispatches(&records) });
            if pending > 0 {
                out["stillRunning"] = json!(pending);
                out["note"] = json!(
                    "Still working — this is not an error and nothing was interrupted. Call \
                     `dispatch_wait` again to keep waiting."
                );
            }
            return Ok(out.to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn dispatch_snapshot(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let records = selected(state, thread_id, args);
    Ok(json!({ "dispatches": render_dispatches(&records) }).to_string())
}

/// Cancel one worker or several. Takes the same `dispatchIds` array as
/// `dispatch_wait` and `dispatch_status`, because a fan-out that went wrong
/// went wrong on more than one machine — and a model that just called
/// `dispatch` with three machines should not have to unwind it one call at a
/// time. `dispatchId` stays accepted: it is what the tool asked for until now.
async fn dispatch_cancel(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let mut ids: Vec<String> = args
        .get("dispatchIds")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(one) = args.get("dispatchId").and_then(Value::as_str) {
        ids.push(one.to_string());
    }
    anyhow::ensure!(
        !ids.is_empty(),
        "name the worker(s) to stop: dispatchId, or dispatchIds for several"
    );
    ids.sort();
    ids.dedup();

    let mut cancelled = Vec::new();
    let mut refused = Vec::new();
    for id in ids {
        // Every id is checked against THIS chat before anything is stopped:
        // one stray id must not cancel a worker another thread is waiting on.
        let owned = state
            .hub
            .dispatch
            .get(&id)
            .is_some_and(|r| r.parent_thread_id == thread_id);
        if !owned {
            refused.push(json!({ "dispatchId": id, "why": "not a worker of this chat" }));
            continue;
        }
        match crate::dispatch::handle(
            state,
            &crate::mobile::Principal::Master,
            "dispatch.cancel",
            &json!({ "dispatchId": id }),
        )
        .await
        {
            Ok(_) => cancelled.push(id),
            // One that will not stop must be named, not folded into a
            // cheerful aggregate — the same rule the fan-out itself follows.
            Err(e) => refused.push(json!({ "dispatchId": id, "why": format!("{e:#}") })),
        }
    }
    let mut out = json!({ "ok": refused.is_empty(), "cancelled": cancelled });
    if !refused.is_empty() {
        out["refused"] = json!(refused);
    }
    Ok(out.to_string())
}

/// The worker's side of the contract. Stashed rather than applied immediately:
/// the dispatch concludes at the turn boundary, so an agent that reports and
/// then keeps working still ends with its own words.
fn report_result(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let thread = state
        .hub
        .store
        .thread(thread_id)
        .context("unknown thread")?;
    anyhow::ensure!(
        thread.dispatch.is_some(),
        "report_result is only for dispatched work — nobody sent this chat a brief"
    );
    let status = args
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("succeeded");
    let summary = args
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("a report needs a summary")?;
    let changed = args
        .get("changed")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    state.hub.stash_dispatch_report(
        thread_id,
        status != "failed",
        crate::dispatch::DispatchResult {
            summary: summary.to_string(),
            changed,
            artifacts: Vec::new(),
            inferred: false,
        },
    );
    Ok(json!({
        "ok": true,
        "note": "Report recorded. It is sent to whoever dispatched you when this turn ends, \
                 along with anything you published with publish_artifact. Stop now.",
    })
    .to_string())
}

async fn machines(state: &ServerState, thread_id: &str) -> Result<String> {
    let mut out = Vec::new();
    let me = resolve_machine(state, None)?;
    out.push(describe_machine(state, thread_id, &me, Some(state.device.info())).await);

    for peer in state.peernet.registry.list() {
        let target = Target {
            online: state.peernet.is_online(&peer.machine_id),
            machine_id: peer.machine_id.clone(),
            name: peer.name.clone(),
            is_self: false,
        };
        // Live info only from a machine that is actually there; an offline peer
        // still belongs in the list, just without capabilities.
        let info = if target.online && !peer.needs_upgrade() {
            rpc(state, &target, "device.info", json!({})).await.ok()
        } else {
            None
        };
        out.push(describe_machine(state, thread_id, &target, info).await);
    }
    Ok(json!({ "machines": out }).to_string())
}

async fn describe_machine(
    state: &ServerState,
    thread_id: &str,
    target: &Target,
    info: Option<Value>,
) -> Value {
    let get = |key: &str| {
        info.as_ref()
            .and_then(|i| i.get(key).cloned())
            .unwrap_or(Value::Null)
    };
    json!({
        "machineId": target.machine_id,
        "name": target.name,
        "isThisMachine": target.is_self,
        "online": target.online,
        "os": get("os"),
        "arch": get("arch"),
        "shell": if get("os").as_str() == Some("windows") { "powershell" } else { "sh" },
        "agents": get("capabilities"),
        "roots": roots_for(state, thread_id, &target.machine_id),
    })
}

async fn run_on_machine(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    require_execute_access(state, thread_id)?;
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .context("missing required argument: command")?;
    let target = resolve_machine(state, args.get("machine").and_then(Value::as_str))?;
    let project_id = resolve_root(
        state,
        thread_id,
        &target,
        args.get("root").and_then(Value::as_str),
    )?;

    let mut payload = json!({
        "command": command,
        "projectId": project_id,
    });
    if let Some(subdir) = args.get("subdir").and_then(Value::as_str) {
        payload["subdir"] = json!(subdir);
    }
    if let Some(timeout) = args.get("timeoutSeconds").and_then(Value::as_u64) {
        payload["timeoutSeconds"] = json!(timeout);
    }

    let started = rpc(state, &target, "exec.start", payload).await?;
    let job_id = started["jobId"]
        .as_str()
        .context("the machine did not return a job id")?
        .to_string();

    let snapshot = poll_until(state, &target, &job_id, started, RUN_WAIT_BUDGET).await?;
    Ok(render_job(&target, &snapshot, true))
}

/// Poll a job until it finishes or the budget runs out, accumulating output.
/// Each individual poll is short so nothing sits on a peer link.
async fn poll_until(
    state: &ServerState,
    target: &Target,
    job_id: &str,
    first: Value,
    budget: Duration,
) -> Result<Value> {
    let mut snapshot = first;
    let began = Instant::now();
    let mut stdout = snapshot["stdout"].as_str().unwrap_or_default().to_string();
    let mut stderr = snapshot["stderr"].as_str().unwrap_or_default().to_string();

    while snapshot["status"] == "running" && began.elapsed() < budget {
        let remaining = budget.saturating_sub(began.elapsed()).as_secs().max(1);
        let next = rpc(
            state,
            target,
            "exec.status",
            json!({
                "jobId": job_id,
                "waitSeconds": remaining.min(POLL_SLICE),
                "stdoutOffset": snapshot["stdoutOffset"],
                "stderrOffset": snapshot["stderrOffset"],
            }),
        )
        .await?;
        stdout.push_str(next["stdout"].as_str().unwrap_or_default());
        stderr.push_str(next["stderr"].as_str().unwrap_or_default());
        snapshot = next;
    }
    snapshot["stdout"] = json!(stdout);
    snapshot["stderr"] = json!(stderr);
    Ok(snapshot)
}

async fn exec_status(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    require_execute_access(state, thread_id)?;
    let job_id = args
        .get("jobId")
        .and_then(Value::as_str)
        .context("missing required argument: jobId")?;
    let target = resolve_machine(state, args.get("machine").and_then(Value::as_str))?;
    let budget = args
        .get("waitSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(TOOL_ANSWER_BUDGET)
        .clamp(0, TOOL_ANSWER_BUDGET);
    let first = rpc(
        state,
        &target,
        "exec.status",
        json!({ "jobId": job_id, "waitSeconds": budget.min(POLL_SLICE) }),
    )
    .await?;
    let snapshot = poll_until(
        state,
        &target,
        job_id,
        first,
        Duration::from_secs(budget),
    )
    .await?;
    Ok(render_job(&target, &snapshot, false))
}

async fn exec_cancel(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    require_execute_access(state, thread_id)?;
    let job_id = args
        .get("jobId")
        .and_then(Value::as_str)
        .context("missing required argument: jobId")?;
    let target = resolve_machine(state, args.get("machine").and_then(Value::as_str))?;
    rpc(state, &target, "exec.cancel", json!({ "jobId": job_id })).await?;
    Ok(json!({ "ok": true, "jobId": job_id, "status": "cancelled" }).to_string())
}

/// Keep the tail of a stream and say so, rather than quietly handing the model
/// a fragment it will treat as the whole thing.
fn tail(text: &str) -> Value {
    if text.len() <= TOOL_OUTPUT_TAIL {
        return json!(text);
    }
    let cut = (text.len() - TOOL_OUTPUT_TAIL..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    json!(format!(
        "[… {} earlier bytes omitted — this is the tail …]\n{}",
        cut,
        &text[cut..]
    ))
}

fn render_job(target: &Target, snapshot: &Value, include_hint: bool) -> String {
    let status = snapshot["status"].as_str().unwrap_or("unknown");
    let mut out = json!({
        "machine": target.name,
        "status": status,
        "exitCode": snapshot["exitCode"],
        "command": snapshot["command"],
        "cwd": snapshot["cwd"],
        "shell": snapshot["shell"],
        "elapsedMs": snapshot["elapsedMs"],
        "stdout": tail(snapshot["stdout"].as_str().unwrap_or_default()),
        "stderr": tail(snapshot["stderr"].as_str().unwrap_or_default()),
    });
    if let Some(error) = snapshot["error"].as_str() {
        out["error"] = json!(error);
    }
    if status == "running" {
        out["jobId"] = snapshot["jobId"].clone();
        if include_hint {
            out["note"] = json!(
                "Still running. This is not an error and the command was not interrupted — \
                 call `exec_status` with this jobId to keep watching it."
            );
        }
    } else if status == "exited" && snapshot["exitCode"] != json!(0) {
        out["note"] = json!(
            "The command ran and returned a non-zero exit code. That is a failure of the \
             command, not of the machine."
        );
    }
    out.to_string()
}

/// Agents whose CLI this machine advertises, as `Agent` values. Used by the
/// dispatch tools to refuse a target that cannot run what was asked for before
/// a thread is created there rather than after.
pub fn advertised_agents(info: &Value) -> Vec<Agent> {
    let Some(list) = info.get("capabilities").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(Value::as_str)
        .filter_map(|c| match c.strip_prefix("run-")? {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "kimi" => Some(Agent::Kimi),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_and_says_what_it_dropped() {
        let long = "x".repeat(TOOL_OUTPUT_TAIL + 500);
        let v = tail(&long);
        let s = v.as_str().unwrap();
        assert!(s.starts_with("[…"), "the omission is announced");
        assert!(s.ends_with('x'));
        assert!(s.len() < long.len());

        let short = "just this";
        assert_eq!(tail(short), json!("just this"));
    }

    #[test]
    fn tail_cuts_on_a_char_boundary() {
        // Multi-byte throughout: a naive byte slice panics here.
        let long = "─".repeat(TOOL_OUTPUT_TAIL);
        let v = tail(&long);
        assert!(v.as_str().unwrap().ends_with('─'));
    }

    #[test]
    fn advertised_agents_reads_the_capability_list() {
        let info = json!({ "capabilities": ["run-claude", "run-codex", "browser"] });
        let agents = advertised_agents(&info);
        assert_eq!(agents, vec![Agent::Claude, Agent::Codex]);
        assert!(advertised_agents(&json!({})).is_empty());
    }

    #[test]
    fn a_nonzero_exit_is_reported_as_the_commands_failure_not_the_machines() {
        let snapshot = json!({
            "status": "exited", "exitCode": 2, "command": "false", "cwd": "/tmp",
            "shell": "sh", "elapsedMs": 5, "stdout": "", "stderr": "", "error": Value::Null,
            "jobId": "j1",
        });
        let target = Target {
            machine_id: "m".into(),
            name: "mac".into(),
            is_self: false,
            online: true,
        };
        let rendered = render_job(&target, &snapshot, true);
        assert!(rendered.contains("not of the machine"));
        assert!(!rendered.contains("jobId"), "a finished job needs no handle");
    }

    #[test]
    fn a_still_running_job_hands_back_a_handle_and_says_it_is_not_an_error() {
        let snapshot = json!({
            "status": "running", "exitCode": Value::Null, "command": "cargo build",
            "cwd": "/x", "shell": "sh", "elapsedMs": 90_000, "stdout": "compiling",
            "stderr": "", "error": Value::Null, "jobId": "j2",
        });
        let target = Target {
            machine_id: "m".into(),
            name: "win".into(),
            is_self: false,
            online: true,
        };
        let rendered = render_job(&target, &snapshot, true);
        assert!(rendered.contains("j2"));
        assert!(rendered.contains("not an error"));
    }
}

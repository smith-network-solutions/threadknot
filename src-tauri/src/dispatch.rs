//! Dispatch — one thread handing a brief to another agent, possibly on another
//! machine, and getting a result back.
//!
//! A dispatch is a **child thread**, not another lane in the parent. Parley
//! models several agents as lanes sharing one transcript and one working tree,
//! and it cannot run them concurrently for two concrete reasons: artifact
//! baselines are keyed by thread, and two agents editing one checkout clobber
//! each other. A child thread has its own event log, its own driver, its own
//! artifact baseline, its own working directory and — the point of the whole
//! feature — its own machine. Every reason lanes must be sequential evaporates,
//! and the remote case comes free, because remote *threads* already work.
//!
//! What is paid for that is context: the worker's reasoning is not inline in the
//! parent's feed. It is a click away in a real thread rather than summarized
//! into nothing, which is the trade Threadknot should make — a forty-turn
//! implementation session inlined into a planner's transcript is unreadable, and
//! would not fit its context budget anyway.
//!
//! ## Both machines keep a record
//!
//! The ledger lives on the parent (it owns the answer) *and* on the worker (it
//! has to know where to send one). `role_here` reads the two machine ids to say
//! which copy this is. A worker whose parent is unreachable keeps the finished
//! report and re-sends it when the link comes back — completion is pushed, never
//! awaited on a peer request, because a build outlives every timeout on the way.

use crate::agents::Hub;
use crate::limits;
use crate::protocol::{
    new_id, now_iso, Access, Agent, AgentEvent, DispatchOrigin, Mode, Thread, ThreadSettings,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DispatchStatus {
    /// Waiting for another dispatch to release the same working tree.
    Queued,
    Running,
    Succeeded,
    /// The worker ran and reported failure, or its turn died.
    Failed,
    Cancelled,
}

impl DispatchStatus {
    pub fn is_done(self) -> bool {
        !matches!(self, DispatchStatus::Queued | DispatchStatus::Running)
    }
}

/// A deliverable the worker produced. The bytes stay on the machine that made
/// them; `machine_id` is what lets the parent render them through the existing
/// byte proxy instead of copying a build artifact across the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchArtifact {
    pub id: String,
    pub name: String,
    pub machine_id: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchResult {
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<DispatchArtifact>,
    #[serde(default)]
    pub changed: Vec<String>,
    /// True when the worker never called `report_result` and this summary was
    /// salvaged from its last message. Surfaced rather than hidden: a missing
    /// report must not read as a clean success.
    #[serde(default)]
    pub inferred: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchRecord {
    pub id: String,
    pub parent_thread_id: String,
    pub parent_machine_id: String,
    /// Empty until the worker machine has created the thread.
    #[serde(default)]
    pub child_thread_id: String,
    /// Where the work runs.
    pub machine_id: String,
    pub project_id: String,
    pub agent: Agent,
    pub settings: ThreadSettings,
    pub brief: String,
    pub label: String,
    pub status: DispatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<DispatchResult>,
    /// Hops from a human-driven thread; 1 for a dispatch a person's thread
    /// issued. Capped, because an agent that can dispatch agents that can
    /// dispatch agents is a fork bomb with a subscription attached.
    #[serde(default = "one")]
    pub depth: u8,
    /// Worker-side: when the report was accepted by the parent. `None` on a
    /// finished dispatch means it is still owed and will be re-sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
    /// Parent-side: when the completion was announced into the parent's thread.
    ///
    /// Separate from `result.is_some()` because on a LOCAL dispatch both sides
    /// are the same ledger — the worker writes its result and then hands it to
    /// the parent, so "has a result" is already true by the time the parent
    /// looks, and guarding on that swallowed the announcement entirely. What
    /// must not happen twice is the *announcement*, so that is what is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced_at: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

fn one() -> u8 {
    1
}

/// Which side of the dispatch this machine is on. Both, when a thread dispatches
/// to its own machine — which is the ordinary cross-*harness* case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Parent,
    Worker,
    Both,
    Neither,
}

impl DispatchRecord {
    pub fn role_here(&self, machine_id: &str) -> Role {
        match (
            self.parent_machine_id == machine_id,
            self.machine_id == machine_id,
        ) {
            (true, true) => Role::Both,
            (true, false) => Role::Parent,
            (false, true) => Role::Worker,
            (false, false) => Role::Neither,
        }
    }

    pub fn is_local(&self) -> bool {
        self.parent_machine_id == self.machine_id
    }
}

#[derive(Serialize, Deserialize, Default)]
struct LedgerFile {
    #[serde(default)]
    dispatches: Vec<DispatchRecord>,
}

pub struct DispatchLedger {
    path: PathBuf,
    records: Mutex<Vec<DispatchRecord>>,
}

impl DispatchLedger {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("dispatches.json");
        let records = if path.exists() {
            let file: LedgerFile =
                serde_json::from_str(&std::fs::read_to_string(&path).context("read dispatches")?)
                    .context("parse dispatches.json")?;
            file.dispatches
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            records: Mutex::new(records),
        })
    }

    fn flush(&self, records: &[DispatchRecord]) {
        // Trim finished records so the file cannot grow without bound; the live
        // ones are never dropped.
        let mut keep: Vec<&DispatchRecord> = records.iter().filter(|r| !r.status.is_done()).collect();
        let mut done: Vec<&DispatchRecord> = records.iter().filter(|r| r.status.is_done()).collect();
        done.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let overflow = done.len().saturating_sub(limits::MAX_REMEMBERED_DISPATCHES);
        keep.extend(done.into_iter().skip(overflow));
        let file = LedgerFile {
            dispatches: keep.into_iter().cloned().collect(),
        };
        let write = || -> Result<()> {
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::error!("persisting the dispatch ledger failed: {e:#}");
        }
    }

    pub fn upsert(&self, record: DispatchRecord) {
        let mut records = self.records.lock().unwrap();
        match records.iter_mut().find(|r| r.id == record.id) {
            Some(existing) => *existing = record,
            None => records.push(record),
        }
        self.flush(&records);
    }

    pub fn get(&self, id: &str) -> Option<DispatchRecord> {
        self.records.lock().unwrap().iter().find(|r| r.id == id).cloned()
    }

    pub fn by_child_thread(&self, thread_id: &str) -> Option<DispatchRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.child_thread_id == thread_id)
            .cloned()
    }

    pub fn update<F: FnOnce(&mut DispatchRecord)>(&self, id: &str, f: F) -> Option<DispatchRecord> {
        let mut records = self.records.lock().unwrap();
        let found = records.iter_mut().find(|r| r.id == id)?;
        f(found);
        let updated = found.clone();
        self.flush(&records);
        Some(updated)
    }

    pub fn list(&self) -> Vec<DispatchRecord> {
        self.records.lock().unwrap().clone()
    }

    pub fn for_parent(&self, thread_id: &str) -> Vec<DispatchRecord> {
        let mut out: Vec<_> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.parent_thread_id == thread_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        out
    }

    pub fn live_for_parent(&self, thread_id: &str) -> usize {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.parent_thread_id == thread_id && !r.status.is_done())
            .count()
    }

    fn running_on(&self, machine_id: &str) -> usize {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.machine_id == machine_id && r.status == DispatchStatus::Running)
            .count()
    }

    /// Is some other dispatch already working in this checkout? Two agents in
    /// one working tree is the hazard that makes Parley's lanes sequential; here
    /// the answer is to queue rather than to interleave.
    fn tree_busy(&self, machine_id: &str, project_id: &str, except: &str) -> bool {
        self.records.lock().unwrap().iter().any(|r| {
            r.id != except
                && r.machine_id == machine_id
                && r.project_id == project_id
                && r.status == DispatchStatus::Running
        })
    }

    fn next_queued(&self, machine_id: &str, project_id: &str) -> Option<DispatchRecord> {
        let records = self.records.lock().unwrap();
        let mut queued: Vec<&DispatchRecord> = records
            .iter()
            .filter(|r| {
                r.machine_id == machine_id
                    && r.project_id == project_id
                    && r.status == DispatchStatus::Queued
            })
            .collect();
        queued.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        queued.first().map(|r| (*r).clone()).clone()
    }

    /// Finished dispatches this machine owes a report for.
    pub fn unreported(&self, local_machine_id: &str) -> Vec<DispatchRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                r.status.is_done()
                    && r.reported_at.is_none()
                    && matches!(r.role_here(local_machine_id), Role::Worker)
            })
            .cloned()
            .collect()
    }
}

// ------------------------------------------------------------- narrowing ---

fn rank(access: Access) -> u8 {
    match access {
        Access::Read => 0,
        Access::Edits => 1,
        Access::Full => 2,
    }
}

/// A dispatch may only ever narrow. There is no request a parent can make that
/// grants its worker more than the parent itself holds, and a target machine's
/// own ceiling narrows it further — mesh membership is not consent to have
/// arbitrary agents run at full tilt on your laptop.
pub fn narrow(requested: Option<Access>, parent: Access, target_ceiling: Access) -> Access {
    let want = requested.unwrap_or(parent);
    [want, parent, target_ceiling]
        .into_iter()
        .min_by_key(|a| rank(*a))
        .unwrap_or(Access::Read)
}

// ----------------------------------------------------------------- briefs ---

/// The prompt a worker wakes up to. Written to be readable by Codex and Kimi as
/// well as Claude, because choosing the harness is the entire point.
pub fn brief_for(record: &DispatchRecord, parent_title: &str, from_machine: &str) -> String {
    let mut out = String::new();
    out.push_str(
        "You are running as a **dispatched worker** inside Threadknot. Another agent is \
         planning a piece of work and has handed you one concrete job from it.\n\n",
    );
    out.push_str(&format!(
        "- Sent by: the thread \"{parent_title}\" on machine `{from_machine}`\n\
         - Your job: `{}`\n\n",
        record.label
    ));
    out.push_str("## The brief\n\n");
    out.push_str(record.brief.trim());
    out.push_str("\n\n## How to finish\n\n");
    out.push_str(
        "Do the work, then **call the `report_result` tool exactly once** as your final \
         action. That call is the only thing the sender receives — it does not read this \
         thread. A report that says the work failed, and why, is far more useful than \
         silence or an optimistic summary.\n\n\
         In `summary`, state what you actually did and what the sender needs to know to \
         continue: what changed, what you verified, what you could not do. If you produced \
         a file the sender asked for, publish it with `publish_artifact` first, and it will \
         be attached to your report automatically.\n\n\
         Do not ask the sender questions — it cannot answer mid-run. If the brief is \
         ambiguous, make the most reasonable interpretation, do the work, and say plainly \
         in your report which interpretation you took.",
    );
    if record.settings.access != Access::Full {
        out.push_str(&format!(
            "\n\nYou are running with **{}** access, so some actions will be refused. Report \
             what you were unable to do rather than working around it.",
            match record.settings.access {
                Access::Read => "read-only",
                Access::Edits => "edits-only",
                Access::Full => "full",
            }
        ));
    }
    out
}

// ------------------------------------------------------------- lifecycle ---

/// Create the worker thread on THIS machine and (unless queued behind another
/// dispatch in the same checkout) start its turn.
///
/// Runs on the machine that will do the work: called directly for a local
/// dispatch, and over `mesh.dispatchStart` for a remote one.
pub fn begin_worker(hub: &Arc<Hub>, mut record: DispatchRecord, autostart: bool) -> Result<DispatchRecord> {
    let store = &hub.store;
    anyhow::ensure!(
        store.project(&record.project_id).is_some(),
        "unknown project on this machine — attach that root to the workspace first"
    );
    anyhow::ensure!(
        hub.dispatch.running_on(&record.machine_id) < limits::MAX_LIVE_DISPATCHES_PER_MACHINE,
        "{} already has {} dispatched workers running, which is the limit",
        record.machine_id,
        limits::MAX_LIVE_DISPATCHES_PER_MACHINE
    );

    let thread = store.create_thread(
        record.project_id.clone(),
        record.agent,
        record.settings.clone(),
    )?;
    let origin = DispatchOrigin {
        id: record.id.clone(),
        parent_thread_id: record.parent_thread_id.clone(),
        parent_machine_id: record.parent_machine_id.clone(),
        label: record.label.clone(),
    };
    let title = format!("⇢ {}", record.label);
    store.update_thread(&thread.id, |t| {
        t.dispatch = Some(origin);
        t.title = title;
    })?;
    record.child_thread_id = thread.id.clone();

    // Queue behind whatever is already writing in this checkout rather than
    // interleaving two agents in one working tree.
    let busy = hub
        .dispatch
        .tree_busy(&record.machine_id, &record.project_id, &record.id);
    record.status = if busy || !autostart {
        DispatchStatus::Queued
    } else {
        DispatchStatus::Running
    };
    if record.status == DispatchStatus::Running {
        record.started_at = Some(now_iso());
    }
    hub.dispatch.upsert(record.clone());
    hub.broadcast_state("threads", Some(thread.project_id.clone()));
    hub.broadcast_state("dispatches", None);

    if record.status == DispatchStatus::Running {
        start_worker_turn(hub, &record)?;
    }
    Ok(record)
}

fn start_worker_turn(hub: &Arc<Hub>, record: &DispatchRecord) -> Result<()> {
    let parent_title = hub
        .store
        .thread(&record.parent_thread_id)
        .map(|t| t.title)
        .unwrap_or_else(|| "a planning thread".into());
    let from = if record.is_local() {
        "this machine".to_string()
    } else {
        record.parent_machine_id.clone()
    };
    let brief = brief_for(record, &parent_title, &from);
    hub.start_dispatch_turn(&record.child_thread_id, brief)
}

/// Turn the worker's ending into a result. Called at the child's turn boundary.
///
/// `reported` is what the agent passed to `report_result`, when it remembered
/// to. Models forget, so a missing report is salvaged from the last thing it
/// said and flagged `inferred` — a dispatch that silently reads as a clean
/// success is worse than one that admits it is guessing.
pub fn conclude_worker(
    hub: &Arc<Hub>,
    thread_id: &str,
    completed: bool,
    reported: Option<(bool, DispatchResult)>,
) {
    let Some(record) = hub.dispatch.by_child_thread(thread_id) else {
        return;
    };
    if record.status.is_done() {
        return;
    }
    let (status, mut result) = match reported {
        Some((ok, result)) => (
            if ok {
                DispatchStatus::Succeeded
            } else {
                DispatchStatus::Failed
            },
            result,
        ),
        None => {
            let summary = hub
                .store
                .last_assistant_text(thread_id)
                .unwrap_or_else(|| {
                    "The worker finished without saying anything. Open its thread to see what \
                     happened."
                        .to_string()
                });
            (
                if completed {
                    DispatchStatus::Succeeded
                } else {
                    DispatchStatus::Failed
                },
                DispatchResult {
                    summary: truncate(&summary, limits::DISPATCH_SUMMARY_MAX),
                    inferred: true,
                    ..Default::default()
                },
            )
        }
    };
    result.summary = truncate(&result.summary, limits::DISPATCH_SUMMARY_MAX);
    // Anything the worker published during its turn is part of the deliverable
    // whether or not it thought to list it.
    let published = hub.store.list_artifacts_for_thread(thread_id);
    for artifact in published {
        if !result.artifacts.iter().any(|a| a.id == artifact.id) {
            result.artifacts.push(DispatchArtifact {
                id: artifact.id,
                name: artifact.name,
                machine_id: hub.store.local_machine_id(),
                mime_type: artifact.mime_type,
                size_bytes: artifact.size_bytes,
            });
        }
    }

    let Some(updated) = hub.dispatch.update(&record.id, |r| {
        r.status = status;
        r.result = Some(result.clone());
        r.finished_at = Some(now_iso());
    }) else {
        return;
    };
    hub.broadcast_state("dispatches", None);
    deliver_report(hub, &updated);
    release_tree(hub, &updated);
}

/// Hand the finished record to whoever is waiting for it. Local dispatches
/// short-circuit; remote ones push over the mesh and keep the report if the
/// parent is not there.
pub fn deliver_report(hub: &Arc<Hub>, record: &DispatchRecord) {
    if record.is_local() {
        accept_report(hub, &record.id, record.status, record.result.clone());
        hub.dispatch.update(&record.id, |r| {
            r.reported_at = Some(now_iso())
        });
        return;
    }
    let Some(peernet) = hub.peernet() else {
        return;
    };
    let hub2 = Arc::clone(hub);
    let record = record.clone();
    tokio::spawn(async move {
        let payload = json!({
            "dispatchId": record.id,
            "status": record.status,
            "result": record.result,
        });
        match peernet
            .request(&record.parent_machine_id, "mesh.dispatchReport", payload, None)
            .await
        {
            Ok(_) => {
                hub2.dispatch
                    .update(&record.id, |r| r.reported_at = Some(now_iso()));
            }
            Err(e) => {
                // Not an error worth shouting about: the parent being asleep is
                // ordinary, and the record stays owed until it is delivered.
                tracing::info!(
                    "dispatch {} report deferred ({e:#}) — will retry when {} reconnects",
                    record.id,
                    record.parent_machine_id
                );
            }
        }
    });
}

/// Re-send every report this machine still owes. Called when a peer link comes
/// up, so a worker that finished while the parent was away is not lost.
pub fn flush_pending_reports(hub: &Arc<Hub>, machine_id: &str) {
    let local = hub.store.local_machine_id();
    for record in hub.dispatch.unreported(&local) {
        if record.parent_machine_id == machine_id {
            deliver_report(hub, &record);
        }
    }
}

/// Ask the worker machine how a dispatch ended, for the cases where it could
/// not tell us itself.
///
/// Completion is pushed, and that is the right default — it is immediate and
/// costs nothing while everyone is reachable. But "the worker can always open a
/// connection back to the parent" is an assumption real networks break in both
/// boring and exotic ways: a firewall that allows one direction, NAT, a laptop
/// that moved, or — the case that found this — macOS Local Network privacy
/// silently refusing outbound LAN connections from an unsigned binary with
/// `EHOSTUNREACH` while the same machine happily accepts inbound ones.
///
/// Without this, such a dispatch is finished on the worker and `Running`
/// forever on the parent: the work was done, and the answer is simply stranded.
/// So the side that *can* reach the other also checks, and either direction
/// working is enough.
/// Whether the parent should fall back to asking a worker directly.
///
/// Pure, so the rule that decides how chatty the mesh gets is testable rather
/// than buried in a loop. `None` — never heard anything — counts as silence:
/// a worker that could never reach us must not get a grace period before we
/// notice, or its first minutes look identical to a hung one.
pub(crate) fn should_poll(silence: Option<std::time::Duration>) -> bool {
    silence.is_none_or(|since| since >= limits::DISPATCH_PUSH_SILENCE)
}

pub fn spawn_reconciler(state: crate::server::ServerState) {
    tokio::spawn(async move {
        // Last activity line we relayed from a poll, so polling does not
        // re-emit the same line every couple of seconds.
        let mut echoed: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        loop {
            tokio::time::sleep(limits::DISPATCH_RECONCILE_INTERVAL).await;
            let local = state.device.machine_id.clone();
            let quiet: Vec<DispatchRecord> = state
                .hub
                .dispatch
                .list()
                .into_iter()
                .filter(|r| {
                    !r.status.is_done()
                        && matches!(r.role_here(&local), Role::Parent)
                        && !r.child_thread_id.is_empty()
                        && state.peernet.is_online(&r.machine_id)
                        // The whole point: a worker whose pushes are arriving is
                        // never polled. Only silence costs a request.
                        && should_poll(state.hub.dispatch_silence(&r.id))
                })
                .collect();
            for record in quiet {
                let asked = state
                    .peernet
                    .request(
                        &record.machine_id,
                        "dispatch.get",
                        json!({ "dispatchId": record.id }),
                        None,
                    )
                    .await;
                let remote = match asked {
                    Ok(remote) => remote,
                    Err(e) => {
                        tracing::debug!("reconcile poll of {} failed: {e:#}", record.id);
                        continue;
                    }
                };
                tracing::debug!(
                    "reconcile poll {} -> status={:?} lastActivity={:?}",
                    record.id,
                    remote.get("status"),
                    remote.get("lastActivity")
                );

                // Progress first: a worker that cannot reach us is still doing
                // something, and "building… 4m elapsed" is the difference
                // between a live row and one that looks hung.
                if let Some(line) = remote.get("lastActivity").and_then(|v| v.as_object()) {
                    let text = line
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let activity = line
                        .get("activity")
                        .and_then(Value::as_str)
                        .unwrap_or("status")
                        .to_string();
                    if !text.is_empty() && echoed.get(&record.id) != Some(&text) {
                        echoed.insert(record.id.clone(), text.clone());
                        state.hub.emit(
                            &record.parent_thread_id,
                            AgentEvent::SubagentProgress {
                                task_id: record.id.clone(),
                                activity,
                                text,
                            },
                        );
                    }
                }

                let status: Option<DispatchStatus> = remote
                    .get("status")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let Some(status) = status.filter(|s| s.is_done()) else {
                    continue;
                };
                let result: Option<DispatchResult> = remote
                    .get("result")
                    .filter(|v| !v.is_null())
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                tracing::info!(
                    "dispatch {} finished on {} but its report never arrived — adopting it",
                    record.id,
                    record.machine_id
                );
                echoed.remove(&record.id);
                accept_report(&state.hub, &record.id, status, result);
            }
        }
    });
}

/// Apply a worker's report on the PARENT machine.
pub fn accept_report(
    hub: &Arc<Hub>,
    dispatch_id: &str,
    status: DispatchStatus,
    result: Option<DispatchResult>,
) {
    let Some(record) = hub.dispatch.get(dispatch_id) else {
        tracing::warn!("report for unknown dispatch {dispatch_id}");
        return;
    };
    // A worker retries its report until the parent acknowledges, so the same
    // result can arrive several times; the parent's thread must only hear once.
    if record.announced_at.is_some() {
        return;
    }
    let Some(updated) = hub.dispatch.update(dispatch_id, |r| {
        r.status = status;
        // A re-delivery carrying nothing must not erase what we already have.
        if result.is_some() {
            r.result = result.clone();
        }
        r.finished_at = Some(now_iso());
        r.announced_at = Some(now_iso());
    }) else {
        return;
    };
    let summary = updated
        .result
        .as_ref()
        .map(|r| r.summary.clone())
        .unwrap_or_default();
    hub.note_dispatch_heard(&updated.id);
    hub.forget_dispatch_tracking(&updated.id);
    hub.emit(
        &updated.parent_thread_id,
        AgentEvent::SubagentCompleted {
            task_id: updated.id.clone(),
            status: match status {
                DispatchStatus::Succeeded => "completed".into(),
                DispatchStatus::Cancelled => "cancelled".into(),
                _ => "error".into(),
            },
            summary: Some(summary),
        },
    );
    // A scheduled dispatch's parent runs no turn of its own, so no
    // `TurnCompleted` will ever settle it — this is what does. A no-op for an
    // agent-issued dispatch, whose parent settled when its own turn ended.
    crate::schedules::settle_if_idle(hub, &updated.parent_thread_id);
    push_finished(hub, &updated);
    hub.broadcast_state("dispatches", None);
}

/// Tell the phone a worker is done. The value is entirely in the away case: a
/// nightly cross-platform build finishes at 03:00 and the only thing that can
/// say so is a push. Deliberately NOT folded into `push_for_event`'s
/// `SubagentCompleted` arm — that would also fire for a provider's own
/// in-process subagents, which are steps inside a turn the user is watching,
/// and would bury the one notification that matters under a dozen that don't.
fn push_finished(hub: &Arc<Hub>, record: &DispatchRecord) {
    let Some(push) = hub.push() else { return };
    let Some(parent) = hub.store.thread(&record.parent_thread_id) else {
        return;
    };
    let Some(workspace_id) = hub.store.workspace_for_project(&parent.project_id) else {
        return;
    };
    let project_name = hub
        .store
        .project(&parent.project_id)
        .map(|p| p.name)
        .unwrap_or_default();
    let outcome = match record.status {
        DispatchStatus::Succeeded => "finished",
        DispatchStatus::Cancelled => "was cancelled",
        _ => "failed",
    };
    let detail = record
        .result
        .as_ref()
        .map(|r| truncate(&r.summary, 160))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{} {outcome}", record.agent.display_name()));
    push.enqueue(crate::push::PushJob {
        kind: crate::push::PushKind::DispatchFinished,
        project_id: parent.project_id.clone(),
        workspace_id,
        project_name,
        thread_id: parent.id.clone(),
        thread_title: parent.title.clone(),
        notice: Some(crate::protocol::EventNotice {
            title: format!("{} {outcome} · {}", record.label, parent.title),
            body: detail,
        }),
        only_device: None,
    });
}

/// A dispatch finished: let the next one queued for the same checkout start.
fn release_tree(hub: &Arc<Hub>, finished: &DispatchRecord) {
    let Some(next) = hub
        .dispatch
        .next_queued(&finished.machine_id, &finished.project_id)
    else {
        return;
    };
    if hub
        .dispatch
        .tree_busy(&next.machine_id, &next.project_id, &next.id)
    {
        return;
    }
    let started = hub.dispatch.update(&next.id, |r| {
        r.status = DispatchStatus::Running;
        r.started_at = Some(now_iso());
    });
    if let Some(record) = started {
        if let Err(e) = start_worker_turn(hub, &record) {
            tracing::error!("queued dispatch {} failed to start: {e:#}", record.id);
            conclude_failure(hub, &record.id, &format!("could not start: {e:#}"));
        }
    }
}

/// Mark a dispatch failed without a worker turn ever having run.
pub fn conclude_failure(hub: &Arc<Hub>, dispatch_id: &str, why: &str) {
    let Some(updated) = hub.dispatch.update(dispatch_id, |r| {
        r.status = DispatchStatus::Failed;
        r.finished_at = Some(now_iso());
        r.result = Some(DispatchResult {
            summary: why.to_string(),
            ..Default::default()
        });
    }) else {
        return;
    };
    deliver_report(hub, &updated);
    hub.broadcast_state("dispatches", None);
}

fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let cut = (0..=max)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}…", &text[..cut])
}

/// Wire shape for a client or a peer.
pub fn to_wire(record: &DispatchRecord, local_machine_id: &str) -> Value {
    let mut v = serde_json::to_value(record).unwrap_or(json!({}));
    v["isLocal"] = json!(record.is_local());
    v["roleHere"] = json!(match record.role_here(local_machine_id) {
        Role::Parent => "parent",
        Role::Worker => "worker",
        Role::Both => "both",
        Role::Neither => "neither",
    });
    // The brief can be long and the client shows it on demand; the ledger view
    // does not need every byte of it in every list response.
    if let Some(brief) = v.get("brief").and_then(Value::as_str) {
        v["briefPreview"] = json!(truncate(brief, 240));
    }
    v
}

/// Settings for a worker: the parent's, with access narrowed, always in Build
/// mode. Plan mode would have the worker raise plan-approval cards at a sender
/// that cannot answer them.
pub fn worker_settings(
    parent: &Thread,
    agent: Agent,
    model: Option<String>,
    effort: Option<String>,
    access: Access,
) -> Result<ThreadSettings> {
    let mut settings = parent.settings.clone();
    settings.access = access;
    settings.mode = Mode::Build;
    match model.filter(|m| !m.trim().is_empty()) {
        Some(model) => settings.model = model,
        // Model ids do not survive the crossing between providers. Inheriting
        // the parent's is right for a same-agent worker and catastrophic
        // otherwise: Codex handed `claude-opus-5` fails at the API with a
        // message about ChatGPT accounts that names nothing real.
        None if agent != parent.agent => {
            settings.model = crate::agents::default_model_for(agent)?
        }
        None => {}
    }
    match effort {
        Some(e) => settings.effort = Some(e),
        // Effort vocabularies are per-provider; one inherited from a different
        // agent kind may not exist there.
        None if agent != parent.agent => settings.effort = None,
        None => {}
    }
    // A worker does not inherit the parent's signed-in browser identity — that
    // is a credential, and handing it to a delegated agent is not implied by
    // "do this build".
    settings.browser_profile_id = None;
    Ok(settings)
}

/// Mint the record for a new dispatch. Validation that needs the target machine
/// (does it accept dispatch, does it have that agent) happens in `server.rs`,
/// which is where the mesh lives.
#[allow(clippy::too_many_arguments)]
pub fn new_record(
    parent: &Thread,
    parent_machine_id: &str,
    machine_id: String,
    project_id: String,
    agent: Agent,
    settings: ThreadSettings,
    brief: String,
    label: String,
    depth: u8,
) -> DispatchRecord {
    DispatchRecord {
        id: new_id(),
        parent_thread_id: parent.id.clone(),
        parent_machine_id: parent_machine_id.to_string(),
        child_thread_id: String::new(),
        machine_id,
        project_id,
        agent,
        settings,
        brief,
        label,
        status: DispatchStatus::Queued,
        result: None,
        depth,
        reported_at: None,
        announced_at: None,
        created_at: now_iso(),
        started_at: None,
        finished_at: None,
    }
}

/// How deep a dispatch issued by this thread would be, and whether that is
/// allowed at all.
pub fn depth_for(parent: &Thread, ledger: &DispatchLedger) -> Result<u8> {
    let parent_depth = parent
        .dispatch
        .as_ref()
        .and_then(|o| ledger.get(&o.id))
        .map(|r| r.depth)
        .unwrap_or(0);
    let depth = parent_depth + 1;
    anyhow::ensure!(
        depth <= limits::MAX_DISPATCH_DEPTH,
        "a dispatched worker cannot dispatch further work (depth {} of {} allowed) — \
         report back to the thread that sent you instead",
        depth,
        limits::MAX_DISPATCH_DEPTH
    );
    Ok(depth)
}

/// Refuse a parent that already has too many workers out.
pub fn check_parent_budget(ledger: &DispatchLedger, parent_thread_id: &str) -> Result<()> {
    let live = ledger.live_for_parent(parent_thread_id);
    anyhow::ensure!(
        live < limits::MAX_LIVE_DISPATCHES_PER_PARENT,
        "this thread already has {live} workers running, which is the limit — \
         wait for one to report before sending more"
    );
    Ok(())
}

// ------------------------------------------------------------------- RPC ---

/// What a target machine will accept, asked of the machine itself rather than
/// assumed. An offline peer answers nothing, which is the refusal.
struct TargetPolicy {
    accepts: bool,
    ceiling: Access,
    agents: Vec<Agent>,
}

async fn target_policy(
    state: &crate::server::ServerState,
    machine_id: &str,
) -> Result<TargetPolicy> {
    let info = if machine_id == state.device.machine_id {
        state.device.info()
    } else {
        state
            .peernet
            .request(machine_id, "device.info", json!({}), None)
            .await
            .context("could not reach that machine")?
    };
    Ok(TargetPolicy {
        // An older build has no opinion; treat silence as consent, since it
        // predates the setting and its owner never declined.
        accepts: info
            .get("acceptsDispatch")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        ceiling: info
            .get("maxDispatchAccess")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(Access::Full),
        agents: crate::mcp_fleet::advertised_agents(&info),
    })
}

fn parse_agent(raw: Option<&str>, fallback: Agent) -> Result<Agent> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(fallback);
    };
    serde_json::from_value(json!(raw.to_ascii_lowercase()))
        .with_context(|| format!("unknown agent \"{raw}\" — try claude, codex or kimi"))
}

/// `dispatch.*` and the `mesh.dispatch*` half that carries them between
/// machines.
pub async fn handle(
    state: &crate::server::ServerState,
    principal: &crate::mobile::Principal,
    kind: &str,
    p: &Value,
) -> Result<Value> {
    let hub = &state.hub;
    let local = state.device.machine_id.clone();
    let field = |key: &str| -> Result<String> {
        p.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("missing {key}"))
    };

    match kind {
        "dispatch.create" => {
            // A dispatch is "create a thread" and "run code in it" at once, and
            // the capability table can only name one. The other is named here.
            principal.require(crate::mobile::Capability::Threads)?;
            let parent_thread_id = field("parentThreadId")?;
            // The ledger belongs to the parent's machine, so a request about a
            // thread that lives elsewhere is forwarded there rather than
            // answered here. Note this routes on the PARENT's machine, which is
            // why `dispatch.create` cannot join the generic `machineId` router:
            // there, `machineId` names the worker.
            let Some(parent) = hub.store.thread(&parent_thread_id) else {
                let owner = p
                    .get("parentMachineId")
                    .and_then(Value::as_str)
                    .filter(|m| *m != local)
                    .context("unknown thread")?;
                return state
                    .peernet
                    .request(owner, kind, p.clone(), principal.mesh_assertion())
                    .await;
            };

            let depth = depth_for(&parent, &hub.dispatch)?;
            check_parent_budget(&hub.dispatch, &parent_thread_id)?;

            let machine_id = p
                .get("machineId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| local.clone());
            let agent = parse_agent(p.get("agent").and_then(Value::as_str), parent.agent)?;

            let policy = target_policy(state, &machine_id).await?;
            anyhow::ensure!(
                policy.accepts,
                "that machine is not accepting dispatched work (Settings → machines)"
            );
            anyhow::ensure!(
                policy.agents.is_empty() || policy.agents.contains(&agent),
                "that machine does not have {} installed — it has: {}",
                agent.display_name(),
                policy
                    .agents
                    .iter()
                    .map(|a| a.display_name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let requested: Option<Access> = p
                .get("access")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let access = narrow(requested, parent.settings.access, policy.ceiling);
            let settings = worker_settings(
                &parent,
                agent,
                p.get("model").and_then(Value::as_str).map(str::to_string),
                p.get("effort").and_then(Value::as_str).map(str::to_string),
                access,
            )?;

            let project_id = field("projectId")?;
            let brief = field("brief")?;
            anyhow::ensure!(!brief.trim().is_empty(), "a dispatch needs a brief");
            let label = p
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|l| !l.trim().is_empty())
                .unwrap_or_else(|| "delegated work".to_string());
            let autostart = p
                .get("autostart")
                .and_then(Value::as_bool)
                .unwrap_or(true);

            let record = new_record(
                &parent, &local, machine_id, project_id, agent, settings, brief, label, depth,
            );

            // Before anything starts, make the worker's checkout match ours —
            // and refuse rather than build the wrong commit.
            let synced = if p
                .get("syncRef")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !record.is_local()
            {
                Some(sync_worker_checkout(state, &parent.project_id, &record).await?)
            } else {
                None
            };

            let stored = if record.machine_id == local {
                begin_worker(hub, record, autostart)?
            } else {
                // The worker machine creates the thread and owns the process;
                // we keep the ledger entry that says we are owed an answer.
                let started = state
                    .peernet
                    .request(
                        &record.machine_id,
                        "mesh.dispatchStart",
                        json!({ "record": record, "autostart": autostart }),
                        None,
                    )
                    .await
                    .context("the target machine refused the dispatch")?;
                let mut stored = record;
                stored.child_thread_id = started
                    .get("childThreadId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                stored.status = serde_json::from_value(
                    started.get("status").cloned().unwrap_or(json!("running")),
                )
                .unwrap_or(DispatchStatus::Running);
                stored.started_at = Some(now_iso());
                hub.dispatch.upsert(stored.clone());
                stored
            };

            announce_start(state, &stored);
            hub.broadcast_state("dispatches", None);
            let mut wire = to_wire(&stored, &local);
            if let Some(sha) = synced {
                wire["syncedTo"] = json!(sha);
            }
            Ok(wire)
        }

        "dispatch.list" => {
            let records = match p.get("threadId").and_then(Value::as_str) {
                Some(thread_id) => hub.dispatch.for_parent(thread_id),
                None => hub.dispatch.list(),
            };
            Ok(json!({
                "dispatches": records.iter().map(|r| to_wire(r, &local)).collect::<Vec<_>>()
            }))
        }

        "dispatch.get" => {
            let id = field("dispatchId")?;
            let record = hub.dispatch.get(&id).context("unknown dispatch")?;
            let mut wire = to_wire(&record, &local);
            // Ride the worker's latest activity along, so a parent that has had
            // to fall back to asking still gets live progress and not just a
            // status word.
            if let Some((activity, text)) = hub.dispatch_activity(&id) {
                wire["lastActivity"] = json!({ "activity": activity, "text": text });
            }
            Ok(wire)
        }

        "dispatch.cancel" => {
            let id = field("dispatchId")?;
            let record = hub.dispatch.get(&id).context("unknown dispatch")?;
            anyhow::ensure!(!record.status.is_done(), "that dispatch already finished");
            // Stop the work first, then record it: a cancel that marks the
            // ledger and leaves an agent running is the worst of both.
            if record.machine_id == local {
                let _ = hub.interrupt(&record.child_thread_id);
            } else {
                // Tell the worker machine it is CANCELLED, not merely
                // interrupted. Interrupting alone ends the turn, and the turn
                // boundary there infers a report from the last assistant
                // message and files it as `failed` — which then lands on the
                // parent and overwrites the cancel. Marking the worker's own
                // copy first makes `conclude_worker` see a finished record and
                // say nothing.
                let cancelled = state
                    .peernet
                    .request(
                        &record.machine_id,
                        "mesh.dispatchCancel",
                        json!({ "dispatchId": id }),
                        None,
                    )
                    .await;
                if cancelled.is_err() {
                    // A worker on an older build does not know that request.
                    // Fall back to the interrupt; the parent-side guard below
                    // is what keeps the outcome correct either way.
                    let _ = state
                        .peernet
                        .request(
                            &record.machine_id,
                            "turn.interrupt",
                            json!({ "threadId": record.child_thread_id }),
                            None,
                        )
                        .await;
                }
            }
            let result = DispatchResult {
                summary: "Cancelled before it reported.".into(),
                ..Default::default()
            };
            if matches!(record.role_here(&local), Role::Worker) {
                let updated = hub.dispatch.update(&id, |r| {
                    r.status = DispatchStatus::Cancelled;
                    r.finished_at = Some(now_iso());
                    r.result = Some(result.clone());
                });
                if let Some(record) = updated {
                    deliver_report(hub, &record);
                }
            } else {
                // The parent's single landing for ANY terminal status. Going
                // through it rather than writing the record by hand is what
                // makes a cancel terminal: it stamps `announced_at`, so a
                // report the worker filed a moment before it died is ignored
                // instead of resurrecting the dispatch as `failed`. It also
                // announces the completion in the parent's feed and settles a
                // coordinator thread — neither of which a hand-written record
                // did.
                accept_report(hub, &id, DispatchStatus::Cancelled, Some(result));
            }
            hub.broadcast_state("dispatches", None);
            Ok(json!({ "ok": true }))
        }

        // A worker-side report arriving from this machine's own client (or from
        // the smoke harness). The real path is the turn boundary; this is the
        // same landing.
        "dispatch.report" => {
            let id = field("dispatchId")?;
            let record = hub.dispatch.get(&id).context("unknown dispatch")?;
            // A dispatch that has already reached a terminal status keeps it.
            // The case that matters is a cancel: the worker's turn is
            // interrupted, its turn boundary infers a report from whatever it
            // last said, and that report arrives here a moment later. Letting
            // it through leaves the two machines telling different stories
            // about the same dispatch — the parent says cancelled, the worker
            // says failed — and a fan-out reading the wrong one misreports its
            // own outcome.
            anyhow::ensure!(
                !record.status.is_done(),
                "that dispatch already finished as {:?} — a later report cannot change it",
                record.status
            );
            let status: DispatchStatus = serde_json::from_value(
                p.get("status").cloned().unwrap_or(json!("succeeded")),
            )
            .unwrap_or(DispatchStatus::Succeeded);
            let result = DispatchResult {
                summary: p
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                changed: p
                    .get("changed")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                artifacts: Vec::new(),
                inferred: false,
            };
            match record.role_here(&local) {
                Role::Worker => {
                    let updated = hub
                        .dispatch
                        .update(&id, |r| {
                            r.status = status;
                            r.result = Some(result.clone());
                            r.finished_at = Some(now_iso());
                        })
                        .context("unknown dispatch")?;
                    deliver_report(hub, &updated);
                }
                Role::Parent | Role::Both => {
                    accept_report(hub, &id, status, Some(result));
                }
                Role::Neither => anyhow::bail!("this machine is not part of that dispatch"),
            }
            Ok(json!({ "ok": true }))
        }

        // ------------------------------------------------------ mesh half ---
        "mesh.dispatchStart" => {
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
            anyhow::ensure!(
                state.device.accepts_dispatch(),
                "this machine is not accepting dispatched work"
            );
            let mut record: DispatchRecord = serde_json::from_value(
                p.get("record").cloned().context("missing record")?,
            )?;
            // The sender says who it is; the credential says who it actually is.
            // Without this check any paired machine could plant a dispatch that
            // reports to — and takes its authority from — a third one.
            let caller = principal.peer_machine_id().unwrap_or_default();
            anyhow::ensure!(
                record.parent_machine_id == caller,
                "a dispatch must name the machine that sent it as its parent"
            );
            anyhow::ensure!(
                record.machine_id == local,
                "that dispatch is addressed to another machine"
            );
            anyhow::ensure!(
                record.depth <= limits::MAX_DISPATCH_DEPTH,
                "dispatch depth {} exceeds what this machine allows",
                record.depth
            );
            // Re-apply our own ceiling here rather than trusting that the sender
            // did: the far side's check is a courtesy, this one is the rule.
            if let Some(ceiling) = state.device.max_dispatch_access() {
                record.settings.access = narrow(
                    Some(record.settings.access),
                    Access::Full,
                    ceiling,
                );
            }
            let autostart = p.get("autostart").and_then(Value::as_bool).unwrap_or(true);
            let stored = begin_worker(&state.hub, record, autostart)?;
            Ok(json!({
                "childThreadId": stored.child_thread_id,
                "status": stored.status,
            }))
        }

        "mesh.dispatchReport" => {
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
            let id = field("dispatchId")?;
            let record = hub.dispatch.get(&id).context("unknown dispatch")?;
            let caller = principal.peer_machine_id().unwrap_or_default();
            anyhow::ensure!(
                record.machine_id == caller,
                "only the machine running a dispatch may report on it"
            );
            let status: DispatchStatus =
                serde_json::from_value(p.get("status").cloned().unwrap_or(json!("succeeded")))
                    .unwrap_or(DispatchStatus::Succeeded);
            let result: Option<DispatchResult> = p
                .get("result")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            accept_report(hub, &id, status, result);
            Ok(json!({ "ok": true }))
        }

        // The parent cancelling a worker that runs here. Distinct from a bare
        // `turn.interrupt` because the ledger has to know: an interrupted turn
        // still hits the turn boundary, and the boundary infers a report from
        // whatever the worker last said and files it as `failed`. Marking the
        // record first makes `conclude_worker` see a finished dispatch and
        // stay quiet, so the parent's `cancelled` is the only account of it.
        "mesh.dispatchCancel" => {
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
            let id = field("dispatchId")?;
            let record = hub.dispatch.get(&id).context("unknown dispatch")?;
            let caller = principal.peer_machine_id().unwrap_or_default();
            anyhow::ensure!(
                record.parent_machine_id == caller,
                "only the machine that sent a dispatch may cancel it"
            );
            hub.dispatch.update(&id, |r| {
                r.status = DispatchStatus::Cancelled;
                r.finished_at = Some(now_iso());
                r.result = Some(DispatchResult {
                    summary: "Cancelled by the thread that sent it.".into(),
                    ..Default::default()
                });
                // The parent already knows — it is the one asking. Stamping
                // this stops the worker filing a report back at it.
                r.announced_at = Some(now_iso());
            });
            let _ = hub.interrupt(&record.child_thread_id);
            hub.forget_dispatch_tracking(&id);
            hub.broadcast_state("dispatches", None);
            Ok(json!({ "ok": true }))
        }

        "mesh.dispatchProgress" => {
            anyhow::ensure!(principal.is_peer(), "mesh calls are peer-only");
            let id = field("dispatchId")?;
            let Some(record) = hub.dispatch.get(&id) else {
                return Ok(json!({ "ok": true }));
            };
            let caller = principal.peer_machine_id().unwrap_or_default();
            anyhow::ensure!(
                record.machine_id == caller,
                "only the machine running a dispatch may report on it"
            );
            // Hearing a push is what keeps this dispatch off the poll list.
            hub.note_dispatch_heard(&record.id);
            hub.emit(
                &record.parent_thread_id,
                AgentEvent::SubagentProgress {
                    task_id: record.id.clone(),
                    activity: p
                        .get("activity")
                        .and_then(Value::as_str)
                        .unwrap_or("status")
                        .to_string(),
                    text: p
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
            );
            Ok(json!({ "ok": true }))
        }

        _ => anyhow::bail!("unknown request: {kind}"),
    }
}

/// Bring the worker's checkout to the same commit as the sender's, and refuse
/// the dispatch if it cannot be done.
///
/// This is the difference between a cross-platform build and three builds of
/// three different commits. The failure it prevents is the quiet one: a Windows
/// installer of last Tuesday's code looks, in every artifact card and every
/// summary, exactly like a correct build. So the check is a *comparison of
/// shas*, not a `git pull` that is assumed to have worked.
///
/// It deliberately does not push. Pushing is a consequential, outward-facing
/// action on a shared remote, and doing it implicitly because a model said
/// `syncRef` is not a decision this code gets to make — so an unpushed commit
/// is reported as such, with the fix named.
async fn sync_worker_checkout(
    state: &crate::server::ServerState,
    parent_project: &str,
    record: &DispatchRecord,
) -> Result<String> {
    let budget = std::time::Duration::from_secs(180);
    let local = state.device.machine_id.clone();

    let (code, want) = crate::exec::run_capture(
        state,
        &local,
        parent_project,
        "git rev-parse HEAD",
        budget,
    )
    .await?;
    anyhow::ensure!(code == 0, "this project is not a git checkout, so there is nothing to sync: {want}");
    let want = want.trim().to_string();

    let (_, _) = crate::exec::run_capture(
        state,
        &record.machine_id,
        &record.project_id,
        "git fetch --all --quiet",
        budget,
    )
    .await?;
    // `--ff-only` on purpose: a merge or a rebase on a machine nobody is looking
    // at is how a checkout ends up in a state only a human can unpick.
    let (_, pull) = crate::exec::run_capture(
        state,
        &record.machine_id,
        &record.project_id,
        &format!("git checkout --quiet {want} || git pull --ff-only"),
        budget,
    )
    .await?;

    let (code, got) = crate::exec::run_capture(
        state,
        &record.machine_id,
        &record.project_id,
        "git rev-parse HEAD",
        budget,
    )
    .await?;
    anyhow::ensure!(code == 0, "could not read the worker's HEAD: {got}");
    let got = got.trim().to_string();
    anyhow::ensure!(
        got == want,
        "refusing to dispatch: that machine's checkout is at {} but this one is at {}. \
         The commit is probably not on the remote yet — push it, then dispatch again. \
         (git said: {})",
        &got[..got.len().min(8)],
        &want[..want.len().min(8)],
        pull.lines().last().unwrap_or("nothing").trim(),
    );
    Ok(want)
}

/// Put the worker on the parent thread's agent panel the moment it starts.
fn announce_start(state: &crate::server::ServerState, record: &DispatchRecord) {
    let machine_name = if record.machine_id == state.device.machine_id {
        state.device.friendly_name()
    } else {
        state
            .peernet
            .registry
            .peer(&record.machine_id)
            .map(|p| p.name)
            .unwrap_or_else(|| record.machine_id.clone())
    };
    state.hub.emit(
        &record.parent_thread_id,
        AgentEvent::SubagentStarted {
            task_id: record.id.clone(),
            // No provider tool call to nest under; the client synthesizes a card.
            tool_use_id: String::new(),
            description: record.label.clone(),
            subagent_type: "dispatch".into(),
            background: true,
            prompt: Some(record.brief.clone()),
            dispatch: Some(Box::new(crate::protocol::DispatchAttribution {
                machine_id: record.machine_id.clone(),
                machine_name,
                agent: record.agent,
                child_thread_id: record.child_thread_id.clone(),
            })),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(access: Access) -> ThreadSettings {
        ThreadSettings {
            model: "m".into(),
            effort: None,
            wide_context: false,
            claude_chrome: false,
            access,
            mode: Mode::Build,
            browser_profile_id: None,
        }
    }

    fn thread_fixture() -> Thread {
        Thread {
            id: "p".into(),
            project_id: "proj".into(),
            machine_id: "a".into(),
            agent: Agent::Claude,
            title: "Plan".into(),
            settings: settings(Access::Full),
            provider_session_id: None,
            session_anchors: Default::default(),
            provider_run_id: None,
            status: crate::protocol::ThreadStatus::Idle,
            settled_at: None,
            kept_active_at: None,
            favorite: None,
            participants: Vec::new(),
            active_speaker: None,
            parley: None,
            dispatch: None,
            created_at: now_iso(),
            updated_at: now_iso(),
        }
    }

    fn record(parent_machine: &str, machine: &str) -> DispatchRecord {
        DispatchRecord {
            id: new_id(),
            parent_thread_id: "p".into(),
            parent_machine_id: parent_machine.into(),
            child_thread_id: "c".into(),
            machine_id: machine.into(),
            project_id: "proj".into(),
            agent: Agent::Codex,
            settings: settings(Access::Full),
            brief: "do it".into(),
            label: "job".into(),
            status: DispatchStatus::Running,
            result: None,
            depth: 1,
            reported_at: None,
            announced_at: None,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
        }
    }

    #[test]
    fn access_only_ever_narrows() {
        // A worker cannot be given more than the parent holds…
        assert_eq!(narrow(Some(Access::Full), Access::Read, Access::Full), Access::Read);
        // …nor more than the target machine permits…
        assert_eq!(narrow(Some(Access::Full), Access::Full, Access::Edits), Access::Edits);
        // …and asking for less than you hold is honoured.
        assert_eq!(narrow(Some(Access::Read), Access::Full, Access::Full), Access::Read);
        // Unspecified inherits the parent, still capped by the target.
        assert_eq!(narrow(None, Access::Full, Access::Read), Access::Read);
        assert_eq!(narrow(None, Access::Edits, Access::Full), Access::Edits);
    }

    #[test]
    fn role_is_read_from_the_two_machine_ids() {
        assert_eq!(record("a", "b").role_here("a"), Role::Parent);
        assert_eq!(record("a", "b").role_here("b"), Role::Worker);
        assert_eq!(record("a", "a").role_here("a"), Role::Both);
        assert_eq!(record("a", "b").role_here("c"), Role::Neither);
        assert!(record("a", "a").is_local());
        assert!(!record("a", "b").is_local());
    }

    #[test]
    fn a_worker_never_inherits_a_signed_in_browser_profile() {
        let mut parent = thread_fixture();
        parent.settings = settings(Access::Full);
        parent.settings.browser_profile_id = Some("logged-in-as-the-owner".into());
        let child = worker_settings(&parent, Agent::Codex, None, None, Access::Full).unwrap();
        assert!(
            child.browser_profile_id.is_none(),
            "a delegated agent must not inherit the owner's logged-in browser"
        );
        assert_eq!(child.mode, Mode::Build, "a worker never runs in plan mode");
    }

    #[test]
    fn effort_is_dropped_when_the_worker_is_a_different_provider() {
        let mut parent = thread_fixture();
        parent.agent = Agent::Claude;
        parent.settings = settings(Access::Full);
        parent.settings.effort = Some("high".into());
        // Same provider keeps it…
        let same = worker_settings(&parent, Agent::Claude, None, None, Access::Full).unwrap();
        assert_eq!(same.effort.as_deref(), Some("high"));
        // …a different one does not, because the vocabulary may not exist there.
        let other = worker_settings(&parent, Agent::Codex, None, None, Access::Full).unwrap();
        assert_eq!(other.effort, None);
    }

    #[test]
    fn the_ledger_round_trips_and_trims_only_finished_records() {
        let dir = std::env::temp_dir().join(format!("tk-dispatch-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = DispatchLedger::open(&dir).unwrap();

        let live = record("a", "a");
        ledger.upsert(live.clone());
        for i in 0..(limits::MAX_REMEMBERED_DISPATCHES + 10) {
            let mut done = record("a", "a");
            done.status = DispatchStatus::Succeeded;
            done.created_at = format!("2026-01-01T00:00:{:02}Z", i % 60);
            ledger.upsert(done);
        }
        let reopened = DispatchLedger::open(&dir).unwrap();
        let all = reopened.list();
        assert!(
            all.iter().any(|r| r.id == live.id),
            "a running dispatch is never trimmed"
        );
        assert!(all.len() <= limits::MAX_REMEMBERED_DISPATCHES + 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_brief_demands_a_report_and_names_the_restriction() {
        let mut r = record("a", "b");
        r.settings.access = Access::Read;
        let brief = brief_for(&r, "Plan the release", "arch");
        assert!(brief.contains("report_result"));
        assert!(brief.contains("read-only"), "the worker is told why it will be refused");
        assert!(brief.contains("do it"), "the actual brief survives");
        assert!(
            brief.contains("does not read this thread"),
            "the worker must know the summary is the whole channel"
        );
    }

    /// The rule that decides whether the fallback costs anything. A healthy
    /// dispatch pushes every `DISPATCH_PROGRESS_INTERVAL`, so it must never be
    /// polled; a worker that cannot reach us must be polled from the first tick.
    #[test]
    fn only_a_worker_that_has_gone_quiet_is_polled() {
        use std::time::Duration;
        assert!(
            should_poll(None),
            "never having heard from a worker is silence, not a grace period"
        );
        assert!(
            !should_poll(Some(Duration::from_secs(0))),
            "a worker that just pushed is not polled"
        );
        assert!(
            !should_poll(Some(limits::DISPATCH_PROGRESS_INTERVAL)),
            "one push interval is normal cadence, not silence"
        );
        assert!(
            !should_poll(Some(limits::DISPATCH_PROGRESS_INTERVAL * 2)),
            "a single missed push is jitter, not a broken channel"
        );
        assert!(
            should_poll(Some(limits::DISPATCH_PUSH_SILENCE)),
            "sustained silence starts the fallback"
        );
        assert!(should_poll(Some(Duration::from_secs(600))));
        // The threshold has to sit above the push cadence or every healthy
        // dispatch gets polled as well as pushed.
        assert!(limits::DISPATCH_PUSH_SILENCE > limits::DISPATCH_PROGRESS_INTERVAL * 2);
    }

    #[test]
    fn truncate_keeps_char_boundaries() {
        let s = "─".repeat(100);
        let t = truncate(&s, 10);
        assert!(t.ends_with('…'));
        assert!(t.len() <= 14);
    }
}

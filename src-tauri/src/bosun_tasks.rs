//! Bosun tasks (docs/BOSUN.md §10): the to-do list a Bosun keeps.
//!
//! A task is one labelled to-do per real-world item (an Orbit ticket, an email
//! thread, a call), with a status, a next step, and at most one thread behind
//! it. Threads are working notes; the task is what the person manages.
//!
//! Everything here that decides something is a pure function over a task (or
//! the task list) so the rules in §10.3 can be tested without a hub: key
//! derivation, sweep upsert + reconcile, snooze expiry, the status a thread
//! event implies, and whether a queued update is due. The engine glue —
//! opening threads, starting turns, settling — sits beside them and only
//! applies what they return.
//!
//! `bosun-tasks.json` holds the tasks plus the bodies of signals parked in a
//! task's `pending_updates`: by the time the thread is idle again the signal
//! has long left the pending queue, and the follow-up turn needs its text.

use super::*;

/// History entries kept per task, newest last.
const TASK_HISTORY_CAP: usize = 50;
/// Open tasks shown to triage, newest first.
pub(crate) const OPEN_TASKS_IN_TRIAGE: usize = 60;
/// Done/Dropped tasks older than this leave the file.
const CLOSED_PRUNE_DAYS: i64 = 14;
/// `bosun.task.list { includeClosed }` window.
const CLOSED_LIST_DAYS: i64 = 3;
/// A sweep may carry many items; one bad lookout must not flood the list.
pub(crate) const SWEEP_ITEM_CAP: usize = 500;
const TASK_TITLE_CHARS: usize = 120;
const TASK_SUMMARY_CHARS: usize = 1500;
const TASK_NEXT_STEP_CHARS: usize = 400;

/// `bosun-tasks.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TasksFile {
    #[serde(default)]
    pub tasks: Vec<BosunTask>,
    /// Bodies of signals parked in some task's `pending_updates`.
    #[serde(default)]
    pub signals: BTreeMap<String, Signal>,
    /// §10.6 ran on this data dir.
    #[serde(default)]
    pub migrated: bool,
}

/// What the agent said about its task, in memory: whether it set a status
/// during the turn in flight, and the last resting status it chose.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentMark {
    pub set_this_turn: bool,
    pub last: Option<TaskStatus>,
    /// The task was already closed when the turn in flight started: someone
    /// is still talking in a finished task's thread, so its turn end must not
    /// put the thread away again.
    pub closed_at_turn_start: bool,
}

pub(crate) fn load_tasks(dir: &Path) -> TasksFile {
    let path = dir.join("bosun-tasks.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return TasksFile::default();
    };
    match serde_json::from_str(&raw) {
        Ok(file) => file,
        Err(e) => {
            // Keep the unreadable file for a human rather than overwriting it
            // with an empty list on the next flush.
            let aside = dir.join(format!("bosun-tasks.json.corrupt-{}", Local::now().format("%Y%m%d%H%M%S")));
            tracing::warn!("bosun-tasks.json unreadable ({e}); moved to {}", aside.display());
            let _ = std::fs::rename(&path, aside);
            TasksFile::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

fn ref_value<'a>(refs: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    refs.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn is_email_kind(kind: &str) -> bool {
    matches!(kind.trim().to_ascii_lowercase().as_str(), "email" | "gmail" | "mail")
}

/// BOSUN.md §10.2. `refs.key` wins; else Orbit (`taskId` + `org`), Gmail
/// (`threadId` on an email signal; account from `refs.account`, else the
/// lookout), folder files (`call:<path>`); else none.
pub(crate) fn derive_key(kind: &str, refs: &BTreeMap<String, String>, lookout_id: &str) -> Option<String> {
    if let Some(key) = ref_value(refs, "key") {
        return Some(key.to_string());
    }
    if let (Some(task), Some(org)) = (ref_value(refs, "taskId"), ref_value(refs, "org")) {
        return Some(format!("orbit:{org}:{task}"));
    }
    if is_email_kind(kind) {
        if let Some(thread) = ref_value(refs, "threadId") {
            let account = ref_value(refs, "account").unwrap_or(lookout_id);
            return Some(format!("gmail:{account}:{thread}"));
        }
    }
    if matches!(kind, "file" | "call") {
        if let Some(path) = ref_value(refs, "path") {
            return Some(format!("call:{path}"));
        }
    }
    None
}

/// `orbit:<org>:<taskId>` → (org, taskId).
pub(crate) fn orbit_from_key(key: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix("orbit:")?;
    let (org, id) = rest.split_once(':')?;
    (!org.is_empty() && !id.is_empty()).then(|| (org.to_string(), id.to_string()))
}

fn orbit_from_refs(refs: &BTreeMap<String, String>, key: Option<&str>) -> Option<OrbitLink> {
    let (org, task_id) = match (ref_value(refs, "org"), ref_value(refs, "taskId")) {
        (Some(org), Some(id)) => (org.to_string(), id.to_string()),
        _ => key.and_then(orbit_from_key)?,
    };
    Some(OrbitLink {
        org,
        task_id,
        ticket_number: ref_value(refs, "ticket").map(|t| t.trim_start_matches('#').to_string()),
        url: ref_value(refs, "url").unwrap_or_default().to_string(),
    })
}

fn orbit_from_item(item: &SweepItem) -> Option<OrbitLink> {
    let (org, from_key) = orbit_from_key(&item.key).unzip();
    let task_id = item
        .orbit_task_id
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or(from_key)?;
    Some(OrbitLink {
        org: org.unwrap_or_default(),
        task_id,
        ticket_number: item
            .orbit_ticket
            .as_deref()
            .map(|t| t.trim().trim_start_matches('#').to_string())
            .filter(|t| !t.is_empty()),
        url: item.url.clone().unwrap_or_default(),
    })
}

/// The `source.kind` a signal kind files under.
pub(crate) fn source_kind(signal_kind: &str) -> String {
    match signal_kind {
        "file" => "call".into(),
        k if is_email_kind(k) => "email".into(),
        k => k.to_string(),
    }
}

/// `refs.openKeys`: a JSON array of strings (a lookout that emitted the array
/// raw gets it stringified by `signal_from_object`) or a comma/newline list.
pub(crate) fn parse_open_keys(raw: &str) -> HashSet<String> {
    let raw = raw.trim();
    if raw.starts_with('[') {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(raw) {
            return list.into_iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
        }
    }
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn new_task(bosun_id: &str, key: String, title: &str, status: TaskStatus, source: TaskSource, now: &str) -> BosunTask {
    BosunTask {
        id: new_id(),
        bosun_id: bosun_id.to_string(),
        key,
        title: cap_chars(title.trim(), TASK_TITLE_CHARS),
        client: None,
        workspace_id: None,
        status,
        summary: String::new(),
        next_step: None,
        thread_id: None,
        orbit: None,
        source,
        due: None,
        snoozed_until: None,
        pending_updates: Vec::new(),
        history: Vec::new(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
        done_at: None,
    }
}

pub(crate) fn push_history(task: &mut BosunTask, status: Option<TaskStatus>, note: &str, by: &str, now: &str) {
    task.history.push(TaskEvent {
        at: now.to_string(),
        status,
        note: cap_chars(note.trim(), 500),
        by: by.to_string(),
    });
    if task.history.len() > TASK_HISTORY_CAP {
        let drop = task.history.len() - TASK_HISTORY_CAP;
        task.history.drain(..drop);
    }
    task.updated_at = now.to_string();
}

/// Move a task to `status`, keeping `done_at` and `snoozed_until` honest, and
/// note it. No-op (and no history line) when nothing changes.
pub(crate) fn set_status(task: &mut BosunTask, status: TaskStatus, note: &str, by: &str, now: &str) -> bool {
    if task.status == status {
        return false;
    }
    task.status = status;
    task.done_at = status.is_closed().then(|| now.to_string());
    if status != TaskStatus::Snoozed {
        task.snoozed_until = None;
    }
    push_history(task, Some(status), note, by, now);
    true
}

/// A snooze is a date ("YYYY-MM-DD", local midnight) or an RFC 3339 instant.
pub(crate) fn parse_snooze(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if let Some(t) = parse_iso(s) {
        return Some(t);
    }
    let day = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    day.and_hms_opt(0, 0, 0)?.and_local_timezone(Local).earliest()
}

fn snooze_passed(task: &BosunTask, now: DateTime<Local>) -> bool {
    // An unreadable date must not hide a task forever.
    task.snoozed_until
        .as_deref()
        .is_none_or(|s| parse_snooze(s).is_none_or(|until| until <= now))
}

/// Snoozed tasks whose time has come go back to Todo. Returns the ids moved.
pub(crate) fn expire_snoozes(tasks: &mut [BosunTask], now: DateTime<Local>) -> Vec<String> {
    let stamp = iso_local(now);
    let mut woke = Vec::new();
    for task in tasks.iter_mut() {
        if task.status == TaskStatus::Snoozed && snooze_passed(task, now) {
            set_status(task, TaskStatus::Todo, "Snooze ended", "bosun", &stamp);
            woke.push(task.id.clone());
        }
    }
    woke
}

/// Done/Dropped tasks closed more than 14 days ago leave the file.
pub(crate) fn prune_closed(tasks: &mut Vec<BosunTask>, now: DateTime<Local>) -> usize {
    let before = tasks.len();
    let cutoff = now - chrono::Duration::days(CLOSED_PRUNE_DAYS);
    tasks.retain(|t| {
        !t.status.is_closed()
            || parse_iso(t.done_at.as_deref().unwrap_or(&t.updated_at)).is_none_or(|at| at > cutoff)
    });
    before - tasks.len()
}

/// What a work/ask signal does to a task that already has a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// No usable thread: open one (§6).
    OpenThread,
    /// The thread is idle: a follow-up turn there, now.
    FollowUp,
    /// Busy or waiting on a question: park the signal in `pending_updates`.
    Queue,
}

pub(crate) fn route_for(thread_status: Option<ThreadStatus>) -> Route {
    match thread_status {
        None => Route::OpenThread,
        Some(ThreadStatus::Idle) => Route::FollowUp,
        Some(ThreadStatus::Running | ThreadStatus::WaitingApproval) => Route::Queue,
    }
}

/// Whether the per-tick drain owes this task its one follow-up turn.
pub(crate) fn drain_due(task: &BosunTask, thread_status: Option<ThreadStatus>) -> bool {
    !task.pending_updates.is_empty() && thread_status == Some(ThreadStatus::Idle)
}

/// A thread lifecycle event, as far as its task cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadCue {
    TurnStarted,
    /// A question or approval card is up.
    NeedsInput,
    /// The person answered it; the turn carries on.
    InputResolved,
    /// Completed, aborted or failed.
    TurnEnded,
}

pub(crate) fn cue_for(event: &AgentEvent) -> Option<ThreadCue> {
    Some(match event {
        AgentEvent::TurnStarted { .. } => ThreadCue::TurnStarted,
        AgentEvent::QuestionRequest { .. } | AgentEvent::ApprovalRequest { .. } => ThreadCue::NeedsInput,
        AgentEvent::QuestionResolved { .. } | AgentEvent::ApprovalResolved { .. } => ThreadCue::InputResolved,
        AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted | AgentEvent::Error { .. } => ThreadCue::TurnEnded,
        _ => return None,
    })
}

fn resting(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::NeedsYou | TaskStatus::WaitingOnClient | TaskStatus::Todo)
}

/// §10.3 "thread state drives status". Closed tasks are never reopened by a
/// thread event. At a turn's end a task still marked Working means nobody
/// (neither the agent this turn nor a question card) gave it a resting
/// status, so it takes the agent's last one — NeedsYou when there is none,
/// since someone should look.
pub(crate) fn status_for_cue(current: TaskStatus, cue: ThreadCue, mark: AgentMark) -> Option<TaskStatus> {
    if current.is_closed() {
        return None;
    }
    let next = match cue {
        ThreadCue::TurnStarted => TaskStatus::Working,
        ThreadCue::NeedsInput => TaskStatus::NeedsYou,
        ThreadCue::InputResolved if current == TaskStatus::NeedsYou => TaskStatus::Working,
        ThreadCue::InputResolved => return None,
        ThreadCue::TurnEnded if current == TaskStatus::Working => {
            mark.last.filter(|s| resting(*s)).unwrap_or(TaskStatus::NeedsYou)
        }
        ThreadCue::TurnEnded => return None,
    };
    (next != current).then_some(next)
}

fn cue_note(cue: ThreadCue) -> &'static str {
    match cue {
        ThreadCue::TurnStarted => "Agent started working",
        ThreadCue::NeedsInput => "Agent is waiting on your answer",
        ThreadCue::InputResolved => "You answered; agent is working",
        ThreadCue::TurnEnded => "Agent stopped",
    }
}

/// What one sweep did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SweepOutcome {
    pub created: Vec<String>,
    pub refreshed: usize,
    /// (task id, its thread) for every task the reconcile closed.
    pub closed: Vec<(String, Option<String>)>,
}

fn closed_note(scope: &str) -> (&'static str, &'static str) {
    if scope.starts_with("orbit:") {
        ("closed in Orbit", "orbit")
    } else if scope.starts_with("gmail:") {
        ("replied", "bosun")
    } else {
        ("no longer open at the source", "bosun")
    }
}

/// §10.3 sweeps: upsert one task per item (new ones Todo, no thread), then
/// close every open task in `refs.scope` missing from `refs.openKeys`.
///
/// The reconcile only runs when the sweep carries BOTH a non-empty scope and
/// an `openKeys` ref (an empty one closes everything in scope): an empty
/// scope would match every key, and a sweep without `openKeys` lists only
/// what needs attention, which says nothing about what is closed.
pub(crate) fn apply_sweep(
    tasks: &mut Vec<BosunTask>,
    bosun_id: &str,
    signal: &Signal,
    workspace_id: Option<&str>,
    now: DateTime<Local>,
) -> SweepOutcome {
    let stamp = iso_local(now);
    let mut out = SweepOutcome::default();
    let mut listed: HashSet<String> = HashSet::new();
    for item in signal.items.iter().take(SWEEP_ITEM_CAP) {
        let key = item.key.trim();
        if key.is_empty() {
            continue;
        }
        listed.insert(key.to_string());
        let detail = cap_chars(item.detail.trim(), TASK_SUMMARY_CHARS);
        let orbit = orbit_from_item(item);
        if let Some(task) = tasks
            .iter_mut()
            .find(|t| t.bosun_id == bosun_id && t.key == key && !t.status.is_closed())
        {
            let before = task.clone();
            if !detail.is_empty() {
                task.summary = detail;
            }
            task.due = item.due.clone().filter(|d| !d.trim().is_empty());
            if let Some(mut link) = orbit {
                // A sweep does not know the ticket number every time; never
                // forget one the task already had.
                if link.ticket_number.is_none() {
                    link.ticket_number = task.orbit.as_ref().and_then(|o| o.ticket_number.clone());
                }
                task.orbit = Some(link);
            }
            if task.client.is_none() {
                task.client = item.client.clone().filter(|c| !c.trim().is_empty());
            }
            if task.status == TaskStatus::Snoozed && snooze_passed(task, now) {
                set_status(task, TaskStatus::Todo, "Snooze ended", "bosun", &stamp);
            }
            if *task != before {
                task.updated_at = stamp.clone();
                out.refreshed += 1;
            }
            continue;
        }
        let source = TaskSource {
            kind: "sweep".into(),
            lookout_id: signal.lookout_id.clone(),
            signal_id: signal.id.clone(),
            url: item.url.clone().filter(|u| !u.trim().is_empty()),
            path: None,
        };
        let title = if item.title.trim().is_empty() { key } else { item.title.as_str() };
        let mut task = new_task(bosun_id, key.to_string(), title, TaskStatus::Todo, source, &stamp);
        task.client = item.client.clone().filter(|c| !c.trim().is_empty());
        task.workspace_id = workspace_id.map(str::to_string);
        task.summary = detail;
        task.due = item.due.clone().filter(|d| !d.trim().is_empty());
        task.orbit = orbit;
        push_history(&mut task, Some(TaskStatus::Todo), &format!("Found by {}", signal.title), "bosun", &stamp);
        out.created.push(task.id.clone());
        tasks.push(task);
    }

    let scope = ref_value(&signal.refs, "scope").unwrap_or("");
    // Present-but-empty means "nothing in scope is open any more"; absent
    // means the lookout could not see the whole list, so close nothing.
    let Some(open_keys) = signal.refs.get("openKeys").map(|raw| parse_open_keys(raw)) else {
        return out;
    };
    if scope.is_empty() {
        return out;
    }
    let (note, by) = closed_note(scope);
    for task in tasks.iter_mut() {
        if task.bosun_id != bosun_id
            || task.status.is_closed()
            || !task.key.starts_with(scope)
            || open_keys.contains(&task.key)
            || listed.contains(&task.key)
        {
            continue;
        }
        set_status(task, TaskStatus::Done, note, by, &stamp);
        out.closed.push((task.id.clone(), task.thread_id.clone()));
    }
    out
}

/// Strip emoji and other pictographs from a thread title (§10.6).
pub(crate) fn strip_glyphs(title: &str) -> String {
    let kept: String = title
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || c.is_ascii_punctuation() || "·—–‘’“”…".contains(*c))
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `bosun.task.update` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskUpdate {
    pub task_id: String,
    pub status: Option<TaskStatus>,
    pub title: Option<String>,
    /// `Some(None)` is an explicit `snoozedUntil: null` (clear).
    pub snoozed_until: Option<Option<String>>,
    pub note: Option<String>,
    /// `Some("")` clears.
    pub summary: Option<String>,
    /// `Some(None)` (null or "") clears.
    pub next_step: Option<Option<String>>,
    pub due: Option<Option<String>>,
    pub client: Option<Option<String>>,
    pub workspace_id: Option<String>,
}

/// Absent → None; null or "" → Some(None); text → Some(Some(trimmed)).
fn tri_state(p: &Value, key: &str) -> Result<Option<Option<String>>> {
    Ok(match p.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) if s.trim().is_empty() => Some(None),
        Some(Value::String(s)) => Some(Some(s.trim().to_string())),
        Some(_) => anyhow::bail!("{key} must be a string or null"),
    })
}

pub(crate) fn valid_due(due: &str) -> bool {
    NaiveDate::parse_from_str(due, "%Y-%m-%d").is_ok() || parse_iso(due).is_some()
}

impl TaskUpdate {
    pub(crate) fn parse(p: &Value) -> Result<Self> {
        let task_id = p
            .get("taskId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .context("missing taskId")?
            .to_string();
        let status = match p.get("status") {
            None | Some(Value::Null) => None,
            Some(v) => Some(serde_json::from_value::<TaskStatus>(v.clone()).context("unknown status")?),
        };
        let text = |k: &str| {
            p.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let snoozed_until = match p.get("snoozedUntil") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) if s.trim().is_empty() => Some(None),
            Some(Value::String(s)) => {
                anyhow::ensure!(parse_snooze(s).is_some(), "snoozedUntil must be YYYY-MM-DD or an ISO date-time");
                Some(Some(s.trim().to_string()))
            }
            Some(_) => anyhow::bail!("snoozedUntil must be a string or null"),
        };
        let due = tri_state(p, "due")?;
        if let Some(Some(d)) = &due {
            anyhow::ensure!(valid_due(d), "due must be YYYY-MM-DD or an ISO date-time");
        }
        Ok(Self {
            task_id,
            status,
            title: text("title"),
            snoozed_until,
            note: text("note"),
            summary: tri_state(p, "summary")?.map(Option::unwrap_or_default),
            next_step: tri_state(p, "nextStep")?,
            due,
            client: tri_state(p, "client")?,
            workspace_id: text("workspaceId"),
        })
    }
}

/// Apply a person's edit. Reopen conflicts are checked by the caller, which
/// can see the other tasks.
pub(crate) fn apply_user_update(task: &mut BosunTask, upd: &TaskUpdate, now: &str) -> Result<()> {
    let mut edited: Vec<&str> = Vec::new();
    if let Some(title) = &upd.title {
        if *title != task.title {
            task.title = cap_chars(title, TASK_TITLE_CHARS);
            edited.push("title");
        }
    }
    if let Some(summary) = &upd.summary {
        if *summary != task.summary {
            task.summary = cap_chars(summary, TASK_SUMMARY_CHARS);
            edited.push("summary");
        }
    }
    if let Some(next) = &upd.next_step {
        let next = next.as_deref().map(|n| cap_chars(n, TASK_NEXT_STEP_CHARS));
        if next != task.next_step {
            task.next_step = next;
            edited.push("next step");
        }
    }
    if let Some(due) = &upd.due {
        if *due != task.due {
            task.due = due.clone();
            edited.push("due date");
        }
    }
    if let Some(client) = &upd.client {
        if *client != task.client {
            task.client = client.clone();
            edited.push("client");
        }
    }
    if let Some(ws) = &upd.workspace_id {
        if Some(ws) != task.workspace_id.as_ref() {
            task.workspace_id = Some(ws.clone());
            edited.push("workspace");
        }
    }
    if !edited.is_empty() {
        push_history(task, None, &format!("You edited the {}", edited.join(", ")), "you", now);
    }
    let mut status = upd.status;
    match &upd.snoozed_until {
        Some(Some(until)) => {
            task.snoozed_until = Some(until.clone());
            if status.is_none() {
                status = Some(TaskStatus::Snoozed);
            }
        }
        Some(None) => {
            task.snoozed_until = None;
            if status.is_none() && task.status == TaskStatus::Snoozed {
                status = Some(TaskStatus::Todo);
            }
        }
        None => {}
    }
    if status == Some(TaskStatus::Snoozed) {
        anyhow::ensure!(task.snoozed_until.is_some(), "snoozing needs snoozedUntil");
    }
    let note = upd.note.clone();
    match status {
        Some(s) if s != task.status => {
            let snooze = task.snoozed_until.clone();
            let default = match s {
                TaskStatus::Snoozed => format!("Snoozed until {}", snooze.unwrap_or_default()),
                other => format!("Marked {}", status_label(other)),
            };
            // Clears the snooze for every status but Snoozed itself.
            set_status(task, s, note.as_deref().unwrap_or(&default), "you", now);
        }
        _ => {
            if let Some(note) = note {
                push_history(task, None, &note, "you", now);
            }
        }
    }
    Ok(())
}

pub(crate) fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Todo => "to do",
        TaskStatus::Working => "working",
        TaskStatus::NeedsYou => "needs you",
        TaskStatus::WaitingOnClient => "waiting on client",
        TaskStatus::Snoozed => "snoozed",
        TaskStatus::Done => "done",
        TaskStatus::Dropped => "dropped",
    }
}

/// Rebuild an Orbit task URL for `id` from one we already know.
fn orbit_url(known: Option<&str>, org: &str, id: &str) -> String {
    if let Some(known) = known.filter(|u| !u.is_empty()) {
        if let Some(at) = known.find("/tasks") {
            let base = &known[..at];
            // `.../<org>/tasks[/<old id>]` — the org segment is the last one.
            let base = base.rsplit_once('/').map(|(b, _)| b).unwrap_or(base);
            return format!("{base}/{org}/tasks/{id}");
        }
    }
    String::new()
}

/// The agent's `bosun_task` call (§10.4), applied to its task. Returns whether
/// it set a status. `known_orbit_url` is any Orbit URL this Bosun has seen, to
/// build a link for a ticket the task did not come from.
pub(crate) fn apply_agent_update(task: &mut BosunTask, args: &Value, known_orbit_url: Option<&str>, now: &str) -> Result<Option<TaskStatus>> {
    let text = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let status = match args.get("status") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            serde_json::from_value::<TaskStatus>(v.clone())
                .context("status must be one of todo, working, needs_you, waiting_on_client, done, dropped")?,
        ),
    };
    anyhow::ensure!(
        status != Some(TaskStatus::Snoozed),
        "snoozing is the person's call; use waiting_on_client or needs_you"
    );
    let mut changed = Vec::new();
    if let Some(title) = text("title") {
        task.title = cap_chars(&title, TASK_TITLE_CHARS);
        changed.push("title");
    }
    if let Some(client) = text("client") {
        task.client = Some(cap_chars(&client, 120));
        changed.push("client");
    }
    if let Some(summary) = text("summary") {
        task.summary = cap_chars(&summary, TASK_SUMMARY_CHARS);
        changed.push("summary");
    }
    if let Some(next) = text("nextStep") {
        task.next_step = Some(cap_chars(&next, TASK_NEXT_STEP_CHARS));
        changed.push("next step");
    }
    if let Some(due) = text("due") {
        task.due = Some(cap_chars(&due, 40));
        changed.push("due");
    }
    if let Some(raw) = text("orbitTaskId") {
        let id = raw.trim_start_matches('#').trim().to_string();
        let org = text("orbitOrg")
            .or_else(|| task.orbit.as_ref().map(|o| o.org.clone()).filter(|o| !o.is_empty()))
            .or_else(|| orbit_from_key(&task.key).map(|(org, _)| org));
        // "#101324" or "101324" names the ticket number; anything else is the
        // task id Orbit's API uses.
        let is_ticket = id.chars().all(|c| c.is_ascii_digit());
        match (&mut task.orbit, is_ticket) {
            (Some(link), true) => link.ticket_number = Some(id),
            (Some(link), false) => {
                if link.task_id != id {
                    let org = org.unwrap_or_else(|| link.org.clone());
                    link.url = orbit_url(Some(&link.url), &org, &id);
                    link.task_id = id;
                    link.org = org;
                }
            }
            (None, is_ticket) => {
                let org = org.context("orbitOrg is required to link a task to Orbit")?;
                let (task_id, ticket_number) = if is_ticket { (String::new(), Some(id)) } else { (id, None) };
                task.orbit = Some(OrbitLink {
                    url: if task_id.is_empty() { String::new() } else { orbit_url(known_orbit_url, &org, &task_id) },
                    org,
                    task_id,
                    ticket_number,
                });
            }
        }
        changed.push("Orbit link");
    }
    let note = text("note");
    if let Some(s) = status {
        let default = format!("Agent marked it {}", status_label(s));
        if !set_status(task, s, note.as_deref().unwrap_or(&default), "bosun", now) {
            if let Some(note) = &note {
                push_history(task, None, note, "bosun", now);
            }
        }
    } else if let Some(note) = &note {
        push_history(task, None, note, "bosun", now);
    } else if !changed.is_empty() {
        task.updated_at = now.to_string();
    }
    Ok(status)
}

/// `bosun_task { followUp }`: the next to-do this work leads to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FollowUpSpec {
    pub title: String,
    /// Local YYYY-MM-DD.
    pub due: String,
    pub summary: Option<String>,
    pub next_step: Option<String>,
    pub steps: Vec<String>,
    /// Put it on its due day's plan (default yes).
    pub plan: bool,
}

impl FollowUpSpec {
    pub(crate) fn parse(v: &Value) -> Result<Self> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let title = text("title").context("followUp needs a title")?;
        Ok(Self {
            title: cap_chars(&title, TASK_TITLE_CHARS),
            due: plan::resolve_day(v.get("due").and_then(Value::as_str), true)?,
            summary: text("summary").map(|s| cap_chars(&s, TASK_SUMMARY_CHARS)),
            next_step: text("nextStep").map(|s| cap_chars(&s, TASK_NEXT_STEP_CHARS)),
            steps: v
                .get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|s| cap_chars(s.trim(), 200))
                .filter(|s| !s.is_empty())
                .take(12)
                .collect(),
            plan: v.get("plan").and_then(Value::as_bool).unwrap_or(true),
        })
    }
}

/// The follow-up to-do for `original`: same Bosun, client, workspace and
/// Orbit link, so the ticket stays open until the follow-up is ticked. It
/// takes over the original's key when the original is closed, so the next
/// signal or sweep for that ticket lands on the follow-up.
pub(crate) fn follow_up_task(original: &BosunTask, spec: &FollowUpSpec, now: &str) -> BosunTask {
    let key = if original.status.is_closed() && !original.key.starts_with("adhoc:") && !original.key.starts_with("plan:") {
        original.key.clone()
    } else {
        format!("adhoc:{}", new_id())
    };
    let source = TaskSource { kind: "follow-up".into(), ..Default::default() };
    let mut task = new_task(&original.bosun_id, key, &spec.title, TaskStatus::Todo, source, now);
    task.client = original.client.clone();
    task.workspace_id = original.workspace_id.clone();
    task.orbit = original.orbit.clone();
    task.due = Some(spec.due.clone());
    task.summary = spec
        .summary
        .clone()
        .unwrap_or_else(|| cap_chars(&format!("Follow-up to \"{}\".", original.title), TASK_SUMMARY_CHARS));
    task.next_step = spec.next_step.clone();
    push_history(&mut task, Some(TaskStatus::Todo), &format!("Follow-up from \"{}\"", original.title), "bosun", now);
    task
}

// ---------------------------------------------------------------------------
// Registry: the tasks file
// ---------------------------------------------------------------------------

impl BosunRegistry {
    fn flush_tasks(&self, file: &TasksFile) -> Result<()> {
        write_atomic(&self.dir.join("bosun-tasks.json"), &serde_json::to_string(file)?)
    }

    /// Run `f` over the tasks file and persist it.
    pub(crate) fn with_tasks<R>(&self, f: impl FnOnce(&mut TasksFile) -> R) -> R {
        let mut file = self.tasks.lock().unwrap();
        let out = f(&mut file);
        // Drop cached bodies no task waits on any more.
        let wanted: HashSet<String> = file
            .tasks
            .iter()
            .flat_map(|t| t.pending_updates.iter().cloned())
            .collect();
        file.signals.retain(|id, _| wanted.contains(id));
        if let Err(e) = self.flush_tasks(&file) {
            tracing::warn!("persist bosun tasks: {e:#}");
        }
        out
    }

    pub fn task(&self, id: &str) -> Option<BosunTask> {
        self.tasks.lock().unwrap().tasks.iter().find(|t| t.id == id).cloned()
    }

    pub(crate) fn open_task_for_key(&self, bosun_id: &str, key: &str) -> Option<BosunTask> {
        self.tasks
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.bosun_id == bosun_id && t.key == key && !t.status.is_closed())
            .cloned()
    }

    /// Open tasks (and, with `include_closed`, those closed in the last three
    /// days), most recently updated first.
    pub fn list_tasks(&self, bosun_id: Option<&str>, include_closed: bool) -> Vec<BosunTask> {
        let cutoff = Local::now() - chrono::Duration::days(CLOSED_LIST_DAYS);
        let mut out: Vec<BosunTask> = self
            .tasks
            .lock()
            .unwrap()
            .tasks
            .iter()
            .filter(|t| bosun_id.is_none_or(|b| t.bosun_id == b))
            .filter(|t| {
                !t.status.is_closed()
                    || (include_closed
                        && parse_iso(t.done_at.as_deref().unwrap_or(&t.updated_at)).is_some_and(|at| at > cutoff))
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        out
    }

    /// Insert unless an open task with the same key exists, which is returned
    /// instead: "at most ONE open task per (bosun_id, key)".
    pub(crate) fn insert_task(&self, task: BosunTask) -> (BosunTask, bool) {
        self.with_tasks(|file| {
            if let Some(existing) = file
                .tasks
                .iter()
                .find(|t| t.bosun_id == task.bosun_id && t.key == task.key && !t.status.is_closed())
            {
                return (existing.clone(), false);
            }
            file.tasks.push(task.clone());
            (task, true)
        })
    }

    pub(crate) fn update_task(&self, id: &str, f: impl FnOnce(&mut BosunTask) -> Result<()>) -> Result<BosunTask> {
        self.with_tasks(|file| {
            let task = file.tasks.iter_mut().find(|t| t.id == id).context("unknown task")?;
            f(task)?;
            Ok(task.clone())
        })
    }

    pub(crate) fn delete_task(&self, id: &str) -> Result<BosunTask> {
        self.with_tasks(|file| {
            let at = file.tasks.iter().position(|t| t.id == id).context("unknown task")?;
            Ok(file.tasks.remove(at))
        })
    }

    /// Whether reopening `task` would make a second open task for its key.
    pub(crate) fn reopen_conflict(&self, task: &BosunTask) -> bool {
        self.tasks
            .lock()
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.id != task.id && t.bosun_id == task.bosun_id && t.key == task.key && !t.status.is_closed())
    }

    pub(crate) fn agent_mark(&self, task_id: &str) -> AgentMark {
        self.marks.lock().unwrap().get(task_id).copied().unwrap_or_default()
    }

    fn note_agent_status(&self, task_id: &str, status: TaskStatus) {
        let mut marks = self.marks.lock().unwrap();
        let mark = marks.entry(task_id.to_string()).or_default();
        mark.set_this_turn = true;
        if resting(status) {
            mark.last = Some(status);
        }
    }

    fn new_turn(&self, task_id: &str, closed: bool) {
        let mut marks = self.marks.lock().unwrap();
        let mark = marks.entry(task_id.to_string()).or_default();
        mark.set_this_turn = false;
        mark.closed_at_turn_start = closed;
    }

    /// Any Orbit URL this Bosun has seen, for building links.
    fn known_orbit_url(&self, bosun_id: &str) -> Option<String> {
        self.tasks
            .lock()
            .unwrap()
            .tasks
            .iter()
            .filter(|t| t.bosun_id == bosun_id)
            .filter_map(|t| t.orbit.as_ref().map(|o| o.url.clone()))
            .find(|u| u.contains("/tasks"))
    }

    pub(crate) fn tasks_migrated(&self) -> bool {
        self.tasks.lock().unwrap().migrated
    }

    /// Snooze expiry and pruning (every tick). Returns whether anything moved.
    pub(crate) fn maintain_tasks(&self, now: DateTime<Local>) -> bool {
        let dirty = {
            let mut file = self.tasks.lock().unwrap();
            let woke = expire_snoozes(&mut file.tasks, now);
            let pruned = prune_closed(&mut file.tasks, now);
            !woke.is_empty() || pruned > 0
        };
        if dirty {
            self.with_tasks(|_| ());
        }
        dirty
    }

    pub(crate) fn forget_bosun_tasks(&self, bosun_id: &str) {
        self.with_tasks(|file| file.tasks.retain(|t| t.bosun_id != bosun_id));
    }
}

// ---------------------------------------------------------------------------
// Engine glue
// ---------------------------------------------------------------------------

/// Park a thread in the settled shelf, for everyone, if it is idle. A running
/// thread is settled by its own turn end instead (see [`on_thread_event`]):
/// settling it now would be undone by the very next lifecycle event.
pub(crate) fn settle_thread(hub: &Hub, thread_id: &str) -> bool {
    let now = now_iso();
    let settled = hub.store.try_update_thread_quiet(thread_id, |t| {
        anyhow::ensure!(t.status == ThreadStatus::Idle, "busy");
        // A favorite stays on the list until its owner un-stars it.
        anyhow::ensure!(t.favorite != Some(true), "favorite");
        if t.settled_at.is_none() {
            t.settled_at = Some(now);
        }
        t.kept_active_at = None;
        Ok(())
    });
    match settled {
        Ok(thread) => {
            // Per-person overlays would otherwise keep it on someone's list.
            hub.people.unpark_thread(thread_id);
            hub.broadcast_state("threads", Some(thread.project_id));
            true
        }
        Err(_) => false,
    }
}

/// Called by the hub for every event it emits (after the thread's status and
/// shelf writes). Cheap for everything that is not a lifecycle event on a
/// task's own thread.
pub fn on_thread_event(hub: &Hub, thread_id: &str, event: &AgentEvent) {
    let Some(cue) = cue_for(event) else { return };
    let Some(thread) = hub.store.thread(thread_id) else { return };
    let Some(task_id) = thread.origin.as_ref().and_then(|o| o.task_id.clone()) else {
        return;
    };
    let registry = &hub.bosuns;
    let Some(task) = registry.task(&task_id) else { return };
    // A thread merged into another during migration still points at the
    // task, but only the task's own thread drives it.
    if task.thread_id.as_deref() != Some(thread_id) {
        return;
    }
    if cue == ThreadCue::TurnStarted {
        registry.new_turn(&task_id, task.status.is_closed());
    }
    let mark = registry.agent_mark(&task_id);
    let mut task = task;
    if let Some(next) = status_for_cue(task.status, cue, mark) {
        let now = now_iso();
        if let Ok(updated) = registry.update_task(&task_id, |t| {
            set_status(t, next, cue_note(cue), "bosun", &now);
            Ok(())
        }) {
            task = updated;
            hub.broadcast_state("bosunTasks", None);
        }
    }
    if cue == ThreadCue::TurnEnded {
        // Settle only a task that closed during this turn. One that was
        // already closed when the turn started is a finished task someone is
        // still working in; shelving it after every reply loses the thread.
        if task.status.is_closed() && !mark.closed_at_turn_start {
            settle_thread(hub, thread_id);
        }
        if !task.pending_updates.is_empty() {
            registry.kick.notify_one();
        }
    }
}

fn task_from_signal(bosun: &Bosun, plan: &Planned, signal: &Signal, key: Option<String>, now: &str) -> BosunTask {
    let source = TaskSource {
        kind: source_kind(&signal.kind),
        lookout_id: signal.lookout_id.clone(),
        signal_id: signal.id.clone(),
        url: ref_value(&signal.refs, "url").map(str::to_string),
        path: ref_value(&signal.refs, "path").map(str::to_string),
    };
    let key = key.unwrap_or_else(|| format!("adhoc:{}", new_id()));
    let mut task = new_task(&bosun.id, key.clone(), &plan.title, TaskStatus::Working, source, now);
    task.client = ref_value(&signal.refs, "client").map(str::to_string);
    task.workspace_id = plan.workspace_id.clone().or_else(|| Some(bosun.home_workspace_id.clone()));
    task.summary = cap_chars(plan.reason.trim(), TASK_SUMMARY_CHARS);
    task.orbit = orbit_from_refs(&signal.refs, Some(&key));
    push_history(&mut task, Some(TaskStatus::Working), &format!("Opened from {}: {}", signal.kind, signal.title), "bosun", now);
    task
}

/// Appended to every work prompt for a task's thread (§10.4).
pub(crate) fn task_instructions(task: &BosunTask, bosun_name: &str) -> String {
    format!(
        "\n## Your to-do\nThis thread works one to-do on {bosun_name}'s list (currently titled \"{title}\", status {status}).\n\
         Call the `bosun_task` tool once now with a clear title that names the client and a\n\
         one-to-three-sentence summary, and once more right before you stop with a status\n\
         (needs_you, waiting_on_client or done) and nextStep: what happens next and who does it.\n\
         If the work is an Orbit ticket, pass its id in orbitTaskId.\n\
         When the person says this part is done but there is a next step for another day (\"I replied,\n\
         make a task for tomorrow to finish and release it\"), call `bosun_task` with status done and\n\
         followUp {{ title, due }}: the follow-up keeps the client and Orbit ticket and lands on that day's plan.\n",
        title = task.title,
        status = status_label(task.status),
    )
}

const FOLLOW_UP_REMINDER: &str =
    "\nBefore you stop, update the to-do with the `bosun_task` tool: status (needs_you, waiting_on_client or done) and nextStep.\n";

/// The decision's thread for a work/ask signal, through its task (§10.3).
pub(crate) async fn work_on_task(
    state: &ServerState,
    bosun: &Bosun,
    plan: &Planned,
    signal: &Signal,
    lookout_name: &str,
    person: &str,
) -> Result<String> {
    let hub = &state.hub;
    let store = &hub.store;
    let registry = &hub.bosuns;
    let now = now_iso();
    let key = derive_key(&signal.kind, &signal.refs, &signal.lookout_id);
    let open = |t: &BosunTask| t.bosun_id == bosun.id && !t.status.is_closed();
    // Deterministic key match wins; the model's taskId only when it matched
    // nothing; then the task behind a thread triage asked to merge into.
    let merge = merge_target(store, bosun, plan);
    let found = key
        .as_deref()
        .and_then(|k| registry.open_task_for_key(&bosun.id, k))
        .or_else(|| plan.task_id.as_deref().and_then(|id| registry.task(id)).filter(open))
        .or_else(|| {
            merge
                .as_ref()
                .and_then(|t| t.origin.as_ref()?.task_id.clone())
                .and_then(|id| registry.task(&id))
                .filter(open)
        });
    let task = match found {
        Some(task) => {
            let line = format!("Signal: {} ({})", signal.title, plan.reason);
            registry.update_task(&task.id, |t| {
                if t.client.is_none() {
                    t.client = ref_value(&signal.refs, "client").map(str::to_string);
                }
                if t.orbit.is_none() {
                    t.orbit = orbit_from_refs(&signal.refs, key.as_deref());
                }
                push_history(t, None, &line, "bosun", &now);
                Ok(())
            })?
        }
        None => registry.insert_task(task_from_signal(bosun, plan, signal, key, &now)).0,
    };
    hub.broadcast_state("bosunTasks", None);

    let thread = task.thread_id.as_deref().and_then(|id| store.thread(id));
    match route_for(thread.as_ref().map(|t| t.status)) {
        Route::FollowUp => {
            let thread = thread.expect("FollowUp implies a thread");
            reopen_for_work(registry, &task.id, &now)?;
            hub.start_injected_turn(&thread.id, format!("{}{FOLLOW_UP_REMINDER}", follow_up_prompt(signal, lookout_name)))?;
            return Ok(thread.id);
        }
        Route::Queue => {
            let thread = thread.expect("Queue implies a thread");
            let note = format!("Update queued while the thread is busy: {}", signal.title);
            registry.with_tasks(|file| {
                file.signals.insert(signal.id.clone(), signal.clone());
                if let Some(t) = file.tasks.iter_mut().find(|t| t.id == task.id) {
                    if !t.pending_updates.contains(&signal.id) {
                        t.pending_updates.push(signal.id.clone());
                    }
                    push_history(t, None, &note, "bosun", &now);
                }
            });
            hub.broadcast_state("bosunTasks", None);
            return Ok(thread.id);
        }
        Route::OpenThread => {}
    }

    // A thread opened before tasks existed that triage asked to continue:
    // adopt it rather than opening a second one for the same item.
    if let Some(thread) = merge.filter(|t| t.origin.as_ref().is_some_and(|o| o.task_id.is_none())) {
        store.update_thread(&thread.id, |t| {
            if let Some(o) = t.origin.as_mut() {
                o.task_id = Some(task.id.clone());
            }
        })?;
        registry.update_task(&task.id, |t| {
            t.thread_id = Some(thread.id.clone());
            set_status(t, TaskStatus::Working, "Continuing in an earlier thread", "bosun", &now);
            Ok(())
        })?;
        hub.start_injected_turn(&thread.id, format!("{}{FOLLOW_UP_REMINDER}", follow_up_prompt(signal, lookout_name)))?;
        return Ok(thread.id);
    }

    let thread_id = open_work(state, bosun, plan, signal, lookout_name, person, &task).await?;
    registry.update_task(&task.id, |t| {
        t.thread_id = Some(thread_id.clone());
        set_status(t, TaskStatus::Working, "Thread opened", "bosun", &now);
        Ok(())
    })?;
    hub.broadcast_state("bosunTasks", None);
    Ok(thread_id)
}

/// A closed task getting new work is open again — unless another open task
/// already holds its key, in which case the work goes on regardless and the
/// task stays as it was.
fn reopen_for_work(registry: &BosunRegistry, task_id: &str, now: &str) -> Result<()> {
    let Some(task) = registry.task(task_id) else { return Ok(()) };
    if task.status.is_closed() && !registry.reopen_conflict(&task) {
        registry.update_task(task_id, |t| {
            set_status(t, TaskStatus::Working, "Reopened", "bosun", now);
            Ok(())
        })?;
    }
    Ok(())
}

/// `log` decisions: a history line on the open task with the same key.
pub(crate) fn note_log_on_task(registry: &BosunRegistry, bosun: &Bosun, plan: &Planned, signal: &Signal) -> bool {
    let Some(key) = derive_key(&signal.kind, &signal.refs, &signal.lookout_id) else {
        return false;
    };
    let Some(task) = registry.open_task_for_key(&bosun.id, &key) else {
        return false;
    };
    let line = if plan.reason.trim().is_empty() {
        format!("Logged: {}", signal.title)
    } else {
        format!("Logged: {} — {}", signal.title, plan.reason)
    };
    registry
        .update_task(&task.id, |t| {
            push_history(t, None, &line, "bosun", &now_iso());
            Ok(())
        })
        .is_ok()
}

/// Queued updates older than this are dropped rather than delivered. A
/// thread can sit busy (or waiting on a question) for days; by then the
/// signal is history, and replaying a pile of them at once (as every restart
/// used to, when the threads finally read idle) only wakes threads to say
/// "nothing new" and buzzes the phone for each.
pub(crate) const QUEUED_UPDATE_MAX_AGE_HOURS: i64 = 12;

/// Which queued updates are still worth a turn: none for a closed task (it
/// was finished after they queued; a genuinely new signal reopens it through
/// the normal path), and only recent ones otherwise. A signal whose time does
/// not parse is kept, and so is one whose text is gone (it says so itself).
pub(crate) fn deliverable_updates(task_closed: bool, signals: &[Option<Signal>], now: DateTime<Local>) -> Vec<bool> {
    let cutoff = now - chrono::Duration::hours(QUEUED_UPDATE_MAX_AGE_HOURS);
    signals
        .iter()
        .map(|s| {
            !task_closed
                && s.as_ref()
                    .and_then(|s| parse_iso(&s.observed_at))
                    .is_none_or(|at| at > cutoff)
        })
        .collect()
}

/// §10.3: every task of this Bosun whose thread is idle and has queued
/// updates gets ONE follow-up turn carrying all of them.
pub(crate) fn drain_pending_updates(hub: &Arc<Hub>, bosun: &Bosun, lookout_names: &HashMap<String, String>) -> usize {
    let registry = &hub.bosuns;
    let store = &hub.store;
    let candidates: Vec<BosunTask> = registry
        .list_tasks(Some(&bosun.id), true)
        .into_iter()
        .filter(|t| !t.pending_updates.is_empty())
        .collect();
    let mut started = 0;
    for task in candidates {
        let thread = task.thread_id.as_deref().and_then(|id| store.thread(id));
        let Some(thread) = thread else {
            // The thread is gone; there is nowhere to deliver these.
            let _ = registry.update_task(&task.id, |t| {
                t.pending_updates.clear();
                t.thread_id = None;
                push_history(t, None, "Queued updates dropped: the thread no longer exists", "bosun", &now_iso());
                Ok(())
            });
            continue;
        };
        if !drain_due(&task, Some(thread.status)) {
            continue;
        }
        let (signals, ids) = registry.with_tasks(|file| {
            let ids = file
                .tasks
                .iter_mut()
                .find(|t| t.id == task.id)
                .map(|t| std::mem::take(&mut t.pending_updates))
                .unwrap_or_default();
            let signals: Vec<Option<Signal>> = ids.iter().map(|id| file.signals.get(id).cloned()).collect();
            (signals, ids)
        });
        if ids.is_empty() {
            continue;
        }
        let keep = deliverable_updates(task.status.is_closed(), &signals, Local::now());
        let dropped = keep.iter().filter(|k| !**k).count();
        let (ids, signals): (Vec<String>, Vec<Option<Signal>>) = ids
            .into_iter()
            .zip(signals)
            .zip(keep)
            .filter_map(|(pair, k)| k.then_some(pair))
            .unzip();
        if dropped > 0 {
            let why = if task.status.is_closed() { "the to-do is closed" } else { "too old to act on" };
            let _ = registry.update_task(&task.id, |t| {
                push_history(t, None, &format!("{dropped} queued update(s) dropped: {why}"), "bosun", &now_iso());
                Ok(())
            });
        }
        if ids.is_empty() {
            continue;
        }
        let mut prompt = format!("## New signals (arrived while you were busy: {})\n", ids.len());
        for (id, signal) in ids.iter().zip(&signals) {
            match signal {
                Some(s) => {
                    let name = lookout_names.get(&s.lookout_id).map(String::as_str).unwrap_or(&s.lookout_id);
                    prompt.push_str(&format!("\n### {}\n{}\n", s.title, render_signal(s, name)));
                }
                None => prompt.push_str(&format!("\n### {id}\n(signal text no longer available)\n")),
            }
        }
        prompt.push_str(FOLLOW_UP_REMINDER);
        let now = now_iso();
        let _ = reopen_for_work(registry, &task.id, &now);
        match hub.start_injected_turn(&thread.id, prompt) {
            Ok(()) => {
                started += 1;
                let _ = registry.update_task(&task.id, |t| {
                    push_history(t, None, &format!("{} queued update(s) delivered", ids.len()), "bosun", &now);
                    Ok(())
                });
            }
            Err(e) => {
                // Put them back; the next tick tries again.
                tracing::warn!("bosun: deliver queued updates to {}: {e:#}", thread.id);
                registry.with_tasks(|file| {
                    for (id, s) in ids.iter().zip(signals) {
                        if let Some(s) = s {
                            file.signals.insert(id.clone(), s);
                        }
                    }
                    if let Some(t) = file.tasks.iter_mut().find(|t| t.id == task.id) {
                        let mut back = ids.clone();
                        back.append(&mut t.pending_updates);
                        t.pending_updates = back;
                    }
                });
            }
        }
    }
    if started > 0 {
        hub.broadcast_state("bosunTasks", None);
    }
    started
}

/// A sweep's `hints.workspace` (id or name) among the workspaces this Bosun
/// knows; home otherwise.
fn sweep_workspace(store: &Store, bosun: &Bosun, hint: Option<&str>) -> String {
    let Some(hint) = hint.map(str::trim).filter(|h| !h.is_empty()) else {
        return bosun.home_workspace_id.clone();
    };
    std::iter::once(bosun.home_workspace_id.as_str())
        .chain(bosun.charters.iter().map(|c| c.workspace_id.as_str()))
        .find(|id| *id == hint || workspace_name(store, id).eq_ignore_ascii_case(hint))
        .unwrap_or(&bosun.home_workspace_id)
        .to_string()
}

fn scope_label(store: &Store, signal: &Signal, workspace_id: &str) -> String {
    if signal.hints.workspace.as_deref().is_some_and(|h| !h.trim().is_empty()) {
        return workspace_name(store, workspace_id);
    }
    ref_value(&signal.refs, "scope")
        .map(|s| s.trim_end_matches(':').to_string())
        .unwrap_or_else(|| workspace_name(store, workspace_id))
}

/// One phone notification for a sweep that found new to-dos. It belongs to
/// no thread (sweeps open none); it is routed to the sweep's workspace.
fn push_sweep(hub: &Hub, bosun: &Bosun, workspace_id: &str, label: &str, new: usize) {
    let project_id = local_member(&hub.store, workspace_id)
        .map(|m| m.project_id)
        .unwrap_or_default();
    let title = format!("{} · {label}", bosun.name);
    let body = format!("{new} new to-do{}", if new == 1 { "" } else { "s" });
    hub.enqueue_push(crate::push::PushJob {
        kind: crate::push::PushKind::Bosun,
        seq: None,
        project_id,
        workspace_id: workspace_id.to_string(),
        project_name: workspace_name(&hub.store, workspace_id),
        thread_id: String::new(),
        thread_title: String::new(),
        notice: Some(EventNotice { title, body }),
        only_device: None,
        badge: None,
    });
}

/// Sweeps skip triage (§10.3): each pending sweep upserts tasks and
/// reconciles, one ledger line for the lot. Returns how many were handled.
pub(crate) fn process_sweeps(hub: &Arc<Hub>, bosun: &Bosun) -> usize {
    let registry = &hub.bosuns;
    let store = &hub.store;
    let sweeps: Vec<Signal> = registry
        .state
        .lock()
        .unwrap()
        .pending
        .iter()
        .filter(|s| s.bosun_id == bosun.id && s.kind == "sweep")
        .cloned()
        .collect();
    if sweeps.is_empty() {
        return 0;
    }
    let mut wake = Wake {
        id: new_id(),
        bosun_id: bosun.id.clone(),
        at: now_iso(),
        signals: sweeps.len() as u32,
        decisions: Vec::new(),
        skipped: None,
        triage_ms: 0,
    };
    for signal in &sweeps {
        let workspace_id = sweep_workspace(store, bosun, signal.hints.workspace.as_deref());
        let outcome = registry.with_tasks(|file| {
            apply_sweep(&mut file.tasks, &bosun.id, signal, Some(&workspace_id), Local::now())
        });
        for (_, thread) in &outcome.closed {
            if let Some(thread) = thread {
                settle_thread(hub, thread);
            }
        }
        if !outcome.created.is_empty() {
            push_sweep(hub, bosun, &workspace_id, &scope_label(store, signal, &workspace_id), outcome.created.len());
        }
        let has_reconcile = signal.refs.contains_key("openKeys") && ref_value(&signal.refs, "scope").is_some();
        wake.decisions.push(Decision {
            signal_id: signal.id.clone(),
            title: cap_chars(&signal.title, 80),
            decision: DecisionKind::Log,
            workspace_id: Some(workspace_id),
            thread_id: None,
            reason: format!(
                "sweep: {} new, {} refreshed, {} closed{}",
                outcome.created.len(),
                outcome.refreshed,
                outcome.closed.len(),
                if has_reconcile { "" } else { " (no scope/openKeys: nothing reconciled)" }
            ),
            error: None,
        });
    }
    let ids: Vec<String> = sweeps.iter().map(|s| s.id.clone()).collect();
    registry.settle(&bosun.id, &ids);
    if let Err(e) = registry.append_wake(&wake) {
        tracing::warn!("bosun ledger append: {e:#}");
    }
    hub.broadcast_state("bosunTasks", None);
    hub.broadcast_state("bosuns", None);
    sweeps.len()
}

/// A Signal standing in for a task, so `bosun.task.work` can reuse the §7.2
/// prompt (standing orders, rules) with the task as its subject.
pub(crate) fn signal_for_task(task: &BosunTask) -> Signal {
    let mut body = String::new();
    if !task.summary.trim().is_empty() {
        body.push_str(&format!("Summary: {}\n", task.summary.trim()));
    }
    if let Some(next) = &task.next_step {
        body.push_str(&format!("Next step: {next}\n"));
    }
    if let Some(client) = &task.client {
        body.push_str(&format!("Client: {client}\n"));
    }
    if let Some(due) = &task.due {
        body.push_str(&format!("Due: {due}\n"));
    }
    if let Some(o) = &task.orbit {
        let ticket = o.ticket_number.as_deref().map(|t| format!(" #{t}")).unwrap_or_default();
        body.push_str(&format!("Orbit{ticket}: org {} task {} {}\n", o.org, o.task_id, o.url));
    }
    body.push_str(&format!("Status: {}\n", status_label(task.status)));
    let recent: Vec<String> = task
        .history
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|e| format!("- {} ({}) {}", e.at.get(..16).unwrap_or(&e.at), e.by, e.note))
        .collect();
    if !recent.is_empty() {
        body.push_str(&format!("History:\n{}\n", recent.join("\n")));
    }
    let mut refs = BTreeMap::from([("key".to_string(), task.key.clone())]);
    if let Some(url) = &task.source.url {
        refs.insert("url".into(), url.clone());
    }
    if let Some(path) = &task.source.path {
        refs.insert("path".into(), path.clone());
    }
    if let Some(client) = &task.client {
        refs.insert("client".into(), client.clone());
    }
    Signal {
        id: if task.source.signal_id.is_empty() { format!("task:{}", task.id) } else { task.source.signal_id.clone() },
        lookout_id: task.source.lookout_id.clone(),
        bosun_id: task.bosun_id.clone(),
        kind: if task.source.kind.is_empty() { "manual".into() } else { task.source.kind.clone() },
        observed_at: now_iso(),
        title: task.title.clone(),
        body,
        refs,
        hints: SignalHints::default(),
        items: Vec::new(),
    }
}

/// `bosun.task.work`: open or continue the task's one thread.
pub(crate) async fn work_task(state: &ServerState, principal: &Principal, task_id: &str, instruction: Option<String>) -> Result<String> {
    let hub = &state.hub;
    let registry = &hub.bosuns;
    let store = &hub.store;
    let task = registry.task(task_id).context("unknown task")?;
    let bosun = registry.get(&task.bosun_id).context("this to-do's Bosun no longer exists")?;
    let now = now_iso();
    let reopen = |registry: &BosunRegistry| -> Result<()> {
        let task = registry.task(task_id).context("unknown task")?;
        anyhow::ensure!(
            !(task.status.is_closed() && registry.reopen_conflict(&task)),
            "another open to-do already tracks this item"
        );
        registry.update_task(task_id, |t| {
            set_status(t, TaskStatus::Working, "You asked for work on it", "you", &now);
            Ok(())
        })?;
        Ok(())
    };
    if let Some(thread) = task.thread_id.as_deref().and_then(|id| store.thread(id)) {
        // Busy: the conversation is already moving; the UI opens it.
        if thread.status == ThreadStatus::Idle {
            reopen(registry)?;
            let text = match instruction.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(i) => format!("{i}\n{FOLLOW_UP_REMINDER}"),
                None => format!(
                    "Pick this to-do back up and take it as far as the standing orders allow.\n\n## To-do\n{}\n{FOLLOW_UP_REMINDER}",
                    signal_for_task(&task).body
                ),
            };
            hub.start_injected_turn(&thread.id, text)?;
            hub.broadcast_state("bosunTasks", None);
        }
        return Ok(thread.id);
    }
    let workspace_id = task.workspace_id.clone().unwrap_or_else(|| bosun.home_workspace_id.clone());
    if charter(&bosun, &workspace_id)
        .and_then(|c| c.member.as_ref())
        .is_some_and(|m| m.machine_id != store.local_machine_id())
    {
        principal.require(Capability::Terminal)?;
    }
    reopen(registry)?;
    let task = registry.task(task_id).context("unknown task")?;
    let signal = signal_for_task(&task);
    let plan = Planned {
        signal_id: signal.id.clone(),
        decision: DecisionKind::Work,
        workspace_id: Some(workspace_id),
        title: task.title.clone(),
        reason: String::new(),
        brief: instruction
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| "Work on this to-do.".into()),
        merge_into: None,
        task_id: Some(task.id.clone()),
    };
    let lookout_name = bosun
        .lookouts
        .iter()
        .find(|l| l.id == task.source.lookout_id)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| "to-do list".into());
    let person = person_name(hub, bosun.author.as_deref());
    let thread_id = open_work(state, &bosun, &plan, &signal, &lookout_name, &person, &task).await?;
    registry.update_task(task_id, |t| {
        t.thread_id = Some(thread_id.clone());
        push_history(t, None, "Thread opened", "bosun", &now);
        Ok(())
    })?;
    hub.broadcast_state("bosunTasks", None);
    Ok(thread_id)
}

// ---------------------------------------------------------------------------
// MCP: `bosun_task` (§10.4)
// ---------------------------------------------------------------------------

/// The task this thread works, if it is the task's own thread.
fn thread_task(state: &ServerState, thread_id: &str) -> Option<BosunTask> {
    let thread = state.hub.store.thread(thread_id)?;
    let task_id = thread.origin.as_ref()?.task_id.clone()?;
    state
        .hub
        .bosuns
        .task(&task_id)
        .filter(|t| t.thread_id.as_deref() == Some(thread_id))
}

pub fn mcp_tool_specs_for(state: &ServerState, thread_id: &str) -> Vec<Value> {
    if thread_task(state, thread_id).is_none() {
        return Vec::new();
    }
    let s = |d: &str| json!({ "type": "string", "description": d });
    vec![json!({
        "name": "bosun_task",
        "description":
            "Update the to-do this thread works on (the person's Bosun task list). Call it once \
             at the start with a clear title that names the client and a 1-3 sentence summary, \
             and once right before you stop with status (needs_you, waiting_on_client or done) \
             and nextStep. Every field is optional; only what you pass changes. Returns the task. \
             When this work leads to a next to-do (the person says \"I replied, now make a task for \
             tomorrow to finish and release it\"), pass followUp: it creates that to-do with this one's \
             client, workspace and Orbit link (the ticket stays open until the follow-up is done) and \
             adds it to that day's plan. Usually together with status done. Closing a to-do here never \
             closes its Orbit ticket.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "status": { "type": "string",
                            "enum": ["todo", "working", "needs_you", "waiting_on_client", "done", "dropped"],
                            "description": "needs_you: the person must act or decide. waiting_on_client: \
                                            ball is in the client's court. done: finished and verified." },
                "title": s("Imperative, names the client: \"Reply to Keith Roofing about job 2451\"."),
                "client": s("Client or organisation this is for."),
                "summary": s("1-3 plain sentences: what this is and where it stands."),
                "nextStep": s("What happens next and who does it."),
                "due": s("ISO date (YYYY-MM-DD) if there is a deadline."),
                "orbitTaskId": s("The Orbit task id (or ticket number) this work belongs to."),
                "orbitOrg": s("Orbit org slug, when linking a task that is not linked yet."),
                "note": s("One line for the task's history."),
                "followUp": {
                    "type": "object",
                    "description": "The next to-do this work leads to.",
                    "properties": {
                        "title": s("Imperative, names the client: \"Finish and release the line-totals display for Crossland\"."),
                        "due": s("YYYY-MM-DD, \"today\" or \"tomorrow\". Default tomorrow."),
                        "summary": s("1-3 sentences: what is left and why."),
                        "nextStep": s("What happens next and who does it."),
                        "steps": { "type": "array", "items": { "type": "string" }, "description": "0-5 short sub-steps for the plan row." },
                        "plan": { "type": "boolean", "description": "Put it on the due day's plan. Default true." },
                    },
                    "required": ["title"],
                },
            },
            "required": [],
        },
    })]
}

pub async fn mcp_bosun_task(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let hub = &state.hub;
    let registry = &hub.bosuns;
    let task = thread_task(state, thread_id).context("this thread is not working on a Bosun to-do")?;
    let known = registry.known_orbit_url(&task.bosun_id);
    let now = now_iso();
    let mut set = None;
    let updated = registry.update_task(&task.id, |t| {
        set = apply_agent_update(t, args, known.as_deref(), &now)?;
        Ok(())
    })?;
    // An ad-hoc task the agent tied to an Orbit ticket takes the ticket's key,
    // so the next sweep or ticket signal lands on it instead of beside it.
    let updated = match updated
        .orbit
        .as_ref()
        .filter(|o| updated.key.starts_with("adhoc:") && !o.org.is_empty() && !o.task_id.is_empty())
        .map(|o| format!("orbit:{}:{}", o.org, o.task_id))
    {
        Some(key) if registry.open_task_for_key(&updated.bosun_id, &key).is_none() => {
            registry.update_task(&updated.id, |t| {
                t.key = key;
                Ok(())
            })?
        }
        _ => updated,
    };
    if let Some(status) = set {
        registry.note_agent_status(&updated.id, status);
    }
    // The next to-do this work leads to (after the status, so a closed
    // original hands its key on).
    let follow = match args.get("followUp").filter(|v| !v.is_null()) {
        Some(v) => {
            let spec = FollowUpSpec::parse(v)?;
            let (follow, created) = registry.insert_task(follow_up_task(&updated, &spec, &now));
            let planned = if spec.plan {
                registry.append_to_plan(&spec.due, &follow.id, None, &spec.steps)?;
                hub.broadcast_state("bosunPlan", None);
                Some(spec.due.clone())
            } else {
                None
            };
            let note = match (&planned, created) {
                (_, false) => format!("Follow-up already tracked: \"{}\"", follow.title),
                (Some(day), true) => format!("Follow-up added for {day}: \"{}\"", follow.title),
                (None, true) => format!("Follow-up added: \"{}\" (due {})", follow.title, spec.due),
            };
            let now = now_iso();
            registry.update_task(&updated.id, |t| {
                push_history(t, None, &note, "bosun", &now);
                Ok(())
            })?;
            Some((follow, planned))
        }
        None => None,
    };
    // Closed mid-turn: the thread is running, so its turn end settles it.
    if updated.status.is_closed() {
        settle_thread(hub, thread_id);
    }
    hub.broadcast_state("bosunTasks", None);
    let updated = registry.task(&updated.id).unwrap_or(updated);
    match follow {
        None => Ok(serde_json::to_string(&updated)?),
        Some((follow, planned)) => Ok(serde_json::to_string(&json!({
            "task": updated,
            "followUp": follow,
            "plannedFor": planned,
        }))?),
    }
}

// ---------------------------------------------------------------------------
// Migration (§10.6)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct MigrationReport {
    pub created: usize,
    pub settled: Vec<String>,
    /// (thread id, status note) for duplicates folded into another thread.
    pub notes: Vec<(String, String)>,
}

fn is_sweep_thread(thread: &Thread, origin: &ThreadOrigin) -> bool {
    origin.signal_kind == "sweep"
        || origin.signal_id.contains(":sweep:")
        || thread.title.to_lowercase().contains("sweep")
}

fn settle_quietly(store: &Store, thread_id: &str, now: &str) -> bool {
    store
        .update_thread_quiet(thread_id, |t| {
            if t.settled_at.is_none() {
                t.settled_at = Some(now.to_string());
            }
            t.kept_active_at = None;
        })
        .is_ok()
}

/// Give every pre-task Bosun thread a task. Idempotent: a thread that already
/// carries `origin.task_id` is never looked at again, and duplicates are
/// stamped with the task they were folded into.
pub(crate) fn migrate_threads(store: &Store, registry: &BosunRegistry) -> MigrationReport {
    let now = now_iso();
    let bosuns: HashSet<String> = registry.list().into_iter().map(|b| b.id).collect();
    let mut report = MigrationReport::default();
    let mut groups: BTreeMap<(String, String), Vec<Thread>> = BTreeMap::new();
    for thread in store.all_threads() {
        let Some(origin) = thread.origin.clone() else { continue };
        if origin.kind != "bosun" || origin.day_log || origin.task_id.is_some() || !bosuns.contains(&origin.bosun_id) {
            continue;
        }
        if is_sweep_thread(&thread, &origin) {
            if thread.settled_at.is_none() && settle_quietly(store, &thread.id, &now) {
                report.settled.push(thread.id.clone());
            }
            continue;
        }
        let key = derive_key(&origin.signal_kind, &origin.refs, &origin.lookout_id)
            .unwrap_or_else(|| format!("adhoc:{}", thread.id));
        groups.entry((origin.bosun_id.clone(), key)).or_default().push(thread);
    }
    for ((bosun_id, key), mut threads) in groups {
        threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let existing = registry.open_task_for_key(&bosun_id, &key);
        // An open task (a sweep's, say) that has no thread yet takes the
        // newest one; a task that already has a thread keeps it, and every
        // thread here is a duplicate of it.
        let (task, keep) = match existing {
            Some(task) if task.thread_id.as_deref().is_some_and(|id| store.thread(id).is_some()) => (task, None),
            Some(task) => (task, Some(threads.remove(0))),
            None => {
                let keep = threads.remove(0);
                let origin = keep.origin.clone().expect("grouped threads have an origin");
                let status = match keep.status {
                    ThreadStatus::Running => TaskStatus::Working,
                    ThreadStatus::WaitingApproval | ThreadStatus::Idle => TaskStatus::NeedsYou,
                };
                let title = strip_glyphs(&keep.title);
                let source = TaskSource {
                    kind: source_kind(&origin.signal_kind),
                    lookout_id: origin.lookout_id.clone(),
                    signal_id: origin.signal_id.clone(),
                    url: ref_value(&origin.refs, "url").map(str::to_string),
                    path: ref_value(&origin.refs, "path").map(str::to_string),
                };
                let mut task = new_task(
                    &bosun_id,
                    key.clone(),
                    if title.is_empty() { "Bosun to-do" } else { &title },
                    status,
                    source,
                    &now,
                );
                task.created_at = threads
                    .iter()
                    .chain(std::iter::once(&keep))
                    .map(|t| t.created_at.clone())
                    .min()
                    .unwrap_or_else(|| now.clone());
                task.client = ref_value(&origin.refs, "client").map(str::to_string);
                task.workspace_id = store.workspace_for_project(&keep.project_id);
                task.orbit = orbit_from_refs(&origin.refs, Some(&key));
                push_history(&mut task, Some(status), "Created from an existing Bosun thread", "bosun", &now);
                let (task, _) = registry.insert_task(task);
                report.created += 1;
                (task, Some(keep))
            }
        };
        if let Some(keep) = &keep {
            let _ = store.update_thread_quiet(&keep.id, |t| {
                if let Some(o) = t.origin.as_mut() {
                    o.task_id = Some(task.id.clone());
                }
            });
            let _ = registry.update_task(&task.id, |t| {
                t.thread_id = Some(keep.id.clone());
                Ok(())
            });
        }
        let home = keep
            .as_ref()
            .map(|k| k.id.clone())
            .or_else(|| task.thread_id.clone())
            .unwrap_or_default();
        for dup in threads {
            let _ = store.update_thread_quiet(&dup.id, |t| {
                if let Some(o) = t.origin.as_mut() {
                    o.task_id = Some(task.id.clone());
                }
            });
            if settle_quietly(store, &dup.id, &now) {
                report.settled.push(dup.id.clone());
            }
            report.notes.push((
                dup.id.clone(),
                format!("Folded into the to-do \"{}\"; its work continues in thread {home}.", task.title),
            ));
        }
    }
    report
}

/// Engine start: run §10.6 once per data dir.
pub(crate) fn migrate_once(hub: &Hub) {
    let registry = &hub.bosuns;
    if registry.tasks_migrated() {
        return;
    }
    let report = migrate_threads(&hub.store, registry);
    for (thread_id, note) in &report.notes {
        hub.emit(thread_id, AgentEvent::Status { text: note.clone() });
    }
    for thread_id in &report.settled {
        hub.people.unpark_thread(thread_id);
    }
    registry.with_tasks(|file| file.migrated = true);
    if report.created > 0 || !report.settled.is_empty() {
        tracing::info!(
            "bosun tasks: migrated {} task(s), settled {} thread(s)",
            report.created,
            report.settled.len()
        );
        hub.broadcast_state("threads", None);
        hub.broadcast_state("bosunTasks", None);
    }
}

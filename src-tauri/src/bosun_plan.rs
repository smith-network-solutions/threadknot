//! Day plans (docs/BOSUN.md §11): the to-dos for one day, ordered into time
//! blocks, worked down and ticked off.
//!
//! A plan holds no work of its own. Every row is a Bosun task, so ticking it
//! is closing the task (and, through §10.8, its Orbit ticket); the plan only
//! adds the order, the blocks, and a few sub-steps per row. Quick items that
//! are not tracked yet become ad-hoc tasks when they are added.
//!
//! `bosun-plan.json` holds the last [`PLAN_KEEP_DAYS`] days. Writes replace a
//! day whole (`bosun.plan.set`): the person drags rows and ticks steps, the
//! agent builds a draft, and last write wins, the same as a task edit.

use super::*;

const PLAN_KEEP_DAYS: usize = 30;
const PLAN_BLOCK_CAP: usize = 24;
const PLAN_ITEM_CAP: usize = 40;
const PLAN_STEP_CAP: usize = 12;
const PLAN_TITLE_CHARS: usize = 120;
const PLAN_STEP_CHARS: usize = 200;

/// `bosun-plan.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlanFile {
    /// Keyed by local date, YYYY-MM-DD.
    #[serde(default)]
    pub days: BTreeMap<String, DayPlan>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayPlan {
    pub date: String,
    #[serde(default)]
    pub blocks: Vec<PlanBlock>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBlock {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// "HH:MM", local. Optional: a block may be just a heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default)]
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    #[serde(default)]
    pub id: String,
    pub task_id: String,
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

pub(crate) fn load_plans(dir: &Path) -> PlanFile {
    let path = dir.join("bosun-plan.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return PlanFile::default();
    };
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        let aside = dir.join(format!("bosun-plan.json.corrupt-{}", Local::now().format("%Y%m%d%H%M%S")));
        tracing::warn!("bosun-plan.json unreadable ({e}); moved to {}", aside.display());
        let _ = std::fs::rename(&path, aside);
        PlanFile::default()
    })
}

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

pub(crate) fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

pub(crate) fn valid_date(date: &str) -> bool {
    NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
}

/// "9:00", "09:00" → "09:00". None for anything else.
pub(crate) fn normalize_time(raw: &str) -> Option<String> {
    let (h, m) = raw.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60 && raw.trim().len() <= 5).then(|| format!("{h:02}:{m:02}"))
}

fn optional_time(raw: Option<&String>, what: &str) -> Result<Option<String>> {
    match raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(t) => normalize_time(t).map(Some).with_context(|| format!("{what} must be HH:MM (got {t})")),
    }
}

/// Make a plan safe to keep: ids filled in, times normalised, caps applied,
/// rows whose task does not exist dropped, and each task kept once (the
/// first row wins). `task_exists` answers for a task id.
pub(crate) fn normalize_plan(mut plan: DayPlan, task_exists: impl Fn(&str) -> bool, now: &str) -> Result<DayPlan> {
    anyhow::ensure!(valid_date(&plan.date), "date must be YYYY-MM-DD");
    anyhow::ensure!(plan.blocks.len() <= PLAN_BLOCK_CAP, "a day holds at most {PLAN_BLOCK_CAP} blocks");
    let mut seen = HashSet::new();
    for block in &mut plan.blocks {
        if block.id.trim().is_empty() {
            block.id = new_id();
        }
        block.title = cap_chars(block.title.trim(), PLAN_TITLE_CHARS);
        block.start = optional_time(block.start.as_ref(), "start")?;
        block.end = optional_time(block.end.as_ref(), "end")?;
        if let (Some(s), Some(e)) = (&block.start, &block.end) {
            anyhow::ensure!(e > s, "block \"{}\" ends before it starts", block.title);
        }
        block.items.retain(|i| task_exists(&i.task_id) && seen.insert(i.task_id.clone()));
        anyhow::ensure!(block.items.len() <= PLAN_ITEM_CAP, "a block holds at most {PLAN_ITEM_CAP} rows");
        for item in &mut block.items {
            if item.id.trim().is_empty() {
                item.id = new_id();
            }
            item.steps.retain(|s| !s.text.trim().is_empty());
            item.steps.truncate(PLAN_STEP_CAP);
            for step in &mut item.steps {
                if step.id.trim().is_empty() {
                    step.id = new_id();
                }
                step.text = cap_chars(step.text.trim(), PLAN_STEP_CHARS);
            }
        }
    }
    plan.updated_at = now.to_string();
    Ok(plan)
}

/// Keep the newest `keep` days.
pub(crate) fn prune_days(days: &mut BTreeMap<String, DayPlan>, keep: usize) {
    while days.len() > keep {
        let Some(oldest) = days.keys().next().cloned() else { break };
        days.remove(&oldest);
    }
}

/// The most recent day before `date` that has rows, for "carry over".
fn last_before(days: &BTreeMap<String, DayPlan>, date: &str) -> Option<String> {
    days.range(..date.to_string())
        .rev()
        .find(|(_, p)| p.blocks.iter().any(|b| !b.items.is_empty()))
        .map(|(d, _)| d.clone())
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

impl BosunRegistry {
    pub(crate) fn plan(&self, date: &str) -> (DayPlan, Option<String>) {
        let file = self.plans.lock().unwrap();
        let plan = file.days.get(date).cloned().unwrap_or_else(|| DayPlan {
            date: date.to_string(),
            ..Default::default()
        });
        (plan, last_before(&file.days, date))
    }

    pub(crate) fn put_plan(&self, plan: DayPlan) -> Result<DayPlan> {
        let plan = {
            let tasks = self.tasks.lock().unwrap();
            normalize_plan(plan, |id| tasks.tasks.iter().any(|t| t.id == id), &now_iso())?
        };
        let mut file = self.plans.lock().unwrap();
        file.days.insert(plan.date.clone(), plan.clone());
        prune_days(&mut file.days, PLAN_KEEP_DAYS);
        write_atomic(&self.dir.join("bosun-plan.json"), &serde_json::to_string(&*file)?)?;
        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// Wire (`bosun.plan.*`)
// ---------------------------------------------------------------------------

fn date_field(p: &Value) -> Result<String> {
    match p.get("date").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(today()),
        Some(d) => {
            anyhow::ensure!(valid_date(d), "date must be YYYY-MM-DD");
            Ok(d.to_string())
        }
    }
}

/// `bosun.plan.get { date? }` → `{ plan, lastDate }`. An unplanned day is an
/// empty plan, not an error.
pub(crate) fn handle_get(registry: &BosunRegistry, p: &Value) -> Result<Value> {
    let (plan, last) = registry.plan(&date_field(p)?);
    Ok(json!({ "plan": plan, "lastDate": last }))
}

/// `bosun.plan.set { date, blocks }` → the stored plan.
pub(crate) fn handle_set(registry: &BosunRegistry, p: &Value) -> Result<Value> {
    let blocks: Vec<PlanBlock> = serde_json::from_value(p.get("blocks").cloned().unwrap_or(Value::Array(Vec::new())))
        .context("blocks is not a list of plan blocks")?;
    let plan = registry.put_plan(DayPlan { date: date_field(p)?, blocks, updated_at: String::new() })?;
    Ok(serde_json::to_value(plan)?)
}

/// `bosun.plan.draft { bosunId?, date?, instruction? }` → `{ taskId, threadId }`.
/// "Plan my day": an ad-hoc to-do whose thread builds the plan with the
/// `bosun_plan` tool, so the drafting is a conversation the person can steer.
pub(crate) async fn handle_draft(state: &ServerState, principal: &Principal, p: &Value) -> Result<Value> {
    let hub = &state.hub;
    let registry = &hub.bosuns;
    let date = date_field(p)?;
    let bosun = match p.get("bosunId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(id) => registry.get(id).context("unknown bosun")?,
        None => registry.list().into_iter().next().context("there is no Bosun to plan with")?,
    };
    let label = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .map(|d| d.format("%a %b %-d").to_string())
        .unwrap_or_else(|_| date.clone());
    let key = format!("plan:{date}");
    let task = match registry.open_task_for_key(&bosun.id, &key) {
        Some(t) => t,
        None => {
            let now = now_iso();
            let source = TaskSource { kind: "manual".into(), ..Default::default() };
            let mut task = new_task(&bosun.id, key, &format!("Plan my day: {label}"), TaskStatus::Todo, source, &now);
            task.workspace_id = Some(bosun.home_workspace_id.clone());
            task.summary = format!("Build the day plan for {label} in the Today view.");
            push_history(&mut task, Some(TaskStatus::Todo), "Plan my day", "you", &now);
            registry.insert_task(task).0
        }
    };
    let extra = p
        .get("instruction")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|i| format!("\n\nThe person added: {i}"))
        .unwrap_or_default();
    let instruction = format!(
        "Build the person's day plan for {date} ({label}) with the `bosun_plan` tool.\n\n\
         1. Call `bosun_plan` with action \"tasks\" for every open to-do, and action \"get\" for what is already planned.\n\
         2. Check what else needs them today if your tools reach it: calendar events (fixed meetings block time), \
            unanswered email from real people, anything due or overdue.\n\
         3. Rank by leverage: paying customers blocked or at risk, revenue waiting on a quote or a reply, \
            two-minute decisions that unblock someone else, then deep work. Group into time blocks of 30 to 120 minutes \
            around fixed meetings, with a lunch block, starting at the next half hour from now if the day has begun.\n\
         4. Call `bosun_plan` with action \"set\": each block gets a short title, start and end (HH:MM), and rows. \
            A row is an existing to-do (taskId) or, for something not tracked yet, a new one (title + client). \
            Give a row 0 to 5 short steps only when the work has distinct steps. Each to-do appears once. \
            Leave out what can wait; do not plan this planning to-do itself.\n\
         5. Reply with three lines on how you ordered the day and what you left out, then mark this to-do done with `bosun_task`.\n\n\
         This replaces whatever is planned for {date}; keep rows the person already placed unless you have a reason.{extra}"
    );
    let thread_id = work_task(state, principal, &task.id, Some(instruction)).await?;
    Ok(json!({ "taskId": task.id, "threadId": thread_id }))
}

// ---------------------------------------------------------------------------
// MCP: `bosun_plan`
// ---------------------------------------------------------------------------

/// The Bosun whose task thread this is.
fn thread_bosun(state: &ServerState, thread_id: &str) -> Option<String> {
    let thread = state.hub.store.thread(thread_id)?;
    let task_id = thread.origin.as_ref()?.task_id.clone()?;
    state.hub.bosuns.task(&task_id).map(|t| t.bosun_id)
}

pub fn mcp_plan_tool_specs_for(state: &ServerState, thread_id: &str) -> Vec<Value> {
    if thread_bosun(state, thread_id).is_none() {
        return Vec::new();
    }
    let s = |d: &str| json!({ "type": "string", "description": d });
    vec![json!({
        "name": "bosun_plan",
        "description":
            "Read or write the person's day plan (the Today view): their to-dos for one day, ordered into \
             time blocks they tick off. action \"tasks\" lists every open to-do (id, title, client, status, \
             due, Orbit ticket, next step). action \"get\" returns the plan for a date. action \"set\" \
             replaces the plan for a date with `blocks`. Ticking a row closes its to-do, so only real \
             to-dos are rows: a row is { taskId } for an existing one, or { title, client? } to create one.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["tasks", "get", "set"] },
                "date": s("YYYY-MM-DD, local. Default today."),
                "blocks": {
                    "type": "array",
                    "description": "For set: the whole day, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": s("Short: \"Utah Home Electric QuickBooks fix\"."),
                            "start": s("HH:MM"),
                            "end": s("HH:MM"),
                            "items": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "taskId": s("An existing to-do's id."),
                                        "title": s("A new to-do's title, imperative, naming the client."),
                                        "client": s("A new to-do's client."),
                                        "steps": { "type": "array", "items": { "type": "string" },
                                                   "description": "0-5 short sub-steps." },
                                    },
                                },
                            },
                        },
                        "required": ["title"],
                    },
                },
            },
            "required": ["action"],
        },
    })]
}

fn task_line(t: &BosunTask) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "client": t.client,
        "status": t.status,
        "due": t.due,
        "orbitTicket": t.orbit.as_ref().and_then(|o| o.ticket_number.clone()),
        "nextStep": t.next_step,
        "updatedAt": t.updated_at,
    })
}

pub async fn mcp_bosun_plan(state: &ServerState, thread_id: &str, args: &Value) -> Result<String> {
    let hub = &state.hub;
    let registry = &hub.bosuns;
    let bosun_id = thread_bosun(state, thread_id).context("this thread is not a Bosun to-do thread")?;
    let date = date_field(args)?;
    match args.get("action").and_then(Value::as_str).unwrap_or("") {
        "tasks" => {
            let open: Vec<Value> = registry
                .list_tasks(Some(&bosun_id), false)
                .iter()
                .filter(|t| !t.key.starts_with("plan:"))
                .map(task_line)
                .collect();
            Ok(serde_json::to_string(&json!({ "tasks": open }))?)
        }
        "get" => Ok(serde_json::to_string(&registry.plan(&date).0)?),
        "set" => {
            let bosun = registry.get(&bosun_id).context("this to-do's Bosun no longer exists")?;
            let raw = args.get("blocks").and_then(Value::as_array).context("set needs blocks")?;
            let now = now_iso();
            let mut blocks = Vec::new();
            for b in raw {
                let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
                let mut items = Vec::new();
                for it in b.get("items").and_then(Value::as_array).into_iter().flatten() {
                    let task_id = match text(it, "taskId") {
                        Some(id) => id,
                        None => {
                            let Some(title) = text(it, "title") else { continue };
                            let source = TaskSource { kind: "manual".into(), ..Default::default() };
                            let mut task = new_task(&bosun.id, format!("adhoc:{}", new_id()), &title, TaskStatus::Todo, source, &now);
                            task.client = text(it, "client");
                            task.workspace_id = Some(bosun.home_workspace_id.clone());
                            push_history(&mut task, Some(TaskStatus::Todo), "Added to the day plan", "bosun", &now);
                            registry.insert_task(task).0.id
                        }
                    };
                    let steps = it
                        .get("steps")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(|s| PlanStep { id: String::new(), text: s.to_string(), done: false })
                        .collect();
                    items.push(PlanItem { id: String::new(), task_id, steps });
                }
                blocks.push(PlanBlock {
                    id: String::new(),
                    title: text(b, "title").unwrap_or_default(),
                    start: text(b, "start"),
                    end: text(b, "end"),
                    items,
                });
            }
            let plan = registry.put_plan(DayPlan { date, blocks, updated_at: String::new() })?;
            hub.broadcast_state("bosunTasks", None);
            hub.broadcast_state("bosunPlan", None);
            let rows: usize = plan.blocks.iter().map(|b| b.items.len()).sum();
            Ok(format!("Saved the plan for {}: {} blocks, {rows} rows.", plan.date, plan.blocks.len()))
        }
        other => anyhow::bail!("action must be tasks, get or set (got {other:?})"),
    }
}

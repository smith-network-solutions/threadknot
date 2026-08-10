//! Scheduled runs: recurring agent turns (Codex-automation style, run locally).
//!
//! Cadences are human presets (hourly / daily / weekdays / weekly at a local
//! time), never cron. The scheduler is a background loop in the same style as
//! `usage::spawn_poller`: tick every 30 s (cheap; robust across suspend/clock
//! drift, where a long `sleep_until` is not) plus a `Notify` kick whenever a
//! schedule is created or edited. Each firing creates a FRESH thread in the
//! target project and starts a normal turn, so persistence, streaming, the
//! thread list, and done/failed notifications all come for free.
//!
//! Catch-up policy: a due time missed by ≤ 60 min (laptop suspend, brief
//! restart) still fires; older misses are skipped with a note in `last_error`
//! so the user can see why nothing ran, and the schedule rolls forward.
//!
//! A schedule carrying a `dispatch` block fires workers instead of a turn (see
//! `fire_dispatch`). Schedules are deliberately NOT routable: the machine
//! holding the record is the one that ticks it, and the machines it dispatches
//! to are named in the record. A nightly cross-platform build is one schedule
//! on one box, not three schedules that must be kept in step.

use crate::agents::Hub;
use crate::protocol::{now_iso, AgentEvent, Cadence, Schedule, ThreadStatus};
use crate::server::ServerState;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Notify;

/// Fire anything due within this window after its planned time; skip older.
const CATCH_UP: Duration = Duration::minutes(60);
const TICK: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
pub struct SchedState {
    /// Poked on schedule create/update/run so the loop re-plans immediately.
    pub kick: Notify,
}

fn iso(t: DateTime<Local>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

/// Resolve a local wall-clock instant, stepping forward out of a DST gap.
fn resolve_local(date: chrono::NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    let naive = date.and_time(time);
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => Some(t),
        chrono::LocalResult::Ambiguous(t, _) => Some(t),
        chrono::LocalResult::None => Local
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest(),
    }
}

/// Next `time`-of-day strictly after `after` on a day whose weekday (0=Sun) is
/// in `days`.
fn next_at_time(after: DateTime<Local>, time: &str, days: &[u8]) -> Option<DateTime<Local>> {
    let time = parse_hhmm(time)?;
    for offset in 0..=14 {
        let date = after.date_naive() + Duration::days(offset);
        let dow = date.weekday().num_days_from_sunday() as u8;
        if !days.contains(&dow) {
            continue;
        }
        if let Some(t) = resolve_local(date, time) {
            if t > after {
                return Some(t);
            }
        }
    }
    None
}

/// The first firing instant strictly after `after`. None only for degenerate
/// input (empty weekly day set, unparseable time).
pub fn next_occurrence(cadence: &Cadence, after: DateTime<Local>) -> Option<DateTime<Local>> {
    match cadence {
        Cadence::Hourly { every_hours } => {
            let every = (*every_hours).clamp(1, 24);
            let mut t = after
                .with_minute(0)?
                .with_second(0)?
                .with_nanosecond(0)?
                + Duration::hours(1);
            for _ in 0..48 {
                if t.hour() % every == 0 {
                    return Some(t);
                }
                t += Duration::hours(1);
            }
            None
        }
        Cadence::Daily { time } => next_at_time(after, time, &[0, 1, 2, 3, 4, 5, 6]),
        Cadence::Weekdays { time } => next_at_time(after, time, &[1, 2, 3, 4, 5]),
        Cadence::Weekly { days, time } => next_at_time(after, time, days),
    }
}

/// ISO string for the next firing from now (used when creating/editing).
pub fn next_run_iso(cadence: &Cadence) -> Option<String> {
    next_occurrence(cadence, Local::now()).map(iso)
}

/// This firing's thread: fresh, titled, and announced. Titled before anything
/// runs in it so first-message title generation recognizes it as intentional
/// and leaves it alone.
fn open_thread(hub: &Arc<Hub>, schedule: &Schedule) -> Result<String> {
    let thread = hub.store.create_thread(
        schedule.project_id.clone(),
        schedule.agent,
        schedule.settings.clone(),
    )?;
    let title = format!("{} · {}", schedule.name, Local::now().format("%b %-d, %H:%M"));
    let _ = hub.store.update_thread(&thread.id, |t| t.title = title);
    hub.broadcast_state("threads", Some(schedule.project_id.clone()));
    Ok(thread.id)
}

/// Create a fresh thread for this schedule and start the turn. Returns the new
/// thread id.
pub fn fire(hub: &Arc<Hub>, schedule: &Schedule) -> Result<String> {
    let thread_id = open_thread(hub, schedule)?;
    hub.start_turn(&thread_id, schedule.prompt.clone(), Vec::new())?;
    Ok(thread_id)
}

/// Dispatch mode: the firing's thread is a COORDINATOR — it runs no turn of its
/// own, it holds the crew. One dispatch per named machine, created through
/// `dispatch.create` rather than the ledger directly so a scheduled dispatch
/// gets exactly the checks an agent-issued one does: target consent, the
/// agent-installed check, access narrowing, depth and concurrency caps.
///
/// Partial failure is reported, never swallowed. A nightly build that quietly
/// ran on two machines out of three is worse than one that ran on none, because
/// the missing platform is discovered by a user rather than by the schedule.
pub async fn fire_dispatch(state: &ServerState, schedule: &Schedule) -> Result<String> {
    let hub = &state.hub;
    let spec = schedule
        .dispatch
        .as_ref()
        .context("not a dispatching schedule")?;
    let thread_id = open_thread(hub, schedule)?;

    // The brief in the user slot, flagged `injected`: it is a machine-issued
    // instruction, and the UI labels it as one. It also un-parks the thread and
    // marks it Running, which is honest — this thread has workers out.
    hub.emit(
        &thread_id,
        AgentEvent::UserMessage {
            text: schedule.prompt.clone(),
            attachments: Vec::new(),
            mid_turn: false,
            injected: true,
        },
    );

    // No machine named is the same default the `dispatch` tool takes: here.
    let targets: Vec<Option<&str>> = if spec.machines.is_empty() {
        vec![None]
    } else {
        spec.machines.iter().map(|m| Some(m.as_str())).collect()
    };
    let label = spec
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or(&schedule.name);

    let mut sent = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for target in &targets {
        match send_one(state, &thread_id, schedule, spec, label, *target).await {
            Ok(()) => sent += 1,
            Err(e) => failures.push(format!("{}: {e:#}", target.unwrap_or("this machine"))),
        }
    }

    if sent == 0 {
        // Nothing is out, so nothing will ever settle this thread. The Error
        // event both says why and returns it to Idle.
        let why = format!("no worker started — {}", failures.join("; "));
        hub.emit(&thread_id, AgentEvent::Error { message: why.clone() });
        anyhow::bail!(why);
    }
    if !failures.is_empty() {
        // A Status line, not an Error: workers ARE running, and Error would
        // settle the thread out from under them.
        hub.emit(
            &thread_id,
            AgentEvent::Status {
                text: format!(
                    "{sent} of {} workers started. Refused — {}",
                    targets.len(),
                    failures.join("; ")
                ),
            },
        );
    }
    Ok(thread_id)
}

/// One target of a scheduled fan-out.
async fn send_one(
    state: &ServerState,
    thread_id: &str,
    schedule: &Schedule,
    spec: &crate::protocol::ScheduleDispatch,
    label: &str,
    machine: Option<&str>,
) -> Result<()> {
    let target = crate::mcp_fleet::resolve_machine(state, machine)?;
    let project_id =
        crate::mcp_fleet::resolve_root(state, thread_id, &target, spec.root.as_deref())?;
    // A fan-out's rows are unreadable if every one of them says the same thing.
    let label = if spec.machines.len() > 1 {
        format!("{label} · {}", target.name)
    } else {
        label.to_string()
    };

    let mut payload = json!({
        "parentThreadId": thread_id,
        "brief": schedule.prompt,
        "label": label,
        "machineId": target.machine_id,
        "projectId": project_id,
        "syncRef": spec.sync_ref,
    });
    if let Some(agent) = spec.agent {
        payload["agent"] = json!(agent);
    }
    for (key, value) in [("model", &spec.model), ("effort", &spec.effort)] {
        if let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            payload[key] = json!(v);
        }
    }
    crate::dispatch::handle(
        state,
        &crate::mobile::Principal::Master,
        "dispatch.create",
        &payload,
    )
    .await?;
    Ok(())
}

/// A coordinator thread has nothing of its own to finish, so nothing marks it
/// idle when the crew is done. Called as each report lands: once no dispatch
/// for this parent is still live and no session is running in it, settle it.
/// A thread whose own agent ran the turn is already Idle here, so this is a
/// no-op for every dispatch that was issued by an agent rather than a schedule.
pub fn settle_if_idle(hub: &Arc<Hub>, thread_id: &str) {
    if hub.is_running(thread_id) || hub.dispatch.live_for_parent(thread_id) > 0 {
        return;
    }
    let settled = hub.store.update_thread(thread_id, |t| {
        if t.status == ThreadStatus::Running {
            t.status = ThreadStatus::Idle;
        }
    });
    if let Ok(thread) = settled {
        hub.broadcast_state("threads", Some(thread.project_id));
    }
}

async fn tick(state: &ServerState) {
    let hub = &state.hub;
    let now = Local::now();
    let mut changed = false;

    for schedule in hub.store.list_schedules() {
        if !schedule.enabled {
            continue;
        }
        let due = schedule
            .next_run_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Local));

        let Some(due) = due else {
            // Missing/unparseable plan (fresh schedule or clock weirdness).
            let next = next_occurrence(&schedule.cadence, now).map(iso);
            let _ = hub.store.update_schedule(&schedule.id, |s| s.next_run_at = next);
            changed = true;
            continue;
        };
        if due > now {
            continue;
        }

        let missed = now - due > CATCH_UP;
        let result = if missed {
            Err(anyhow::anyhow!(
                "Missed the {} run (Threadknot wasn't running); next one is scheduled",
                due.format("%b %-d %H:%M"),
            ))
        } else {
            run(state, &schedule).await
        };
        if let Err(e) = &result {
            tracing::warn!("schedule '{}' did not run: {e:#}", schedule.name);
        }
        let next = next_occurrence(&schedule.cadence, now).map(iso);
        let _ = hub.store.update_schedule(&schedule.id, |s| {
            s.next_run_at = next;
            match &result {
                Ok(thread_id) => {
                    s.last_run_at = Some(now_iso());
                    s.last_thread_id = Some(thread_id.clone());
                    s.last_error = None;
                }
                Err(e) => s.last_error = Some(format!("{e:#}")),
            }
        });
        changed = true;
    }

    if changed {
        hub.broadcast_state("schedules", None);
    }
}

/// One firing, whichever kind this schedule is.
async fn run(state: &ServerState, schedule: &Schedule) -> Result<String> {
    match schedule.dispatch {
        Some(_) => fire_dispatch(state, schedule).await,
        None => fire(&state.hub, schedule),
    }
}

/// Takes the whole `ServerState`, not just the hub, because a dispatching
/// schedule needs the mesh — the same reason `dispatch::spawn_reconciler` does.
pub fn spawn_scheduler(state: ServerState) {
    tokio::spawn(async move {
        loop {
            tick(&state).await;
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = state.hub.sched.kick.notified() => {}
            }
        }
    });
}

/// "Run now" from the UI: fires immediately without touching the plan. Also the
/// way a disabled schedule earns its keep as a saved, one-click dispatch.
pub async fn run_now(state: &ServerState, schedule_id: &str) -> Result<String> {
    let hub = &state.hub;
    let schedule = hub
        .store
        .schedule(schedule_id)
        .ok_or_else(|| anyhow::anyhow!("unknown schedule"))?;
    let result = run(state, &schedule).await;
    let _ = hub.store.update_schedule(schedule_id, |s| match &result {
        Ok(thread_id) => {
            s.last_run_at = Some(now_iso());
            s.last_thread_id = Some(thread_id.clone());
            s.last_error = None;
        }
        Err(e) => s.last_error = Some(format!("{e:#}")),
    });
    hub.broadcast_state("schedules", None);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn daily_rolls_to_tomorrow_when_past() {
        // 2026-07-20 is a Monday.
        let c = Cadence::Daily { time: "09:00".into() };
        assert_eq!(
            next_occurrence(&c, at(2026, 7, 20, 10, 0)),
            Some(at(2026, 7, 21, 9, 0))
        );
        assert_eq!(
            next_occurrence(&c, at(2026, 7, 20, 8, 0)),
            Some(at(2026, 7, 20, 9, 0))
        );
    }

    #[test]
    fn weekdays_skip_weekend() {
        let c = Cadence::Weekdays { time: "09:00".into() };
        // Friday after 9am -> Monday.
        assert_eq!(
            next_occurrence(&c, at(2026, 7, 24, 10, 0)),
            Some(at(2026, 7, 27, 9, 0))
        );
    }

    #[test]
    fn weekly_picks_next_listed_day() {
        // days: Wednesday (3) and Saturday (6); from Monday -> Wednesday.
        let c = Cadence::Weekly { days: vec![3, 6], time: "07:30".into() };
        assert_eq!(
            next_occurrence(&c, at(2026, 7, 20, 12, 0)),
            Some(at(2026, 7, 22, 7, 30))
        );
        // Empty day set never fires.
        let none = Cadence::Weekly { days: vec![], time: "07:30".into() };
        assert_eq!(next_occurrence(&none, at(2026, 7, 20, 12, 0)), None);
    }

    /// Every schedule already on disk predates dispatch mode. The block is
    /// optional precisely so those keep loading — a `Schedule` that failed to
    /// deserialize would take `projects.json` with it, and the whole file is
    /// one blob.
    #[test]
    fn a_schedule_written_before_dispatch_mode_still_loads() {
        let stored = serde_json::json!({
            "id": "s1",
            "projectId": "p1",
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "edits", "mode": "build" },
            "name": "nightly",
            "prompt": "run the suite",
            "cadence": { "type": "daily", "time": "03:00" },
            "enabled": true,
            "createdAt": "2026-01-01T00:00:00Z",
        });
        let schedule: Schedule = serde_json::from_value(stored).expect("legacy schedule loads");
        assert!(schedule.dispatch.is_none(), "absent means run here");

        // And a dispatching one round-trips through the same blob.
        let with = serde_json::json!({
            "machines": ["windows", "mac"],
            "agent": "codex",
            "root": "threadknot",
            "syncRef": true,
        });
        let spec: crate::protocol::ScheduleDispatch =
            serde_json::from_value(with).expect("dispatch block parses");
        assert_eq!(spec.machines.len(), 2);
        assert!(spec.sync_ref);
        assert!(spec.model.is_none(), "absent model tracks the agent default");
    }

    #[test]
    fn hourly_hits_divisible_hours() {
        let c = Cadence::Hourly { every_hours: 6 };
        // 07:10 -> 12:00; 12:00 sharp -> 18:00 (strictly after).
        assert_eq!(
            next_occurrence(&c, at(2026, 7, 20, 7, 10)),
            Some(at(2026, 7, 20, 12, 0))
        );
        assert_eq!(
            next_occurrence(&c, at(2026, 7, 20, 12, 0)),
            Some(at(2026, 7, 20, 18, 0))
        );
        // Every hour: 23:30 -> next midnight.
        let c1 = Cadence::Hourly { every_hours: 1 };
        assert_eq!(
            next_occurrence(&c1, at(2026, 7, 20, 23, 30)),
            Some(at(2026, 7, 21, 0, 0))
        );
    }
}

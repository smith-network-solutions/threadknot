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

use crate::agents::Hub;
use crate::protocol::{now_iso, Cadence, Schedule};
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike};
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

/// Create a fresh thread for this schedule and start the turn. Returns the new
/// thread id. The thread is titled before `start_turn` so first-message title
/// generation recognizes it as intentional and leaves it alone.
pub fn fire(hub: &Arc<Hub>, schedule: &Schedule) -> Result<String> {
    let thread = hub.store.create_thread(
        schedule.project_id.clone(),
        schedule.agent,
        schedule.settings.clone(),
    )?;
    let title = format!("{} · {}", schedule.name, Local::now().format("%b %-d, %H:%M"));
    let _ = hub.store.update_thread(&thread.id, |t| t.title = title);
    hub.broadcast_state("threads", Some(schedule.project_id.clone()));
    hub.start_turn(&thread.id, schedule.prompt.clone(), Vec::new())?;
    Ok(thread.id)
}

fn tick(hub: &Arc<Hub>) {
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
            fire(hub, &schedule)
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

pub fn spawn_scheduler(hub: Arc<Hub>) {
    tokio::spawn(async move {
        loop {
            tick(&hub);
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = hub.sched.kick.notified() => {}
            }
        }
    });
}

/// "Run now" from the UI: fires immediately without touching the plan.
pub fn run_now(hub: &Arc<Hub>, schedule_id: &str) -> Result<String> {
    let schedule = hub
        .store
        .schedule(schedule_id)
        .ok_or_else(|| anyhow::anyhow!("unknown schedule"))?;
    let result = fire(hub, &schedule);
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

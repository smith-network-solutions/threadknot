//! Short-lived desktop activity leases and unread push catch-up, scoped to the
//! authenticated person. No activity or input history is written to disk.
use crate::push::{PushJob, PushKind};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const LEASE: Duration = Duration::from_secs(12);
const MAX_PENDING: usize = 512;
const MAX_CLIENTS: usize = 128;
const MAX_READS: usize = 4096;

#[derive(Default)]
pub struct Presence {
    clients: HashMap<(String, String), Instant>,
    read: HashMap<(String, String), (u64, Instant)>,
    pending: HashMap<(String, String), PushJob>,
}

impl Presence {
    pub fn report(&mut self, person: &str, client: &str, active_for_ms: u64, now: Instant) {
        self.clients.retain(|_, until| *until > now);
        let key = (person.to_owned(), client.to_owned());
        if active_for_ms == 0 {
            self.clients.remove(&key);
        } else if self.clients.contains_key(&key) || self.clients.len() < MAX_CLIENTS {
            self.clients
                .insert(key, now + Duration::from_millis(active_for_ms).min(LEASE));
        }
    }

    pub fn acknowledge(&mut self, person: &str, thread: &str, seq: u64, now: Instant) {
        // Retain a watermark even if the send worker hasn't consumed the event
        // yet. A read racing enqueue must not resurrect an old notification.
        if self.read.len() >= MAX_READS {
            if let Some(oldest) = self
                .read
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
            {
                self.read.remove(&oldest);
            }
        }
        let key = (person.to_owned(), thread.to_owned());
        let value = self.read.entry(key.clone()).or_insert((seq, now));
        value.0 = value.0.max(seq);
        value.1 = now;
        if self
            .pending
            .get(&key)
            .is_some_and(|job| job.seq.is_some_and(|s| s <= value.0))
        {
            self.pending.remove(&key);
        }
    }

    fn active(&self, person: &str, now: Instant) -> bool {
        self.clients
            .iter()
            .any(|((p, _), until)| p == person && *until > now)
    }

    pub fn offer(&mut self, person: String, job: PushJob, now: Instant) -> Vec<(String, PushJob)> {
        if job.kind == PushKind::Test {
            return vec![(person, job)];
        }
        let key = (person.clone(), job.thread_id.clone());
        if job
            .seq
            .is_some_and(|seq| self.read.get(&key).is_some_and(|(read, _)| seq <= *read))
        {
            return vec![];
        }
        // Keep the newest status per person/thread. Out-of-order driver sends
        // cannot overwrite a later attention event already held for catch-up.
        if self.pending.get(&key).is_some_and(|old| old.seq > job.seq) {
            return vec![];
        }
        if self.active(&person, now) {
            if self.pending.contains_key(&key) || self.pending.len() < MAX_PENDING {
                self.pending.insert(key, job);
                return vec![];
            }
            // Capacity exhaustion fails open: send rather than lose the alert.
        }
        self.pending.remove(&key);
        vec![(person, job)]
    }

    pub fn ready(&mut self, now: Instant) -> Vec<(String, PushJob)> {
        self.clients.retain(|_, until| *until > now);
        let keys: Vec<_> = self
            .pending
            .keys()
            .filter(|(person, _)| !self.active(person, now))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| self.pending.remove(&key).map(|job| (key.0, job)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(seq: u64) -> PushJob {
        PushJob {
            kind: PushKind::TurnCompleted,
            project_id: "p".into(),
            workspace_id: "w".into(),
            project_name: "Project".into(),
            thread_id: "thread".into(),
            thread_title: "Chat".into(),
            seq: Some(seq),
            notice: None,
            only_device: None,
        }
    }
    #[test]
    fn active_then_away_delivers_only_latest_unread_status() {
        let mut p = Presence::default();
        let now = Instant::now();
        p.report("owner", "desktop", 30_000, now);
        assert!(p.offer("owner".into(), job(1), now).is_empty());
        assert!(p.offer("owner".into(), job(2), now).is_empty());
        assert!(p.ready(now + Duration::from_secs(11)).is_empty());
        let ready = p.ready(now + Duration::from_secs(12));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].1.seq, Some(2));
        assert!(p.ready(now + Duration::from_secs(13)).is_empty());
    }
    #[test]
    fn read_races_enqueue_and_cannot_erase_newer_activity() {
        let mut p = Presence::default();
        let now = Instant::now();
        p.report("owner", "desktop", 30_000, now);
        p.acknowledge("owner", "thread", 5, now);
        assert!(p.offer("owner".into(), job(5), now).is_empty());
        assert!(p.offer("owner".into(), job(6), now).is_empty());
        p.acknowledge("owner", "thread", 4, now);
        assert_eq!(p.ready(now + Duration::from_secs(12))[0].1.seq, Some(6));
    }
    #[test]
    fn opening_thread_clears_held_push_and_people_are_independent() {
        let mut p = Presence::default();
        let now = Instant::now();
        p.report("owner", "desktop", 30_000, now);
        assert!(p.offer("owner".into(), job(1), now).is_empty());
        assert_eq!(p.offer("guest".into(), job(1), now).len(), 1);
        p.acknowledge("owner", "thread", 1, now);
        assert!(p.ready(now + Duration::from_secs(12)).is_empty());
    }
    #[test]
    fn multiple_desktops_and_last_input_deadline() {
        let mut p = Presence::default();
        let now = Instant::now();
        p.report("owner", "a", 1_000, now);
        p.report("owner", "b", 8_000, now);
        p.offer("owner".into(), job(1), now);
        p.report("owner", "a", 0, now);
        assert!(p.ready(now + Duration::from_secs(7)).is_empty());
        assert_eq!(p.ready(now + Duration::from_secs(8)).len(), 1);
    }
    #[test]
    fn tests_bypass_suppression() {
        let mut p = Presence::default();
        let now = Instant::now();
        p.report("owner", "desktop", 30_000, now);
        let mut j = job(1);
        j.kind = PushKind::Test;
        assert_eq!(p.offer("owner".into(), j, now).len(), 1);
    }
}

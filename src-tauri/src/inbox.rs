//! The notification inbox: every notification Threadknot would send, kept so
//! a burst of them can be worked through afterwards instead of vanishing with
//! the lock screen.
//!
//! Recorded at the one place a phone push is queued (`Hub::enqueue_push`), and
//! recorded whether or not a phone is paired or the push is later held back by
//! presence: the inbox is the full list of "this wanted you", and presence
//! only decides whether it also buzzed. Read state lives here, server-side, so
//! opening a chat on the desktop clears it on the phone too.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::{new_id, now_iso};
use crate::push::{PushJob, PushKind};

/// Entries older than this leave the file.
const KEEP_DAYS: i64 = 14;
/// And never more than this many, newest kept.
const KEEP_MAX: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxEntry {
    pub id: String,
    pub at: String,
    /// `turn_completed`, `approval_request`, `question_request`,
    /// `dispatch_finished`, `bosun`, `error` (the push payload's eventKind).
    pub kind: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub project_name: String,
    /// Empty for notices that belong to no thread (a Bosun sweep).
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub thread_title: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct InboxFile {
    #[serde(default)]
    entries: Vec<InboxEntry>,
}

/// What to mark read. Any combination; an empty request marks nothing.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkRead {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub all: bool,
}

pub struct Inbox {
    path: PathBuf,
    entries: Mutex<Vec<InboxEntry>>,
}

impl Inbox {
    /// A file that will not parse is set aside rather than failing startup:
    /// the inbox is a convenience, never a reason the app will not open.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("inbox.json");
        let entries = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<InboxFile>(&raw) {
                Ok(file) => file.entries,
                Err(e) => {
                    tracing::warn!("inbox.json unreadable, starting empty: {e}");
                    let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    fn flush(&self, entries: &[InboxEntry]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        let file = InboxFile {
            entries: entries.to_vec(),
        };
        std::fs::write(&tmp, serde_json::to_string(&file)?).context("write inbox")?;
        std::fs::rename(&tmp, &self.path).context("replace inbox.json")?;
        Ok(())
    }

    /// Record one notification. Test pushes are not notifications about
    /// anything and are skipped. Returns the unread count after.
    pub fn record(&self, job: &PushJob) -> usize {
        if job.kind == PushKind::Test {
            return self.unread();
        }
        let (title, body) = match &job.notice {
            Some(n) => (n.title.clone(), n.body.clone()),
            None => {
                let what = if job.thread_title.is_empty() {
                    job.project_name.clone()
                } else {
                    job.thread_title.clone()
                };
                (what, job.kind.label().to_string())
            }
        };
        let entry = InboxEntry {
            id: new_id(),
            at: now_iso(),
            kind: job.kind.event_kind().to_string(),
            workspace_id: job.workspace_id.clone(),
            project_id: job.project_id.clone(),
            project_name: job.project_name.clone(),
            thread_id: job.thread_id.clone(),
            thread_title: job.thread_title.clone(),
            title,
            body,
            read_at: None,
        };
        let mut entries = self.entries.lock().unwrap();
        entries.push(entry);
        prune(&mut entries, Utc::now());
        if let Err(e) = self.flush(&entries) {
            tracing::warn!("inbox: {e:#}");
        }
        entries.iter().filter(|e| e.read_at.is_none()).count()
    }

    /// Newest first.
    pub fn list(&self, limit: usize) -> Vec<InboxEntry> {
        let entries = self.entries.lock().unwrap();
        entries.iter().rev().take(limit).cloned().collect()
    }

    pub fn unread(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.read_at.is_none())
            .count()
    }

    /// Returns how many entries changed (0 means nothing to broadcast).
    pub fn mark_read(&self, req: &MarkRead) -> usize {
        let now = now_iso();
        let mut entries = self.entries.lock().unwrap();
        let mut changed = 0;
        for e in entries.iter_mut().filter(|e| e.read_at.is_none()) {
            let hit = req.all
                || req.ids.contains(&e.id)
                || req
                    .thread_id
                    .as_deref()
                    .is_some_and(|t| !t.is_empty() && t == e.thread_id);
            if hit {
                e.read_at = Some(now.clone());
                changed += 1;
            }
        }
        if changed > 0 {
            if let Err(e) = self.flush(&entries) {
                tracing::warn!("inbox: {e:#}");
            }
        }
        changed
    }
}

fn prune(entries: &mut Vec<InboxEntry>, now: DateTime<Utc>) {
    let cutoff = now - Duration::days(KEEP_DAYS);
    entries.retain(|e| {
        DateTime::parse_from_rfc3339(&e.at)
            .map(|at| at.with_timezone(&Utc) > cutoff)
            .unwrap_or(true)
    });
    if entries.len() > KEEP_MAX {
        let extra = entries.len() - KEEP_MAX;
        entries.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::EventNotice;

    fn job(kind: PushKind, thread: &str) -> PushJob {
        PushJob {
            kind,
            seq: None,
            project_id: "p".into(),
            workspace_id: "w".into(),
            project_name: "Project".into(),
            thread_id: thread.into(),
            thread_title: format!("Chat {thread}"),
            notice: Some(EventNotice {
                title: format!("Done · {thread}"),
                body: "finished".into(),
            }),
            only_device: None,
            badge: None,
        }
    }

    fn inbox() -> Inbox {
        let dir = std::env::temp_dir().join(format!("threadknot-inbox-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        Inbox::open(&dir)
    }

    #[test]
    fn records_lists_newest_first_and_skips_tests() {
        let inbox = inbox();
        assert_eq!(inbox.record(&job(PushKind::TurnCompleted, "a")), 1);
        assert_eq!(inbox.record(&job(PushKind::Bosun, "b")), 2);
        assert_eq!(inbox.record(&job(PushKind::Test, "c")), 2, "test pushes are not kept");
        let list = inbox.list(10);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].thread_id, "b");
        assert_eq!(list[0].kind, "bosun");
        assert_eq!(list[1].title, "Done · a");
    }

    #[test]
    fn mark_read_by_thread_id_and_all() {
        let inbox = inbox();
        inbox.record(&job(PushKind::TurnCompleted, "a"));
        inbox.record(&job(PushKind::TurnCompleted, "a"));
        inbox.record(&job(PushKind::QuestionRequest, "b"));
        inbox.record(&job(PushKind::Error, "c"));
        let thread = MarkRead { thread_id: Some("a".into()), ..Default::default() };
        assert_eq!(inbox.mark_read(&thread), 2);
        assert_eq!(inbox.unread(), 2);
        let b = inbox.list(10).into_iter().find(|e| e.thread_id == "b").unwrap();
        assert_eq!(inbox.mark_read(&MarkRead { ids: vec![b.id], ..Default::default() }), 1);
        assert_eq!(inbox.mark_read(&MarkRead { all: true, ..Default::default() }), 1);
        assert_eq!(inbox.unread(), 0);
        assert_eq!(inbox.mark_read(&MarkRead { all: true, ..Default::default() }), 0, "nothing left to change");
    }

    #[test]
    fn survives_reopen_and_prunes_old_entries() {
        let dir = std::env::temp_dir().join(format!("threadknot-inbox-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let inbox = Inbox::open(&dir);
            inbox.record(&job(PushKind::TurnCompleted, "a"));
        }
        let inbox = Inbox::open(&dir);
        assert_eq!(inbox.unread(), 1);
        let mut entries = vec![
            InboxEntry { at: (Utc::now() - Duration::days(30)).to_rfc3339(), ..inbox.list(1)[0].clone() },
            inbox.list(1)[0].clone(),
        ];
        prune(&mut entries, Utc::now());
        assert_eq!(entries.len(), 1, "a month-old entry is dropped");
    }
}

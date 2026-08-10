//! Expo push delivery. A bounded queue decouples agent event emission from
//! network I/O: `Hub::emit` never waits on Expo. The worker batches messages,
//! retries transient failures with backoff, tracks tickets → receipts, and
//! disables tokens Expo reports as dead (`DeviceNotRegistered`).

use crate::mobile::MobileStore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const QUEUE_CAP: usize = 512;
const MAX_ATTEMPTS: u32 = 5;
const BATCH_SIZE: usize = 100;
/// Expo asks that receipts be checked ~15 minutes after sending.
const RECEIPT_DELAY: Duration = Duration::from_secs(15 * 60);
const RECEIPT_POLL: Duration = Duration::from_secs(60);

fn push_url() -> String {
    std::env::var("THREADKNOT_EXPO_PUSH_URL")
        .unwrap_or_else(|_| "https://exp.host/--/api/v2/push/send".into())
}

fn receipts_url() -> String {
    std::env::var("THREADKNOT_EXPO_RECEIPTS_URL")
        .unwrap_or_else(|_| "https://exp.host/--/api/v2/push/getReceipts".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushKind {
    TurnCompleted,
    ApprovalRequest,
    QuestionRequest,
    /// A dispatched worker reported back. Its own kind rather than a
    /// `TurnCompleted`: the thread it names is the PARENT, not the thread whose
    /// turn ended, and a phone routing on `eventKind` should open the one you
    /// can act on. An older mobile build that doesn't know the string falls
    /// through to just showing the notification, which is the right default.
    DispatchFinished,
    Error,
    Test,
}

impl PushKind {
    fn label(self) -> &'static str {
        match self {
            PushKind::TurnCompleted => "Turn complete",
            PushKind::ApprovalRequest => "Approval needed",
            PushKind::QuestionRequest => "Question waiting",
            PushKind::DispatchFinished => "Dispatch finished",
            PushKind::Error => "Agent error",
            PushKind::Test => "Test notification",
        }
    }

    /// Wire value carried in the notification data payload (mobile routing).
    fn event_kind(self) -> &'static str {
        match self {
            PushKind::TurnCompleted => "turn_completed",
            PushKind::ApprovalRequest => "approval_request",
            PushKind::QuestionRequest => "question_request",
            PushKind::DispatchFinished => "dispatch_finished",
            PushKind::Error => "error",
            PushKind::Test => "test",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PushJob {
    pub kind: PushKind,
    pub project_id: String,
    /// Workspace the thread belongs to — the unit devices subscribe to. Empty
    /// for server-level notices that belong to no workspace.
    pub workspace_id: String,
    pub project_name: String,
    pub thread_id: String,
    pub thread_title: String,
    /// Canonical copy shared with desktop/browser notifications. `None` is
    /// retained for transport tests and callers from older code paths.
    pub notice: Option<crate::protocol::EventNotice>,
    /// Test pushes target exactly one device; agent events fan out.
    pub only_device: Option<String>,
}

/// A sent ticket awaiting its receipt: (receipt id, expo push token, due time).
struct PendingReceipt {
    id: String,
    token: String,
    due: Instant,
}

pub struct PushService {
    tx: mpsc::Sender<PushJob>,
}

impl PushService {
    pub fn spawn(mobile: Arc<MobileStore>, server_id: String) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(QUEUE_CAP);
        let receipts: Arc<Mutex<Vec<PendingReceipt>>> = Arc::new(Mutex::new(Vec::new()));
        let client = reqwest::Client::new();

        tokio::spawn(worker(
            rx,
            Arc::clone(&mobile),
            server_id,
            Arc::clone(&receipts),
            client.clone(),
        ));
        tokio::spawn(receipt_loop(mobile, receipts, client));

        Arc::new(Self { tx })
    }

    /// Non-blocking enqueue; drops (with a log) if the queue is saturated so
    /// agent turns can never stall on push delivery.
    pub fn enqueue(&self, job: PushJob) {
        if let Err(e) = self.tx.try_send(job) {
            tracing::warn!("push queue full, dropping notification: {e}");
        }
    }
}

async fn worker(
    mut rx: mpsc::Receiver<PushJob>,
    mobile: Arc<MobileStore>,
    server_id: String,
    receipts: Arc<Mutex<Vec<PendingReceipt>>>,
    client: reqwest::Client,
) {
    while let Some(job) = rx.recv().await {
        let targets: Vec<crate::mobile::MobileDevice> = match &job.only_device {
            Some(id) => mobile
                .device(id)
                .into_iter()
                .filter(|d| d.expo_push_token.is_some())
                .collect(),
            None => mobile.push_targets(&job.workspace_id, job.kind == PushKind::Error),
        };
        if targets.is_empty() {
            continue;
        }

        let (title, body) = match &job.notice {
            Some(notice) => (notice.title.clone(), notice.body.clone()),
            None => {
                let title = if job.project_name.is_empty() {
                    "Threadknot".to_string()
                } else {
                    job.project_name.clone()
                };
                let body = if job.thread_title.is_empty() {
                    job.kind.label().to_string()
                } else {
                    format!("{} — {}", job.kind.label(), job.thread_title)
                };
                (title, body)
            }
        };
        let status_title = if job.project_name.is_empty() {
            "Threadknot".to_string()
        } else {
            job.project_name.clone()
        };
        let status_body = if job.thread_title.is_empty() {
            job.kind.label().to_string()
        } else {
            format!("{} — {}", job.kind.label(), job.thread_title)
        };
        let data = json!({
            "version": 1,
            "serverId": server_id,
            "projectId": job.project_id,
            "threadId": job.thread_id,
            "eventKind": job.kind.event_kind(),
        });

        // One message per *token*, not per device row. Re-pairing a phone creates
        // a new row while the OS keeps handing out the same Expo token, so a
        // device that had been paired three times received every notification
        // three times. Expo accepts duplicate recipients without complaint, which
        // is why this went unnoticed — the duplication is only visible on the
        // phone.
        // Duplicate rows for one physical phone can briefly disagree while a
        // re-pair is being cleaned up. Privacy fails closed: one row disabling
        // previews disables them for that token.
        let previews_by_token = token_preview_preferences(
            targets
                .iter()
                .map(|device| (device.expo_push_token.as_deref(), device.notification_previews)),
        );
        let messages: Vec<Value> =
            distinct_tokens(targets.iter().map(|d| d.expo_push_token.as_deref()))
                .into_iter()
                .map(|token| {
                    let show_preview = previews_by_token.get(token).copied().unwrap_or(false);
                    json!({
                        "to": token,
                        "title": if show_preview { &title } else { &status_title },
                        "body": if show_preview { &body } else { &status_body },
                        "data": data,
                        "sound": "default",
                        "priority": "high",
                        "channelId": "threadknot",
                    })
                })
                .collect();

        for chunk in messages.chunks(BATCH_SIZE) {
            send_batch(&client, &mobile, &receipts, chunk).await;
        }
    }
}

/// Distinct push tokens, in first-seen order.
///
/// Re-pairing a phone creates a new device row while the OS keeps handing back
/// the same Expo token, so a device paired three times appeared three times in
/// `push_targets` and was sent every notification three times. Expo accepts
/// duplicate recipients without complaining — it issues a ticket per message —
/// so nothing upstream reports this. The only place it shows is the phone.
fn distinct_tokens<'a>(tokens: impl IntoIterator<Item = Option<&'a str>>) -> Vec<&'a str> {
    let mut seen = std::collections::HashSet::new();
    tokens
        .into_iter()
        .flatten()
        .filter(|token| seen.insert(*token))
        .collect()
}

/// Resolve preview permission per physical push token. Duplicate device rows
/// fail closed so a stale registration can never expose content a newer row
/// asked to hide.
fn token_preview_preferences<'a>(
    devices: impl IntoIterator<Item = (Option<&'a str>, bool)>,
) -> HashMap<&'a str, bool> {
    let mut by_token = HashMap::new();
    for (token, previews) in devices {
        if let Some(token) = token {
            by_token
                .entry(token)
                .and_modify(|show| *show &= previews)
                .or_insert(previews);
        }
    }
    by_token
}

/// Split a `PUSH_TOO_MANY_EXPERIENCE_IDS` rejection into one token group per
/// Expo project.
///
/// Expo refuses any request whose messages target more than one project, and it
/// answers with the partition it wants: `details` maps each project slug to the
/// tokens belonging to it. That is the only way to learn which project a token
/// belongs to — the token itself does not say, and nothing on this side records
/// it — so the error body is not merely a diagnostic, it is the fix.
///
/// This is not hypothetical tidiness. The Armada→Threadknot rename changed the
/// EAS project slug, so a device store that had ever paired with the old build
/// held `@servicestorm/armada-mobile` tokens alongside `.../threadknot-mobile`
/// ones. Every batch then mixed projects, every batch was rejected whole, and
/// **no device received anything at all** — including phones running the current
/// app, whose own token was perfectly valid.
fn experience_groups(body: &Value) -> Option<Vec<Vec<String>>> {
    let errors = body.get("errors")?.as_array()?;
    let conflict = errors
        .iter()
        .find(|e| e.get("code").and_then(|c| c.as_str()) == Some("PUSH_TOO_MANY_EXPERIENCE_IDS"))?;
    let details = conflict.get("details")?.as_object()?;
    let groups: Vec<Vec<String>> = details
        .values()
        .map(|tokens| {
            tokens
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        })
        .filter(|g: &Vec<String>| !g.is_empty())
        .collect();
    // One group is not a partition — resending it unchanged would loop.
    (groups.len() > 1).then_some(groups)
}

/// POST one batch, retrying transient failures (429 / 5xx / network) with
/// exponential backoff. Per-ticket errors are terminal for that message.
///
/// `allow_split` is false on the retry of a partitioned batch, so a server that
/// kept reporting a conflict could not drive this into unbounded recursion.
async fn send_batch(
    client: &reqwest::Client,
    mobile: &MobileStore,
    receipts: &Mutex<Vec<PendingReceipt>>,
    messages: &[Value],
) {
    send_batch_inner(client, mobile, receipts, messages, true).await
}

async fn send_batch_inner(
    client: &reqwest::Client,
    mobile: &MobileStore,
    receipts: &Mutex<Vec<PendingReceipt>>,
    messages: &[Value],
    allow_split: bool,
) {
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=MAX_ATTEMPTS {
        let resp = client
            .post(push_url())
            .json(&serde_json::Value::Array(messages.to_vec()))
            .send()
            .await;
        match resp {
            Ok(resp) if resp.status().is_success() => {
                let body: Value = resp.json().await.unwrap_or_default();
                handle_tickets(mobile, receipts, messages, &body);
                return;
            }
            Ok(resp)
                if resp.status().as_u16() == 429 || resp.status().is_server_error() =>
            {
                tracing::warn!(
                    "expo push transient failure (attempt {attempt}): HTTP {}",
                    resp.status()
                );
            }
            Ok(resp) => {
                let status = resp.status();
                // Read the body before deciding anything. Logging the status
                // alone is what made this class of failure undiagnosable: a bare
                // "HTTP 400" hid a message that named both the cause and the
                // remedy, and notifications were silently dead for days.
                let body: Value = resp.json().await.unwrap_or_default();

                if allow_split {
                    if let Some(groups) = experience_groups(&body) {
                        tracing::warn!(
                            "expo push spans {} projects; resending one request per project",
                            groups.len()
                        );
                        for tokens in groups {
                            let subset: Vec<Value> = messages
                                .iter()
                                .filter(|m| {
                                    m.get("to")
                                        .and_then(|t| t.as_str())
                                        .is_some_and(|t| tokens.iter().any(|k| k == t))
                                })
                                .cloned()
                                .collect();
                            if subset.is_empty() {
                                continue;
                            }
                            Box::pin(send_batch_inner(client, mobile, receipts, &subset, false))
                                .await;
                        }
                        return;
                    }
                }

                tracing::error!("expo push rejected: HTTP {status}: {body}");
                return;
            }
            Err(e) => {
                tracing::warn!("expo push network error (attempt {attempt}): {e}");
            }
        }
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    }
    tracing::error!("expo push failed after {MAX_ATTEMPTS} attempts, giving up on batch");
}

/// Tickets arrive positionally aligned with the sent messages. Record ok
/// tickets for the receipt pass; act on immediate `DeviceNotRegistered`.
fn handle_tickets(
    mobile: &MobileStore,
    receipts: &Mutex<Vec<PendingReceipt>>,
    messages: &[Value],
    body: &Value,
) {
    let Some(tickets) = body.get("data").and_then(|d| d.as_array()) else {
        return;
    };
    let due = Instant::now() + RECEIPT_DELAY;
    let mut pending = receipts.lock().unwrap();
    for (i, ticket) in tickets.iter().enumerate() {
        let token = messages
            .get(i)
            .and_then(|m| m.get("to"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        match ticket.get("status").and_then(|s| s.as_str()) {
            Some("ok") => {
                if let Some(id) = ticket.get("id").and_then(|v| v.as_str()) {
                    pending.push(PendingReceipt {
                        id: id.to_string(),
                        token: token.to_string(),
                        due,
                    });
                }
            }
            _ => {
                let detail = ticket
                    .pointer("/details/error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                if detail == "DeviceNotRegistered" && !token.is_empty() {
                    tracing::info!("expo token dead (ticket), disabling: {token}");
                    mobile.disable_push_token(token);
                } else {
                    tracing::warn!("expo push ticket error: {detail}");
                }
            }
        }
    }
}

/// Periodically resolve due receipts; `DeviceNotRegistered` here is the
/// authoritative dead-token signal (APNs/FCM rejections surface asynchronously).
async fn receipt_loop(
    mobile: Arc<MobileStore>,
    receipts: Arc<Mutex<Vec<PendingReceipt>>>,
    client: reqwest::Client,
) {
    loop {
        tokio::time::sleep(RECEIPT_POLL).await;
        let now = Instant::now();
        let (due, by_id): (Vec<String>, HashMap<String, String>) = {
            let mut pending = receipts.lock().unwrap();
            let mut ids = Vec::new();
            let mut map = HashMap::new();
            pending.retain(|r| {
                if r.due <= now {
                    ids.push(r.id.clone());
                    map.insert(r.id.clone(), r.token.clone());
                    false
                } else {
                    true
                }
            });
            (ids, map)
        };
        if due.is_empty() {
            continue;
        }
        for chunk in due.chunks(300) {
            let resp = client
                .post(receipts_url())
                .json(&json!({ "ids": chunk }))
                .send()
                .await;
            let Ok(resp) = resp else { continue };
            let Ok(body) = resp.json::<Value>().await else { continue };
            let Some(map) = body.get("data").and_then(|d| d.as_object()) else {
                continue;
            };
            for (id, receipt) in map {
                if receipt.get("status").and_then(|s| s.as_str()) == Some("error") {
                    let detail = receipt
                        .pointer("/details/error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    if detail == "DeviceNotRegistered" {
                        if let Some(token) = by_id.get(id) {
                            tracing::info!("expo token dead (receipt), disabling: {token}");
                            mobile.disable_push_token(token);
                        }
                    } else {
                        tracing::warn!("expo push receipt error: {detail}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body Expo returned for this machine on 2026-08-09, when the device
    /// store still held tokens from the pre-rename `armada-mobile` project. The
    /// structure is verbatim rather than paraphrased: it is the contract this
    /// parsing depends on, and a hand-written approximation would not have caught
    /// that `details` is keyed by project slug.
    ///
    /// The **token values and request id are synthetic**, and must stay that way.
    /// A real `ExponentPushToken` is enough for anyone to push to that device, so
    /// they do not belong in a published repository. Only their shape and their
    /// grouping matter here, both of which are preserved.
    const CONFLICT: &str = r#"{"errors":[{"code":"PUSH_TOO_MANY_EXPERIENCE_IDS","type":"USER",
      "message":"All push notification messages in the same request must be for the same project; check the details field to investigate conflicting tokens.",
      "details":{"@servicestorm/armada-mobile":["ExponentPushToken[EXAMPLEtokenOldOne0001]","ExponentPushToken[EXAMPLEtokenOldTwo0002]","ExponentPushToken[EXAMPLEtokenOldThree03]"],
                 "@servicestorm/threadknot-mobile":["ExponentPushToken[EXAMPLEtokenCurrent001]"]},
      "isTransient":false,"requestId":"00000000-0000-4000-8000-000000000000"}]}"#;

    #[test]
    fn a_project_conflict_is_split_into_one_group_per_project() {
        let body: Value = serde_json::from_str(CONFLICT).unwrap();
        let groups = experience_groups(&body).expect("two projects must partition");
        assert_eq!(groups.len(), 2);
        let sizes = {
            let mut s: Vec<usize> = groups.iter().map(Vec::len).collect();
            s.sort_unstable();
            s
        };
        assert_eq!(sizes, vec![1, 3], "three old-project tokens and one current");
        assert!(
            groups
                .iter()
                .flatten()
                .all(|t| t.starts_with("ExponentPushToken[")),
            "groups carry push tokens, which is what messages are matched on"
        );
    }

    /// Only a genuine partition may be retried. A single group means resending
    /// the same set, which would spin.
    #[test]
    fn a_single_group_is_not_treated_as_a_partition() {
        let body: Value = serde_json::json!({
            "errors": [{
                "code": "PUSH_TOO_MANY_EXPERIENCE_IDS",
                "details": { "@servicestorm/threadknot-mobile": ["ExponentPushToken[only]"] }
            }]
        });
        assert!(experience_groups(&body).is_none());
    }

    #[test]
    fn unrelated_rejections_are_not_mistaken_for_a_project_conflict() {
        for body in [
            serde_json::json!({"errors": [{"code": "PUSH_TOO_MANY_NOTIFICATIONS"}]}),
            serde_json::json!({"errors": []}),
            serde_json::json!({"data": [{"status": "ok", "id": "x"}]}),
            serde_json::json!({}),
        ] {
            assert!(
                experience_groups(&body).is_none(),
                "must not split on {body}"
            );
        }
    }

    /// The partition Expo returns has to be usable to filter the messages we
    /// already built, which means matching on the `to` field.
    #[test]
    fn groups_select_the_messages_they_describe() {
        let body: Value = serde_json::from_str(CONFLICT).unwrap();
        let groups = experience_groups(&body).unwrap();
        let messages: Vec<Value> = [
            "ExponentPushToken[EXAMPLEtokenOldOne0001]",
            "ExponentPushToken[EXAMPLEtokenCurrent001]",
            "ExponentPushToken[EXAMPLEtokenOldThree03]",
        ]
        .iter()
        .map(|t| serde_json::json!({ "to": t, "title": "x", "body": "y" }))
        .collect();

        let mut selected = 0;
        for tokens in &groups {
            let subset: Vec<&Value> = messages
                .iter()
                .filter(|m| {
                    m.get("to")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| tokens.iter().any(|k| k == t))
                })
                .collect();
            selected += subset.len();
        }
        assert_eq!(
            selected,
            messages.len(),
            "every message must land in exactly one project's request"
        );
    }
}

#[cfg(test)]
mod dedupe_tests {
    use super::{distinct_tokens, token_preview_preferences};

    #[test]
    fn a_phone_paired_more_than_once_is_notified_once() {
        // Exactly the state found on this machine: six device rows, four tokens,
        // two of them repeated because the phone and iPad had re-paired.
        let rows = [
            Some("ExponentPushToken[A]"),
            Some("ExponentPushToken[B]"),
            Some("ExponentPushToken[A]"),
            Some("ExponentPushToken[B]"),
            Some("ExponentPushToken[C]"),
            Some("ExponentPushToken[D]"),
        ];
        assert_eq!(
            distinct_tokens(rows),
            vec![
                "ExponentPushToken[A]",
                "ExponentPushToken[B]",
                "ExponentPushToken[C]",
                "ExponentPushToken[D]"
            ],
            "first-seen order, one message per phone"
        );
    }

    #[test]
    fn duplicate_rows_disable_previews_if_either_row_opts_out() {
        let prefs = token_preview_preferences([
            (Some("ExponentPushToken[A]"), true),
            (Some("ExponentPushToken[A]"), false),
            (Some("ExponentPushToken[B]"), true),
            (None, false),
        ]);
        assert_eq!(prefs.get("ExponentPushToken[A]"), Some(&false));
        assert_eq!(prefs.get("ExponentPushToken[B]"), Some(&true));
        assert_eq!(prefs.len(), 2);
    }

    #[test]
    fn devices_without_a_token_are_skipped_rather_than_sent_empty() {
        assert_eq!(
            distinct_tokens([None, Some("ExponentPushToken[A]"), None]),
            vec!["ExponentPushToken[A]"]
        );
        assert!(distinct_tokens([None, None]).is_empty());
    }
}

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
    Error,
    Test,
}

impl PushKind {
    fn label(self) -> &'static str {
        match self {
            PushKind::TurnCompleted => "Turn complete",
            PushKind::ApprovalRequest => "Approval needed",
            PushKind::QuestionRequest => "Question waiting",
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
        let data = json!({
            "version": 1,
            "serverId": server_id,
            "projectId": job.project_id,
            "threadId": job.thread_id,
            "eventKind": job.kind.event_kind(),
        });

        let messages: Vec<Value> = targets
            .iter()
            .filter_map(|d| d.expo_push_token.as_ref())
            .map(|token| {
                json!({
                    "to": token,
                    "title": title,
                    "body": body,
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

/// POST one batch, retrying transient failures (429 / 5xx / network) with
/// exponential backoff. Per-ticket errors are terminal for that message.
async fn send_batch(
    client: &reqwest::Client,
    mobile: &MobileStore,
    receipts: &Mutex<Vec<PendingReceipt>>,
    messages: &[Value],
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
                tracing::error!("expo push rejected: HTTP {}", resp.status());
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

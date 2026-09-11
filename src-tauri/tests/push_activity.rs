//! Exercise the real push worker against a local gateway; never contact Expo.
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use threadknot_lib::{
    mobile::{default_capabilities, MobileStore},
    protocol::EventNotice,
    push::{PushJob, PushKind, PushService},
};

fn job(seq: u64) -> PushJob {
    PushJob {
        kind: PushKind::TurnCompleted,
        seq: Some(seq),
        project_id: "project".into(),
        workspace_id: "workspace".into(),
        project_name: "Project".into(),
        thread_id: "thread".into(),
        thread_title: "Chat".into(),
        notice: Some(EventNotice {
            title: "Finished".into(),
            body: format!("Private result {seq}"),
        }),
        only_device: None,
    }
}

#[tokio::test]
async fn real_worker_holds_reads_coalesces_and_rechecks_phone_preferences() {
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let router = axum::Router::new().route(
        "/send",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let sent = sent.clone();
            async move {
                sent.send(body).unwrap();
                axum::Json(json!({"data": [{"status": "ok"}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::env::set_var(
        "THREADKNOT_PUSH_SEND_URL",
        format!("http://{}/send", listener.local_addr().unwrap()),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let dir = std::env::temp_dir().join(format!("threadknot-push-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mobile = Arc::new(MobileStore::open(&dir).unwrap());
    let (phone, _) = mobile
        .pair("Phone".into(), "ios".into(), default_capabilities())
        .unwrap();
    mobile
        .update(&phone.id, |d| {
            d.expo_push_token = Some("ExponentPushToken[fixture]".into())
        })
        .unwrap();
    let push = PushService::spawn(mobile.clone(), "server".into());
    let activity = |ms| {
        push.presence
            .lock()
            .unwrap()
            .report("owner", "desktop", ms, Instant::now())
    };
    // A test push is a FIFO barrier proving earlier jobs reached the worker.
    let barrier = || {
        let mut test = job(0);
        test.kind = PushKind::Test;
        test.only_device = Some(phone.id.clone());
        push.enqueue(test);
    };
    activity(30_000);
    push.enqueue(job(1));
    push.presence
        .lock()
        .unwrap()
        .acknowledge("owner", "thread", 1, Instant::now());
    barrier();
    let test = receive(&mut received).await;
    assert_eq!(test[0]["data"]["eventKind"], "test");
    activity(0);
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), received.recv())
            .await
            .is_err(),
        "reading must cancel held delivery"
    );

    activity(30_000);
    push.enqueue(job(2));
    push.enqueue(job(3));
    barrier();
    receive(&mut received).await;
    activity(0);
    let catchup = receive(&mut received).await;
    assert_eq!(catchup[0]["body"], "Private result 3");
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), received.recv())
            .await
            .is_err(),
        "one catch-up per thread"
    );

    activity(30_000);
    push.enqueue(job(4));
    barrier();
    receive(&mut received).await;
    mobile
        .update(&phone.id, |d| d.notification_previews = false)
        .unwrap();
    activity(0);
    let private = receive(&mut received).await;
    assert_eq!(
        private[0]["body"], "Turn complete — Chat",
        "apply current preview choice at delivery"
    );

    activity(30_000);
    push.enqueue(job(5));
    barrier();
    receive(&mut received).await;
    mobile
        .update(&phone.id, |d| d.notifications_enabled = false)
        .unwrap();
    activity(0);
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), received.recv())
            .await
            .is_err(),
        "muting while held prevents delivery"
    );
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

async fn receive(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

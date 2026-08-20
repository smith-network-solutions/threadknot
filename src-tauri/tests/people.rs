//! Several people sharing one machine, driven against the REAL handlers.
//!
//! The feature is a convenience layer, so the thing worth regression-testing is
//! not that it works but that it did not change anything underneath it. The
//! store this binary boots against is written by hand, in the shape a build
//! from before people existed would have left it — a stashed workspace, a
//! starred workspace, a thread on the settled shelf, no `author` anywhere — and
//! the first tests assert that an owner still sees exactly that.
//!
//! One data dir and one server for the whole binary. Each test works on its own
//! subjects (its own workspace, its own thread, its own person), so they can
//! share the harness without racing each other's assertions.

use std::sync::OnceLock;

use serde_json::{json, Value};
use threadknot_lib::mobile::{Capability, DeviceGrant, Principal};
use threadknot_lib::people::OWNER_ID;
use threadknot_lib::protocol::ClientRequest;
use threadknot_lib::server::{self, ServerState};

// ---------------------------------------------------------------- harness ---

/// Ids baked into the legacy store below, so the compatibility tests can name
/// exactly the records that build would have written.
const LEGACY_PROJECT: &str = "legacy-project";
const LEGACY_STASHED_WS: &str = "legacy-stashed";
const LEGACY_STARRED_WS: &str = "legacy-starred";
const LEGACY_PLAIN_WS: &str = "legacy-plain";
const LEGACY_SETTLED_THREAD: &str = "legacy-settled-thread";
const LEGACY_SETTLED_AT: &str = "2026-07-01T10:00:00Z";

struct Harness {
    state: ServerState,
    /// The dir the legacy `projects.json` was written into.
    dir: std::path::PathBuf,
}

/// `projects.json` exactly as a pre-people build would have flushed it: no
/// `author` on the thread, no `people.json` beside it, and the sidebar
/// preferences living on the records themselves.
fn legacy_store_json() -> String {
    let settings = json!({
        "model": "sonnet",
        "wideContext": false,
        "claudeChrome": false,
        "access": "read",
        "mode": "build",
    });
    let workspace = |id: &str, name: &str, extra: Value| {
        let mut ws = json!({
            "id": id,
            "name": name,
            "createdAt": "2026-06-01T00:00:00Z",
            "updatedAt": "2026-06-01T00:00:00Z",
            "members": [{ "machineId": "legacy-machine", "projectId": LEGACY_PROJECT }],
        });
        for (k, v) in extra.as_object().unwrap() {
            ws[k] = v.clone();
        }
        ws
    };
    serde_json::to_string_pretty(&json!({
        "projects": [{
            "id": LEGACY_PROJECT,
            "name": "Legacy",
            "path": "/tmp/threadknot-people-legacy",
            "createdAt": "2026-06-01T00:00:00Z",
        }],
        "threads": [
            {
                "id": LEGACY_SETTLED_THREAD,
                "projectId": LEGACY_PROJECT,
                "machineId": "legacy-machine",
                "agent": "claude",
                "title": "Filed away last month",
                "settings": settings,
                "status": "idle",
                "settledAt": LEGACY_SETTLED_AT,
                "favorite": true,
                "createdAt": "2026-06-01T00:00:00Z",
                "updatedAt": "2026-06-02T00:00:00Z",
            },
            {
                "id": "legacy-active-thread",
                "projectId": LEGACY_PROJECT,
                "machineId": "legacy-machine",
                "agent": "claude",
                "title": "Still open",
                "settings": settings,
                "status": "idle",
                "createdAt": "2026-06-01T00:00:00Z",
                "updatedAt": "2026-06-03T00:00:00Z",
            },
        ],
        "workspaces": [
            workspace(LEGACY_STASHED_WS, "Stashed", json!({ "hidden": true })),
            workspace(LEGACY_STARRED_WS, "Starred", json!({ "favorite": true })),
            workspace(LEGACY_PLAIN_WS, "Plain", json!({})),
        ],
    }))
    .unwrap()
}

fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("threadknot-people-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Written BEFORE the store opens: the point is to boot onto an existing
        // install rather than to build one through the new code paths.
        std::fs::write(dir.join("projects.json"), legacy_store_json()).unwrap();
        std::env::set_var("THREADKNOT_DATA_DIR", &dir);
        std::env::set_var("THREADKNOT_PORT", "0");

        let (state, _) = threadknot_lib::build_server_state().expect("build state");
        Harness { state, dir }
    })
}

async fn call(
    principal: &Principal,
    kind: &str,
    payload: Value,
) -> Result<Value, String> {
    let req = ClientRequest {
        id: 1,
        kind: kind.into(),
        payload,
        mesh: None,
    };
    server::handle_request(&harness().state, principal, req)
        .await
        .map_err(|e| format!("{e:#}"))
}

async fn ok(principal: &Principal, kind: &str, payload: Value) -> Value {
    call(principal, kind, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind} failed: {e}"))
}

/// Register a real person and a real paired device that speaks for them, so
/// `acting_person` resolves through the same lookup production uses instead of
/// through a hand-built principal.
async fn person_with_device(name: &str) -> (String, Principal) {
    let h = harness();
    let person = ok(&Principal::Master, "person.create", json!({ "name": name })).await;
    let person_id = person["id"].as_str().unwrap().to_string();
    let (device, _credential) = h
        .state
        .mobile
        .pair(
            format!("{name}'s browser"),
            "web".into(),
            Capability::ALL.to_vec(),
        )
        .unwrap();
    ok(
        &Principal::Master,
        "device.setPerson",
        json!({ "deviceId": device.id, "personId": person_id }),
    )
    .await;
    let principal = Principal::Device(DeviceGrant {
        id: device.id,
        capabilities: Capability::ALL.to_vec(),
    });
    (person_id, principal)
}

/// A workspace of this test's own, injected through the store so the shared
/// legacy fixtures stay untouched by whichever test runs first.
fn scratch_workspace(name: &str) -> String {
    let id = format!("scratch-{}", uuid::Uuid::new_v4());
    harness()
        .state
        .hub
        .store
        .upsert_workspace_replica(threadknot_lib::protocol::Workspace {
            id: id.clone(),
            name: name.into(),
            image: None,
            created_at: "2026-08-01T00:00:00Z".into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            favorite: None,
            hidden: None,
            members: vec![],
        })
        .unwrap();
    id
}

/// One workspace out of `workspace.list`, as this principal sees it.
async fn workspace_seen_by(principal: &Principal, workspace_id: &str) -> Value {
    let list = ok(principal, "workspace.list", json!({})).await;
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == workspace_id)
        .unwrap_or_else(|| panic!("{workspace_id} missing from workspace.list"))
        .clone()
}

async fn thread_seen_by(principal: &Principal, thread_id: &str) -> Value {
    let list = ok(
        principal,
        "thread.list",
        json!({ "projectId": LEGACY_PROJECT }),
    )
    .await;
    list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == thread_id)
        .unwrap_or_else(|| panic!("{thread_id} missing from thread.list"))
        .clone()
}

/// The workspace as it sits on DISK, which is what the mesh replicates and what
/// an older build would read back.
fn stored_workspace(id: &str) -> threadknot_lib::protocol::Workspace {
    harness().state.hub.store.workspace(id).expect("workspace")
}

fn stored_thread(id: &str) -> threadknot_lib::protocol::Thread {
    harness().state.hub.store.thread(id).expect("thread")
}

// ------------------------------------------------- backwards compatibility ---

#[tokio::test]
async fn an_existing_store_loads_with_its_preferences_intact() {
    // Nothing here goes through the overlay: this is the pre-people install,
    // read by the post-people build, seen by the owner.
    let stashed = workspace_seen_by(&Principal::Master, LEGACY_STASHED_WS).await;
    assert_eq!(stashed["hidden"], json!(true), "a stashed workspace stayed stashed");

    let starred = workspace_seen_by(&Principal::Master, LEGACY_STARRED_WS).await;
    assert_eq!(starred["favorite"], json!(true));
    assert!(starred.get("hidden").is_none(), "unset stays unset, not false");

    let plain = workspace_seen_by(&Principal::Master, LEGACY_PLAIN_WS).await;
    assert!(plain.get("hidden").is_none());
    assert!(plain.get("favorite").is_none());

    let settled = thread_seen_by(&Principal::Master, LEGACY_SETTLED_THREAD).await;
    assert_eq!(settled["settledAt"], json!(LEGACY_SETTLED_AT));
    assert_eq!(settled["favorite"], json!(true));
}

#[tokio::test]
async fn legacy_threads_carry_no_author_and_read_as_the_owners() {
    let settled = thread_seen_by(&Principal::Master, LEGACY_SETTLED_THREAD).await;
    assert!(
        settled.get("author").is_none(),
        "a pre-people thread must not gain an author on load"
    );
    // And the file on disk is not rewritten with one either.
    assert_eq!(stored_thread(LEGACY_SETTLED_THREAD).author, None);
}

#[tokio::test]
async fn the_owner_is_seeded_and_is_who_a_master_credential_acts_as() {
    let listed = ok(&Principal::Master, "person.list", json!({})).await;
    assert_eq!(listed["acting"], json!(OWNER_ID));
    let people = listed["people"].as_array().unwrap();
    assert!(people.iter().any(|p| p["id"] == json!(OWNER_ID)));

    let hello = ok(&Principal::Master, "hello", json!({})).await;
    assert_eq!(hello["person"], json!(OWNER_ID));
}

#[tokio::test]
async fn a_device_with_no_person_still_acts_as_the_owner() {
    // Every device paired before this feature is exactly this: a credential
    // with no assignment. It must keep seeing the owner's sidebar.
    let h = harness();
    let (device, _) = h
        .state
        .mobile
        .pair("Old phone".into(), "ios".into(), Capability::ALL.to_vec())
        .unwrap();
    let principal = Principal::Device(DeviceGrant {
        id: device.id,
        capabilities: Capability::ALL.to_vec(),
    });

    let hello = ok(&principal, "hello", json!({})).await;
    assert_eq!(hello["person"], json!(OWNER_ID));
    let stashed = workspace_seen_by(&principal, LEGACY_STASHED_WS).await;
    assert_eq!(stashed["hidden"], json!(true));
}

#[tokio::test]
async fn an_owner_toggle_still_writes_the_record_the_mesh_replicates() {
    // The overlay must not become the only place a preference lives, or a
    // paired machine would stop hearing about it.
    let ws_id = scratch_workspace("Owner toggle subject");
    ok(
        &Principal::Master,
        "workspace.setHidden",
        json!({ "workspaceId": ws_id, "hidden": true }),
    )
    .await;
    assert_eq!(stored_workspace(&ws_id).hidden, Some(true));

    ok(
        &Principal::Master,
        "workspace.setHidden",
        json!({ "workspaceId": ws_id, "hidden": false }),
    )
    .await;
    assert_eq!(stored_workspace(&ws_id).hidden, None);
}

// ------------------------------------------------------- several people ---

#[tokio::test]
async fn a_teammates_toggle_never_touches_the_record_or_the_owners_view() {
    let (_id, intern) = person_with_device("Stash Intern").await;

    // Their own workspace to work on, so the assertion is about isolation
    // rather than about which test ran first.
    let ws_id = scratch_workspace("Intern stash subject");

    ok(
        &intern,
        "workspace.setHidden",
        json!({ "workspaceId": ws_id, "hidden": true }),
    )
    .await;

    // Their sidebar changed.
    assert_eq!(
        workspace_seen_by(&intern, &ws_id).await["hidden"],
        json!(true)
    );
    // The owner's did not.
    assert!(workspace_seen_by(&Principal::Master, &ws_id)
        .await
        .get("hidden")
        .is_none());
    // And neither did the record the mesh would replicate.
    assert_eq!(stored_workspace(&ws_id).hidden, None);
}

#[tokio::test]
async fn a_new_person_inherits_the_sidebar_and_can_then_disagree_with_it() {
    let (_id, intern) = person_with_device("Inherit Intern").await;

    // Nothing was migrated for them, so they fall through to the stored flag:
    // the workspace the owner stashed months ago is stashed for them too.
    assert_eq!(
        workspace_seen_by(&intern, LEGACY_STASHED_WS).await["hidden"],
        json!(true)
    );

    // Pulling it back is an opinion, stored as an explicit false so the
    // fallback cannot hand them the owner's `true` again on the next read.
    ok(
        &intern,
        "workspace.setHidden",
        json!({ "workspaceId": LEGACY_STASHED_WS, "hidden": false }),
    )
    .await;
    assert!(workspace_seen_by(&intern, LEGACY_STASHED_WS)
        .await
        .get("hidden")
        .is_none());
    // The owner still has it stashed, on the record and in their view.
    assert_eq!(
        workspace_seen_by(&Principal::Master, LEGACY_STASHED_WS).await["hidden"],
        json!(true)
    );
    assert_eq!(stored_workspace(LEGACY_STASHED_WS).hidden, Some(true));
}

#[tokio::test]
async fn a_thread_is_stamped_with_whoever_started_it() {
    let (intern_id, intern) = person_with_device("Author Intern").await;

    let theirs = ok(
        &intern,
        "thread.create",
        json!({
            "projectId": LEGACY_PROJECT,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
        }),
    )
    .await;
    assert_eq!(theirs["author"], json!(intern_id));
    assert_eq!(
        stored_thread(theirs["id"].as_str().unwrap()).author.as_deref(),
        Some(intern_id.as_str())
    );

    // The owner stamps nothing, so a single-person install keeps writing the
    // record it always wrote.
    let mine = ok(
        &Principal::Master,
        "thread.create",
        json!({
            "projectId": LEGACY_PROJECT,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
        }),
    )
    .await;
    assert!(mine.get("author").is_none());
    assert_eq!(stored_thread(mine["id"].as_str().unwrap()).author, None);
}

#[tokio::test]
async fn shelving_a_thread_is_per_person() {
    let (_id, intern) = person_with_device("Shelf Intern").await;

    let thread = ok(
        &Principal::Master,
        "thread.create",
        json!({
            "projectId": LEGACY_PROJECT,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
        }),
    )
    .await;
    let thread_id = thread["id"].as_str().unwrap().to_string();

    ok(
        &intern,
        "thread.setSettled",
        json!({ "threadId": thread_id, "settled": true }),
    )
    .await;

    assert!(thread_seen_by(&intern, &thread_id).await["settledAt"].is_string());
    assert!(thread_seen_by(&Principal::Master, &thread_id)
        .await
        .get("settledAt")
        .is_none());
    assert_eq!(stored_thread(&thread_id).settled_at, None);
}

#[tokio::test]
async fn new_activity_unparks_the_thread_for_everyone_who_filed_it() {
    let (_id, intern) = person_with_device("Unpark Intern").await;
    let h = harness();

    let thread = ok(
        &Principal::Master,
        "thread.create",
        json!({
            "projectId": LEGACY_PROJECT,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
        }),
    )
    .await;
    let thread_id = thread["id"].as_str().unwrap().to_string();
    ok(
        &intern,
        "thread.setSettled",
        json!({ "threadId": thread_id, "settled": true }),
    )
    .await;
    assert!(thread_seen_by(&intern, &thread_id).await["settledAt"].is_string());

    // The hub clears the record's fields on new activity; the overlay has to
    // go with it or the news lands in a shelf they have collapsed.
    h.state.hub.people.unpark_thread(&thread_id);

    assert!(thread_seen_by(&intern, &thread_id)
        .await
        .get("settledAt")
        .is_none());
}

#[tokio::test]
async fn starring_a_thread_is_per_person() {
    let (_id, intern) = person_with_device("Star Intern").await;

    // The legacy thread is starred on the record. The intern un-stars it for
    // themselves; the owner keeps their star and so does the file.
    ok(
        &intern,
        "thread.setFavorite",
        json!({ "threadId": LEGACY_SETTLED_THREAD, "favorite": false }),
    )
    .await;

    assert!(thread_seen_by(&intern, LEGACY_SETTLED_THREAD)
        .await
        .get("favorite")
        .is_none());
    assert_eq!(
        thread_seen_by(&Principal::Master, LEGACY_SETTLED_THREAD).await["favorite"],
        json!(true)
    );
    assert_eq!(stored_thread(LEGACY_SETTLED_THREAD).favorite, Some(true));
}

// ------------------------------------------------------------ management ---

#[tokio::test]
async fn managing_people_needs_the_master_token() {
    let device = Principal::Device(DeviceGrant {
        id: "unassigned-device".into(),
        capabilities: Capability::ALL.to_vec(),
    });

    // Reading the roster is open — the sidebar needs names to label with.
    ok(&device, "person.list", json!({})).await;

    for (kind, payload) in [
        ("person.create", json!({ "name": "Sneaky" })),
        ("person.update", json!({ "personId": OWNER_ID, "name": "Renamed" })),
        ("person.delete", json!({ "personId": "whoever" })),
        (
            "person.setClaudeLogin",
            json!({ "personId": OWNER_ID, "isolated": true }),
        ),
        (
            "device.setPerson",
            json!({ "deviceId": "unassigned-device", "personId": OWNER_ID }),
        ),
    ] {
        let err = call(&device, kind, payload)
            .await
            .expect_err(&format!("{kind} was allowed from a device credential"));
        assert!(
            err.contains("master token"),
            "{kind} failed for the wrong reason: {err}"
        );
    }
}

#[tokio::test]
async fn the_owner_record_cannot_be_deleted() {
    let err = call(
        &Principal::Master,
        "person.delete",
        json!({ "personId": OWNER_ID }),
    )
    .await
    .expect_err("the owner was deletable");
    assert!(err.contains("owner cannot be removed"), "{err}");
}

#[tokio::test]
async fn deleting_a_person_hands_their_devices_back_to_the_owner() {
    let (person_id, principal) = person_with_device("Leaving Soon").await;

    // Something of theirs, so we can check the stamp outlives them.
    let thread = ok(
        &principal,
        "thread.create",
        json!({
            "projectId": LEGACY_PROJECT,
            "agent": "claude",
            "settings": { "model": "sonnet", "access": "read", "mode": "build" },
        }),
    )
    .await;
    let thread_id = thread["id"].as_str().unwrap().to_string();

    let result = ok(
        &Principal::Master,
        "person.delete",
        json!({ "personId": person_id }),
    )
    .await;
    assert_eq!(result["releasedDevices"], json!(1));

    // The credential still works and now speaks for the owner rather than for
    // a record that no longer exists.
    let hello = ok(&principal, "hello", json!({})).await;
    assert_eq!(hello["person"], json!(OWNER_ID));

    // The thread keeps the stamp: an id with no record is "someone who left",
    // which is more honest than silently becoming the owner's work.
    assert_eq!(
        stored_thread(&thread_id).author.as_deref(),
        Some(person_id.as_str())
    );
}

#[tokio::test]
async fn a_person_can_be_given_their_own_claude_login() {
    let (person_id, _principal) = person_with_device("Own Login").await;

    let result = ok(
        &Principal::Master,
        "person.setClaudeLogin",
        json!({ "personId": person_id, "isolated": true }),
    )
    .await;
    let dir = result["person"]["claudeConfigDir"].as_str().unwrap();
    assert!(std::path::Path::new(dir).is_dir());
    assert!(dir.starts_with(harness().dir.to_str().unwrap()));
    assert!(result["loginCommand"]
        .as_str()
        .unwrap()
        .starts_with("CLAUDE_CONFIG_DIR="));

    // Reversible, and reverting clears the field rather than leaving the
    // driver pointed at a config dir with no login in it.
    let back = ok(
        &Principal::Master,
        "person.setClaudeLogin",
        json!({ "personId": person_id, "isolated": false }),
    )
    .await;
    assert!(back["person"]["claudeConfigDir"].is_null());
    assert!(back["loginCommand"].is_null());
}

#[tokio::test]
async fn assigning_a_device_to_an_unknown_person_is_refused() {
    let h = harness();
    let (device, _) = h
        .state
        .mobile
        .pair("Spare".into(), "web".into(), Capability::ALL.to_vec())
        .unwrap();
    let err = call(
        &Principal::Master,
        "device.setPerson",
        json!({ "deviceId": device.id, "personId": "no-such-person" }),
    )
    .await
    .expect_err("an unknown person was accepted");
    assert!(err.contains("unknown person"), "{err}");
}

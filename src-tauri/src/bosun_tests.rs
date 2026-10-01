use super::*;
use std::fs::File;

fn tempdir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("threadknot-bosun-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn minimal_bosun() -> Bosun {
    bosun_from_create(&json!({ "name": "Blackbeard", "homeWorkspaceId": "home" }), None).unwrap()
}

fn signal(id: &str) -> Signal {
    Signal {
        id: id.into(),
        lookout_id: "l1".into(),
        bosun_id: "b1".into(),
        kind: "ticket".into(),
        observed_at: now_iso(),
        title: format!("title {id}"),
        body: "body".into(),
        refs: BTreeMap::new(),
        hints: SignalHints::default(),
        items: Vec::new(),
    }
}

fn item(signal_id: &str, decision: &str, workspace: Option<&str>) -> TriageItem {
    TriageItem {
        signal_id: signal_id.into(),
        decision: Some(decision.into()),
        workspace_id: workspace.map(str::to_string),
        title: Some("Do it".into()),
        reason: Some("because".into()),
        ..Default::default()
    }
}

#[test]
fn a_minimal_create_payload_fills_every_default() {
    let b = minimal_bosun();
    assert!(b.enabled);
    assert!(!b.id.is_empty());
    assert_eq!(b.triage.model, "haiku");
    assert_eq!(b.triage.agent, Agent::Claude);
    assert_eq!(b.work.agent, Agent::Claude);
    assert_eq!(b.work.settings.access, Access::Edits);
    assert_eq!(b.work.settings.mode, Mode::Build);
    assert_eq!(b.budget.max_turns_per_hour, 6);
    assert_eq!(b.budget.max_concurrent, 2);
    assert!(b.quiet_hours.is_none() && b.lookouts.is_empty() && b.charters.is_empty());
    assert!(b.author.is_none());

    // Lookout and charter defaults come from serde too.
    let b = bosun_from_create(
        &json!({
            "name": "B", "homeWorkspaceId": "home",
            "lookouts": [{ "kind": { "type": "folder", "path": "/tmp", "pattern": "*.md" } }],
            "charters": [{ "workspaceId": "w1" }],
        }),
        Some("alice".into()),
    )
    .unwrap();
    assert_eq!(b.author.as_deref(), Some("alice"));
    let l = &b.lookouts[0];
    assert!(l.enabled);
    assert_eq!(l.interval_secs, 300);
    assert!(matches!(l.kind, LookoutKind::Folder { max_age_days: 3, .. }));
    let c = &b.charters[0];
    assert!(c.log_thread && c.allow_work && c.access.is_none() && c.member.is_none());
}

#[test]
fn normalize_mints_lookout_ids_and_webhook_secrets() {
    let root = tempdir("normalize");
    let project_dir = root.join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    let store = Store::open_for_test(root.join("data")).unwrap();
    store.migrate_mesh("m1").unwrap();
    let project = store.create_project(project_dir.to_string_lossy().into_owned(), None).unwrap();
    let ws = store.workspace_for_project(&project.id).unwrap();
    let mut b = bosun_from_create(
        &json!({
            "name": "  B  ", "homeWorkspaceId": ws,
            "lookouts": [
                { "kind": { "type": "webhook" }, "intervalSecs": 1 },
                { "kind": { "type": "timer", "cadence": { "type": "daily", "time": "09:00" } } },
            ],
        }),
        None,
    )
    .unwrap();
    normalize(&store, &mut b).unwrap();
    assert_eq!(b.name, "B");
    assert!(b.lookouts.iter().all(|l| !l.id.is_empty()));
    assert_eq!(b.lookouts[0].interval_secs, MIN_INTERVAL_SECS);
    assert!(matches!(&b.lookouts[0].kind, LookoutKind::Webhook { secret } if secret.len() == 32));
    assert!(matches!(&b.lookouts[1].kind, LookoutKind::Timer { next_run_at: Some(_), .. }));

    b.home_workspace_id = "nope".into();
    assert!(normalize(&store, &mut b).is_err(), "home workspace must exist");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn an_edit_keeps_what_a_lookout_learned() {
    let mut old = minimal_bosun();
    old.lookouts.push(Lookout {
        id: "l1".into(),
        name: "calls".into(),
        enabled: true,
        kind: LookoutKind::Folder { path: "/tmp".into(), pattern: "*.md".into(), max_age_days: 3 },
        interval_secs: 60,
        watermark: Some("2026-09-28T10:00:00Z".into()),
        last_run_at: Some("2026-09-28T10:00:00Z".into()),
        last_error: None,
        last_signal_count: 4,
    });
    let mut next = old.clone();
    next.lookouts[0].watermark = None;
    next.lookouts[0].last_signal_count = 0;
    next.lookouts[0].name = "renamed".into();
    preserve_runtime(&mut next, &old);
    assert_eq!(next.lookouts[0].watermark.as_deref(), Some("2026-09-28T10:00:00Z"));
    assert_eq!(next.lookouts[0].last_signal_count, 4);
    assert_eq!(next.lookouts[0].name, "renamed");
}

#[test]
fn signals_are_deduped_against_seen_and_pending() {
    let dir = tempdir("dedupe");
    let reg = BosunRegistry::open(&dir).unwrap();
    assert_eq!(reg.enqueue("b1", vec![signal("a"), signal("b"), signal("a")]).len(), 2);
    // Already pending.
    assert_eq!(reg.enqueue("b1", vec![signal("a")]).len(), 0);
    reg.settle("b1", &["a".to_string()]);
    assert_eq!(reg.pending_for("b1", 25).len(), 1);
    // Already seen.
    assert_eq!(reg.enqueue("b1", vec![signal("a")]).len(), 0);
    // Another Bosun has its own memory.
    let mut other = signal("a");
    other.bosun_id = "b2".into();
    assert_eq!(reg.enqueue("b2", vec![other]).len(), 1);

    // Pending and seen survive a restart.
    let reopened = BosunRegistry::open(&dir).unwrap();
    assert_eq!(reopened.pending_for("b1", 25).len(), 1);
    assert_eq!(reopened.enqueue("b1", vec![signal("a")]).len(), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn seen_is_capped_newest_last() {
    let mut seen: Vec<String> = (0..SEEN_CAP).map(|i| i.to_string()).collect();
    remember(&mut seen, &["new".to_string()]);
    assert_eq!(seen.len(), SEEN_CAP);
    assert_eq!(seen.first().map(String::as_str), Some("1"));
    assert_eq!(seen.last().map(String::as_str), Some("new"));
}

#[test]
fn ndjson_parses_signals_watermarks_and_skips_garbage() {
    let out = r#"{"title": "Ticket 1", "body": "b", "kind": "ticket", "id": "orbit:1", "refs": {"url": "https://x"}, "hints": {"workspace": "Service Storm"}}
not json at all
{"watermark": "w1"}
{"nothing": "useful"}
[1, 2]

{"title": "No id", "body": "hello"}
{"watermark": "w2"}
"#;
    let (signals, watermark, bad) = parse_ndjson("b1", "l1", out);
    assert_eq!(signals.len(), 2);
    assert_eq!(watermark.as_deref(), Some("w2"), "the last watermark wins");
    assert_eq!(bad, 3);
    let first = &signals[0];
    assert_eq!(first.id, "orbit:1");
    assert_eq!(first.kind, "ticket");
    assert_eq!(first.refs.get("url").map(String::as_str), Some("https://x"));
    assert_eq!(first.hints.workspace.as_deref(), Some("Service Storm"));
    assert_eq!((first.bosun_id.as_str(), first.lookout_id.as_str()), ("b1", "l1"));
    // Missing id derives sha256(lookoutId + title + body).
    assert_eq!(signals[1].id, sha256_hex(&["l1", "No id", "hello"]));
    assert_eq!(signals[1].id.len(), 64);
}

#[test]
fn bodies_are_capped_at_32_kib_on_a_char_boundary() {
    let body = "é".repeat(BODY_CAP);
    let line = json!({ "title": "big", "body": body }).to_string();
    let (signals, _, _) = parse_ndjson("b", "l", &line);
    assert!(signals[0].body.len() <= BODY_CAP);
    assert!(signals[0].body.len() > BODY_CAP - 2);
}

#[test]
fn quiet_hours_inside_outside_and_across_midnight() {
    let t = |s: &str| NaiveTime::parse_from_str(s, "%H:%M").unwrap();
    let day = QuietHours { start: "09:00".into(), end: "17:00".into() };
    assert!(in_quiet_hours(&day, t("09:00")));
    assert!(in_quiet_hours(&day, t("12:30")));
    assert!(!in_quiet_hours(&day, t("17:00")));
    assert!(!in_quiet_hours(&day, t("08:59")));

    let night = QuietHours { start: "22:00".into(), end: "07:00".into() };
    assert!(in_quiet_hours(&night, t("23:30")));
    assert!(in_quiet_hours(&night, t("00:00")));
    assert!(in_quiet_hours(&night, t("06:59")));
    assert!(!in_quiet_hours(&night, t("07:00")));
    assert!(!in_quiet_hours(&night, t("12:00")));
    assert!(!in_quiet_hours(&night, t("21:59")));

    let broken = QuietHours { start: "late".into(), end: "07:00".into() };
    assert!(!in_quiet_hours(&broken, t("03:00")), "a typo never silences a Bosun");
}

#[test]
fn budget_downgrades_work_and_ask_to_log() {
    let b = minimal_bosun();
    let pending = vec![signal("a"), signal("b"), signal("c"), signal("d")];
    let items = vec![
        item("a", "work", Some("home")),
        item("b", "ignore", None),
        item("c", "ask", Some("home")),
        item("d", "work", Some("home")),
    ];
    let mut planned = validate_decisions(items, &pending, &b);
    assert_eq!(apply_budget(&mut planned, 1), 2);
    assert_eq!(planned[0].decision, DecisionKind::Work);
    assert_eq!(planned[1].decision, DecisionKind::Ignore);
    assert_eq!(planned[2].decision, DecisionKind::Log);
    assert_eq!(planned[2].reason, "budget");
    assert_eq!(planned[3].decision, DecisionKind::Log);
    let mut none_left = planned.clone();
    assert_eq!(apply_budget(&mut none_left, 0), 1);
}

#[test]
fn triage_output_is_validated() {
    let mut b = minimal_bosun();
    b.charters.push(Charter {
        workspace_id: "ro".into(),
        summary: "read only".into(),
        route_hints: vec![],
        standing_orders: String::new(),
        access: None,
        member: None,
        log_thread: true,
        allow_work: false,
    });
    let pending = vec![signal("a"), signal("b"), signal("c"), signal("d")];
    let items = vec![
        item("a", "work", Some("mystery")),
        item("b", "work", Some("ro")),
        item("ghost", "work", Some("home")),
        item("d", "log", Some("ro")),
        // A second answer for the same signal does not override the first.
        item("d", "work", Some("home")),
    ];
    let planned = validate_decisions(items, &pending, &b);
    assert_eq!(planned.len(), 4, "unknown signal ids are dropped");

    assert_eq!(planned[0].decision, DecisionKind::Ask, "unknown workspace -> ask");
    assert_eq!(planned[0].workspace_id.as_deref(), Some("home"), "unknown workspace -> home");
    assert_eq!(planned[1].decision, DecisionKind::Ask, "work on allowWork=false -> ask");
    assert_eq!(planned[1].workspace_id.as_deref(), Some("ro"));
    assert_eq!(planned[2].decision, DecisionKind::Ignore, "missing decision -> ignore");
    assert_eq!(planned[2].reason, "no decision returned");
    assert_eq!(planned[3].decision, DecisionKind::Log);
    assert_eq!(planned[3].workspace_id.as_deref(), Some("ro"));
}

#[test]
fn triage_envelope_parses_structured_output() {
    let envelope = json!({
        "type": "result",
        "structured_output": { "decisions": [
            { "signalId": "a", "decision": "work", "workspaceId": null, "title": "t", "reason": "r",
              "mergeIntoThreadId": "th1" },
            { "garbage": true },
        ]},
    });
    let items = parse_triage_output(&envelope).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].merge_into_thread_id.as_deref(), Some("th1"));
    assert!(parse_triage_output(&json!({ "result": "text" })).is_err());
}

#[test]
fn glob_matches_names() {
    assert!(glob_match("triage.md", "triage.md"));
    assert!(!glob_match("triage.md", "triage.mdx"));
    assert!(glob_match("*.md", "note.md"));
    assert!(glob_match("*", "anything"));
    assert!(glob_match("call-??.txt", "call-07.txt"));
    assert!(!glob_match("call-??.txt", "call-7.txt"));
    assert!(glob_match("a*b*c", "aXXbYYc"));
    assert!(!glob_match("a*b*c", "aXXbYY"));
}

fn touch(path: &Path, text: &str, mtime: SystemTime) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    File::options().write(true).open(path).unwrap().set_modified(mtime).unwrap();
}

#[test]
fn folder_lookout_honours_pattern_age_and_watermark() {
    let root = tempdir("folder");
    let now = SystemTime::now();
    let hour = Duration::from_secs(3600);
    touch(&root.join("call-1/triage.md"), "first call", now - hour * 2);
    touch(&root.join("call-2/triage.md"), "second call", now - hour);
    touch(&root.join("call-2/notes.txt"), "not matched", now);
    touch(&root.join("old/triage.md"), "too old", now - hour * 24 * 10);
    touch(&root.join(".hidden/triage.md"), "dot dir", now);
    touch(&root.join("a/b/c/d/e/f/g/triage.md"), "too deep", now);

    let (signals, newest) = scan_folder("b1", "l1", &root, "triage.md", 3, None, now);
    let titles: Vec<&str> = signals.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["call-1/triage.md", "call-2/triage.md"], "oldest first");
    assert_eq!(signals[1].body, "second call");
    let path = root.join("call-2/triage.md").to_string_lossy().into_owned();
    assert_eq!(signals[1].refs.get("path"), Some(&path));
    assert_eq!(signals[1].id, format!("folder:{path}"));
    assert_eq!(signals[1].kind, "file");

    // Only files after the watermark are new.
    let mark = now - hour - Duration::from_secs(60);
    let (after, _) = scan_folder("b1", "l1", &root, "triage.md", 3, Some(mark), now);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].title, "call-2/triage.md");
    // The newest mtime becomes the next watermark; nothing is newer than it.
    let (none, _) = scan_folder("b1", "l1", &root, "triage.md", 3, newest, now);
    assert!(none.is_empty());
    // max_age_days is what kept the 10-day-old file out above.
    let (wide, _) = scan_folder("b1", "l1", &root, "*.md", 30, None, now);
    assert_eq!(wide.len(), 3, "a wider max age admits the old file");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn day_log_is_reused_for_the_same_date_and_new_the_next() {
    let root = tempdir("daylog");
    let project_dir = root.join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    let store = Store::open_for_test(root.join("data")).unwrap();
    store.migrate_mesh("m1").unwrap();
    let project = store.create_project(project_dir.to_string_lossy().into_owned(), Some("Proj".into())).unwrap();
    let ws = store.workspace_for_project(&project.id).unwrap();
    let mut b = minimal_bosun();
    b.home_workspace_id = ws.clone();
    let today = Local::now().date_naive();

    let (first, created) = ensure_day_log(&store, &project.id, &b, &ws, "Proj", &signal("a"), today).unwrap();
    assert!(created);
    let origin = first.origin.clone().unwrap();
    assert!(origin.day_log);
    assert_eq!(origin.bosun_id, b.id);
    assert_eq!(origin.kind, "bosun");
    assert!(first.title.starts_with("Blackbeard log · Proj · "), "{}", first.title);
    assert_eq!(first.status, ThreadStatus::Idle);

    let (again, created) = ensure_day_log(&store, &project.id, &b, &ws, "Proj", &signal("b"), today).unwrap();
    assert!(!created);
    assert_eq!(again.id, first.id);

    // Another Bosun keeps its own log.
    let mut other = b.clone();
    other.id = "other".into();
    let (theirs, created) = ensure_day_log(&store, &project.id, &other, &ws, "Proj", &signal("c"), today).unwrap();
    assert!(created);
    assert_ne!(theirs.id, first.id);

    let tomorrow = today.succ_opt().unwrap();
    let (next, created) = ensure_day_log(&store, &project.id, &b, &ws, "Proj", &signal("d"), tomorrow).unwrap();
    assert!(created);
    assert_ne!(next.id, first.id);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn hail_auth_accepts_the_secret_and_the_master_only_where_allowed() {
    assert!(!hail_authorized(None, "s3cret", "master", true), "no bearer -> 401");
    assert!(!hail_authorized(Some(""), "", "", true));
    assert!(!hail_authorized(Some("wrong"), "s3cret", "master", true));
    assert!(hail_authorized(Some("s3cret"), "s3cret", "master", true));
    assert!(hail_authorized(Some("s3cret"), "s3cret", "master", false));
    assert!(hail_authorized(Some("master"), "s3cret", "master", true));
    assert!(!hail_authorized(Some("master"), "s3cret", "master", false), "the relay ingress refuses the master token");
    assert!(!hail_authorized(Some("s3cre"), "s3cret", "master", true));
}

#[test]
fn hail_bodies_become_signals() {
    let titled = json!({ "title": "Invoice failed", "body": "Acme", "refs": { "url": "u", "n": 3 } });
    let s = hail_signal("b1", "l1", titled.as_object().unwrap());
    assert_eq!(s.title, "Invoice failed");
    assert_eq!(s.kind, "webhook");
    assert_eq!(s.body, "Acme");
    assert_eq!(s.refs.get("n").map(String::as_str), Some("3"));
    assert_eq!(s.id, sha256_hex(&["l1", &titled.to_string()]));

    let raw = json!({ "event": "push", "repo": "x" });
    let s = hail_signal("b1", "l1", raw.as_object().unwrap());
    assert_eq!(s.title, "Webhook");
    assert_eq!(s.body, raw.to_string());
    assert_eq!(s.id, sha256_hex(&["l1", &raw.to_string()]));
}

#[test]
fn terminal_is_needed_for_commands_and_remote_members() {
    assert!(payload_needs_terminal(
        &json!({ "lookouts": [{ "kind": { "type": "command", "command": "x" } }] }),
        "me"
    ));
    assert!(!payload_needs_terminal(
        &json!({ "lookouts": [{ "kind": { "type": "folder", "path": "/", "pattern": "*" } }] }),
        "me"
    ));
    assert!(payload_needs_terminal(
        &json!({ "charters": [{ "workspaceId": "w", "member": { "machineId": "other", "projectId": "p" } }] }),
        "me"
    ));
    assert!(!payload_needs_terminal(
        &json!({ "charters": [{ "workspaceId": "w", "member": { "machineId": "me", "projectId": "p" } }] }),
        "me"
    ));
}

#[test]
fn work_prompt_carries_orders_signal_and_brief() {
    let mut s = signal("a");
    s.refs.insert("url".into(), "https://t/42".into());
    let p = work_prompt("Summarise and stop.", "Blackbeard", "Spencer", &s, "Calls", "look at it", false, "");
    assert!(p.starts_with("Summarise and stop.\n\n---\nYou were woken by Blackbeard"));
    assert!(p.contains("a decision from Spencer"));
    assert!(p.contains("## Signal\nkind: ticket"));
    assert!(p.contains("source: Calls"));
    assert!(p.contains("refs: url=https://t/42"));
    assert!(p.contains("## Triage brief\nlook at it"));
    assert!(!p.contains("Triage chose ASK"));
    let ask = work_prompt("", "B", "Spencer", &s, "Calls", "", true, "");
    assert!(ask.contains("Triage chose ASK"));
}

#[test]
fn a_merge_target_in_another_workspace_is_refused() {
    let root = tempdir("merge");
    let store = Store::open_for_test(root.join("data")).unwrap();
    store.migrate_mesh("m1").unwrap();
    let mut projects = Vec::new();
    for name in ["A", "B"] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        projects.push(store.create_project(dir.to_string_lossy().into_owned(), Some(name.into())).unwrap());
    }
    let ws_a = store.workspace_for_project(&projects[0].id).unwrap();
    let ws_b = store.workspace_for_project(&projects[1].id).unwrap();
    let mut b = minimal_bosun();
    b.home_workspace_id = ws_a.clone();

    // A thread this Bosun opened in workspace A, idle, not a day log.
    let thread = store
        .create_thread(projects[0].id.clone(), b.work.agent, b.work.settings.clone(), None)
        .unwrap();
    let origin = origin_for(&b, &signal("s1"), false);
    store.update_thread(&thread.id, |t| t.origin = Some(origin)).unwrap();

    let planned = |workspace: Option<&str>| Planned {
        signal_id: "s2".into(),
        decision: DecisionKind::Work,
        workspace_id: workspace.map(str::to_string),
        title: "t".into(),
        reason: String::new(),
        brief: String::new(),
        merge_into: Some(thread.id.clone()),
        task_id: None,
    };
    assert!(merge_target(&store, &b, &planned(Some(&ws_a))).is_some(), "same workspace merges");
    assert!(merge_target(&store, &b, &planned(None)).is_some(), "home (A) merges");
    assert!(merge_target(&store, &b, &planned(Some(&ws_b))).is_none(), "another workspace opens fresh");

    // Somebody else's thread never merges, even in the right workspace.
    let mut other = b.clone();
    other.id = "other".into();
    assert!(merge_target(&store, &other, &planned(Some(&ws_a))).is_none());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn related_work_lists_earlier_threads_but_not_day_logs() {
    let root = tempdir("related");
    let dir = root.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open_for_test(root.join("data")).unwrap();
    store.migrate_mesh("m1").unwrap();
    let project = store.create_project(dir.to_string_lossy().into_owned(), Some("SS".into())).unwrap();
    let b = minimal_bosun();
    let fixed = store
        .create_thread(project.id.clone(), b.work.agent, b.work.settings.clone(), None)
        .unwrap();
    store.update_thread(&fixed.id, |t| t.title = "QB payment sync fix for Acme".into()).unwrap();
    let ws = store.workspace_for_project(&project.id).unwrap();
    let (log, _) = ensure_day_log(&store, &project.id, &b, &ws, "SS", &signal("x"), Local::now().date_naive()).unwrap();

    let hit = |id: &str| crate::search_index::IndexedHit {
        thread_id: id.to_string(),
        snippet: "matched a payment that never reached QuickBooks".into(),
        reason: String::new(),
        score: 1.0,
        message_seq: Some(3),
    };
    let out = render_related(&project.id, &store, &[hit(&fixed.id), hit(&log.id), hit("missing")]);
    assert!(out.contains(&format!("projectId: {}", project.id)));
    assert!(out.contains("QB payment sync fix for Acme"), "{out}");
    assert!(out.contains(&fixed.id));
    assert!(!out.contains(&log.id), "day logs are noise here");
    assert!(!out.contains("nothing indexed"));

    let empty = render_related(&project.id, &store, &[]);
    assert!(empty.contains("nothing indexed matches"));

    // The prompt carries the section verbatim, after the brief.
    let p = work_prompt("orders", "B", "Spencer", &signal("y"), "Orbit", "brief", false, &out);
    assert!(p.contains("## Earlier work in this workspace"));
    assert!(p.find("## Triage brief").unwrap() < p.find("## Earlier work").unwrap());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_newer_sweep_supersedes_the_queued_one_but_tickets_queue_up() {
    let root = tempdir("supersede");
    let registry = BosunRegistry::open(&root).unwrap();
    let mut a = signal("orbit:sweep:x:T09");
    a.kind = "sweep".into();
    a.lookout_id = "sweep-ss".into();
    let mut t1 = signal("orbit:task:1:v1");
    t1.kind = "ticket".into();
    t1.lookout_id = "orbit".into();
    registry.enqueue("b1", vec![a.clone(), t1.clone()]);
    let mut b = a.clone();
    b.id = "orbit:sweep:x:T11".into();
    let mut t2 = t1.clone();
    t2.id = "orbit:task:2:v1".into();
    registry.enqueue("b1", vec![b.clone(), t2.clone()]);
    let pending = registry.pending_for("b1", 50);
    let ids: Vec<&str> = pending.iter().map(|s| s.id.as_str()).collect();
    assert!(!ids.contains(&"orbit:sweep:x:T09"), "stale sweep dropped: {ids:?}");
    assert!(ids.contains(&"orbit:sweep:x:T11"));
    assert!(ids.contains(&"orbit:task:1:v1") && ids.contains(&"orbit:task:2:v1"), "tickets both kept: {ids:?}");
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Tasks (BOSUN.md §10)
// ---------------------------------------------------------------------------

fn refs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn task(key: &str, status: TaskStatus) -> BosunTask {
    let mut t = tasks::new_task("b1", key.into(), &format!("task {key}"), status, TaskSource::default(), &now_iso());
    t.status = status;
    t
}

#[test]
fn task_keys_are_derived_from_refs() {
    // A lookout's own key always wins.
    assert_eq!(
        derive_key("ticket", &refs(&[("key", "teams:c1:m1"), ("taskId", "t"), ("org", "o")]), "l").as_deref(),
        Some("teams:c1:m1")
    );
    assert_eq!(
        derive_key("ticket", &refs(&[("taskId", "cmx1"), ("org", "acme-1")]), "l").as_deref(),
        Some("orbit:acme-1:cmx1")
    );
    // taskId without org is not enough.
    assert_eq!(derive_key("ticket", &refs(&[("taskId", "cmx1")]), "l"), None);
    assert_eq!(
        derive_key("email", &refs(&[("threadId", "1a0e"), ("account", "me@x.com")]), "gmail-l").as_deref(),
        Some("gmail:me@x.com:1a0e")
    );
    assert_eq!(derive_key("email", &refs(&[("threadId", "1a0e")]), "gmail-l").as_deref(), Some("gmail:gmail-l:1a0e"));
    // A threadId on something that is not email (Teams) is not a gmail key.
    assert_eq!(derive_key("teams", &refs(&[("threadId", "1a0e")]), "l"), None);
    assert_eq!(
        derive_key("file", &refs(&[("path", "/calls/a/triage.md")]), "l").as_deref(),
        Some("call:/calls/a/triage.md")
    );
    assert_eq!(derive_key("webhook", &refs(&[("url", "https://x")]), "l"), None);
    assert_eq!(derive_key("ticket", &refs(&[("key", "  ")]), "l"), None, "blank key is no key");
    assert_eq!(orbit_from_key("orbit:acme-1:cmx1"), Some(("acme-1".into(), "cmx1".into())));
    assert_eq!(orbit_from_key("gmail:a:b"), None);
}

#[test]
fn at_most_one_open_task_per_key() {
    let root = tempdir("onekey");
    let registry = BosunRegistry::open(&root).unwrap();
    let (first, created) = registry.insert_task(task("orbit:acme:1", TaskStatus::Working));
    assert!(created);
    let (again, created) = registry.insert_task(task("orbit:acme:1", TaskStatus::Todo));
    assert!(!created, "a second open task for the key is refused");
    assert_eq!(again.id, first.id);
    // Another Bosun may track the same item.
    let mut other = task("orbit:acme:1", TaskStatus::Todo);
    other.bosun_id = "b2".into();
    assert!(registry.insert_task(other).1);
    // Once closed, the key is free again — and reopening the old one would
    // now conflict.
    registry
        .update_task(&first.id, |t| {
            set_status(t, TaskStatus::Done, "done", "you", &now_iso());
            Ok(())
        })
        .unwrap();
    let (fresh, created) = registry.insert_task(task("orbit:acme:1", TaskStatus::Todo));
    assert!(created && fresh.id != first.id);
    assert!(registry.reopen_conflict(&registry.task(&first.id).unwrap()));
    // Persisted: a reopened registry sees the same list.
    let reopened = BosunRegistry::open(&root).unwrap();
    assert_eq!(reopened.list_tasks(Some("b1"), false).len(), 1);
    assert_eq!(reopened.list_tasks(Some("b1"), true).len(), 2, "closed today shows with includeClosed");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn updates_queue_while_the_thread_is_busy_and_drain_once_idle() {
    // Where a work/ask signal goes, by the task's thread state.
    assert_eq!(route_for(None), Route::OpenThread);
    assert_eq!(route_for(Some(ThreadStatus::Idle)), Route::FollowUp);
    assert_eq!(route_for(Some(ThreadStatus::Running)), Route::Queue);
    assert_eq!(route_for(Some(ThreadStatus::WaitingApproval)), Route::Queue, "a pending question queues too");

    let mut t = task("orbit:acme:1", TaskStatus::Working);
    assert!(!drain_due(&t, Some(ThreadStatus::Idle)), "nothing queued, nothing to drain");
    t.pending_updates = vec!["s1".into(), "s2".into()];
    assert!(!drain_due(&t, Some(ThreadStatus::Running)));
    assert!(!drain_due(&t, Some(ThreadStatus::WaitingApproval)));
    assert!(!drain_due(&t, None));
    assert!(drain_due(&t, Some(ThreadStatus::Idle)));

    // The registry keeps queued signal bodies exactly as long as a task
    // still waits on them.
    let root = tempdir("queue");
    let registry = BosunRegistry::open(&root).unwrap();
    let (t, _) = registry.insert_task(t);
    registry.with_tasks(|f| {
        f.signals.insert("s1".into(), signal("s1"));
        f.signals.insert("s2".into(), signal("s2"));
        f.signals.insert("orphan".into(), signal("orphan"));
    });
    let reopened = BosunRegistry::open(&root).unwrap();
    let kept: Vec<String> = reopened.with_tasks(|f| f.signals.keys().cloned().collect());
    assert_eq!(kept, vec!["s1".to_string(), "s2".to_string()], "unreferenced bodies are dropped");
    let taken = reopened.with_tasks(|f| std::mem::take(&mut f.tasks[0].pending_updates));
    assert_eq!(taken.len(), 2, "one drain takes them all");
    assert!(reopened.with_tasks(|f| f.signals.is_empty()));
    assert!(reopened.task(&t.id).unwrap().pending_updates.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

fn sweep(items: Value, extra_refs: Value) -> Signal {
    let mut body = json!({
        "title": "Orbit sweep · Acme: 2 items need you",
        "kind": "sweep",
        "id": "orbit:sweep:acme:2026-10-01T09",
        "refs": { "org": "acme" },
        "items": items,
        "hints": { "workspace": "Acme" },
    });
    for (k, v) in extra_refs.as_object().unwrap() {
        body["refs"][k] = v.clone();
    }
    hail_signal("b1", "sweep-l", body.as_object().unwrap())
}

#[test]
fn sweeps_upsert_tasks_and_reconcile_within_scope() {
    let mut working = task("orbit:acme:1", TaskStatus::Working);
    working.thread_id = Some("thread-1".into());
    let mut tasks = vec![
        working,
        task("orbit:acme:2", TaskStatus::NeedsYou),
        task("orbit:other:9", TaskStatus::Todo),
        task("gmail:me:abc", TaskStatus::Todo),
    ];
    // Lookouts may send openKeys as a JSON array; it reaches refs as its text.
    let s = sweep(
        json!([
            { "key": "orbit:acme:1", "title": "Reply to Keith", "detail": "Client replied", "due": "2026-10-03",
              "orbitTaskId": "1", "orbitTicket": "#101", "url": "https://o.x/acme/tasks/1", "group": "replied", "client": null },
            { "key": "orbit:acme:3", "title": "Fix login for Fortis", "client": "Fortis", "detail": "Overdue",
              "orbitTaskId": "3", "url": "https://o.x/acme/tasks/3" },
            { "title": "no key, skipped" },
        ]),
        json!({ "scope": "orbit:acme:", "openKeys": ["orbit:acme:1", "orbit:acme:3"] }),
    );
    assert_eq!(s.items.len(), 2, "keyless item dropped at parse");
    assert_eq!(s.refs["openKeys"], r#"["orbit:acme:1","orbit:acme:3"]"#);
    let out = apply_sweep(&mut tasks, "b1", &s, Some("ws-acme"), Local::now());

    fn find(tasks: &[BosunTask], k: &str) -> BosunTask {
        tasks.iter().find(|t| t.key == k).unwrap().clone()
    }
    let refreshed = find(&tasks, "orbit:acme:1");
    assert_eq!(refreshed.status, TaskStatus::Working, "status kept");
    assert_eq!(refreshed.summary, "Client replied");
    assert_eq!(refreshed.due.as_deref(), Some("2026-10-03"));
    assert_eq!(refreshed.orbit.as_ref().unwrap().ticket_number.as_deref(), Some("101"));
    assert_eq!(out.refreshed, 1);

    let new = find(&tasks, "orbit:acme:3");
    assert_eq!(new.status, TaskStatus::Todo);
    assert!(new.thread_id.is_none(), "sweeps never open threads");
    assert_eq!(new.client.as_deref(), Some("Fortis"));
    assert_eq!(new.workspace_id.as_deref(), Some("ws-acme"));
    assert_eq!(new.source.kind, "sweep");
    assert_eq!(new.orbit.as_ref().unwrap().org, "acme");
    assert_eq!(out.created, vec![new.id.clone()]);

    let closed = find(&tasks, "orbit:acme:2");
    assert_eq!(closed.status, TaskStatus::Done);
    assert!(closed.done_at.is_some());
    assert_eq!(closed.history.last().unwrap().note, "closed in Orbit");
    assert_eq!(closed.history.last().unwrap().by, "orbit");
    assert_eq!(out.closed, vec![(closed.id.clone(), None)]);

    assert_eq!(find(&tasks, "orbit:other:9").status, TaskStatus::Todo, "outside the scope: untouched");
    assert_eq!(find(&tasks, "gmail:me:abc").status, TaskStatus::Todo);

    // Same sweep again: nothing new, nothing more closed.
    let again = apply_sweep(&mut tasks, "b1", &s, Some("ws-acme"), Local::now());
    assert!(again.created.is_empty() && again.closed.is_empty());

    // An empty-but-present openKeys closes everything in scope, the working
    // task (and so its thread) included.
    let empty = sweep(json!([]), json!({ "scope": "orbit:acme:", "openKeys": "" }));
    let out = apply_sweep(&mut tasks, "b1", &empty, None, Local::now());
    assert_eq!(out.closed.len(), 2);
    assert!(out.closed.contains(&(refreshed.id.clone(), Some("thread-1".into()))));
    assert_eq!(find(&tasks, "orbit:other:9").status, TaskStatus::Todo);

    // No openKeys at all: nothing is reconciled.
    let mut tasks = vec![task("orbit:acme:5", TaskStatus::Todo)];
    let partial = sweep(json!([]), json!({ "scope": "orbit:acme:" }));
    assert!(apply_sweep(&mut tasks, "b1", &partial, None, Local::now()).closed.is_empty());
    // Nor without a scope, which would otherwise match every key.
    let unscoped = sweep(json!([]), json!({ "openKeys": "" }));
    assert!(apply_sweep(&mut tasks, "b1", &unscoped, None, Local::now()).closed.is_empty());
    assert_eq!(tasks[0].status, TaskStatus::Todo);

    // Comma lists parse too.
    assert_eq!(parse_open_keys("a, b\nc,,").len(), 3);
}

#[test]
fn snoozes_expire_back_to_todo() {
    let now = Local::now();
    let mut past = task("k1", TaskStatus::Snoozed);
    past.snoozed_until = Some(iso_local(now - chrono::Duration::minutes(1)));
    let mut future = task("k2", TaskStatus::Snoozed);
    future.snoozed_until = Some(iso_local(now + chrono::Duration::days(1)));
    let mut by_date = task("k3", TaskStatus::Snoozed);
    by_date.snoozed_until = Some((now.date_naive() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string());
    let mut tasks = vec![past, future, by_date, task("k4", TaskStatus::NeedsYou)];
    let woke = expire_snoozes(&mut tasks, now);
    assert_eq!(woke.len(), 2);
    assert_eq!(tasks[0].status, TaskStatus::Todo);
    assert!(tasks[0].snoozed_until.is_none());
    assert_eq!(tasks[1].status, TaskStatus::Snoozed);
    assert_eq!(tasks[2].status, TaskStatus::Todo, "a bare date means local midnight");
    assert_eq!(tasks[3].status, TaskStatus::NeedsYou);

    // A sweep refreshing a task whose snooze has passed also wakes it.
    let mut snoozed = task("orbit:acme:1", TaskStatus::Snoozed);
    snoozed.snoozed_until = Some(iso_local(now - chrono::Duration::hours(1)));
    let mut tasks = vec![snoozed];
    apply_sweep(&mut tasks, "b1", &sweep(json!([{ "key": "orbit:acme:1", "title": "t" }]), json!({})), None, now);
    assert_eq!(tasks[0].status, TaskStatus::Todo);

    // Closed tasks leave the file after 14 days.
    let mut old = task("k5", TaskStatus::Done);
    old.done_at = Some(iso_local(now - chrono::Duration::days(15)));
    let mut recent = task("k6", TaskStatus::Dropped);
    recent.done_at = Some(iso_local(now - chrono::Duration::days(2)));
    let mut tasks = vec![old, recent, task("k7", TaskStatus::Todo)];
    assert_eq!(prune_closed(&mut tasks, now), 1);
    assert_eq!(tasks.len(), 2);
}

#[test]
fn thread_events_drive_task_status() {
    let none = AgentMark::default();
    use TaskStatus::*;
    assert_eq!(status_for_cue(Todo, ThreadCue::TurnStarted, none), Some(Working));
    assert_eq!(status_for_cue(Working, ThreadCue::TurnStarted, none), None);
    assert_eq!(status_for_cue(Working, ThreadCue::NeedsInput, none), Some(NeedsYou));
    assert_eq!(status_for_cue(NeedsYou, ThreadCue::InputResolved, none), Some(Working));
    assert_eq!(status_for_cue(WaitingOnClient, ThreadCue::InputResolved, none), None);
    // Turn ends with nobody having set a resting status: NeedsYou by default…
    assert_eq!(status_for_cue(Working, ThreadCue::TurnEnded, none), Some(NeedsYou));
    // …or the agent's last one.
    let waiting = AgentMark { set_this_turn: false, last: Some(WaitingOnClient) };
    assert_eq!(status_for_cue(Working, ThreadCue::TurnEnded, waiting), Some(WaitingOnClient));
    // The agent set a status this turn (it is no longer Working): kept.
    let set = AgentMark { set_this_turn: true, last: Some(WaitingOnClient) };
    assert_eq!(status_for_cue(WaitingOnClient, ThreadCue::TurnEnded, set), None);
    // A question still up when the turn ends: NeedsYou stays.
    assert_eq!(status_for_cue(NeedsYou, ThreadCue::TurnEnded, waiting), None);
    // Closed tasks are never reopened by their thread.
    for closed in [Done, Dropped] {
        for cue in [ThreadCue::TurnStarted, ThreadCue::NeedsInput, ThreadCue::TurnEnded] {
            assert_eq!(status_for_cue(closed, cue, none), None);
        }
    }
    assert_eq!(cue_for(&AgentEvent::TurnAborted), Some(ThreadCue::TurnEnded));
    assert_eq!(cue_for(&AgentEvent::Status { text: "x".into() }), None);

    // The agent's own update, through the tool.
    let mut t = task("adhoc:1", TaskStatus::Working);
    let set = tasks::apply_agent_update(
        &mut t,
        &json!({ "status": "waiting_on_client", "title": "Reply to Keith Roofing about job 2451",
                 "nextStep": "Keith confirms the date", "orbitTaskId": "cmx9", "orbitOrg": "acme" }),
        Some("https://o.x/acme/tasks/old"),
        &now_iso(),
    )
    .unwrap();
    assert_eq!(set, Some(WaitingOnClient));
    assert_eq!(t.status, WaitingOnClient);
    assert_eq!(t.title, "Reply to Keith Roofing about job 2451");
    let link = t.orbit.as_ref().unwrap();
    assert_eq!((link.org.as_str(), link.task_id.as_str()), ("acme", "cmx9"));
    assert_eq!(link.url, "https://o.x/acme/tasks/cmx9");
    assert_eq!(t.history.last().unwrap().by, "bosun");
    // A ticket number lands on the existing link.
    tasks::apply_agent_update(&mut t, &json!({ "orbitTaskId": "#101324" }), None, &now_iso()).unwrap();
    assert_eq!(t.orbit.as_ref().unwrap().ticket_number.as_deref(), Some("101324"));
    assert!(tasks::apply_agent_update(&mut t, &json!({ "status": "snoozed" }), None, &now_iso()).is_err());
    assert!(tasks::apply_agent_update(&mut t, &json!({ "status": "bogus" }), None, &now_iso()).is_err());
}

#[test]
fn migration_groups_duplicates_keeps_the_newest_and_settles_sweeps() {
    let root = tempdir("migrate");
    let dir = root.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open_for_test(root.join("data")).unwrap();
    store.migrate_mesh("m1").unwrap();
    let project = store.create_project(dir.to_string_lossy().into_owned(), Some("SS".into())).unwrap();
    let registry = BosunRegistry::open(&root.join("data")).unwrap();
    let mut b = minimal_bosun();
    b.home_workspace_id = store.workspace_for_project(&project.id).unwrap();
    registry.insert(b.clone()).unwrap();

    let make = |title: &str, sig: Signal, updated: &str, status: ThreadStatus| {
        let t = store.create_thread(project.id.clone(), b.work.agent, b.work.settings.clone(), None).unwrap();
        let origin = origin_for(&b, &sig, false);
        store
            .update_thread_quiet(&t.id, |t| {
                t.title = title.into();
                t.origin = Some(origin);
                t.updated_at = updated.into();
                t.status = status;
            })
            .unwrap()
    };
    let mut ticket = signal("orbit:task:cmx:1");
    ticket.refs = refs(&[("taskId", "cmx"), ("org", "acme"), ("url", "https://o.x/acme/tasks/cmx")]);
    let older = make("🎫 Fortis login", ticket.clone(), "2026-09-28T10:00:00Z", ThreadStatus::Idle);
    let newer = make("🎫 Fortis login (again)", ticket, "2026-09-29T10:00:00Z", ThreadStatus::WaitingApproval);
    let mut sw = signal("orbit:sweep:acme:2026-09-29T09");
    sw.kind = "sweep".into();
    let sweep_thread = make("Review the Orbit sweep", sw, "2026-09-29T09:00:00Z", ThreadStatus::Idle);
    let loose = make("Webhook ping", signal("w1"), "2026-09-29T08:00:00Z", ThreadStatus::Running);
    let ws = b.home_workspace_id.clone();
    let (log, _) = ensure_day_log(&store, &project.id, &b, &ws, "SS", &signal("x"), Local::now().date_naive()).unwrap();
    let mut stranger = minimal_bosun();
    stranger.id = "gone".into();
    let orphan = store.create_thread(project.id.clone(), b.work.agent, b.work.settings.clone(), None).unwrap();
    let origin = origin_for(&stranger, &signal("o"), false);
    store.update_thread(&orphan.id, |t| t.origin = Some(origin)).unwrap();

    let report = tasks::migrate_threads(&store, &registry);
    assert_eq!(report.created, 2, "one task for the Fortis key, one for the keyless thread");
    let all = registry.list_tasks(Some(&b.id), false);
    let fortis = all.iter().find(|t| t.key == "orbit:acme:cmx").expect("keyed task");
    assert_eq!(fortis.thread_id.as_deref(), Some(newer.id.as_str()), "newest thread kept");
    assert_eq!(fortis.status, TaskStatus::NeedsYou, "waiting_approval → NeedsYou");
    assert_eq!(fortis.title, "Fortis login (again)", "glyphs stripped");
    assert_eq!(fortis.orbit.as_ref().unwrap().task_id, "cmx");
    let adhoc = all.iter().find(|t| t.key.starts_with("adhoc:")).expect("keyless task");
    assert_eq!(adhoc.thread_id.as_deref(), Some(loose.id.as_str()));
    assert_eq!(adhoc.status, TaskStatus::Working, "running → Working");

    let older_now = store.thread(&older.id).unwrap();
    assert!(older_now.settled_at.is_some(), "duplicate settled");
    assert_eq!(older_now.origin.unwrap().task_id.as_deref(), Some(fortis.id.as_str()));
    assert!(report.notes.iter().any(|(id, note)| id == &older.id && note.contains(&newer.id)));
    assert!(store.thread(&newer.id).unwrap().settled_at.is_none());
    let sweep_now = store.thread(&sweep_thread.id).unwrap();
    assert!(sweep_now.settled_at.is_some() && sweep_now.origin.unwrap().task_id.is_none(), "sweeps: settled, no task");
    assert!(store.thread(&log.id).unwrap().origin.unwrap().task_id.is_none(), "day logs untouched");
    assert!(store.thread(&orphan.id).unwrap().origin.unwrap().task_id.is_none(), "unknown Bosun skipped");

    // Idempotent: a second pass finds nothing to do.
    let again = tasks::migrate_threads(&store, &registry);
    assert_eq!(again.created, 0);
    assert!(again.settled.is_empty() && again.notes.is_empty());
    assert_eq!(registry.list_tasks(Some(&b.id), true).len(), 2);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn legacy_threads_and_signals_without_task_fields_still_load() {
    let thread: Thread = serde_json::from_value(json!({
        "id": "t1", "projectId": "p1", "machineId": "m1", "agent": "claude", "title": "Old",
        "settings": { "model": "sonnet", "access": "edits", "mode": "build" },
        "status": "idle", "createdAt": "2026-09-28T00:00:00Z", "updatedAt": "2026-09-28T00:00:00Z",
        "origin": { "kind": "bosun", "bosunId": "b1", "bosunName": "Blackbeard", "signalId": "s1",
                    "lookoutId": "l1", "signalKind": "ticket", "refs": { "url": "u" }, "dayLog": false },
    }))
    .expect("pre-task bosun thread loads");
    assert!(thread.origin.as_ref().unwrap().task_id.is_none());
    let out = serde_json::to_value(&thread).unwrap();
    assert!(out["origin"].get("taskId").is_none(), "absent taskId is not written back");

    let signal: Signal = serde_json::from_value(json!({
        "id": "s1", "lookoutId": "l1", "bosunId": "b1", "kind": "ticket",
        "observedAt": "2026-09-28T00:00:00Z", "title": "t",
    }))
    .expect("pre-task signal loads");
    assert!(signal.items.is_empty());
    assert!(serde_json::to_value(&signal).unwrap().get("items").is_none());

    // And the state file that holds pending signals.
    let state: BosunState = serde_json::from_value(json!({ "pending": [serde_json::to_value(&signal).unwrap()] })).unwrap();
    assert_eq!(state.pending.len(), 1);

    // A task serializes camelCase with snake_case statuses.
    let mut t = task("orbit:acme:1", TaskStatus::NeedsYou);
    t.next_step = Some("call".into());
    let v = serde_json::to_value(&t).unwrap();
    assert_eq!(v["status"], "needs_you");
    assert_eq!(v["nextStep"], "call");
    assert_eq!(v["bosunId"], "b1");
    assert!(v["pendingUpdates"].as_array().unwrap().is_empty());
    assert_eq!(serde_json::from_value::<BosunTask>(v).unwrap(), t);
}

#[test]
fn task_update_payloads_parse() {
    let upd = TaskUpdate::parse(&json!({ "taskId": "t1", "status": "waiting_on_client", "title": " New ", "note": "called" }))
        .unwrap();
    assert_eq!(upd.task_id, "t1");
    assert_eq!(upd.status, Some(TaskStatus::WaitingOnClient));
    assert_eq!(upd.title.as_deref(), Some("New"));
    assert_eq!(upd.note.as_deref(), Some("called"));
    assert_eq!(upd.snoozed_until, None);

    let snooze = TaskUpdate::parse(&json!({ "taskId": "t1", "snoozedUntil": "2026-10-05" })).unwrap();
    assert_eq!(snooze.snoozed_until, Some(Some("2026-10-05".into())));
    let clear = TaskUpdate::parse(&json!({ "taskId": "t1", "snoozedUntil": null, "due": null, "nextStep": "" })).unwrap();
    assert_eq!(clear.snoozed_until, Some(None));
    assert_eq!(clear.due, Some(None));
    assert_eq!(clear.next_step, Some(None));
    let edits = TaskUpdate::parse(&json!({ "taskId": "t1", "summary": "s", "client": "Keith", "due": "2026-10-09",
                                           "workspaceId": "w2", "nextStep": "call" }))
    .unwrap();
    assert_eq!(edits.summary.as_deref(), Some("s"));
    assert_eq!(edits.client, Some(Some("Keith".into())));
    assert_eq!(edits.workspace_id.as_deref(), Some("w2"));

    assert!(TaskUpdate::parse(&json!({ "status": "done" })).is_err(), "taskId required");
    assert!(TaskUpdate::parse(&json!({ "taskId": "t1", "status": "finished" })).is_err());
    assert!(TaskUpdate::parse(&json!({ "taskId": "t1", "snoozedUntil": "next week" })).is_err());
    assert!(TaskUpdate::parse(&json!({ "taskId": "t1", "due": "soon" })).is_err());

    // Applying: snoozedUntil without status snoozes; clearing it wakes.
    let now = now_iso();
    let mut t = task("k", TaskStatus::NeedsYou);
    apply_user_update(&mut t, &snooze, &now).unwrap();
    assert_eq!(t.status, TaskStatus::Snoozed);
    assert_eq!(t.snoozed_until.as_deref(), Some("2026-10-05"));
    apply_user_update(&mut t, &clear, &now).unwrap();
    assert_eq!(t.status, TaskStatus::Todo);
    apply_user_update(&mut t, &edits, &now).unwrap();
    assert_eq!((t.summary.as_str(), t.client.as_deref(), t.due.as_deref()), ("s", Some("Keith"), Some("2026-10-09")));
    assert!(t.history.iter().any(|e| e.by == "you" && e.note.contains("summary")));
    let done = TaskUpdate::parse(&json!({ "taskId": "k", "status": "done" })).unwrap();
    apply_user_update(&mut t, &done, &now).unwrap();
    assert!(t.done_at.is_some());
    assert!(apply_user_update(&mut t, &TaskUpdate::parse(&json!({ "taskId": "k", "status": "snoozed" })).unwrap(), &now).is_err(),
        "snoozing needs a date");
}

#[test]
fn triage_carries_open_tasks_and_task_ids() {
    let b = minimal_bosun();
    let mut t = task("orbit:acme:1", TaskStatus::NeedsYou);
    t.client = Some("Keith Roofing".into());
    let p = triage_prompt(&b, "Spencer", &[], &[], &[t.clone()], &[signal("s1")], &HashMap::new());
    assert!(p.contains("## Open tasks"));
    assert!(p.contains(&format!("taskId: {} key: orbit:acme:1", t.id)));
    assert!(p.contains("client: Keith Roofing status: needs you"));
    assert!(p.find("## Open tasks").unwrap() < p.find("## Signals").unwrap());
    assert!(triage_schema().to_string().contains("\"taskId\""));
    let mut it = item("s1", "work", Some("home"));
    it.task_id = Some(format!(" {} ", t.id));
    let planned = validate_decisions(vec![it], &[signal("s1")], &b);
    assert_eq!(planned[0].task_id.as_deref(), Some(t.id.as_str()));
}

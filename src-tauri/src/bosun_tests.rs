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
    assert!(first.title.starts_with("⚓ Blackbeard log · Proj · "), "{}", first.title);
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
    let p = work_prompt("Summarise and stop.", "Blackbeard", "Spencer", &s, "Calls", "look at it", false);
    assert!(p.starts_with("Summarise and stop.\n\n---\nYou were woken by Blackbeard"));
    assert!(p.contains("a decision from Spencer"));
    assert!(p.contains("## Signal\nkind: ticket"));
    assert!(p.contains("source: Calls"));
    assert!(p.contains("refs: url=https://t/42"));
    assert!(p.contains("## Triage brief\nlook at it"));
    assert!(!p.contains("Triage chose ASK"));
    let ask = work_prompt("", "B", "Spencer", &s, "Calls", "", true);
    assert!(ask.contains("Triage chose ASK"));
}

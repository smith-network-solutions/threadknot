use crate::protocol::{Agent, AgentEvent, Thread, ThreadSettings};
use crate::store::Store;
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn fixture() -> (Store, PathBuf, Thread) {
    let dir = std::env::temp_dir().join(format!("threadknot-index-test-{}", uuid::Uuid::new_v4()));
    let store = Store::open_at(dir.clone()).unwrap();
    let project = store
        .create_project(dir.to_string_lossy().into_owned(), None)
        .unwrap();
    let settings: ThreadSettings =
        serde_json::from_value(json!({ "model": "test", "access": "full", "mode": "build" }))
            .unwrap();
    let thread = store
        .create_thread(project.id, Agent::Claude, settings, None)
        .unwrap();
    (store, dir, thread)
}

fn say(store: &Store, id: &str, text: &str) {
    store
        .append_event(
            id,
            None,
            &AgentEvent::AssistantMessage { text: text.into() },
        )
        .unwrap();
}

fn await_hits(
    store: &Store,
    id: &str,
    query: &str,
    expected: usize,
) -> crate::search_index::IndexedResults {
    let start = Instant::now();
    loop {
        let out = store
            .indexed_search(&[id.into()], query, 20, false)
            .unwrap();
        if out.index.ready && out.results.len() == expected {
            return out;
        }
        assert!(
            // Background commits can be delayed by the full suite's disk I/O
            // on Windows. Keep polling readiness, not a fixed sleep, and still
            // fail if the requested index state never arrives.
            start.elapsed() < Duration::from_secs(60),
            "query {query}: {} results, ready {}, indexed {}/{}, error {:?}",
            out.results.len(),
            out.index.ready,
            out.index.indexed_threads,
            out.index.total_threads,
            out.index.error
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn prefixes_find_message_words_urls_and_titles_with_matching_excerpts() {
    let (store, dir, thread) = fixture();
    say(
        &store,
        &thread.id,
        &format!(
            "{} The link is https://butterfly-effect-calendar.example.app. Café planning.",
            "Earlier unrelated discussion. ".repeat(80)
        ),
    );
    await_hits(&store, &thread.id, "butterfly", 1);
    for query in [
        "butter",
        "butterf",
        "butterfl",
        "BUTTERFL",
        "butterf cal",
        "café",
        "CAF",
    ] {
        let found = await_hits(&store, &thread.id, query, 1);
        assert!(
            found.results[0].snippet.contains("butterfly")
                || found.results[0].snippet.contains("Café"),
            "{query}: {}",
            found.results[0].snippet
        );
        assert_eq!(found.results[0].message_seq, Some(0));
    }
    // Search syntax remains literal, and starts-with is not an infix search.
    await_hits(&store, &thread.id, "butterf*", 1);
    await_hits(&store, &thread.id, "utterfl", 0);
    await_hits(&store, &thread.id, "b", 0);
    store
        .update_thread(&thread.id, |t| t.title = "Dragonfly project".into())
        .unwrap();
    await_hits(&store, &thread.id, "dragonf", 1);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn exact_and_multiple_words_rank_above_broad_completions() {
    let (store, dir, thread) = fixture();
    let other = store
        .create_thread(
            thread.project_id.clone(),
            Agent::Claude,
            thread.settings.clone(),
            None,
        )
        .unwrap();
    say(&store, &thread.id, "butter calendar notes");
    say(&store, &other.id, "butterfly report");
    await_hits(&store, &thread.id, "butter", 1);
    await_hits(&store, &other.id, "butterfly", 1);
    let ids = [other.id, thread.id.clone()];
    for (query, count) in [("butter", 2), ("but cal", 1)] {
        let found = store.indexed_search(&ids, query, 20, true).unwrap();
        assert_eq!(found.results.len(), count);
        assert_eq!(found.results[0].thread_id, thread.id, "{query}");
        if count > 1 {
            assert!(found.results[0].score > found.results[1].score);
        }
    }
    let broad = store
        .indexed_search(&ids, "butterf zzzunmatched", 20, true)
        .unwrap();
    assert_eq!(broad.results.len(), 1);
    assert_eq!(broad.results[0].reason, "Matched some search words");
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn typo_fallback_is_scoped_conservative_and_keeps_matching_context() {
    let (store, dir, thread) = fixture();
    say(
        &store,
        &thread.id,
        &format!("{} butterfly résumé", "Unrelated introduction. ".repeat(80)),
    );
    await_hits(&store, &thread.id, "butterfly", 1);
    for query in ["butterfy", "buttefrly", "butterflx", "buutterfly", "résumè"] {
        let found = await_hits(&store, &thread.id, query, 1);
        assert_eq!(found.results[0].reason, "Similar spelling match");
        assert!(
            found.results[0].snippet.contains("butterfly")
                || found.results[0].snippet.contains("résumé")
        );
    }
    await_hits(&store, &thread.id, "bxtterflx", 0);
    await_hits(&store, &thread.id, "bute", 0);
    assert!(store
        .indexed_search(&["not-in-scope".into()], "butterfy", 20, true)
        .unwrap()
        .results
        .is_empty());
    assert!(await_hits(&store, &thread.id, "butter", 1).results[0]
        .reason
        .is_empty());
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backfills_tails_reopens_and_catches_up_without_duplicate_messages() {
    let (store, dir, thread) = fixture();
    say(
        &store,
        &thread.id,
        "We voided the duplicate invoice yesterday.",
    );
    let first = await_hits(&store, &thread.id, "duplicate invoice", 1);
    assert!(first.results[0].snippet.contains("invoice"));
    assert_eq!(first.results[0].message_seq, Some(0));
    say(&store, &thread.id, "The sapphire deployment is now ready.");
    assert_eq!(
        await_hits(&store, &thread.id, "sapphire", 1).results[0].message_seq,
        Some(1)
    );
    drop(store);
    // Simulate an older build appending while the index is offline.
    let store = Store::open_at(dir.clone()).unwrap();
    say(
        &store,
        &thread.id,
        "The emerald followup arrived after restart.",
    );
    let out = await_hits(&store, &thread.id, "emerald", 1);
    assert_eq!(out.results[0].message_seq, Some(2));
    await_hits(&store, &thread.id, "invoice", 1);
    let index = tantivy::Index::open_in_dir(dir.join("search-index-v1")).unwrap();
    let reader: tantivy::IndexReader = index.reader().unwrap();
    assert_eq!(
        reader.searcher().num_docs(),
        4,
        "one title plus three messages"
    );
    drop(reader);
    drop(index);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rename_delete_restore_and_project_scope_track_the_catalog() {
    let (store, dir, thread) = fixture();
    say(&store, &thread.id, "uniqueinvoice");
    await_hits(&store, &thread.id, "uniqueinvoice", 1);
    assert!(store
        .indexed_search(&["../../projects".into()], "uniqueinvoice", 20, false)
        .unwrap()
        .results
        .is_empty());
    assert!(store
        .indexed_search(&[], "uniqueinvoice", 20, false)
        .unwrap()
        .results
        .is_empty());
    store
        .update_thread(&thread.id, |t| t.title = "quartztitle".into())
        .unwrap();
    await_hits(&store, &thread.id, "quartztitle", 1);
    let events = store.read_events(&thread.id);
    store.delete_thread(&thread.id).unwrap();
    assert!(
        store
            .indexed_search(&[thread.id.clone()], "uniqueinvoice", 20, false)
            .unwrap()
            .results
            .is_empty(),
        "deleted catalog entries are hidden before the writer commits"
    );
    store.restore_thread(thread.clone(), events).unwrap();
    await_hits(&store, &thread.id, "uniqueinvoice", 1);
    let other_dir = dir.join("other");
    std::fs::create_dir_all(&other_dir).unwrap();
    let other = store
        .create_project(other_dir.to_string_lossy().into_owned(), None)
        .unwrap();
    let answer = crate::mcp_search::execute(
        &store,
        "conversation_search",
        &json!({"query":"uniqueinvoice", "projectId":other.id}),
    )
    .unwrap();
    assert_eq!(answer["results"].as_array().unwrap().len(), 0);
    store.delete_project(&thread.project_id).unwrap();
    assert!(store
        .indexed_search(&[thread.id], "uniqueinvoice", 20, false)
        .unwrap()
        .results
        .is_empty());
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unicode_tools_and_agent_context_are_bounded_and_validated() {
    let (store, dir, thread) = fixture();
    say(&store, &thread.id, "Résumé café invoice");
    store
        .append_event(
            &thread.id,
            None,
            &AgentEvent::ToolStart {
                call_id: "secretprotocolid".into(),
                name: "shell".into(),
                detail: "toolneedle".into(),
            },
        )
        .unwrap();
    say(
        &store,
        &thread.id,
        &format!("latermessage {}", "é".repeat(4000)),
    );
    await_hits(&store, &thread.id, "CAFÉ", 1);
    await_hits(&store, &thread.id, "toolneedle", 1);
    assert!(store
        .indexed_search(&[thread.id.clone()], "toolneedle", 20, true)
        .unwrap()
        .results
        .is_empty());
    assert!(store
        .indexed_search(&[thread.id.clone()], "secretprotocolid", 20, false)
        .unwrap()
        .results
        .is_empty());
    let result =
        crate::mcp_search::execute(&store, "conversation_search", &json!({ "query":"café" }))
            .unwrap();
    assert_eq!(result["results"][0]["threadId"], thread.id);
    let result = crate::mcp_search::execute(
        &store,
        "conversation_read",
        &json!({ "threadId":thread.id, "aroundSeq":0, "limit":1 }),
    )
    .unwrap();
    assert_eq!(result["messages"][0]["seq"], 0);
    assert_eq!(result["nextAfterSeq"], 0);
    let result = crate::mcp_search::execute(
        &store,
        "conversation_read",
        &json!({ "threadId":thread.id, "afterSeq":1, "limit":1 }),
    )
    .unwrap();
    assert_eq!(result["messages"][0]["truncated"], true);
    assert_eq!(
        result["messages"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        3000
    );
    for args in [
        json!({"threadId":"../../projects"}),
        json!({"threadId":thread.id,"limit":200}),
        json!({"threadId":thread.id,"aroundSeq":0,"afterSeq":0}),
    ] {
        assert!(crate::mcp_search::execute(&store, "conversation_read", &args).is_err());
    }
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn partial_tail_and_replaced_transcript_are_reconciled() {
    use std::io::Write;
    let (store, dir, thread) = fixture();
    say(&store, &thread.id, "originalword");
    await_hits(&store, &thread.id, "originalword", 1);
    let path = dir.join("threads").join(format!("{}.jsonl", thread.id));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    let line = json!({"seq":1,"ts":"2026-09-18T00:00:00Z","event":{"kind":"assistant_message","text":"tailword"}}).to_string();
    file.write_all(line[..line.len() / 2].as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(1300));
    assert!(store
        .indexed_search(&[thread.id.clone()], "tailword", 20, false)
        .unwrap()
        .results
        .is_empty());
    writeln!(file, "{}", &line[line.len() / 2..]).unwrap();
    drop(file);
    await_hits(&store, &thread.id, "tailword", 1);
    let replacement = vec![crate::protocol::PersistedEvent {
        seq: 0,
        ts: crate::protocol::now_iso(),
        speaker: None,
        event: AgentEvent::AssistantMessage {
            text: "replacedword".into(),
        },
    }];
    store.restore_thread(thread.clone(), replacement).unwrap();
    await_hits(&store, &thread.id, "replacedword", 1);
    assert!(store
        .indexed_search(&[thread.id], "originalword", 20, false)
        .unwrap()
        .results
        .is_empty());
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupt_cache_rebuilds_without_changing_transcripts() {
    let (store, dir, thread) = fixture();
    say(&store, &thread.id, "recoverableword");
    await_hits(&store, &thread.id, "recoverableword", 1);
    let path = dir.join("threads").join(format!("{}.jsonl", thread.id));
    let original = std::fs::read(&path).unwrap();
    drop(store);
    std::fs::write(dir.join("search-index-v1/meta.json"), "invalid metadata").unwrap();
    let store = Store::open_at(dir.clone()).unwrap();
    await_hits(&store, &thread.id, "recoverableword", 1);
    assert_eq!(std::fs::read(path).unwrap(), original);
    assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|entry| entry
        .file_name()
        .to_string_lossy()
        .starts_with("search-index-broken-")));
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

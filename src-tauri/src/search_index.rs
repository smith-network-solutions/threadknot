//! Local, disposable conversation index. JSONL files remain authoritative.
//!
//! One writer tails complete JSONL records on a background thread. Its byte
//! checkpoints live in Tantivy's commit payload, so documents and offsets
//! become durable together. A crash can never checkpoint uncommitted messages.
use crate::protocol::{AgentEvent, PersistedEvent, Thread};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, FuzzyTermQuery, Occur, Query, TermQuery,
    TermSetQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, STORED, STRING, TEXT};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CatalogEntry {
    title: String,
    project_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Checkpoint {
    entry: CatalogEntry,
    offset: u64,
    modified: u128,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub ready: bool,
    pub indexed_threads: usize,
    pub total_threads: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedHit {
    pub thread_id: String,
    pub snippet: String,
    pub reason: String,
    pub score: f32,
    pub message_seq: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedResults {
    pub results: Vec<IndexedHit>,
    pub index: IndexStatus,
}

#[derive(Clone, Copy)]
struct Fields {
    key: Field,
    thread: Field,
    title: Field,
    body: Field,
    seq: Field,
    kind: Field,
}

struct Shared {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    catalog: Mutex<BTreeMap<String, CatalogEntry>>,
    reset: Mutex<HashSet<String>>,
    status: Mutex<IndexStatus>,
}

pub struct SearchIndex {
    shared: Arc<Shared>,
    wake: Option<mpsc::SyncSender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SearchIndex {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl SearchIndex {
    pub fn open(data_dir: &Path, threads: &[Thread]) -> Result<Self> {
        let path = data_dir.join("search-index-v1");
        std::fs::create_dir_all(&path)?;
        crate::store::restrict_dir(&path);
        let mut schema = Schema::builder();
        let fields = Fields {
            key: schema.add_text_field("key", STRING),
            thread: schema.add_text_field("thread", STRING | STORED),
            title: schema.add_text_field("title", TEXT | STORED),
            body: schema.add_text_field("body", TEXT | STORED),
            seq: schema.add_u64_field("seq", STORED),
            kind: schema.add_text_field("kind", STRING),
        };
        let schema = schema.build();
        let index = if path.join("meta.json").exists() {
            match Index::open_in_dir(&path) {
                Ok(index) if index.schema() == schema => index,
                other => {
                    tracing::warn!(?other, "rebuilding unreadable conversation search cache");
                    // Preserve the failed cache for diagnosis. Never touch transcripts.
                    std::fs::rename(
                        &path,
                        data_dir.join(format!("search-index-broken-{}", uuid::Uuid::new_v4())),
                    )?;
                    std::fs::create_dir_all(&path)?;
                    crate::store::restrict_dir(&path);
                    Index::create_in_dir(&path, schema)?
                }
            }
        } else {
            Index::create_in_dir(&path, schema)?
        };
        let writer = index.writer_with_num_threads(1, 20_000_000)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let shared = Arc::new(Shared {
            index,
            reader,
            fields,
            catalog: Mutex::new(catalog(threads)),
            reset: Mutex::new(HashSet::new()),
            status: Mutex::new(IndexStatus {
                total_threads: threads.len(),
                ..Default::default()
            }),
        });
        let (tx, rx) = mpsc::sync_channel(1);
        let worker_shared = Arc::clone(&shared);
        let dir = data_dir.to_owned();
        let worker = std::thread::Builder::new()
            .name("conversation-index".into())
            .spawn(move || run_worker(worker_shared, dir, writer, rx))?;
        Ok(Self {
            shared,
            wake: Some(tx),
            worker: Some(worker),
        })
    }

    pub fn catalog_changed(&self, threads: &[Thread]) {
        *self.shared.catalog.lock().unwrap() = catalog(threads);
        self.notify();
    }

    pub fn notify(&self) {
        if let Some(tx) = &self.wake {
            let _ = tx.try_send(());
        }
    }

    pub fn reset_thread(&self, id: &str) {
        self.shared.reset.lock().unwrap().insert(id.to_owned());
        self.notify();
    }

    pub fn status(&self) -> IndexStatus {
        self.shared.status.lock().unwrap().clone()
    }

    pub fn search(
        &self,
        ids: &[String],
        text: &str,
        limit: usize,
        messages_only: bool,
    ) -> Result<IndexedResults> {
        let status = self.status();
        let empty = || IndexedResults {
            results: vec![],
            index: status.clone(),
        };
        if text.trim().is_empty() || ids.is_empty() {
            return Ok(empty());
        }
        let fields = self.shared.fields;
        // Intersect before querying, including while a deletion is waiting for a commit.
        let allowed: Vec<Term> = {
            let catalog = self.shared.catalog.lock().unwrap();
            ids.iter()
                .filter(|id| catalog.contains_key(*id))
                .map(|id| Term::from_field_text(fields.thread, id))
                .collect()
        };
        if allowed.is_empty() {
            return Ok(empty());
        }
        // Use the index's own normalization, including Unicode case handling.
        // No user input is interpreted as query syntax or a regular expression.
        let mut analyzer = self.shared.index.tokenizer_for_field(fields.body)?;
        let mut words = Vec::new();
        let mut tokens = analyzer.token_stream(text);
        while tokens.advance() && words.len() < 64 {
            let word = &tokens.token().text;
            if !words.contains(word) {
                words.push(word.clone());
            }
        }
        drop(tokens);
        if words.is_empty() {
            return Ok(empty());
        }
        let scoped_query = |fuzzy, require_all| {
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
                (Occur::Must, word_query(fields, &words, fuzzy, require_all)),
                (Occur::Must, Box::new(TermSetQuery::new(allowed.clone()))),
            ];
            if messages_only {
                clauses.push((
                    Occur::Must,
                    Box::new(TermSetQuery::new(
                        ["message", "title"]
                            .iter()
                            .map(|kind| Term::from_field_text(fields.kind, kind)),
                    )),
                ));
            }
            BooleanQuery::new(clauses)
        };
        let searcher = self.shared.reader.searcher();
        let mut all_words = words.len() > 1;
        let mut query = scoped_query(false, all_words);
        if all_words
            && searcher
                .search(&query, &TopDocs::with_limit(1).order_by_score())?
                .is_empty()
        {
            // Descriptions may contain filler or words spread across messages.
            // Retain broad recall only when no message contains all the words.
            all_words = false;
            query = scoped_query(false, false);
        }
        let fuzzy = words.iter().any(|w| w.chars().count() >= 5)
            && searcher
                .search(&query, &TopDocs::with_limit(1).order_by_score())?
                .is_empty();
        if fuzzy {
            query = scoped_query(true, false);
        }
        let mut results: Vec<IndexedHit> = Vec::new();
        let mut positions: HashMap<String, usize> = HashMap::new();
        let limit = limit.clamp(1, 10_000);
        let mut offset = 0;
        // Page over message hits until enough distinct threads are found. A long
        // thread must not crowd every other thread out of the result window.
        loop {
            let hits = searcher.search(
                &query,
                &TopDocs::with_limit(256).and_offset(offset).order_by_score(),
            )?;
            let count = hits.len();
            for (score, address) in hits {
                let document: TantivyDocument = searcher.doc(address)?;
                let id = document
                    .get_first(fields.thread)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if positions
                    .get(id)
                    .is_some_and(|pos| !results[*pos].snippet.is_empty())
                {
                    continue;
                }
                let body = document
                    .get_first(fields.body)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let snippet = matching_excerpt(&mut analyzer, fields.body, body, &words, fuzzy);
                let seq = document.get_first(fields.seq).and_then(|v| v.as_u64());
                if let Some(pos) = positions.get(id) {
                    if !snippet.is_empty() {
                        results[*pos].snippet = snippet;
                        results[*pos].message_seq = seq.filter(|seq| *seq != u64::MAX);
                    }
                    continue;
                }
                if results.len() >= limit {
                    continue;
                }
                positions.insert(id.to_owned(), results.len());
                results.push(IndexedHit {
                    thread_id: id.to_owned(),
                    snippet,
                    reason: if fuzzy {
                        "Similar spelling match".into()
                    } else if words.len() > 1 && !all_words {
                        "Matched some search words".into()
                    } else {
                        String::new()
                    },
                    score,
                    message_seq: seq.filter(|seq| *seq != u64::MAX),
                });
            }
            if count < 256 || results.len() >= limit {
                break;
            }
            offset += count;
        }
        Ok(IndexedResults {
            results,
            index: status,
        })
    }
}

/// Word beginnings are useful while typing; one-letter prefixes and fuzzy
/// short words are too broad. Exact words retain BM25 and a fixed bonus so a
/// rare completion does not displace the literal word the user entered.
fn word_query(fields: Fields, words: &[String], fuzzy: bool, require_all: bool) -> Box<dyn Query> {
    let mut any = Vec::new();
    let mut all = Vec::new();
    for word in words {
        let mut alternatives: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut exact = Vec::new();
        for (field, boost) in [(fields.title, 3.0), (fields.body, 1.0)] {
            let term = Term::from_field_text(field, word);
            let literal: Box<dyn Query> =
                Box::new(TermQuery::new(term.clone(), IndexRecordOption::WithFreqs));
            exact.push((Occur::Should, literal.box_clone()));
            alternatives.push((Occur::Should, Box::new(BoostQuery::new(literal, boost))));
            if word.chars().count() >= 2 {
                alternatives.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(FuzzyTermQuery::new_prefix(term.clone(), 0, true)),
                        boost * 0.25,
                    )),
                ));
            }
            if fuzzy && word.chars().count() >= 5 {
                alternatives.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(FuzzyTermQuery::new(term, 1, true)),
                        boost * 0.1,
                    )),
                ));
            }
        }
        alternatives.push((
            Occur::Should,
            Box::new(ConstScoreQuery::new(
                Box::new(BooleanQuery::new(exact)),
                4.0,
            )),
        ));
        let query: Box<dyn Query> = Box::new(BooleanQuery::new(alternatives));
        all.push((Occur::Must, query.box_clone()));
        any.push((Occur::Should, query));
    }
    Box::new(BooleanQuery::new(if require_all { all } else { any }))
}

fn matching_excerpt(
    analyzer: &mut TextAnalyzer,
    field: Field,
    body: &str,
    words: &[String],
    fuzzy: bool,
) -> String {
    // Automaton queries do not expose their expanded terms to SnippetGenerator.
    // Recover the actual matching words from this message, rather than showing
    // its opening paragraph when the completion occurs much farther down.
    let mut matches = BTreeMap::new();
    let mut tokens = analyzer.token_stream(body);
    while tokens.advance() {
        let token = &tokens.token().text;
        let weight = words
            .iter()
            .map(|word| {
                if token == word {
                    2.0_f32
                } else if word.chars().count() >= 2 && token.starts_with(word) {
                    1.0
                } else if fuzzy && word.chars().count() >= 5 && one_edit_apart(word, token) {
                    0.5
                } else {
                    0.0
                }
            })
            .fold(0.0_f32, f32::max);
        if weight > 0.0 {
            matches.insert(token.clone(), weight);
        }
    }
    drop(tokens);
    let snippet = SnippetGenerator::new(matches, analyzer.clone(), field, 260).snippet(body);
    if snippet.is_empty() {
        body.chars().take(260).collect()
    } else {
        snippet.fragment().to_owned()
    }
}

/// One insertion, deletion, substitution, or adjacent transposition, matching
/// the fuzzy query's distance-one behavior. Operate on characters, not bytes.
fn one_edit_apart(a: &str, b: &str) -> bool {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let i = a
        .iter()
        .zip(&b)
        .position(|(a, b)| a != b)
        .unwrap_or(a.len().min(b.len()));
    if i == a.len().min(b.len()) {
        return true;
    }
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Less => a[i..] == b[i + 1..],
        std::cmp::Ordering::Greater => a[i + 1..] == b[i..],
        std::cmp::Ordering::Equal => {
            a[i + 1..] == b[i + 1..]
                || (i + 1 < a.len()
                    && a[i] == b[i + 1]
                    && a[i + 1] == b[i]
                    && a[i + 2..] == b[i + 2..])
        }
    }
}

fn catalog(threads: &[Thread]) -> BTreeMap<String, CatalogEntry> {
    threads
        .iter()
        .map(|t| {
            (
                t.id.clone(),
                CatalogEntry {
                    title: t.title.clone(),
                    project_id: t.project_id.clone(),
                },
            )
        })
        .collect()
}

fn run_worker(shared: Arc<Shared>, dir: PathBuf, mut writer: IndexWriter, rx: mpsc::Receiver<()>) {
    let mut checkpoints: BTreeMap<String, Checkpoint> = shared
        .index
        .load_metas()
        .ok()
        .and_then(|meta| meta.payload)
        .and_then(|payload| serde_json::from_str(&payload).ok())
        .unwrap_or_default();
    loop {
        let reset = std::mem::take(&mut *shared.reset.lock().unwrap());
        let result = sync_index(&shared, &dir, &mut writer, &mut checkpoints, &reset);
        if let Err(error) = result {
            shared.reset.lock().unwrap().extend(reset);
            tracing::warn!(%error, "conversation index update failed; retrying");
            let mut status = shared.status.lock().unwrap();
            status.ready = false;
            status.error = Some(error.to_string());
            // Return to the last durable checkpoints after a failed batch.
            let _ = writer.rollback();
            checkpoints = shared
                .index
                .load_metas()
                .ok()
                .and_then(|m| m.payload)
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {
                // Coalesce bursts of streamed events and catalog changes.
                std::thread::sleep(Duration::from_millis(150));
                while rx.try_recv().is_ok() {}
            }
        }
    }
}

fn sync_index(
    shared: &Shared,
    dir: &Path,
    writer: &mut IndexWriter,
    checkpoints: &mut BTreeMap<String, Checkpoint>,
    reset: &HashSet<String>,
) -> Result<()> {
    let catalog = shared.catalog.lock().unwrap().clone();
    let f = shared.fields;
    let mut changed = false;
    let deleted: Vec<String> = checkpoints
        .keys()
        .filter(|id| !catalog.contains_key(*id))
        .cloned()
        .collect();
    for id in deleted {
        writer.delete_term(Term::from_field_text(f.thread, &id));
        checkpoints.remove(&id);
        changed = true;
    }
    {
        let mut status = shared.status.lock().unwrap();
        status.total_threads = catalog.len();
    }
    let mut pending_threads = 0;
    for (id, entry) in &catalog {
        let path = dir
            .join("threads")
            .join(format!("{}.jsonl", crate::store::safe_segment(id)));
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => Some(metadata),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let len = metadata.as_ref().map_or(0, |m| m.len());
        let modified = metadata
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |t| t.as_nanos());
        let old = checkpoints.get(id);
        let rebuild = reset.contains(id)
            || old.is_none_or(|old| {
                old.entry != *entry
                    || len < old.offset
                    || (len == old.offset && old.modified != modified)
            });
        if !rebuild && old.is_some_and(|old| old.offset == len && old.modified == modified) {
            continue;
        }
        let mut offset = if rebuild {
            0
        } else {
            old.map_or(0, |old| old.offset)
        };
        if rebuild {
            writer.delete_term(Term::from_field_text(f.thread, id));
            writer.add_document(doc!(f.key => format!("{id}:title"), f.thread => id.as_str(),
                f.title => entry.title.as_str(), f.body => "", f.seq => u64::MAX, f.kind => "title"))?;
        }
        if metadata.is_some() {
            let mut file = std::fs::File::open(&path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut reader = std::io::BufReader::new(file);
            let mut line = Vec::new();
            // Bound each pass to the length observed above. Concurrent appends
            // are picked up on the next pass, including an unfinished last line.
            while offset < len {
                line.clear();
                let read = reader.read_until(b'\n', &mut line)?;
                if read == 0 || line.last() != Some(&b'\n') || offset + read as u64 > len {
                    break;
                }
                offset += read as u64;
                let Ok(event) = serde_json::from_slice::<PersistedEvent>(&line) else {
                    continue;
                };
                let body = crate::store::event_search_text(&event.event);
                if body.is_empty() {
                    continue;
                }
                let key = format!("{id}:{}", event.seq);
                writer.delete_term(Term::from_field_text(f.key, &key));
                let kind = if matches!(
                    event.event,
                    AgentEvent::UserMessage { .. } | AgentEvent::AssistantMessage { .. }
                ) {
                    "message"
                } else {
                    "event"
                };
                writer.add_document(
                    doc!(f.key => key, f.thread => id.as_str(), f.title => entry.title.as_str(),
                    f.body => body, f.seq => event.seq, f.kind => kind),
                )?;
            }
        }
        checkpoints.insert(
            id.clone(),
            Checkpoint {
                entry: entry.clone(),
                offset,
                modified,
            },
        );
        changed = true;
        pending_threads += 1;
        if pending_threads >= 20 {
            commit_index(shared, writer, checkpoints)?;
            pending_threads = 0;
            changed = false;
        }
    }
    if changed {
        commit_index(shared, writer, checkpoints)?;
    }
    shared.reader.reload()?;
    let mut status = shared.status.lock().unwrap();
    status.indexed_threads = checkpoints.len();
    status.ready = true;
    status.error = None;
    Ok(())
}

fn commit_index(
    shared: &Shared,
    writer: &mut IndexWriter,
    checkpoints: &BTreeMap<String, Checkpoint>,
) -> Result<()> {
    let payload = serde_json::to_string(checkpoints)?;
    let mut commit = writer
        .prepare_commit()
        .context("prepare conversation index commit")?;
    commit.set_payload(&payload);
    commit.commit()?;
    shared.reader.reload()?;
    shared.status.lock().unwrap().indexed_threads = checkpoints.len();
    Ok(())
}

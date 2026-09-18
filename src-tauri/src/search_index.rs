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
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermSetQuery};
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::snippet::SnippetGenerator;
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
        let mut parser =
            QueryParser::for_index(&self.shared.index, vec![fields.title, fields.body]);
        parser.set_field_boost(fields.title, 3.0);
        // Treat the search box as literal words, not a field/query-language endpoint.
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .take(64)
            .map(|word| format!("\"{word}\""))
            .collect();
        if words.is_empty() {
            return Ok(empty());
        }
        let (text_query, _) = parser.parse_query_lenient(&words.join(" "));
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (Occur::Must, text_query),
            (Occur::Must, Box::new(TermSetQuery::new(allowed))),
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
        let query = BooleanQuery::new(clauses);
        let searcher = self.shared.reader.searcher();
        let mut snippets = SnippetGenerator::create(&searcher, &query, fields.body)?;
        snippets.set_max_num_chars(260);
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
                let mut snippet = snippets.snippet_from_doc(&document).fragment().to_owned();
                if snippet.is_empty() {
                    snippet = document
                        .get_first(fields.body)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .chars()
                        .take(260)
                        .collect();
                }
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
                    reason: String::new(),
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

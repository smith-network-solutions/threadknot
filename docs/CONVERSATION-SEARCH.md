# Conversation search

Threadknot embeds Tantivy in its Rust process. No separate search server,
account, port, or embedding service is required. Each machine indexes its own
persisted conversations in `<data-dir>/search-index-v1/`.

## Backfill and updates

Startup opens the index and starts a background writer. Existing conversations
are backfilled automatically. Results become available in batches of 20 threads;
`index.ready` remains false until the initial pass finishes. The index status
reports indexed and total thread counts, plus any indexing error.

One document represents a visible transcript event, with its thread ID, title,
text, and event sequence number. A separate title document covers empty threads.
The index includes the same visible event fields as the previous content search,
including tool text, diffs, questions, and artifacts. The sidebar panel, AI
candidate retrieval and agent searches use user/assistant messages and titles
so tool logs do not crowd out the conversations themselves. The older
`thread.search` endpoint continues searching all visible event fields.

The writer tails complete JSONL lines from its saved byte offsets. Appends wake
it, bursts are coalesced, and a periodic pass catches changes made outside the
running process. It records byte offsets and file metadata in the Tantivy commit
payload, atomically with the indexed documents. Restarting catches up from those
offsets. An unfinished final line is retried, never checkpointed past.

Renames and project changes rebuild the affected thread. Restores explicitly
invalidate its checkpoint. Deleted threads are excluded from search immediately
through the catalog and removed from the index on the next writer pass. Changes
normally reach the index within roughly a second; the open search panel refreshes
every two seconds. Only persisted events are indexed, not streaming deltas.

JSONL files and `projects.json` remain authoritative. An unreadable index metadata
file or incompatible schema is moved to a `search-index-broken-<uuid>` directory
and rebuilt. Other index errors are reported rather than preventing app startup.
To rebuild manually, stop Threadknot and move `search-index-v1` aside, then start
it again. Never remove conversation files to repair search.

## Sidebar and AI search

Typing in the sidebar performs a debounced indexed search with excerpts. Title
substring matches remain available immediately. All projects is the default;
the project chip restricts the submitted thread IDs. Requests fan out to each
thread's owning machine. Offline or outdated peers produce a partial-results
notice instead of silently appearing to have no matches.

Enter or **Search Threads** runs the selected model. Tantivy supplies up to 40
matching threads and the server adds up to 20 recent threads for descriptions
with little keyword overlap. Only those transcripts are read into model digests.
During initial backfill or an index outage, AI search retains its previous scan
fallback. Indexed search itself does not invoke an AI provider.

`thread.indexedSearch` accepts `{query, threadIds, machineId?}` and returns
`{results, index}`. Results contain `threadId`, a plain-text `snippet`, BM25 `score`,
an empty `reason`, and nullable `messageSeq`. At most 60 distinct threads are
returned. Queries are limited to 400 characters and 10,000 supplied thread IDs.
Words are searched literally without exposing Tantivy's query language.

`thread.search` retains its `{threadIds}` response for older callers. It uses
the index when ready and the previous content scan during backfill or failure.
Indexed matching is token-based, rather than an arbitrary substring scan.

## Agent tools

The existing authenticated MCP endpoint advertises two read-only tools to its
connected agents. No extra MCP installation is necessary. Both operate on the
local machine's catalog; they do not implicitly retrieve a peer's transcripts.

- `conversation_search({query, projectId?, limit?})` returns up to 20 ranked
  threads, titles, excerpts, sequence locations and index readiness. Omit
  `projectId` to search all local projects.
- `conversation_read({threadId, aroundSeq?, afterSeq?, limit?})` returns up to
  20 visible entries with timestamps, roles and sequence numbers. Use `aroundSeq`
  from a hit to read its context, or `afterSeq: nextAfterSeq` to continue.
  Each entry is capped at 3,000 characters and marked when truncated.

Thread membership is validated before opening a transcript. Historical text is
returned as data, with no instruction authority. These tools make no model calls;
their selected excerpts enter the calling agent's context.

## Verification

`cargo test --lib search_index_tests` covers backfill, live appends, restart
catch-up, document deduplication, rename/delete/restore, project scope, Unicode,
tool-text inclusion, bounded agent context, incomplete JSONL records and corrupt-cache recovery.

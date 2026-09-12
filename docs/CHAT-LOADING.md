# Chat history loading

The client requests the most recent 1,000 events on opening a thread. Scrolling
back fetches older pages; a “Load older messages” button also allows explicit
loading and retry. Find searches loaded history and labels that scope while
older history remains. The existing transcript row window still limits the
initial render to 60 rows.

`thread.get` accepts optional `limit` (1–2,000) and `before` (an opaque numeric
cursor). Its optional `nextBefore` response is the cursor for the next older
page; null means the beginning was reached. Clients must return the cursor
unchanged. The store reads backward in 64 KiB blocks, returning at most the
requested count or approximately 2 MiB of raw events (a single event can exceed
that budget). Cursors refer to byte boundaries, so appends do not shift them.
Disk reads run outside the async server executor. Requests without `limit`
retain the full-history behavior for compatibility.

Replay trims long shell, MCP, and edit call details to a UTF-8-safe head and tail,
in addition to the existing output trimming. Structured agent invocation details
remain intact. Opening a trimmed call retrieves the full detail using
`thread.toolOutput` with `includeDetail: true`. The persisted log is unchanged.

Browsers supporting `DecompressionStream` request `/ws?compression=gzip`.
The server then sends frames of at least 4 KiB as gzip-compressed binary messages;
smaller frames remain JSON text. This is application-level compression, not
WebSocket permessage-deflate. Decompression and dispatch are serialized so a
later text event cannot overtake a compressed response. Browsers without the API
and clients that do not opt in continue using text. Older servers ignore the
new query parameter and return ordinary text, which the new client still accepts.
The browser API is documented by
[MDN](https://developer.mozilla.org/en-US/docs/Web/API/DecompressionStream).

Both the frontend bundle and backend binary must be rebuilt and restarted for
all improvements. Mixed versions work but may retain full-history loading or
uncompressed transfers. Peer owners must also be updated to serve pages. No
stored transcript migration is required.

## Validation

- `npm run build`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --test limits --test authorization_matrix`
- `node scripts/test-replay.mjs [optional JSONL path]`
- `node scripts/test-ws-compression.mjs`

A desktop benchmark of Jobber Migration Review (84,488 events) measured 29.1 s
for the original replay reducer, 564 ms for the optimized full replay, and 1.5 ms
for the initial 1,000-event page. Full replay content matched the original reducer.
These numbers exclude network, JSON parsing, and browser rendering; they are not
mobile end-to-end timings. Real phone scrolling and relay latency still need a
check after rollout.

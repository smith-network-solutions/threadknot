// Run: node scripts/test-replay.mjs [optional real JSONL transcript]
import { build } from 'esbuild';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';
const dir = await mkdtemp(join(tmpdir(), 'threadknot-replay-'));
try {
  await build({ entryPoints: ['src/state/store.tsx'], outfile: join(dir, 'store.mjs'), bundle: true, platform: 'node', format: 'esm' });
  await build({ entryPoints: ['src/state/feed.ts'], outfile: join(dir, 'feed.mjs'), bundle: true, platform: 'node', format: 'esm' });
  const { replayEvents, applyEvent } = await import(join(dir, 'feed.mjs'));
  const { initialState, reducer } = await import(join(dir, 'store.mjs'));
  const event = (seq, event) => ({ seq, ts: '2026-09-12T00:00:00Z', speaker: 'main', event });
  const events = [
    event(0, { kind: 'assistant_delta', text: 'Working' }),
    event(1, { kind: 'tool_start', callId: 'a', name: 'shell', detail: 'echo hello' }),
    event(2, { kind: 'tool_start', callId: 'b', name: 'shell', detail: 'echo world' }),
    event(3, { kind: 'tool_end', callId: 'a', name: 'shell', output: 'hello', isError: false }),
    event(4, { kind: 'tool_end', callId: 'b', name: 'shell', output: 'world', isError: false }),
    event(5, { kind: 'assistant_message', text: 'Done' }),
  ];
  const stripIds = feed => feed.map(({id, ...item}) => item);
  const legacy = events => events.reduce((feed, pe) => applyEvent(feed, pe.event, pe.ts, pe.speaker), []);
  assert.deepEqual(stripIds(replayEvents(events)), stripIds(legacy(events)));
  const unstamped = events.map(pe => ({...pe, speaker: pe.seq < 3 ? undefined : 'main'}));
  assert.deepEqual(stripIds(replayEvents(unstamped)), stripIds(legacy(unstamped)));
  let state = reducer(initialState, { type: 'openThread', threadId: 'test' });
  const thread = { id: 'test', projectId: 'p' };
  state = reducer(state, { type: 'feedLoaded', threadId: 'test', thread, events: events.slice(3), nextBefore: 100 });
  const live = event(6, { kind: 'assistant_message', text: 'Still here' });
  state = reducer(state, { type: 'agentEvent', threadId: 'test', seq: live.seq, event: live.event, timestamp: live.ts, speaker: live.speaker });
  state = reducer(state, { type: 'feedOlderLoaded', threadId: 'test', before: 100, events: events.slice(0, 3), nextBefore: null });
  assert.deepEqual(stripIds(state.feed), stripIds(legacy([...events, live])));
  assert.equal(state.lastSeq, 6);
  assert.equal(state.feedBefore, null);
  state = reducer(state, {type: 'feedLoaded', threadId: 'test', thread, events: [...events.slice(3), live], nextBefore: 100});
  assert.deepEqual(stripIds(state.feed), stripIds(legacy([...events, live])));
  assert.equal(state.feedBefore, null, 'reconnect retains earlier pages');
  assert.equal(reducer(state, {type: 'feedOlderLoaded', threadId: 'other', before: 100, events: []}), state);
  // Events arriving while the initial snapshot is in flight survive the load.
  let opening = reducer(initialState, {type: 'openThread', threadId: 'test'});
  opening = reducer(opening, {type: 'agentEvent', threadId: 'test', seq: 6, event: live.event, timestamp: live.ts, speaker: live.speaker});
  opening = reducer(opening, {type: 'feedLoaded', threadId: 'test', thread, events, nextBefore: null});
  assert.deepEqual(stripIds(opening.feed), stripIds(legacy([...events, live])));
  console.log('Replay equivalence, split tools, live arrivals, and stale-page tests passed.');
  if (process.argv[2]) {
    const real = (await readFile(process.argv[2], 'utf8')).trim().split('\n').flatMap(line => {try {return [JSON.parse(line)];} catch {return [];}});
    let t = performance.now();
    const optimized = replayEvents(real);
    console.log(`${real.length} events: optimized replay ${(performance.now()-t).toFixed(1)}ms`);
    t = performance.now();
    replayEvents(real.slice(-1000));
    console.log(`Initial 1000-event page: ${(performance.now()-t).toFixed(1)}ms`);
    t = performance.now();
    const original = legacy(real);
    console.log(`Original replay: ${(performance.now()-t).toFixed(1)}ms`);
    assert.deepEqual(stripIds(optimized), stripIds(original));
    console.log('Full real transcript matches original replay.');
  }
} finally { await rm(dir, { recursive: true, force: true }); }

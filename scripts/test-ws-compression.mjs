// Run: node scripts/test-ws-compression.mjs
import { build } from 'esbuild';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import assert from 'node:assert/strict';
const dir = await mkdtemp(join(tmpdir(), 'threadknot-ws-'));
const sockets = [];
class Socket {
  static OPEN = 1;
  readyState = 1;
  constructor(url) { this.url = String(url); sockets.push(this); }
  close() {}
}
globalThis.WebSocket = Socket;
try {
  await build({entryPoints: ['src/lib/ws.ts'], outfile: join(dir, 'ws.mjs'), bundle: true, platform: 'node', format: 'esm'});
  const {ThreadknotClient} = await import(join(dir, 'ws.mjs'));
  const client = new ThreadknotClient();
  const seen = [];
  client.onEvent = event => seen.push(event.seq);
  client.connect('ws://localhost/ws?token=example');
  const socket = sockets[0];
  assert.equal(new URL(socket.url).searchParams.get('compression'), 'gzip');
  assert.equal(new URL(socket.url).searchParams.get('token'), 'example');
  const compressed = new Blob([gzipSync(JSON.stringify({type: 'event', seq: 1}))]);
  socket.onmessage({data: compressed});
  socket.onmessage({data: JSON.stringify({type: 'event', seq: 2})});
  await new Promise(resolve => setTimeout(resolve, 50));
  assert.deepEqual(seen, [1, 2], 'text cannot overtake decompression');
  socket.onmessage({data: compressed});
  client.reconnect();
  await new Promise(resolve => setTimeout(resolve, 50));
  assert.deepEqual(seen, [1, 2], 'old socket frames are ignored after reconnect');
  const decoder = globalThis.DecompressionStream;
  globalThis.DecompressionStream = undefined;
  const legacy = new ThreadknotClient();
  legacy.connect('ws://localhost/ws');
  assert.equal(new URL(sockets.at(-1).url).searchParams.has('compression'), false);
  sockets.at(-1).onmessage({data: JSON.stringify({type: 'event', seq: 3})});
  globalThis.DecompressionStream = decoder;
  console.log('Compression negotiation, frame ordering, stale sockets, and browser fallback passed.');
} finally { await rm(dir, {recursive: true, force: true}); }

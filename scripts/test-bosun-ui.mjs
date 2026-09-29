// Run: node scripts/test-bosun-ui.mjs
// Unit tests for src/lib/bosun.ts (the Bosun UI's pure helpers). Bundled
// through esbuild the same way scripts/test-replay.mjs loads TS.
import { build } from 'esbuild';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';

const dir = await mkdtemp(join(tmpdir(), 'threadknot-bosun-ui-'));
let passed = 0;
const test = (name, fn) => {
  fn();
  passed += 1;
  console.log(`ok - ${name}`);
};

try {
  await build({
    entryPoints: ['src/lib/bosun.ts'],
    outfile: join(dir, 'bosun.mjs'),
    bundle: true,
    platform: 'node',
    format: 'esm',
    logLevel: 'error',
  });
  const b = await import(join(dir, 'bosun.mjs'));

  // Local timestamps, so day grouping is tested in whatever TZ this runs in.
  const at = (y, mo, d, h, mi) => new Date(y, mo - 1, d, h, mi).toISOString();
  const wake = (id, iso, signals, kinds, skipped) => ({
    id,
    bosunId: 'b1',
    at: iso,
    signals,
    decisions: kinds.map((k, i) => ({ signalId: `${id}-${i}`, title: `t${i}`, decision: k, reason: 'r' })),
    skipped,
    triageMs: 10,
  });
  const thread = (id, iso, origin, extra = {}) => ({
    id,
    projectId: 'p1',
    agent: 'claude',
    title: id,
    settings: { model: 'm', access: 'edits', mode: 'build' },
    status: 'idle',
    createdAt: iso,
    updatedAt: iso,
    ...(origin ? { origin } : {}),
    ...extra,
  });
  const origin = (dayLog = false) => ({
    kind: 'bosun', bosunId: 'b1', bosunName: 'Blackbeard', signalId: 's', lookoutId: 'l',
    signalKind: 'ticket', refs: {}, dayLog,
  });

  test('wake summary line', () => {
    const w = wake('w1', at(2026, 9, 28, 16, 2), 3, ['ignore', 'ignore', 'log']);
    assert.equal(b.wakeSummary(w), '16:02 · 3 signals · ignored 2, logged 1, woke 0');
    const w2 = wake('w2', at(2026, 9, 28, 9, 5), 4, ['work', 'ask', 'log', 'ignore']);
    assert.equal(b.wakeSummary(w2), '09:05 · 4 signals · ignored 1, logged 1, woke 2');
    const one = wake('w3', at(2026, 9, 28, 9, 5), 1, ['work']);
    assert.equal(b.wakeSummary(one), '09:05 · 1 signal · ignored 0, logged 0, woke 1');
    const skipped = wake('w4', at(2026, 9, 28, 23, 59), 2, [], 'triage failed: timeout');
    assert.equal(b.wakeSummary(skipped), '23:59 · 2 signals · triage failed: timeout');
    const budget = wake('w5', at(2026, 9, 28, 10, 0), 1, ['log'], 'budget: 6/6 this hour');
    assert.equal(b.wakeSummary(budget), '10:00 · 1 signal · ignored 0, logged 1, woke 0 · budget: 6/6 this hour');
  });

  test('day grouping: newest day first, items interleaved newest first', () => {
    const wakes = [
      wake('a', at(2026, 9, 27, 8, 0), 1, ['log']),
      wake('b', at(2026, 9, 28, 16, 2), 3, ['ignore']),
      wake('c', at(2026, 9, 28, 9, 0), 1, ['work']),
    ];
    const threads = [
      thread('t1', at(2026, 9, 28, 9, 1), origin()),
      thread('t2', at(2026, 9, 27, 8, 1), origin(true)),
      thread('plain', at(2026, 9, 28, 12, 0), undefined),
    ];
    const days = b.bosunTimeline(wakes, threads);
    assert.deepEqual(days.map((d) => d.key), ['2026-09-28', '2026-09-27']);
    assert.deepEqual(
      days[0].items.map((i) => (i.kind === 'wake' ? `w:${i.wake.id}` : `t:${i.thread.id}`)),
      ['w:b', 't:t1', 'w:c'],
    );
    assert.deepEqual(
      days[1].items.map((i) => (i.kind === 'wake' ? `w:${i.wake.id}` : `t:${i.thread.id}`)),
      ['t:t2', 'w:a'],
    );
    assert.deepEqual(b.bosunTimeline([], []), []);
  });

  test('day labels', () => {
    const now = new Date(2026, 8, 28, 12, 0);
    assert.equal(b.dayLabel('2026-09-28', now), 'Today');
    assert.equal(b.dayLabel('2026-09-27', now), 'Yesterday');
    assert.equal(b.dayLabel('2026-09-21', now), 'Mon, Sep 21');
    assert.equal(b.dayLabel('2025-12-31', now), 'Wed, Dec 31 2025');
  });

  test('validation passes for a complete draft', () => {
    const d = b.defaultBosunDraft({ homeWorkspaceId: 'ws1', workModel: 'claude-opus-5' });
    d.lookouts = [
      { ...b.newLookout('command'), kind: { type: 'command', command: './lookouts/orbit-tasks.py', args: [], env: {} } },
      { ...b.newLookout('folder'), kind: { type: 'folder', path: '~/Calls', pattern: 'triage.md', maxAgeDays: 3 } },
      b.newLookout('webhook'),
      b.newLookout('timer'),
    ];
    d.quietHours = { start: '22:00', end: '07:00' };
    d.charters = [b.newCharter('ws1'), b.newCharter('ws2')];
    assert.deepEqual(b.validateBosunDraft(d, [{ id: 'ws1' }, { id: 'ws2' }]), []);
    assert.match(d.lookouts[2].kind.secret, /^[0-9a-f]{32}$/);
    assert.match(b.randomSecret(), /^[0-9a-f]{32}$/);
  });

  test('validation failures', () => {
    const fields = (d, ws) => b.validateBosunDraft(d, ws).map((i) => i.field);
    const base = () => b.defaultBosunDraft({ homeWorkspaceId: 'ws1' });

    let d = base(); d.name = '  ';
    assert.deepEqual(fields(d), ['name']);
    d = base(); d.homeWorkspaceId = '';
    assert.deepEqual(fields(d), ['homeWorkspaceId']);
    d = base();
    assert.deepEqual(fields(d, [{ id: 'other' }]), ['homeWorkspaceId']);
    d = base(); d.quietHours = { start: '7:00', end: '24:00' };
    assert.deepEqual(fields(d), ['quietHours.start', 'quietHours.end']);
    d = base(); d.budget = { maxTurnsPerHour: 0, maxConcurrent: 0 };
    assert.deepEqual(fields(d), ['budget.maxTurnsPerHour', 'budget.maxConcurrent']);

    d = base();
    const cmd = b.newLookout('command'); cmd.intervalSecs = 29;
    const folder = b.newLookout('folder'); folder.kind.path = '~/Calls'; folder.intervalSecs = 30;
    const hook = b.newLookout('webhook'); hook.kind.secret = '';
    const weekly = b.newLookout('timer'); weekly.kind.cadence = { type: 'weekly', days: [], time: '09:00' };
    const badTime = b.newLookout('timer'); badTime.kind.cadence = { type: 'daily', time: '25:00' };
    const hourly = b.newLookout('timer'); hourly.kind.cadence = { type: 'hourly', everyHours: 2 };
    // Interval is ignored for webhook/timer lookouts.
    hourly.intervalSecs = 1;
    d.lookouts = [cmd, folder, hook, weekly, badTime, hourly];
    assert.deepEqual(fields(d), [
      'lookouts.0.command',
      'lookouts.0.intervalSecs',
      'lookouts.2.secret',
      'lookouts.3.cadence',
      'lookouts.4.cadence',
    ]);

    d = base(); d.charters = [b.newCharter('ws1'), b.newCharter('ws1')];
    assert.deepEqual(fields(d), ['charters.1.workspaceId']);
  });

  test('bosunThreads / bosunAttentionThreads', () => {
    const t1 = thread('t1', at(2026, 9, 28, 9, 0), origin(), { updatedAt: at(2026, 9, 28, 10, 0) });
    const t2 = thread('t2', at(2026, 9, 28, 11, 0), origin(), { status: 'waiting_approval', updatedAt: at(2026, 9, 28, 11, 0) });
    const t3 = thread('t3', at(2026, 9, 28, 8, 0), origin(true));
    const plain = thread('plain', at(2026, 9, 28, 12, 0), undefined);
    const other = thread('other', at(2026, 9, 28, 12, 0), { ...origin(), kind: 'something-else' });
    const state = {
      threads: { p1: [t1, t3, plain], p2: [t2, other] },
      attention: { t1: true, plain: true },
    };
    assert.deepEqual(b.bosunThreads(state).map((t) => t.id), ['t2', 't1', 't3']);
    // t1 unread, t2 waiting on approval; `plain` is unread but not a bosun's.
    assert.deepEqual(b.bosunAttentionThreads(state).map((t) => t.id), ['t2', 't1']);
    assert.deepEqual(b.bosunAttentionThreads({ threads: {}, attention: {} }), []);
  });

  test('normalizeDraft + env helpers', () => {
    const d = b.defaultBosunDraft({ homeWorkspaceId: 'ws1' });
    d.name = '  Jeeves ';
    d.charters = [{ ...b.newCharter('ws1'), routeHints: [' sender @x.com ', '', 'caller Bill'] }];
    const n = b.normalizeDraft(d);
    assert.equal(n.name, 'Jeeves');
    assert.deepEqual(n.charters[0].routeHints, ['sender @x.com', 'caller Bill']);
    assert.deepEqual(b.textToEnv('A=1\nB = two=2\n\nbad\n=x'), { A: '1', B: ' two=2' });
    assert.equal(b.envToText({ A: '1', B: '2' }), 'A=1\nB=2');
  });

  console.log(`\n${passed} passed`);
} finally {
  await rm(dir, { recursive: true, force: true });
}

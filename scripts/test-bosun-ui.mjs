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

  test('day grouping: newest day first, threads newest first, wakes folded', () => {
    const wakes = [
      wake('a', at(2026, 9, 27, 8, 0), 1, ['log']),
      wake('b', at(2026, 9, 28, 16, 2), 3, ['ignore']),
      wake('c', at(2026, 9, 28, 9, 0), 1, ['work']),
    ];
    const threads = [
      thread('t1', at(2026, 9, 28, 9, 1), origin()),
      thread('t3', at(2026, 9, 28, 17, 30), origin()),
      thread('t2', at(2026, 9, 27, 8, 1), origin(true)),
      thread('plain', at(2026, 9, 28, 12, 0), undefined),
    ];
    const days = b.bosunTimeline(wakes, threads);
    assert.deepEqual(days.map((d) => d.key), ['2026-09-28', '2026-09-27']);
    // No wake is interleaved with the threads: they live in their own list.
    assert.deepEqual(days[0].threads.map((t) => t.id), ['t3', 't1']);
    assert.deepEqual(days[0].wakes.map((w) => w.id), ['b', 'c']);
    assert.deepEqual(days[1].threads.map((t) => t.id), ['t2']);
    assert.deepEqual(days[1].wakes.map((w) => w.id), ['a']);
    assert.equal(days[0].summary.wakes, 2);
    assert.deepEqual(b.bosunTimeline([], []), []);
  });

  test('per-day summary aggregation', () => {
    const wakes = [
      wake('a', at(2026, 9, 28, 8, 0), 3, ['ignore', 'ignore', 'log']),
      wake('b', at(2026, 9, 28, 9, 0), 4, ['work', 'ask', 'log', 'ignore']),
      wake('c', at(2026, 9, 28, 10, 0), 2, [], 'triage failed: timeout'),
      wake('d', at(2026, 9, 28, 11, 0), 1, ['log'], 'budget: 6/6 this hour'),
      wake('e', at(2026, 9, 28, 12, 0), 1, [], 'triage failed: timeout'),
    ];
    const s = b.summarizeWakes(wakes);
    assert.deepEqual(
      { wakes: s.wakes, signals: s.signals, ignored: s.ignored, logged: s.logged, woke: s.woke },
      { wakes: 5, signals: 11, ignored: 3, logged: 3, woke: 2 },
    );
    // Skipped wakes, newest first, whatever the reason.
    assert.deepEqual(s.skipped.map((w) => w.id), ['e', 'd', 'c']);
    assert.equal(
      b.daySummaryLine('Today', s),
      'Today · 5 wakes · 11 signals · ignored 3 · logged 3 · woke 2',
    );
    assert.equal(
      b.skippedLine(s),
      '3 wakes skipped: triage failed: timeout (+1 other reason)',
    );

    const one = b.summarizeWakes([wake('x', at(2026, 9, 28, 8, 0), 1, ['work'])]);
    assert.equal(b.daySummaryLine('Yesterday', one), 'Yesterday · 1 wake · 1 signal · ignored 0 · logged 0 · woke 1');
    assert.equal(b.skippedLine(one), null);

    const lone = b.summarizeWakes([wake('y', at(2026, 9, 28, 8, 0), 2, [], 'triage failed: exit 1')]);
    assert.equal(b.skippedLine(lone), '1 wake skipped: triage failed: exit 1');

    // A day that only has threads is just its label.
    const empty = b.summarizeWakes([]);
    assert.equal(b.daySummaryLine('Today', empty), 'Today');

    // The timeline carries the same summary per day.
    const days = b.bosunTimeline(wakes, []);
    assert.equal(days.length, 1);
    assert.equal(days[0].summary.signals, 11);
    assert.equal(days[0].summary.skipped.length, 3);
  });

  test('displayTitle strips leading anchors only', () => {
    const t = (title) => ({ title });
    assert.equal(b.displayTitle(t('⚓ Call: BOL generation fixed')), 'Call: BOL generation fixed');
    assert.equal(b.displayTitle(t('⚓ ⚓ Call: BOL generation fixed')), 'Call: BOL generation fixed');
    assert.equal(b.displayTitle(t('⚓ ⚓ ⚓ Call: BOL generation fixed')), 'Call: BOL generation fixed');
    assert.equal(b.displayTitle(t('⚓⚓⚓Call')), 'Call');
    assert.equal(b.displayTitle(t('  ⚓️  ⚓\t Ticket 42  ')), 'Ticket 42');
    // Other titles are left alone, including an anchor that is not leading.
    assert.equal(b.displayTitle(t('Fix the anchor ⚓ icon')), 'Fix the anchor ⚓ icon');
    assert.equal(b.displayTitle(t('Plain title')), 'Plain title');
    assert.equal(b.displayTitle(t('')), '');
    assert.equal(b.displayTitle(t('⚓ ')), '');
    assert.equal(b.displayTitle(undefined), '');
    assert.equal(b.stripTitleMarks('⚓ ⚓ Blackbeard: done'), 'Blackbeard: done');
    // Pure: the thread is not mutated.
    const th = t('⚓ ⚓ x');
    b.displayTitle(th);
    assert.equal(th.title, '⚓ ⚓ x');
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

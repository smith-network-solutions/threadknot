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

  // ---- tasks (docs/BOSUN.md §10.7) ----------------------------------------

  const task = (id, status, extra = {}) => ({
    id,
    bosunId: 'b1',
    key: `orbit:sns:${id}`,
    title: `Task ${id}`,
    client: null,
    workspaceId: 'ws1',
    status,
    summary: '',
    nextStep: null,
    threadId: null,
    orbit: null,
    source: { kind: 'ticket', lookoutId: 'l', signalId: 's' },
    due: null,
    snoozedUntil: null,
    pendingUpdates: [],
    history: [],
    createdAt: at(2026, 9, 20, 9, 0),
    updatedAt: at(2026, 9, 20, 9, 0),
    doneAt: null,
    ...extra,
  });
  const NOW = new Date(2026, 9, 1, 12, 0); // Thu Oct 1 2026, 12:00 local

  test('task groups: fixed order, empty groups dropped, Dropped folds into Done', () => {
    const tasks = [
      task('d', 'done', { doneAt: at(2026, 9, 30, 10, 0) }),
      task('w', 'working'),
      task('n', 'needs_you'),
      task('s', 'snoozed'),
      task('x', 'dropped', { updatedAt: at(2026, 10, 1, 9, 0) }),
      task('c', 'waiting_on_client'),
      task('old', 'done', { doneAt: at(2026, 9, 20, 9, 0) }),
    ];
    const g = b.groupTasks(tasks, NOW);
    assert.deepEqual(g.map((x) => x.id), ['needs_you', 'working', 'waiting_on_client', 'snoozed', 'done']);
    assert.deepEqual(g.map((x) => x.label), ['Needs you', 'Working', 'Waiting on client', 'Snoozed', 'Done (3 days)']);
    assert.deepEqual(g.map((x) => x.collapsed), [false, false, false, true, true]);
    // Closed older than 3 days is left out even if the server sent it.
    assert.deepEqual(g[4].tasks.map((t) => t.id), ['x', 'd']);
    assert.deepEqual(b.groupTasks([], NOW), []);
  });

  test('task sort within a group: due soonest, undated last, then updated newest', () => {
    const tasks = [
      task('a', 'todo', { updatedAt: at(2026, 9, 29, 9, 0) }),
      task('b', 'todo', { due: '2026-10-05' }),
      task('c', 'todo', { due: '2026-10-02', updatedAt: at(2026, 9, 1, 9, 0) }),
      task('d', 'todo', { updatedAt: at(2026, 9, 30, 9, 0) }),
      task('e', 'todo', { due: '2026-10-02', updatedAt: at(2026, 9, 25, 9, 0) }),
    ];
    const [todo] = b.groupTasks(tasks, NOW);
    assert.equal(todo.id, 'todo');
    assert.deepEqual(todo.tasks.map((t) => t.id), ['e', 'c', 'b', 'd', 'a']);
  });

  test('task row meta line: client · workspace · due or age', () => {
    const t = task('a', 'needs_you', { client: 'Keith Roofing', due: '2026-10-01' });
    assert.equal(b.taskMetaLine(t, 'Service Storm', NOW), 'Keith Roofing · Service Storm · due today');
    assert.equal(b.taskMetaLine({ ...t, due: '2026-10-02' }, null, NOW), 'Keith Roofing · due tomorrow');
    assert.equal(b.taskMetaLine({ ...t, due: '2026-10-09' }, null, NOW), 'Keith Roofing · due Oct 9');
    assert.equal(b.taskMetaLine({ ...t, due: '2026-09-28' }, null, NOW), 'Keith Roofing · overdue Sep 28');
    const aged = task('b', 'todo', { createdAt: new Date(NOW.getTime() - 3 * 3600_000).toISOString() });
    assert.equal(b.taskMetaLine(aged, 'Home', NOW), 'Home · 3h ago');
    const fresh = task('c', 'todo', { createdAt: NOW.toISOString() });
    assert.equal(b.taskMetaLine(fresh, '', NOW), 'just now');
    const done = task('d', 'done', { doneAt: new Date(NOW.getTime() - 2 * 86400_000).toISOString() });
    assert.equal(b.taskMetaLine(done, 'Home', NOW), 'Home · done 2d ago');
  });

  test('snooze presets: tomorrow 8am and next Monday 8am, local time', () => {
    const [tomorrow, monday] = b.snoozePresets(NOW);
    assert.equal(tomorrow.label, 'Tomorrow 8am');
    assert.equal(new Date(tomorrow.until).getTime(), new Date(2026, 9, 2, 8, 0).getTime());
    assert.equal(monday.label, 'Next Monday 8am');
    assert.equal(new Date(monday.until).getTime(), new Date(2026, 9, 5, 8, 0).getTime());
    // On a Monday, "next Monday" is a week out, never today.
    const [, fromMonday] = b.snoozePresets(new Date(2026, 9, 5, 7, 0));
    assert.equal(new Date(fromMonday.until).getTime(), new Date(2026, 9, 12, 8, 0).getTime());
    // Sunday night: tomorrow and next Monday are the same morning.
    const [t2, m2] = b.snoozePresets(new Date(2026, 9, 4, 22, 0));
    assert.equal(t2.until, m2.until);
    // Month rollover.
    const [t3] = b.snoozePresets(new Date(2026, 9, 31, 23, 30));
    assert.equal(new Date(t3.until).getTime(), new Date(2026, 10, 1, 8, 0).getTime());
    assert.equal(new Date(b.snoozeUntilDate('2026-10-20')).getTime(), new Date(2026, 9, 20, 8, 0).getTime());
    assert.equal(b.snoozeUntilDate(''), null);
  });

  test('workspace roll-up counts and the folded row label', () => {
    const tasks = [
      task('a', 'needs_you'),
      task('b', 'needs_you'),
      task('c', 'working'),
      task('d', 'snoozed'),
      task('e', 'done'),
      task('f', 'todo', { workspaceId: 'ws2' }),
      task('g', 'needs_you', { workspaceId: null }),
    ];
    const r = b.workspaceTaskRollup(tasks);
    assert.deepEqual(r.get('ws1'), { needYou: 2, open: 3 });
    assert.deepEqual(r.get('ws2'), { needYou: 0, open: 1 });
    assert.equal(r.has('ws3'), false);
    assert.deepEqual(b.bosunRowLabel('Blackbeard', r.get('ws1')), { text: 'Blackbeard · 2 need you', muted: false });
    assert.deepEqual(b.bosunRowLabel('Blackbeard', { needYou: 1, open: 1 }), { text: 'Blackbeard · 1 needs you', muted: false });
    assert.deepEqual(b.bosunRowLabel('Blackbeard', r.get('ws2')), { text: 'Blackbeard · 1 open', muted: true });
    assert.equal(b.bosunRowLabel('Blackbeard', undefined), null);
    assert.equal(b.bosunRowLabel('Blackbeard', { needYou: 0, open: 0 }), null);
    assert.equal(b.needsYouCount(tasks), 3);
  });

  test('hide-bosun-threads filter: day logs folded too, order kept', () => {
    const threads = [
      thread('p1', at(2026, 9, 28, 9, 0)),
      thread('bt', at(2026, 9, 28, 9, 1), origin()),
      thread('p2', at(2026, 9, 28, 9, 2)),
      thread('log', at(2026, 9, 28, 9, 3), origin(true)),
      thread('other', at(2026, 9, 28, 9, 4), { ...origin(), kind: 'schedule' }),
    ];
    const { shown, bosun } = b.splitBosunThreads(threads);
    assert.deepEqual(shown.map((t) => t.id), ['p1', 'p2', 'other']);
    assert.deepEqual(bosun.map((t) => t.id), ['bt', 'log']);
  });

  test('task search, manual detection, display title', () => {
    const t = task('a', 'todo', { title: '⚓ Reply to Keith', client: 'Keith Roofing', summary: 'Job 2451 roof leak' });
    assert.equal(b.taskMatches(t, ''), true);
    assert.equal(b.taskMatches(t, 'keith'), true);
    assert.equal(b.taskMatches(t, '2451'), true);
    assert.equal(b.taskMatches(t, 'invoice'), false);
    assert.equal(b.displayTitle(t), 'Reply to Keith');
    assert.equal(b.isManualTask(t), false);
    assert.equal(b.isManualTask(task('m', 'todo', { key: 'adhoc:1', source: { kind: 'manual', lookoutId: '', signalId: '' } })), true);
    assert.equal(b.isOpenTask(t), true);
    assert.equal(b.isOpenTask(task('s', 'snoozed')), false);
  });

  // ---- task page: thread tail helpers ----
  let seqN = 0;
  const ev = (event) => ({ seq: ++seqN, ts: at(2026, 10, 2, 9, seqN % 60), event });
  const q = (requestId) => ev({
    kind: 'question_request',
    requestId,
    questions: [{ id: 'q1', header: 'Call', question: 'Which build?', options: [{ label: 'A', description: '' }], multiSelect: false, allowOther: true, isSecret: false }],
  });

  test('lastAssistantMessage: newest non-blank, ignores deltas and other kinds', () => {
    assert.equal(b.lastAssistantMessage([]), null);
    const events = [
      ev({ kind: 'user_message', text: 'hi' }),
      ev({ kind: 'assistant_message', text: 'first' }),
      ev({ kind: 'tool_start', callId: 'c', name: 'Bash', detail: '' }),
      ev({ kind: 'assistant_message', text: 'second **bold**' }),
      ev({ kind: 'assistant_message', text: '   ' }),
      ev({ kind: 'assistant_delta', text: 'streaming' }),
      ev({ kind: 'turn_completed' }),
    ];
    const got = b.lastAssistantMessage(events);
    assert.equal(got.text, 'second **bold**');
    assert.equal(got.seq, events[3].seq);
    assert.equal(b.lastAssistantMessage([ev({ kind: 'user_message', text: 'x' })]), null);
  });

  test('pendingQuestion: open while the turn is live', () => {
    const events = [ev({ kind: 'turn_started' }), ev({ kind: 'assistant_message', text: 'hm' }), q('r1')];
    const p = b.pendingQuestion(events);
    assert.equal(p.requestId, 'r1');
    assert.equal(p.questions[0].question, 'Which build?');
  });

  test('pendingQuestion: closed by question_resolved for the same request only', () => {
    assert.equal(b.pendingQuestion([ev({ kind: 'turn_started' }), q('r1'), ev({ kind: 'question_resolved', requestId: 'r1', answers: { q1: ['A'] } })]), null);
    const other = b.pendingQuestion([ev({ kind: 'turn_started' }), q('r2'), ev({ kind: 'question_resolved', requestId: 'zz' })]);
    assert.equal(other.requestId, 'r2');
  });

  test('pendingQuestion: closed by any turn boundary or error', () => {
    for (const kind of ['turn_completed', 'turn_aborted', 'turn_started']) {
      assert.equal(b.pendingQuestion([q('r1'), ev({ kind })]), null, kind);
    }
    assert.equal(b.pendingQuestion([q('r1'), ev({ kind: 'error', message: 'boom' })]), null);
  });

  test('pendingQuestion: a replayed request that was already answered stays closed; the newest open one wins', () => {
    const replay = [ev({ kind: 'turn_started' }), q('r1'), ev({ kind: 'question_resolved', requestId: 'r1' }), q('r1')];
    assert.equal(b.pendingQuestion(replay), null);
    const two = [ev({ kind: 'turn_started' }), q('r1'), ev({ kind: 'question_resolved', requestId: 'r1' }), ev({ kind: 'assistant_message', text: 'ok' }), q('r2')];
    assert.equal(b.pendingQuestion(two).requestId, 'r2');
    assert.equal(b.pendingQuestion([]), null);
  });

  test('visibleHistory hides the migration note only when other history exists', () => {
    const mig = { at: at(2026, 9, 28, 9, 0), note: 'Created from an existing Bosun thread', by: 'bosun' };
    const st = { at: at(2026, 9, 28, 10, 0), status: 'working', note: '', by: 'bosun' };
    const note = { at: at(2026, 9, 28, 11, 0), note: 'Asked Bill about the builder', by: 'bosun' };
    assert.deepEqual(b.visibleHistory([mig, st, note]), [st, note]);
    assert.deepEqual(b.visibleHistory([mig]), [mig]);
    assert.deepEqual(b.visibleHistory([]), []);
    assert.deepEqual(b.visibleHistory([{ ...mig, note: 'created from an existing Bosun thread.' }, note]), [note]);
  });

  test('recent touches: dedupe keeps newest, newest first, capped', () => {
    let list = [];
    list = b.addRecentTouch(list, 'a', at(2026, 10, 1, 9, 0));
    list = b.addRecentTouch(list, 'b', at(2026, 10, 1, 10, 0));
    list = b.addRecentTouch(list, 'a', at(2026, 10, 1, 11, 0));
    assert.deepEqual(list, [
      { taskId: 'a', at: at(2026, 10, 1, 11, 0) },
      { taskId: 'b', at: at(2026, 10, 1, 10, 0) },
    ]);
    // An older touch never replaces a newer one.
    list = b.addRecentTouch(list, 'b', at(2026, 9, 1, 10, 0));
    assert.equal(list.find((e) => e.taskId === 'b').at, at(2026, 10, 1, 10, 0));
    let many = [];
    for (let i = 0; i < 40; i++) many = b.addRecentTouch(many, `t${i}`, new Date(2026, 9, 1, 0, i).toISOString());
    assert.equal(many.length, b.RECENT_MAX);
    assert.equal(many[0].taskId, 't39');
    assert.equal(many[29].taskId, 't10');
    const th = b.addRecentThreadTouch(b.addRecentThreadTouch([], 'x', at(2026, 10, 1, 9, 0)), 'x', at(2026, 10, 1, 9, 5));
    assert.deepEqual(th, [{ threadId: 'x', at: at(2026, 10, 1, 9, 5) }]);
  });

  test('recent touches: parsing drops junk and normalizes', () => {
    assert.deepEqual(b.parseRecentTouches(null), []);
    assert.deepEqual(b.parseRecentTouches('not json'), []);
    assert.deepEqual(b.parseRecentTouches('{"taskId":"a"}'), []);
    const raw = JSON.stringify([
      { taskId: 'a', at: at(2026, 10, 1, 9, 0) },
      { taskId: '', at: at(2026, 10, 1, 9, 0) },
      { taskId: 'b', at: 'nope' },
      { taskId: 'c', at: at(2026, 10, 1, 12, 0), extra: 1 },
      { taskId: 'a', at: at(2026, 10, 1, 8, 0) },
      null,
      7,
    ]);
    assert.deepEqual(b.parseRecentTouches(raw), [
      { taskId: 'c', at: at(2026, 10, 1, 12, 0) },
      { taskId: 'a', at: at(2026, 10, 1, 9, 0) },
    ]);
    assert.deepEqual(
      b.parseRecentThreadTouches(JSON.stringify([{ threadId: 't', at: at(2026, 10, 1, 9, 0) }, { taskId: 'x', at: at(2026, 10, 1, 9, 0) }])),
      [{ threadId: 't', at: at(2026, 10, 1, 9, 0) }],
    );
  });

  test('recent tasks: 3-day window, limit, replied only when the Bosun wrote after your touch', () => {
    const byBosun = (iso) => ({ at: iso, note: 'did a thing', by: 'bosun' });
    const byYou = (iso) => ({ at: iso, note: 'moved', by: 'you' });
    const tasks = [
      // Touched 9:00; Bosun updated at 11:30 with the last word -> replied, sorts at 11:30.
      task('r', 'needs_you', { updatedAt: at(2026, 10, 1, 11, 30), history: [byBosun(at(2026, 10, 1, 11, 30))] }),
      // Touched 10:00; updated later but the last entry is yours -> not replied.
      task('y', 'todo', { updatedAt: at(2026, 10, 1, 11, 45), history: [byBosun(at(2026, 10, 1, 8, 0)), byYou(at(2026, 10, 1, 11, 45))] }),
      // Touched 11:00; the Bosun's entry predates the touch -> not replied.
      task('o', 'working', { updatedAt: at(2026, 10, 1, 10, 0), history: [byBosun(at(2026, 10, 1, 10, 0))] }),
      // Touched four days ago -> outside the window.
      task('old', 'todo'),
    ];
    const touches = [
      { taskId: 'o', at: at(2026, 10, 1, 11, 0) },
      { taskId: 'y', at: at(2026, 10, 1, 10, 0) },
      { taskId: 'r', at: at(2026, 10, 1, 9, 0) },
      { taskId: 'gone', at: at(2026, 10, 1, 11, 50) },
      { taskId: 'old', at: at(2026, 9, 27, 11, 0) },
    ];
    const r = b.recentTasks(tasks, touches, NOW);
    assert.deepEqual(r.map((x) => x.task.id), ['r', 'o', 'y']);
    assert.deepEqual(r.map((x) => x.replied), [true, false, false]);
    assert.equal(r[0].at, at(2026, 10, 1, 11, 30));
    assert.equal(r[0].touchedAt, at(2026, 10, 1, 9, 0));
    assert.equal(r[1].at, at(2026, 10, 1, 11, 0));
    // Limit: six by default.
    const lots = Array.from({ length: 9 }, (_, i) => task(`l${i}`, 'todo'));
    const lotsTouches = lots.map((t, i) => ({ taskId: t.id, at: new Date(2026, 9, 1, 10, i).toISOString() }));
    const six = b.recentTasks(lots, lotsTouches, NOW);
    assert.equal(six.length, b.RECENT_LIMIT);
    assert.equal(six[0].task.id, 'l8');
    assert.deepEqual(b.recentTasks(tasks, [], NOW), []);
    // Finished to-dos leave Recent unless asked for.
    const doneTask = { ...tasks[0], status: "done" };
    const withDone = [doneTask, ...tasks.slice(1)];
    assert.ok(!b.recentTasks(withDone, touches, NOW).some((r) => r.task.id === doneTask.id), "done hidden by default");
    assert.ok(b.recentTasks(withDone, touches, NOW, undefined, true).some((r) => r.task.id === doneTask.id), "done shown on request");
  });

  test('split keeps recently touched Bosun threads inline', () => {
    const withTask = (taskId) => ({ ...origin(), taskId });
    const threads = [
      thread('p1', at(2026, 9, 28, 9, 0)),
      thread('opened', at(2026, 9, 28, 9, 1), origin()),
      thread('viaTask', at(2026, 9, 28, 9, 2), withTask('t1')),
      thread('stale', at(2026, 9, 28, 9, 3), withTask('t2')),
      thread('log', at(2026, 9, 28, 9, 4), origin(true)),
      thread('oldOpen', at(2026, 9, 28, 9, 5), origin()),
    ];
    const keep = b.recentBosunThreadFilter(
      [
        { taskId: 't1', at: at(2026, 10, 1, 8, 0) },
        { taskId: 't2', at: at(2026, 9, 30, 11, 0) }, // 25h before NOW
      ],
      [
        { threadId: 'opened', at: at(2026, 10, 1, 11, 0) },
        { threadId: 'oldOpen', at: at(2026, 9, 29, 12, 0) },
      ],
      NOW,
    );
    const { shown, bosun } = b.splitBosunThreads(threads, keep);
    assert.deepEqual(shown.map((t) => t.id), ['p1', 'opened', 'viaTask']);
    assert.deepEqual(bosun.map((t) => t.id), ['stale', 'log', 'oldOpen']);
    // Without a filter nothing changes.
    assert.deepEqual(b.splitBosunThreads(threads).shown.map((t) => t.id), ['p1']);
    // A non-Bosun thread is never moved by the filter.
    const all = b.splitBosunThreads([thread('p2', at(2026, 9, 28, 9, 0))], () => false);
    assert.deepEqual(all.shown.map((t) => t.id), ['p2']);
  });

  console.log(`\n${passed} passed`);
} finally {
  await rm(dir, { recursive: true, force: true });
}

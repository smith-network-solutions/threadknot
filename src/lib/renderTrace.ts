/**
 * Render forensics — the instrument for the "whole screen flashes" bug.
 *
 * The flash is intermittent, lasts one or two frames, and mostly shows up with
 * several threads streaming at once, which makes it invisible to screenshots
 * and to a human watching. This module records what the screen was actually
 * doing, so the evidence survives the moment and can be read back out of a
 * headless browser with one `__tk.report()`.
 *
 * Four independent recorders, because "the screen flashed" has four different
 * mechanical causes and only one of them is a React re-render:
 *
 *  1. COMMITS  — every React commit, via the DevTools global hook, with the
 *     components that actually ran, the ones that mounted, and the ones that
 *     were deleted. A re-render is cheap; a *remount* destroys DOM and is the
 *     classic flash.
 *  2. DOM      — MutationObserver over #root, <head> and <html>'s attributes.
 *     A stylesheet inserted, `data-theme` rewritten, or the wallpaper var
 *     re-set repaints every pixel without React being involved at all.
 *  3. FRAMES   — rAF deltas + longtask entries. A 200ms frame reads as a
 *     flash even when the DOM survives it.
 *  4. ANCHORS  — the identity and scroll position of the panes that must never
 *     be replaced (.app, .work-pane, .feed…). If one of these nodes is swapped
 *     for a different node, that IS the flash, and the scroll jump proves it.
 *
 * Off by default and costs nothing when off: no hook is installed, no observer
 * is created, and `traceDispatch` returns the dispatch it was given.
 *
 * Turn it on with `?tktrace=1` (sticks in localStorage) or
 * `localStorage.setItem("threadknot.trace","1")` + reload — it must install
 * before react-dom evaluates, which is why main.tsx imports this first.
 *
 * See docs/RENDER-FORENSICS.md for the whole workflow.
 */

const LS_KEY = "threadknot.trace";

/** Fibers visited per commit before the walk gives up. The steady-state walk
 *  prunes to a handful of nodes; this only bites on a full-tree remount, which
 *  is exactly the case we already know about by the time we hit the cap. */
const MAX_FIBERS = 20_000;
/** React's `PerformedWork` flag: the component's render function actually ran
 *  during this commit (as opposed to being carried over by a bailout). */
const PERFORMED_WORK = 0b1;
/** `Placement`: this fiber's DOM node was inserted or moved. */
const PLACEMENT = 0b10;

const COMMIT_RING = 300;
const ACTION_RING = 200;
const INCIDENT_CAP = 300;
/** A frame this long is visible as a hitch even if nothing was destroyed. */
const LONG_FRAME_MS = 120;
/** More commits than this inside COMMIT_STORM_MS is a dispatch storm: the tree
 *  is re-rendering faster than it can paint. */
const COMMIT_STORM_N = 12;
const COMMIT_STORM_MS = 250;
/** A mount this soon after the same component was deleted is a destroy/rebuild
 *  cycle — the thing that actually flashes — rather than new UI appearing. */
const REMOUNT_WINDOW_MS = 2_000;
/** Below this, a destroy-and-rebuild is a leaf swapping (an icon, a chip) and
 *  cannot be what the user sees flash. Tuned so a feed item or a pane clears
 *  it and an icon does not. */
const REMOUNT_MIN_FIBERS = 20;
/** Deleting this many components at once is structural, not a list item. */
const MASS_UNMOUNT_N = 25;
/** Boot legitimately inserts every stylesheet and mounts the whole tree. Record
 *  it (it is useful context) but keep it off the console, or the real incidents
 *  drown in it. */
const BOOT_QUIET_MS = 2_500;
/** Panes that live for the lifetime of the window. Any of these being replaced
 *  by a *different* node is a remount of everything under it. */
const ANCHORS = [
  ".app",
  ".sidebar",
  ".work-pane",
  ".main-split",
  ".thread-pane",
  ".feed-scroll",
  ".feed-inner",
  ".composer",
];
/** The transcript's scroll container (ThreadView). Its scroll position is the
 *  user-visible test for "did my thread stay where it was". */
const FEED_SCROLLER = ".feed-scroll";

function readFlag(): boolean {
  try {
    const q = new URLSearchParams(window.location.search);
    const param = q.get("tktrace");
    if (param === "1" || param === "on") {
      localStorage.setItem(LS_KEY, "1");
      return true;
    }
    if (param === "0" || param === "off") {
      localStorage.removeItem(LS_KEY);
      return false;
    }
    return localStorage.getItem(LS_KEY) === "1";
  } catch {
    return false;
  }
}

export const TRACE_ON: boolean = typeof window !== "undefined" && readFlag();

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

export interface ActionRecord {
  t: number;
  type: string;
  /** Whatever narrows the action down: threadId, scope, kind. */
  about?: string;
}

export interface CommitRecord {
  t: number;
  /** Wall time the fiber walk itself took — if this climbs, the instrument is
   *  distorting what it measures and should be read with suspicion. */
  walkMs: number;
  /** Components whose render function ran, most first. */
  rendered: [string, number][];
  /** Components mounted in this commit (fresh fiber, no alternate). */
  mounts: [string, number][];
  /** Components deleted in this commit. */
  unmounts: [string, number][];
  /** Host nodes inserted or moved, as `tag.class`. */
  placements: string[];
  /** Actions dispatched since the previous commit — the commit's cause. */
  actions: string[];
  /** Components mounted here that were deleted moments ago: their DOM was
   *  thrown away and rebuilt, which is what a flash looks like. */
  remounted: string[];
  truncated?: boolean;
}

export interface Incident {
  t: number;
  iso: string;
  kind: string;
  detail: string;
  /** Actions dispatched in the 500ms before the incident. */
  actions: string[];
  /** The commits around it, newest last. */
  commits: CommitRecord[];
  frameMs: number;
  /** Happened during boot, when mounting everything is the correct behaviour. */
  boot?: boolean;
}

const actions: ActionRecord[] = [];
const commits: CommitRecord[] = [];
const incidents: Incident[] = [];
/** Fiber objects already accounted for. `alternate === null` only means "this
 *  fiber has never re-rendered", so on its own it re-reports every untouched
 *  component as a fresh mount on every commit. */
const seenFibers = new WeakSet<object>();
/** name → when it was last deleted and how much went with it, for the remount
 *  test. */
const recentUnmounts = new Map<string, { t: number; weight: number }>();
/** Lifetime render counts, for `__tk.hot()`. */
const renderTotals = new Map<string, number>();
const mountTotals = new Map<string, number>();
const unmountTotals = new Map<string, number>();

let pendingActions: string[] = [];
let lastFrameMs = 0;
let startedAt = 0;
/** Separate from `startedAt`, which `clear()` resets: clearing the buffers just
 *  before a stress run must not silence the console for the run. */
let bootUntil = 0;

/**
 * Incidents outlive the page. A watch left open for hours is worth nothing if
 * a reload, a navigation, or a webview restart takes the evidence with it, so
 * the incident list (minus the heavy per-commit detail) is mirrored into
 * localStorage on the origin being watched. Read it back with
 * `__tk.persisted()` — including from a session that did not record it.
 */
const LS_LOG = "threadknot.trace.log";
const PERSIST_CAP = 250;
let persistTimer: ReturnType<typeof setTimeout> | null = null;

function persistSoon(): void {
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    try {
      const slim = incidents
        .filter((i) => !i.boot)
        .slice(-PERSIST_CAP)
        .map(({ commits: _commits, ...rest }) => rest);
      localStorage.setItem(LS_LOG, JSON.stringify(slim));
    } catch {
      // Quota or a private-mode storage refusal: the in-memory ring is still
      // authoritative for this session.
    }
  }, 1_000);
}

function persisted(): Omit<Incident, "commits">[] {
  try {
    const raw = localStorage.getItem(LS_LOG);
    return raw ? (JSON.parse(raw) as Omit<Incident, "commits">[]) : [];
  } catch {
    return [];
  }
}

function push<T>(ring: T[], item: T, cap: number): void {
  ring.push(item);
  if (ring.length > cap) ring.splice(0, ring.length - cap);
}

function bump(map: Map<string, number>, key: string, by = 1): void {
  map.set(key, (map.get(key) ?? 0) + by);
}

function top(map: Map<string, number>, n: number): [string, number][] {
  return [...map.entries()].sort((a, b) => b[1] - a[1]).slice(0, n);
}

function recentActions(windowMs: number): string[] {
  const cut = performance.now() - windowMs;
  return actions.filter((a) => a.t >= cut).map((a) => `${a.type}${a.about ? `(${a.about})` : ""}`);
}

/** Kinds that can repeat every few hundred milliseconds under load. Left
 *  unthrottled, a long watch fills the ring with them and evicts the one
 *  structural incident it was left open to catch. Never dropped silently: the
 *  suppressed count rides along on the next one and in the report. */
const CHATTY = new Set([
  "commit-storm",
  "long-frame",
  "longtask",
  "stylesheet-added",
  "stylesheet-removed",
  "root-attrs-changed",
  "root-attrs-rewritten",
  "fonts-loaded",
  "visibility",
]);
const CHATTY_MIN_GAP_MS = 5_000;
const lastByKind = new Map<string, number>();
const suppressedByKind = new Map<string, number>();

function record(kind: string, detail: string): void {
  const t = performance.now();
  if (CHATTY.has(kind)) {
    const last = lastByKind.get(kind);
    if (last !== undefined && t - last < CHATTY_MIN_GAP_MS) {
      bump(suppressedByKind, kind);
      return;
    }
    lastByKind.set(kind, t);
    const skipped = suppressedByKind.get(kind);
    if (skipped) {
      detail += ` (+${skipped} more suppressed since the last one)`;
      suppressedByKind.set(kind, 0);
    }
  }
  if (incidents.length >= INCIDENT_CAP) incidents.shift();
  const boot = t < bootUntil;
  const inc: Incident = {
    t: Math.round(t),
    iso: new Date().toISOString(),
    kind,
    detail,
    actions: recentActions(500),
    commits: commits.slice(-4),
    frameMs: Math.round(lastFrameMs),
    ...(boot ? { boot: true } : {}),
  };
  incidents.push(inc);
  persistSoon();
  if (boot) return;
  // One line, greppable, so a headless browser's console log is enough to see
  // that it happened and roughly why, without evaluating anything.
  console.warn(
    `[tk-flash] ${kind} — ${detail} | frame=${inc.frameMs}ms | actions: ${inc.actions.slice(-8).join(", ") || "none"}`,
  );
}

// ---------------------------------------------------------------------------
// 1. Commits — the React DevTools global hook
// ---------------------------------------------------------------------------

interface Fiber {
  tag: number;
  key: string | null;
  elementType: unknown;
  type: unknown;
  stateNode: unknown;
  child: Fiber | null;
  sibling: Fiber | null;
  alternate: Fiber | null;
  flags: number;
  subtreeFlags: number;
  deletions: Fiber[] | null;
  memoizedProps: unknown;
}

interface DevtoolsHook {
  renderers: Map<number, unknown>;
  supportsFiber: boolean;
  inject(renderer: unknown): number;
  onCommitFiberRoot(id: number, root: { current: Fiber }, priority?: number): void;
  onPostCommitFiberRoot(id: number, root: unknown): void;
  onCommitFiberUnmount(id: number, fiber: Fiber): void;
  checkDCE?(fn: unknown): void;
  [k: string]: unknown;
}

function fiberName(f: Fiber): string | null {
  const t = (f.type ?? f.elementType) as
    | string
    | (((...a: unknown[]) => unknown) & { displayName?: string })
    | { displayName?: string; render?: unknown; type?: unknown }
    | null
    | undefined;
  if (typeof t === "function") return t.displayName || t.name || "Anonymous";
  // Host elements (div, span) are noise on their own; they are reported only
  // as placements, where the class name is what identifies the region.
  if (typeof t === "string") return null;
  if (t && typeof t === "object") {
    if (typeof t.displayName === "string") return t.displayName;
    const inner = (t.render ?? t.type) as
      | (((...a: unknown[]) => unknown) & { displayName?: string })
      | undefined;
    if (typeof inner === "function") return inner.displayName || inner.name || "Wrapped";
  }
  if (f.tag === 3) return "HostRoot";
  return null;
}

function hostLabel(f: Fiber): string {
  const el = f.stateNode as Element | null;
  if (!el || !el.tagName) return "?";
  const cls = typeof el.className === "string" && el.className ? `.${el.className.split(/\s+/)[0]}` : "";
  return `${el.tagName.toLowerCase()}${cls}`;
}

/** How many fibers went with a deletion. A swapped icon and a destroyed
 *  transcript are both "an unmount"; only the weight tells them apart, and only
 *  the heavy one can flash. */
function subtreeWeight(f: Fiber, cap = 2_000): number {
  let n = 0;
  const stack: Fiber[] = [f];
  while (stack.length > 0 && n < cap) {
    const cur = stack.pop() as Fiber;
    n++;
    if (cur.child) stack.push(cur.child);
    // Siblings of the deleted root are not part of it — only descend.
    if (cur !== f && cur.sibling) stack.push(cur.sibling);
  }
  return n;
}

/** Name the root of a deleted subtree (a deletion is one fiber; everything
 *  under it goes with it, so the root name is what identifies the loss). */
function nameDeleted(f: Fiber, into: Map<string, number>, weights: Map<string, number>): void {
  const weight = subtreeWeight(f);
  const remember = (name: string) => {
    bump(into, name);
    weights.set(name, Math.max(weights.get(name) ?? 0, weight));
  };
  const direct = fiberName(f);
  if (direct) {
    remember(direct);
    return;
  }
  // A deleted host element: report the first named component beneath it, which
  // is what a reader will recognise.
  let child = f.child;
  let guard = 0;
  while (child && guard++ < 50) {
    const n = fiberName(child);
    if (n) {
      remember(n);
      return;
    }
    child = child.child;
  }
  remember(hostLabel(f));
}

function walkCommit(rootFiber: Fiber): CommitRecord {
  const t0 = performance.now();
  const rendered = new Map<string, number>();
  const mounts = new Map<string, number>();
  const unmounts = new Map<string, number>();
  const placements: string[] = [];
  const deletedWeights = new Map<string, number>();
  let truncated = false;
  let visited = 0;

  const stack: Fiber[] = [rootFiber];
  while (stack.length > 0) {
    const f = stack.pop() as Fiber;
    if (++visited > MAX_FIBERS) {
      truncated = true;
      break;
    }

    const fresh = f.alternate === null && !seenFibers.has(f);
    if (f.alternate === null) seenFibers.add(f);

    const name = fiberName(f);
    if (name) {
      if (f.flags & PERFORMED_WORK) {
        bump(rendered, name);
        bump(renderTotals, name);
      }
      if (fresh) {
        bump(mounts, name);
        bump(mountTotals, name);
      }
    } else if (f.flags & PLACEMENT && f.tag === 5) {
      if (placements.length < 24) placements.push(hostLabel(f));
    }

    if (f.deletions) {
      for (const d of f.deletions) nameDeleted(d, unmounts, deletedWeights);
    }

    if (f.sibling) stack.push(f.sibling);
    // Prune: React bubbles child flags into subtreeFlags, so a zero on both
    // means nothing below this fiber changed. This is what keeps the walk at a
    // few dozen nodes in steady state instead of the whole tree.
    if (f.child && ((f.flags | f.subtreeFlags) !== 0 || f.alternate === null)) stack.push(f.child);
  }

  // A mount is a remount when the same component was deleted moments ago —
  // checked before this commit's own deletions are folded in, so a list that
  // swaps an item within one commit still reads as a rebuild of that item.
  const now = performance.now();
  const remounted: string[] = [];
  for (const [name] of mounts) {
    const gone = recentUnmounts.get(name);
    if (!gone || now - gone.t > REMOUNT_WINDOW_MS) continue;
    // Weight decides whether this is the flash or just a list row swapping an
    // icon: both are a destroy-and-rebuild, only one is visible.
    if (gone.weight >= REMOUNT_MIN_FIBERS) remounted.push(`${name}(${gone.weight} fibers)`);
  }
  for (const [k, v] of unmounts) {
    bump(unmountTotals, k, v);
    recentUnmounts.set(k, { t: now, weight: deletedWeights.get(k) ?? 1 });
  }
  if (recentUnmounts.size > 500) {
    for (const [k, u] of recentUnmounts) if (now - u.t > REMOUNT_WINDOW_MS) recentUnmounts.delete(k);
  }

  return {
    t: Math.round(t0),
    walkMs: Math.round((performance.now() - t0) * 100) / 100,
    rendered: top(rendered, 12),
    mounts: top(mounts, 12),
    unmounts: top(unmounts, 12),
    placements,
    actions: pendingActions,
    remounted,
    ...(truncated ? { truncated } : {}),
  };
}

let commitTimes: number[] = [];

function onCommit(root: { current: Fiber }): void {
  let rec: CommitRecord;
  try {
    rec = walkCommit(root.current);
  } catch (e) {
    console.warn("[tk-trace] commit walk failed", e);
    pendingActions = [];
    return;
  }
  pendingActions = [];
  push(commits, rec, COMMIT_RING);

  if (rec.remounted.length > 0) {
    record(
      "remount",
      `destroyed and rebuilt: ${rec.remounted.join(", ")} | mounts: ${rec.mounts
        .map(([n, c]) => `${n}×${c}`)
        .join(", ")}`,
    );
  }

  const deleted = rec.unmounts.reduce((n, [, c]) => n + c, 0);
  if (deleted >= MASS_UNMOUNT_N) {
    record("mass-unmount", `${deleted} components deleted: ${rec.unmounts.map(([n, c]) => `${n}×${c}`).join(", ")}`);
  }

  const now = performance.now();
  commitTimes.push(now);
  commitTimes = commitTimes.filter((t) => now - t <= COMMIT_STORM_MS);
  if (commitTimes.length === COMMIT_STORM_N) {
    record(
      "commit-storm",
      `${commitTimes.length} commits in ${COMMIT_STORM_MS}ms | hottest: ${rec.rendered
        .slice(0, 5)
        .map(([n, c]) => `${n}×${c}`)
        .join(", ")}`,
    );
  }
}

function installHook(): void {
  const w = window as unknown as { __REACT_DEVTOOLS_GLOBAL_HOOK__?: DevtoolsHook };
  const existing = w.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (existing) {
    // React DevTools is present (the Tauri webview or a browser extension).
    // Chain rather than replace, so the extension keeps working.
    const prev = existing.onCommitFiberRoot?.bind(existing);
    existing.onCommitFiberRoot = (id, root, priority) => {
      try {
        onCommit(root);
      } catch {
        /* never break the app for a diagnostic */
      }
      prev?.(id, root, priority);
    };
    return;
  }
  // No DevTools: install the minimum surface React looks for. React only
  // requires `supportsFiber` + `inject`; the rest must exist as callables.
  let nextId = 1;
  const hook: DevtoolsHook = {
    renderers: new Map(),
    supportsFiber: true,
    inject(renderer) {
      const id = nextId++;
      hook.renderers.set(id, renderer);
      return id;
    },
    onCommitFiberRoot(_id, root) {
      try {
        onCommit(root);
      } catch {
        /* never break the app for a diagnostic */
      }
    },
    onPostCommitFiberRoot() {},
    onCommitFiberUnmount() {},
    checkDCE() {},
  };
  w.__REACT_DEVTOOLS_GLOBAL_HOOK__ = hook;
}

// ---------------------------------------------------------------------------
// 2. DOM — the repaints React never sees
// ---------------------------------------------------------------------------

function installDomWatch(): void {
  // <head>: a stylesheet or webfont <link> inserted mid-session restyles every
  // element on the page. `ensureFontLoaded` in appearance.ts does exactly this.
  new MutationObserver((records) => {
    for (const r of records) {
      for (const n of Array.from(r.addedNodes)) {
        const el = n as Element;
        if (el.tagName === "STYLE" || el.tagName === "LINK") {
          const href = el.getAttribute("href");
          record("stylesheet-added", `<${el.tagName.toLowerCase()}> ${href ?? (el.textContent ?? "").slice(0, 80)}`);
        }
      }
      for (const n of Array.from(r.removedNodes)) {
        const el = n as Element;
        if (el.tagName === "STYLE" || el.tagName === "LINK") record("stylesheet-removed", `<${el.tagName.toLowerCase()}>`);
      }
    }
  }).observe(document.head, { childList: true });

  // <html> attributes: data-theme / data-family / data-skin-* and the inline
  // custom properties are the whole palette. Rewriting them repaints the app
  // even when the value is identical.
  let lastRootAttrs = snapshotRootAttrs();
  new MutationObserver(() => {
    const now = snapshotRootAttrs();
    const changed: string[] = [];
    for (const k of Object.keys(now)) if (now[k] !== lastRootAttrs[k]) changed.push(`${k}: ${lastRootAttrs[k] ?? "∅"} → ${now[k] ?? "∅"}`);
    // A rewrite with no net change is the interesting case: nothing needed to
    // happen, and the screen repainted anyway.
    record(
      changed.length ? "root-attrs-changed" : "root-attrs-rewritten",
      changed.length ? changed.join("; ") : "documentElement style/dataset written with no net change",
    );
    lastRootAttrs = now;
  }).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["style", "class", "data-theme", "data-family", "data-skin-off", "data-phosphor"],
  });

  // Fonts finishing a load reflow and repaint every glyph on screen — a real,
  // very visible flash that involves no React commit at all.
  const fonts = (document as unknown as { fonts?: FontFaceSet }).fonts;
  fonts?.addEventListener?.("loadingdone", (e) => {
    const n = (e as unknown as { fontfaces?: unknown[] }).fontfaces?.length ?? 0;
    record("fonts-loaded", `${n} face(s) finished loading — every glyph repaints`);
  });

  // Whole-document visibility flips (a webview suspend/resume) look identical
  // to a flash from the user's side, and they trigger the reconnect path.
  document.addEventListener("visibilitychange", () => {
    record("visibility", `document.visibilityState = ${document.visibilityState}`);
  });
}

function snapshotRootAttrs(): Record<string, string | null> {
  const r = document.documentElement;
  return {
    style: r.getAttribute("style"),
    class: r.getAttribute("class"),
    "data-theme": r.getAttribute("data-theme"),
    "data-family": r.getAttribute("data-family"),
    "data-skin-off": r.getAttribute("data-skin-off"),
  };
}

// ---------------------------------------------------------------------------
// 3 + 4. Frames and anchors
// ---------------------------------------------------------------------------

function installFrameWatch(): void {
  const anchors = new Map<string, Element>();
  let scroller: Element | null = null;
  let lastScrollTop = 0;
  let lastScrollHeight = 0;
  let prev = performance.now();

  /** Cheap enough to run on every frame, and correct without one: this is what
   *  still watches while the tab is in the background, where rAF is suspended
   *  but the socket keeps delivering and React keeps committing. */
  function checkStructure(): void {
    // Anchor identity: the node, not the selector. A replaced node means the
    // subtree under it was destroyed and rebuilt.
    for (const sel of ANCHORS) {
      const el = document.querySelector(sel);
      const had = anchors.get(sel);
      if (el && had && el !== had) record("anchor-replaced", `${sel} is a different DOM node than it was`);
      else if (!el && had) record("anchor-detached", `${sel} left the document`);
      if (el) anchors.set(sel, el);
      else anchors.delete(sel);
    }

    // Scroll position surviving is the user-visible test for "did my thread
    // stay put". A jump to the top with the content still there is the flash.
    const feed = document.querySelector(FEED_SCROLLER);
    if (feed !== scroller) {
      scroller = feed;
      lastScrollTop = feed?.scrollTop ?? 0;
      lastScrollHeight = feed?.scrollHeight ?? 0;
    } else if (feed) {
      const st = feed.scrollTop;
      const sh = feed.scrollHeight;
      if (lastScrollTop > 120 && st <= 1 && Math.abs(sh - lastScrollHeight) < 40) {
        record("scroll-reset", `${FEED_SCROLLER} scrollTop ${Math.round(lastScrollTop)} → 0 with content unchanged`);
      }
      lastScrollTop = st;
      lastScrollHeight = sh;
    }
  }

  function tick(): void {
    const now = performance.now();
    const delta = now - prev;
    prev = now;

    // A hidden tab suspends rAF entirely, so the gap on wake is minutes long
    // and means nothing. Frame timing is only meaningful while the page is
    // actually painting; structure is checked either way, below.
    if (!document.hidden && !wokeThisTick) {
      lastFrameMs = delta;
      if (delta > LONG_FRAME_MS) {
        const recent = commits.filter((c) => now - c.t < delta + 50);
        const who = recent.flatMap((c) => c.rendered).slice(0, 6).map(([n, c]) => `${n}×${c}`);
        record(
          "long-frame",
          `${Math.round(delta)}ms frame, ${recent.length} commit(s)${who.length ? ` | ${who.join(", ")}` : ""}`,
        );
      }
    }
    wokeThisTick = false;

    checkStructure();
    requestAnimationFrame(tick);
  }

  let wokeThisTick = false;
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) {
      // First frame back is not a 4-minute stall.
      wokeThisTick = true;
      prev = performance.now();
    }
  });
  requestAnimationFrame(tick);
  // Background insurance: throttled to ~1Hz by the browser, which is still
  // enough to catch a pane being replaced while nobody is looking.
  setInterval(checkStructure, 500);

  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.duration >= LONG_FRAME_MS) {
          record("longtask", `${Math.round(entry.duration)}ms task blocked the main thread`);
        }
      }
    }).observe({ entryTypes: ["longtask"] });
  } catch {
    // Not every engine ships the longtask entry type; the rAF delta above
    // already covers the visible symptom.
  }
}

// ---------------------------------------------------------------------------
// Public instrumentation points (no-ops when tracing is off)
// ---------------------------------------------------------------------------

/** Note something the app is about to do that has no React commit of its own —
 *  applying a theme, writing root CSS vars, loading a font. */
export function traceMark(label: string, detail?: string): void {
  if (!TRACE_ON) return;
  push(actions, { t: performance.now(), type: `mark:${label}`, ...(detail ? { about: detail } : {}) }, ACTION_RING);
  pendingActions.push(detail ? `mark:${label}(${detail})` : `mark:${label}`);
}

function describe(action: { type: string } & Record<string, unknown>): string | undefined {
  const parts: string[] = [];
  if (typeof action.threadId === "string") parts.push(action.threadId.slice(0, 8));
  if (typeof action.scope === "string") parts.push(action.scope);
  if (typeof action.projectId === "string") parts.push(`p:${action.projectId.slice(0, 6)}`);
  const ev = action.event as { kind?: string } | undefined;
  if (ev && typeof ev.kind === "string") parts.push(ev.kind);
  return parts.length ? parts.join(" ") : undefined;
}

/**
 * Wrap the store's dispatch so every commit can name the actions that caused
 * it. Returns the dispatch unchanged when tracing is off, so the identity that
 * App.tsx hands to context stays stable either way.
 */
export function traceDispatch<A extends { type: string }>(dispatch: (a: A) => void): (a: A) => void {
  if (!TRACE_ON) return dispatch;
  return (action: A) => {
    const about = describe(action as A & Record<string, unknown>);
    push(actions, { t: performance.now(), type: action.type, ...(about ? { about } : {}) }, ACTION_RING);
    pendingActions.push(about ? `${action.type}(${about})` : action.type);
    dispatch(action);
  };
}

interface StoreBinding {
  dispatch: (a: { type: string } & Record<string, unknown>) => void;
  getState: () => unknown;
}
let binding: StoreBinding | null = null;

/** App.tsx hands the live store over so `__tk.stress()` can drive the real
 *  reducer with synthetic traffic instead of burning agent turns. */
export function bindTraceStore<A extends { type: string }>(b: {
  dispatch: (a: A) => void;
  getState: () => unknown;
}): void {
  if (TRACE_ON) binding = b as unknown as StoreBinding;
}

// ---------------------------------------------------------------------------
// The console API
// ---------------------------------------------------------------------------

interface Report {
  enabled: true;
  uptimeMs: number;
  commits: number;
  incidents: Incident[];
  byKind: Record<string, number>;
  hotRenders: [string, number][];
  hotMounts: [string, number][];
  hotUnmounts: [string, number][];
  recentCommits: CommitRecord[];
  worstWalkMs: number;
  /** Repeats of a chatty kind that were throttled rather than recorded. */
  suppressed: Record<string, number>;
}

function report(limit = 40): Report {
  const byKind: Record<string, number> = {};
  for (const i of incidents) byKind[i.kind] = (byKind[i.kind] ?? 0) + 1;
  return {
    enabled: true,
    uptimeMs: Math.round(performance.now() - startedAt),
    commits: commits.length,
    incidents: incidents.slice(-limit),
    byKind,
    hotRenders: top(renderTotals, 20),
    hotMounts: top(mountTotals, 20),
    hotUnmounts: top(unmountTotals, 20),
    recentCommits: commits.slice(-8),
    worstWalkMs: commits.reduce((m, c) => Math.max(m, c.walkMs), 0),
    suppressed: Object.fromEntries([...suppressedByKind].filter(([, n]) => n > 0)),
  };
}

function clear(): void {
  actions.length = 0;
  commits.length = 0;
  incidents.length = 0;
  recentUnmounts.clear();
  renderTotals.clear();
  mountTotals.clear();
  unmountTotals.clear();
  startedAt = performance.now();
}

/**
 * Drive the real reducer with synthetic streaming traffic. The flash needs
 * several threads producing at once, which normally means several live agent
 * turns; this reproduces the same dispatch pressure through the same code path
 * for free, and deterministically.
 */
function stress(opts: { threadIds?: string[]; hz?: number; seconds?: number } = {}): Promise<string> {
  if (!binding) return Promise.resolve("no store bound — is tracing on?");
  const hz = opts.hz ?? 30;
  const seconds = opts.seconds ?? 10;
  const state = binding.getState() as {
    activeThreadId?: string | null;
    threads?: Record<string, { id: string }[]>;
  };
  const known = Object.values(state.threads ?? {}).flat().map((t) => t.id);
  const ids = opts.threadIds ?? [
    ...(state.activeThreadId ? [state.activeThreadId] : []),
    ...known.filter((id) => id !== state.activeThreadId).slice(0, 3),
  ];
  if (ids.length === 0) return Promise.resolve("no threads to stress — open a project first");

  const before = incidents.length;
  return new Promise<string>((resolve) => {
    let n = 0;
    const total = hz * seconds;
    const timer = setInterval(() => {
      const threadId = ids[n % ids.length] as string;
      binding?.dispatch({
        type: "agentEvent",
        threadId,
        seq: -1,
        timestamp: new Date().toISOString(),
        event: { kind: "assistant_delta", text: `stress ${n} ` },
      });
      if (++n >= total) {
        clearInterval(timer);
        resolve(
          `stressed ${ids.length} thread(s) at ${hz}Hz for ${seconds}s — ${incidents.length - before} incident(s); read __tk.report()`,
        );
      }
    }, Math.max(1, Math.round(1000 / hz)));
  });
}

let installed = false;

export function installRenderTrace(): void {
  if (installed) return;
  installed = true;
  if (!TRACE_ON) {
    // Leave a way to turn it on from a console that has no URL bar.
    (window as unknown as Record<string, unknown>).__tk = {
      enabled: false,
      on(): string {
        localStorage.setItem(LS_KEY, "1");
        return "render trace armed — reload the window";
      },
    };
    return;
  }
  startedAt = performance.now();
  bootUntil = startedAt + BOOT_QUIET_MS;
  installHook();
  const ready = () => {
    installDomWatch();
    installFrameWatch();
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready, { once: true });
  else ready();

  (window as unknown as Record<string, unknown>).__tk = {
    enabled: true,
    report,
    incidents: (n = 40) => incidents.slice(-n),
    /** Incidents mirrored to localStorage: survives a reload, a navigation
     *  away and back, and a webview restart. */
    persisted: (n = 250) => persisted().slice(-n),
    clearPersisted: () => {
      localStorage.removeItem(LS_LOG);
      return "persisted log cleared";
    },
    /** Everything the persisted log holds, as one console line. */
    dumpPersisted: () => {
      console.log(`[tk-persisted] ${JSON.stringify(persisted())}`);
      return "dumped";
    },
    commits: (n = 20) => commits.slice(-n),
    actions: (n = 40) => actions.slice(-n),
    hot: (n = 20) => ({ renders: top(renderTotals, n), mounts: top(mountTotals, n), unmounts: top(unmountTotals, n) }),
    clear,
    stress,
    /** The live store state. "What did state look like when it broke" is the
     *  question a commit log cannot answer on its own. */
    state: () => binding?.getState() ?? null,
    /** One JSON line on the console — the whole report, readable by anything
     *  that can only scrape console output. */
    dump: (n = 40) => {
      console.log(`[tk-report] ${JSON.stringify(report(n))}`);
      return "dumped";
    },
    off(): string {
      localStorage.removeItem(LS_KEY);
      return "render trace disarmed — reload the window";
    },
  };
  console.log("[tk-trace] render forensics armed — __tk.report() / __tk.dump() / __tk.stress()");
}

// Installed on import rather than from a call site: react-dom reads
// `__REACT_DEVTOOLS_GLOBAL_HOOK__` while its *own* module evaluates, so the
// hook has to exist before that import runs. main.tsx imports this file first
// for exactly this reason — keep it there, and keep it first.
installRenderTrace();

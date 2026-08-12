# Render forensics — catching the whole-screen flash

The long-running UI bug: with several threads streaming at once, the entire
window — sidebar, transcript, composer — flashes for a frame or two. It is
intermittent, it never survives long enough to screenshot, and an agent driving
the app can only see stills, so every previous attempt at it has been guesswork
about which component "probably" re-rendered.

`src/lib/renderTrace.ts` replaces the guessing with a recording. It is inert
until armed, costs ~0.2ms per commit when it is, and answers three questions
after the fact: **did the screen actually rebuild, what rebuilt it, and which
action caused that.**

## Arming it

```
http://localhost:42800/?tktrace=1        # sticks in localStorage for this origin
```

or from any console: `__tk.on()` then reload. `?tktrace=0` / `__tk.off()` turns
it off. It has to install before `react-dom` evaluates (React reads the DevTools
global hook while its own module runs), which is why `main.tsx` imports it
first — **keep that import first**.

It works in the shipped build, not only in `vite dev`: the flash happens in the
real Tauri app under real load, so the diagnostic has to live there. Two
consequences to know about:

- `dist/` must contain a build that includes it — `npm run build`. The running
  server serves `dist/` from disk, so a rebuild plus a reload is enough; the
  Rust binary does not need restarting.
- `vite.config.ts` sets `esbuild.keepNames: true`. Without it minification
  renames every component to two letters and a report reads `rc remounted`,
  which is worthless. Costs ~1% of bundle size and makes production stack traces
  readable too.

## What it records

A flash has four possible mechanisms and only one of them is a React
re-render, so there are four independent recorders:

| Recorder | Sees | Why it matters |
| --- | --- | --- |
| **Commits** (DevTools global hook) | every commit: which components ran, which mounted, which were deleted, and the dispatched actions since the last commit | a re-render is cheap; a *remount* throws DOM away and is the classic flash |
| **DOM** (MutationObserver) | `<style>`/`<link>` inserted into `<head>`, `<html>`'s `style`/`data-theme`/`data-family` rewritten, webfonts finishing a load | these repaint every pixel with **no React commit at all** — `applyAppearance()` and `ensureFontLoaded()` both do it |
| **Frames** (rAF + longtask) | frame deltas over 120ms, long tasks | a 200ms frame reads as a flash even when nothing was destroyed |
| **Anchors** (per frame) | the identity of `.app`, `.sidebar`, `.work-pane`, `.main-split`, `.thread-pane`, `.feed-scroll`, `.composer`, plus the transcript's scroll position | if one of these nodes is *replaced by a different node*, that IS the flash, and a scroll jump proves the user saw it |

Anything notable becomes an **incident**: a kind, a detail, the frame time, the
actions dispatched in the 500ms before it, and the surrounding commits. Every
incident also prints one greppable console line:

```
[tk-flash] remount — destroyed and rebuilt: ThreadView(412 fibers) | ... | frame=180ms | actions: agentEvent(c86ace16 assistant_delta), hello, ...
```

Incident kinds: `remount`, `mass-unmount`, `commit-storm`, `long-frame`,
`longtask`, `anchor-replaced`, `anchor-detached`, `scroll-reset`,
`stylesheet-added`/`-removed`, `root-attrs-changed`, `root-attrs-rewritten`,
`fonts-loaded`, `visibility`.

Two deliberate filters, so the report is signal rather than a log:

- Incidents in the first 2.5s carry `boot: true` and stay off the console —
  boot legitimately mounts everything and inserts every stylesheet.
- A `remount` is only raised when the destroyed subtree was ≥20 fibers. An icon
  swapping inside a row is a destroy-and-rebuild too, and cannot be what
  flashed.

## The console API

```js
__tk.report(n)     // everything: incident list, counts by kind, hottest components, recent commits
__tk.dump(n)       // the same report as ONE console line — for a client that can only scrape console output
__tk.incidents(n)  // just the incidents
__tk.commits(n)    // recent commits: rendered / mounts / unmounts / placements / causing actions
__tk.actions(n)    // recent dispatched actions
__tk.hot(n)        // lifetime render, mount and unmount counts per component
__tk.clear()       // reset the buffers (does not re-enter the boot-quiet window)
__tk.stress(opts)  // see below
```

`report().worstWalkMs` is the instrument watching itself: if it climbs above a
millisecond or two, the tracer is distorting what it measures and the numbers
should be read with suspicion.

## Reproducing on purpose

The flash needs several threads producing at once, which normally means several
live agent turns — slow, expensive, and non-deterministic. `__tk.stress()`
drives the **real reducer** with synthetic streaming events instead, through the
same dispatch path, for free:

```js
__tk.clear();
await __tk.stress({ hz: 40, seconds: 6 });   // → "stressed 4 thread(s) … — N incident(s)"
__tk.report();
```

It defaults to the open thread plus three others from the sidebar; pass
`{ threadIds: [...] }` to choose. The events are local-only — nothing is sent to
the server and no transcript is written.

## Driving it from an agent

The whole point is that a coding agent can read this without seeing the screen.
Threadknot serves the same UI over HTTP, so the browser MCP can be the observer:

1. `browser_navigate` → `http://localhost:42800/?tktrace=1`
   (append `&token=<the token from server.json>` once, for an unpaired profile).
2. Open a thread, then either reproduce by hand or call `__tk.stress()`.
3. `browser_console` → the `[tk-flash]` lines, or `browser_evaluate` →
   `JSON.stringify(__tk.report())` for the structured version.

## Reading a report

- **`anchor-replaced` / `remount` of a pane** — the tree was destroyed and
  rebuilt. Look at the incident's `actions`: something dispatched an action that
  changed a `key`, flipped a conditional branch, or re-created a parent element.
  This is a genuine flash.
- **`commit-storm` with a huge `hotRenders`** — nothing was destroyed; the app
  is re-rendering everything, repeatedly, faster than it paints. The store is a
  single `useReducer` in `App.tsx` behind one context, so **every** component
  that calls `useStore()` re-renders on **every** action, and `React.memo` does
  not stop it — a context change bypasses memo. Under four streaming threads a
  full transcript of `ToolRow`s re-renders on every token delta.
- **`root-attrs-rewritten` / `stylesheet-added` / `fonts-loaded` with no
  commits nearby** — React is innocent. Something re-applied the theme or
  loaded a font, and the browser repainted the document. Check the marked
  action: `mark:applyAppearance(<theme>)`.
- **`long-frame` with few commits** — the stall is not render work. Look for a
  synchronous parse (`react-markdown`, `highlight.js`) or a layout read.

## Instrumenting something new

- `traceMark(label, detail)` — for work with no React commit to attribute it to
  (theme applies, direct DOM writes). Already on `applyAppearance`.
- `traceDispatch(dispatch)` — already wrapping the store's dispatch in
  `App.tsx`; it is what lets a commit name its cause. Returns the dispatch
  unchanged when tracing is off, so the identity handed to context is stable
  either way.

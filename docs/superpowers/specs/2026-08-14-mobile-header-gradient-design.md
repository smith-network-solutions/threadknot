# Mobile thread header: relocate the project directory, float the header

**Date:** 2026-08-14
**Scope:** phone-width web UI (`max-width: 767px`), which is also what the Expo
WebView shell in `mobile/` renders. Desktop is unchanged.

## Problem

`.thread-head` (`src/components/ThreadView.tsx:1008`) stacks two lines on
phones — the thread title, and `.thread-project-path` beneath it. Two problems
converge at the top of the screen:

1. The header takes no `env(safe-area-inset-top)` padding. On a Dynamic Island
   phone (iPhone 14 Pro and every Pro since) the first line renders under the
   Island. Every phone shipping today has this cutout, so this is the default
   case, not an edge case.
2. The header is chrome: a solid `rgba(9,9,10,0.72)` fill, an 8px backdrop
   blur, and a hairline `border-bottom`. It reads as a title bar bolted above
   the conversation rather than part of it.

## Goals

- Get the project directory out of the header, without losing it.
- Give the header a background that fades out downward instead of ending at a
  hard rule: 60% opacity at the top, 0% at the bottom.
- Pad for the Island.

## Non-goals

- Desktop styling. The desktop header keeps its solid fill, blur, border, and
  its in-header project path.
- Quick threads. `.thread-head.quick-thread-head` already floats transparently
  over the feed with its own fixed metrics; it is explicitly excluded below.
- Any change to `mobile/`. The Expo shell hosts this same CSS; nothing in the
  native layer moves.

## Design

### 1. The project directory moves into the feed

Rendered as the first child of `.feed-inner` (`ThreadView.tsx:1275`), under the
same condition that guards the header copy today —
`project?.path && !hermesHome && !quickHome`:

```jsx
<div
  className="feed-project-caption"
  title={project.path}
  aria-label={`Project directory: ${project.path}`}
>
  {elidePathMiddle(project.path)}
</div>
```

`.thread-project-path` gets `display: none` at ≤767px; `.feed-project-caption`
gets `display: none` at ≥768px. Exactly one exists at any viewport, so assistive
tech never announces the directory twice.

Living in the scrollport is the whole point: it is visible on arrival and
scrolls away as you read, so it costs nothing at rest near the Island.

This makes one existing rule dead — `.thread-head.has-lanes
.thread-project-path` (`styles.css:8591`) exists solely to promote the path to
its own row on phones. It and its comment are removed.

### 2. Truncation keeps the head and the tail

The owner segment is load-bearing. A thread can be dispatched to another
harness or another machine (`docs/USING-DISPATCH.md`), so `/home/spencer/…`
answers "whose device is this running on" — which is a large part of why the
caption exists at all. That rules out collapsing `$HOME` to `~`, and rules out
a leading ellipsis that eats the username.

New helper in `src/lib/format.ts`:

```ts
/**
 * Shorten a path for display while preserving both ends: the leading segments
 * identify the machine and owner (a thread may be running on someone else's
 * box), and the final segment is the project. Only the middle is elided.
 *   /home/spencer/WebstormProjects/threadknot -> /home/spencer/…/threadknot
 */
export function elidePathMiddle(path: string, keepHead = 2, keepTail = 1): string
```

Segment-count based, not width-measured — deterministic, testable, and no
layout read. Paths short enough to survive intact are returned unchanged. The
untruncated path stays available via `title` and `aria-label`.

### 3. The header floats over the feed

```css
/* inside @media (max-width: 767px) */
.thread-head {
  position: absolute;
  inset: 0 0 auto;
  z-index: 20;
  padding: calc(10px + env(safe-area-inset-top)) 12px 10px;
  background: linear-gradient(
    180deg,
    color-mix(in srgb, var(--panel) 60%, transparent),
    transparent
  ) !important;
  backdrop-filter: none !important;
  border-bottom: 0 !important;
  pointer-events: none;
}
.thread-head > * { pointer-events: auto; }
```

Three deliberate choices:

- **`color-mix(… var(--panel) …)` rather than a literal `rgba()`.** `--panel`
  is already declared per palette (`#090909` dark, `#ffffff` light, solar's
  warm white), so one rule is correct in every theme.
- **`!important` is load-bearing, not laziness.** `:root[data-theme="solar"]
  .thread-head` (`styles.css:9983`) and `:root[data-theme="arcade"]
  .thread-head` (`10151`) both set `background` and sit *after* this media
  query in source order, at higher specificity. Without `!important` the
  gradient silently loses on two of five themes. `.thread-head.quick-thread-head`
  already carries `!important` on these same three properties for exactly this
  reason; this follows that precedent. Arcade's `::after` scanline overlay is
  suppressed on mobile the same way the quick head suppresses it.
- **`pointer-events` is required, not defensive.** Once absolute, the header's
  box spans the full pane width and its lower half is fully transparent. Left
  alone it would swallow taps on the top of the feed — an invisible dead zone.

### 4. Feed clearance is measured

Header height is variable: an ordinary thread is one row, a `has-lanes` thread
wraps to as many as four. A `ResizeObserver` in `ThreadView` writes the observed
height to a `--head-h` custom property on the `.thread-pane` element:

```css
.thread-pane:not(:has(> .quick-thread-head)) .feed-inner {
  padding-top: calc(var(--head-h, 54px) + 12px);
}
```

The observer writes through a ref via `style.setProperty`, so it drives no React
re-render — relevant because `ThreadView` is a component `docs/RENDER-FORENSICS.md`
watches. The `54px` fallback covers first paint before the observer fires. The
`:not()` leaves quick threads on their existing fixed `108px`.

The alternative — fixed `min-height` plus a matching `padding-top`, as the quick
head does — was rejected: it needs a second pair of numbers for `.has-lanes`,
and both pairs drift the moment header content changes.

## Files touched

| File | Change |
| --- | --- |
| `src/lib/format.ts` | add `elidePathMiddle` |
| `src/components/ThreadView.tsx` | render `.feed-project-caption`; `ResizeObserver` → `--head-h` |
| `src/styles.css` | mobile `.thread-head` gradient/float/safe-area; caption styles; hide each copy at the other breakpoint; delete the dead `has-lanes` path rule |

## Testing

- Unit: `elidePathMiddle` — long path elides the middle, short path is
  untouched, a path with fewer segments than `keepHead + keepTail` is
  untouched, trailing slash, root `/`, and a Windows-style path pass through
  without mangling.
- Build gate: `npm run build`. No Rust changes, so `cargo build && cargo clippy`
  should be a no-op — confirm it is (two builds in a row at ~0.2s, per
  `CLAUDE.md`).
- Visual, at 390×844 against the running app's LAN URL in a headless browser:
  the caption renders at the top of the feed and scrolls away; the header fades
  rather than ending in a rule; a tap in the transparent lower band lands on the
  feed, not the header; a `has-lanes` thread clears its taller header; the first
  message is not hidden under the Island. Repeat on the light and arcade themes
  to confirm the `!important` gradient holds.

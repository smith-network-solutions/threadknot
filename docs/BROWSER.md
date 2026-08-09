# Threadknot driven browser

Threadknot's browser is one isolated Chrome session per thread, jointly
controlled by the agent and the user. Chrome runs on the machine that owns the
thread; a chat opened from a peer reaches it through a spliced socket. It is not a preview iframe and it is not
an invisible automation browser with a separate viewer layered on afterward.
The agent's MCP tools and the Browser workspace operate on the same CDP pages.

This document records the product contract and the external systems studied for
the 2026-07-25 browser rebuild.

## Product contract

1. **Shared reality.** Every navigation, tab switch, pointer action, form edit,
   dialog, and failure the agent causes must be reflected in the human surface.
2. **Semantic first, pixels available.** Agents receive a compact accessibility
   tree with stable document-scoped refs for ordinary control, plus a screenshot
   for spatial or visual reasoning.
3. **Deterministic actions.** Ref/selector actions resolve through CDP, scroll
   into view, expose their target to the user, execute once, and get a short
   bounded settling window.
4. **Human legibility.** Agent actions have started/targeting/completed/failed
   phases, a visible pointer, a target outline, a current-action HUD, and a
   reconnect-safe activity trail.
5. **Human takeover.** The canvas forwards hover, click, drag, wheel, keyboard
   chords, navigation, tabs, and dialogs to the same pages. Agent controls do
   not put a glass pane over the browser.
6. **Durable while useful, disposable by design.** Changing workspace views or
   disconnecting a phone preserves the in-memory session. Explicit Restart,
   thread/server replacement, or a dead Chrome process replaces it. Browser
   profiles are isolated temporary directories, not the user's daily profile.

## What we adopted

| System studied | Proven idea kept in Threadknot | What Threadknot deliberately did not copy |
|---|---|---|
| [Microsoft Playwright MCP](https://github.com/microsoft/playwright-mcp) | Accessibility snapshots with action refs; semantic tools before screenshots; bounded waits; tabs, dialogs, files, console, and network as first-class controls; post-action observation option. | A Node/Playwright sidecar and its full generic tool surface. Threadknot stays in-process Rust/CDP and advertises a smaller cohesive catalog. |
| [Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp) and its [design principles](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/main/docs/design-principles.md) | Agent-agnostic MCP, compact human-readable results, stable backend-node identity, deterministic small actions, actionable stale-state errors, and progressive escape hatches. | Dumping raw CDP or large protocol objects into context; performance/debug domains that are not yet part of Threadknot's browser product. |
| [Browser Use](https://github.com/browser-use/browser-use) | Highlighted interaction targets, visible cursor motion, ordered action history, session continuity, and DOM state mapped to interaction indexes. | A vision-first control loop, a second model/orchestrator, cloud-browser coupling, and Python runtime ownership. |
| [Stagehand](https://github.com/browserbase/stagehand) | The separation between deterministic primitives and higher-level observe/act workflows; previewable actions and self-healing state as product direction. | A hosted Browserbase dependency or another JavaScript automation service. Provider agents already supply reasoning; Threadknot supplies the reliable browser substrate. |

The skill ecosystem was checked as an adoption signal rather than treated as a
quality guarantee. At research time, Browser Use's official browser skill was
the clear high-install leader on skills.sh, while the official Chrome DevTools
skill and Microsoft's Playwright MCP had substantial real-world use and active
upstream maintenance. Generic repackaged browser skills were not adopted merely
because they appeared in search results.

## Architecture

```text
Claude / Codex
      │ thread bearer token
      ▼
  POST /mcp ── browser_* ──┐
                            │
                      BrowserRegistry
                      session = thread id
                            │
                      chromiumoxide/CDP
                            │
       ┌────────────────────┴────────────────────┐
       │ same pages, tabs, profile, viewport     │
       ▼                                         ▼
semantic AX tree + PNG                 JPEG screencast + JSON
stable eN backend refs                 GET /browser WebSocket
       │                                         │
       └──────────── agent actions ──────────────┤
                                                 ▼
                                      Browser workspace / phone
                                      cursor + target + activity
                                      human mouse/key/tab takeover
```

`BrowserRegistry` serializes agent actions per session. Human input remains
live so a user can intentionally intervene. All attached viewers receive the
same frame/nav/tab/dialog/activity broadcasts.

### The snapshot is a tree, and it includes frames

The snapshot is built by walking `childIds` from each frame's root, not by
iterating the flat node array: nesting is load-bearing, because "the Delete
button in the INV-002 row" is only expressible if the button sits under its
row. Nodes that carry no meaning (wrapper `generic` divs, layout table cells,
ignored nodes) are *flattened* — their children are still walked, at the parent
depth — rather than dropped, which would hide real content. `InlineTextBox`
nodes are dropped outright: their parent `StaticText` already carries the words,
and emitting them turned "Skip to content" into thirteen lines. Static text
already spoken by an enclosing control, and pure separators, are dropped too.

Every frame contributes to one snapshot and one ref space. Cross-origin frames
stay in-process (`--disable-features=IsolateOrigins,site-per-process`) so a
single CDP session can read and drive them; checkout fields, embedded auth and
third-party widgets are part of the flow, not a blind spot. CSS selectors that
miss in the main document are retried against every child frame's document.

Interactive accessibility nodes receive refs (`e1`, `e2`, …) backed by Chrome
backend node ids. Consecutive snapshots in one document retain refs where those
backend ids survive. Navigation, reload, re-navigating to the same URL, and
viewport changes all clear the map. A missing ref produces an explicit request
for a fresh snapshot — and a raw CDP node failure ("Node does not have a layout
object") is rewritten into the same instruction, never surfaced as-is and never
answered with a fallback click on a guessed node.

Page targets are wired lazily. Popups and `_blank` links activate automatically;
background tab streams are stopped, and selecting a tab starts its stream after
reapplying the shared device viewport. Page-owned console, network, navigation,
and dialog events affect the shared UI only while that page is active.

## Tool shape

The MCP catalog groups into:

- observe: `browser_snapshot`, `browser_status`, `browser_console`,
  `browser_network`, `browser_network_body`, `browser_tabs`,
  `browser_downloads`
- evidence: `browser_screenshot` (viewport, full page, or one element; writes a
  PNG to disk so `publish_artifact` can deliver it)
- navigate/lifecycle: `browser_navigate`, `browser_back`,
  `browser_forward`, `browser_reload`, `browser_new_tab`,
  `browser_switch_tab`, `browser_close_tab`, `browser_resize`
- pointer: `browser_click`, `browser_hover`, `browser_drag`,
  `browser_scroll`
- forms/input: `browser_fill`, `browser_fill_form`, `browser_select`,
  `browser_check`, `browser_type`, `browser_press`, `browser_upload`
- synchronization/escape hatch: `browser_wait_for`,
  `browser_handle_dialog`, `browser_evaluate`
- recording: `browser_record_flow` (storyboard → narrated MP4),
  `browser_record_start` / `browser_record_stop` (ad-hoc capture)

Refs are preferred, selectors are the deterministic fallback, and coordinates
are the visual fallback. Mutating actions accept `includeSnapshot` where a
follow-up observation is useful. Fill/type values are never placed in the
human-facing activity stream.

Four input details decide whether real flows work at all:

- **Keys carry `text`.** Chrome only performs a key's *default action* when the
  keydown carries text, so `Enter` without `"\r"` fires a keydown and submits
  nothing — a silent failure the tool would report as success. Enter, Tab and
  every printable key now carry it.
- **Clicks must reach their target.** Before a ref/selector click, the element's
  own document is asked what is actually at that point; if a cookie banner,
  modal backdrop or sticky header is in the way, the click is refused and names
  the blocker instead of being swallowed. Coordinate clicks skip the check,
  since those state their own target.
- **Typing is per-key.** `browser_type` dispatches a real keydown/keyup per
  character rather than `Input.insertText`, so search-as-you-type, hotkeys,
  maxlength filters and editors behave as they do for a human. `fast: true`
  restores the single-shot insert for long strings.
- **Paste crosses the process boundary explicitly.** The Browser canvas catches
  the host's Ctrl/Cmd+V paste event and sends its plain-text payload over the
  browser socket. Forwarding only the shortcut cannot work because isolated
  headless Chrome does not share the viewer's system clipboard. On phones, the
  clipboard button reads through the native mobile bridge; an editable manual
  sheet is the fallback for LAN browsers and denied clipboard permissions. The
  socket advertises native paste support so a newer viewer connected to an
  older machine can fall back to the established per-key transport.

Observation-only tools (`status`, `console`, `network`, `tabs`, `downloads`)
never launch Chrome: asking whether a browser is open must not open one.

## Recorded walkthroughs

`browser_record_flow` produces a Scribe-style training video from a storyboard
in **one** call. That single-call shape is the whole point: driving
act → screenshot → act over MCP paces the result at model latency, which yields
a slideshow rather than a demonstration. The storyboard is described up front so
playback runs on timings chosen for a viewer.

Three problems had to be solved, and each explains a piece of the design.

**The screencast is variable rate.** CDP emits a frame on repaint and nothing
while the page is idle, so a 900ms deliberate pause is indistinguishable from a
dropped frame. `recorder.rs` resamples: it subscribes to the session's frame
broadcast, holds the newest JPEG, and a 30fps ticker writes it to ffmpeg once per
output frame — repeating through idle stretches. Frames go over stdin as MJPEG,
so Threadknot never decodes a JPEG; ffmpeg does it once on the way to H.264.

**There is no cursor to record.** Headless Chrome renders no pointer for
synthetic input. `browser_overlay.js` is injected via
`Page.addScriptToEvaluateOnNewDocument` and draws the pointer, click ripples,
captions and spotlight inside a shadow root, `pointer-events:none` so it can
never intercept the input being dispatched underneath. A page with strict CSP
may block the injection; that degrades to an un-annotated recording rather than
a failure.

**Motion was teleport.** `glide()` walks an eased cubic path. The overlay
animates the *visible* pointer locally at display rate while Rust dispatches real
CDP moves along the same curve every 40ms — the page needs enough samples to fire
hover states, not enough for smooth motion, and an evaluate per display frame
would buy nothing. Typing is jittered around a mean with longer rests after
spaces and punctuation; an exact metronome reads as synthetic.

Pacing knobs (`moveMs`, `dwellMs`, `keyMs`, `readMs`) apply per flow and can be
overridden per step. `readMs` is the beat between a caption appearing and its
action running, so a viewer reads the instruction before watching it happen.

> **Gotcha:** a fresh document gets a fresh overlay, and it starts hidden. Any
> step that changes document must re-assert it (`reassert_presentation`) — not
> just `navigate`, but a `click` on a link and a `type` with `submit` too.
> Captions mask the bug because every step sets its own; the pointer just
> silently disappears for the rest of the recording.

## Signed-in profiles

By default every thread gets a **disposable** browser: a fresh temporary
profile, discarded with the session. A thread can instead attach a **signed-in
profile** — a durable Chrome user-data directory under
`~/.threadknot/browser/profiles/<id>`, listed in `browser-profiles.json`.

Threadknot stores the **session**, never the credentials. The human signs in by hand
in the Browser pane (which already supports full takeover, so password managers,
one-time codes and passkey prompts all work); what persists afterwards is
whatever Chrome kept — cookies, localStorage, IndexedDB. Chrome's own password
manager stays off, and no agent ever sees a secret being typed.

A signed-in browser can act as its owner, so it is fenced in:

| Control | Where it is enforced |
|---|---|
| **Origin scope** — a profile may only load documents from the sites it was created for. Scoping is optional: no sites (stored as `*`) means any http/https site, but never non-web schemes like `file://` | In the browser: `Fetch` interception fails off-scope document loads with `BlockedByClient`, so a link, redirect, `window.open` or injected script cannot escape either. Tool calls also refuse early, with a reason. |
| **No arbitrary JavaScript** — `browser_evaluate` is refused while signed in | It is the one tool that could read the page and act cross-origin at will; the semantic tools cover ordinary work. |
| **Site isolation stays on** | Disposable profiles trade it for cross-origin iframe reach. A profile holding a real login keeps Chrome's cross-site boundary; its cross-origin frames are reported as hidden rather than silently omitted. |
| **Master principal only** | `GET /browser` refuses a signed-in session to a paired-device credential: a shared LAN link must not put someone in front of a logged-in account. |
| **Master token to manage** | `browser.profile.create/update/delete` require this machine's own token. A phone can drive a session the owner set up; it cannot widen or create one. |
| **One chat at a time** | Chrome will not open a profile twice, so a second thread is told which thread holds it instead of hanging on a lock. |
| **Machine-bound** | A session belongs to the machine whose Chrome holds it and is never synced across the mesh. It is still *reachable* from anywhere: a chat on a peer opens that peer's browser through a spliced `/browser` socket, and `browser.profile.*` routes to the owning machine, so the owner can create a login and sign it in without walking over to the machine. Only a local master may splice — a paired phone cannot launder its own refusal through a machine it is paired with. |

Scope changes and deletion close any live session on that profile first —
deletion erases the directory, so "sign out" really signs out.

Chrome only writes its cookie jar on a clean exit, so sessions are closed with
`Browser.close`, not a kill — including on app exit (a Tauri `RunEvent::Exit`
hook and the headless binary's ctrl-c path both drain the registry). When the
app dies without that hook (SIGKILL from `scripts/restart.sh`, a crash), the
signed-in Chrome survives as an orphan still holding the profile's
`SingletonLock`; the next startup finds it, SIGTERMs it so it flushes and
releases the profile, and only force-kills a hung one. Session-only cookies —
which Chrome normally deletes on its next launch — are kept by setting
`session.restore_on_startup = 1` ("continue where you left off") in the
profile's Preferences before each launch, so logins that live in session
cookies survive a restart too.

**At rest**, the profile directory holds Chrome's own storage, and chromiumoxide
launches Chrome with `--password-store=basic` — whose cookie key is a fixed
constant. Its real protection is `~/.threadknot` being `0700`, which is the trust
model Threadknot already runs on: `server.json` holds the master token in plaintext,
and the `claude`/`codex` logins sit unencrypted in the home directory. Anyone who
can read the home directory already holds all of those. Encrypting the profile
under an OS-keyring key would defend a *stolen disk* specifically; it is not yet
implemented, and it would not defend a live compromised machine.

## Security boundary

Browser agents still face prompt injection; visual transparency is not a proof
that page instructions are safe. The design follows the practical guidance in
OpenAI's [agent prompt-injection defenses](https://openai.com/index/designing-agents-to-resist-prompt-injection/):
keep the agent's data sources and action sinks explicit, minimize ambient
authority, and preserve human visibility for consequential actions.

- Chrome never attaches to the user's normal browser profile or cookies; a
  signed-in profile is one Threadknot created and scoped, never the user's daily one.
- Each thread gets a unique temporary profile, removed when its session drops,
  and profiles orphaned by a kill or crash are swept at startup (skipping any
  directory a live Chrome still holds). Startup also terminates headless Chromes
  whose Threadknot is gone — a SIGKILLed app re-parents its browsers to init or a
  subreaper, where they would otherwise run until reboot — identified by a
  parent that is no longer a live Threadknot, so a second running Threadknot's browsers
  are never touched. Disposable orphans are killed and their directories
  deleted; signed-in orphans are SIGTERMed (flush + lock release) and their
  directories are never deleted — they are the stored session.
- Upload canonicalizes every path and rejects anything outside the thread's
  project root; `browser_screenshot` applies the same rule to its destination,
  and defaults to the session's own directory.
- Site isolation is disabled *inside disposable browsers* so one CDP session can
  drive cross-origin frames. That protection exists to stop one site reading
  another site's logged-in data, and a disposable profile holds none — so
  signed-in profiles keep it on and give up cross-origin frame reach instead.
- Provider access/approval modes remain the authority for non-browser tools.
- Browser text entry is redacted from the activity feed. It remains visible in
  the page itself, as it would in an ordinary shared screen.
- The master UI token and per-thread MCP bearer tokens are never sent to page
  JavaScript.

Chrome's own [remote debugging security change](https://developer.chrome.com/blog/remote-debugging-port)
is additional support for the isolated-profile decision: automation should not
point remote debugging at a user's default data directory.

## Verification

Fast unit tests cover URL normalization, Unicode-safe truncation, keyboard and
mouse mappings, drag target mapping, snapshot node classification (emit /
flatten / drop), key default-action text, stale-error rewriting, and
activity-feed redaction.

Two ignored tests launch a real Chrome/Chromium.
`live_chrome_semantic_action_round_trip` verifies navigation, snapshot
production, ref-based fill/check/click, screenshot capture, JavaScript state,
the new/switch/close tab lifecycle, and automatic `_blank` popup activation.

`live_chrome_real_flow_capabilities` covers the things that make or break a real
user flow, each of which was silently broken before:

- typing then Enter actually **submits** the form
- typing fires one real keydown per character
- an iframe's contents appear in the snapshot, get refs, and resolve by selector
- table rows survive, so repeated "Delete" buttons nest under the row they act on
- no per-character `InlineTextBox` noise
- `browser_screenshot` writes a real PNG file
- a failed resource load reaches `browser_console`, not only the network log
- a ref used after a reload fails with an instruction, not a raw CDP string
- a click an overlay would swallow is refused, while ordinary nested markup
  (a button wrapping a span, an input inside its label) still clicks
- the current page's own document request survives its navigation in the
  network log, while the previous page's requests do not linger

Run them with:

```bash
cargo test live_chrome --lib -- --ignored --nocapture
```

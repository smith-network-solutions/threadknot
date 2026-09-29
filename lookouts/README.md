# Lookouts

A lookout is a small program that tells a Bosun what happened since it last
looked. Threadknot runs it on a timer (a `command` lookout, min 30 s), reads
what it prints, and hands new signals to the Bosun's triage. Full spec:
[`docs/BOSUN.md`](../docs/BOSUN.md) §4.

## The contract

**In:** environment variables.

| Var | Meaning |
| --- | --- |
| `BOSUN_WATERMARK` | Whatever your last run printed as its watermark. Empty on the first run. |
| `BOSUN_STATE_DIR` | A directory that already exists and belongs to this lookout. Cache tokens here. |
| `BOSUN_ID`, `BOSUN_NAME`, `LOOKOUT_ID`, `LOOKOUT_NAME` | Who is asking. |
| anything in the lookout's `env` map | Your own config (API keys, filters). |

**Out:** NDJSON on stdout, one JSON object per line. A line is either a
signal or a watermark:

```json
{"id": "acme:ticket:42:2026-09-28T15:02:11Z", "kind": "ticket", "title": "Login broken", "body": "...", "observedAt": "2026-09-28T15:02:11Z", "refs": {"url": "https://..."}, "hints": {"workspace": "Acme"}}
{"watermark": "2026-09-28T15:02:11Z"}
```

- Only `title` is required. Leave out `id` and Threadknot hashes the lookout id, title and body.
- Make `id` deterministic. The engine drops ids it has already seen, so re-emitting a signal is harmless and crash-safe.
- `refs` values must be strings.
- The **last** `watermark` line wins. It is opaque to Threadknot: a timestamp, a cursor, a history id, anything you can resume from.
- Print nothing when nothing happened. An empty run costs nothing: no wake, no ledger line.
- On the first run (empty watermark), print only a watermark. Otherwise switching the lookout on floods the Bosun with the whole backlog.
- Exit 0 on success. A non-zero exit records `last_error` and **keeps the old watermark**, so the next run retries; signals already printed are still queued. Use it for auth and network failures, not for "nothing new".
- stderr is for humans (the test dialog shows it). Never print secrets there.
- You have 120 s.

## A lookout in ten lines

Signals each new file in `~/Drop` (a `folder` lookout does this natively; it
is only here to show the shape):

```bash
#!/usr/bin/env bash
# drop.sh: one signal per file added to ~/Drop since the last run.
set -euo pipefail
now=$(date +%s)
[ -z "${BOSUN_WATERMARK:-}" ] && { jq -nc --arg w "$now" '{watermark:$w}'; exit 0; }
find ~/Drop -maxdepth 1 -type f -newermt "@$BOSUN_WATERMARK" ! -newermt "@$now" -print0 |
  while IFS= read -r -d '' f; do
    jq -nc --arg f "$f" --rawfile b "$f" '{id:("drop:"+$f), kind:"file", title:($f|split("/")[-1]), body:$b[0:4000], refs:{path:$f}}'
  done
jq -nc --arg w "$now" '{watermark:$w}'
```

## Testing one by hand

```bash
BOSUN_WATERMARK= ./lookouts/gmail-gws.sh | jq                 # first run: just a watermark
BOSUN_WATERMARK=881009 ./lookouts/gmail-gws.sh | jq -c '{id,title}'
BOSUN_WATERMARK=2026-09-26T00:00:00Z BOSUN_STATE_DIR=$(mktemp -d) ./lookouts/orbit-tasks.py | jq
BOSUN_STATE_DIR=$(mktemp -d) ./lookouts/orbit-tasks.py --sweep --now | jq -r .body   # sweep, ignoring the window
BOSUN_STATE_DIR=$(mktemp -d) ./lookouts/gmail-gws.sh --sweep --now | jq -r .body
./lookouts/gmail-gws.sh --selftest                            # offline checks
./lookouts/orbit-tasks.py --selftest
./lookouts/test-lookouts.sh                                   # both, live, read-only
./lookouts/test-lookouts.sh --offline                         # selftests only
```

`BOSUN_STATE_DIR` is optional when testing by hand (`orbit-tasks.py` falls
back to a temp dir; `gmail-gws.sh` uses it only for the sweep's `snooze.json`
and falls back to `$TMPDIR/gmail-gws-sweep`).

## Bundled lookouts

### `gmail-gws.sh`: new inbox mail

Needs `gws` (Google Workspace CLI, authenticated for the account) and `jq`.
Watermark: a Gmail `historyId`. First run prints the profile's current
`historyId`. Later runs call `users.history.list` from it and signal each
added message still carrying the label. If the history id is too old (Gmail
keeps roughly a week), it re-seeds from the profile and says so on stderr.

| Env | Default | Meaning |
| --- | --- | --- |
| `GOOGLE_WORKSPACE_CLI_CONFIG_DIR` | `~/.config/gws` | gws config for the mailbox to watch. |
| `GMAIL_LABEL` | `INBOX` | Label to watch. |
| `GMAIL_ALLOW_DOMAINS_FILE` | none | One domain per line, `#` comments. Bulk mail (a `List-Unsubscribe` header, or `Precedence: bulk/list/junk`) is dropped unless the sender's domain or a parent domain is listed. Without the file, all bulk mail is dropped. |
| `GMAIL_FULL_BODY` | unset | `1` puts the decoded `text/plain` part (max 8000 chars) in the body instead of Gmail's snippet. One extra API call per message. |
| `GMAIL_MAX_MESSAGES` | `100` | Per-run cap. When hit, the watermark stops at the last message handled and the next run continues. |

Also skipped: mail from the account itself, drafts, sent, spam, trash, and
messages that left the label before the run. Read-only (`getProfile`,
`history.list`, `messages.get`). Exits 2 on auth failure, 1 on other API or
network failure.

```json
{"id":"gmail:1a0e973643a9d5b0","kind":"email","title":"Build failed for g5 — Railway","body":"From: Railway <hello@notify.railway.app>\nTo: spencer@smithnetworksolutions.com\nDate: Mon, 28 Sep 2026 19:17:27 +0000\n\nBuild failed for project: g5 ...","observedAt":"2026-09-28T15:17:27-04:00","refs":{"url":"https://mail.google.com/mail/u/0/#inbox/1a0e973643a9d5b0","from":"hello@notify.railway.app","threadId":"1a0e973643a9d5b0","messageId":"1a0e973643a9d5b0"}}
```

The link uses `/mail/u/0/`, the first signed-in Google account in the browser.
If the watched mailbox is not account 0 there, the link opens the wrong inbox.

### `orbit-tasks.py`: Orbit tasks created or updated

Python 3 standard library only. Watermark: the newest task `updatedAt` seen
(ISO 8601). First run prints the org's newest `updatedAt` and no signals.
Later runs list tasks sorted by `updatedAt` descending and stop paging at the
watermark. A task gets one signal per update (the id includes `updatedAt`).

| Env | Default | Meaning |
| --- | --- | --- |
| `ORBIT_API_URL` | required | e.g. `https://orbit.servicestorm.io` |
| `ORBIT_API_KEY` | required | Agent API key; exchanged for a 15-minute access token. |
| `ORBIT_ORG_ID` | required | Org to watch. |
| `ORBIT_ORG_SLUG` | required | Used in task links, e.g. `oscar-edge-817a467`. |
| `ORBIT_WORKSPACE_HINT` | none | Copied to `hints.workspace` on every signal. |
| `ORBIT_INCLUDE_STATUSES` | all but `done`, `cancelled` | Comma list of statuses to signal. Orbit orgs have custom statuses (`awaiting_response`, `client_inbound_lead`, ...), so the default is an exclusion. |
| `ORBIT_WEB_URL` | `ORBIT_API_URL` | Base for task links, if the web app lives elsewhere. |
| `ORBIT_MAX_SIGNALS` | `50` | Per-run cap, oldest first; the watermark stops at the last one emitted. |

Body: status, priority, due date, client, assignee, project, ticket number,
Orbit's last-activity summary, the description (max 8000 chars) and the
latest comment or reply (max 2000 chars, marked internal when it is).
`BOSUN_STATE_DIR` holds `token.json` (mode 0600; refreshed on 401),
`members.json` and `clients.json` (name caches, refreshed daily). GET only.
Exits 2 on auth failure, 1 on other API or network failure.

```json
{"id":"orbit:task:cmukodblz1mt5emwekgo1az3r:2026-09-28T03:15:47.532Z","kind":"ticket","title":"New link for form","body":"Status: todo\nPriority: medium\nDue: 2026-09-29\nClient: Aletheia Integrative\nAssignee: unassigned\nTicket: #101324\nLast activity: New ticket from melissa@aletheia.md","observedAt":"2026-09-28T03:15:47.532Z","refs":{"url":"https://orbit.servicestorm.io/oscar-edge-817a467/tasks/cmukodblz1mt5emwekgo1az3r","org":"oscar-edge-817a467","taskId":"cmukodblz1mt5emwekgo1az3r","client":"Aletheia Integrative"},"hints":{"workspace":"Service Storm"}}
```

One lookout per org. For both of Spencer's orgs, add two lookouts with
different `ORBIT_ORG_ID`, `ORBIT_ORG_SLUG` and `ORBIT_WORKSPACE_HINT`.

### Sweep mode: what is still waiting on you

`orbit-tasks.py --sweep` and `gmail-gws.sh --sweep` are a second, model-free
kind of lookout: instead of "what changed since the watermark" they rebuild
the list of items still waiting on the person and nag about it. Point a
separate `command` lookout at the same script with `--sweep` in its args
(an interval of 1-2 h suits it).

- **No watermark.** A sweep never prints a watermark line.
- **One signal per run, only when the list is non-empty.** An empty list, or
  a run outside the work window, prints nothing and exits 0.
- **The id changes every hour** (`orbit:sweep:<org slug>:<YYYY-MM-DDTHH>`,
  `gmail:sweep:<account>:<YYYY-MM-DDTHH>`, local time), so the engine's dedupe
  lets one sweep through per hour while anything stays open.
- `kind: "sweep"`; `refs` = `url`, `org` or `account`, `snoozeFile`, `count`;
  `hints.workspace` from `ORBIT_WORKSPACE_HINT` / `GMAIL_WORKSPACE_HINT`.
- The body is grouped Markdown, one line per item ending in `(id …)`, capped
  at 32 KiB, with a footer of per-group counts, the snoozed count, and the
  snooze instruction.

| Env | Default | Meaning |
| --- | --- | --- |
| `SWEEP_HOURS` | `08:00-18:00` | Local work window. The end is exclusive; `22:00-06:00` wraps midnight. |
| `SWEEP_DAYS` | `mon-fri` | Days the window applies: a range (`fri-mon` wraps), a list (`mon,wed`), or `all`. |
| `SWEEP_ALWAYS` | unset | `1` ignores the window. `--sweep --now` sets it. |

**Snooze contract.** `$BOSUN_STATE_DIR/snooze.json` is a JSON object mapping
an item id (Orbit task id, Gmail thread id) to an ISO date or datetime:

```json
{ "cmtm7ibbz01zxemmr7p9ka6a0": "2026-09-29", "1a0a5cb3e9add4a9": "2026-09-30T13:00:00-04:00" }
```

An item snoozed until a moment still in the future is left out of the list and
counted as "snoozed N" in the footer. A bare date means local midnight at the
start of that day; a datetime without an offset is local time. Expired (and
unparseable) entries are pruned whenever the sweep reads the file. The agent
writes it; the absolute path is in `refs.snoozeFile`.

**Orbit groups** (each open task goes in the first that matches; everything
else is not listed). Tasks whose title contains an `ORBIT_SWEEP_IGNORE`
substring, archived tasks, and `done`/`cancelled` are skipped.

1. *Client replied, nobody answered*: `status` is `response_received`, or
   `lastActivityType` is `EXTERNAL_REPLY`/`NEW_INBOUND` (summaries like
   "Reply from x@y" / "New ticket from x@y"), **and** the newest task email
   (`/tasks/{id}/emails`, `direction`) is `inbound` with no later non-internal
   staff comment. A newest `outbound` email, or a public staff comment after
   the last inbound email, counts as answered and the task falls through.
2. *Overdue*: `dueDate` before today (local).
3. *Nudged, not acknowledged*: `nudgedAt` set and `acknowledgedNudge` falsy.
   Both fields (and `taskInboxes`) exist only on the single-task GET, so the
   sweep fetches each open task once (6 in parallel).
4. *Unassigned*: no `assigneeId` and no `assignee` in `assignments`.
5. *Mine and stale*: assigned to you and `lastActivityAt` (or `updatedAt`)
   older than `ORBIT_STALE_DAYS`.

Line: `- [#ticket] title — client · status · due · assignee · last activity · <url> (id <taskId>)`.

| Env | Default | Meaning |
| --- | --- | --- |
| `ORBIT_ME_EMAIL` | `spencer@smithnetworksolutions.com` | Comma list of your Orbit member emails (or userIds), resolved through the members list. Spencer's Orbit login is his personal Gmail in both orgs, so set it to that; unresolved, "Mine and stale" stays empty and stderr says so. |
| `ORBIT_STALE_DAYS` | `3` | Days without activity before a task of yours counts as stale. |
| `ORBIT_SWEEP_IGNORE` | `Marketing Touchpoint` | Comma list of title substrings (case-insensitive) to skip. |

**Gmail groups.** Inbox threads from the last `GMAIL_SWEEP_DAYS` whose newest
message (drafts, spam, trash ignored) is from someone other than the account
and older than `GMAIL_SWEEP_MIN_AGE_HOURS`. Skipped: bulk mail (the same rule
as poll mode), auto-replies (`Auto-Submitted: auto-replied`, `X-Autoreply`,
"Out of office"/"Automatic reply" subjects), calendar mail (Google Calendar
sender, `Invitation:`/`Accepted:`/… subjects, Outlook `calendarmessage`), and
threads where the account wrote last.

1. *Waiting on you*: a person (not a machine sender) at an allow-listed
   domain, or a person with the account in `To`/`Cc`. Machine sender = a
   `noreply`/`notifications`/`alerts`/… local part, a `notify.`/`mail.`/`send.`
   … subdomain, `Auto-Submitted: auto-generated`, or (outside allow-listed
   domains) a Gmail category other than Primary.
2. *Probably a task*: anything else whose subject or snippet reads like work:
   a form submission or new lead, a failed build/crawl/payment, a security
   alert or sign-in, a signature request (DocuSign, Dropbox Sign), "action
   required", past due, expiring, run payroll.

Line: `- <age>h · <name> <address> · <subject> — <snippet> · https://mail.google.com/mail/u/0/#inbox/<threadId> (id <threadId>)`.

| Env | Default | Meaning |
| --- | --- | --- |
| `GMAIL_SWEEP_MIN_AGE_HOURS` | `20` | How long the newest message must have sat. |
| `GMAIL_SWEEP_DAYS` | `14` | How far back to look (`newer_than:Nd`). |
| `GMAIL_WORKSPACE_HINT` | none | Copied to `hints.workspace`. |

Read-only: `threads.list` and `threads.get` (format `metadata`), 8 in
parallel; about 12 s for 240 threads. If any thread fetch fails the run exits
1 rather than signal a partial list.

Heuristics, so expect noise: cold sales mail written to the account by a real
person lands in *Waiting on you*; notifications with no task keyword are
dropped.

```json
{"id":"orbit:sweep:smith-network-solutions-501877:2026-09-28T09","kind":"sweep","title":"Orbit sweep · SNS: 10 items need you","body":"### Client replied, nobody answered (3)\n- [#100463] Fortis server login — Fortis Institute · response_received · due 2026-09-29 · unassigned · Reply from daniel@fortisinstitute.org · https://orbit.servicestorm.io/smith-network-solutions-501877/tasks/cmula36uo2gb4emwesxb3av8k (id cmula36uo2gb4emwesxb3av8k)\n...","observedAt":"2026-09-28T09:00:02-04:00","refs":{"url":"https://orbit.servicestorm.io/smith-network-solutions-501877/tasks","org":"smith-network-solutions-501877","snoozeFile":"<data dir>/bosun/<lookoutId>/snooze.json","count":"10"},"hints":{"workspace":"SNS"}}
```

### `teams-cdp.mjs`: a 1:1 Microsoft Teams chat

Reads a one-to-one Microsoft Teams chat straight out of the **running Teams for
Linux app** over the Chrome DevTools Protocol — no Graph API, no bot, no
credentials of its own. Node 22+ only (uses the global `WebSocket` and `fetch`);
zero dependencies. Watermark: an ISO 8601 timestamp (the newest message's
arrival time). First run prints only that watermark and no signals.

**How it works.** Teams for Linux is Electron, so its window is a Chromium page.
Launched with a debugging port, that page is attachable. The script finds the
"Microsoft Teams" page target, then runs code in the signed-in page with
`Runtime.evaluate`. Message history is read from the client's **own IndexedDB
caches** — `conversation-manager` (resolve the contact's display name to a 1:1
conversation) and `replychain-manager` (the messages) — which is read-only and
**sends no read receipts**. Inline images and file attachments are downloaded
from the page with `fetch(url, { credentials: "include" })` (the app's cookies
authorize them) and written under `BOSUN_STATE_DIR/attachments/<messageId>/`.

Messages newer than the watermark are grouped into **bursts** (a run of
messages ≤10 min apart); each burst that contains at least one message from the
contact becomes one signal (Spencer's own replies are included for context).

**One-time launcher change.** Teams must be started with a CDP port. Edit the
launcher (`~/.local/share/applications/teams-for-linux.desktop`, or the
`teams-for-linux` invocation) to add:

```
teams-for-linux --remote-debugging-port=9222
```

The port is loopback-only. Nothing else changes; the app runs normally.

| Env | Default | Meaning |
| --- | --- | --- |
| `TEAMS_CDP_URL` | `http://127.0.0.1:9222` | CDP base of the running Teams app. |
| `TEAMS_CONTACT` | `William Hunt` | Display name of the other party; resolved to the most-recently-active 1:1 conversation. |
| `TEAMS_WORKSPACE_HINT` | none | Copied to `hints.workspace` on every signal. |
| `TEAMS_MAX_MESSAGES` | `200` | Max new messages per run (newest kept). |
| `TEAMS_INCLUDE_MINE_ONLY` | unset | `1` also emits bursts that contain only your own messages. |
| `BOSUN_WATERMARK`, `BOSUN_STATE_DIR` | set by Threadknot | Watermark is an ISO time; state dir holds `attachments/`. |

Signal `id` is `teams:<conversationId>:<lastMessageId>`, `kind` is `"teams"`.
The body is `[HH:MM] Name: text` per line (local time, HTML stripped, links kept
as URLs, quoted replies shown as `> [reply to …]`, images inlined as
`[image: <path>]`). `refs.attachments` is a `;`-joined list of saved paths.

```json
{"id":"teams:19:uni01_…@thread.v2:1790608949943","kind":"teams","title":"Teams · Bill: She got it","body":"[11:17] Me: can you have her try again? That should be be fixed now\n[11:22] William Hunt: She got it","observedAt":"2026-09-28T15:22:29.943Z","refs":{"conversationId":"19:uni01_…@thread.v2","lastMessageId":"1790608949943","attachments":"","url":"https://teams.live.com/v2/#/conversations/19%3Auni01_…%40thread.v2?ctx=chat"}}
```

**Extra modes.** `--dump --days N` (default 3) or `--dump --since <ISO>` prints
the window as Markdown (newest last, attachment paths inline) for an agent to
read on demand; it never touches the watermark. `--selftest` runs offline checks
(HTML stripping, reply extraction, burst grouping, watermark comparison,
filename/extension derivation).

Exit codes: 0 ok (including nothing new); **3** Teams not running / CDP port
closed (prints no watermark, so the run retries); **2** signed out (no Teams
page or no message cache).

**Caveats.**
- Requires the running app with the debugging port; if Teams is closed the run
  exits 3 and retries later. The URL scheme is loopback-only.
- Reads the client's local cache, so it only sees history the app has already
  synced. It **does not** page the server or open the chat, so it never sends a
  read receipt or changes any state.
- **Personal (consumer) Teams** was the live target: `teams.live.com/v2`, MSA
  login, message ids that are millisecond timestamps, conversation ids like
  `19:uni01_…@thread.v2`, and inline images at
  `us-api.asm.skype.com/v1/objects/<id>/views/imgo`. Work/school Teams
  (`teams.microsoft.com`, `teams.cloud.microsoft`) uses the same IndexedDB
  managers and message schema, so the reader should carry over, but that path
  is untested here.
- What breaks on a Teams update: the IndexedDB database/store names
  (`replychain-manager` → `replychains`, `conversation-manager` →
  `conversations`) and the message fields (`content`, `imDisplayName`,
  `originalArrivalTime`, `isSentByCurrentUser`, `messageType`,
  `properties.files`). If Microsoft renames those, resolution returns "not
  found" (exit 0, no signals) or messages come back empty — visible in the test
  dialog's stderr — rather than crashing.

### `test-lookouts.sh`

Runs all three selftests, then each lookout live with a fresh `mktemp` state
dir: empty watermark (must print only a watermark), an older watermark (real
signals), a stale Gmail history id (must re-seed), an expired Orbit token (must
refresh), and — when the CDP port answers — the Teams reader through seed →
since → caught-up. Checks that every stdout line is JSON with a `title` or a
`watermark`, that `refs` values are strings, and that the last line is a
watermark. When credentials are present it also runs both sweeps with
`--sweep --now` (at most one `kind: "sweep"` line, hourly id, string refs, no
watermark, body ≤ 32 KiB), snoozes the first listed Orbit task until tomorrow
(it must drop out and the footer must say `snoozed 1`), checks that an expired
snooze entry is pruned, and runs each sweep with `SWEEP_DAYS` set to tomorrow
(it must print nothing). `teams-cdp` auto-skips its live portion when Teams is
not running with the debugging port; `SKIP_TEAMS=1` skips it outright. Orbit is
skipped when there is no `ORBIT_API_KEY` and no `ORBIT_ENV_FILE`. Env overrides
are listed in its header.

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
./lookouts/gmail-gws.sh --selftest                            # offline checks
./lookouts/orbit-tasks.py --selftest
./lookouts/test-lookouts.sh                                   # both, live, read-only
./lookouts/test-lookouts.sh --offline                         # selftests only
```

`BOSUN_STATE_DIR` is optional when testing by hand (`orbit-tasks.py` falls
back to a temp dir; `gmail-gws.sh` does not use it).

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

### `test-lookouts.sh`

Runs both selftests, then each lookout live with a fresh `mktemp` state dir:
empty watermark (must print only a watermark), an older watermark (real
signals), a stale Gmail history id (must re-seed), and an expired Orbit token
(must refresh). Checks that every stdout line is JSON with a `title` or a
`watermark`, that `refs` values are strings, and that the last line is a
watermark. Env overrides are listed in its header.

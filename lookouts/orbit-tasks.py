#!/usr/bin/env python3
"""orbit-tasks.py — Bosun lookout: Orbit tasks created or updated since the watermark.

Contract (docs/BOSUN.md §4): reads BOSUN_WATERMARK (an ISO 8601 timestamp, the
newest task `updatedAt` already seen), prints one NDJSON signal per task
updated after it, then {"watermark": "<newest updatedAt>"}.

  First run (empty BOSUN_WATERMARK): prints only a watermark (the org's newest
  `updatedAt`, so server and local clocks never disagree). No signals, so the
  backlog is not dumped on the Bosun.
  Later runs: GET /api/orgs/{org}/tasks?sortBy=updatedAt&sortOrder=desc, paged
  until a task at or before the watermark. The API has no updatedAfter
  filter; this is the cheapest correct query.

Env:
  ORBIT_API_URL           e.g. https://orbit.servicestorm.io
  ORBIT_API_KEY           agent API key (exchanged for a 15-minute access token)
  ORBIT_ORG_ID            org to watch
  ORBIT_ORG_SLUG          org slug for task URLs (e.g. oscar-edge-817a467)
  ORBIT_WORKSPACE_HINT    optional; copied to hints.workspace on every signal
  ORBIT_INCLUDE_STATUSES  optional comma list of statuses to signal. Default:
                          every status except done and cancelled (filtered
                          server-side with statusNotIn).
  ORBIT_WEB_URL           optional; base for task links (default: ORBIT_API_URL)
  ORBIT_MAX_SIGNALS       optional; max signals per run (default 50). Oldest
                          first; the watermark stops at the last one emitted.
  BOSUN_WATERMARK, BOSUN_STATE_DIR   set by Threadknot

State (BOSUN_STATE_DIR): token.json (access + refresh token, mode 0600),
members.json and clients.json (name lookups, refreshed daily).

Read-only: only GET requests plus the auth exchange/refresh POSTs.
Exit codes: 0 ok; 2 auth failure; 1 network/API failure (no watermark is
printed, so the next run retries from the same point).

Sweep mode (--sweep): no watermark. Lists open tasks still waiting on you and
prints ONE signal (kind "sweep", id orbit:sweep:<slug>:<YYYY-MM-DDTHH> local)
when the list is non-empty, nothing when it is empty or outside the window.
Groups, first match wins: client replied and nobody answered (status
response_received or lastActivityType EXTERNAL_REPLY/NEW_INBOUND, newest task
email inbound and no later non-internal staff comment); overdue; nudged and
not acknowledged; unassigned; mine and stale.
  SWEEP_HOURS          local window, default 08:00-18:00 (end exclusive)
  SWEEP_DAYS           default mon-fri (also "mon,wed", "all")
  SWEEP_ALWAYS=1       ignore the window (--sweep --now sets it)
  ORBIT_ME_EMAIL       comma list of your member emails (or userIds);
                       default spencer@smithnetworksolutions.com
  ORBIT_STALE_DAYS     default 3
  ORBIT_SWEEP_IGNORE   comma list of title substrings to skip,
                       default "Marketing Touchpoint"
  Snooze: BOSUN_STATE_DIR/snooze.json, {"<task id>": "<ISO date|datetime>"};
  hidden until then, expired entries pruned on read.

By hand:   BOSUN_WATERMARK= ./lookouts/orbit-tasks.py | jq
Sweep:     BOSUN_STATE_DIR=$(mktemp -d) ./lookouts/orbit-tasks.py --sweep --now | jq -r .body
Self-test: ./lookouts/orbit-tasks.py --selftest   (offline)
One task:  ./lookouts/orbit-tasks.py --task <id>[,<id>] | ./lookouts/hail.sh   (re-feed a ticket as new)
"""

import hashlib
import json
import os
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone

PAGE_LIMIT = 100          # API max is 200
MAX_PAGES = 20
DESC_CAP = 8000
COMMENT_CAP = 2000
CACHE_TTL = 24 * 3600
DEFAULT_EXCLUDED = ("done", "cancelled")
# Cloudflare in front of Orbit returns 403 to the default "Python-urllib/x" agent.
USER_AGENT = "threadknot-bosun-orbit-tasks/1"


def log(msg):
    print(f"orbit-tasks: {msg}", file=sys.stderr)


class AuthError(Exception):
    pass


class ApiError(Exception):
    pass


# ---------------------------------------------------------------- pure helpers

def parse_ts(s):
    """ISO 8601 -> aware datetime (UTC if no offset). None on failure."""
    if not s:
        return None
    try:
        dt = datetime.fromisoformat(s.strip().replace("Z", "+00:00"))
    except ValueError:
        return None
    return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)


def newer_than(task, mark):
    ts = parse_ts(task.get("updatedAt"))
    return ts is not None and ts > mark


def status_wanted(status, include):
    if include:
        return status in include
    return status not in DEFAULT_EXCLUDED


def latest_comment(comments):
    """Newest comment (replies included) by createdAt, or None."""
    flat = []
    for c in comments or []:
        flat.append(c)
        flat.extend(c.get("replies") or [])
    flat = [c for c in flat if parse_ts(c.get("createdAt"))]
    return max(flat, key=lambda c: parse_ts(c["createdAt"])) if flat else None


def clean_title(s):
    """Drop control characters some email-sourced titles carry."""
    return "".join(ch for ch in (s or "") if ch >= " " or ch == "\t").strip()


def fmt_day(s):
    ts = parse_ts(s)
    return ts.strftime("%Y-%m-%d") if ts else (s or "")


EMAIL_CAP = 4000
MAX_EMAILS = 3


def email_texts(emails, task_id, state_dir, saved):
    """The task's conversation as it arrived by mail, newest last, capped.

    Tickets that come in through a support inbox carry no description at all;
    the customer's words live only here. Inline images arrive as base64 data
    URIs in bodyHtml, so they are written to the state dir and the paths
    collected in `saved`; other attachments are listed by name and size (they
    sit in Orbit's storage with no public download route)."""
    out = []
    ordered = sorted(emails, key=lambda e: e.get("receivedAt") or e.get("createdAt") or "")
    for mail in ordered[-MAX_EMAILS:]:
        who = mail.get("fromName") or mail.get("fromAddress") or "unknown"
        addr = mail.get("fromAddress") or ""
        when = mail.get("receivedAt") or mail.get("sentAt") or mail.get("createdAt") or ""
        text = (mail.get("bodyText") or html_to_text(mail.get("bodyHtml") or "") or mail.get("snippet") or "").strip()
        if len(text) > EMAIL_CAP:
            text = text[:EMAIL_CAP] + "\n[truncated]"
        head = f"Email — {who}{f' <{addr}>' if addr and addr not in who else ''}, {when}, {mail.get('direction') or ''}"
        if mail.get("subject"):
            head += f"\nSubject: {mail['subject']}"
        lines = [head, text]
        paths = save_inline_images(mail, task_id, state_dir)
        saved.extend(paths)
        for path in paths:
            lines.append(f"[image: {path}]")
        for att in mail.get("attachments") or []:
            if att.get("isInline") and paths:
                continue
            lines.append(f"[attachment: {att.get('filename') or att.get('attachmentId')} {att.get('mimeType') or ''} {att.get('size') or ''} bytes, in Orbit]")
        out.append("\n".join(l for l in lines if l))
    return out


def save_inline_images(mail, task_id, state_dir):
    if not state_dir:
        return []
    import base64
    import re as _re
    html = mail.get("bodyHtml") or ""
    paths = []
    names = [a.get("filename") for a in (mail.get("attachments") or []) if a.get("isInline")]
    for n, m in enumerate(_re.finditer(r'src="data:(image/[a-z0-9.+-]+);base64,([A-Za-z0-9+/=\s]+)"', html)):
        mime, data = m.group(1), m.group(2)
        ext = {"image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp"}.get(mime, mime.split("/")[-1])
        name = names[n] if n < len(names) and names[n] else f"inline-{n + 1}.{ext}"
        name = _re.sub(r"[^A-Za-z0-9._-]+", "_", name)
        if not name.lower().endswith("." + ext):
            name += "." + ext
        folder = os.path.join(state_dir, "attachments", task_id)
        os.makedirs(folder, exist_ok=True)
        path = os.path.join(folder, name)
        try:
            with open(path, "wb") as fh:
                fh.write(base64.b64decode("".join(data.split())))
            paths.append(path)
        except Exception as exc:  # noqa: BLE001
            log(f"inline image for {task_id} not saved: {exc}")
    return paths


def html_to_text(html):
    import re as _re
    text = _re.sub(r"<br\s*/?>|</p>|</div>", "\n", html, flags=_re.I)
    text = _re.sub(r"<[^>]+>", "", text)
    return _re.sub(r"\n{3,}", "\n\n", text).strip()


def task_emails(orbit, base, task_id):
    try:
        return orbit.get(f"{base}/tasks/{task_id}/emails").get("data") or []
    except ApiError as e:
        log(f"emails for {task_id} unavailable: {e}")
        return []


def build_signal(task, *, slug, web_url, hint, members, clients, comment, emails=None, state_dir=None):
    tid = task["id"]
    client = (task.get("client") or {}).get("name") or clients.get(task.get("clientId") or "", "")
    assignee = members.get(task.get("assigneeId") or "", "") or (task.get("assigneeId") or "")
    lines = [
        f"Status: {task.get('status') or ''}",
        f"Priority: {task.get('priority') or ''}",
        f"Due: {fmt_day(task.get('dueDate')) or 'none'}",
        f"Client: {client or 'none'}",
        f"Assignee: {assignee or 'unassigned'}",
    ]
    project = (task.get("project") or {}).get("name")
    if project:
        lines.append(f"Project: {project}")
    if task.get("ticketNumber"):
        lines.append(f"Ticket: #{task['ticketNumber']}")
    if task.get("lastActivitySummary"):
        lines.append(f"Last activity: {task['lastActivitySummary']}")
    if task.get("taskType"):
        lines.append(f"Type: {task['taskType']}")
    if task.get("tags"):
        lines.append("Tags: " + ", ".join(str(t) for t in task["tags"]))
    if task.get("sourceEmail"):
        lines.append(f"From: {task['sourceEmail']}")
    if (task.get("aiSummary") or "").strip():
        lines.append(f"Summary: {task['aiSummary'].strip()}")
    body = "\n".join(lines)
    desc = (task.get("description") or "").strip()
    if desc:
        if len(desc) > DESC_CAP:
            desc = desc[:DESC_CAP] + "\n[truncated]"
        body += "\n\n" + desc
    if comment:
        who = (comment.get("user") or {}).get("displayName") \
            or (comment.get("portalUser") or {}).get("name") or "someone"
        tag = " (internal)" if comment.get("isInternal") else ""
        text = (comment.get("content") or "").strip()
        if len(text) > COMMENT_CAP:
            text = text[:COMMENT_CAP] + "\n[truncated]"
        body += f"\n\nLatest comment — {who}, {comment.get('createdAt', '')}{tag}:\n{text}"
    saved = []
    for mail in email_texts(emails or [], tid, state_dir, saved):
        body += "\n\n" + mail
    refs = {
        "url": f"{web_url.rstrip('/')}/{slug}/tasks/{tid}",
        "org": slug,
        "taskId": tid,
    }
    if client:
        refs["client"] = client
    if saved:
        refs["attachments"] = ";".join(saved)
    sig = {
        "id": f"orbit:task:{tid}:{task.get('updatedAt', '')}",
        "kind": "ticket",
        "title": clean_title(task.get("title")) or f"Task {tid}",
        "body": body,
        "observedAt": task.get("updatedAt") or "",
        "refs": refs,
    }
    if hint:
        sig["hints"] = {"workspace": hint}
    return sig


def select_new(tasks, mark, include, max_signals):
    """tasks: newest-first. Returns (oldest-first wanted list, new watermark str|None)."""
    fresh = [t for t in tasks if newer_than(t, mark)]
    fresh.sort(key=lambda t: parse_ts(t["updatedAt"]))
    if len(fresh) > max_signals:
        fresh = fresh[:max_signals]
    wm = fresh[-1]["updatedAt"] if fresh else None
    return [t for t in fresh if status_wanted(t.get("status"), include)], wm


def selftest():
    ok = fail = 0

    def check(desc, got, want):
        nonlocal ok, fail
        if got == want:
            ok += 1
        else:
            fail += 1
            print(f"FAIL: {desc}: want {want!r} got {got!r}")

    mark = parse_ts("2026-09-26T12:00:00-04:00")
    check("parse Z", parse_ts("2026-09-26T16:00:00.000Z"), mark)
    check("parse naive", parse_ts("2026-09-26T16:00:00"), mark)
    check("parse junk", parse_ts("yesterday"), None)
    check("equal is not newer", newer_than({"updatedAt": "2026-09-26T16:00:00.000Z"}, mark), False)
    check("newer", newer_than({"updatedAt": "2026-09-26T16:00:00.001Z"}, mark), True)
    check("default excludes done", status_wanted("done", None), False)
    check("default keeps custom", status_wanted("awaiting_response", None), True)
    check("include list", status_wanted("todo", {"todo"}), True)
    check("include list excludes", status_wanted("blocked", {"todo"}), False)

    tasks = [  # newest first, as the API returns them
        {"id": "c", "updatedAt": "2026-09-27T10:00:00.000Z", "status": "todo"},
        {"id": "b", "updatedAt": "2026-09-27T09:00:00.000Z", "status": "done"},
        {"id": "a", "updatedAt": "2026-09-27T08:00:00.000Z", "status": "blocked"},
        {"id": "old", "updatedAt": "2026-09-25T08:00:00.000Z", "status": "todo"},
    ]
    got, wm = select_new(tasks, mark, None, 50)
    check("select ids oldest first", [t["id"] for t in got], ["a", "c"])
    check("watermark = newest seen (even if filtered)", wm, "2026-09-27T10:00:00.000Z")
    got, wm = select_new(tasks, mark, None, 2)
    check("capped ids", [t["id"] for t in got], ["a"])
    check("capped watermark stops at last considered", wm, "2026-09-27T09:00:00.000Z")
    check("nothing new", select_new(tasks[3:], mark, None, 50), ([], None))

    comments = [
        {"content": "first", "createdAt": "2026-09-01T00:00:00Z", "replies": [
            {"content": "reply-newest", "createdAt": "2026-09-03T00:00:00Z"}]},
        {"content": "second", "createdAt": "2026-09-02T00:00:00Z", "replies": []},
    ]
    check("latest comment includes replies", latest_comment(comments)["content"], "reply-newest")
    check("no comments", latest_comment([]), None)
    check("title control chars", clean_title("New Support: Returns \x14 x.com"), "New Support: Returns  x.com")

    task = {"id": "t1", "title": "Fix login", "status": "todo", "priority": "high",
            "dueDate": "2026-09-30T00:00:00.000Z", "clientId": "cl1", "assigneeId": "u1",
            "updatedAt": "2026-09-27T10:00:00.000Z", "description": "It broke.", "ticketNumber": 42}
    sig = build_signal(task, slug="acme-1", web_url="https://orbit.example", hint="Acme",
                       members={"u1": "Oscar"}, clients={"cl1": "Acme Co"},
                       comment={"content": "on it", "createdAt": "2026-09-27T10:00:00Z",
                                "user": {"displayName": "Oscar"}, "isInternal": True})
    check("signal id", sig["id"], "orbit:task:t1:2026-09-27T10:00:00.000Z")
    check("signal url", sig["refs"]["url"], "https://orbit.example/acme-1/tasks/t1")
    check("signal hint", sig["hints"], {"workspace": "Acme"})
    check("refs all strings", all(isinstance(v, str) for v in sig["refs"].values()), True)
    check("body has client", "Client: Acme Co" in sig["body"], True)
    check("body has assignee", "Assignee: Oscar" in sig["body"], True)
    check("body has due day", "Due: 2026-09-30" in sig["body"], True)
    check("body has comment", "Latest comment — Oscar" in sig["body"] and "(internal)" in sig["body"], True)
    check("no hint when unset", "hints" in build_signal(task, slug="s", web_url="u", hint="",
                                                        members={}, clients={}, comment=None), False)
    # -- sweep: window
    from datetime import timedelta as _td
    local = timezone(_td(hours=-4))
    mon9 = datetime(2026, 9, 28, 9, 0, tzinfo=local)  # a Monday
    check("hours parse", parse_hours("08:00-18:00"), (480, 1080))
    check("days mon-fri", parse_days("mon-fri"), {0, 1, 2, 3, 4})
    check("days list", parse_days("Mon,wed"), {0, 2})
    check("days wrap", parse_days("fri-mon"), {4, 5, 6, 0})
    check("days all", parse_days("all"), set(range(7)))
    check("in window", in_window(mon9, "08:00-18:00", "mon-fri"), True)
    check("end exclusive", in_window(mon9.replace(hour=18), "08:00-18:00", "mon-fri"), False)
    check("before start", in_window(mon9.replace(hour=7, minute=59), "08:00-18:00", "mon-fri"), False)
    check("weekend out", in_window(mon9 - _td(days=1), "08:00-18:00", "mon-fri"), False)
    check("overnight wrap", in_window(mon9.replace(hour=23), "22:00-06:00", "all"), True)
    check("tiny window", in_window(mon9, "00:00-00:01", "all"), False)
    try:
        parse_hours("8am-6pm")
        check("bad hours raises", False, True)
    except ValueError:
        check("bad hours raises", True, True)

    # -- sweep: snooze filter + prune
    sd = tempfile.mkdtemp()
    sp = os.path.join(sd, "snooze.json")
    check("no snooze file", load_snooze(sp, mon9), {})
    with open(sp, "w") as f:
        json.dump({"a": "2026-09-29", "b": "2026-09-28T08:00:00-04:00", "c": "2026-09-28T12:00",
                   "d": "2026-09-28", "e": "garbage"}, f)
    active = load_snooze(sp, mon9)
    check("snooze keeps future", sorted(active), ["a", "c"])
    with open(sp) as f:
        check("snooze pruned on disk", sorted(json.load(f)), ["a", "c"])
    check("bare date = local midnight", snooze_until("2026-09-29", local), datetime(2026, 9, 29, tzinfo=local))

    # -- sweep: grouping
    stale = mon9 - _td(days=3)
    g = lambda t, ans=False: sweep_group(t, today="2026-09-28", me={"me"}, stale_before=stale, answered=ans)
    check("response_received -> replied", g({"status": "response_received", "assigneeId": "x"}), "replied")
    check("EXTERNAL_REPLY -> replied", g({"status": "todo", "lastActivityType": "EXTERNAL_REPLY", "assigneeId": "x"}), "replied")
    check("answered falls through", g({"status": "response_received", "assigneeId": "x"}, True), None)
    check("overdue", g({"status": "todo", "dueDate": "2026-09-27T00:00:00.000Z", "assigneeId": "x"}), "overdue")
    check("due today not overdue", g({"status": "todo", "dueDate": "2026-09-28T00:00:00.000Z", "assigneeId": "x"}), None)
    check("nudged", g({"status": "todo", "nudgedAt": "2026-09-27T00:00:00Z", "acknowledgedNudge": False, "assigneeId": "x"}), "nudged")
    check("nudge acked", g({"status": "todo", "nudgedAt": "2026-09-27T00:00:00Z", "acknowledgedNudge": True, "assigneeId": "x"}), None)
    check("unassigned", g({"status": "todo"}), "unassigned")
    check("assignments count as assigned", g({"status": "todo", "assignments": [{"userId": "x", "role": "assignee"}]}), None)
    check("mine stale", g({"status": "todo", "assigneeId": "me", "updatedAt": "2026-09-20T00:00:00Z"}), "stale")
    check("mine via assignments", g({"status": "todo", "assignments": [{"userId": "me", "role": "assignee"}],
                                     "lastActivityAt": "2026-09-20T00:00:00Z", "updatedAt": "2026-09-28T00:00:00Z"}), "stale")
    check("mine fresh", g({"status": "todo", "assigneeId": "me", "updatedAt": "2026-09-27T00:00:00Z"}), None)
    check("first match wins", g({"status": "response_received", "dueDate": "2026-09-01"}), "replied")
    check("ignore substring", ignored({"title": "Weekly marketing touchpoint - Acme"}, ["marketing touchpoint"]), True)

    # -- sweep: inbound vs outbound
    e_in = {"direction": "inbound", "receivedAt": "2026-09-26T15:44:36Z"}
    e_out = {"direction": "outbound", "sentAt": "2026-09-26T15:35:32Z", "receivedAt": "2026-09-26T15:35:32Z"}
    staff = lambda ts, internal=False: {"userId": "u1", "createdAt": ts, "isInternal": internal}
    check("newest inbound unanswered", answered_after_inbound([e_out, e_in], []), False)
    check("newest outbound answered", answered_after_inbound([e_in, dict(e_out, receivedAt="2026-09-27T00:00:00Z")], []), True)
    check("staff comment after inbound", answered_after_inbound([e_in], [staff("2026-09-27T00:00:00Z")]), True)
    check("internal comment is not an answer", answered_after_inbound([e_in], [staff("2026-09-27T00:00:00Z", True)]), False)
    check("staff comment before inbound", answered_after_inbound([e_in], [staff("2026-09-25T00:00:00Z")]), False)
    check("portal comment is inbound", answered_after_inbound([], [{"portalUserId": "p", "createdAt": "2026-09-27T00:00:00Z"}]), False)
    check("no mail, staff last", answered_after_inbound([], [staff("2026-09-27T00:00:00Z")]), True)

    # -- sweep: signal + id format + cap
    tasks_g = {"replied": [{"id": "t9", "title": "Help", "ticketNumber": 7, "status": "response_received",
                            "clientId": "cl1", "assigneeId": "u1", "lastActivitySummary": "Reply from a@b.co"}],
               "unassigned": [{"id": "t8", "title": "New lead", "status": "todo", "dueDate": "2026-10-01T00:00:00.000Z"}]}
    sw = build_sweep(tasks_g, slug="acme-1", label="Acme", web_url="https://o.x", hint="Acme", snoozed=2,
                     snooze_path="/s/snooze.json", now=mon9, members={"u1": "Oscar"}, clients={"cl1": "Acme Co"})
    check("sweep id", sw["id"], "orbit:sweep:acme-1:2026-09-28T09")
    check("sweep kind", sw["kind"], "sweep")
    check("sweep title", sw["title"], "Orbit sweep · Acme: 2 items need you")
    check("sweep refs", sw["refs"], {"url": "https://o.x/acme-1/tasks", "org": "acme-1",
                                     "snoozeFile": "/s/snooze.json", "count": "2"})
    check("sweep line", sw["body"].split("\n")[1],
          "- [#7] Help — Acme Co · response_received · no due · Oscar · Reply from a@b.co · https://o.x/acme-1/tasks/t9 (id t9)")
    check("groups in order", sw["body"].index("### Client replied") < sw["body"].index("### Unassigned"), True)
    check("footer counts", "overdue 0" in sw["body"] and "unassigned 1" in sw["body"] and "snoozed 2" in sw["body"], True)
    check("footer snooze path", "/s/snooze.json" in sw["body"].split("---")[-1], True)
    check("empty -> no signal", build_sweep({}, slug="s", label="s", web_url="u", hint="", snoozed=0,
                                            snooze_path="p", now=mon9, members={}, clients={}), None)
    check("one item title", build_sweep({"overdue": [{"id": "z"}]}, slug="s", label="S", web_url="u", hint="",
                                        snoozed=0, snooze_path="p", now=mon9, members={}, clients={})["title"],
          "Orbit sweep · S: 1 item needs you")
    big = cap_body(["### G\n" + "\n".join("- " + "x" * 200 for _ in range(400))], "---\nfooter", 32 * 1024)
    check("cap under 32 KiB", len(big.encode()) <= 32 * 1024, True)
    check("cap keeps footer", big.endswith("---\nfooter"), True)
    check("cap notes dropped", "more lines not shown" in big, True)

    json.dumps(sig)
    print(f"orbit-tasks selftest: {ok} passed, {fail} failed")
    return 0 if fail == 0 else 1


# ---------------------------------------------------------------- sweep mode
#
# `--sweep`: a model-free "what is still waiting on me" check. No watermark;
# one signal per run, only when the list is non-empty; the id carries the
# local hour so the engine sees a fresh signal each run while anything is open.

SWEEP_BODY_CAP = 32 * 1024
SWEEP_WORKERS = 6
INBOUND_ACTIVITY = ("EXTERNAL_REPLY", "NEW_INBOUND")
INBOUND_SUMMARY = ("reply from ", "new ticket from ")
SWEEP_GROUPS = (
    ("replied", "Client replied, nobody answered"),
    ("overdue", "Overdue"),
    ("nudged", "Nudged, not acknowledged"),
    ("unassigned", "Unassigned"),
    ("stale", "Mine and stale"),
)
DAY_NAMES = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")


def parse_hours(spec):
    """'08:00-18:00' -> (480, 1080) minutes since midnight. Raises ValueError."""
    a, b = spec.strip().split("-", 1)

    def mins(x):
        h, m = x.strip().split(":")
        h, m = int(h), int(m)
        if not (0 <= h <= 24 and 0 <= m < 60):
            raise ValueError(x)
        return h * 60 + m
    return mins(a), mins(b)


def parse_days(spec):
    """'mon-fri' / 'mon,wed,fri' / 'fri-mon' / 'all' -> set of weekday ints (Mon=0)."""
    spec = (spec or "").strip().lower()
    if spec in ("", "all", "*", "daily"):
        return set(range(7))
    out = set()
    for part in spec.split(","):
        part = part.strip()[:7]
        if "-" in part:
            a, b = (DAY_NAMES.index(p.strip()[:3]) for p in part.split("-", 1))
            i = a
            while True:
                out.add(i)
                if i == b:
                    break
                i = (i + 1) % 7
        elif part:
            out.add(DAY_NAMES.index(part[:3]))
    return out


def in_window(now, hours, days):
    """now: aware or naive local datetime. End of the hours range is exclusive;
    a range that ends before it starts wraps past midnight."""
    start, end = parse_hours(hours)
    m = now.hour * 60 + now.minute
    if now.weekday() not in parse_days(days):
        return False
    if start <= end:
        return start <= m < end
    return m >= start or m < end


def snooze_until(value, tz):
    """Snooze value -> aware datetime. Bare dates mean local midnight of that day."""
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        dt = datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
    except ValueError:
        return None
    return dt if dt.tzinfo else dt.replace(tzinfo=tz)


def load_snooze(path, now):
    """Active snoozes {id: until}. Expired or unparseable entries are pruned
    from the file (rewritten only if something changed)."""
    try:
        with open(path) as f:
            raw = json.load(f)
    except FileNotFoundError:
        return {}
    except (OSError, ValueError) as e:
        log(f"snooze file unreadable ({e}); ignoring it")
        return {}
    if not isinstance(raw, dict):
        log("snooze file is not a JSON object; ignoring it")
        return {}
    keep = {k: v for k, v in raw.items() if (snooze_until(v, now.tzinfo) or now) > now}
    if len(keep) != len(raw):
        try:
            tmp = path + ".tmp"
            with open(tmp, "w") as f:
                json.dump(keep, f, indent=2)
            os.replace(tmp, path)
            log(f"snooze: pruned {len(raw) - len(keep)} expired entr{'y' if len(raw) - len(keep) == 1 else 'ies'}")
        except OSError as e:
            log(f"snooze prune not written: {e}")
    return keep


def looks_inbound(task):
    if task.get("status") == "response_received":
        return True
    if (task.get("lastActivityType") or "") in INBOUND_ACTIVITY:
        return True
    return (task.get("lastActivitySummary") or "").lower().startswith(INBOUND_SUMMARY)


def mail_time(mail):
    return parse_ts(mail.get("receivedAt") or mail.get("sentAt") or mail.get("createdAt"))


def answered_after_inbound(emails, comments):
    """True when the newest email is outbound, or a non-internal staff comment
    is newer than the newest inbound email (or, with no email, is the newest
    comment). Portal comments are the client talking, so they are inbound."""
    mails = [m for m in emails or [] if mail_time(m)]
    last_in = None
    if mails:
        newest = max(mails, key=mail_time)
        if (newest.get("direction") or "").lower() == "outbound":
            return True
        ins = [mail_time(m) for m in mails if (m.get("direction") or "").lower() == "inbound"]
        last_in = max(ins) if ins else None
    flat = []
    for c in comments or []:
        flat.append(c)
        flat.extend(c.get("replies") or [])
    flat = [c for c in flat if parse_ts(c.get("createdAt"))]
    if not flat:
        return False
    if last_in is None:
        newest_c = max(flat, key=lambda c: parse_ts(c["createdAt"]))
        return staff_reply(newest_c)
    return any(staff_reply(c) and parse_ts(c["createdAt"]) > last_in for c in flat)


def staff_reply(c):
    return bool(c.get("userId") or c.get("user")) and not c.get("portalUserId") \
        and not c.get("portalUser") and not c.get("isInternal")


def is_mine(task, me):
    if not me:
        return False
    if task.get("assigneeId") in me:
        return True
    return any(a.get("userId") in me and (a.get("role") or "assignee") == "assignee"
               for a in task.get("assignments") or [])


def is_unassigned(task):
    return not task.get("assigneeId") and not any(
        (a.get("role") or "assignee") == "assignee" for a in task.get("assignments") or [])


def sweep_group(task, *, today, me, stale_before, answered):
    """First matching group key, or None. `answered` is the answered_after_inbound
    verdict (only consulted for inbound-looking tasks)."""
    if looks_inbound(task) and not answered:
        return "replied"
    due = fmt_day(task.get("dueDate")) if task.get("dueDate") else ""
    if due and due < today:
        return "overdue"
    if task.get("nudgedAt") and not task.get("acknowledgedNudge"):
        return "nudged"
    if is_unassigned(task):
        return "unassigned"
    last = parse_ts(task.get("lastActivityAt")) or parse_ts(task.get("updatedAt"))
    if is_mine(task, me) and last and last < stale_before:
        return "stale"
    return None


def ignored(task, patterns):
    title = (task.get("title") or "").lower()
    return any(p in title for p in patterns)


def sweep_line(task, *, slug, web_url, members, clients):
    tid = task["id"]
    client = (task.get("client") or {}).get("name") or clients.get(task.get("clientId") or "", "") or "no client"
    assignee = members.get(task.get("assigneeId") or "", "") or task.get("assigneeId") or ""
    if not assignee:
        names = [((a.get("user") or {}).get("displayName") or members.get(a.get("userId") or "", ""))
                 for a in task.get("assignments") or [] if (a.get("role") or "assignee") == "assignee"]
        assignee = ", ".join(n for n in names if n) or "unassigned"
    due = f"due {fmt_day(task['dueDate'])}" if task.get("dueDate") else "no due"
    parts = [client, task.get("status") or "", due, assignee]
    if task.get("lastActivitySummary"):
        parts.append(task["lastActivitySummary"].strip())
    parts.append(f"{web_url.rstrip('/')}/{slug}/tasks/{tid}")
    num = f"[#{task['ticketNumber']}] " if task.get("ticketNumber") else ""
    title = clean_title(task.get("title")) or f"Task {tid}"
    return f"- {num}{title} — {' · '.join(parts)} (id {tid})"


def snooze_footer(path):
    return (f"Snooze: add \"<id>\": \"<YYYY-MM-DD or ISO datetime>\" to the JSON object in {path} "
            "(hidden until then; a bare date means local midnight).")


def cap_body(sections, footer, cap):
    """Join group sections, dropping trailing lines to stay under `cap` bytes
    (UTF-8); the footer is always kept."""
    body = "\n\n".join(sections)
    tail = "\n\n" + footer
    if len((body + tail).encode()) <= cap:
        return body + tail
    lines = body.split("\n")
    dropped = 0
    while lines and len(("\n".join(lines) + f"\n…{dropped} more lines not shown" + tail).encode()) > cap:
        lines.pop()
        dropped += 1
    return "\n".join(lines) + f"\n…{dropped} more lines not shown" + tail


def build_sweep(grouped, *, slug, label, web_url, hint, snoozed, snooze_path, now, members, clients):
    """grouped: {group key: [task, ...]}. Returns the signal dict, or None if empty."""
    total = sum(len(v) for v in grouped.values())
    if not total:
        return None
    sections, counts = [], []
    for key, name in SWEEP_GROUPS:
        items = grouped.get(key) or []
        counts.append(f"{name.lower()} {len(items)}")
        if items:
            lines = [f"### {name} ({len(items)})"]
            lines += [sweep_line(t, slug=slug, web_url=web_url, members=members, clients=clients) for t in items]
            sections.append("\n".join(lines))
    footer = "---\n" + " · ".join(counts) + f" · snoozed {snoozed}\n" + snooze_footer(snooze_path)
    sig = {
        "id": f"orbit:sweep:{slug}:{now.strftime('%Y-%m-%dT%H')}",
        "kind": "sweep",
        "title": f"Orbit sweep · {label}: {total} item{'s' if total != 1 else ''} need{'s' if total == 1 else ''} you",
        "body": cap_body(sections, footer, SWEEP_BODY_CAP),
        "observedAt": now.isoformat(timespec="seconds"),
        "refs": {"url": f"{web_url.rstrip('/')}/{slug}/tasks", "org": slug,
                 "snoozeFile": snooze_path, "count": str(total)},
    }
    if hint:
        sig["hints"] = {"workspace": hint}
    return sig


# ---------------------------------------------------------------- API client

class Orbit:
    def __init__(self, api_url, api_key, state_dir):
        self.api = api_url.rstrip("/")
        self.key = api_key
        self.token_path = os.path.join(state_dir, "token.json")
        self.key_id = hashlib.sha256(api_key.encode()).hexdigest()[:16]
        self.tok = self._load_token()

    # -- token cache
    def _load_token(self):
        try:
            with open(self.token_path) as f:
                t = json.load(f)
            if t.get("keyId") == self.key_id and t.get("accessToken"):
                return t
        except (OSError, ValueError):
            pass
        return {}

    def _save_token(self, tokens):
        self.tok = {"keyId": self.key_id, "accessToken": tokens.get("accessToken"),
                    "refreshToken": tokens.get("refreshToken"), "savedAt": int(time.time())}
        tmp = self.token_path + ".tmp"
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump(self.tok, f)
        os.replace(tmp, self.token_path)

    def _post_auth(self, path, payload):
        req = urllib.request.Request(self.api + path, data=json.dumps(payload).encode(),
                                     headers={"Content-Type": "application/json", "User-Agent": USER_AGENT}, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                data = json.load(r)
        except urllib.error.HTTPError as e:
            return None, e.code
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            raise ApiError(f"auth request failed: {e}")
        tokens = ((data or {}).get("data") or {}).get("tokens") or {}
        return (tokens if tokens.get("accessToken") else None), 200

    def authenticate(self, try_refresh=True):
        if try_refresh and self.tok.get("refreshToken"):
            tokens, _ = self._post_auth("/api/auth/refresh", {"refreshToken": self.tok["refreshToken"]})
            if tokens:
                self._save_token(tokens)
                return
        tokens, code = self._post_auth("/api/auth/agent/exchange", {"apiKey": self.key})
        if not tokens:
            raise AuthError(f"API key exchange failed (HTTP {code})")
        self._save_token(tokens)

    # -- GET with 401 refresh and 429/5xx backoff
    def get(self, path, params=None):
        if not self.tok.get("accessToken"):
            self.authenticate(try_refresh=False)
        url = self.api + path + ("?" + urllib.parse.urlencode(params) if params else "")
        reauthed = False
        for attempt in range(4):
            req = urllib.request.Request(url, headers={"Authorization": f"Bearer {self.tok['accessToken']}",
                                                       "Accept": "application/json",
                                                       "User-Agent": USER_AGENT})
            try:
                with urllib.request.urlopen(req, timeout=30) as r:
                    return json.load(r)
            except urllib.error.HTTPError as e:
                if e.code == 401 and not reauthed:
                    reauthed = True
                    self.authenticate()
                    continue
                if e.code in (401, 403):
                    raise AuthError(f"GET {path}: HTTP {e.code}")
                if e.code == 429 or e.code >= 500:
                    wait = e.headers.get("Retry-After")
                    time.sleep(min(float(wait) if wait and wait.isdigit() else 2 ** attempt, 20))
                    continue
                body = e.read(300).decode(errors="replace")
                raise ApiError(f"GET {path}: HTTP {e.code} {body}")
            except (urllib.error.URLError, TimeoutError, OSError) as e:
                if attempt == 3:
                    raise ApiError(f"GET {path}: {e}")
                time.sleep(2 ** attempt)
        raise ApiError(f"GET {path}: gave up after retries")


def cached_map(orbit, state_dir, name, path, key_fn, val_fn):
    """id->name map for members/clients, cached for CACHE_TTL. Never fatal."""
    fp = os.path.join(state_dir, f"{name}.json")
    try:
        if time.time() - os.path.getmtime(fp) < CACHE_TTL:
            with open(fp) as f:
                return json.load(f)
    except (OSError, ValueError):
        pass
    out = {}
    try:
        page = 1
        while page <= 10:
            res = orbit.get(path, {"limit": 200, "page": page})
            for item in res.get("data") or []:
                k, v = key_fn(item), val_fn(item)
                if k and v:
                    out[k] = v
            pg = res.get("pagination") or {}
            if page >= (pg.get("totalPages") or 1):
                break
            page += 1
        with open(fp, "w") as f:
            json.dump(out, f)
    except (ApiError, OSError) as e:
        log(f"{name} lookup unavailable ({e}); names may be ids")
    return out


def run_sweep(orbit, env, *, org, slug, web_url, hint, state_dir):
    now = datetime.now().astimezone()
    if env.get("SWEEP_ALWAYS") != "1":
        hours, days = env.get("SWEEP_HOURS") or "08:00-18:00", env.get("SWEEP_DAYS") or "mon-fri"
        try:
            inside = in_window(now, hours, days)
        except ValueError:
            log(f"bad SWEEP_HOURS/SWEEP_DAYS ({hours!r} / {days!r})")
            return 1
        if not inside:
            return 0
    try:
        stale_days = float(env.get("ORBIT_STALE_DAYS") or 3)
    except ValueError:
        stale_days = 3.0
    ignore = [p.strip().lower() for p in env.get("ORBIT_SWEEP_IGNORE", "Marketing Touchpoint").split(",") if p.strip()]
    wanted_me = [e.strip().lower() for e in (env.get("ORBIT_ME_EMAIL") or "spencer@smithnetworksolutions.com").split(",") if e.strip()]
    base = f"/api/orgs/{org}"
    snooze_path = os.path.abspath(os.path.join(state_dir, "snooze.json"))
    snoozes = load_snooze(snooze_path, now)

    members = cached_map(orbit, state_dir, "members", base + "/members",
                         lambda m: m.get("userId") or (m.get("user") or {}).get("id"),
                         lambda m: (m.get("user") or {}).get("displayName"))
    clients = cached_map(orbit, state_dir, "clients", base + "/clients",
                         lambda c: c.get("id"), lambda c: c.get("name"))
    emails_map = cached_map(orbit, state_dir, "member-emails", base + "/members",
                            lambda m: ((m.get("user") or {}).get("email") or "").lower(),
                            lambda m: m.get("userId") or (m.get("user") or {}).get("id"))
    me = {emails_map[e] for e in wanted_me if e in emails_map} | {e for e in wanted_me if e in members}
    if not me:
        log(f"ORBIT_ME_EMAIL {','.join(wanted_me)} is not a member of this org; 'Mine and stale' is empty")

    tasks, page = [], 1
    params = {"statusNotIn": ",".join(DEFAULT_EXCLUDED), "sortBy": "updatedAt", "sortOrder": "desc", "limit": PAGE_LIMIT}
    while page <= MAX_PAGES:
        res = orbit.get(base + "/tasks", dict(params, page=page))
        batch = res.get("data") or []
        tasks.extend(batch)
        if not batch or page >= ((res.get("pagination") or {}).get("totalPages") or 1):
            break
        page += 1
    else:
        log(f"stopped after {MAX_PAGES} pages")

    open_tasks, snoozed, skipped = [], 0, 0
    for t in tasks:
        if t.get("archivedAt") or t.get("status") in DEFAULT_EXCLUDED or ignored(t, ignore):
            skipped += 1
        elif t.get("id") in snoozes:
            snoozed += 1
        else:
            open_tasks.append(t)

    # The list endpoint omits nudgedAt/acknowledgedNudge/taskInboxes; the
    # single-task GET has them. Inbound-looking tasks also need their email
    # thread (direction of the newest message) and comments.
    def detail(t):
        tid = t["id"]
        try:
            full = orbit.get(f"{base}/tasks/{tid}").get("data") or {}
        except ApiError as e:
            log(f"task {tid} detail unavailable ({e}); grouping from the list row")
            full = {}
        merged = dict(t, **{k: full.get(k) for k in ("nudgedAt", "acknowledgedNudge", "taskInboxes") if k in full})
        answered = False
        if looks_inbound(merged):
            emails = task_emails(orbit, base, tid) if merged.get("taskInboxes") else []
            comments = []
            if ((t.get("_count") or {}).get("comments") or 0) > 0:
                try:
                    comments = orbit.get(f"{base}/tasks/{tid}/comments").get("data") or []
                except ApiError as e:
                    log(f"comments for {tid} unavailable: {e}")
            answered = answered_after_inbound(emails, comments)
        return merged, answered

    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=SWEEP_WORKERS) as pool:
        detailed = list(pool.map(detail, open_tasks))

    today = now.strftime("%Y-%m-%d")
    stale_before = now - timedelta(days=stale_days)
    grouped = {k: [] for k, _ in SWEEP_GROUPS}
    for t, answered in detailed:
        g = sweep_group(t, today=today, me=me, stale_before=stale_before, answered=answered)
        if g:
            grouped[g].append(t)
    for items in grouped.values():  # oldest activity first: longest-waiting on top
        items.sort(key=lambda t: parse_ts(t.get("lastActivityAt")) or parse_ts(t.get("updatedAt"))
                   or datetime.min.replace(tzinfo=timezone.utc))
    sig = build_sweep(grouped, slug=slug, label=hint or slug, web_url=web_url, hint=hint,
                      snoozed=snoozed, snooze_path=snooze_path, now=now, members=members, clients=clients)
    log(f"sweep: {len(tasks)} open fetched, {skipped} ignored, {snoozed} snoozed, "
        + ", ".join(f"{k} {len(v)}" for k, v in grouped.items()))
    if sig:
        print(json.dumps(sig, ensure_ascii=False))
    return 0


# ---------------------------------------------------------------- main

def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        return selftest()

    env = os.environ
    missing = [k for k in ("ORBIT_API_URL", "ORBIT_API_KEY", "ORBIT_ORG_ID", "ORBIT_ORG_SLUG") if not env.get(k)]
    if missing:
        log("missing env: " + ", ".join(missing))
        return 1
    org, slug = env["ORBIT_ORG_ID"], env["ORBIT_ORG_SLUG"]
    web_url = env.get("ORBIT_WEB_URL") or env["ORBIT_API_URL"]
    hint = env.get("ORBIT_WORKSPACE_HINT", "").strip()
    include = {s.strip() for s in env.get("ORBIT_INCLUDE_STATUSES", "").split(",") if s.strip()} or None
    try:
        max_signals = max(1, int(env.get("ORBIT_MAX_SIGNALS") or 50))
    except ValueError:
        max_signals = 50
    state_dir = env.get("BOSUN_STATE_DIR") or os.path.join(tempfile.gettempdir(), f"orbit-tasks-{org}")
    os.makedirs(state_dir, exist_ok=True)

    orbit = Orbit(env["ORBIT_API_URL"], env["ORBIT_API_KEY"], state_dir)
    if "--sweep" in sys.argv:
        if "--now" in sys.argv:
            env["SWEEP_ALWAYS"] = "1"
        return run_sweep(orbit, env, org=org, slug=slug, web_url=web_url, hint=hint, state_dir=state_dir)
    base = f"/api/orgs/{org}"
    list_params = {"sortBy": "updatedAt", "sortOrder": "desc", "limit": PAGE_LIMIT}

    # Specific tasks, regardless of watermark: `--task id[,id]` or ORBIT_TASK_IDS.
    # For re-feeding a ticket the lookout missed (seeded past it, or triaged
    # away) as if it had just arrived: pipe the output into hail.sh. Prints no
    # watermark line, so it never moves the lookout's own position.
    fixed = [a for a in sys.argv[1:] if a not in ("--task",)]
    if "--task" in sys.argv:
        fixed = sys.argv[sys.argv.index("--task") + 1:sys.argv.index("--task") + 2]
    ids = ",".join(fixed if "--task" in sys.argv else [env.get("ORBIT_TASK_IDS", "")])
    ids = [i.strip() for i in ids.split(",") if i.strip()]
    if ids:
        members = cached_map(orbit, state_dir, "members", base + "/members",
                             lambda m: m.get("userId") or (m.get("user") or {}).get("id"),
                             lambda m: (m.get("user") or {}).get("displayName"))
        clients = cached_map(orbit, state_dir, "clients", base + "/clients",
                             lambda c: c.get("id"), lambda c: c.get("name"))
        for tid in ids:
            t = orbit.get(f"{base}/tasks/{tid}").get("data") or {}
            if not t.get("id"):
                log(f"task {tid} not found")
                continue
            comment = None
            if ((t.get("_count") or {}).get("comments") or 0) > 0:
                try:
                    comment = latest_comment(orbit.get(f"{base}/tasks/{tid}/comments").get("data"))
                except ApiError as e:
                    log(f"comments for {tid} unavailable: {e}")
            print(json.dumps(build_signal(t, slug=slug, web_url=web_url, hint=hint,
                                          members=members, clients=clients, comment=comment,
                                          emails=task_emails(orbit, base, tid), state_dir=state_dir),
                             ensure_ascii=False))
        return 0

    watermark = env.get("BOSUN_WATERMARK", "").strip()
    mark = parse_ts(watermark)
    if not mark:
        if watermark:
            log(f"watermark {watermark!r} is not ISO 8601; re-seeding")
        res = orbit.get(base + "/tasks", dict(list_params, limit=1))
        newest = (res.get("data") or [{}])[0].get("updatedAt")
        seed = newest or datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        print(json.dumps({"watermark": seed}))
        return 0

    if not include:
        list_params["statusNotIn"] = ",".join(DEFAULT_EXCLUDED)
    tasks, page = [], 1
    while page <= MAX_PAGES:
        res = orbit.get(base + "/tasks", dict(list_params, page=page))
        batch = res.get("data") or []
        tasks.extend(batch)
        pg = res.get("pagination") or {}
        if not batch or not newer_than(batch[-1], mark) or page >= (pg.get("totalPages") or 1):
            break
        page += 1
    else:
        log(f"stopped after {MAX_PAGES} pages")

    wanted, new_wm = select_new(tasks, mark, include, max_signals)
    if wanted:
        members = cached_map(orbit, state_dir, "members", base + "/members",
                             lambda m: m.get("userId") or (m.get("user") or {}).get("id"),
                             lambda m: (m.get("user") or {}).get("displayName"))
        clients = cached_map(orbit, state_dir, "clients", base + "/clients",
                             lambda c: c.get("id"), lambda c: c.get("name"))
    for t in wanted:
        comment = None
        if ((t.get("_count") or {}).get("comments") or 0) > 0:
            try:
                comment = latest_comment(orbit.get(f"{base}/tasks/{t['id']}/comments").get("data"))
            except ApiError as e:
                log(f"comments for {t['id']} unavailable: {e}")
        sig = build_signal(t, slug=slug, web_url=web_url, hint=hint,
                           members=members, clients=clients, comment=comment,
                           emails=task_emails(orbit, base, t["id"]), state_dir=state_dir)
        print(json.dumps(sig, ensure_ascii=False))
    log(f"since {watermark}: {len(tasks)} fetched, {len(wanted)} signalled")
    print(json.dumps({"watermark": new_wm or watermark}))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except AuthError as e:
        log(f"auth failed: {e}")
        sys.exit(2)
    except ApiError as e:
        log(str(e))
        sys.exit(1)

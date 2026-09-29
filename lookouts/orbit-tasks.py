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

By hand:   BOSUN_WATERMARK= ./lookouts/orbit-tasks.py | jq
Self-test: ./lookouts/orbit-tasks.py --selftest   (offline)
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
from datetime import datetime, timezone

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


def build_signal(task, *, slug, web_url, hint, members, clients, comment):
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
    refs = {
        "url": f"{web_url.rstrip('/')}/{slug}/tasks/{tid}",
        "org": slug,
        "taskId": tid,
    }
    if client:
        refs["client"] = client
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
    json.dumps(sig)
    print(f"orbit-tasks selftest: {ok} passed, {fail} failed")
    return 0 if fail == 0 else 1


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
    base = f"/api/orgs/{org}"
    list_params = {"sortBy": "updatedAt", "sortOrder": "desc", "limit": PAGE_LIMIT}

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
                           members=members, clients=clients, comment=comment)
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
